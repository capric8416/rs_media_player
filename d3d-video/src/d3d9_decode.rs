//! DXVA2 decoder creation and checked submission of codec-prepared decode pictures.
//!
//! AVC, HEVC and AV1 picture-parameter translation is kept separate from the D3D9 transport so
//! codec adapters can produce driver-ready DXVA2 buffers without owning COM submission details.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D9::IDirect3DSurface9;
use windows::Win32::Media::MediaFoundation::{
    DXVA2_BitStreamDateBufferType, DXVA2_BufferfType, DXVA2_DecodeBufferDesc,
    DXVA2_DecodeExecuteParams, DXVA2_InverseQuantizationMatrixBufferType,
    DXVA2_PictureParametersBufferType, DXVA2_SliceControlBufferType,
    DXVA2_VideoDecoderRenderTarget, IDirectXVideoDecoder,
};

use crate::avc::AvcDecodeState;
use crate::d3d9_device::{D3d9CompletionToken, D3d9DecodeCapability, D3d9Device};
use crate::hevc::HevcDecodeState;
use crate::{
    BitstreamFormat, Codec, CodecDecodeState, ColorInfo, D3dBackend, D3dDecode, DecodeCapability,
    DecoderConfig, EncodedSample, FrameInfo, FrameSize, GpuFrame, PixelFormat, SampleTiming,
    VideoError, VideoErrorKind, VideoResult, WaitStatus,
};

/// A driver-ready DXVA2 compressed-buffer payload produced by a codec picture adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dxva2Buffer {
    pub buffer_type: DXVA2_BufferfType,
    pub data: Vec<u8>,
    pub first_macroblock: u32,
    pub macroblock_count: u32,
}

/// One decoded picture and its already translated DXVA2 parameter/slice buffers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dxva2Picture {
    pub target_index: usize,
    pub buffers: Vec<Dxva2Buffer>,
}

/// A D3D9 decoder surface and completion token for one submitted picture.
pub struct D3d9DecodedFrame {
    pub(crate) surface: IDirect3DSurface9,
    pub(crate) surface_index: usize,
    pub(crate) completion: D3d9CompletionToken,
    pub(crate) info: FrameInfo,
    pub(crate) sample_aspect: crate::SampleAspectRatio,
    pub(crate) surface_lease: Option<crate::frame::SurfaceLease>,
    pub(crate) retained_source_lease: Option<crate::frame::SurfaceLease>,
    pub(crate) retained_source_surface: Option<IDirect3DSurface9>,
}

impl Drop for D3d9DecodedFrame {
    fn drop(&mut self) {
        match self.completion.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) | Err(_) => {
                std::mem::forget(self.surface.clone());
                if let Some(surface) = self.retained_source_surface.take() {
                    std::mem::forget(surface);
                }
                if let Some(lease) = self.surface_lease.take() {
                    std::mem::forget(lease);
                }
                if let Some(lease) = self.retained_source_lease.take() {
                    std::mem::forget(lease);
                }
            }
        }
    }
}

impl D3d9DecodedFrame {
    /// Returns the decoder output surface for D3D9 processing or presentation.
    #[must_use]
    pub const fn surface(&self) -> &IDirect3DSurface9 {
        &self.surface
    }

    #[must_use]
    pub const fn surface_index(&self) -> usize {
        self.surface_index
    }

    #[must_use]
    pub const fn sample_aspect_ratio(&self) -> crate::SampleAspectRatio {
        self.sample_aspect
    }
}

impl GpuFrame for D3d9DecodedFrame {
    fn backend(&self) -> D3dBackend {
        D3dBackend::D3d9
    }

    fn info(&self) -> &FrameInfo {
        &self.info
    }

    fn is_ready(&self) -> VideoResult<bool> {
        self.completion.is_ready()
    }

    fn wait(&mut self, timeout: Duration) -> VideoResult<WaitStatus> {
        self.completion.wait(timeout)
    }
}

/// Tile byte range and coordinates for an AV1 hardware frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dxva2Av1Tile {
    pub data_offset: u32,
    pub data_size: u32,
    pub row: u16,
    pub column: u16,
}

impl Dxva2Picture {
    /// Builds a picture with normalized encoded data plus caller-prepared codec parameter buffers.
    ///
    /// `macroblock_count` is used by AVC/HEVC DXVA drivers for the bitstream descriptor and should
    /// be zero for AV1. Slice-control or tile metadata remains the codec adapter's responsibility.
    ///
    /// # Errors
    ///
    /// Returns framing errors, invalid-input errors for conflicting buffer lists, or allocation
    /// errors while building the driver bitstream payload.
    pub fn from_sample(
        target_index: usize,
        sample: EncodedSample<'_>,
        codec: Codec,
        format: BitstreamFormat,
        macroblock_count: u32,
        mut parameter_buffers: Vec<Dxva2Buffer>,
    ) -> VideoResult<Self> {
        if parameter_buffers
            .iter()
            .any(|buffer| buffer.buffer_type == DXVA2_BitStreamDateBufferType)
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_sample",
                "parameter buffers must not contain a bitstream buffer",
            ));
        }
        let data = build_dxva2_bitstream(sample, codec, format)?;
        parameter_buffers
            .try_reserve(1)
            .map_err(|_| allocation_error())?;
        parameter_buffers.push(Dxva2Buffer {
            buffer_type: DXVA2_BitStreamDateBufferType,
            data,
            first_macroblock: 0,
            macroblock_count,
        });
        validate_picture_buffers(&parameter_buffers)?;
        Ok(Self {
            target_index,
            buffers: parameter_buffers,
        })
    }

    /// Builds an AVC DXVA2 picture with the VCL bitstream and short slice-control buffers.
    ///
    /// `parameter_buffers` must include the driver-ready AVC picture-parameter buffer and may
    /// include its quantization buffer. The selected decoder config must use
    /// `ConfigBitstreamRaw == 2` (short slice controls).
    ///
    /// # Errors
    ///
    /// Returns framing, validation, or allocation errors if sample buffers cannot be constructed.
    pub fn from_avc_sample(
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        coded_size: FrameSize,
        mut parameter_buffers: Vec<Dxva2Buffer>,
    ) -> VideoResult<Self> {
        if parameter_buffers.iter().any(|buffer| {
            buffer.buffer_type == DXVA2_BitStreamDateBufferType
                || buffer.buffer_type == DXVA2_SliceControlBufferType
        }) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_avc_sample",
                "AVC parameter buffers must not contain bitstream or slice-control buffers",
            ));
        }
        let [bitstream, slice_control] = build_avc_slice_buffers(sample, format, coded_size)?;
        parameter_buffers
            .try_reserve(2)
            .map_err(|_| allocation_error())?;
        parameter_buffers.push(bitstream);
        parameter_buffers.push(slice_control);
        validate_picture_buffers(&parameter_buffers)?;
        Ok(Self {
            target_index,
            buffers: parameter_buffers,
        })
    }

    /// Builds an AVC DXVA2 IDR picture from codec-configuration SPS/PPS and an encoded sample.
    ///
    /// The generated picture-parameter and inverse-quantization buffers cover progressive
    /// 8-bit 4:2:0 IDR pictures. Inter pictures, fields, high bit depth, and additional AVC tools
    /// outside the parser's supported syntax are rejected by the parameter builder.
    ///
    /// # Errors
    ///
    /// Returns parser, framing, unsupported-syntax, or allocation errors.
    pub fn from_avc_idr_sample(
        target_index: usize,
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        coded_size: FrameSize,
    ) -> VideoResult<Self> {
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_avc_idr_sample",
                "DXVA surface index exceeds u8",
            )
        })?;
        let mut idr_slice = None;
        for nal in codec_nals(sample.data, format, "Dxva2Picture::from_avc_idr_sample")? {
            let Some(&header) = nal.first() else {
                continue;
            };
            if matches!(header & 0x1f, 1 | 5) {
                if header & 0x1f != 5 {
                    return Err(decode_error(
                        VideoErrorKind::Unsupported,
                        "Dxva2Picture::from_avc_idr_sample",
                        "this constructor only accepts AVC IDR access units",
                    ));
                }
                if idr_slice.is_none() {
                    idr_slice = Some(nal);
                }
            }
        }
        let idr_slice = idr_slice.ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_avc_idr_sample",
                "access unit contains no AVC IDR slice",
            )
        })?;
        let picture_parameters =
            crate::avc::idr_picture_parameters(sps_nal, pps_nal, idr_slice, surface_index)?;
        let picture_width_in_mbs = u32::from(u16::from_le_bytes([
            picture_parameters[0],
            picture_parameters[1],
        ])) + 1;
        let picture_height_in_mbs = u32::from(u16::from_le_bytes([
            picture_parameters[2],
            picture_parameters[3],
        ])) + 1;
        if picture_width_in_mbs != coded_size.width().div_ceil(16)
            || picture_height_in_mbs != coded_size.height().div_ceil(16)
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_avc_idr_sample",
                "AVC SPS dimensions do not match configured coded dimensions",
            ));
        }
        let quant_matrix = crate::avc::idr_quant_matrix(sps_nal, pps_nal)?;
        Self::from_avc_sample(
            target_index,
            sample,
            format,
            coded_size,
            vec![
                Dxva2Buffer {
                    buffer_type: DXVA2_PictureParametersBufferType,
                    data: picture_parameters,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
                Dxva2Buffer {
                    buffer_type: DXVA2_InverseQuantizationMatrixBufferType,
                    data: quant_matrix,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
            ],
        )
    }

    /// Builds an HEVC DXVA2 picture with its VCL bitstream and short slice-control buffers.
    ///
    /// `parameter_buffers` must include the driver-ready HEVC picture-parameter buffer and may
    /// include its scaling-list buffer.
    ///
    /// # Errors
    ///
    /// Returns framing, validation, or allocation errors if sample buffers cannot be constructed.
    pub fn from_hevc_sample(
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        mut parameter_buffers: Vec<Dxva2Buffer>,
    ) -> VideoResult<Self> {
        if parameter_buffers.iter().any(|buffer| {
            buffer.buffer_type == DXVA2_BitStreamDateBufferType
                || buffer.buffer_type == DXVA2_SliceControlBufferType
        }) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_hevc_sample",
                "HEVC parameter buffers must not contain bitstream or slice-control buffers",
            ));
        }
        let [bitstream, slice_control] = build_hevc_slice_buffers(sample, format)?;
        parameter_buffers
            .try_reserve(2)
            .map_err(|_| allocation_error())?;
        parameter_buffers.push(bitstream);
        parameter_buffers.push(slice_control);
        validate_picture_buffers(&parameter_buffers)?;
        Ok(Self {
            target_index,
            buffers: parameter_buffers,
        })
    }

    /// Builds an HEVC IDR DXVA2 picture from VPS/SPS/PPS and an encoded access unit.
    ///
    /// The generated picture parameters cover Main/Main10 4:2:0 IDR I pictures using the
    /// standard scaling matrices. Inter-picture reference state and custom matrices remain
    /// outside this constructor's supported syntax.
    ///
    /// # Errors
    ///
    /// Returns parser, framing, unsupported-syntax, or allocation errors.
    pub fn from_hevc_idr_sample(
        target_index: usize,
        vps_nal: &[u8],
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
    ) -> VideoResult<Self> {
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_hevc_idr_sample",
                "DXVA surface index exceeds u8",
            )
        })?;
        let mut idr_slice = None;
        for nal in codec_nals(sample.data, format, "Dxva2Picture::from_hevc_idr_sample")? {
            if nal.len() < 2 {
                continue;
            }
            let unit_type = (nal[0] >> 1) & 0x3f;
            if unit_type <= 31 {
                if !matches!(unit_type, 19 | 20) {
                    return Err(decode_error(
                        VideoErrorKind::Unsupported,
                        "Dxva2Picture::from_hevc_idr_sample",
                        "this constructor only accepts HEVC IDR access units",
                    ));
                }
                if idr_slice.is_none() {
                    idr_slice = Some(nal);
                }
            }
        }
        let idr_slice = idr_slice.ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_hevc_idr_sample",
                "access unit contains no HEVC IDR slice",
            )
        })?;
        let parameters = crate::hevc::idr_picture_parameters(
            vps_nal,
            sps_nal,
            pps_nal,
            idr_slice,
            surface_index,
        )?;
        let quant_matrix = crate::hevc::idr_quant_matrix(vps_nal, sps_nal, pps_nal)?;
        Self::from_hevc_sample(
            target_index,
            sample,
            format,
            vec![
                Dxva2Buffer {
                    buffer_type: DXVA2_PictureParametersBufferType,
                    data: parameters,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
                Dxva2Buffer {
                    buffer_type: DXVA2_InverseQuantizationMatrixBufferType,
                    data: quant_matrix,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
            ],
        )
    }

    /// Builds an AV1 picture from tile payload bytes and the parsed frame tile descriptors.
    ///
    /// The caller supplies the driver-ready AV1 picture-parameter buffer and parsed tile ranges.
    ///
    /// # Errors
    ///
    /// Returns invalid-input when tile ranges overlap or exceed the payload, or allocation errors.
    pub fn from_av1_tiles(
        target_index: usize,
        tile_payload: &[u8],
        tiles: &[Dxva2Av1Tile],
        mut parameter_buffers: Vec<Dxva2Buffer>,
    ) -> VideoResult<Self> {
        if parameter_buffers.iter().any(|buffer| {
            buffer.buffer_type == DXVA2_BitStreamDateBufferType
                || buffer.buffer_type == DXVA2_SliceControlBufferType
        }) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_av1_tiles",
                "AV1 parameter buffers must not contain bitstream or tile-control buffers",
            ));
        }
        let [bitstream, tile_control] = build_av1_tile_buffers(tile_payload, tiles)?;
        parameter_buffers
            .try_reserve(2)
            .map_err(|_| allocation_error())?;
        parameter_buffers.push(bitstream);
        parameter_buffers.push(tile_control);
        validate_picture_buffers(&parameter_buffers)?;
        Ok(Self {
            target_index,
            buffers: parameter_buffers,
        })
    }

    /// Parses an AV1 key/intra frame header and tile-group OBUs into DXVA2 picture buffers.
    ///
    /// Pass either a `FRAME_HEADER` with complete tile-group OBUs or a combined `OBU_FRAME` with
    /// an empty tile-group list. Stateful inter decoding is provided by
    /// `D3d9Decoder::decode_av1_frame_or_show_existing`.
    ///
    /// # Errors
    ///
    /// Returns parser, framing, unsupported-syntax, or allocation errors.
    pub fn from_av1_intra_obus(
        target_index: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
    ) -> VideoResult<Self> {
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "Dxva2Picture::from_av1_intra_obus",
                "DXVA surface index exceeds u8",
            )
        })?;
        let built = crate::av1::build_intra_dxva_input(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
        )?;
        Self::from_av1_dxva_input(target_index, &built)
    }

    pub(crate) fn from_av1_dxva_input(
        target_index: usize,
        built: &crate::av1::Av1FrameDxvaInput,
    ) -> VideoResult<Self> {
        let mut tiles = Vec::new();
        tiles
            .try_reserve(built.tiles.len())
            .map_err(|_| allocation_error())?;
        tiles.extend(built.tiles.iter().map(|tile| Dxva2Av1Tile {
            data_offset: tile.data_offset,
            data_size: tile.data_size,
            row: tile.row,
            column: tile.column,
        }));
        Self::from_av1_tiles(
            target_index,
            &built.tile_payload,
            &tiles,
            vec![Dxva2Buffer {
                buffer_type: DXVA2_PictureParametersBufferType,
                data: built.picture_parameters.clone(),
                first_macroblock: 0,
                macroblock_count: 0,
            }],
        )
    }
}

/// A decoder instance bound to its codec profile, coded dimensions, and surface pool.
pub struct D3d9Decoder {
    device: D3d9Device,
    decoder: IDirectXVideoDecoder,
    surfaces: Vec<IDirect3DSurface9>,
    coded_size: FrameSize,
    output_format: PixelFormat,
    codec: Codec,
    poisoned: AtomicBool,
}

impl D3d9Decoder {
    /// Configures DXVA2 and initializes the matching codec parameter/DPB state together.
    ///
    /// # Errors
    ///
    /// Returns codec-private parsing, unsupported configuration, surface allocation, or decoder
    /// creation errors.
    pub fn configure_with_state(
        device: D3d9Device,
        config: &crate::DecoderConfig,
        requested_surface_count: u32,
    ) -> VideoResult<(Self, crate::CodecDecodeState)> {
        let state = config.create_codec_state()?;
        let decoder = Self::configure(device, config, requested_surface_count)?;
        Ok((decoder, state))
    }

    /// Creates a DXVA2 decoder using a profile and configuration reported by the driver.
    ///
    /// # Errors
    ///
    /// Returns invalid-input or backend errors if the selected profile, surfaces, or configuration
    /// cannot create a hardware decoder.
    pub fn create(
        device: D3d9Device,
        capability: &D3d9DecodeCapability,
        size: FrameSize,
        config: &windows::Win32::Media::MediaFoundation::DXVA2_ConfigPictureDecode,
        surfaces: Vec<IDirect3DSurface9>,
    ) -> VideoResult<Self> {
        if !device.capabilities().contains(capability) {
            return Err(decode_error(
                VideoErrorKind::Unsupported,
                "D3d9Decoder::create",
                "decode profile and output format were not advertised by this device",
            ));
        }
        if surfaces.is_empty() {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::create",
                "decoder surface pool must not be empty",
            ));
        }
        let decoder = device.create_dxva2_decoder(capability, size, config, &surfaces)?;
        Ok(Self {
            device,
            decoder,
            surfaces,
            coded_size: size,
            output_format: capability.output_format,
            codec: capability.profile.codec(),
            poisoned: AtomicBool::new(false),
        })
    }

    /// Selects a driver decoder configuration and allocates its bound output surface pool.
    ///
    /// The pool is raised to the minimum target count required by the selected DXVA2 config.
    ///
    /// # Errors
    ///
    /// Returns validation, unsupported-profile, driver-config, allocation, or decoder creation
    /// errors.
    pub fn configure(
        device: D3d9Device,
        config: &DecoderConfig,
        requested_surface_count: u32,
    ) -> VideoResult<Self> {
        config.validate()?;
        if requested_surface_count == 0 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::configure",
                "requested surface pool must not be empty",
            ));
        }
        let capability = device
            .capabilities()
            .iter()
            .find(|capability| {
                capability.profile == config.profile && capability.bit_depth == config.bit_depth
            })
            .cloned()
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::Unsupported,
                    "D3d9Decoder::configure",
                    "requested codec profile and bit depth are not supported",
                )
            })?;
        let driver_configs =
            device.decoder_configs(config.profile, config.bit_depth, config.coded_size)?;
        let raw_mode = if config.profile.codec() == Codec::Avc {
            2
        } else {
            1
        };
        let selected_config = driver_configs
            .iter()
            .filter(|driver_config| {
                driver_config.ConfigBitstreamRaw == 1
                    || (config.profile.codec() == Codec::Avc
                        && driver_config.ConfigBitstreamRaw == 2)
            })
            .max_by_key(|driver_config| {
                let raw_score = u8::from(driver_config.ConfigBitstreamRaw == raw_mode);
                let clear_score = u8::from(
                    driver_config.guidConfigBitstreamEncryption
                        == windows::Win32::Media::MediaFoundation::DXVA2_NoEncrypt,
                );
                (clear_score, raw_score)
            })
            .ok_or_else(|| {
            decode_error(
                VideoErrorKind::Unsupported,
                "D3d9Decoder::configure",
                "driver reported no compatible decoder bitstream configuration for coded dimensions",
            )
        })?;
        let surface_count =
            requested_surface_count.max(u32::from(selected_config.ConfigMinRenderTargetBuffCount));
        let surfaces = device.create_surface_pool(
            config.coded_size,
            capability.output_format,
            surface_count,
            DXVA2_VideoDecoderRenderTarget,
        )?;
        Self::create(
            device,
            &capability,
            config.coded_size,
            selected_config,
            surfaces,
        )
    }

    /// Returns the decoder's bound output surfaces.
    #[must_use]
    pub fn surfaces(&self) -> &[IDirect3DSurface9] {
        &self.surfaces
    }

    #[must_use]
    pub const fn coded_size(&self) -> FrameSize {
        self.coded_size
    }

    /// Submits one translated AVC, HEVC, or AV1 picture to the DXVA2 decoder.
    ///
    /// A picture adapter must provide the codec-specific picture-parameter, bitstream, and
    /// slice-control buffers. This method owns their complete `BeginFrame`/`Execute`/`EndFrame`
    /// lifetime and returns a D3D9 event query that can be polled or waited.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, invalid-state, backend, or device-lost errors if validation or any
    /// DXVA2 buffer/frame operation fails.
    pub fn submit_picture(&self, picture: &Dxva2Picture) -> VideoResult<D3d9CompletionToken> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::submit_picture",
                "a prior submitted picture has unknown completion; recreate the decoder",
            ));
        }
        let max_surface_index = if self.codec == Codec::Av1 {
            u8::MAX
        } else {
            0x7f
        };
        if picture.target_index > usize::from(max_surface_index) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_picture",
                "target index exceeds the codec's DXVA picture-entry field",
            ));
        }
        let target = self.surfaces.get(picture.target_index).ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_picture",
                "target index is outside the bound surface pool",
            )
        })?;
        validate_picture_buffers(&picture.buffers)?;

        // SAFETY: The selected target belongs to this decoder's retained target pool.
        unsafe { self.decoder.BeginFrame(target, None) }
            .map_err(|error| backend_error("IDirectXVideoDecoder::BeginFrame", error.code().0))?;

        let submit_result = self.submit_buffers_and_execute(&picture.buffers);
        // EndFrame is required for every successfully begun frame, including failed buffer writes.
        // SAFETY: BeginFrame succeeded above; null requests no completion HANDLE from DXVA2.
        let end_result = unsafe { self.decoder.EndFrame(None) }
            .map_err(|error| backend_error("IDirectXVideoDecoder::EndFrame", error.code().0));
        if let Err(error) = submit_result {
            self.poison_and_retain();
            return Err(error);
        }
        if let Err(error) = end_result {
            self.poison_and_retain();
            return Err(error);
        }
        match self.device.signal_completion() {
            Ok(completion) => Ok(completion),
            Err(error) => {
                self.poison_and_retain();
                Err(error)
            }
        }
    }

    fn poison_and_retain(&self) {
        self.poisoned.store(true, Ordering::Release);
        // Retain the submission set because no event query proves its surfaces can be released.
        std::mem::forget(self.decoder.clone());
        std::mem::forget(self.device.clone());
        for surface in &self.surfaces {
            std::mem::forget(surface.clone());
        }
    }

    /// Submits a picture and returns its retained output surface with timing and color metadata.
    ///
    /// The caller must avoid selecting a surface again while the returned frame or a downstream
    /// operation still uses it. The completion token reports when decode commands have finished.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, backend, or device-lost errors when submission fails.
    pub fn submit_picture_frame(
        &self,
        picture: &Dxva2Picture,
        timing: SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        let surface = self
            .surfaces
            .get(picture.target_index)
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d9Decoder::submit_picture_frame",
                    "target index is outside the bound surface pool",
                )
            })?
            .clone();
        let completion = self.submit_picture(picture)?;
        Ok(D3d9DecodedFrame {
            surface,
            surface_index: picture.target_index,
            completion,
            info: FrameInfo {
                size: self.coded_size,
                format: self.output_format,
                color,
                pts: timing.pts,
                duration: timing.duration,
                time_base: timing.time_base,
            },
            sample_aspect: crate::SampleAspectRatio::SQUARE,
            surface_lease: None,
            retained_source_lease: None,
            retained_source_surface: None,
        })
    }

    /// Builds and submits one supported AVC IDR access unit to the selected surface.
    ///
    /// This connects SPS/PPS and Annex B or length-prefixed input parsing to the DXVA2 transport.
    /// The current AVC parameter translator supports progressive 8-bit 4:2:0 IDR pictures.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn submit_avc_idr_sample(
        &self,
        target_index: usize,
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
    ) -> VideoResult<D3d9CompletionToken> {
        let picture = Dxva2Picture::from_avc_idr_sample(
            target_index,
            sps_nal,
            pps_nal,
            sample,
            format,
            self.coded_size,
        )?;
        self.submit_picture(&picture)
    }

    /// Decodes a supported AVC IDR access unit and returns its output surface frame.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn decode_avc_idr_sample(
        &self,
        target_index: usize,
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        let timing = sample.timing;
        let picture = Dxva2Picture::from_avc_idr_sample(
            target_index,
            sps_nal,
            pps_nal,
            sample,
            format,
            self.coded_size,
        )?;
        self.submit_picture_frame(&picture, timing, color)
    }

    /// Decodes an AVC access unit using caller-retained parameter and reference-picture state.
    ///
    /// Supports progressive 8-bit 4:2:0 I/P/B frame slices. State advances only after the DXVA2
    /// submission succeeds. The caller chooses an output surface not retained by a live reference.
    ///
    /// # Errors
    ///
    /// Returns parsing, missing-reference, unsupported-syntax, invalid-state, backend, or
    /// device-lost errors.
    pub fn decode_avc_sample(
        &self,
        state: &mut AvcDecodeState,
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        if self.codec != Codec::Avc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::decode_avc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_avc_sample",
                "DXVA H.264 surface index exceeds u8",
            )
        })?;
        let expected_mbs = (
            self.coded_size.width().div_ceil(16),
            self.coded_size.height().div_ceil(16),
        );
        if state.dimensions_in_mbs() != expected_mbs {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_avc_sample",
                "AVC SPS dimensions do not match the configured decoder size",
            ));
        }
        let nals = codec_nals(sample.data, format, "D3d9Decoder::decode_avc_sample")?;
        let mut slices = Vec::new();
        for nal in nals {
            state.validate_parameter_set(nal)?;
            if nal
                .first()
                .is_some_and(|header| matches!(header & 0x1f, 1 | 5))
            {
                slices.try_reserve(1).map_err(|_| allocation_error())?;
                slices.push(nal);
            }
        }
        let prepared = state.prepare(&slices, surface_index)?;
        let quant_matrix = state.quant_matrix()?;
        let picture = Dxva2Picture::from_avc_sample(
            target_index,
            sample,
            format,
            self.coded_size,
            vec![
                Dxva2Buffer {
                    buffer_type: DXVA2_PictureParametersBufferType,
                    data: prepared.picture_parameters.clone(),
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
                Dxva2Buffer {
                    buffer_type: DXVA2_InverseQuantizationMatrixBufferType,
                    data: quant_matrix,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
            ],
        )?;
        let frame = self.submit_picture_frame(&picture, sample.timing, color)?;
        state.commit(prepared);
        Ok(frame)
    }

    /// Decodes one supported HEVC access unit using caller-retained VPS/SPS/PPS and DPB state.
    ///
    /// The current parser accepts one first-slice segment per access unit. State advances only
    /// after DXVA2 submission succeeds. The target surface must not contain a retained reference.
    ///
    /// # Errors
    ///
    /// Returns parsing, missing-reference, unsupported-syntax, invalid-state, backend, or
    /// device-lost errors.
    pub fn decode_hevc_sample(
        &self,
        state: &mut HevcDecodeState,
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        if self.codec != Codec::Hevc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::decode_hevc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_hevc_sample",
                "DXVA HEVC surface index exceeds u8",
            )
        })?;
        if state.dimensions() != (self.coded_size.width(), self.coded_size.height()) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_hevc_sample",
                "HEVC SPS dimensions do not match the configured decoder size",
            ));
        }
        let mut slice_nals = Vec::new();
        for nal in codec_nals(sample.data, format, "D3d9Decoder::decode_hevc_sample")? {
            state.validate_parameter_set(nal)?;
            if nal.len() >= 2 && (nal[0] >> 1) & 0x3f <= 31 {
                slice_nals.try_reserve(1).map_err(|_| allocation_error())?;
                slice_nals.push(nal);
            }
        }
        if slice_nals.is_empty() {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_hevc_sample",
                "access unit contains no HEVC VCL slice",
            ));
        }
        let prepared = state.prepare(&slice_nals, surface_index)?;
        let quant_matrix = state.quant_matrix()?;
        let picture = Dxva2Picture::from_hevc_sample(
            target_index,
            sample,
            format,
            vec![
                Dxva2Buffer {
                    buffer_type: DXVA2_PictureParametersBufferType,
                    data: prepared.picture_parameters.clone(),
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
                Dxva2Buffer {
                    buffer_type: DXVA2_InverseQuantizationMatrixBufferType,
                    data: quant_matrix,
                    first_macroblock: 0,
                    macroblock_count: 0,
                },
            ],
        )?;
        let frame = self.submit_picture_frame(&picture, sample.timing, color)?;
        state.commit(prepared);
        Ok(frame)
    }

    /// Builds and submits one supported HEVC IDR access unit to the selected surface.
    ///
    /// The current HEVC parameter translator supports Main/Main10 4:2:0 IDR I pictures with
    /// standard scaling matrices.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn submit_hevc_idr_sample(
        &self,
        target_index: usize,
        vps_nal: &[u8],
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
    ) -> VideoResult<D3d9CompletionToken> {
        let picture = Dxva2Picture::from_hevc_idr_sample(
            target_index,
            vps_nal,
            sps_nal,
            pps_nal,
            sample,
            format,
        )?;
        self.submit_picture(&picture)
    }

    /// Decodes a supported HEVC IDR access unit and returns its output surface frame.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    #[expect(
        clippy::too_many_arguments,
        reason = "HEVC requires three distinct parameter-set NALs plus sample framing and metadata"
    )]
    pub fn decode_hevc_idr_sample(
        &self,
        target_index: usize,
        vps_nal: &[u8],
        sps_nal: &[u8],
        pps_nal: &[u8],
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        let timing = sample.timing;
        let picture = Dxva2Picture::from_hevc_idr_sample(
            target_index,
            vps_nal,
            sps_nal,
            pps_nal,
            sample,
            format,
        )?;
        self.submit_picture_frame(&picture, timing, color)
    }

    /// Parses and submits one AV1 key or intra-only frame to the selected surface.
    ///
    /// Tile-group OBUs must cover the complete frame in raster order. Inter frames and
    /// show-existing-frame headers require reference state and are rejected.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn submit_av1_intra_obus(
        &self,
        target_index: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
    ) -> VideoResult<D3d9CompletionToken> {
        if self.codec != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::submit_av1_intra_obus",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_av1_intra_obus",
                "AV1 decoder surface index exceeds u8",
            )
        })?;
        let built = crate::av1::build_intra_dxva_input(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
        )?;
        if u32::from(built.width) != self.coded_size.width()
            || u32::from(built.height) != self.coded_size.height()
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_av1_intra_obus",
                "AV1 frame dimensions do not match configured decoder dimensions",
            ));
        }
        let picture = Dxva2Picture::from_av1_dxva_input(target_index, &built)?;
        self.submit_picture(&picture)
    }

    /// Decodes an AV1 key/intra frame and returns its output surface frame.
    ///
    /// # Errors
    ///
    /// Returns parsing, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn decode_av1_intra_obus(
        &self,
        target_index: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
        timing: SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        if self.codec != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::decode_av1_intra_obus",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_av1_intra_obus",
                "AV1 decoder surface index exceeds u8",
            )
        })?;
        let built = crate::av1::build_intra_dxva_input(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
        )?;
        self.submit_av1_dxva_input(target_index, &built, timing, color)
    }

    fn submit_av1_dxva_input(
        &self,
        target_index: usize,
        built: &crate::av1::Av1FrameDxvaInput,
        timing: SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        if u32::from(built.width) != self.coded_size.width()
            || u32::from(built.height) != self.coded_size.height()
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_av1_dxva_input",
                "AV1 frame dimensions do not match configured decoder dimensions",
            ));
        }
        let picture = Dxva2Picture::from_av1_dxva_input(target_index, built)?;
        self.submit_picture_frame(&picture, timing, color)
    }

    /// Decodes an AV1 key/intra picture or supported error-resilient inter frame, or redisplays a
    /// previously decoded reference slot. Reference slots advance only after successful submission.
    ///
    /// # Errors
    ///
    /// Returns parsing, missing-reference, unsupported-syntax, invalid-input, backend, or
    /// device-lost errors.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame submit inputs mirror distinct AV1 OBU and timing data"
    )]
    pub fn decode_av1_frame_or_show_existing(
        &self,
        state: &mut crate::Av1DecodeState,
        target_index: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
        timing: SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d9DecodedFrame> {
        if self.codec != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::decode_av1_frame_or_show_existing",
                "decoder was configured for a different codec",
            ));
        }
        if let Some(surface_index) =
            state.show_existing_surface(sequence_header_obu, frame_header_obu)?
        {
            let surface = self
                .surfaces
                .get(usize::from(surface_index))
                .ok_or_else(|| {
                    decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d9Decoder::decode_av1_frame_or_show_existing",
                        "AV1 reference slot names a surface outside this decoder pool",
                    )
                })?;
            let completion = self.device.signal_completion()?;
            let render_size = state.surface_render_size(surface_index)?;
            return Ok(D3d9DecodedFrame {
                surface: surface.clone(),
                surface_index: usize::from(surface_index),
                completion,
                info: FrameInfo {
                    size: self.coded_size,
                    format: self.output_format,
                    color,
                    pts: timing.pts,
                    duration: timing.duration,
                    time_base: timing.time_base,
                },
                sample_aspect: crate::SampleAspectRatio::from_render_size(
                    self.coded_size,
                    render_size,
                )?,
                surface_lease: None,
                retained_source_lease: None,
                retained_source_surface: None,
            });
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::decode_av1_frame_or_show_existing",
                "AV1 decoder surface index exceeds u8",
            )
        })?;
        let built = crate::av1::build_intra_dxva_input_with_slots(
            sequence_header_obu,
            frame_header_obu,
            tile_group_obus,
            surface_index,
            state.slots(),
        )?;
        if !state.can_overwrite_surface(surface_index, built.refresh_frame_flags) {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9Decoder::decode_av1_frame_or_show_existing",
                "target surface still backs an AV1 slot not refreshed by this frame",
            ));
        }
        let mut frame = self.submit_av1_dxva_input(target_index, &built, timing, color)?;
        state.commit_frame(&built, surface_index);
        let render_size = state.surface_render_size(surface_index)?;
        frame.sample_aspect =
            crate::SampleAspectRatio::from_render_size(self.coded_size, render_size)?;
        Ok(frame)
    }

    fn submit_buffers_and_execute(&self, buffers: &[Dxva2Buffer]) -> VideoResult<()> {
        let mut descriptions = Vec::new();
        descriptions
            .try_reserve_exact(buffers.len())
            .map_err(|_| allocation_error())?;
        for buffer in buffers {
            let byte_count = u32::try_from(buffer.data.len()).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d9Decoder::submit_picture",
                    "DXVA2 buffer exceeds the 32-bit API size limit",
                )
            })?;
            copy_to_dxva_buffer(&self.decoder, buffer, byte_count)?;
            descriptions.push(DXVA2_DecodeBufferDesc {
                CompressedBufferType: buffer.buffer_type.0.cast_unsigned(),
                DataSize: byte_count,
                FirstMBaddress: buffer.first_macroblock,
                NumMBsInBuffer: buffer.macroblock_count,
                ..DXVA2_DecodeBufferDesc::default()
            });
        }
        let execute = DXVA2_DecodeExecuteParams {
            NumCompBuffers: u32::try_from(descriptions.len()).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d9Decoder::submit_picture",
                    "DXVA2 buffer count exceeds the 32-bit API limit",
                )
            })?,
            pCompressedBuffers: descriptions.as_mut_ptr(),
            pExtensionData: ptr::null_mut(),
        };
        // SAFETY: Buffer descriptions and pointed-to driver buffers stay alive for the call.
        unsafe { self.decoder.Execute(&raw const execute) }
            .map_err(|error| backend_error("IDirectXVideoDecoder::Execute", error.code().0))?;
        Ok(())
    }
}

/// Converts one encoded sample to the framing used by DXVA2 AVC/HEVC bitstream buffers.
///
/// NAL samples are emitted as three-byte Annex B start codes followed by each original NAL. AV1
/// samples keep their low-overhead OBU byte representation after structural validation.
///
/// # Errors
///
/// Returns invalid-input when the codec and framing disagree or a NAL/OBU is malformed; returns a
/// backend error if the output allocation cannot be reserved.
pub fn build_dxva2_bitstream(
    sample: EncodedSample<'_>,
    codec: Codec,
    format: BitstreamFormat,
) -> VideoResult<Vec<u8>> {
    sample.validate()?;
    match (codec, format) {
        (Codec::Avc | Codec::Hevc, BitstreamFormat::AnnexB) => {
            let mut output = Vec::new();
            for unit in crate::nal::AnnexBNalIter::new(sample.data)? {
                append_nal(&mut output, unit.data)?;
            }
            ensure_bitstream_nonempty(output)
        }
        (Codec::Avc | Codec::Hevc, BitstreamFormat::LengthPrefixed { length_size }) => {
            let mut output = Vec::new();
            for unit in crate::nal::LengthPrefixedNalIter::new(sample.data, length_size)? {
                append_nal(&mut output, unit?.data)?;
            }
            ensure_bitstream_nonempty(output)
        }
        (Codec::Av1, BitstreamFormat::Av1Obu | BitstreamFormat::Av1Isobmff) => {
            for obu in crate::obu::ObuIter::new(sample.data) {
                obu?;
            }
            let mut output = Vec::new();
            output
                .try_reserve_exact(sample.data.len())
                .map_err(|_| allocation_error())?;
            output.extend_from_slice(sample.data);
            Ok(output)
        }
        _ => Err(decode_error(
            VideoErrorKind::InvalidInput,
            "build_dxva2_bitstream",
            "codec and bitstream framing do not match",
        )),
    }
}

/// Builds AVC DXVA2 bitstream and short slice-control buffers from one access unit.
///
/// Only VCL NAL units (types 1 and 5) are submitted, matching `FFmpeg`'s DXVA2 H.264 path. Each
/// short slice record points at a three-byte Annex B prefix; the bitstream is zero-padded to a
/// 128-byte boundary and the final record includes that padding in its byte count.
///
/// # Errors
///
/// Returns framing errors, invalid-input for access units without AVC slices, or overflow/allocation
/// errors while building the DXVA buffers.
pub fn build_avc_slice_buffers(
    sample: EncodedSample<'_>,
    format: BitstreamFormat,
    coded_size: FrameSize,
) -> VideoResult<[Dxva2Buffer; 2]> {
    let macroblock_width = coded_size.width().div_ceil(16);
    let macroblock_height = coded_size.height().div_ceil(16);
    let macroblock_count = macroblock_width
        .checked_mul(macroblock_height)
        .ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "build_avc_slice_buffers",
                "macroblock count overflowed",
            )
        })?;
    build_nal_slice_buffers(sample, format, Codec::Avc, macroblock_count)
}

/// Builds HEVC DXVA2 bitstream and short slice-control buffers from one access unit.
///
/// The DXVA2 HEVC path uses the same 10-byte short slice records as AVC, while its bitstream
/// descriptor reports zero macroblocks.
///
/// # Errors
///
/// Returns framing errors, invalid-input for access units without HEVC slices, or overflow/allocation
/// errors while building the DXVA buffers.
pub fn build_hevc_slice_buffers(
    sample: EncodedSample<'_>,
    format: BitstreamFormat,
) -> VideoResult<[Dxva2Buffer; 2]> {
    build_nal_slice_buffers(sample, format, Codec::Hevc, 0)
}

/// Builds an AV1 DXVA2 bitstream and tile-control buffer from frame tile payload metadata.
///
/// The tile payload is zero-padded to 128 bytes. Tile ranges must be nonempty, nonoverlapping, and
/// wholly contained in the supplied payload; row/column coordinates must be unique.
///
/// # Errors
///
/// Returns invalid-input for malformed tile descriptors or allocation errors.
pub fn build_av1_tile_buffers(
    tile_payload: &[u8],
    tiles: &[Dxva2Av1Tile],
) -> VideoResult<[Dxva2Buffer; 2]> {
    if tile_payload.is_empty() || tiles.is_empty() || tiles.len() > 256 {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "build_av1_tile_buffers",
            "AV1 tile payload and one through 256 tile descriptors are required",
        ));
    }
    for (index, tile) in tiles.iter().enumerate() {
        let end = tile
            .data_offset
            .checked_add(tile.data_size)
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "build_av1_tile_buffers",
                    "tile byte range overflowed",
                )
            })?;
        if tile.data_size == 0 || usize::try_from(end).map_or(true, |end| end > tile_payload.len())
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "build_av1_tile_buffers",
                "tile byte range is empty or exceeds the tile payload",
            ));
        }
        if tiles[..index].iter().any(|previous| {
            let previous_end = previous.data_offset.saturating_add(previous.data_size);
            let same_coordinate = previous.row == tile.row && previous.column == tile.column;
            let overlaps = tile.data_offset < previous_end && previous.data_offset < end;
            same_coordinate || overlaps
        }) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "build_av1_tile_buffers",
                "AV1 tile ranges overlap or coordinates repeat",
            ));
        }
    }

    let padding = 128 - tile_payload.len() % 128;
    let padded_size = tile_payload
        .len()
        .checked_add(padding)
        .ok_or_else(allocation_error)?;
    let mut bitstream = Vec::new();
    bitstream
        .try_reserve_exact(padded_size)
        .map_err(|_| allocation_error())?;
    bitstream.extend_from_slice(tile_payload);
    bitstream.resize(padded_size, 0);

    let table_size = tiles.len().checked_mul(16).ok_or_else(allocation_error)?;
    let mut tile_data = Vec::new();
    tile_data
        .try_reserve_exact(table_size)
        .map_err(|_| allocation_error())?;
    for tile in tiles {
        tile_data.extend_from_slice(&tile.data_offset.to_le_bytes());
        tile_data.extend_from_slice(&tile.data_size.to_le_bytes());
        tile_data.extend_from_slice(&tile.row.to_le_bytes());
        tile_data.extend_from_slice(&tile.column.to_le_bytes());
        tile_data.extend_from_slice(&0_u16.to_le_bytes());
        tile_data.push(0xff);
        tile_data.push(0);
    }
    Ok([
        Dxva2Buffer {
            buffer_type: DXVA2_BitStreamDateBufferType,
            data: bitstream,
            first_macroblock: 0,
            macroblock_count: 0,
        },
        Dxva2Buffer {
            buffer_type: DXVA2_SliceControlBufferType,
            data: tile_data,
            first_macroblock: 0,
            macroblock_count: 0,
        },
    ])
}

#[expect(
    clippy::too_many_lines,
    reason = "AVC/HEVC access-unit assembly follows FFmpeg's common DXVA buffer ordering"
)]
fn build_nal_slice_buffers(
    sample: EncodedSample<'_>,
    format: BitstreamFormat,
    codec: Codec,
    macroblock_count: u32,
) -> VideoResult<[Dxva2Buffer; 2]> {
    sample.validate()?;
    let operation = match codec {
        Codec::Avc => "build_avc_slice_buffers",
        Codec::Hevc => "build_hevc_slice_buffers",
        Codec::Av1 => {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "build_nal_slice_buffers",
                "AV1 does not use NAL slices",
            ));
        }
    };
    let mut bitstream = Vec::new();
    let mut slices = Vec::new();
    for nal in codec_nals(sample.data, format, operation)? {
        let Some(&header) = nal.first() else {
            continue;
        };
        if header & 0x80 != 0 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                operation,
                "NAL forbidden_zero_bit is set",
            ));
        }
        let is_slice = match codec {
            Codec::Avc => matches!(header & 0x1f, 1 | 5),
            Codec::Hevc => {
                if nal.len() < 2 || nal[1].trailing_zeros() >= 3 {
                    return Err(decode_error(
                        VideoErrorKind::InvalidInput,
                        operation,
                        "HEVC NAL header is truncated or temporal_id_plus1 is zero",
                    ));
                }
                let layer_id = ((header & 1) << 5) | (nal[1] >> 3);
                if layer_id != 0 {
                    return Err(decode_error(
                        VideoErrorKind::Unsupported,
                        operation,
                        "HEVC enhancement layers are unsupported",
                    ));
                }
                (header >> 1) & 0x3f <= 31
            }
            Codec::Av1 => false,
        };
        if !is_slice {
            continue;
        }
        let offset = u32::try_from(bitstream.len()).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                operation,
                "bitstream offset exceeds u32",
            )
        })?;
        let nal_size = u32::try_from(nal.len()).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                operation,
                "NAL size exceeds u32",
            )
        })?;
        let slice_size = nal_size.checked_add(3).ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                operation,
                "slice size overflowed",
            )
        })?;
        bitstream
            .try_reserve(nal.len().saturating_add(3))
            .map_err(|_| allocation_error())?;
        bitstream.extend_from_slice(&[0, 0, 1]);
        bitstream.extend_from_slice(nal);
        slices.try_reserve(1).map_err(|_| allocation_error())?;
        slices.push((offset, slice_size));
    }
    if slices.is_empty() {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            operation,
            "access unit contains no supported VCL NAL units",
        ));
    }

    let padding = 128 - bitstream.len() % 128;
    bitstream
        .try_reserve(padding)
        .map_err(|_| allocation_error())?;
    bitstream.resize(bitstream.len() + padding, 0);
    if let Some((_, final_size)) = slices.last_mut() {
        *final_size = final_size
            .checked_add(u32::try_from(padding).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    operation,
                    "padding exceeds u32",
                )
            })?)
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    operation,
                    "final slice size overflowed",
                )
            })?;
    }

    let mut slice_data = Vec::new();
    let byte_len = slices.len().checked_mul(10).ok_or_else(allocation_error)?;
    slice_data
        .try_reserve_exact(byte_len)
        .map_err(|_| allocation_error())?;
    for (offset, size) in slices {
        slice_data.extend_from_slice(&offset.to_le_bytes());
        slice_data.extend_from_slice(&size.to_le_bytes());
        slice_data.extend_from_slice(&0_u16.to_le_bytes());
    }
    Ok([
        Dxva2Buffer {
            buffer_type: DXVA2_BitStreamDateBufferType,
            data: bitstream,
            first_macroblock: 0,
            macroblock_count,
        },
        Dxva2Buffer {
            buffer_type: DXVA2_SliceControlBufferType,
            data: slice_data,
            first_macroblock: 0,
            macroblock_count,
        },
    ])
}

pub(crate) fn codec_nals<'data>(
    data: &'data [u8],
    format: BitstreamFormat,
    operation: &'static str,
) -> VideoResult<Vec<&'data [u8]>> {
    let mut nals = Vec::new();
    match format {
        BitstreamFormat::AnnexB => {
            for nal in crate::nal::AnnexBNalIter::new(data)? {
                nals.try_reserve(1).map_err(|_| allocation_error())?;
                nals.push(nal.data);
            }
        }
        BitstreamFormat::LengthPrefixed { length_size } => {
            for nal in crate::nal::LengthPrefixedNalIter::new(data, length_size)? {
                nals.try_reserve(1).map_err(|_| allocation_error())?;
                nals.push(nal?.data);
            }
        }
        _ => {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                operation,
                "AVC/HEVC requires Annex B or length-prefixed NAL framing",
            ));
        }
    }
    Ok(nals)
}

fn append_nal(output: &mut Vec<u8>, nal: &[u8]) -> VideoResult<()> {
    output
        .try_reserve(nal.len().saturating_add(3))
        .map_err(|_| allocation_error())?;
    output.extend_from_slice(&[0, 0, 1]);
    output.extend_from_slice(nal);
    Ok(())
}

fn ensure_bitstream_nonempty(output: Vec<u8>) -> VideoResult<Vec<u8>> {
    if output.is_empty() {
        Err(decode_error(
            VideoErrorKind::InvalidInput,
            "build_dxva2_bitstream",
            "sample contains no NAL units",
        ))
    } else {
        Ok(output)
    }
}

fn validate_picture_buffers(buffers: &[Dxva2Buffer]) -> VideoResult<()> {
    if buffers.is_empty() {
        return Err(decode_error(
            VideoErrorKind::InvalidInput,
            "D3d9Decoder::submit_picture",
            "picture must contain at least one DXVA2 buffer",
        ));
    }
    for (index, buffer) in buffers.iter().enumerate() {
        if buffer.data.is_empty() {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_picture",
                "DXVA2 buffers must not be empty",
            ));
        }
        if buffers[..index]
            .iter()
            .any(|previous| previous.buffer_type == buffer.buffer_type)
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9Decoder::submit_picture",
                "a picture cannot submit the same DXVA2 buffer type twice",
            ));
        }
    }
    Ok(())
}

fn copy_to_dxva_buffer(
    decoder: &IDirectXVideoDecoder,
    buffer: &Dxva2Buffer,
    byte_count: u32,
) -> VideoResult<()> {
    let mut destination: *mut c_void = ptr::null_mut();
    let mut capacity = 0;
    // SAFETY: Output pointers are writable and the buffer type is supplied by the codec adapter.
    unsafe { decoder.GetBuffer(buffer.buffer_type, &raw mut destination, &raw mut capacity) }
        .map_err(|error| backend_error("IDirectXVideoDecoder::GetBuffer", error.code().0))?;

    let copy_result = if destination.is_null() {
        Err(decode_error(
            VideoErrorKind::Backend,
            "IDirectXVideoDecoder::GetBuffer",
            "driver returned a null buffer pointer",
        ))
    } else if byte_count > capacity {
        Err(decode_error(
            VideoErrorKind::InvalidInput,
            "IDirectXVideoDecoder::GetBuffer",
            "driver buffer capacity is smaller than the codec payload",
        ))
    } else {
        // SAFETY: `data` has `byte_count` initialized bytes and the driver reported at least that
        // much writable capacity in the live GetBuffer allocation.
        unsafe {
            ptr::copy_nonoverlapping(
                buffer.data.as_ptr(),
                destination.cast(),
                byte_count as usize,
            );
        };
        Ok(())
    };
    // ReleaseBuffer is required even when capacity validation/copy setup fails.
    // SAFETY: GetBuffer succeeded above for this exact buffer type.
    let release_result = unsafe { decoder.ReleaseBuffer(buffer.buffer_type.0.cast_unsigned()) }
        .map_err(|error| backend_error("IDirectXVideoDecoder::ReleaseBuffer", error.code().0));
    copy_result?;
    release_result
}

fn decode_error(
    kind: VideoErrorKind,
    operation: &'static str,
    message: &'static str,
) -> VideoError {
    VideoError::new(kind, operation, message)
}

fn allocation_error() -> VideoError {
    decode_error(
        VideoErrorKind::Backend,
        "D3d9Decoder::submit_picture",
        "unable to allocate DXVA2 buffer descriptions",
    )
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    let kind = crate::error::backend_kind_from_hresult(code);
    VideoError::new(kind, operation, "DXVA2 decode operation failed").with_backend_code(code)
}

/// Stateful DXVA2 adapter for the shared send/receive decoder contract.
pub struct D3d9DecodeSession {
    device: D3d9Device,
    surface_count: u32,
    capabilities: Vec<DecodeCapability>,
    decoder: Option<D3d9Decoder>,
    state: Option<CodecDecodeState>,
    config: Option<DecoderConfig>,
    av1_sequence_header: Option<Vec<u8>>,
    pending: VecDeque<D3d9DecodedFrame>,
    surface_leases: Vec<Arc<AtomicUsize>>,
    draining: bool,
}

impl D3d9DecodeSession {
    /// Creates a session shell for one DXVA2 device; call `configure` before sending samples.
    ///
    /// # Errors
    ///
    /// Returns invalid-input when the requested surface count is zero.
    pub fn new(device: D3d9Device, surface_count: u32) -> VideoResult<Self> {
        if surface_count == 0 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9DecodeSession::new",
                "surface count must be nonzero",
            ));
        }
        let min_size = FrameSize::new(1, 1)?;
        let max_size = FrameSize::new(16_384, 16_384)?;
        let capabilities = device
            .capabilities()
            .iter()
            .map(|capability| DecodeCapability {
                profile: capability.profile,
                bit_depth: capability.bit_depth,
                chroma_sampling: capability.chroma_sampling,
                output_formats: vec![capability.output_format],
                min_size,
                max_size,
            })
            .collect();
        Ok(Self {
            device,
            surface_count,
            capabilities,
            decoder: None,
            state: None,
            config: None,
            av1_sequence_header: None,
            pending: VecDeque::new(),
            surface_leases: (0..surface_count)
                .map(|_| Arc::new(AtomicUsize::new(0)))
                .collect(),
            draining: false,
        })
    }

    fn choose_target_surface(&self) -> VideoResult<usize> {
        let decoder = self.decoder.as_ref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::send_sample",
                "decoder is not configured",
            )
        })?;
        let state = self.state.as_ref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::send_sample",
                "codec state is not configured",
            )
        })?;
        (0..decoder.surfaces.len().min(128))
            .find(|candidate| {
                if self.surface_leases[*candidate].load(Ordering::Acquire) != 0 {
                    return false;
                }
                if self
                    .pending
                    .iter()
                    .any(|frame| frame.surface_index == *candidate)
                {
                    return false;
                }
                let Ok(surface) = u8::try_from(*candidate) else {
                    return false;
                };
                match state {
                    CodecDecodeState::Avc(state) => state
                        .reference_surfaces()
                        .iter()
                        .all(|(_, reference)| *reference != surface),
                    CodecDecodeState::Hevc(state) => state
                        .reference_surfaces()
                        .iter()
                        .all(|(_, reference)| *reference != surface),
                    CodecDecodeState::Av1(state) => state.can_overwrite_surface(surface, 0),
                }
            })
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d9DecodeSession::send_sample",
                    "no DXVA2 surface is free of codec references and queued output frames",
                )
            })
    }

    fn decode_av1_sample(
        &mut self,
        target_surface: usize,
        sample: EncodedSample<'_>,
    ) -> VideoResult<D3d9DecodedFrame> {
        let mut frame_header = None;
        let mut tile_groups = Vec::new();
        for obu in crate::obu::ObuIter::new(sample.data) {
            let obu = obu?;
            match obu.kind {
                crate::obu::ObuType::FRAME_HEADER
                | crate::obu::ObuType::REDUNDANT_FRAME_HEADER
                | crate::obu::ObuType::FRAME => {
                    if frame_header.replace(obu.raw).is_some() {
                        return Err(decode_error(
                            VideoErrorKind::InvalidInput,
                            "D3d9DecodeSession::decode_av1_sample",
                            "sample contains multiple AV1 frame headers",
                        ));
                    }
                }
                crate::obu::ObuType::TILE_GROUP => tile_groups.push(obu.raw),
                _ => {}
            }
        }
        let frame_header = frame_header.ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d9DecodeSession::decode_av1_sample",
                "sample contains no AV1 frame-header OBU",
            )
        })?;
        let sequence_header = self.av1_sequence_header.as_deref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::decode_av1_sample",
                "AV1 sequence header is not configured",
            )
        })?;
        let Some(CodecDecodeState::Av1(state)) = self.state.as_mut() else {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::decode_av1_sample",
                "AV1 codec state is not configured",
            ));
        };
        self.decoder
            .as_ref()
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d9DecodeSession::decode_av1_sample",
                    "decoder is not configured",
                )
            })?
            .decode_av1_frame_or_show_existing(
                state,
                target_surface,
                sequence_header,
                frame_header,
                &tile_groups,
                sample.timing,
                ColorInfo::default(),
            )
    }
}

impl D3dDecode for D3d9DecodeSession {
    type Frame = D3d9DecodedFrame;

    fn capabilities(&self) -> &[DecodeCapability] {
        &self.capabilities
    }

    fn configure(&mut self, config: &DecoderConfig) -> VideoResult<()> {
        let state = config.create_codec_state()?;
        let codec_private = config.parse_codec_private()?;
        let av1_sequence_header = match codec_private {
            crate::CodecPrivateData::Av1 {
                sequence_header_obu,
            } => Some(sequence_header_obu),
            _ => None,
        };
        let decoder = D3d9Decoder::configure(self.device.clone(), config, self.surface_count)?;
        self.pending.clear();
        self.decoder = Some(decoder);
        self.state = Some(state);
        self.config = Some(config.clone());
        self.av1_sequence_header = av1_sequence_header;
        self.draining = false;
        Ok(())
    }

    fn send_sample(&mut self, sample: EncodedSample<'_>) -> VideoResult<()> {
        if self.draining {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::send_sample",
                "input was already drained; call flush before resuming",
            ));
        }
        sample.validate()?;
        let (codec, bitstream_format) = self
            .config
            .as_ref()
            .map(|config| (config.profile.codec(), config.bitstream_format))
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d9DecodeSession::send_sample",
                    "decoder is not configured",
                )
            })?;
        let target = self.choose_target_surface()?;
        let mut frame = match codec {
            Codec::Avc => {
                let Some(CodecDecodeState::Avc(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d9DecodeSession::send_sample",
                        "AVC codec state is not configured",
                    ));
                };
                self.decoder
                    .as_ref()
                    .ok_or_else(|| {
                        decode_error(
                            VideoErrorKind::InvalidState,
                            "D3d9DecodeSession::send_sample",
                            "decoder is not configured",
                        )
                    })?
                    .decode_avc_sample(
                        state,
                        target,
                        sample,
                        bitstream_format,
                        ColorInfo::default(),
                    )?
            }
            Codec::Hevc => {
                let Some(CodecDecodeState::Hevc(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d9DecodeSession::send_sample",
                        "HEVC codec state is not configured",
                    ));
                };
                self.decoder
                    .as_ref()
                    .ok_or_else(|| {
                        decode_error(
                            VideoErrorKind::InvalidState,
                            "D3d9DecodeSession::send_sample",
                            "decoder is not configured",
                        )
                    })?
                    .decode_hevc_sample(
                        state,
                        target,
                        sample,
                        bitstream_format,
                        ColorInfo::default(),
                    )?
            }
            Codec::Av1 => self.decode_av1_sample(target, sample)?,
        };
        if let Some(counter) = self.surface_leases.get(frame.surface_index).cloned() {
            frame.surface_lease = Some(crate::frame::SurfaceLease::new(counter));
        }
        self.pending
            .try_reserve(1)
            .map_err(|_| allocation_error())?;
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> VideoResult<Option<Self::Frame>> {
        Ok(self.pending.pop_front())
    }

    fn drain(&mut self) -> VideoResult<()> {
        if self.decoder.is_none() {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d9DecodeSession::drain",
                "decoder is not configured",
            ));
        }
        self.draining = true;
        Ok(())
    }

    fn flush(&mut self) -> VideoResult<()> {
        self.pending.clear();
        if let Some(state) = &mut self.state {
            state.reset();
        }
        self.draining = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Dxva2Av1Tile, Dxva2Buffer, Dxva2Picture, build_av1_tile_buffers, build_avc_slice_buffers,
        build_dxva2_bitstream, build_hevc_slice_buffers, validate_picture_buffers,
    };
    use crate::{BitstreamFormat, Codec, EncodedSample, SampleTiming, TimeBase};
    use windows::Win32::Media::MediaFoundation::{
        DXVA2_BitStreamDateBufferType, DXVA2_PictureParametersBufferType,
    };

    fn buffer(
        buffer_type: windows::Win32::Media::MediaFoundation::DXVA2_BufferfType,
    ) -> Dxva2Buffer {
        Dxva2Buffer {
            buffer_type,
            data: vec![1],
            first_macroblock: 0,
            macroblock_count: 1,
        }
    }

    #[test]
    fn requires_payload_and_unique_buffer_kinds() {
        assert!(validate_picture_buffers(&[]).is_err());
        assert!(validate_picture_buffers(&[buffer(DXVA2_PictureParametersBufferType)]).is_ok());
        assert!(
            validate_picture_buffers(&[
                buffer(DXVA2_PictureParametersBufferType),
                buffer(DXVA2_BitStreamDateBufferType),
            ])
            .is_ok()
        );
        assert!(
            validate_picture_buffers(&[
                buffer(DXVA2_PictureParametersBufferType),
                buffer(DXVA2_PictureParametersBufferType),
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_empty_dxva_payloads() {
        let mut empty = buffer(DXVA2_BitStreamDateBufferType);
        empty.data.clear();
        assert!(validate_picture_buffers(&[empty]).is_err());
    }

    #[test]
    fn converts_length_prefixed_nals_to_dxva_annex_b() -> crate::VideoResult<()> {
        let time_base = TimeBase::new(1, 1000)?;
        let sample = EncodedSample {
            data: &[2, 0x65, 0xaa, 1, 0x41],
            timing: SampleTiming {
                dts: None,
                pts: None,
                duration: None,
                time_base,
            },
            is_random_access: false,
        };
        assert_eq!(
            build_dxva2_bitstream(
                sample,
                Codec::Avc,
                BitstreamFormat::LengthPrefixed { length_size: 1 }
            )?,
            [0, 0, 1, 0x65, 0xaa, 0, 0, 1, 0x41]
        );
        Ok(())
    }

    #[test]
    fn validates_av1_obus_and_rejects_codec_framing_mismatch() -> crate::VideoResult<()> {
        let time_base = TimeBase::new(1, 1)?;
        let sample = EncodedSample {
            data: &[0x0a, 0x00],
            timing: SampleTiming {
                dts: None,
                pts: None,
                duration: None,
                time_base,
            },
            is_random_access: false,
        };
        assert_eq!(
            build_dxva2_bitstream(sample, Codec::Av1, BitstreamFormat::Av1Obu)?,
            sample.data
        );
        assert!(build_dxva2_bitstream(sample, Codec::Avc, BitstreamFormat::Av1Obu).is_err());
        Ok(())
    }

    #[test]
    fn builds_dxva_picture_with_one_encoded_buffer() -> crate::VideoResult<()> {
        let sample = EncodedSample {
            data: &[0, 0, 1, 0x65],
            timing: SampleTiming {
                dts: None,
                pts: None,
                duration: None,
                time_base: TimeBase::new(1, 1)?,
            },
            is_random_access: true,
        };
        let picture = Dxva2Picture::from_sample(
            3,
            sample,
            Codec::Avc,
            BitstreamFormat::AnnexB,
            120,
            vec![buffer(DXVA2_PictureParametersBufferType)],
        )?;
        assert_eq!(picture.target_index, 3);
        assert_eq!(picture.buffers.len(), 2);
        assert_eq!(picture.buffers[1].data, [0, 0, 1, 0x65]);
        assert_eq!(picture.buffers[1].macroblock_count, 120);
        Ok(())
    }

    #[test]
    fn builds_avc_vcl_bitstream_and_short_slice_controls() -> crate::VideoResult<()> {
        let sample = EncodedSample {
            data: &[
                0, 0, 1, 0x67, 0x01, 0, 0, 1, 0x68, 0x02, 0, 0, 1, 0x65, 0xaa, 0, 0, 1, 0x41, 0xbb,
                0xcc,
            ],
            timing: SampleTiming {
                dts: None,
                pts: None,
                duration: None,
                time_base: TimeBase::new(1, 1)?,
            },
            is_random_access: true,
        };
        let coded_size = crate::FrameSize::new(32, 32)?;
        let [bitstream, slice_control] =
            build_avc_slice_buffers(sample, BitstreamFormat::AnnexB, coded_size)?;
        assert_eq!(bitstream.data.len(), 128);
        assert_eq!(
            &bitstream.data[..11],
            &[0, 0, 1, 0x65, 0xaa, 0, 0, 1, 0x41, 0xbb, 0xcc]
        );
        assert!(bitstream.data[11..].iter().all(|byte| *byte == 0));
        assert_eq!(bitstream.macroblock_count, 4);
        assert_eq!(slice_control.data.len(), 20);
        assert_eq!(&slice_control.data[..10], &[0, 0, 0, 0, 5, 0, 0, 0, 0, 0]);
        assert_eq!(&slice_control.data[10..], &[5, 0, 0, 0, 123, 0, 0, 0, 0, 0]);
        Ok(())
    }

    #[test]
    fn builds_hevc_vcl_bitstream_and_short_slice_controls() -> crate::VideoResult<()> {
        let sample = EncodedSample {
            data: &[
                0, 0, 1, 0x40, 0x01, 0x11, 0, 0, 1, 0x26, 0x01, 0xaa, 0, 0, 1, 0x02, 0x01, 0xbb,
            ],
            timing: SampleTiming {
                dts: None,
                pts: None,
                duration: None,
                time_base: TimeBase::new(1, 1)?,
            },
            is_random_access: true,
        };
        let [bitstream, slice_control] = build_hevc_slice_buffers(sample, BitstreamFormat::AnnexB)?;
        assert_eq!(bitstream.macroblock_count, 0);
        assert_eq!(bitstream.data.len(), 128);
        assert_eq!(
            &bitstream.data[..12],
            &[0, 0, 1, 0x26, 0x01, 0xaa, 0, 0, 1, 0x02, 0x01, 0xbb]
        );
        assert_eq!(&slice_control.data[0..10], &[0, 0, 0, 0, 6, 0, 0, 0, 0, 0]);
        assert_eq!(&slice_control.data[10..], &[6, 0, 0, 0, 122, 0, 0, 0, 0, 0]);
        Ok(())
    }

    #[test]
    fn builds_av1_bitstream_and_tile_control_records() -> crate::VideoResult<()> {
        let payload = [10, 11, 12, 13];
        let tiles = [
            Dxva2Av1Tile {
                data_offset: 0,
                data_size: 2,
                row: 0,
                column: 0,
            },
            Dxva2Av1Tile {
                data_offset: 2,
                data_size: 2,
                row: 0,
                column: 1,
            },
        ];
        let [bitstream, tile_control] = build_av1_tile_buffers(&payload, &tiles)?;
        assert_eq!(bitstream.data.len(), 128);
        assert_eq!(&bitstream.data[..4], &payload);
        assert!(bitstream.data[4..].iter().all(|byte| *byte == 0));
        assert_eq!(tile_control.data.len(), 32);
        assert_eq!(
            &tile_control.data[..16],
            &[0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0]
        );
        assert_eq!(
            &tile_control.data[16..],
            &[2, 0, 0, 0, 2, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0xff, 0]
        );
        let overlapping = [
            tiles[0],
            Dxva2Av1Tile {
                data_offset: 1,
                ..tiles[1]
            },
        ];
        assert!(build_av1_tile_buffers(&payload, &overlapping).is_err());
        Ok(())
    }
}

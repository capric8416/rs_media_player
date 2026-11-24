//! D3D11 video-decoder creation and submission of codec-prepared picture buffers.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_VDOV, D3D11_VDOV_DIMENSION_TEXTURE2D, D3D11_VIDEO_DECODER_BUFFER_DESC,
    D3D11_VIDEO_DECODER_BUFFER_TYPE, D3D11_VIDEO_DECODER_DESC,
    D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC, D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0, ID3D11Texture2D,
    ID3D11VideoDecoder, ID3D11VideoDecoderOutputView,
};
use windows::core::Interface;

use crate::avc::AvcDecodeState;
use crate::d3d11_device::{D3d11CompletionToken, D3d11DecodeCapability, D3d11Device, dxgi_format};
use crate::hevc::HevcDecodeState;
use crate::{
    BitstreamFormat, Codec, CodecDecodeState, ColorInfo, D3dBackend, D3dDecode, DecodeCapability,
    DecoderConfig, EncodedSample, FrameInfo, FrameSize, GpuFrame, PixelFormat, SampleTiming,
    VideoError, VideoErrorKind, VideoResult, WaitStatus,
};

/// One codec-prepared D3D11 decoder buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct D3d11DecodeBuffer {
    pub buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE,
    pub data: Vec<u8>,
    pub first_macroblock: u32,
    pub macroblock_count: u32,
}

/// A decoded D3D11 array-texture slice and completion query.
pub struct D3d11DecodedFrame {
    pub(crate) texture: ID3D11Texture2D,
    pub(crate) surface_index: usize,
    pub(crate) completion: D3d11CompletionToken,
    pub(crate) info: FrameInfo,
    pub(crate) sample_aspect: crate::SampleAspectRatio,
    pub(crate) surface_lease: Option<crate::frame::SurfaceLease>,
    pub(crate) retained_source_lease: Option<crate::frame::SurfaceLease>,
    pub(crate) retained_source_texture: Option<ID3D11Texture2D>,
}

impl Drop for D3d11DecodedFrame {
    fn drop(&mut self) {
        match self.completion.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) | Err(_) => {
                std::mem::forget(self.texture.clone());
                if let Some(texture) = self.retained_source_texture.take() {
                    std::mem::forget(texture);
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

impl D3d11DecodedFrame {
    #[must_use]
    pub const fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
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

impl GpuFrame for D3d11DecodedFrame {
    fn backend(&self) -> D3dBackend {
        D3dBackend::D3d11
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

/// D3D11 video decoder bound to one NV12/P010 array texture and its output views.
pub struct D3d11Decoder {
    device: D3d11Device,
    decoder: ID3D11VideoDecoder,
    texture: ID3D11Texture2D,
    output_views: Vec<ID3D11VideoDecoderOutputView>,
    size: FrameSize,
    format: PixelFormat,
    codec: Codec,
    poisoned: AtomicBool,
}

impl D3d11Decoder {
    /// Configures D3D11 and initializes the matching codec parameter/DPB state together.
    ///
    /// # Errors
    ///
    /// Returns codec-private parsing, unsupported configuration, surface allocation, or decoder
    /// creation errors.
    pub fn configure_with_state(
        device: D3d11Device,
        config: &crate::DecoderConfig,
        requested_surface_count: u32,
    ) -> VideoResult<(Self, crate::CodecDecodeState)> {
        let state = config.create_codec_state()?;
        let decoder = Self::configure(device, config, requested_surface_count)?;
        Ok((decoder, state))
    }

    /// Creates a D3D11 decoder and output views for an allocated decoder texture.
    ///
    /// The caller chooses a config returned by `D3d11Device::decoder_configs` and supplies the
    /// corresponding capability. Parameter translation and codec-specific buffer assembly remain
    /// the responsibility of the codec adapter.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, backend, or unsupported errors when decoder resources cannot be
    /// created for the selected profile, format, size, or array texture.
    pub fn create(
        device: D3d11Device,
        capability: &D3d11DecodeCapability,
        size: FrameSize,
        config: &windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_DECODER_CONFIG,
        surface_count: u32,
    ) -> VideoResult<Self> {
        if surface_count == 0 || surface_count > 64 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::create",
                "decoder surface count must be in 1..=64",
            ));
        }
        if !device.capabilities().contains(capability) {
            return Err(decode_error(
                VideoErrorKind::Unsupported,
                "D3d11Decoder::create",
                "decode capability was not advertised by this device",
            ));
        }
        let format = capability.output_format;
        let surface_size = aligned_surface_size(size, capability.profile.codec())?;
        let texture = device.create_decoder_texture(surface_size, format, surface_count)?;
        let description = D3D11_VIDEO_DECODER_DESC {
            Guid: capability.decoder_profile,
            SampleWidth: size.width(),
            SampleHeight: size.height(),
            OutputFormat: dxgi_format(format),
        };
        // SAFETY: Both descriptors remain valid for this synchronous creation call, and the
        // capability was verified against this device above.
        let decoder = unsafe {
            device
                .video_device()
                .CreateVideoDecoder(&raw const description, config)
        }
        .map_err(|error| backend_error("CreateVideoDecoder", error.code().0))?;
        let resource = texture
            .cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>()
            .map_err(|error| backend_error("ID3D11Texture2D::cast", error.code().0))?;
        let mut output_views = Vec::new();
        let view_capacity = usize::try_from(surface_count).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::create",
                "surface count does not fit the host address size",
            )
        })?;
        output_views
            .try_reserve_exact(view_capacity)
            .map_err(|_| allocation_error("D3d11Decoder::create"))?;
        for array_slice in 0..surface_count {
            let view_desc = D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC {
                DecodeProfile: capability.decoder_profile,
                ViewDimension: D3D11_VDOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VDOV {
                        ArraySlice: array_slice,
                    },
                },
            };
            let mut output_view = None;
            // SAFETY: The texture resource and descriptor are valid during the call, the output
            // pointer is writable, and `array_slice` is inside the allocated texture array.
            unsafe {
                device.video_device().CreateVideoDecoderOutputView(
                    &resource,
                    &raw const view_desc,
                    Some(&raw mut output_view),
                )
            }
            .map_err(|error| backend_error("CreateVideoDecoderOutputView", error.code().0))?;
            output_views.push(output_view.ok_or_else(|| {
                backend_error(
                    "CreateVideoDecoderOutputView",
                    0x8000_4003_u32.cast_signed(),
                )
            })?);
        }
        Ok(Self {
            device,
            decoder,
            texture,
            output_views,
            size,
            format,
            codec: capability.profile.codec(),
            poisoned: AtomicBool::new(false),
        })
    }

    /// Selects the best driver configuration and creates the decoder surface array.
    ///
    /// AVC selects short-slice mode (`ConfigBitstreamRaw = 2`); HEVC and AV1 select raw mode 1,
    /// matching `FFmpeg`'s DXVA configuration policy.
    ///
    /// # Errors
    ///
    /// Returns validation, unsupported-profile/configuration, allocation, or decoder creation
    /// errors.
    pub fn configure(
        device: D3d11Device,
        config: &crate::DecoderConfig,
        requested_surface_count: u32,
    ) -> VideoResult<Self> {
        config.validate()?;
        if requested_surface_count == 0 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::configure",
                "requested surface count must be nonzero",
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
                    "D3d11Decoder::configure",
                    "requested codec profile and bit depth are not supported",
                )
            })?;
        let raw_mode = if config.profile.codec() == Codec::Avc {
            2
        } else {
            1
        };
        let driver_config = device
            .decoder_configs(config.profile, config.bit_depth, config.coded_size)?
            .into_iter()
            .filter(|candidate| {
                candidate.ConfigBitstreamRaw == 1
                    || (config.profile.codec() == Codec::Avc && candidate.ConfigBitstreamRaw == 2)
            })
            .max_by_key(|candidate| {
                let raw_score = u8::from(candidate.ConfigBitstreamRaw == raw_mode);
                let clear_score = u8::from(
                    candidate.guidConfigBitstreamEncryption
                        == windows::Win32::Media::MediaFoundation::DXVA2_NoEncrypt,
                );
                (clear_score, raw_score)
            })
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::Unsupported,
                    "D3d11Decoder::configure",
                    "driver exposes no compatible decoder bitstream configuration",
                )
            })?;
        let surface_count =
            requested_surface_count.max(u32::from(driver_config.ConfigMinRenderTargetBuffCount));
        Self::create(
            device,
            &capability,
            config.coded_size,
            &driver_config,
            surface_count,
        )
    }

    /// Submits caller-prepared picture, bitstream, and slice-control buffers.
    ///
    /// State is advanced by the codec adapter only after this method returns a frame.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, invalid-state, allocation, backend, or device-lost errors.
    pub fn submit_picture(
        &self,
        target_index: usize,
        buffers: &[D3d11DecodeBuffer],
        timing: crate::SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d11DecodedFrame> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11Decoder::submit_picture",
                "a prior submitted picture has unknown completion; recreate the decoder",
            ));
        }
        if buffers.is_empty() {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::submit_picture",
                "at least one decoder buffer is required",
            ));
        }
        let view = self.output_views.get(target_index).ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::submit_picture",
                "target surface index exceeds the decoder texture array",
            )
        })?;
        for (index, buffer) in buffers.iter().enumerate() {
            if buffers[..index]
                .iter()
                .any(|previous| previous.buffer_type == buffer.buffer_type)
            {
                return Err(decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d11Decoder::submit_picture",
                    "decoder buffer types must be unique within one picture",
                ));
            }
        }
        let context = self.device.video_context();
        // SAFETY: Decoder and output view belong to this live device; the view index was checked.
        unsafe { context.DecoderBeginFrame(&self.decoder, view, 0, None) }
            .map_err(|error| backend_error("DecoderBeginFrame", error.code().0))?;

        let submit_result = self.submit_buffers(buffers);
        // SAFETY: A successful BeginFrame above is paired with one EndFrame call.
        let end_result = unsafe { context.DecoderEndFrame(&self.decoder) }
            .map_err(|error| backend_error("DecoderEndFrame", error.code().0));
        if let Err(error) = submit_result {
            self.poison_and_retain();
            return Err(error);
        }
        if let Err(error) = end_result {
            self.poison_and_retain();
            return Err(error);
        }
        let completion = match self.device.signal_completion() {
            Ok(completion) => completion,
            Err(error) => {
                self.poison_and_retain();
                return Err(error);
            }
        };
        Ok(D3d11DecodedFrame {
            texture: self.texture.clone(),
            surface_index: target_index,
            completion,
            info: FrameInfo {
                size: self.size,
                format: self.format,
                color,
                pts: timing.pts,
                duration: timing.duration,
                time_base: timing.time_base,
            },
            sample_aspect: crate::SampleAspectRatio::SQUARE,
            surface_lease: None,
            retained_source_lease: None,
            retained_source_texture: None,
        })
    }

    fn poison_and_retain(&self) {
        self.poisoned.store(true, Ordering::Release);
        // Keep the decoder, output array, views and device alive because completion is unknown.
        std::mem::forget(self.decoder.clone());
        std::mem::forget(self.texture.clone());
        std::mem::forget(self.device.clone());
        for view in &self.output_views {
            std::mem::forget(view.clone());
        }
    }

    /// Translates and submits an AVC picture using caller-retained AVC reference state.
    ///
    /// Supports progressive 8-bit 4:2:0 I/P/B slices supported by the shared DXVA translator.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, unsupported-syntax, invalid-input, backend, or
    /// device-lost errors.
    pub fn decode_avc_sample(
        &self,
        state: &mut AvcDecodeState,
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d11DecodedFrame> {
        if self.codec != Codec::Avc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11Decoder::decode_avc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_avc_sample",
                "DXVA H.264 surface index exceeds u8",
            )
        })?;
        if state.dimensions_in_mbs()
            != (
                self.size.width().div_ceil(16),
                self.size.height().div_ceil(16),
            )
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_avc_sample",
                "AVC SPS dimensions do not match configured decoder dimensions",
            ));
        }
        let mut slices = Vec::new();
        for nal in
            crate::d3d9_decode::codec_nals(sample.data, format, "D3d11Decoder::decode_avc_sample")?
        {
            state.validate_parameter_set(nal)?;
            if nal
                .first()
                .is_some_and(|header| matches!(header & 0x1f, 1 | 5))
            {
                slices
                    .try_reserve(1)
                    .map_err(|_| allocation_error("D3d11Decoder::decode_avc_sample"))?;
                slices.push(nal);
            }
        }
        let prepared = state.prepare(&slices, surface_index)?;
        let [bitstream, slice_control] =
            crate::d3d9_decode::build_avc_slice_buffers(sample, format, self.size)?;
        let buffers = [
            d3d11_buffer(0, prepared.picture_parameters.clone(), 0, 0),
            d3d11_buffer(4, state.quant_matrix()?, 0, 0),
            d3d11_from_dxva2(bitstream),
            d3d11_from_dxva2(slice_control),
        ];
        let frame = self.submit_picture(target_index, &buffers, sample.timing, color)?;
        state.commit(prepared);
        Ok(frame)
    }

    /// Translates and submits an HEVC picture using caller-retained HEVC reference state.
    ///
    /// Supports ordered independent and dependent slice segments within one picture. Dependent
    /// segments inherit syntax state from the preceding independent segment.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, unsupported-syntax, invalid-input, backend, or
    /// device-lost errors.
    pub fn decode_hevc_sample(
        &self,
        state: &mut HevcDecodeState,
        target_index: usize,
        sample: EncodedSample<'_>,
        format: BitstreamFormat,
        color: ColorInfo,
    ) -> VideoResult<D3d11DecodedFrame> {
        if self.codec != Codec::Hevc {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11Decoder::decode_hevc_sample",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_hevc_sample",
                "DXVA HEVC surface index exceeds u8",
            )
        })?;
        if state.dimensions() != (self.size.width(), self.size.height()) {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_hevc_sample",
                "HEVC SPS dimensions do not match configured decoder dimensions",
            ));
        }
        let mut slices = Vec::new();
        for nal in
            crate::d3d9_decode::codec_nals(sample.data, format, "D3d11Decoder::decode_hevc_sample")?
        {
            state.validate_parameter_set(nal)?;
            if nal.len() >= 2 && (nal[0] >> 1) & 0x3f <= 31 {
                slices
                    .try_reserve(1)
                    .map_err(|_| allocation_error("D3d11Decoder::decode_hevc_sample"))?;
                slices.push(nal);
            }
        }
        let prepared = state.prepare(&slices, surface_index)?;
        let [bitstream, slice_control] =
            crate::d3d9_decode::build_hevc_slice_buffers(sample, format)?;
        let buffers = [
            d3d11_buffer(0, prepared.picture_parameters.clone(), 0, 0),
            d3d11_buffer(4, state.quant_matrix()?, 0, 0),
            d3d11_from_dxva2(bitstream),
            d3d11_from_dxva2(slice_control),
        ];
        let frame = self.submit_picture(target_index, &buffers, sample.timing, color)?;
        state.commit(prepared);
        Ok(frame)
    }

    /// Parses and submits an AV1 key/intra-only picture with complete tile coverage. A combined
    /// `OBU_FRAME` is accepted with an empty `tile_group_obus` slice.
    ///
    /// This stateless entry point handles key/intra-only frames. Stateful inter decoding is
    /// provided by `decode_av1_frame_or_show_existing`.
    ///
    /// # Errors
    ///
    /// Returns parser, unsupported-syntax, invalid-input, backend, or device-lost errors.
    pub fn decode_av1_intra_obus(
        &self,
        target_index: usize,
        sequence_header_obu: &[u8],
        frame_header_obu: &[u8],
        tile_group_obus: &[&[u8]],
        timing: SampleTiming,
        color: ColorInfo,
    ) -> VideoResult<D3d11DecodedFrame> {
        if self.codec != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11Decoder::decode_av1_intra_obus",
                "decoder was configured for a different codec",
            ));
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_av1_intra_obus",
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
    ) -> VideoResult<D3d11DecodedFrame> {
        if u32::from(built.width) != self.size.width()
            || u32::from(built.height) != self.size.height()
        {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::submit_av1_dxva_input",
                "AV1 frame dimensions do not match configured decoder dimensions",
            ));
        }
        let mut tiles = Vec::new();
        tiles
            .try_reserve_exact(built.tiles.len())
            .map_err(|_| allocation_error("D3d11Decoder::submit_av1_dxva_input"))?;
        tiles.extend(
            built
                .tiles
                .iter()
                .map(|tile| crate::d3d9_decode::Dxva2Av1Tile {
                    data_offset: tile.data_offset,
                    data_size: tile.data_size,
                    row: tile.row,
                    column: tile.column,
                }),
        );
        let [bitstream, tile_control] =
            crate::d3d9_decode::build_av1_tile_buffers(&built.tile_payload, &tiles)?;
        self.submit_picture(
            target_index,
            &[
                d3d11_buffer(0, built.picture_parameters.clone(), 0, 0),
                d3d11_from_dxva2(bitstream),
                d3d11_from_dxva2(tile_control),
            ],
            timing,
            color,
        )
    }

    /// Decodes a key/intra picture or supported error-resilient inter frame, or returns an already
    /// decoded show-existing slot.
    ///
    /// The state advances only after successful decode submission. Inter-frame limitations are
    /// checked by the shared parser before any GPU submission.
    ///
    /// # Errors
    ///
    /// Returns parser, missing-reference, unsupported-syntax, invalid-input, backend, or
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
    ) -> VideoResult<D3d11DecodedFrame> {
        if self.codec != Codec::Av1 {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11Decoder::decode_av1_frame_or_show_existing",
                "decoder was configured for a different codec",
            ));
        }
        if let Some(surface_index) =
            state.show_existing_surface(sequence_header_obu, frame_header_obu)?
        {
            let av1_surface_index = surface_index;
            let surface_index = usize::from(surface_index);
            if surface_index >= self.output_views.len() {
                return Err(decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d11Decoder::decode_av1_frame_or_show_existing",
                    "AV1 reference slot names a surface outside this decoder pool",
                ));
            }
            let completion = self.device.signal_completion()?;
            let render_size = state.surface_render_size(av1_surface_index)?;
            return Ok(D3d11DecodedFrame {
                texture: self.texture.clone(),
                surface_index,
                completion,
                info: FrameInfo {
                    size: self.size,
                    format: self.format,
                    color,
                    pts: timing.pts,
                    duration: timing.duration,
                    time_base: timing.time_base,
                },
                sample_aspect: crate::SampleAspectRatio::from_render_size(self.size, render_size)?,
                surface_lease: None,
                retained_source_lease: None,
                retained_source_texture: None,
            });
        }
        let surface_index = u8::try_from(target_index).map_err(|_| {
            decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11Decoder::decode_av1_frame_or_show_existing",
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
                "D3d11Decoder::decode_av1_frame_or_show_existing",
                "target texture slice still backs an AV1 slot not refreshed by this frame",
            ));
        }
        let mut frame = self.submit_av1_dxva_input(target_index, &built, timing, color)?;
        state.commit_frame(&built, surface_index);
        let render_size = state.surface_render_size(surface_index)?;
        frame.sample_aspect = crate::SampleAspectRatio::from_render_size(self.size, render_size)?;
        Ok(frame)
    }

    fn submit_buffers(&self, buffers: &[D3d11DecodeBuffer]) -> VideoResult<()> {
        let mut descriptions = Vec::new();
        descriptions
            .try_reserve_exact(buffers.len())
            .map_err(|_| allocation_error("D3d11Decoder::submit_buffers"))?;
        for buffer in buffers {
            if buffer.data.is_empty() {
                return Err(decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d11Decoder::submit_buffers",
                    "decoder buffers must not be empty",
                ));
            }
            let data_size = u32::try_from(buffer.data.len()).map_err(|_| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d11Decoder::submit_buffers",
                    "decoder buffer exceeds the D3D11 size limit",
                )
            })?;
            let mut capacity = 0_u32;
            let mut destination: *mut c_void = ptr::null_mut();
            // SAFETY: The decoder is live and both output pointers point to writable local
            // variables for the duration of GetDecoderBuffer.
            unsafe {
                self.device.video_context().GetDecoderBuffer(
                    &self.decoder,
                    buffer.buffer_type,
                    &raw mut capacity,
                    &raw mut destination,
                )
            }
            .map_err(|error| backend_error("GetDecoderBuffer", error.code().0))?;
            if destination.is_null() || capacity < data_size {
                // SAFETY: GetDecoderBuffer succeeded, so release the acquired buffer before exit.
                let release = unsafe {
                    self.device
                        .video_context()
                        .ReleaseDecoderBuffer(&self.decoder, buffer.buffer_type)
                };
                if let Err(error) = release {
                    return Err(backend_error("ReleaseDecoderBuffer", error.code().0));
                }
                return Err(decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d11Decoder::submit_buffers",
                    "driver decoder buffer is smaller than the submitted payload",
                ));
            }
            // SAFETY: GetDecoderBuffer returned a live writable range of `capacity` bytes, and
            // `data_size <= capacity`; source and destination are distinct allocations.
            unsafe {
                ptr::copy_nonoverlapping(
                    buffer.data.as_ptr(),
                    destination.cast(),
                    buffer.data.len(),
                );
            };
            // SAFETY: This buffer was acquired successfully above and is released exactly once.
            unsafe {
                self.device
                    .video_context()
                    .ReleaseDecoderBuffer(&self.decoder, buffer.buffer_type)
            }
            .map_err(|error| backend_error("ReleaseDecoderBuffer", error.code().0))?;
            descriptions.push(D3D11_VIDEO_DECODER_BUFFER_DESC {
                BufferType: buffer.buffer_type,
                BufferIndex: 0,
                DataOffset: 0,
                DataSize: data_size,
                FirstMBaddress: buffer.first_macroblock,
                NumMBsInBuffer: buffer.macroblock_count,
                ..D3D11_VIDEO_DECODER_BUFFER_DESC::default()
            });
        }
        // SAFETY: The descriptions refer to buffers filled and released above; the decoder is
        // still inside the frame started by submit_picture.
        unsafe {
            self.device
                .video_context()
                .SubmitDecoderBuffers(&self.decoder, &descriptions)
        }
        .map_err(|error| backend_error("SubmitDecoderBuffers", error.code().0))
    }
}

fn decode_error(kind: VideoErrorKind, operation: &'static str, detail: &'static str) -> VideoError {
    VideoError::new(kind, operation, detail)
}

fn d3d11_from_dxva2(buffer: crate::d3d9_decode::Dxva2Buffer) -> D3d11DecodeBuffer {
    D3d11DecodeBuffer {
        buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE(buffer.buffer_type.0),
        data: buffer.data,
        first_macroblock: buffer.first_macroblock,
        macroblock_count: buffer.macroblock_count,
    }
}

fn d3d11_buffer(
    buffer_type: i32,
    data: Vec<u8>,
    first_macroblock: u32,
    macroblock_count: u32,
) -> D3d11DecodeBuffer {
    D3d11DecodeBuffer {
        buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE(buffer_type),
        data,
        first_macroblock,
        macroblock_count,
    }
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    let kind = crate::error::backend_kind_from_hresult(code);
    VideoError::new(kind, operation, "D3D11 decoder operation failed").with_backend_code(code)
}

fn allocation_error(operation: &'static str) -> VideoError {
    VideoError::new(
        VideoErrorKind::Backend,
        operation,
        "unable to allocate D3D11 decoder data",
    )
}

/// Stateful D3D11 adapter for the shared send/receive decoder contract.
///
/// This session owns codec DPB state and queues submitted output frames. The device's size limits
/// are adapter-specific and are checked when `configure` creates the video decoder.
pub struct D3d11DecodeSession {
    device: D3d11Device,
    surface_count: u32,
    capabilities: Vec<DecodeCapability>,
    decoder: Option<D3d11Decoder>,
    state: Option<CodecDecodeState>,
    config: Option<DecoderConfig>,
    av1_sequence_header: Option<Vec<u8>>,
    pending: VecDeque<D3d11DecodedFrame>,
    surface_leases: Vec<Arc<AtomicUsize>>,
    draining: bool,
}

impl D3d11DecodeSession {
    /// Creates a session shell for one D3D11 device; call `configure` before sending samples.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the requested decoder surface count is zero.
    pub fn new(device: D3d11Device, surface_count: u32) -> VideoResult<Self> {
        if surface_count == 0 {
            return Err(decode_error(
                VideoErrorKind::InvalidInput,
                "D3d11DecodeSession::new",
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
                "D3d11DecodeSession::send_sample",
                "decoder is not configured",
            )
        })?;
        let state = self.state.as_ref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d11DecodeSession::send_sample",
                "codec state is not configured",
            )
        })?;
        let limit = decoder.output_views.len().min(128);
        (0..limit)
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
                    "D3d11DecodeSession::send_sample",
                    "no decoder surface is free of codec references and queued output frames",
                )
            })
    }

    fn decode_av1_sample(
        &mut self,
        target_surface: usize,
        sample: EncodedSample<'_>,
    ) -> VideoResult<D3d11DecodedFrame> {
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
                            "D3d11DecodeSession::decode_av1_sample",
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
                "D3d11DecodeSession::decode_av1_sample",
                "sample contains no AV1 frame-header OBU",
            )
        })?;
        let sequence_header = self.av1_sequence_header.as_deref().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d11DecodeSession::decode_av1_sample",
                "AV1 sequence header is not configured",
            )
        })?;
        let state = self.state.as_mut().ok_or_else(|| {
            decode_error(
                VideoErrorKind::InvalidState,
                "D3d11DecodeSession::decode_av1_sample",
                "codec state is not configured",
            )
        })?;
        let CodecDecodeState::Av1(state) = state else {
            return Err(decode_error(
                VideoErrorKind::InvalidState,
                "D3d11DecodeSession::decode_av1_sample",
                "configured codec state is not AV1",
            ));
        };
        self.decoder
            .as_ref()
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d11DecodeSession::decode_av1_sample",
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

impl D3dDecode for D3d11DecodeSession {
    type Frame = D3d11DecodedFrame;

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
        let decoder = D3d11Decoder::configure(self.device.clone(), config, self.surface_count)?;
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
                "D3d11DecodeSession::send_sample",
                "input was already drained; call flush before resuming",
            ));
        }
        sample.validate()?;
        let (codec, bitstream_format) = self.config.as_ref().map_or_else(
            || {
                Err(decode_error(
                    VideoErrorKind::InvalidState,
                    "D3d11DecodeSession::send_sample",
                    "decoder is not configured",
                ))
            },
            |config| Ok((config.profile.codec(), config.bitstream_format)),
        )?;
        let target = self.choose_target_surface()?;
        let mut frame = match codec {
            Codec::Avc => {
                let Some(CodecDecodeState::Avc(state)) = self.state.as_mut() else {
                    return Err(decode_error(
                        VideoErrorKind::InvalidState,
                        "D3d11DecodeSession::send_sample",
                        "AVC codec state is not configured",
                    ));
                };
                self.decoder
                    .as_ref()
                    .ok_or_else(|| {
                        decode_error(
                            VideoErrorKind::InvalidState,
                            "D3d11DecodeSession::send_sample",
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
                        "D3d11DecodeSession::send_sample",
                        "HEVC codec state is not configured",
                    ));
                };
                self.decoder
                    .as_ref()
                    .ok_or_else(|| {
                        decode_error(
                            VideoErrorKind::InvalidState,
                            "D3d11DecodeSession::send_sample",
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
            .map_err(|_| allocation_error("D3d11DecodeSession::send_sample"))?;
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
                "D3d11DecodeSession::drain",
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

fn aligned_surface_size(size: FrameSize, codec: Codec) -> VideoResult<FrameSize> {
    let alignment = match codec {
        Codec::Hevc | Codec::Av1 => 128,
        Codec::Avc => 16,
    };
    let align = |value: u32| {
        value
            .checked_add(alignment - 1)
            .map(|rounded| rounded / alignment * alignment)
            .ok_or_else(|| {
                decode_error(
                    VideoErrorKind::InvalidInput,
                    "D3d11Decoder::create",
                    "aligned decoder surface dimensions overflowed",
                )
            })
    };
    FrameSize::new(align(size.width())?, align(size.height())?)
}

#[cfg(test)]
mod tests {
    use super::d3d11_from_dxva2;
    use crate::d3d9_decode::Dxva2Buffer;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
        D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
        D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
    };
    use windows::Win32::Media::MediaFoundation::{
        DXVA2_BitStreamDateBufferType, DXVA2_InverseQuantizationMatrixBufferType,
        DXVA2_PictureParametersBufferType, DXVA2_SliceControlBufferType,
    };

    #[test]
    fn maps_shared_dxva_buffer_kinds_to_d3d11_buffer_kinds() {
        for (dxva2, d3d11) in [
            (
                DXVA2_PictureParametersBufferType,
                D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
            ),
            (
                DXVA2_InverseQuantizationMatrixBufferType,
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
            ),
            (
                DXVA2_SliceControlBufferType,
                D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
            ),
            (
                DXVA2_BitStreamDateBufferType,
                D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
            ),
        ] {
            let buffer = d3d11_from_dxva2(Dxva2Buffer {
                buffer_type: dxva2,
                data: vec![1],
                first_macroblock: 2,
                macroblock_count: 3,
            });
            assert_eq!(buffer.buffer_type, d3d11);
            assert_eq!(buffer.first_macroblock, 2);
            assert_eq!(buffer.macroblock_count, 3);
        }
    }
}

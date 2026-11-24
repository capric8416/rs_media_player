//! D3D11 video-processor scaling and NV12/P010/BGRA format conversion.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::ID3D11VideoContext1;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CAPS, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_ROTATION,
    D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_180,
    D3D11_VIDEO_PROCESSOR_ROTATION_270, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_RATIONAL;
use windows::core::Interface;

use crate::d3d11_decode::D3d11DecodedFrame;
use crate::d3d11_device::D3d11Device;
use crate::{
    D3dScale, FrameInfo, FrameSize, PixelFormat, ScaleCapability, ScaleFilter, ScaleRequest,
    VideoError, VideoErrorKind, VideoResult,
};

/// D3D11 scaler that creates a video-processor pass for each requested output geometry.
pub struct D3d11Scaler {
    device: D3d11Device,
    capabilities: Vec<ScaleCapability>,
}

impl D3d11Scaler {
    /// Creates a scaler using an existing video-capable D3D11 device.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error only if the built-in nonzero size bounds cannot be formed.
    pub fn new(device: D3d11Device) -> VideoResult<Self> {
        let inputs = [PixelFormat::Nv12, PixelFormat::P010];
        let min_size = FrameSize::new(2, 2)?;
        let max_size = FrameSize::new(u32::MAX, u32::MAX)?;
        let mut capabilities = Vec::new();
        for format in inputs {
            capabilities.push(ScaleCapability {
                input_format: format,
                output_formats: supported_outputs(&device, format)?,
                min_input_size: min_size,
                max_input_size: max_size,
                max_output_size: max_size,
                filters: vec![ScaleFilter::DriverDefault],
            });
        }
        Ok(Self {
            device,
            capabilities,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the method owns one checked video-processor setup and submission transaction"
    )]
    fn scale_frame(
        &self,
        input: &D3d11DecodedFrame,
        request: &ScaleRequest,
    ) -> VideoResult<D3d11DecodedFrame> {
        validate_request(input.info.format, input.info.color, request)?;

        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: The input texture is a live COM object and the descriptor is writable.
        unsafe { input.texture.GetDesc(&raw mut texture_desc) };
        if texture_desc.Format != crate::d3d11_device::dxgi_format(input.info.format) {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d11Scaler::scale",
                "input frame format metadata does not match its texture",
            ));
        }
        if !texture_desc.Width.is_multiple_of(2)
            || !texture_desc.Height.is_multiple_of(2)
            || (matches!(request.output_format, PixelFormat::Nv12 | PixelFormat::P010)
                && (!request.output_size.width().is_multiple_of(2)
                    || !request.output_size.height().is_multiple_of(2)))
        {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d11Scaler::scale",
                "NV12/P010 input and 4:2:0 output dimensions must be even",
            ));
        }
        let (source_rect, destination_rect) = crate::d3d12_scale::compute_rotated_rectangles(
            input.info.size,
            request.output_size,
            request.source_rect,
            request.destination_rect,
            request.aspect_mode,
            input.sample_aspect_ratio(),
            request.rotation,
        )?;
        validate_chroma_rects(source_rect, destination_rect, request.output_format)?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            InputWidth: texture_desc.Width,
            InputHeight: texture_desc.Height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            OutputWidth: request.output_size.width(),
            OutputHeight: request.output_size.height(),
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: The descriptor is initialized and valid for the duration of the call.
        let enumerator = unsafe {
            self.device
                .video_device()
                .CreateVideoProcessorEnumerator(&raw const content)
        }
        .map_err(|error| backend_error("CreateVideoProcessorEnumerator", error.code().0))?;
        if request.rotation != crate::d3d_render::Rotation::Degrees0 {
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            // SAFETY: The live enumerator writes its supported processor features to `caps`.
            unsafe { enumerator.GetVideoProcessorCaps(&raw mut caps) }
                .map_err(|error| backend_error("GetVideoProcessorCaps", error.code().0))?;
            if caps.FeatureCaps
                & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION
                    .0
                    .cast_unsigned()
                == 0
            {
                return Err(scale_error(
                    VideoErrorKind::Unsupported,
                    "D3d11Scaler::scale",
                    "video processor does not advertise stream rotation",
                ));
            }
        }
        check_format_support(
            &enumerator,
            crate::d3d11_device::dxgi_format(input.info.format),
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0.cast_unsigned(),
        )?;
        check_format_support(
            &enumerator,
            crate::d3d11_device::dxgi_format(request.output_format),
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT
                .0
                .cast_unsigned(),
        )?;
        // SAFETY: The enumerator is a live interface created by this device; rate index zero is
        // the default processor rate-conversion configuration used by FFmpeg's filter.
        let processor = unsafe {
            self.device
                .video_device()
                .CreateVideoProcessor(&enumerator, 0)
        }
        .map_err(|error| backend_error("CreateVideoProcessor", error.code().0))?;
        let output_color = if request.output_format == PixelFormat::Bgra8 {
            crate::color::bgra8_output_color()
        } else {
            request.output_color.unwrap_or(input.info.color)
        };
        let input_color_space =
            crate::d3d12_scale::checked_color_space(input.info.format, input.info.color)?;
        let output_color_space =
            crate::d3d12_scale::checked_color_space(request.output_format, output_color)?;
        let video_context: ID3D11VideoContext1 =
            self.device.video_context().cast().map_err(|error| {
                backend_error(
                    "ID3D11VideoContext::cast ID3D11VideoContext1",
                    error.code().0,
                )
            })?;
        // SAFETY: The context, processor and color-space values belong to the same D3D11 device.
        unsafe {
            video_context.VideoProcessorSetStreamColorSpace1(&processor, 0, input_color_space);
            video_context.VideoProcessorSetOutputColorSpace1(&processor, output_color_space);
        }

        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            // Zero selects the resource's DXGI format. `DXGI_FORMAT_NV12`/`P010` enum values are
            // not FOURCC codes and some drivers reject them with DXGI_ERROR_UNSUPPORTED.
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: u32::try_from(input.surface_index).map_err(|_| {
                        scale_error(
                            VideoErrorKind::InvalidInput,
                            "D3d11Scaler::scale",
                            "input array slice exceeds the D3D11 range",
                        )
                    })?,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: The texture, enumerator and descriptor belong to this device; output is writable.
        unsafe {
            self.device.video_device().CreateVideoProcessorInputView(
                &input.texture,
                &enumerator,
                &raw const input_desc,
                Some(&raw mut input_view),
            )
        }
        .map_err(|error| backend_error("CreateVideoProcessorInputView", error.code().0))?;
        let input_view = input_view.ok_or_else(|| {
            backend_error(
                "CreateVideoProcessorInputView",
                0x8000_4003_u32.cast_signed(),
            )
        })?;

        let output_texture = self
            .device
            .create_processor_texture(request.output_size, request.output_format)?;
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        // SAFETY: The output texture and view enumerator are compatible objects from this device.
        unsafe {
            self.device.video_device().CreateVideoProcessorOutputView(
                &output_texture,
                &enumerator,
                &raw const output_desc,
                Some(&raw mut output_view),
            )
        }
        .map_err(|error| backend_error("CreateVideoProcessorOutputView", error.code().0))?;
        let output_view = output_view.ok_or_else(|| {
            backend_error(
                "CreateVideoProcessorOutputView",
                0x8000_4003_u32.cast_signed(),
            )
        })?;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            pInputSurface: ManuallyDrop::new(Some(input_view.clone())),
            ..Default::default()
        };
        let background = windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR {
            Anonymous: windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR_0 {
                RGBA: windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_COLOR_RGBA {
                    R: 0.0,
                    G: 0.0,
                    B: 0.0,
                    A: 1.0,
                },
            },
        };
        // SAFETY: Processor and initialized opaque-black RGBA value are valid for this call.
        unsafe {
            self.device
                .video_context()
                .VideoProcessorSetOutputBackgroundColor(&processor, false, &raw const background);
        }
        // SAFETY: Processor is live and these rectangles were checked against frame/output bounds.
        unsafe {
            self.device
                .video_context()
                .VideoProcessorSetStreamSourceRect(
                    &processor,
                    0,
                    true,
                    Some(&raw const source_rect),
                );
            self.device.video_context().VideoProcessorSetStreamDestRect(
                &processor,
                0,
                true,
                Some(&raw const destination_rect),
            );
            self.device.video_context().VideoProcessorSetStreamRotation(
                &processor,
                0,
                request.rotation != crate::d3d_render::Rotation::Degrees0,
                d3d11_rotation(request.rotation),
            );
        }
        // SAFETY: The processor and view are live. The stream holds a cloned input-view reference
        // for the duration of the call; `ManuallyDrop` is converted back and dropped below.
        let blit_result = unsafe {
            self.device.video_context().VideoProcessorBlt(
                &processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        drop(ManuallyDrop::into_inner(std::mem::replace(
            &mut stream.pInputSurface,
            ManuallyDrop::new(None),
        )));
        blit_result.map_err(|error| backend_error("VideoProcessorBlt", error.code().0))?;

        let completion = match self.device.signal_completion() {
            Ok(completion) => completion,
            Err(error) => {
                // The blit was submitted, but a completion marker could not be created. Keep
                // both resources and the decoder lease alive because their reuse is uncertain.
                std::mem::forget(output_texture.clone());
                std::mem::forget(input.texture.clone());
                if let Some(lease) = input
                    .surface_lease
                    .as_ref()
                    .map(crate::frame::SurfaceLease::clone_for_pending_work)
                {
                    std::mem::forget(lease);
                }
                return Err(error);
            }
        };
        let mut info: FrameInfo = input.info;
        info.size = request.output_size;
        info.format = request.output_format;
        info.color = output_color;
        Ok(D3d11DecodedFrame {
            texture: output_texture,
            surface_index: 0,
            completion,
            info,
            sample_aspect: crate::SampleAspectRatio::SQUARE,
            surface_lease: None,
            retained_source_lease: input
                .surface_lease
                .as_ref()
                .map(crate::frame::SurfaceLease::clone_for_pending_work),
            retained_source_texture: Some(input.texture.clone()),
        })
    }
}

fn validate_request(
    input_format: PixelFormat,
    input_color: crate::ColorInfo,
    request: &ScaleRequest,
) -> VideoResult<()> {
    if request.filter != ScaleFilter::DriverDefault {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d11Scaler::scale",
            "D3D11 scaling currently supports only the driver-default filter",
        ));
    }
    if !matches!(input_format, PixelFormat::Nv12 | PixelFormat::P010)
        || !matches!(
            request.output_format,
            PixelFormat::Nv12 | PixelFormat::P010 | PixelFormat::Bgra8
        )
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d11Scaler::scale",
            "D3D11 scaling accepts NV12/P010 input and NV12/P010/BGRA8 output",
        ));
    }
    let expected_output_color = if request.output_format == PixelFormat::Bgra8 {
        crate::color::bgra8_output_color()
    } else {
        input_color
    };
    if request.output_format == PixelFormat::Bgra8
        && matches!(
            input_color.transfer,
            crate::TransferCharacteristic::Smpte2084 | crate::TransferCharacteristic::AribStdB67
        )
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d11Scaler::scale",
            "HDR-to-SDR tone mapping is not implemented for BGRA8 output",
        ));
    }
    if request
        .output_color
        .is_some_and(|color| color != expected_output_color)
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d11Scaler::scale",
            "vf_scale_d3d11 uses the D3D11 processor default color conversion",
        ));
    }
    Ok(())
}

fn validate_chroma_rects(
    source: RECT,
    destination: RECT,
    output_format: PixelFormat,
) -> VideoResult<()> {
    let source_width = source.right - source.left;
    let source_height = source.bottom - source.top;
    let destination_width = destination.right - destination.left;
    let destination_height = destination.bottom - destination.top;
    if source.left % 2 != 0
        || source.top % 2 != 0
        || source_width % 2 != 0
        || source_height % 2 != 0
        || (matches!(output_format, PixelFormat::Nv12 | PixelFormat::P010)
            && (destination.left % 2 != 0
                || destination.top % 2 != 0
                || destination_width % 2 != 0
                || destination_height % 2 != 0))
    {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d11Scaler::scale",
            "4:2:0 processing rectangles must use even coordinates and dimensions",
        ));
    }
    Ok(())
}

fn d3d11_rotation(rotation: crate::d3d_render::Rotation) -> D3D11_VIDEO_PROCESSOR_ROTATION {
    match rotation {
        crate::d3d_render::Rotation::Degrees0 => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
        crate::d3d_render::Rotation::Degrees90 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
        crate::d3d_render::Rotation::Degrees180 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
        crate::d3d_render::Rotation::Degrees270 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
    }
}

fn check_format_support(
    enumerator: &windows::Win32::Graphics::Direct3D11::ID3D11VideoProcessorEnumerator,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    required: u32,
) -> VideoResult<()> {
    // SAFETY: The enumerator is live and the DXGI format was validated as NV12 or P010.
    let support = unsafe { enumerator.CheckVideoProcessorFormat(format) }
        .map_err(|error| backend_error("CheckVideoProcessorFormat", error.code().0))?;
    if support & required != required {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d11Scaler::scale",
            "video processor does not support the requested input or output format",
        ));
    }
    Ok(())
}

fn supported_outputs(device: &D3d11Device, input: PixelFormat) -> VideoResult<Vec<PixelFormat>> {
    let size = FrameSize::new(2, 2)?;
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        InputWidth: size.width(),
        InputHeight: size.height(),
        OutputFrameRate: DXGI_RATIONAL {
            Numerator: 30,
            Denominator: 1,
        },
        OutputWidth: size.width(),
        OutputHeight: size.height(),
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    // SAFETY: Content descriptor is initialized and retained through the synchronous query.
    let enumerator = unsafe {
        device
            .video_device()
            .CreateVideoProcessorEnumerator(&raw const content)
    }
    .map_err(|error| backend_error("CreateVideoProcessorEnumerator", error.code().0))?;
    match check_format_support(
        &enumerator,
        crate::d3d11_device::dxgi_format(input),
        D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0.cast_unsigned(),
    ) {
        Ok(()) => {}
        Err(error) if error.kind() == VideoErrorKind::Unsupported => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let mut supported = Vec::new();
    for output in [PixelFormat::Nv12, PixelFormat::P010, PixelFormat::Bgra8] {
        match check_format_support(
            &enumerator,
            crate::d3d11_device::dxgi_format(output),
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT
                .0
                .cast_unsigned(),
        ) {
            Ok(()) => supported.push(output),
            Err(error) if error.kind() == VideoErrorKind::Unsupported => {}
            Err(error) => return Err(error),
        }
    }
    Ok(supported)
}

impl D3dScale for D3d11Scaler {
    type Frame = D3d11DecodedFrame;

    fn capabilities(&self) -> &[ScaleCapability] {
        &self.capabilities
    }

    fn supports_rotation(&self) -> bool {
        true
    }

    fn scale(&mut self, input: &Self::Frame, request: &ScaleRequest) -> VideoResult<Self::Frame> {
        self.scale_frame(input, request)
    }
}

fn backend_error(context: &'static str, hresult: i32) -> VideoError {
    VideoError::new(
        crate::error::backend_kind_from_hresult(hresult),
        context,
        format!("HRESULT 0x{:08X}", hresult.cast_unsigned()),
    )
}

fn scale_error(kind: VideoErrorKind, context: &'static str, message: &'static str) -> VideoError {
    VideoError::new(kind, context, message)
}

#[cfg(test)]
mod tests {
    use super::validate_request;
    use crate::{AspectMode, ColorInfo, FrameSize, PixelFormat, ScaleFilter, ScaleRequest};

    fn request(format: PixelFormat) -> crate::VideoResult<ScaleRequest> {
        Ok(ScaleRequest {
            output_size: FrameSize::new(1280, 720)?,
            output_format: format,
            source_rect: None,
            destination_rect: None,
            aspect_mode: AspectMode::Stretch,
            rotation: crate::d3d_render::Rotation::Degrees0,
            filter: ScaleFilter::DriverDefault,
            output_color: Some(if format == PixelFormat::Bgra8 {
                crate::color::bgra8_output_color()
            } else {
                ColorInfo::default()
            }),
        })
    }

    #[test]
    fn accepts_yuv_and_bgra_output_for_default_stretch() -> crate::VideoResult<()> {
        for format in [PixelFormat::Nv12, PixelFormat::P010] {
            assert!(
                validate_request(format, crate::ColorInfo::default(), &request(format)?).is_ok()
            );
        }
        assert!(
            validate_request(
                PixelFormat::Nv12,
                crate::ColorInfo::default(),
                &request(PixelFormat::Bgra8)?
            )
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn rejects_unsupported_filters_and_color_requests() -> crate::VideoResult<()> {
        let mut filtered = request(PixelFormat::Nv12)?;
        filtered.filter = ScaleFilter::Linear;
        assert!(
            validate_request(PixelFormat::Nv12, crate::ColorInfo::default(), &filtered).is_err()
        );
        let mut custom_color = request(PixelFormat::Nv12)?;
        custom_color.output_color = Some(crate::ColorInfo {
            range: crate::ColorRange::Full,
            ..crate::ColorInfo::default()
        });
        assert!(
            validate_request(
                PixelFormat::Nv12,
                crate::ColorInfo::default(),
                &custom_color
            )
            .is_err()
        );
        Ok(())
    }
}

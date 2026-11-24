//! DXVA2 video-processor scaling and NV12/P010/BGRA conversion.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{RECT, SIZE};
use windows::Win32::Media::MediaFoundation::{
    DXVA2_AYUVSample16, DXVA2_ExtendedFormat, DXVA2_ExtendedFormat_0, DXVA2_Fixed32,
    DXVA2_Fixed32_0, DXVA2_VideoDesc, DXVA2_VideoProcess_StretchX, DXVA2_VideoProcess_StretchY,
    DXVA2_VideoProcessBltParams, DXVA2_VideoProcessorRenderTarget, DXVA2_VideoSample,
};

use crate::d3d9_decode::D3d9DecodedFrame;
use crate::d3d9_device::{D3d9Device, d3d_format};
use crate::{
    D3dScale, FrameInfo, FrameSize, PixelFormat, ScaleCapability, ScaleFilter, ScaleRequest,
    VideoError, VideoErrorKind, VideoResult,
};

/// DXVA2 scaler that creates a progressive video processor for each operation.
pub struct D3d9Scaler {
    device: D3d9Device,
    capabilities: Vec<ScaleCapability>,
}

impl D3d9Scaler {
    /// Creates a scaler on an existing DXVA2 device.
    ///
    /// # Errors
    ///
    /// Returns invalid input if fixed nonzero capability bounds cannot be constructed.
    pub fn new(device: D3d9Device) -> VideoResult<Self> {
        let inputs = [PixelFormat::Nv12, PixelFormat::P010];
        let min_size = FrameSize::new(2, 2)?;
        let max_size = FrameSize::new(u32::MAX, u32::MAX)?;
        let mut capabilities = Vec::new();
        for format in inputs {
            let outputs = device.supported_video_processor_outputs(format)?;
            capabilities.push(ScaleCapability {
                input_format: format,
                output_formats: outputs,
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
        reason = "DXVA2 processor setup, blit, fence and retained source ownership are one transaction"
    )]
    fn scale_frame(
        &self,
        input: &D3d9DecodedFrame,
        request: &ScaleRequest,
    ) -> VideoResult<D3d9DecodedFrame> {
        validate_request(input.info.format, input.info.color, request)?;
        let (source_rect, destination_rect) = crate::d3d12_scale::compute_rotated_rectangles(
            input.info.size,
            request.output_size,
            request.source_rect,
            request.destination_rect,
            request.aspect_mode,
            input.sample_aspect_ratio(),
            crate::d3d_render::Rotation::Degrees0,
        )?;
        validate_chroma_rects(source_rect, destination_rect, request.output_format)?;
        let target_rect = full_frame_rect(request.output_size, "output")?;
        let output_color = if request.output_format == PixelFormat::Bgra8 {
            crate::color::bgra8_output_color()
        } else {
            request.output_color.unwrap_or(input.info.color)
        };
        let description = video_description(input.info.size, input.info.format, input.info.color)?;
        let destination_format = extended_format(request.output_format, output_color)?;
        let processor = self
            .device
            .create_video_processor(&description, request.output_format)?;
        let mut surfaces = self.device.create_surface_pool(
            request.output_size,
            request.output_format,
            1,
            DXVA2_VideoProcessorRenderTarget,
        )?;
        let output = surfaces.pop().ok_or_else(|| {
            scale_error(
                VideoErrorKind::Backend,
                "D3d9Scaler::scale",
                "DXVA2 returned no processor render target",
            )
        })?;

        let blit = DXVA2_VideoProcessBltParams {
            TargetFrame: 0,
            TargetRect: target_rect,
            ConstrictionSize: SIZE {
                cx: target_rect.right,
                cy: target_rect.bottom,
            },
            StreamingFlags: (DXVA2_VideoProcess_StretchX.0 | DXVA2_VideoProcess_StretchY.0)
                .cast_unsigned(),
            BackgroundColor: DXVA2_AYUVSample16 {
                Cr: 32_768,
                Cb: 32_768,
                Y: if request.output_format == PixelFormat::Bgra8 {
                    0
                } else {
                    4_096
                },
                Alpha: u16::MAX,
            },
            DestFormat: destination_format,
            ..Default::default()
        };
        let mut sample = DXVA2_VideoSample {
            Start: 0,
            End: 333_333,
            SampleFormat: description.SampleFormat,
            SrcSurface: ManuallyDrop::new(Some(input.surface.clone())),
            SrcRect: source_rect,
            DstRect: destination_rect,
            PlanarAlpha: DXVA2_Fixed32 {
                Anonymous: DXVA2_Fixed32_0 { ll: 0x0001_0000 },
            },
            ..Default::default()
        };
        // SAFETY: The processor, source, destination and parameter blocks are live and belong to
        // the same DXVA2 device. The sample retains its source COM reference through the call.
        let blit_result = unsafe {
            processor.VideoProcessBlt(
                &output,
                &raw const blit,
                std::slice::from_ref(&sample),
                None,
            )
        };
        drop(ManuallyDrop::into_inner(std::mem::replace(
            &mut sample.SrcSurface,
            ManuallyDrop::new(None),
        )));
        blit_result.map_err(|error| {
            backend_error("IDirectXVideoProcessor::VideoProcessBlt", error.code().0)
        })?;

        let completion = match self.device.signal_completion() {
            Ok(completion) => completion,
            Err(error) => {
                // The blit was submitted, but a completion marker could not be created. Keep
                // both resources and the decoder lease alive because their reuse is uncertain.
                std::mem::forget(output.clone());
                std::mem::forget(input.surface.clone());
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
        if request.output_format == PixelFormat::Bgra8 {
            info.color = crate::color::bgra8_output_color();
        }
        Ok(D3d9DecodedFrame {
            surface: output,
            // Scaler output surfaces belong to the processor pool, not decoder DPB slots.
            surface_index: usize::MAX,
            completion,
            info,
            sample_aspect: crate::SampleAspectRatio::SQUARE,
            surface_lease: None,
            retained_source_lease: input
                .surface_lease
                .as_ref()
                .map(crate::frame::SurfaceLease::clone_for_pending_work),
            retained_source_surface: Some(input.surface.clone()),
        })
    }
}

fn full_frame_rect(size: FrameSize, name: &'static str) -> VideoResult<RECT> {
    let width = i32::try_from(size.width()).map_err(|_| {
        scale_error(
            VideoErrorKind::InvalidInput,
            "D3d9Scaler::scale",
            if name == "input" {
                "input width exceeds DXVA2 rectangle range"
            } else {
                "output width exceeds DXVA2 rectangle range"
            },
        )
    })?;
    let height = i32::try_from(size.height()).map_err(|_| {
        scale_error(
            VideoErrorKind::InvalidInput,
            "D3d9Scaler::scale",
            if name == "input" {
                "input height exceeds DXVA2 rectangle range"
            } else {
                "output height exceeds DXVA2 rectangle range"
            },
        )
    })?;
    Ok(RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    })
}

impl D3dScale for D3d9Scaler {
    type Frame = D3d9DecodedFrame;

    fn capabilities(&self) -> &[ScaleCapability] {
        &self.capabilities
    }

    fn scale(&mut self, input: &Self::Frame, request: &ScaleRequest) -> VideoResult<Self::Frame> {
        self.scale_frame(input, request)
    }
}

fn validate_request(
    input_format: PixelFormat,
    input_color: crate::ColorInfo,
    request: &ScaleRequest,
) -> VideoResult<()> {
    if request.rotation != crate::d3d_render::Rotation::Degrees0 {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d9Scaler::scale",
            "DXVA2 scaler does not expose frame rotation",
        ));
    }
    if request.filter != ScaleFilter::DriverDefault {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d9Scaler::scale",
            "DXVA2 scaling currently supports only the driver-default filter",
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
            "D3d9Scaler::scale",
            "DXVA2 scaling accepts NV12/P010 input and NV12/P010/BGRA8 output",
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
            "D3d9Scaler::scale",
            "HDR-to-SDR tone mapping is not implemented for BGRA8 output",
        ));
    }
    if request
        .output_color
        .is_some_and(|color| color != expected_output_color)
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d9Scaler::scale",
            "DXVA2 processor default color conversion cannot satisfy explicit output metadata",
        ));
    }
    if matches!(request.output_format, PixelFormat::Nv12 | PixelFormat::P010)
        && (!request.output_size.width().is_multiple_of(2)
            || !request.output_size.height().is_multiple_of(2))
    {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d9Scaler::scale",
            "4:2:0 output dimensions must be even",
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
            "D3d9Scaler::scale",
            "4:2:0 processing rectangles must use even coordinates and dimensions",
        ));
    }
    Ok(())
}

fn video_description(
    size: FrameSize,
    format: PixelFormat,
    color: crate::ColorInfo,
) -> VideoResult<DXVA2_VideoDesc> {
    let d3d_format = d3d_format(format).ok_or_else(|| {
        scale_error(
            VideoErrorKind::Unsupported,
            "D3d9Scaler::scale",
            "DXVA2 processor input supports NV12 and P010",
        )
    })?;
    Ok(DXVA2_VideoDesc {
        SampleWidth: size.width(),
        SampleHeight: size.height(),
        Format: d3d_format,
        SampleFormat: extended_format(format, color)?,
        InputSampleFreq: windows::Win32::Media::MediaFoundation::DXVA2_Frequency {
            Numerator: 30,
            Denominator: 1,
        },
        OutputFrameFreq: windows::Win32::Media::MediaFoundation::DXVA2_Frequency {
            Numerator: 30,
            Denominator: 1,
        },
        ..Default::default()
    })
}

fn extended_format(
    format: PixelFormat,
    color: crate::ColorInfo,
) -> VideoResult<DXVA2_ExtendedFormat> {
    use crate::{ColorMatrix, ColorPrimaries, ColorRange, TransferCharacteristic};

    if matches!(
        color.transfer,
        TransferCharacteristic::Smpte2084 | TransferCharacteristic::AribStdB67
    ) || matches!(
        color.primaries,
        ColorPrimaries::Bt2020 | ColorPrimaries::DisplayP3
    ) || matches!(
        color.matrix,
        ColorMatrix::Bt2020Constant | ColorMatrix::Bt2020NonConstant | ColorMatrix::Fcc
    ) {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d9Scaler::color_space",
            "DXVA2 extended format cannot represent BT.2020, Display-P3, PQ, HLG or FCC metadata",
        ));
    }
    let nominal_range = match color.range {
        ColorRange::Full => 1_u32,
        ColorRange::Limited => 2,
        ColorRange::Unspecified => {
            if format == PixelFormat::Bgra8 {
                1
            } else {
                2
            }
        }
    };
    let matrix = match color.matrix {
        ColorMatrix::Identity if format == PixelFormat::Bgra8 => 0,
        ColorMatrix::Bt470Bg | ColorMatrix::Smpte170M => 2,
        ColorMatrix::Smpte240M => 3,
        ColorMatrix::Bt709 | ColorMatrix::Unspecified => 1,
        ColorMatrix::Identity
        | ColorMatrix::Fcc
        | ColorMatrix::Bt2020Constant
        | ColorMatrix::Bt2020NonConstant => {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d9Scaler::color_space",
                "DXVA2 cannot map the requested YCbCr matrix",
            ));
        }
    };
    let primaries = match color.primaries {
        ColorPrimaries::Unspecified | ColorPrimaries::Bt709 => 2_u32,
        ColorPrimaries::Bt470M => 3,
        ColorPrimaries::Bt470Bg => 4,
        ColorPrimaries::Smpte170M => 5,
        ColorPrimaries::Smpte240M => 6,
        ColorPrimaries::Bt2020 | ColorPrimaries::DisplayP3 => {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d9Scaler::color_space",
                "DXVA2 cannot map the requested color primaries",
            ));
        }
    };
    let transfer = match color.transfer {
        TransferCharacteristic::Unspecified
        | TransferCharacteristic::Bt709
        | TransferCharacteristic::Smpte170M => 5_u32,
        TransferCharacteristic::Gamma22 => 4,
        TransferCharacteristic::Gamma28 => 8,
        TransferCharacteristic::Smpte240M => 6,
        TransferCharacteristic::Srgb => 7,
        TransferCharacteristic::Linear => 1,
        TransferCharacteristic::Bt2020TenBit
        | TransferCharacteristic::Smpte2084
        | TransferCharacteristic::AribStdB67 => {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d9Scaler::color_space",
                "DXVA2 cannot map the requested transfer function",
            ));
        }
    };
    let chroma = if format == PixelFormat::Bgra8 {
        0
    } else {
        8_u32
    };
    let value = 2_u32
        | (chroma << 8)
        | (nominal_range << 12)
        | (matrix << 15)
        | (primaries << 22)
        | (transfer << 27);
    Ok(DXVA2_ExtendedFormat {
        Anonymous: DXVA2_ExtendedFormat_0 { value },
    })
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
            assert!(validate_request(format, ColorInfo::default(), &request(format)?).is_ok());
        }
        assert!(
            validate_request(
                PixelFormat::Nv12,
                ColorInfo::default(),
                &request(PixelFormat::Bgra8)?
            )
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn rejects_unsupported_filters_and_odd_yuv_dimensions() -> crate::VideoResult<()> {
        let mut filter = request(PixelFormat::Nv12)?;
        filter.filter = ScaleFilter::Linear;
        assert!(validate_request(PixelFormat::Nv12, ColorInfo::default(), &filter).is_err());
        let mut odd = request(PixelFormat::Nv12)?;
        odd.output_size = FrameSize::new(1279, 720)?;
        assert!(validate_request(PixelFormat::Nv12, ColorInfo::default(), &odd).is_err());
        let mut odd_bgra = request(PixelFormat::Bgra8)?;
        odd_bgra.output_size = FrameSize::new(1279, 719)?;
        assert!(validate_request(PixelFormat::Nv12, ColorInfo::default(), &odd_bgra).is_ok());
        Ok(())
    }
}

//! D3D12 video-processor configuration and frame recording.

use std::mem::ManuallyDrop;

use crate::d3d_render::Rotation;
use crate::d3d12_device::{D3d12Device, D3d12Texture};
use crate::{
    AspectMode, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, D3dBackend, FrameInfo,
    FrameSize, GpuFrame, PixelFormat, Rect, SampleAspectRatio, ScaleCapability, ScaleFilter,
    ScaleRequest, VideoError, VideoErrorKind, VideoResult, WaitStatus,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS, D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
    D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE, ID3D12CommandAllocator, ID3D12CommandList,
    ID3D12Device, ID3D12PipelineState,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_TYPE,
    DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
    DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020, DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020, DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
    DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_RATIONAL,
};
use windows::Win32::Media::MediaFoundation::{
    D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT, D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
    D3D12_VIDEO_FIELD_TYPE_NONE, D3D12_VIDEO_FORMAT, D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
    D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE, D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS,
    D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC, D3D12_VIDEO_PROCESS_ORIENTATION,
    D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_90, D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_180,
    D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_270, D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT,
    D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS, D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC,
    D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED,
    D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY, D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY,
    D3D12_VIDEO_SIZE_RANGE, ID3D12VideoProcessCommandList, ID3D12VideoProcessor,
};
use windows::core::Interface;

const DEFAULT_FRAME_RATE: DXGI_RATIONAL = DXGI_RATIONAL {
    Numerator: 30,
    Denominator: 1,
};

fn frame_rate_from_info(info: &FrameInfo) -> DXGI_RATIONAL {
    let ticks_per_second = u64::from(info.time_base.denominator());
    let ticks_per_frame = match info.duration {
        Some(duration) if duration > 0 => u64::from(info.time_base.numerator())
            .checked_mul(duration)
            .unwrap_or(0),
        _ => u64::from(info.time_base.numerator()),
    };
    if ticks_per_frame == 0 || ticks_per_second == 0 {
        return DEFAULT_FRAME_RATE;
    }
    let divisor = gcd(ticks_per_second, ticks_per_frame);
    let numerator = ticks_per_second / divisor;
    let denominator = ticks_per_frame / divisor;
    match (u32::try_from(numerator), u32::try_from(denominator)) {
        (Ok(numerator), Ok(denominator)) if numerator != 0 && denominator != 0 => DXGI_RATIONAL {
            Numerator: numerator,
            Denominator: denominator,
        },
        _ => DEFAULT_FRAME_RATE,
    }
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// One-input D3D12 video processor for a fixed format, color space and size pair.
///
/// The caller owns allocator/list reset, resource transitions, queue submission and fence
/// synchronization. Input and output textures must already be in video-process read/write states.
/// The processor receives explicit source and destination rectangles; `D3d12Scaler` applies the
/// frame's pixel aspect ratio while computing those rectangles.
pub struct D3d12ScaleProcessor {
    device: ID3D12Device,
    processor: ID3D12VideoProcessor,
    input_size: FrameSize,
    output_size: FrameSize,
    source_size: FrameSize,
    destination_size: FrameSize,
    input_format: PixelFormat,
    output_format: PixelFormat,
    color_space: DXGI_COLOR_SPACE_TYPE,
    output_color_space: DXGI_COLOR_SPACE_TYPE,
    output_color: ColorInfo,
    max_output_size: FrameSize,
}

impl D3d12ScaleProcessor {
    /// Queries hardware support and creates a fixed-configuration video processor.
    ///
    /// Input and output use the supplied color metadata, mapped for each pixel format. The frame
    /// rate defaults to 30/1, as in `FFmpeg` when no input timing is available.
    ///
    /// # Errors
    ///
    /// Returns unsupported, invalid-input or backend errors when formats/configuration are not
    /// accepted by the adapter.
    pub fn create(
        device: &D3d12Device,
        input_size: FrameSize,
        input_format: PixelFormat,
        output_size: FrameSize,
        output_format: PixelFormat,
        color: ColorInfo,
    ) -> VideoResult<Self> {
        Self::create_with_colors(
            device,
            input_size,
            input_format,
            output_size,
            output_format,
            color,
            color,
        )
    }

    /// Creates a processor with distinct input and output color metadata.
    ///
    /// # Errors
    ///
    /// Returns unsupported, invalid-input or backend errors when formats/configuration are not
    /// accepted by the adapter.
    pub fn create_with_colors(
        device: &D3d12Device,
        input_size: FrameSize,
        input_format: PixelFormat,
        output_size: FrameSize,
        output_format: PixelFormat,
        input_color: ColorInfo,
        output_color: ColorInfo,
    ) -> VideoResult<Self> {
        Self::create_for_rectangles(
            device,
            input_size,
            input_format,
            output_size,
            output_format,
            input_color,
            output_color,
            (input_size, output_size),
            false,
            DEFAULT_FRAME_RATE,
            crate::RgbaColor::BLACK,
        )
    }

    /// Creates a processor for the rectangle sizes implied by a scale request.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, unsupported or backend errors for invalid or unsupported requests.
    pub fn create_for_request(
        device: &D3d12Device,
        input_size: FrameSize,
        input_format: PixelFormat,
        input_color: ColorInfo,
        request: &ScaleRequest,
    ) -> VideoResult<Self> {
        if request.output_format == PixelFormat::Bgra8
            && request.output_color.is_some_and(|color| {
                color != ColorInfo::default() && color != crate::color::bgra8_output_color()
            })
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create_for_request",
                "BGRA8 output uses canonical SDR BT.709/Gamma 2.2 metadata",
            ));
        }
        if request.filter != ScaleFilter::DriverDefault {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create_for_request",
                "only driver-default scaling is supported",
            ));
        }
        let (source, destination) = compute_rotated_rectangles(
            input_size,
            request.output_size,
            request.source_rect,
            request.destination_rect,
            request.aspect_mode,
            SampleAspectRatio::SQUARE,
            request.rotation,
        )?;
        Self::create_for_rectangles(
            device,
            input_size,
            input_format,
            request.output_size,
            request.output_format,
            input_color,
            if request.output_format == PixelFormat::Bgra8 {
                crate::color::bgra8_output_color()
            } else {
                request.output_color.unwrap_or(input_color)
            },
            (rect_size(source)?, rect_size(destination)?),
            request.rotation != Rotation::Degrees0,
            DEFAULT_FRAME_RATE,
            crate::RgbaColor::BLACK,
        )
    }

    #[expect(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "processor configuration includes both texture and rectangle dimensions and color spaces"
    )]
    fn create_for_rectangles(
        device: &D3d12Device,
        input_size: FrameSize,
        input_format: PixelFormat,
        output_size: FrameSize,
        output_format: PixelFormat,
        input_color: ColorInfo,
        output_color: ColorInfo,
        rectangles: (FrameSize, FrameSize),
        enable_orientation: bool,
        frame_rate: DXGI_RATIONAL,
        background: crate::RgbaColor,
    ) -> VideoResult<Self> {
        let (source_size, destination_size) = rectangles;
        if matches!(input_format, PixelFormat::Nv12 | PixelFormat::P010) {
            validate_420_size(input_size, "input")?;
        }
        if matches!(output_format, PixelFormat::Nv12 | PixelFormat::P010) {
            validate_420_size(output_size, "output")?;
        }
        full_rect(input_size)?;
        full_rect(output_size)?;
        let input_dxgi = dxgi_format(input_format)?;
        let output_dxgi = dxgi_format(output_format)?;
        let color_space = checked_color_space(input_format, input_color)?;
        let output_color_space = checked_color_space(output_format, output_color)?;
        if output_format == PixelFormat::Bgra8
            && matches!(
                input_color.transfer,
                crate::TransferCharacteristic::Smpte2084
                    | crate::TransferCharacteristic::AribStdB67
            )
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "HDR-to-SDR tone mapping is not implemented for BGRA8 output",
            ));
        }
        let input_desc = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
            Format: input_dxgi,
            ColorSpace: color_space,
            SourceAspectRatio: DXGI_RATIONAL {
                Numerator: 1,
                Denominator: 1,
            },
            DestinationAspectRatio: DXGI_RATIONAL {
                Numerator: 1,
                Denominator: 1,
            },
            FrameRate: frame_rate,
            SourceSizeRange: size_range(source_size),
            DestinationSizeRange: size_range(destination_size),
            EnableOrientation: enable_orientation.into(),
            FieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
            StereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            ..D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC::default()
        };
        let mut support = D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT {
            NodeIndex: 0,
            InputSample: windows::Win32::Media::MediaFoundation::D3D12_VIDEO_SAMPLE {
                Width: input_size.width(),
                Height: input_size.height(),
                Format: D3D12_VIDEO_FORMAT {
                    Format: input_dxgi,
                    ColorSpace: color_space,
                },
            },
            InputFieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
            InputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            InputFrameRate: frame_rate,
            OutputFormat: D3D12_VIDEO_FORMAT {
                Format: output_dxgi,
                ColorSpace: output_color_space,
            },
            OutputStereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
            OutputFrameRate: frame_rate,
            ..D3D12_FEATURE_DATA_VIDEO_PROCESS_SUPPORT::default()
        };
        let support_bytes = u32::try_from(std::mem::size_of_val(&support)).map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::create",
                "support structure size exceeds u32",
            )
        })?;
        // SAFETY: `support` is initialized with the matching SDK structure and remains writable
        // for the duration of the synchronous feature query.
        unsafe {
            device.video_device().CheckFeatureSupport(
                D3D12_FEATURE_VIDEO_PROCESS_SUPPORT,
                (&raw mut support).cast(),
                support_bytes,
            )
        }
        .map_err(|error| backend_error("D3D12 video process support query", error.code().0))?;
        if !support
            .SupportFlags
            .contains(D3D12_VIDEO_PROCESS_SUPPORT_FLAG_SUPPORTED)
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "adapter does not support this video-process format and size configuration",
            ));
        }

        validate_output_range(destination_size, support.ScaleSupport.OutputSizeRange)?;
        if support
            .ScaleSupport
            .Flags
            .contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_EVEN_DIMENSIONS_ONLY)
            && (!destination_size.width().is_multiple_of(2)
                || !destination_size.height().is_multiple_of(2))
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "adapter requires even destination rectangle dimensions",
            ));
        }
        if support
            .ScaleSupport
            .Flags
            .contains(D3D12_VIDEO_SCALE_SUPPORT_FLAG_POW2_ONLY)
            && !supports_power_of_two_scale(destination_size, support.ScaleSupport.OutputSizeRange)
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "output size does not meet the adapter's power-of-two scaling restriction",
            ));
        }
        let max_output_size = FrameSize::new(
            support.ScaleSupport.OutputSizeRange.MaxWidth,
            support.ScaleSupport.OutputSizeRange.MaxHeight,
        )
        .map_err(|_| {
            scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "adapter reported an empty output-size range",
            )
        })?;

        let output_desc = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC {
            Format: output_dxgi,
            ColorSpace: output_color_space,
            AlphaFillMode: D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE,
            AlphaFillModeSourceStreamIndex: 0,
            BackgroundColor: [
                background.red.clamp(0.0, 1.0),
                background.green.clamp(0.0, 1.0),
                background.blue.clamp(0.0, 1.0),
                background.alpha.clamp(0.0, 1.0),
            ],
            FrameRate: frame_rate,
            EnableStereo: false.into(),
        };
        // SAFETY: Descriptors are initialized, the input descriptor slice is live, and the typed
        // COM result owns the processor returned by the D3D12 video device.
        let processor = unsafe {
            device
                .video_device()
                .CreateVideoProcessor::<ID3D12VideoProcessor>(
                    0,
                    &raw const output_desc,
                    &[input_desc],
                )
        }
        .map_err(|error| {
            backend_error("ID3D12VideoDevice::CreateVideoProcessor", error.code().0)
        })?;
        Ok(Self {
            device: device.device().clone(),
            processor,
            input_size,
            output_size,
            source_size,
            destination_size,
            input_format,
            output_format,
            color_space,
            output_color_space,
            output_color,
            max_output_size,
        })
    }

    /// Records one processor pass into an open video-process command list.
    ///
    /// # Errors
    ///
    /// Returns invalid-input/state errors when texture metadata or resource states do not match
    /// this processor configuration.
    pub fn record(
        &self,
        command_list: &ID3D12VideoProcessCommandList,
        input: &D3d12Texture,
        input_subresource: u32,
        output: &D3d12Texture,
        output_subresource: u32,
    ) -> VideoResult<()> {
        self.record_rectangles(
            command_list,
            input,
            input_subresource,
            output,
            output_subresource,
            rect_from(Rect {
                x: 0,
                y: 0,
                size: self.input_size,
            })?,
            rect_from(Rect {
                x: 0,
                y: 0,
                size: self.output_size,
            })?,
            Rotation::Degrees0,
        )
    }

    /// Records one pass using the shared scale request's crop and aspect policy.
    ///
    /// The processor must have been created for the requested output size and format. D3D12's
    /// processor configuration here uses driver-default scaling; other interpolation choices
    /// are reported as unsupported. The requested output color must match processor creation.
    ///
    /// # Errors
    ///
    /// Returns invalid-input/state or unsupported errors for incompatible requests and textures.
    pub fn record_request(
        &self,
        command_list: &ID3D12VideoProcessCommandList,
        input: &D3d12Texture,
        input_subresource: u32,
        output: &D3d12Texture,
        output_subresource: u32,
        request: &ScaleRequest,
    ) -> VideoResult<()> {
        if request.output_size != self.output_size || request.output_format != self.output_format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::record_request",
                "request output size/format differs from processor configuration",
            ));
        }
        if request.filter != ScaleFilter::DriverDefault {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::record_request",
                "D3D12 video processor exposes driver-default scaling in this port",
            ));
        }
        if request.output_format == PixelFormat::Bgra8
            && request.output_color.is_some_and(|color| {
                color != ColorInfo::default() && color != crate::color::bgra8_output_color()
            })
        {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12Scaler::scale",
                "BGRA8 output uses canonical SDR BT.709/Gamma 2.2 metadata",
            ));
        }
        let requested_output_color = request.output_color.map(|requested| {
            if self.output_format == PixelFormat::Bgra8 && requested == ColorInfo::default() {
                crate::color::bgra8_output_color()
            } else {
                requested
            }
        });
        if requested_output_color.is_some_and(|requested| requested != self.output_color) {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::record_request",
                "output color conversion requires a processor configured for that color space",
            ));
        }
        let (source, destination) = compute_rotated_rectangles(
            self.input_size,
            self.output_size,
            request.source_rect,
            request.destination_rect,
            request.aspect_mode,
            SampleAspectRatio::SQUARE,
            request.rotation,
        )?;
        self.record_rectangles(
            command_list,
            input,
            input_subresource,
            output,
            output_subresource,
            source,
            destination,
            request.rotation,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "these arguments are the exact input and output records for one processor call"
    )]
    fn record_rectangles(
        &self,
        command_list: &ID3D12VideoProcessCommandList,
        input: &D3d12Texture,
        input_subresource: u32,
        output: &D3d12Texture,
        output_subresource: u32,
        source_rect: RECT,
        destination_rect: RECT,
        rotation: Rotation,
    ) -> VideoResult<()> {
        if rect_size(source_rect)? != self.source_size
            || rect_size(destination_rect)? != self.destination_size
        {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::record",
                "source or destination rectangle size differs from processor configuration",
            ));
        }
        validate_process_devices(&self.device, command_list, input, output)?;
        if input.size() != self.input_size || input.format() != self.input_format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::record",
                "input texture does not match processor configuration",
            ));
        }
        if output.size() != self.output_size || output.format() != self.output_format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::record",
                "output texture does not match processor configuration",
            ));
        }
        if input_subresource >= u32::from(input.array_size())
            || output_subresource >= u32::from(output.array_size())
        {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleProcessor::record",
                "texture subresource index is out of range",
            ));
        }
        if input.slice_state(input_subresource)? != D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ
            || output.slice_state(output_subresource)? != D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE
        {
            return Err(scale_error(
                VideoErrorKind::InvalidState,
                "D3d12ScaleProcessor::record",
                "input/output textures must be in VIDEO_PROCESS_READ/WRITE states",
            ));
        }

        let target_rect = full_rect(self.output_size)?;
        let mut input_arguments = [D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS::default()];
        input_arguments[0].InputStream[0].pTexture2D =
            ManuallyDrop::new(Some(input.resource().clone()));
        input_arguments[0].InputStream[0].Subresource = input_subresource;
        input_arguments[0].Transform.SourceRectangle = source_rect;
        input_arguments[0].Transform.DestinationRectangle = destination_rect;
        input_arguments[0].Transform.Orientation = d3d12_orientation(rotation);
        input_arguments[0].AlphaBlending.Alpha = 1.0;
        let mut output_arguments = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS::default();
        output_arguments.OutputStream[0].pTexture2D =
            ManuallyDrop::new(Some(output.resource().clone()));
        output_arguments.OutputStream[0].Subresource = output_subresource;
        output_arguments.TargetRectangle = target_rect;
        // SAFETY: All COM resources and argument structures remain alive for the synchronous
        // ProcessFrames call. The ManuallyDrop clones are released immediately afterward.
        unsafe {
            command_list.ProcessFrames(
                &self.processor,
                &raw const output_arguments,
                &input_arguments,
            );
            ManuallyDrop::drop(&mut input_arguments[0].InputStream[0].pTexture2D);
            ManuallyDrop::drop(&mut output_arguments.OutputStream[0].pTexture2D);
        }
        Ok(())
    }

    #[must_use]
    pub const fn input_size(&self) -> FrameSize {
        self.input_size
    }

    #[must_use]
    pub const fn output_size(&self) -> FrameSize {
        self.output_size
    }

    #[must_use]
    pub const fn color_space(&self) -> DXGI_COLOR_SPACE_TYPE {
        self.color_space
    }

    #[must_use]
    pub const fn output_color_space(&self) -> DXGI_COLOR_SPACE_TYPE {
        self.output_color_space
    }

    #[must_use]
    pub const fn max_output_size(&self) -> FrameSize {
        self.max_output_size
    }
}

/// Maps `FFmpeg` color metadata and supported YUV/RGB formats to D3D12 video-processor spaces.
#[must_use]
pub const fn dxgi_color_space(format: PixelFormat, color: ColorInfo) -> DXGI_COLOR_SPACE_TYPE {
    let is_ten_bit = matches!(format, PixelFormat::P010);
    let is_bt2020 = matches!(
        color.matrix,
        ColorMatrix::Bt2020Constant | ColorMatrix::Bt2020NonConstant
    ) || matches!(color.primaries, ColorPrimaries::Bt2020);
    if is_ten_bit {
        if is_bt2020 {
            return match color.transfer {
                crate::TransferCharacteristic::Smpte2084 => {
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020
                }
                crate::TransferCharacteristic::AribStdB67 => {
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020
                }
                _ => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020,
            };
        }
        return DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
    }
    if is_bt2020 {
        return DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020;
    }
    if matches!(color.matrix, ColorMatrix::Bt470Bg | ColorMatrix::Smpte170M) {
        return DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601;
    }
    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
}

pub(crate) fn checked_color_space(
    format: PixelFormat,
    color: ColorInfo,
) -> VideoResult<DXGI_COLOR_SPACE_TYPE> {
    if format == PixelFormat::Bgra8 {
        if color != ColorInfo::default() && color != crate::color::bgra8_output_color() {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "BGRA8 output uses the crate's SDR BT.709/Gamma 2.2 full-range color metadata",
            ));
        }
        if matches!(
            color.transfer,
            crate::TransferCharacteristic::Smpte2084 | crate::TransferCharacteristic::AribStdB67
        ) {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12ScaleProcessor::create",
                "BGRA8 output cannot preserve PQ or HLG transfer characteristics",
            ));
        }
        return Ok(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
    }
    if matches!(
        color.transfer,
        crate::TransferCharacteristic::Smpte2084 | crate::TransferCharacteristic::AribStdB67
    ) && (format != PixelFormat::P010
        || !matches!(
            color.matrix,
            ColorMatrix::Bt2020Constant | ColorMatrix::Bt2020NonConstant
        ))
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d12ScaleProcessor::create",
            "PQ and HLG require P010 with a BT.2020 matrix for DXGI color-space mapping",
        ));
    }
    let limited = dxgi_color_space(format, color);
    if color.range != ColorRange::Full {
        return Ok(limited);
    }
    if limited == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601 {
        Ok(DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601)
    } else if limited == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709 {
        Ok(DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709)
    } else if limited == DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020 {
        Ok(DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020)
    } else if limited == DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020 {
        Ok(DXGI_COLOR_SPACE_YCBCR_FULL_GHLG_TOPLEFT_P2020)
    } else {
        Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d12ScaleProcessor::create",
            "full-range PQ YCbCr has no matching DXGI color space",
        ))
    }
}

fn dxgi_format(format: PixelFormat) -> VideoResult<DXGI_FORMAT> {
    match format {
        PixelFormat::Nv12 => Ok(DXGI_FORMAT_NV12),
        PixelFormat::P010 => Ok(DXGI_FORMAT_P010),
        PixelFormat::Bgra8 => Ok(DXGI_FORMAT_B8G8R8A8_UNORM),
        PixelFormat::Rgb10A2 => Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d12ScaleProcessor::create",
            "RGB10A2 video-process textures are not yet supported",
        )),
    }
}

fn size_range(size: FrameSize) -> D3D12_VIDEO_SIZE_RANGE {
    D3D12_VIDEO_SIZE_RANGE {
        MaxWidth: size.width(),
        MaxHeight: size.height(),
        MinWidth: size.width(),
        MinHeight: size.height(),
    }
}

#[cfg(test)]
pub(crate) fn compute_rectangles(
    input_size: FrameSize,
    output_size: FrameSize,
    source: Option<Rect>,
    destination: Option<Rect>,
    aspect: AspectMode,
) -> VideoResult<(RECT, RECT)> {
    compute_rectangles_with_aspect(
        input_size,
        output_size,
        source,
        destination,
        aspect,
        SampleAspectRatio::SQUARE,
    )
}

pub(crate) fn compute_rotated_rectangles(
    source_size: FrameSize,
    output_size: FrameSize,
    source_rect: Option<Rect>,
    destination_rect: Option<Rect>,
    aspect_mode: AspectMode,
    sample_aspect: SampleAspectRatio,
    rotation: Rotation,
) -> VideoResult<(RECT, RECT)> {
    if rotation == Rotation::Degrees0 {
        return compute_rectangles_with_aspect(
            source_size,
            output_size,
            source_rect,
            destination_rect,
            aspect_mode,
            sample_aspect,
        );
    }
    if let Some(source) = source_rect {
        validate_rect(source, source_size, "source")?;
    }
    let rotated_size = if matches!(rotation, Rotation::Degrees90 | Rotation::Degrees270) {
        FrameSize::new(source_size.height(), source_size.width())?
    } else {
        source_size
    };
    let rotated_aspect = if matches!(rotation, Rotation::Degrees90 | Rotation::Degrees270) {
        SampleAspectRatio::new(sample_aspect.denominator(), sample_aspect.numerator())?
    } else {
        sample_aspect
    };
    let source = source_rect.unwrap_or(Rect {
        x: 0,
        y: 0,
        size: source_size,
    });
    let rotated_source = rotate_rect(source, source_size, rotation)?;
    let (rotated_crop, destination) = compute_rectangles_with_aspect(
        rotated_size,
        output_size,
        Some(rotated_source),
        destination_rect,
        aspect_mode,
        rotated_aspect,
    )?;
    let left = u32::try_from(rotated_crop.left).map_err(|_| rectangle_error("negative crop x"))?;
    let top = u32::try_from(rotated_crop.top).map_err(|_| rectangle_error("negative crop y"))?;
    let right =
        u32::try_from(rotated_crop.right).map_err(|_| rectangle_error("negative crop right"))?;
    let bottom =
        u32::try_from(rotated_crop.bottom).map_err(|_| rectangle_error("negative crop bottom"))?;
    let unrotated = match rotation {
        Rotation::Degrees0 => Rect {
            x: left,
            y: top,
            size: FrameSize::new(right - left, bottom - top)?,
        },
        Rotation::Degrees90 => Rect {
            x: top,
            y: source_size.height() - right,
            size: FrameSize::new(bottom - top, right - left)?,
        },
        Rotation::Degrees180 => Rect {
            x: source_size.width() - right,
            y: source_size.height() - bottom,
            size: FrameSize::new(right - left, bottom - top)?,
        },
        Rotation::Degrees270 => Rect {
            x: source_size.width() - bottom,
            y: left,
            size: FrameSize::new(bottom - top, right - left)?,
        },
    };
    Ok((rect_from(unrotated)?, destination))
}

fn rotate_rect(rect: Rect, source_size: FrameSize, rotation: Rotation) -> VideoResult<Rect> {
    Ok(match rotation {
        Rotation::Degrees0 => rect,
        Rotation::Degrees90 => Rect {
            x: source_size.height() - rect.y - rect.size.height(),
            y: rect.x,
            size: FrameSize::new(rect.size.height(), rect.size.width())?,
        },
        Rotation::Degrees180 => Rect {
            x: source_size.width() - rect.x - rect.size.width(),
            y: source_size.height() - rect.y - rect.size.height(),
            size: rect.size,
        },
        Rotation::Degrees270 => Rect {
            x: rect.y,
            y: source_size.width() - rect.x - rect.size.width(),
            size: FrameSize::new(rect.size.height(), rect.size.width())?,
        },
    })
}

fn d3d12_orientation(rotation: Rotation) -> D3D12_VIDEO_PROCESS_ORIENTATION {
    match rotation {
        Rotation::Degrees0 => D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT,
        Rotation::Degrees90 => D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_90,
        Rotation::Degrees180 => D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_180,
        Rotation::Degrees270 => D3D12_VIDEO_PROCESS_ORIENTATION_CLOCKWISE_270,
    }
}

fn compute_rectangles_with_aspect(
    input_size: FrameSize,
    output_size: FrameSize,
    source: Option<Rect>,
    destination: Option<Rect>,
    aspect: AspectMode,
    sample_aspect: SampleAspectRatio,
) -> VideoResult<(RECT, RECT)> {
    let source = source.unwrap_or(Rect {
        x: 0,
        y: 0,
        size: input_size,
    });
    let destination = destination.unwrap_or(Rect {
        x: 0,
        y: 0,
        size: output_size,
    });
    validate_rect(source, input_size, "source")?;
    validate_rect(destination, output_size, "destination")?;
    let mut src = rect_from(source)?;
    let mut dst = rect_from(destination)?;

    match aspect {
        AspectMode::Stretch => {}
        AspectMode::Contain => {
            let (width, height) = fit_inside(source.size, destination.size, sample_aspect);
            dst.left = i32::try_from(destination.x + (destination.size.width() - width) / 2)
                .map_err(|_| rectangle_error("destination x exceeds D3D12 RECT range"))?;
            dst.top = i32::try_from(destination.y + (destination.size.height() - height) / 2)
                .map_err(|_| rectangle_error("destination y exceeds D3D12 RECT range"))?;
            dst.right = dst.left
                + i32::try_from(width)
                    .map_err(|_| rectangle_error("destination width exceeds D3D12 RECT range"))?;
            dst.bottom = dst.top
                + i32::try_from(height)
                    .map_err(|_| rectangle_error("destination height exceeds D3D12 RECT range"))?;
        }
        AspectMode::Cover => {
            let (width, height) = fit_cover(source.size, destination.size, sample_aspect);
            let left = source.x + (source.size.width() - width) / 2;
            let top = source.y + (source.size.height() - height) / 2;
            src.left = i32::try_from(left)
                .map_err(|_| rectangle_error("source x exceeds D3D12 RECT range"))?;
            src.top = i32::try_from(top)
                .map_err(|_| rectangle_error("source y exceeds D3D12 RECT range"))?;
            src.right = src.left
                + i32::try_from(width)
                    .map_err(|_| rectangle_error("source width exceeds D3D12 RECT range"))?;
            src.bottom = src.top
                + i32::try_from(height)
                    .map_err(|_| rectangle_error("source height exceeds D3D12 RECT range"))?;
        }
    }
    Ok((src, dst))
}

fn validate_rect(rect: Rect, bounds: FrameSize, label: &'static str) -> VideoResult<()> {
    let right = rect.x.checked_add(rect.size.width());
    let bottom = rect.y.checked_add(rect.size.height());
    if rect.size.width() == 0
        || rect.size.height() == 0
        || right.is_none_or(|value| value > bounds.width())
        || bottom.is_none_or(|value| value > bounds.height())
    {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d12ScaleProcessor::record_request",
            if label == "source" {
                "source rectangle is empty or outside input bounds"
            } else {
                "destination rectangle is empty or outside output bounds"
            },
        ));
    }
    let _ = rect_from(rect)?;
    Ok(())
}

fn rect_from(rect: Rect) -> VideoResult<RECT> {
    let left = i32::try_from(rect.x)
        .map_err(|_| rectangle_error("rectangle x exceeds D3D12 RECT range"))?;
    let top = i32::try_from(rect.y)
        .map_err(|_| rectangle_error("rectangle y exceeds D3D12 RECT range"))?;
    let right = i32::try_from(u64::from(rect.x) + u64::from(rect.size.width()))
        .map_err(|_| rectangle_error("rectangle right edge exceeds D3D12 RECT range"))?;
    let bottom = i32::try_from(u64::from(rect.y) + u64::from(rect.size.height()))
        .map_err(|_| rectangle_error("rectangle bottom edge exceeds D3D12 RECT range"))?;
    Ok(RECT {
        left,
        top,
        right,
        bottom,
    })
}

fn full_rect(size: FrameSize) -> VideoResult<RECT> {
    rect_from(Rect { x: 0, y: 0, size })
}

fn rect_size(rect: RECT) -> VideoResult<FrameSize> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| rectangle_error("invalid rectangle width"))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| rectangle_error("invalid rectangle height"))?;
    FrameSize::new(width, height)
}

fn fit_inside(source: FrameSize, destination: FrameSize, aspect: SampleAspectRatio) -> (u32, u32) {
    let display_width = u128::from(source.width()) * u128::from(aspect.numerator());
    let display_height = u128::from(source.height()) * u128::from(aspect.denominator());
    let width_limited = u128::from(destination.width()) * display_height
        <= u128::from(destination.height()) * display_width;
    if width_limited {
        (
            destination.width(),
            ratio_floor(
                u128::from(destination.width()) * display_height,
                display_width,
            ),
        )
    } else {
        (
            ratio_floor(
                u128::from(destination.height()) * display_width,
                display_height,
            ),
            destination.height(),
        )
    }
}

fn fit_cover(source: FrameSize, destination: FrameSize, aspect: SampleAspectRatio) -> (u32, u32) {
    let width_limited = u128::from(source.width())
        * u128::from(aspect.numerator())
        * u128::from(destination.height())
        <= u128::from(source.height())
            * u128::from(aspect.denominator())
            * u128::from(destination.width());
    if width_limited {
        (
            source.width(),
            ratio_floor(
                u128::from(source.width())
                    * u128::from(aspect.numerator())
                    * u128::from(destination.height()),
                u128::from(aspect.denominator()) * u128::from(destination.width()),
            ),
        )
    } else {
        (
            ratio_floor(
                u128::from(source.height())
                    * u128::from(aspect.denominator())
                    * u128::from(destination.width()),
                u128::from(aspect.numerator()) * u128::from(destination.height()),
            ),
            source.height(),
        )
    }
}

fn ratio_floor(numerator: u128, denominator: u128) -> u32 {
    u32::try_from(numerator / denominator)
        .unwrap_or(u32::MAX)
        .max(1)
}

fn rectangle_error(message: &'static str) -> VideoError {
    scale_error(
        VideoErrorKind::InvalidInput,
        "D3d12ScaleProcessor::record_request",
        message,
    )
}

fn scale_error(kind: VideoErrorKind, context: &'static str, message: &'static str) -> VideoError {
    VideoError::new(kind, context, message)
}

fn backend_error(context: &'static str, code: i32) -> VideoError {
    VideoError::new(
        crate::error::backend_kind_from_hresult(code),
        context,
        format!(
            "Windows API failed with HRESULT 0x{:08X}",
            code.cast_unsigned()
        ),
    )
}

/// A D3D12 texture and frame metadata that can be passed to the D3D12 scaler.
///
/// `from_texture` assumes the caller has synchronized any external GPU work before scaling.
pub struct D3d12ScaleFrame {
    texture: Arc<Mutex<D3d12Texture>>,
    subresource: u32,
    visible_origin: (u32, u32),
    sample_aspect: SampleAspectRatio,
    info: FrameInfo,
    completion: Option<crate::d3d12_device::D3d12CompletionToken>,
    retained_decode: Option<crate::d3d12_decode::D3d12PendingDecode>,
    surface_lease: Option<Arc<AtomicUsize>>,
}

impl D3d12ScaleFrame {
    /// Wraps a caller-owned texture with matching frame metadata.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the visible size exceeds the texture or the format differs.
    pub fn from_texture(texture: D3d12Texture, info: FrameInfo) -> VideoResult<Self> {
        Self::from_texture_subresource(texture, 0, info)
    }

    /// Wraps one array slice of a synchronized texture.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for a mismatched frame or out-of-range array slice.
    pub fn from_texture_subresource(
        texture: D3d12Texture,
        subresource: u32,
        info: FrameInfo,
    ) -> VideoResult<Self> {
        if !frame_fits_texture(info.size, texture.size()) || texture.format() != info.format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleFrame::from_texture",
                "visible frame must fit inside the texture and use the same format",
            ));
        }
        validate_frame_subresource(subresource, texture.array_size())?;
        Ok(Self {
            texture: Arc::new(Mutex::new(texture)),
            subresource,
            visible_origin: (0, 0),
            sample_aspect: SampleAspectRatio::SQUARE,
            info,
            completion: None,
            retained_decode: None,
            surface_lease: None,
        })
    }

    /// Wraps a texture and a fence token signaled after its producing GPU work was submitted.
    ///
    /// The caller must retain the producer's command allocator, decoder, upload buffers and other
    /// referenced resources until the token completes. Dropping this frame waits for the texture's
    /// completion; a fence failure retains the texture resource for the process lifetime.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for mismatched metadata or devices, or a backend error if the
    /// texture's owning device cannot be queried.
    pub fn from_texture_with_completion(
        texture: D3d12Texture,
        info: FrameInfo,
        completion: crate::d3d12_device::D3d12CompletionToken,
    ) -> VideoResult<Self> {
        Self::from_subresource_with_completion(texture, 0, info, completion)
    }

    /// Wraps an array slice and its producer's completion token.
    ///
    /// The producer must retain all other GPU resources until completion, as described by
    /// [`Self::from_texture_with_completion`].
    ///
    /// # Errors
    ///
    /// Returns invalid-input for a mismatched device, metadata or array slice, or a backend error.
    pub fn from_subresource_with_completion(
        texture: D3d12Texture,
        subresource: u32,
        info: FrameInfo,
        completion: crate::d3d12_device::D3d12CompletionToken,
    ) -> VideoResult<Self> {
        let mut device: Option<ID3D12Device> = None;
        // SAFETY: The resource and the output slot are live for this synchronous COM call.
        let device_result = unsafe { texture.resource().GetDevice(&raw mut device) }
            .map_err(|error| backend_error("D3D12 scale frame GetDevice", error.code().0));
        let validation = device_result.and_then(|()| {
            if device.as_ref() != Some(completion.device()) {
                return Err(scale_error(
                    VideoErrorKind::InvalidInput,
                    "D3d12ScaleFrame::from_texture_with_completion",
                    "texture and completion fence belong to different devices",
                ));
            }
            if !frame_fits_texture(info.size, texture.size()) || texture.format() != info.format {
                return Err(scale_error(
                    VideoErrorKind::InvalidInput,
                    "D3d12ScaleFrame::from_texture_with_completion",
                    "visible frame must fit inside the texture and use the same format",
                ));
            }
            validate_frame_subresource(subresource, texture.array_size())?;
            Ok(())
        });
        if let Err(error) = validation {
            // A mismatched or unqueryable device means the token cannot be trusted to cover this
            // texture; retain the COM resource rather than release potentially in-flight work.
            if device.as_ref() == Some(completion.device()) {
                wait_or_retain_texture(&texture, &completion);
            } else {
                std::mem::forget(texture.resource().clone());
            }
            return Err(error);
        }
        Ok(Self {
            texture: Arc::new(Mutex::new(texture)),
            subresource,
            visible_origin: (0, 0),
            sample_aspect: SampleAspectRatio::SQUARE,
            info,
            completion: Some(completion),
            retained_decode: None,
            surface_lease: None,
        })
    }

    pub(crate) fn from_decoder_output(
        texture: Arc<Mutex<D3d12Texture>>,
        subresource: u32,
        info: FrameInfo,
        retained_decode: crate::d3d12_decode::D3d12PendingDecode,
        surface_lease: Arc<AtomicUsize>,
    ) -> VideoResult<Self> {
        let completion = retained_decode.completion();
        let guard = texture.lock().map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidState,
                "D3d12ScaleFrame::from_decoder_output",
                "decoder output texture lock was poisoned",
            )
        })?;
        if !frame_fits_texture(info.size, guard.size()) || guard.format() != info.format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleFrame::from_decoder_output",
                "decoded frame metadata does not match its output texture",
            ));
        }
        validate_frame_subresource(subresource, guard.array_size())?;
        drop(guard);
        surface_lease.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            texture,
            subresource,
            visible_origin: (0, 0),
            sample_aspect: SampleAspectRatio::SQUARE,
            info,
            completion: Some(completion),
            retained_decode: Some(retained_decode),
            surface_lease: Some(surface_lease),
        })
    }

    pub(crate) fn from_existing_decoder_output(
        texture: Arc<Mutex<D3d12Texture>>,
        subresource: u32,
        info: FrameInfo,
        completion: Option<crate::d3d12_device::D3d12CompletionToken>,
        surface_lease: Arc<AtomicUsize>,
    ) -> VideoResult<Self> {
        let guard = texture.lock().map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidState,
                "D3d12ScaleFrame::from_existing_decoder_output",
                "decoder output texture lock was poisoned",
            )
        })?;
        if !frame_fits_texture(info.size, guard.size()) || guard.format() != info.format {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleFrame::from_existing_decoder_output",
                "frame metadata does not match its decoder texture",
            ));
        }
        validate_frame_subresource(subresource, guard.array_size())?;
        drop(guard);
        surface_lease.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            texture,
            subresource,
            visible_origin: (0, 0),
            sample_aspect: SampleAspectRatio::SQUARE,
            info,
            completion,
            retained_decode: None,
            surface_lease: Some(surface_lease),
        })
    }

    #[must_use]
    pub const fn subresource(&self) -> u32 {
        self.subresource
    }

    /// Clones the fence token associated with the producer, if this frame has one.
    #[must_use]
    pub fn completion_token(&self) -> Option<crate::d3d12_device::D3d12CompletionToken> {
        self.completion.clone()
    }

    /// Sets the input pixel aspect ratio used for contain/cover scaling. An AV1 caller can pass the
    /// value from `D3d12PreparedAv1Picture::sample_aspect_ratio` before submitting the picture.
    /// Scaler output uses square pixels.
    pub const fn set_sample_aspect_ratio(&mut self, aspect: SampleAspectRatio) {
        self.sample_aspect = aspect;
    }

    #[must_use]
    pub const fn sample_aspect_ratio(&self) -> SampleAspectRatio {
        self.sample_aspect
    }

    /// Returns the visible image rectangle in texture coordinates.
    #[must_use]
    pub const fn visible_rect(&self) -> Rect {
        Rect {
            x: self.visible_origin.0,
            y: self.visible_origin.1,
            size: self.info.size,
        }
    }

    /// Sets the visible image origin within the texture. Source rectangles in `ScaleRequest` are
    /// relative to this visible image, so cropping composes with this origin.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the visible rectangle exceeds the texture, or invalid-state if
    /// the texture lock is poisoned.
    pub fn set_visible_origin(&mut self, x: u32, y: u32) -> VideoResult<()> {
        let texture = self.texture.lock().map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidState,
                "D3d12ScaleFrame::set_visible_origin",
                "frame texture lock was poisoned",
            )
        })?;
        validate_rect(
            Rect {
                x,
                y,
                size: self.info.size,
            },
            texture.size(),
            "source",
        )?;
        self.visible_origin = (x, y);
        Ok(())
    }

    /// Applies a codec visible rectangle returned by `AvcDecodeState` or `HevcDecodeState`.
    /// The rectangle size must match this frame's visible `FrameInfo::size`.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for a mismatched or out-of-bounds rectangle, or invalid-state if the
    /// texture lock is poisoned.
    pub fn set_visible_rect(&mut self, rect: Rect) -> VideoResult<()> {
        if rect.size != self.info.size {
            return Err(scale_error(
                VideoErrorKind::InvalidInput,
                "D3d12ScaleFrame::set_visible_rect",
                "rectangle size differs from frame metadata",
            ));
        }
        self.set_visible_origin(rect.x, rect.y)
    }

    /// Returns a locked view of the tracked texture resource.
    ///
    /// # Errors
    ///
    /// Returns invalid-state if another operation panicked while holding the frame lock.
    pub fn texture(&self) -> VideoResult<std::sync::MutexGuard<'_, D3d12Texture>> {
        self.texture.lock().map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidState,
                "D3d12ScaleFrame::texture",
                "frame texture lock was poisoned",
            )
        })
    }
}

impl Drop for D3d12ScaleFrame {
    fn drop(&mut self) {
        let completion_safe = if let Some(completion) = &self.completion {
            let texture = self
                .texture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            wait_or_retain_texture(&texture, completion)
        } else {
            true
        };
        if let Some(lease) = &self.surface_lease {
            if completion_safe {
                lease.fetch_sub(1, Ordering::AcqRel);
            } else if let Some(lease) = self.surface_lease.take() {
                // Keep this surface permanently unavailable when the GPU fence cannot establish
                // that the texture is safe to reuse.
                std::mem::forget(lease);
            }
        }
        if !completion_safe && let Some(pending) = self.retained_decode.take() {
            // Avoid a second blocking wait in `D3d12PendingDecode::drop`; the resources were
            // already retained above because completion could not be established in 30 seconds.
            std::mem::forget(pending);
        }
    }
}

fn wait_or_retain_texture(
    texture: &D3d12Texture,
    completion: &crate::d3d12_device::D3d12CompletionToken,
) -> bool {
    match completion.wait(Duration::from_secs(30)) {
        Ok(WaitStatus::Complete) => true,
        Ok(WaitStatus::TimedOut) | Err(_) => {
            std::mem::forget(texture.resource().clone());
            false
        }
    }
}

impl GpuFrame for D3d12ScaleFrame {
    fn backend(&self) -> D3dBackend {
        D3dBackend::D3d12
    }

    fn info(&self) -> &FrameInfo {
        &self.info
    }

    fn is_ready(&self) -> VideoResult<bool> {
        self.completion.as_ref().map_or(
            Ok(true),
            crate::d3d12_device::D3d12CompletionToken::is_ready,
        )
    }

    fn wait(&mut self, timeout: Duration) -> VideoResult<WaitStatus> {
        self.completion
            .as_ref()
            .map_or(Ok(WaitStatus::Complete), |token| token.wait(timeout))
    }
}

/// Synchronous D3D12 video-processor adapter implementing the shared `D3dScale` contract.
///
/// It owns a video-process queue and reusable allocator/list. Calls wait for the preceding pass
/// before allocator reuse and wait for the resulting fence before returning a frame, ensuring
/// resources can be safely released through the regular Rust ownership path.
pub struct D3d12Scaler {
    device: D3d12Device,
    queue: crate::d3d12_device::D3d12Queue,
    allocator: ID3D12CommandAllocator,
    command_list: ID3D12VideoProcessCommandList,
    capabilities: Vec<ScaleCapability>,
    failed_submission: bool,
    background: crate::RgbaColor,
}

impl D3d12Scaler {
    /// Creates a process queue/list and probes common NV12/P010 format pairs at 1080p.
    ///
    /// Capability rows are exact for their probed 1920x1080 input size; each requested operation
    /// is still checked against the adapter using its actual sizes and color spaces.
    ///
    /// # Errors
    ///
    /// Returns backend errors if command resources cannot be created.
    pub fn create(device: &D3d12Device) -> VideoResult<Self> {
        let queue = device.create_queue(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
        // SAFETY: The allocator type matches the video-process queue this scaler owns.
        let allocator = unsafe {
            device
                .device()
                .CreateCommandAllocator::<ID3D12CommandAllocator>(
                    D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
                )
        }
        .map_err(|error| backend_error("Create video-process command allocator", error.code().0))?;
        // SAFETY: The process allocator and command-list type match; no initial pipeline state is
        // used for D3D12 video processing.
        let command_list = unsafe {
            device
                .device()
                .CreateCommandList::<_, _, ID3D12VideoProcessCommandList>(
                    0,
                    D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
                    &allocator,
                    None::<&ID3D12PipelineState>,
                )
        }
        .map_err(|error| backend_error("Create video-process command list", error.code().0))?;
        // SAFETY: Newly created D3D12 command lists begin open and are closed before their first reset.
        unsafe { command_list.Close() }.map_err(|error| {
            backend_error("Close initial video-process command list", error.code().0)
        })?;

        let mut scaler = Self {
            device: device.clone(),
            queue,
            allocator,
            command_list,
            capabilities: Vec::new(),
            failed_submission: false,
            background: crate::RgbaColor::BLACK,
        };
        scaler.capabilities = probe_capabilities(&scaler.device);
        Ok(scaler)
    }

    /// Sets the output background used for uncovered pixels by the next and later process passes.
    pub fn set_background_color(&mut self, background: crate::RgbaColor) {
        self.background = background;
    }
}

impl crate::D3dScale for D3d12Scaler {
    type Frame = D3d12ScaleFrame;

    fn capabilities(&self) -> &[ScaleCapability] {
        &self.capabilities
    }

    fn supports_rotation(&self) -> bool {
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "submission and failure handling must preserve GPU resource lifetimes"
    )]
    fn scale(&mut self, input: &Self::Frame, request: &ScaleRequest) -> VideoResult<Self::Frame> {
        if self.failed_submission {
            return Err(scale_error(
                VideoErrorKind::InvalidState,
                "D3d12Scaler::scale",
                "a previous command-list or submission failure requires recreating the scaler",
            ));
        }
        if request.filter != ScaleFilter::DriverDefault {
            return Err(scale_error(
                VideoErrorKind::Unsupported,
                "D3d12Scaler::scale",
                "D3D12 video processor exposes driver-default scaling in this port",
            ));
        }
        let output_color = if request.output_format == PixelFormat::Bgra8 {
            crate::color::bgra8_output_color()
        } else {
            request.output_color.unwrap_or(input.info.color)
        };
        let (source_rect, destination_rect) = compute_rotated_rectangles(
            input.info.size,
            request.output_size,
            request.source_rect,
            request.destination_rect,
            request.aspect_mode,
            input.sample_aspect,
            request.rotation,
        )?;
        let mut input_texture = input.texture.lock().map_err(|_| {
            scale_error(
                VideoErrorKind::InvalidState,
                "D3d12Scaler::scale",
                "input frame texture lock was poisoned",
            )
        })?;
        let processor = D3d12ScaleProcessor::create_for_rectangles(
            &self.device,
            input_texture.size(),
            input.info.format,
            request.output_size,
            request.output_format,
            input.info.color,
            output_color,
            (rect_size(source_rect)?, rect_size(destination_rect)?),
            request.rotation != Rotation::Degrees0,
            frame_rate_from_info(&input.info),
            self.background,
        )?;
        let mut output = self
            .device
            .create_video_process_texture(request.output_size, request.output_format)?;
        let source_rect =
            translate_computed_rect(source_rect, input.visible_origin, input.info.size)?;
        validate_process_devices(
            self.device.device(),
            &self.command_list,
            &input_texture,
            &output,
        )?;
        if let Some(completion) = input.completion.as_ref() {
            match completion.wait(Duration::from_secs(30))? {
                WaitStatus::Complete => {}
                WaitStatus::TimedOut => {
                    return Err(scale_error(
                        VideoErrorKind::Timeout,
                        "D3d12Scaler::scale",
                        "input frame did not complete before timeout",
                    ));
                }
            }
        }
        if self.queue.wait_idle(Duration::from_secs(30))? != WaitStatus::Complete {
            return Err(scale_error(
                VideoErrorKind::Timeout,
                "D3d12Scaler::scale",
                "video-process queue did not become idle before timeout",
            ));
        }
        let generic_command_list: ID3D12CommandList = self
            .command_list
            .cast()
            .map_err(|error| backend_error("Cast video-process command list", error.code().0))?;
        // SAFETY: The queue was idled above, so no prior list execution still uses this allocator.
        unsafe { self.allocator.Reset() }.map_err(|error| {
            backend_error("Reset video-process command allocator", error.code().0)
        })?;
        // SAFETY: The matching allocator has been reset after prior execution completed.
        unsafe { self.command_list.Reset(&self.allocator) }
            .map_err(|error| backend_error("Reset video-process command list", error.code().0))?;

        let original_input_state = input_texture.slice_state(input.subresource)?;
        let recording = (|| {
            transition_if_needed(
                &mut input_texture,
                &self.command_list,
                input.subresource,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
            )?;
            transition_if_needed(
                &mut output,
                &self.command_list,
                0,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
            )?;
            processor.record_rectangles(
                &self.command_list,
                &input_texture,
                input.subresource,
                &output,
                0,
                source_rect,
                destination_rect,
                request.rotation,
            )?;
            transition_if_needed(
                &mut input_texture,
                &self.command_list,
                input.subresource,
                original_input_state,
            )?;
            transition_if_needed(
                &mut output,
                &self.command_list,
                0,
                windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_COMMON,
            )?;
            // SAFETY: All barriers and the video-process operation have been recorded.
            unsafe { self.command_list.Close() }
                .map_err(|error| backend_error("Close video-process command list", error.code().0))
        })();
        if let Err(error) = recording {
            input_texture.restore_unsubmitted_slice_state(input.subresource, original_input_state);
            // SAFETY: Closing an abandoned open list makes it eligible for a later reset.
            if unsafe { self.command_list.Close() }.is_err() {
                self.failed_submission = true;
            }
            return Err(error);
        }
        let completion = match self.queue.execute_with_completion(&[generic_command_list]) {
            Ok(completion) => completion,
            Err(error) => {
                if error.submitted {
                    self.failed_submission = true;
                    retain_inflight_resources(
                        &input_texture,
                        output,
                        processor,
                        &self.allocator,
                        &self.command_list,
                    );
                } else {
                    input_texture
                        .restore_unsubmitted_slice_state(input.subresource, original_input_state);
                }
                return Err(error.error);
            }
        };
        match completion.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) => {
                self.failed_submission = true;
                retain_inflight_resources(
                    &input_texture,
                    output,
                    processor,
                    &self.allocator,
                    &self.command_list,
                );
                return Err(scale_error(
                    VideoErrorKind::Timeout,
                    "D3d12Scaler::scale",
                    "video-process submission did not complete before the 30-second timeout",
                ));
            }
            Err(error) => {
                self.failed_submission = true;
                retain_inflight_resources(
                    &input_texture,
                    output,
                    processor,
                    &self.allocator,
                    &self.command_list,
                );
                return Err(error);
            }
        }

        let mut info = input.info;
        info.size = request.output_size;
        info.format = request.output_format;
        info.color = output_color;
        Ok(D3d12ScaleFrame {
            texture: Arc::new(Mutex::new(output)),
            subresource: 0,
            visible_origin: (0, 0),
            sample_aspect: SampleAspectRatio::SQUARE,
            info,
            completion: Some(completion),
            retained_decode: None,
            surface_lease: None,
        })
    }
}

fn validate_frame_subresource(subresource: u32, array_size: u16) -> VideoResult<()> {
    if subresource >= u32::from(array_size) {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d12ScaleFrame",
            "array slice is out of range",
        ));
    }
    Ok(())
}

fn frame_fits_texture(frame: FrameSize, texture: FrameSize) -> bool {
    frame.width() <= texture.width() && frame.height() <= texture.height()
}

fn translate_source_rect(
    visible_size: FrameSize,
    origin: (u32, u32),
    source: Option<Rect>,
) -> VideoResult<Rect> {
    let relative = source.unwrap_or(Rect {
        x: 0,
        y: 0,
        size: visible_size,
    });
    validate_rect(relative, visible_size, "source")?;
    Ok(Rect {
        x: origin
            .0
            .checked_add(relative.x)
            .ok_or_else(|| rectangle_error("source x overflow"))?,
        y: origin
            .1
            .checked_add(relative.y)
            .ok_or_else(|| rectangle_error("source y overflow"))?,
        size: relative.size,
    })
}

fn translate_computed_rect(
    rect: RECT,
    origin: (u32, u32),
    visible_size: FrameSize,
) -> VideoResult<RECT> {
    let relative = Rect {
        x: u32::try_from(rect.left).map_err(|_| rectangle_error("negative source x"))?,
        y: u32::try_from(rect.top).map_err(|_| rectangle_error("negative source y"))?,
        size: rect_size(rect)?,
    };
    rect_from(translate_source_rect(visible_size, origin, Some(relative))?)
}

// When the fence cannot establish completion, retain resources for the process lifetime: D3D12
// command lists do not retain their referenced resources. The scaler must not be used again.
fn retain_inflight_resources(
    input: &D3d12Texture,
    output: D3d12Texture,
    processor: D3d12ScaleProcessor,
    allocator: &ID3D12CommandAllocator,
    command_list: &ID3D12VideoProcessCommandList,
) {
    std::mem::forget(input.resource().clone());
    std::mem::forget(output);
    std::mem::forget(processor);
    std::mem::forget(allocator.clone());
    std::mem::forget(command_list.clone());
}

fn transition_if_needed(
    texture: &mut D3d12Texture,
    command_list: &ID3D12VideoProcessCommandList,
    slice: u32,
    desired: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
) -> VideoResult<()> {
    if texture.slice_state(slice)? != desired {
        texture.transition_process_slice(command_list, slice, desired)?;
    }
    Ok(())
}

fn validate_process_devices(
    expected: &ID3D12Device,
    command_list: &ID3D12VideoProcessCommandList,
    input: &D3d12Texture,
    output: &D3d12Texture,
) -> VideoResult<()> {
    let mut input_device: Option<ID3D12Device> = None;
    let mut output_device: Option<ID3D12Device> = None;
    let mut list_device: Option<ID3D12Device> = None;
    // SAFETY: Each interface is live and the output slots hold owned COM references.
    unsafe {
        input
            .resource()
            .GetDevice(&raw mut input_device)
            .and_then(|()| output.resource().GetDevice(&raw mut output_device))
            .and_then(|()| command_list.GetDevice(&raw mut list_device))
    }
    .map_err(|error| backend_error("D3D12 video-process GetDevice", error.code().0))?;
    if input_device.as_ref() != Some(expected)
        || output_device.as_ref() != Some(expected)
        || list_device.as_ref() != Some(expected)
    {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d12ScaleProcessor::record",
            "textures and command list must belong to the processor device",
        ));
    }
    Ok(())
}

fn probe_capabilities(device: &D3d12Device) -> Vec<ScaleCapability> {
    let Ok(probe_size) = FrameSize::new(1920, 1080) else {
        return Vec::new();
    };
    let mut capabilities = Vec::new();
    for input_format in [PixelFormat::Nv12, PixelFormat::P010] {
        for output_format in [PixelFormat::Nv12, PixelFormat::P010, PixelFormat::Bgra8] {
            let Ok(processor) = D3d12ScaleProcessor::create(
                device,
                probe_size,
                input_format,
                probe_size,
                output_format,
                ColorInfo::default(),
            ) else {
                continue;
            };
            capabilities.push(ScaleCapability {
                input_format,
                output_formats: vec![output_format],
                min_input_size: probe_size,
                max_input_size: probe_size,
                max_output_size: processor.max_output_size(),
                filters: vec![ScaleFilter::DriverDefault],
            });
        }
    }
    capabilities
}

fn validate_420_size(size: FrameSize, dimension: &'static str) -> VideoResult<()> {
    if !size.width().is_multiple_of(2) || !size.height().is_multiple_of(2) {
        return Err(scale_error(
            VideoErrorKind::InvalidInput,
            "D3d12ScaleProcessor::create",
            if dimension == "input" {
                "NV12/P010 input dimensions must be even"
            } else {
                "NV12/P010 output dimensions must be even"
            },
        ));
    }
    Ok(())
}

fn supports_power_of_two_scale(size: FrameSize, range: D3D12_VIDEO_SIZE_RANGE) -> bool {
    let (mut width, mut height) = (range.MaxWidth, range.MaxHeight);
    while width >= range.MinWidth && height >= range.MinHeight && width != 0 && height != 0 {
        if width == size.width() && height == size.height() {
            return true;
        }
        if width == 1 && height == 1 {
            break;
        }
        width = width / 2 + width % 2;
        height = height / 2 + height % 2;
    }
    false
}

fn validate_output_range(size: FrameSize, range: D3D12_VIDEO_SIZE_RANGE) -> VideoResult<()> {
    if size.width() < range.MinWidth
        || size.height() < range.MinHeight
        || size.width() > range.MaxWidth
        || size.height() > range.MaxHeight
    {
        return Err(scale_error(
            VideoErrorKind::Unsupported,
            "D3d12ScaleProcessor::create",
            "requested output dimensions exceed the adapter's video-process size range",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorMatrix, ColorPrimaries, TransferCharacteristic};

    #[test]
    fn maps_ffmpeg_d3d12_filter_color_spaces() {
        let bt2020_pq = ColorInfo {
            primaries: ColorPrimaries::Bt2020,
            matrix: ColorMatrix::Bt2020NonConstant,
            transfer: TransferCharacteristic::Smpte2084,
            ..ColorInfo::default()
        };
        assert_eq!(
            dxgi_color_space(PixelFormat::P010, bt2020_pq),
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020
        );
        let bt2020_hlg = ColorInfo {
            transfer: TransferCharacteristic::AribStdB67,
            ..bt2020_pq
        };
        assert_eq!(
            dxgi_color_space(PixelFormat::P010, bt2020_hlg),
            DXGI_COLOR_SPACE_YCBCR_STUDIO_GHLG_TOPLEFT_P2020
        );
        let sd = ColorInfo {
            matrix: ColorMatrix::Smpte170M,
            ..ColorInfo::default()
        };
        assert_eq!(
            dxgi_color_space(PixelFormat::Nv12, sd),
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601
        );
        assert_eq!(
            dxgi_color_space(PixelFormat::Nv12, ColorInfo::default()),
            DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
        );
    }

    #[test]
    fn computes_aspect_rectangles_and_rejects_out_of_bounds() -> VideoResult<()> {
        let input = FrameSize::new(1920, 1080)?;
        let output = FrameSize::new(1000, 1000)?;
        let (_, contain) = compute_rectangles(input, output, None, None, AspectMode::Contain)?;
        assert_eq!(rect_size(contain)?, FrameSize::new(1000, 562)?);
        assert_eq!(
            (contain.left, contain.top, contain.right, contain.bottom),
            (0, 219, 1000, 781)
        );
        let (cover, _) = compute_rectangles(input, output, None, None, AspectMode::Cover)?;
        assert_eq!(rect_size(cover)?, FrameSize::new(1080, 1080)?);
        assert_eq!(
            (cover.left, cover.top, cover.right, cover.bottom),
            (420, 0, 1500, 1080)
        );
        let invalid = Rect {
            x: 1900,
            y: 0,
            size: FrameSize::new(32, 32)?,
        };
        assert!(
            compute_rectangles(input, output, Some(invalid), None, AspectMode::Stretch).is_err()
        );
        Ok(())
    }

    #[test]
    fn aspect_modes_account_for_non_square_pixels() -> VideoResult<()> {
        let input = FrameSize::new(720, 576)?;
        let output = FrameSize::new(1000, 1000)?;
        let aspect = SampleAspectRatio::new(16, 15)?;
        let (_, contain) =
            compute_rectangles_with_aspect(input, output, None, None, AspectMode::Contain, aspect)?;
        assert_eq!(
            (contain.left, contain.top, contain.right, contain.bottom),
            (0, 125, 1000, 875)
        );
        let (cover, _) =
            compute_rectangles_with_aspect(input, output, None, None, AspectMode::Cover, aspect)?;
        assert_eq!(
            (cover.left, cover.top, cover.right, cover.bottom),
            (90, 0, 630, 576)
        );
        Ok(())
    }

    #[test]
    fn scaler_implements_shared_scale_trait() {
        fn assert_scale_implementation<T: crate::D3dScale>() {}
        assert_scale_implementation::<D3d12Scaler>();
    }

    #[test]
    fn checks_array_slice_bounds() {
        assert!(validate_frame_subresource(0, 1).is_ok());
        assert!(validate_frame_subresource(3, 4).is_ok());
        assert!(validate_frame_subresource(4, 4).is_err());
    }

    #[test]
    fn visible_frame_must_fit_within_padded_texture() -> VideoResult<()> {
        let padded = FrameSize::new(1920, 1088)?;
        assert!(frame_fits_texture(FrameSize::new(1920, 1080)?, padded));
        assert!(!frame_fits_texture(FrameSize::new(1920, 1090)?, padded));
        assert!(!frame_fits_texture(FrameSize::new(1922, 1080)?, padded));
        Ok(())
    }

    #[test]
    fn translates_source_crop_relative_to_visible_origin() -> VideoResult<()> {
        let size = FrameSize::new(1920, 1080)?;
        let default = translate_source_rect(size, (8, 4), None)?;
        assert_eq!(default, Rect { x: 8, y: 4, size });
        let crop = Rect {
            x: 100,
            y: 20,
            size: FrameSize::new(500, 400)?,
        };
        let actual = translate_source_rect(size, (8, 4), Some(crop))?;
        assert_eq!(
            actual,
            Rect {
                x: 108,
                y: 24,
                size: crop.size
            }
        );
        assert!(
            translate_source_rect(
                size,
                (8, 4),
                Some(Rect {
                    x: 1900,
                    y: 0,
                    size: crop.size
                })
            )
            .is_err()
        );
        assert!(translate_source_rect(size, (u32::MAX, 0), Some(crop)).is_err());
        Ok(())
    }

    #[test]
    fn honors_full_range_and_rejects_unrepresentable_pq() -> VideoResult<()> {
        let full = ColorInfo {
            range: ColorRange::Full,
            ..ColorInfo::default()
        };
        assert_eq!(
            checked_color_space(PixelFormat::Nv12, full)?,
            DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709
        );
        let pq = ColorInfo {
            range: ColorRange::Full,
            matrix: ColorMatrix::Bt2020NonConstant,
            transfer: TransferCharacteristic::Smpte2084,
            ..ColorInfo::default()
        };
        assert_eq!(
            checked_color_space(PixelFormat::P010, pq)
                .err()
                .map(|e| e.kind()),
            Some(VideoErrorKind::Unsupported)
        );
        Ok(())
    }

    #[test]
    fn checks_hardware_output_size_range() -> VideoResult<()> {
        let range = D3D12_VIDEO_SIZE_RANGE {
            MinWidth: 64,
            MinHeight: 64,
            MaxWidth: 3840,
            MaxHeight: 2160,
        };
        assert!(validate_output_range(FrameSize::new(1920, 1080)?, range).is_ok());
        assert!(validate_output_range(FrameSize::new(32, 1080)?, range).is_err());
        assert!(validate_output_range(FrameSize::new(1920, 2162)?, range).is_err());
        assert!(supports_power_of_two_scale(
            FrameSize::new(1920, 1080)?,
            range
        ));
        assert!(!supports_power_of_two_scale(
            FrameSize::new(1920, 540)?,
            range
        ));
        Ok(())
    }
}

use crate::d3d_render::Rotation;
use crate::{AspectMode, ColorInfo, FrameSize, GpuFrame, PixelFormat, Rect, VideoResult};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ScaleFilter {
    DriverDefault,
    Nearest,
    Linear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScaleCapability {
    pub input_format: PixelFormat,
    pub output_formats: Vec<PixelFormat>,
    pub min_input_size: FrameSize,
    pub max_input_size: FrameSize,
    pub max_output_size: FrameSize,
    pub filters: Vec<ScaleFilter>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScaleRequest {
    pub output_size: FrameSize,
    pub output_format: PixelFormat,
    pub source_rect: Option<Rect>,
    pub destination_rect: Option<Rect>,
    pub aspect_mode: AspectMode,
    /// Clockwise rotation applied before destination fitting.
    pub rotation: Rotation,
    pub filter: ScaleFilter,
    pub output_color: Option<ColorInfo>,
}

/// Hardware video processor for scaling, format conversion, and color conversion.
pub trait D3dScale {
    type Frame: GpuFrame;

    fn capabilities(&self) -> &[ScaleCapability];

    /// Returns whether this scaler has a GPU path for orthogonal frame rotation.
    /// Device and format capability checks may still be required when scaling.
    fn supports_rotation(&self) -> bool {
        false
    }

    /// # Errors
    ///
    /// Returns an unsupported, invalid-input, backend, timeout, or device-lost error when the
    /// requested operation cannot be submitted.
    fn scale(&mut self, input: &Self::Frame, request: &ScaleRequest) -> VideoResult<Self::Frame>;
}

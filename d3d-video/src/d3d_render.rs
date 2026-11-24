use crate::{AspectMode, FrameSize, GpuFrame, Rect, VideoResult};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PresentMode {
    VSync,
    Immediate,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Rotation {
    Degrees0,
    Degrees90,
    Degrees180,
    Degrees270,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbaColor {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl RgbaColor {
    pub const BLACK: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 1.0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderParameters {
    pub destination: Option<Rect>,
    pub aspect_mode: AspectMode,
    pub rotation: Rotation,
    pub background: RgbaColor,
    pub present_mode: PresentMode,
}

impl Default for RenderParameters {
    fn default() -> Self {
        Self {
            destination: None,
            aspect_mode: AspectMode::Contain,
            rotation: Rotation::Degrees0,
            background: RgbaColor::BLACK,
            present_mode: PresentMode::VSync,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PresentResult {
    Presented,
    Occluded,
}

/// Presents a backend-compatible GPU frame to a configured target.
pub trait D3dRender {
    type Frame: GpuFrame;

    fn target_size(&self) -> FrameSize;

    /// # Errors
    ///
    /// Returns an invalid-input, backend, or device-lost error when the target cannot be resized.
    fn resize(&mut self, size: FrameSize) -> VideoResult<()>;

    /// # Errors
    ///
    /// Returns an unsupported, backend, timeout, or device-lost error when the frame cannot be
    /// presented.
    fn render(
        &mut self,
        frame: &Self::Frame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult>;
}

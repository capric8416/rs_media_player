use crate::{
    AspectMode, FrameInfo, FrameSize, GpuFrame, PixelFormat, Rect, SampleAspectRatio, VideoResult,
};

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

/// Returns whether a processed BGRA frame already exactly covers the presentation target.
///
/// A square-pixel frame with matching dimensions needs no further aspect fitting. Background and
/// aspect mode therefore cannot affect the result, while an explicit destination or rotation still
/// requires the backend's video processor.
pub(crate) fn can_direct_present(
    info: &FrameInfo,
    sample_aspect: SampleAspectRatio,
    target_size: FrameSize,
    parameters: &RenderParameters,
) -> bool {
    info.format == PixelFormat::Bgra8
        && info.size == target_size
        && sample_aspect == SampleAspectRatio::SQUARE
        && parameters.destination.is_none()
        && parameters.rotation == Rotation::Degrees0
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

#[cfg(test)]
mod tests {
    use super::{RenderParameters, Rotation, can_direct_present};
    use crate::{ColorInfo, FrameInfo, FrameSize, PixelFormat, Rect, SampleAspectRatio, TimeBase};

    fn frame_info(size: FrameSize, format: PixelFormat) -> crate::VideoResult<FrameInfo> {
        Ok(FrameInfo {
            size,
            format,
            color: ColorInfo::default(),
            pts: None,
            duration: None,
            time_base: TimeBase::new(1, 1)?,
        })
    }

    #[test]
    fn direct_present_accepts_target_sized_square_bgra() -> crate::VideoResult<()> {
        let size = FrameSize::new(1280, 720)?;
        assert!(can_direct_present(
            &frame_info(size, PixelFormat::Bgra8)?,
            SampleAspectRatio::SQUARE,
            size,
            &RenderParameters::default(),
        ));
        Ok(())
    }

    #[test]
    fn direct_present_rejects_operations_requiring_processing() -> crate::VideoResult<()> {
        let size = FrameSize::new(1280, 720)?;
        let info = frame_info(size, PixelFormat::Bgra8)?;
        let parameters = RenderParameters {
            rotation: Rotation::Degrees90,
            ..RenderParameters::default()
        };
        assert!(!can_direct_present(
            &info,
            SampleAspectRatio::SQUARE,
            size,
            &parameters,
        ));

        let parameters = RenderParameters {
            destination: Some(Rect {
                x: 0,
                y: 0,
                size: FrameSize::new(640, 360)?,
            }),
            ..RenderParameters::default()
        };
        assert!(!can_direct_present(
            &info,
            SampleAspectRatio::SQUARE,
            size,
            &parameters,
        ));
        assert!(!can_direct_present(
            &frame_info(size, PixelFormat::Nv12)?,
            SampleAspectRatio::SQUARE,
            size,
            &RenderParameters::default(),
        ));
        assert!(!can_direct_present(
            &info,
            SampleAspectRatio::new(4, 3)?,
            size,
            &RenderParameters::default(),
        ));
        Ok(())
    }
}

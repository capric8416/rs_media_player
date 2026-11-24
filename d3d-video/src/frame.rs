use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::{ColorInfo, TimeBase, VideoError, VideoErrorKind, VideoResult};

/// Keeps a decoder surface reserved while an output frame is alive.
pub(crate) struct SurfaceLease(Arc<AtomicUsize>);

impl SurfaceLease {
    pub(crate) fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }

    pub(crate) fn clone_for_pending_work(&self) -> Self {
        Self::new(self.0.clone())
    }
}

impl Drop for SurfaceLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum D3dBackend {
    D3d9,
    D3d11,
    D3d12,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PixelFormat {
    Nv12,
    P010,
    Bgra8,
    Rgb10A2,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AspectMode {
    Contain,
    Cover,
    Stretch,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FrameSize {
    width: NonZeroU32,
    height: NonZeroU32,
}

impl FrameSize {
    /// Creates a non-empty frame size.
    ///
    /// # Errors
    ///
    /// Returns [`VideoErrorKind::InvalidInput`] when either dimension is zero.
    pub fn new(width: u32, height: u32) -> VideoResult<Self> {
        let width = NonZeroU32::new(width).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "FrameSize::new",
                "width must be non-zero",
            )
        })?;
        let height = NonZeroU32::new(height).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "FrameSize::new",
                "height must be non-zero",
            )
        })?;
        Ok(Self { width, height })
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.width.get()
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.height.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub size: FrameSize,
}

/// Pixel width-to-height ratio, reduced to lowest terms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SampleAspectRatio {
    numerator: u64,
    denominator: u64,
}

impl SampleAspectRatio {
    /// Square pixels, with equal width and height.
    pub const SQUARE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// Constructs a nonzero pixel aspect ratio and reduces it to lowest terms.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if either component is zero.
    pub fn new(numerator: u64, denominator: u64) -> VideoResult<Self> {
        if numerator == 0 || denominator == 0 {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "SampleAspectRatio::new",
                "ratio components must be nonzero",
            ));
        }
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Self {
            numerator: numerator / a,
            denominator: denominator / a,
        })
    }

    /// Derives the pixel aspect needed to display `decoded` with the aspect of `render`.
    ///
    /// # Errors
    ///
    /// Returns invalid-input if the derived ratio cannot be represented.
    pub fn from_render_size(decoded: FrameSize, render: FrameSize) -> VideoResult<Self> {
        Self::new(
            u64::from(decoded.height()) * u64::from(render.width()),
            u64::from(decoded.width()) * u64::from(render.height()),
        )
    }

    #[must_use]
    pub const fn numerator(self) -> u64 {
        self.numerator
    }

    #[must_use]
    pub const fn denominator(self) -> u64 {
        self.denominator
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameInfo {
    pub size: FrameSize,
    pub format: PixelFormat,
    pub color: ColorInfo,
    pub pts: Option<i64>,
    pub duration: Option<u64>,
    pub time_base: TimeBase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitStatus {
    Complete,
    TimedOut,
}

/// Backend-owned GPU picture with its completion primitive.
///
/// Implementations retain backend resources and follow that API's thread-affinity requirements.
pub trait GpuFrame {
    fn backend(&self) -> D3dBackend;
    fn info(&self) -> &FrameInfo;

    /// # Errors
    ///
    /// Returns a backend or device-lost error when completion cannot be queried.
    fn is_ready(&self) -> VideoResult<bool>;

    /// # Errors
    ///
    /// Returns a backend or device-lost error when the wait cannot be performed.
    fn wait(&mut self, timeout: Duration) -> VideoResult<WaitStatus>;
}

#[cfg(test)]
mod tests {
    use super::{FrameSize, SampleAspectRatio};
    use crate::VideoResult;

    #[test]
    fn derives_sample_aspect_from_render_dimensions() -> VideoResult<()> {
        let decoded = FrameSize::new(720, 576)?;
        let render = FrameSize::new(768, 576)?;
        let ratio = SampleAspectRatio::from_render_size(decoded, render)?;
        assert_eq!((ratio.numerator(), ratio.denominator()), (16, 15));
        assert_eq!(
            SampleAspectRatio::from_render_size(decoded, decoded)?,
            SampleAspectRatio::SQUARE
        );
        assert!(SampleAspectRatio::new(0, 1).is_err());
        assert!(SampleAspectRatio::new(1, 0).is_err());
        let maximum = FrameSize::new(u32::MAX, u32::MAX)?;
        assert_eq!(
            SampleAspectRatio::from_render_size(maximum, maximum)?,
            SampleAspectRatio::SQUARE
        );
        Ok(())
    }
}

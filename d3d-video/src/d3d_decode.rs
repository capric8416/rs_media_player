use crate::{
    BitDepth, ChromaSampling, CodecProfile, DecoderConfig, EncodedSample, FrameSize, GpuFrame,
    PixelFormat, VideoResult,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeCapability {
    pub profile: CodecProfile,
    pub bit_depth: BitDepth,
    pub chroma_sampling: ChromaSampling,
    pub output_formats: Vec<PixelFormat>,
    pub min_size: FrameSize,
    pub max_size: FrameSize,
}

/// Stateful compressed-video decoder backed by one Direct3D generation.
pub trait D3dDecode {
    type Frame: GpuFrame;

    fn capabilities(&self) -> &[DecodeCapability];

    /// # Errors
    ///
    /// Returns an unsupported, invalid-input, backend, or device-lost error when configuration
    /// cannot be applied.
    fn configure(&mut self, config: &DecoderConfig) -> VideoResult<()>;

    /// # Errors
    ///
    /// Returns a state, input, backend, or device-lost error when submission fails.
    fn send_sample(&mut self, sample: EncodedSample<'_>) -> VideoResult<()>;

    /// # Errors
    ///
    /// Returns a backend or device-lost error when output cannot be queried.
    fn receive_frame(&mut self) -> VideoResult<Option<Self::Frame>>;

    /// Signal end of input while retaining queued output for `receive_frame`.
    ///
    /// # Errors
    ///
    /// Returns a state, backend, or device-lost error when draining cannot begin.
    fn drain(&mut self) -> VideoResult<()>;

    /// Discard queued input/output and reference state without destroying the device.
    ///
    /// # Errors
    ///
    /// Returns a backend or device-lost error when state cannot be reset safely.
    fn flush(&mut self) -> VideoResult<()>;
}

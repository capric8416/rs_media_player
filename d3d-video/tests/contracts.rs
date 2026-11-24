use std::collections::VecDeque;
use std::time::Duration;

use d3d_video::{
    AspectMode, BitDepth, BitstreamFormat, ChromaSampling, CodecProfile, ColorInfo, D3dBackend,
    D3dDecode, D3dRender, D3dScale, DecodeCapability, DecoderConfig, EncodedSample, FrameInfo,
    FrameSize, GpuFrame, PixelFormat, PresentResult, RenderParameters, SampleTiming,
    ScaleCapability, ScaleFilter, ScaleRequest, TimeBase, VideoError, VideoErrorKind, VideoResult,
    WaitStatus,
};

#[derive(Clone, Debug)]
struct MockFrame {
    info: FrameInfo,
    ready: bool,
}

impl GpuFrame for MockFrame {
    fn backend(&self) -> D3dBackend {
        D3dBackend::D3d11
    }

    fn info(&self) -> &FrameInfo {
        &self.info
    }

    fn is_ready(&self) -> VideoResult<bool> {
        Ok(self.ready)
    }

    fn wait(&mut self, _timeout: Duration) -> VideoResult<WaitStatus> {
        self.ready = true;
        Ok(WaitStatus::Complete)
    }
}

struct MockDecoder {
    capabilities: Vec<DecodeCapability>,
    config: Option<DecoderConfig>,
    output: VecDeque<MockFrame>,
}

impl MockDecoder {
    fn new(size: FrameSize) -> Self {
        Self {
            capabilities: vec![DecodeCapability {
                profile: CodecProfile::AvcHigh,
                bit_depth: BitDepth::Eight,
                chroma_sampling: ChromaSampling::Yuv420,
                output_formats: vec![PixelFormat::Nv12],
                min_size: size,
                max_size: size,
            }],
            config: None,
            output: VecDeque::new(),
        }
    }
}

impl D3dDecode for MockDecoder {
    type Frame = MockFrame;

    fn capabilities(&self) -> &[DecodeCapability] {
        &self.capabilities
    }

    fn configure(&mut self, config: &DecoderConfig) -> VideoResult<()> {
        config.validate()?;
        self.config = Some(config.clone());
        Ok(())
    }

    fn send_sample(&mut self, sample: EncodedSample<'_>) -> VideoResult<()> {
        sample.validate()?;
        let config = self.config.as_ref().ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::NotConfigured,
                "MockDecoder::send_sample",
                "decoder has not been configured",
            )
        })?;
        let format = match config.bit_depth {
            BitDepth::Eight => PixelFormat::Nv12,
            BitDepth::Ten => PixelFormat::P010,
        };
        self.output.push_back(MockFrame {
            info: FrameInfo {
                size: config.coded_size,
                format,
                color: ColorInfo::default(),
                pts: sample.timing.pts,
                duration: sample.timing.duration,
                time_base: sample.timing.time_base,
            },
            ready: false,
        });
        Ok(())
    }

    fn receive_frame(&mut self) -> VideoResult<Option<Self::Frame>> {
        Ok(self.output.pop_front())
    }

    fn drain(&mut self) -> VideoResult<()> {
        Ok(())
    }

    fn flush(&mut self) -> VideoResult<()> {
        self.output.clear();
        Ok(())
    }
}

struct MockScaler {
    capabilities: Vec<ScaleCapability>,
}

impl MockScaler {
    fn new(size: FrameSize) -> Self {
        Self {
            capabilities: vec![ScaleCapability {
                input_format: PixelFormat::Nv12,
                output_formats: vec![PixelFormat::Nv12, PixelFormat::P010],
                min_input_size: size,
                max_input_size: size,
                max_output_size: size,
                filters: vec![ScaleFilter::DriverDefault, ScaleFilter::Linear],
            }],
        }
    }
}

impl D3dScale for MockScaler {
    type Frame = MockFrame;

    fn capabilities(&self) -> &[ScaleCapability] {
        &self.capabilities
    }

    fn scale(&mut self, input: &Self::Frame, request: &ScaleRequest) -> VideoResult<Self::Frame> {
        Ok(MockFrame {
            info: FrameInfo {
                size: request.output_size,
                format: request.output_format,
                color: request.output_color.unwrap_or(input.info.color),
                pts: input.info.pts,
                duration: input.info.duration,
                time_base: input.info.time_base,
            },
            ready: false,
        })
    }
}

struct MockRenderer {
    size: FrameSize,
    presented: usize,
}

impl D3dRender for MockRenderer {
    type Frame = MockFrame;

    fn target_size(&self) -> FrameSize {
        self.size
    }

    fn resize(&mut self, size: FrameSize) -> VideoResult<()> {
        self.size = size;
        Ok(())
    }

    fn render(
        &mut self,
        _frame: &Self::Frame,
        _parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        self.presented += 1;
        Ok(PresentResult::Presented)
    }
}

fn run_pipeline<D, S, R>(
    decoder: &mut D,
    scaler: &mut S,
    renderer: &mut R,
    sample: EncodedSample<'_>,
    scale_request: &ScaleRequest,
) -> VideoResult<PresentResult>
where
    D: D3dDecode,
    S: D3dScale<Frame = D::Frame>,
    R: D3dRender<Frame = D::Frame>,
{
    decoder.send_sample(sample)?;
    let input = decoder.receive_frame()?.ok_or_else(|| {
        VideoError::new(
            VideoErrorKind::InvalidState,
            "run_pipeline",
            "mock decoder produced no frame",
        )
    })?;
    let output = scaler.scale(&input, scale_request)?;
    renderer.render(&output, &RenderParameters::default())
}

fn size(width: u32, height: u32) -> VideoResult<FrameSize> {
    FrameSize::new(width, height)
}

#[test]
fn associated_frame_type_composes_decode_scale_and_render() -> VideoResult<()> {
    let input_size = size(1920, 1080)?;
    let output_size = size(1280, 720)?;
    let time_base = TimeBase::new(1, 90_000)?;
    let config = DecoderConfig {
        profile: CodecProfile::AvcHigh,
        bit_depth: BitDepth::Eight,
        chroma_sampling: ChromaSampling::Yuv420,
        coded_size: input_size,
        bitstream_format: BitstreamFormat::LengthPrefixed { length_size: 4 },
        codec_private: vec![1, 100, 0, 40],
    };
    let mut decoder = MockDecoder::new(input_size);
    decoder.configure(&config)?;
    let mut scaler = MockScaler::new(input_size);
    let mut renderer = MockRenderer {
        size: output_size,
        presented: 0,
    };
    let encoded = [0_u8, 0, 0, 1];
    let sample = EncodedSample {
        data: &encoded,
        timing: SampleTiming {
            dts: Some(0),
            pts: Some(3_003),
            duration: Some(3_003),
            time_base,
        },
        is_random_access: true,
    };
    let request = ScaleRequest {
        output_size,
        output_format: PixelFormat::Nv12,
        source_rect: None,
        destination_rect: None,
        aspect_mode: AspectMode::Contain,
        filter: ScaleFilter::Linear,
        output_color: None,
    };

    let result = run_pipeline(&mut decoder, &mut scaler, &mut renderer, sample, &request)?;

    assert_eq!(result, PresentResult::Presented);
    assert_eq!(renderer.presented, 1);
    Ok(())
}

#[test]
fn frame_completion_is_explicit() -> VideoResult<()> {
    let mut frame = MockFrame {
        info: FrameInfo {
            size: size(16, 16)?,
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: None,
            duration: None,
            time_base: TimeBase::new(1, 1_000)?,
        },
        ready: false,
    };

    assert!(!frame.is_ready()?);
    assert_eq!(frame.wait(Duration::from_millis(1))?, WaitStatus::Complete);
    assert!(frame.is_ready()?);
    Ok(())
}

#[test]
fn invalid_codec_framing_is_a_typed_error() -> VideoResult<()> {
    let config = DecoderConfig {
        profile: CodecProfile::Av1Main,
        bit_depth: BitDepth::Ten,
        chroma_sampling: ChromaSampling::Yuv420,
        coded_size: size(3840, 2160)?,
        bitstream_format: BitstreamFormat::AnnexB,
        codec_private: Vec::new(),
    };

    let error = config.validate().err().ok_or_else(|| {
        VideoError::new(
            VideoErrorKind::InvalidState,
            "invalid_codec_framing_is_a_typed_error",
            "invalid configuration unexpectedly passed",
        )
    })?;
    assert_eq!(error.kind(), VideoErrorKind::InvalidInput);
    assert_eq!(error.operation(), "DecoderConfig::validate");
    Ok(())
}

#[test]
fn zero_sized_frames_are_rejected() {
    let error = FrameSize::new(0, 1080).err();
    assert!(matches!(
        error.map(|value| value.kind()),
        Some(VideoErrorKind::InvalidInput)
    ));
}

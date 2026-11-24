//! Rust interfaces and Windows implementations for DXVA2/D3D9, D3D11, and D3D12 video decode,
//! processing, and presentation.
//!
//! Codec parsing and hardware submission are exposed as separate APIs; the D3D decode trait is
//! the shared lifecycle contract, while backend-specific methods currently expose the codec
//! picture submission paths. No `FFmpeg` C types are exposed.

mod av1;
#[expect(
    dead_code,
    reason = "some parsed AVC syntax is not yet represented by the supported DXVA picture subset"
)]
mod avc;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "bit-reader helpers include syntax paths not yet used by the current codec subset"
    )
)]
mod bit_reader;
mod codec;
mod color;
#[cfg(windows)]
pub mod d3d11_decode;
#[cfg(windows)]
pub mod d3d11_device;
#[cfg(windows)]
pub mod d3d11_render;
#[cfg(windows)]
pub mod d3d11_scale;
#[cfg(windows)]
pub mod d3d12_decode;
#[cfg(windows)]
pub mod d3d12_device;
#[cfg(windows)]
pub mod d3d12_render;
#[cfg(windows)]
pub mod d3d12_scale;
#[cfg(windows)]
pub mod d3d9_decode;
#[cfg(windows)]
pub mod d3d9_device;
#[cfg(windows)]
pub mod d3d9_render;
#[cfg(windows)]
pub mod d3d9_scale;
pub mod d3d_decode;
pub mod d3d_render;
pub mod d3d_scale;
mod error;
mod frame;
#[expect(
    dead_code,
    reason = "some parsed HEVC syntax is not yet represented by the supported DXVA picture subset"
)]
mod hevc;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "NAL helpers include syntax paths not yet used by the current codec subset"
    )
)]
mod nal;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "OBU helpers include syntax paths not yet used by the current AV1 picture subset"
    )
)]
mod obu;
pub mod playback;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "RBSP helpers include syntax paths not yet used by the current codec subset"
    )
)]
mod rbsp;

pub use av1::Av1DecodeState;
pub use avc::AvcDecodeState;
pub use codec::{
    BitDepth, BitstreamFormat, ChromaSampling, Codec, CodecDecodeState, CodecPrivateData,
    CodecProfile, DecoderConfig, EncodedSample, SampleTiming, TimeBase,
};
pub use color::{
    Chromaticity, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, Hdr10Metadata,
    MasteringDisplayMetadata, TransferCharacteristic,
};
pub use d3d_decode::{D3dDecode, DecodeCapability};
pub use d3d_render::{
    D3dRender, PresentMode, PresentResult, RenderParameters, RgbaColor, Rotation,
};
pub use d3d_scale::{D3dScale, ScaleCapability, ScaleFilter, ScaleRequest};
#[cfg(windows)]
pub use d3d9_decode::{
    D3d9DecodeSession, D3d9DecodedFrame, D3d9Decoder, Dxva2Av1Tile, Dxva2Buffer, Dxva2Picture,
};
#[cfg(windows)]
pub use d3d9_render::D3d9Renderer;
#[cfg(windows)]
pub use d3d9_scale::D3d9Scaler;
#[cfg(windows)]
pub use d3d11_decode::{D3d11DecodeBuffer, D3d11DecodeSession, D3d11DecodedFrame, D3d11Decoder};
#[cfg(windows)]
pub use d3d11_render::D3d11Renderer;
#[cfg(windows)]
pub use d3d11_scale::D3d11Scaler;
#[cfg(windows)]
pub use d3d12_decode::{
    D3d12DecodeArgument, D3d12DecodeSession, D3d12Decoder, D3d12InputBitstream, D3d12PendingDecode,
    D3d12PreparedAv1Picture, D3d12PreparedAvcPicture, D3d12PreparedHevcPicture,
};
#[cfg(windows)]
pub use d3d12_render::{D3d12RenderFrame, D3d12Renderer};
#[cfg(windows)]
pub use d3d12_scale::{D3d12ScaleFrame, D3d12ScaleProcessor, D3d12Scaler};
pub use error::{VideoError, VideoErrorKind, VideoResult};
pub use frame::{
    AspectMode, D3dBackend, FrameInfo, FrameSize, GpuFrame, PixelFormat, Rect, SampleAspectRatio,
    WaitStatus,
};
pub use hevc::HevcDecodeState;
pub use playback::{Mp4VideoPlayback, PlaybackError, PlaybackOptions, PlaybackStep};

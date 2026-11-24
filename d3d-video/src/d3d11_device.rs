//! D3D11 device ownership, hardware video capability discovery and decoder surface allocation.
//!
//! This module does not submit compressed pictures. Decode, scale and render operations live in
//! their own backend modules.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_VIDEO_DECODER_CONFIG, D3D11_VIDEO_DECODER_DESC, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_P010,
};
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::core::{GUID, Interface};

use crate::WaitStatus;
use crate::{
    BitDepth, ChromaSampling, CodecProfile, FrameSize, PixelFormat, VideoError, VideoErrorKind,
    VideoResult,
};

const MAX_DECODER_ARRAY_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d11DeviceOptions {
    pub enable_multithread_protection: bool,
    pub feature_level: D3D_FEATURE_LEVEL,
}

impl Default for D3d11DeviceOptions {
    fn default() -> Self {
        Self {
            enable_multithread_protection: true,
            feature_level: D3D_FEATURE_LEVEL_11_0,
        }
    }
}

/// Shared D3D11 device, immediate context and video interfaces.
#[derive(Clone)]
pub struct D3d11Device {
    inner: Arc<D3d11DeviceInner>,
}

struct D3d11DeviceInner {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    capabilities: Vec<D3d11DecodeCapability>,
    feature_level: D3D_FEATURE_LEVEL,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct D3d11DecodeCapability {
    pub profile: CodecProfile,
    pub bit_depth: BitDepth,
    pub chroma_sampling: ChromaSampling,
    pub output_format: PixelFormat,
    pub decoder_profile: GUID,
}

impl D3d11Device {
    /// Creates a hardware D3D11 device with video support and discovers decoder profiles.
    ///
    /// # Errors
    ///
    /// Returns a backend error if device or video interfaces cannot be created.
    pub fn create(options: D3d11DeviceOptions) -> VideoResult<Self> {
        // SAFETY: Null adapter selects the default hardware adapter; returned COM interfaces are
        // immediately owned by windows-rs wrappers, and all optional output pointers are valid.
        let (device, context, feature_level) = unsafe {
            let mut device = None;
            let mut context = None;
            let mut feature_level = D3D_FEATURE_LEVEL(0);
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[options.feature_level]),
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                Some(&raw mut feature_level),
                Some(&raw mut context),
            )
            .map_err(|error| backend_error("D3D11CreateDevice", error.code().0))?;
            let device = device
                .ok_or_else(|| backend_error("D3D11CreateDevice", 0x8000_4003_u32.cast_signed()))?;
            let context = context
                .ok_or_else(|| backend_error("D3D11CreateDevice", 0x8000_4003_u32.cast_signed()))?;
            (device, context, feature_level)
        };
        Self::from_device(
            device,
            context,
            feature_level,
            options.enable_multithread_protection,
        )
    }

    /// Wraps caller-owned device and immediate context, retaining their COM references.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the video interfaces or decoder profiles cannot be queried.
    pub fn from_device(
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        feature_level: D3D_FEATURE_LEVEL,
        enable_multithread_protection: bool,
    ) -> VideoResult<Self> {
        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(|error| backend_error("ID3D11Device::cast video device", error.code().0))?;
        let video_context: ID3D11VideoContext = context.cast().map_err(|error| {
            backend_error("ID3D11DeviceContext::cast video context", error.code().0)
        })?;
        if enable_multithread_protection {
            let multithread = context
                .cast::<windows::Win32::Graphics::Direct3D11::ID3D11Multithread>()
                .map_err(|error| backend_error("ID3D11Multithread::cast", error.code().0))?;
            // SAFETY: This call toggles the documented D3D11 context-protection flag on a valid
            // `ID3D11Multithread` interface held for the duration of the call.
            // This API returns the previous protection state as BOOL, not HRESULT; false is a
            // valid previous state and must not be interpreted as an operation failure.
            let _previously_protected = unsafe { multithread.SetMultithreadProtected(true) };
        }
        let capabilities = discover_capabilities(&video_device)?;
        Ok(Self {
            inner: Arc::new(D3d11DeviceInner {
                device,
                context,
                video_device,
                video_context,
                capabilities,
                feature_level,
            }),
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> &[D3d11DecodeCapability] {
        &self.inner.capabilities
    }

    #[must_use]
    pub fn feature_level(&self) -> D3D_FEATURE_LEVEL {
        self.inner.feature_level
    }

    #[must_use]
    pub fn device(&self) -> &ID3D11Device {
        &self.inner.device
    }

    #[must_use]
    pub fn immediate_context(&self) -> &ID3D11DeviceContext {
        &self.inner.context
    }

    #[must_use]
    pub fn video_device(&self) -> &ID3D11VideoDevice {
        &self.inner.video_device
    }

    #[must_use]
    pub fn video_context(&self) -> &ID3D11VideoContext {
        &self.inner.video_context
    }

    /// Allocates an array texture suitable for decoder output views.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input or backend error when the dimensions, array size, format, or
    /// texture allocation is rejected.
    pub fn create_decoder_texture(
        &self,
        size: FrameSize,
        format: PixelFormat,
        array_size: u32,
    ) -> VideoResult<ID3D11Texture2D> {
        if array_size == 0
            || array_size > MAX_DECODER_ARRAY_SIZE
            || !matches!(format, PixelFormat::Nv12 | PixelFormat::P010)
        {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d11Device::create_decoder_texture",
                "decoder texture requires 1..=64 slices and NV12 or P010 format",
            ));
        }
        let description = D3D11_TEXTURE2D_DESC {
            Width: size.width(),
            Height: size.height(),
            MipLevels: 1,
            ArraySize: array_size,
            Format: dxgi_format(format),
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_DECODER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: `description` is fully initialized, dimensions are nonzero through FrameSize,
        // array size was checked, and the output slot lives through the COM call.
        unsafe {
            self.inner
                .device
                .CreateTexture2D(&raw const description, None, Some(&raw mut texture))
        }
        .map_err(|error| backend_error("ID3D11Device::CreateTexture2D", error.code().0))?;
        texture.ok_or_else(|| {
            backend_error(
                "ID3D11Device::CreateTexture2D",
                0x8000_4003_u32.cast_signed(),
            )
        })
    }

    /// Allocates a single-slice texture suitable for video-processor output.
    ///
    /// # Errors
    ///
    /// Returns invalid-input or backend errors for unsupported output formats or allocation
    /// failure.
    pub fn create_processor_texture(
        &self,
        size: FrameSize,
        format: PixelFormat,
    ) -> VideoResult<ID3D11Texture2D> {
        if !matches!(
            format,
            PixelFormat::Nv12 | PixelFormat::P010 | PixelFormat::Bgra8
        ) {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d11Device::create_processor_texture",
                "video-processor output supports NV12, P010 and BGRA8",
            ));
        }
        let description = D3D11_TEXTURE2D_DESC {
            Width: size.width(),
            Height: size.height(),
            MipLevels: 1,
            ArraySize: 1,
            Format: dxgi_format(format),
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        // SAFETY: The descriptor is initialized, and the output slot remains writable for the
        // duration of CreateTexture2D. The returned COM object is owned by its wrapper.
        unsafe {
            self.inner
                .device
                .CreateTexture2D(&raw const description, None, Some(&raw mut texture))
        }
        .map_err(|error| {
            backend_error(
                "ID3D11Device::CreateTexture2D processor output",
                error.code().0,
            )
        })?;
        texture.ok_or_else(|| {
            backend_error(
                "CreateTexture2D processor output",
                0x8000_4003_u32.cast_signed(),
            )
        })
    }

    /// Inserts an event query after already queued immediate-context work.
    ///
    /// Poll or wait on the returned token before reusing the associated surfaces or reading back
    /// results. The token retains the context and query until it is dropped.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the event query cannot be created.
    pub fn signal_completion(&self) -> VideoResult<D3d11CompletionToken> {
        let description = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query = None;
        // SAFETY: The descriptor is initialized and the writable output slot remains valid.
        unsafe {
            self.inner
                .device
                .CreateQuery(&raw const description, Some(&raw mut query))
        }
        .map_err(|error| backend_error("ID3D11Device::CreateQuery", error.code().0))?;
        let query = query.ok_or_else(|| {
            backend_error("ID3D11Device::CreateQuery", 0x8000_4003_u32.cast_signed())
        })?;
        // SAFETY: The event query is a live ID3D11Asynchronous object created by this device.
        unsafe { self.inner.context.End(&query) };
        Ok(D3d11CompletionToken {
            context: self.inner.context.clone(),
            query,
        })
    }

    /// Reports whether the device exposes a profile and bit-depth combination.
    #[must_use]
    pub fn supports(&self, profile: CodecProfile, bit_depth: BitDepth) -> bool {
        self.inner
            .capabilities
            .iter()
            .any(|capability| capability.profile == profile && capability.bit_depth == bit_depth)
    }

    /// Returns the decoder configurations supported for this profile at the requested size.
    ///
    /// D3D11 configuration availability is dimension dependent, so the driver is queried when
    /// the actual coded size is known instead of probing a fabricated maximum during discovery.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the driver rejects configuration enumeration.
    pub fn decoder_configs(
        &self,
        profile: CodecProfile,
        bit_depth: BitDepth,
        size: FrameSize,
    ) -> VideoResult<Vec<D3D11_VIDEO_DECODER_CONFIG>> {
        let mut configs = Vec::new();
        for capability in
            self.inner.capabilities.iter().filter(|capability| {
                capability.profile == profile && capability.bit_depth == bit_depth
            })
        {
            let description = D3D11_VIDEO_DECODER_DESC {
                Guid: capability.decoder_profile,
                SampleWidth: size.width(),
                SampleHeight: size.height(),
                OutputFormat: dxgi_format(capability.output_format),
            };
            // SAFETY: The profile and format were validated during capability discovery; the
            // caller supplies a checked nonzero FrameSize and the descriptor lives through calls.
            let count = unsafe {
                self.inner
                    .video_device
                    .GetVideoDecoderConfigCount(&raw const description)
            }
            .map_err(|error| backend_error("GetVideoDecoderConfigCount", error.code().0))?;
            configs.reserve(count as usize);
            for index in 0..count {
                let mut config = D3D11_VIDEO_DECODER_CONFIG::default();
                // SAFETY: `index` is below the count returned for this descriptor; `config` is writable.
                unsafe {
                    self.inner.video_device.GetVideoDecoderConfig(
                        &raw const description,
                        index,
                        &raw mut config,
                    )
                }
                .map_err(|error| backend_error("GetVideoDecoderConfig", error.code().0))?;
                configs.push(config);
            }
        }
        Ok(configs)
    }
}

/// D3D11 event-query completion token for immediate-context work.
pub struct D3d11CompletionToken {
    context: ID3D11DeviceContext,
    query: ID3D11Query,
}

impl D3d11CompletionToken {
    /// Returns whether all commands before this token have completed on the GPU.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the driver cannot query the event.
    pub fn is_ready(&self) -> VideoResult<bool> {
        let mut complete = 0_i32;
        // SAFETY: The event query is live; `complete` is a writable BOOL-sized output buffer and
        // the size exactly matches the documented event-query result type.
        unsafe {
            self.context
                .GetData(&self.query, Some((&raw mut complete).cast()), 4, 0)
        }
        .map_err(|error| backend_error("ID3D11DeviceContext::GetData", error.code().0))?;
        Ok(complete != 0)
    }

    /// Waits for completion up to `timeout`.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the driver cannot query the event.
    pub fn wait(&self, timeout: Duration) -> VideoResult<WaitStatus> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if self.is_ready()? {
                return Ok(WaitStatus::Complete);
            }
            if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                return Ok(WaitStatus::TimedOut);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn discover_capabilities(
    video_device: &ID3D11VideoDevice,
) -> VideoResult<Vec<D3d11DecodeCapability>> {
    let mut capabilities = Vec::new();
    // SAFETY: `GetVideoDecoderProfileCount` and indexed profile retrieval are documented methods
    // on the retained video-device interface. Each index is bounded by the reported count.
    let profile_count = unsafe { video_device.GetVideoDecoderProfileCount() };
    for index in 0..profile_count {
        // SAFETY: index is strictly below `profile_count` obtained from this same interface.
        let profile_guid = unsafe { video_device.GetVideoDecoderProfile(index) }
            .map_err(|error| backend_error("GetVideoDecoderProfile", error.code().0))?;
        for (profile, bit_depth, format) in [
            (
                CodecProfile::AvcBaseline,
                BitDepth::Eight,
                PixelFormat::Nv12,
            ),
            (CodecProfile::AvcMain, BitDepth::Eight, PixelFormat::Nv12),
            (CodecProfile::AvcHigh, BitDepth::Eight, PixelFormat::Nv12),
            (CodecProfile::HevcMain, BitDepth::Eight, PixelFormat::Nv12),
            (CodecProfile::HevcMain10, BitDepth::Ten, PixelFormat::P010),
            (CodecProfile::Av1Main, BitDepth::Eight, PixelFormat::Nv12),
            (CodecProfile::Av1Main, BitDepth::Ten, PixelFormat::P010),
        ] {
            let Some(expected_guids) = known_profiles(profile, bit_depth) else {
                continue;
            };
            if !expected_guids.contains(&profile_guid) {
                continue;
            }
            // SAFETY: The profile GUID is copied from the device's supported profile list and the
            // format is the matching DXGI NV12/P010 decode surface format.
            let supported = unsafe {
                video_device.CheckVideoDecoderFormat(&raw const profile_guid, dxgi_format(format))
            }
            .map_err(|error| backend_error("CheckVideoDecoderFormat", error.code().0))?;
            if supported.as_bool() {
                capabilities.push(D3d11DecodeCapability {
                    profile,
                    bit_depth,
                    chroma_sampling: ChromaSampling::Yuv420,
                    output_format: format,
                    decoder_profile: profile_guid,
                });
            }
        }
    }
    Ok(capabilities)
}

fn known_profiles(profile: CodecProfile, bit_depth: BitDepth) -> Option<Vec<GUID>> {
    let avc_no_fgt = windows::core::GUID::from_u128(0x1b81be68_a0c7_11d3_b984_00c04f2e73c5);
    let avc_fgt = windows::core::GUID::from_u128(0x1b81be69_a0c7_11d3_b984_00c04f2e73c5);
    let guid = match (profile, bit_depth) {
        (
            CodecProfile::AvcBaseline | CodecProfile::AvcMain | CodecProfile::AvcHigh,
            BitDepth::Eight,
        ) => {
            return Some(vec![avc_no_fgt, avc_fgt]);
        }
        (CodecProfile::HevcMain, BitDepth::Eight) => {
            windows::core::GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0)
        }
        (CodecProfile::HevcMain10, BitDepth::Ten) => {
            windows::core::GUID::from_u128(0x107af0e0_ef1a_4d19_aba8_67a163073d13)
        }
        (CodecProfile::Av1Main, BitDepth::Eight | BitDepth::Ten) => {
            windows::core::GUID::from_u128(0xb8be4ccb_cf53_46ba_8d59_d6b8a6da5d2a)
        }
        _ => return None,
    };
    Some(vec![guid])
}

pub(crate) fn dxgi_format(format: PixelFormat) -> DXGI_FORMAT {
    match format {
        PixelFormat::Nv12 => DXGI_FORMAT_NV12,
        PixelFormat::P010 => DXGI_FORMAT_P010,
        PixelFormat::Bgra8 => DXGI_FORMAT_B8G8R8A8_UNORM,
        PixelFormat::Rgb10A2 => DXGI_FORMAT(0),
    }
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    VideoError::new(
        crate::error::backend_kind_from_hresult(code),
        operation,
        "D3D11 operation failed",
    )
    .with_backend_code(code)
}

#[cfg(test)]
mod tests {
    use super::{MAX_DECODER_ARRAY_SIZE, dxgi_format, known_profiles};
    use crate::{BitDepth, CodecProfile, PixelFormat};
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010};

    #[test]
    fn maps_supported_codec_profiles_to_windows_profile_guids() {
        for (profile, depth) in [
            (CodecProfile::AvcBaseline, BitDepth::Eight),
            (CodecProfile::AvcMain, BitDepth::Eight),
            (CodecProfile::AvcHigh, BitDepth::Eight),
            (CodecProfile::HevcMain, BitDepth::Eight),
            (CodecProfile::HevcMain10, BitDepth::Ten),
            (CodecProfile::Av1Main, BitDepth::Eight),
            (CodecProfile::Av1Main, BitDepth::Ten),
        ] {
            assert!(known_profiles(profile, depth).is_some());
        }
        assert!(known_profiles(CodecProfile::HevcMain10, BitDepth::Eight).is_none());
        assert_eq!(
            known_profiles(CodecProfile::Av1Main, BitDepth::Eight),
            known_profiles(CodecProfile::Av1Main, BitDepth::Ten)
        );
    }

    #[test]
    fn maps_only_decoder_surface_formats_and_caps_array_size() {
        assert_eq!(dxgi_format(PixelFormat::Nv12), DXGI_FORMAT_NV12);
        assert_eq!(dxgi_format(PixelFormat::P010), DXGI_FORMAT_P010);
        assert_eq!(MAX_DECODER_ARRAY_SIZE, 64);
    }
}

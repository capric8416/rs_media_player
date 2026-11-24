//! DXVA2/D3D9 device ownership, decoder capability discovery and surface allocation.

use std::ptr;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::{
    D3D_SDK_VERSION, D3DCREATE_FPU_PRESERVE, D3DCREATE_MULTITHREADED,
    D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DDEVTYPE_HAL, D3DDISPLAYMODE, D3DFMT_A8R8G8B8,
    D3DFORMAT, D3DGETDATA_FLUSH, D3DISSUE_END, D3DMULTISAMPLE_NONE, D3DPOOL_DEFAULT,
    D3DPRESENT_INTERVAL_DEFAULT, D3DPRESENT_PARAMETERS, D3DPRESENTFLAG_VIDEO, D3DQUERYTYPE_EVENT,
    D3DSWAPEFFECT_DISCARD, Direct3DCreate9, Direct3DCreate9Ex, IDirect3D9, IDirect3D9Ex,
    IDirect3DDevice9, IDirect3DQuery9, IDirect3DSurface9,
};
use windows::Win32::Media::MediaFoundation::{
    DXVA2_ConfigPictureDecode, DXVA2_ExtendedFormat, DXVA2_ExtendedFormat_0, DXVA2_Frequency,
    DXVA2_SampleProgressiveFrame, DXVA2_VideoDecoderRenderTarget, DXVA2_VideoDesc,
    DXVA2_VideoRenderTargetType, DXVA2CreateDirect3DDeviceManager9, IDirect3DDeviceManager9,
    IDirectXVideoAccelerationService, IDirectXVideoDecoderService, IDirectXVideoProcessorService,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::{GUID, Interface};

use crate::{
    BitDepth, ChromaSampling, CodecProfile, FrameSize, PixelFormat, VideoError, VideoErrorKind,
    VideoResult, WaitStatus,
};

const DEFAULT_ADAPTER: u32 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d9DeviceOptions {
    pub adapter_ordinal: u32,
    pub enable_multithread_protection: bool,
}

impl Default for D3d9DeviceOptions {
    fn default() -> Self {
        Self {
            adapter_ordinal: DEFAULT_ADAPTER,
            enable_multithread_protection: true,
        }
    }
}

/// Shared D3D9 device and DXVA2 device manager.
#[derive(Clone)]
pub struct D3d9Device {
    inner: Rc<D3d9DeviceInner>,
}

struct D3d9DeviceInner {
    d3d9: IDirect3D9,
    device: IDirect3DDevice9,
    manager: IDirect3DDeviceManager9,
    decoder_service: IDirectXVideoDecoderService,
    acceleration_service: IDirectXVideoAccelerationService,
    capabilities: Vec<D3d9DecodeCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct D3d9DecodeCapability {
    pub profile: CodecProfile,
    pub bit_depth: BitDepth,
    pub chroma_sampling: ChromaSampling,
    pub output_format: PixelFormat,
    pub decoder_guid: GUID,
}

impl D3d9Device {
    /// Creates a `D3D9Ex` device where available, falls back to D3D9, and binds DXVA2 services.
    ///
    /// # Errors
    ///
    /// Returns a backend error if D3D9, the device manager, or DXVA2 video services fail.
    pub fn create(options: D3d9DeviceOptions) -> VideoResult<Self> {
        // SAFETY: GetDesktopWindow has no preconditions and returns the process desktop HWND.
        let desktop = unsafe { GetDesktopWindow() };
        let mut present = D3DPRESENT_PARAMETERS {
            BackBufferWidth: 640,
            BackBufferHeight: 480,
            BackBufferFormat: D3DFORMAT(0),
            BackBufferCount: 0,
            MultiSampleType: D3DMULTISAMPLE_NONE,
            MultiSampleQuality: 0,
            SwapEffect: D3DSWAPEFFECT_DISCARD,
            hDeviceWindow: desktop,
            Windowed: windows::core::BOOL(1),
            EnableAutoDepthStencil: windows::core::BOOL(0),
            AutoDepthStencilFormat: D3DFORMAT(0),
            Flags: D3DPRESENTFLAG_VIDEO,
            FullScreen_RefreshRateInHz: 0,
            PresentationInterval: D3DPRESENT_INTERVAL_DEFAULT as u32,
        };
        let mut behavior = (D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE) as u32;
        if options.enable_multithread_protection {
            behavior |= D3DCREATE_MULTITHREADED as u32;
        }

        // Prefer D3D9Ex, matching FFmpeg's path, then retry with classic D3D9.
        // SAFETY: D3D_SDK_VERSION is the version constant expected by the runtime.
        let ex = unsafe { Direct3DCreate9Ex(D3D_SDK_VERSION) }.ok();
        let (d3d9, device) = if let Some(ex) = ex {
            let base: IDirect3D9 = ex
                .cast()
                .map_err(|error| backend_error("IDirect3D9Ex::cast", error.code().0))?;
            match set_display_format(&base, options.adapter_ordinal, &mut present).and_then(|()| {
                create_ex_device(
                    &ex,
                    options.adapter_ordinal,
                    desktop,
                    behavior,
                    &mut present,
                )
            }) {
                Ok(device) => (base, device),
                Err(_) => {
                    create_legacy_device(options.adapter_ordinal, desktop, behavior, &mut present)?
                }
            }
        } else {
            create_legacy_device(options.adapter_ordinal, desktop, behavior, &mut present)?
        };

        let mut reset_token = 0;
        let mut manager = None;
        // SAFETY: Both output pointers refer to initialized writable storage for this COM API.
        unsafe { DXVA2CreateDirect3DDeviceManager9(&raw mut reset_token, &raw mut manager) }
            .map_err(|error| backend_error("DXVA2CreateDirect3DDeviceManager9", error.code().0))?;
        let manager = manager.ok_or_else(|| null_interface("DXVA2CreateDirect3DDeviceManager9"))?;
        // SAFETY: `device` is a live D3D9 device and `reset_token` was returned by this manager.
        unsafe { manager.ResetDevice(&device, reset_token) }.map_err(|error| {
            backend_error("IDirect3DDeviceManager9::ResetDevice", error.code().0)
        })?;

        Self::from_device(d3d9, device, manager)
    }

    /// Wraps an existing D3D9 device and an already-reset DXVA2 device manager.
    ///
    /// # Errors
    ///
    /// Returns a backend error when decoder services or capability lists cannot be queried.
    pub fn from_device(
        d3d9: IDirect3D9,
        device: IDirect3DDevice9,
        manager: IDirect3DDeviceManager9,
    ) -> VideoResult<Self> {
        let decoder_service = get_video_service::<IDirectXVideoDecoderService>(
            &manager,
            &IDirectXVideoDecoderService::IID,
        )?;
        let acceleration_service = get_video_service::<IDirectXVideoAccelerationService>(
            &manager,
            &IDirectXVideoAccelerationService::IID,
        )?;
        let capabilities = discover_capabilities(&decoder_service)?;
        Ok(Self {
            inner: Rc::new(D3d9DeviceInner {
                d3d9,
                device,
                manager,
                decoder_service,
                acceleration_service,
                capabilities,
            }),
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> &[D3d9DecodeCapability] {
        &self.inner.capabilities
    }

    #[must_use]
    pub fn device(&self) -> &IDirect3DDevice9 {
        &self.inner.device
    }

    #[must_use]
    pub fn d3d9(&self) -> &IDirect3D9 {
        &self.inner.d3d9
    }

    #[must_use]
    pub fn device_manager(&self) -> &IDirect3DDeviceManager9 {
        &self.inner.manager
    }

    pub(crate) fn create_video_processor(
        &self,
        description: &DXVA2_VideoDesc,
        output_format: PixelFormat,
    ) -> VideoResult<windows::Win32::Media::MediaFoundation::IDirectXVideoProcessor> {
        let target_format = d3d_format(output_format).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d9Device::create_video_processor",
                "DXVA2 processor output supports NV12 and P010",
            )
        })?;
        let processor_service = get_video_service::<IDirectXVideoProcessorService>(
            &self.inner.manager,
            &IDirectXVideoProcessorService::IID,
        )?;
        let mut guid_count = 0;
        let mut guids = ptr::null_mut();
        // SAFETY: The initialized video descriptor is compatible with this DXVA2 service; both
        // output pointers address writable storage and the returned array uses COM task memory.
        let guid_result = unsafe {
            processor_service.GetVideoProcessorDeviceGuids(
                description,
                &raw mut guid_count,
                &raw mut guids,
            )
        };
        if let Err(error) = guid_result {
            if !guids.is_null() {
                // SAFETY: Any non-null GUID array returned on failure is still service-owned COM
                // task memory and can be released with the documented allocator.
                unsafe { CoTaskMemFree(Some(guids.cast())) };
            }
            if error.code().0 == 0x8000_4005_u32.cast_signed() {
                return Err(VideoError::new(
                    VideoErrorKind::Unsupported,
                    "D3d9Device::create_video_processor",
                    "the adapter exposes no DXVA2 video processor for this format and size",
                ));
            }
            return Err(backend_error(
                "GetVideoProcessorDeviceGuids",
                error.code().0,
            ));
        }
        if guids.is_null() || guid_count == 0 {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d9Device::create_video_processor",
                "the adapter exposes no DXVA2 video processors for this format and size",
            ));
        }
        // SAFETY: The service returned `guid_count` initialized GUIDs.
        let processor_guids = unsafe { std::slice::from_raw_parts(guids, guid_count as usize) };
        let mut selected = None;
        for guid in processor_guids {
            let mut format_count = 0;
            let mut formats = ptr::null_mut();
            // SAFETY: The GUID came from the service's list and all output pointers are valid.
            let result = unsafe {
                processor_service.GetVideoProcessorRenderTargets(
                    guid,
                    description,
                    &raw mut format_count,
                    &raw mut formats,
                )
            };
            if let Err(error) = result {
                // SAFETY: If the API wrote a non-null allocation before failing, it belongs to COM.
                if !formats.is_null() {
                    // SAFETY: DXVA2 allocated this optional result with the COM task allocator.
                    unsafe { CoTaskMemFree(Some(formats.cast())) };
                }
                // SAFETY: The service allocated the GUID list with the COM task allocator.
                unsafe { CoTaskMemFree(Some(guids.cast())) };
                return Err(backend_error(
                    "GetVideoProcessorRenderTargets",
                    error.code().0,
                ));
            }
            // SAFETY: A non-null service result points to `format_count` initialized formats.
            let supported = !formats.is_null()
                && unsafe { std::slice::from_raw_parts(formats, format_count as usize) }
                    .contains(&target_format);
            if !formats.is_null() {
                // SAFETY: The service allocated the render-target format array with COM memory.
                unsafe { CoTaskMemFree(Some(formats.cast())) };
            }
            if supported {
                selected = Some(*guid);
                break;
            }
        }
        // SAFETY: The service allocated the processor GUID list with COM memory.
        unsafe { CoTaskMemFree(Some(guids.cast())) };
        let guid = selected.ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d9Device::create_video_processor",
                "no DXVA2 processor supports the requested output format",
            )
        })?;
        // SAFETY: The processor GUID was advertised for this descriptor and its output format was
        // confirmed above; parameters remain valid during the synchronous COM call.
        unsafe {
            processor_service.CreateVideoProcessor(&raw const guid, description, target_format, 0)
        }
        .map_err(|error| backend_error("CreateVideoProcessor", error.code().0))
    }

    pub(crate) fn supported_video_processor_outputs(
        &self,
        input_format: PixelFormat,
    ) -> VideoResult<Vec<PixelFormat>> {
        // DXVA2 processor capability enumeration rejects tiny/unspecified video descriptors on
        // several drivers; use a conventional progressive 640x480, 30 Hz query profile.
        let size = FrameSize::new(640, 480)?;
        let description = video_description(size, input_format);
        let mut supported = Vec::new();
        for output_format in [PixelFormat::Nv12, PixelFormat::P010, PixelFormat::Bgra8] {
            match self.create_video_processor(&description, output_format) {
                Ok(processor) => {
                    drop(processor);
                    supported.push(output_format);
                }
                Err(error) if error.kind() == VideoErrorKind::Unsupported => {}
                Err(error) => return Err(error),
            }
        }
        Ok(supported)
    }

    /// Reports whether this device exposes the requested codec profile and bit depth.
    #[must_use]
    pub fn supports(&self, profile: CodecProfile, bit_depth: BitDepth) -> bool {
        self.inner
            .capabilities
            .iter()
            .any(|capability| capability.profile == profile && capability.bit_depth == bit_depth)
    }

    /// Returns driver configurations for a codec profile at the coded dimensions.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the driver rejects configuration enumeration.
    pub fn decoder_configs(
        &self,
        profile: CodecProfile,
        bit_depth: BitDepth,
        size: FrameSize,
    ) -> VideoResult<Vec<DXVA2_ConfigPictureDecode>> {
        let mut result = Vec::new();
        for capability in
            self.inner.capabilities.iter().filter(|capability| {
                capability.profile == profile && capability.bit_depth == bit_depth
            })
        {
            let description = video_description(size, capability.output_format);
            let mut count = 0;
            let mut configs = ptr::null_mut();
            // SAFETY: GUID and descriptor are validated, and output pointers are writable. The
            // DXVA2 service allocates the returned array with COM task memory, released below.
            unsafe {
                self.inner.decoder_service.GetDecoderConfigurations(
                    &raw const capability.decoder_guid,
                    &raw const description,
                    None,
                    &raw mut count,
                    &raw mut configs,
                )
            }
            .map_err(|error| backend_error("GetDecoderConfigurations", error.code().0))?;
            if !configs.is_null() {
                // SAFETY: The service returned `count` initialized configuration records.
                result.extend_from_slice(unsafe {
                    std::slice::from_raw_parts(configs, count as usize)
                });
                // SAFETY: DXVA2 API allocates this buffer with the COM task allocator.
                unsafe { CoTaskMemFree(Some(configs.cast())) };
            }
        }
        Ok(result)
    }

    pub(super) fn create_dxva2_decoder(
        &self,
        capability: &D3d9DecodeCapability,
        size: FrameSize,
        config: &DXVA2_ConfigPictureDecode,
        surfaces: &[IDirect3DSurface9],
    ) -> VideoResult<windows::Win32::Media::MediaFoundation::IDirectXVideoDecoder> {
        if surfaces.is_empty() {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d9Device::create_dxva2_decoder",
                "decoder requires at least one render target",
            ));
        }
        let render_targets: Vec<_> = surfaces.iter().cloned().map(Some).collect();
        let description = video_description(size, capability.output_format);
        // SAFETY: The advertised profile, matching coded-size descriptor and retained render
        // targets are valid for the lifetime of this synchronous decoder creation call.
        unsafe {
            self.inner.decoder_service.CreateVideoDecoder(
                &raw const capability.decoder_guid,
                &raw const description,
                &raw const *config,
                &render_targets,
            )
        }
        .map_err(|error| {
            backend_error(
                "IDirectXVideoDecoderService::CreateVideoDecoder",
                error.code().0,
            )
        })
    }

    /// Allocates a bounded fixed pool of decoder or video-processor surfaces.
    ///
    /// # Errors
    ///
    /// Returns invalid input or a backend error if the format, type or driver allocation fails.
    pub fn create_surface_pool(
        &self,
        size: FrameSize,
        format: PixelFormat,
        count: u32,
        surface_type: DXVA2_VideoRenderTargetType,
    ) -> VideoResult<Vec<IDirect3DSurface9>> {
        let format = d3d_format(format).ok_or_else(|| {
            VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d9Device::create_surface_pool",
                "DXVA2 D3D9 surfaces support NV12 and P010 in this crate",
            )
        })?;
        if count == 0 {
            return Err(VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d9Device::create_surface_pool",
                "surface pool size must be non-zero",
            ));
        }
        if surface_type != DXVA2_VideoDecoderRenderTarget
            && surface_type
                != windows::Win32::Media::MediaFoundation::DXVA2_VideoProcessorRenderTarget
        {
            return Err(VideoError::new(
                VideoErrorKind::Unsupported,
                "D3d9Device::create_surface_pool",
                "unknown DXVA2 surface type",
            ));
        }
        let mut surfaces = Vec::new();
        surfaces.try_reserve_exact(count as usize).map_err(|_| {
            VideoError::new(
                VideoErrorKind::InvalidInput,
                "D3d9Device::create_surface_pool",
                "surface pool allocation exceeds available memory",
            )
        })?;
        surfaces.resize(count as usize, None);
        // SAFETY: The service, dimensions, format, count and surface type were validated. The
        // output array has exactly `count` writable slots and remains alive through the call.
        unsafe {
            self.inner.acceleration_service.CreateSurface(
                size.width(),
                size.height(),
                count - 1,
                format,
                D3DPOOL_DEFAULT,
                0,
                surface_type,
                surfaces.as_mut_ptr(),
                None,
            )
        }
        .map_err(|error| {
            backend_error(
                "IDirectXVideoAccelerationService::CreateSurface",
                error.code().0,
            )
        })?;
        surfaces
            .into_iter()
            .map(|surface| surface.ok_or_else(|| null_interface("DXVA2 CreateSurface result")))
            .collect()
    }

    /// Inserts an event query after queued D3D9 work.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the driver cannot create or issue the query.
    pub fn signal_completion(&self) -> VideoResult<D3d9CompletionToken> {
        // SAFETY: Creating an event query does not require additional caller invariants.
        let query = unsafe { self.inner.device.CreateQuery(D3DQUERYTYPE_EVENT) }
            .map_err(|error| backend_error("IDirect3DDevice9::CreateQuery", error.code().0))?;
        // SAFETY: This is a live event query and END is the required issue command for it.
        unsafe { query.Issue(D3DISSUE_END) }
            .map_err(|error| backend_error("IDirect3DQuery9::Issue", error.code().0))?;
        Ok(D3d9CompletionToken { query })
    }
}

/// D3D9 event-query token for commands submitted before it.
pub struct D3d9CompletionToken {
    query: IDirect3DQuery9,
}

impl D3d9CompletionToken {
    /// Checks whether the event query has completed.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the query fails.
    pub fn is_ready(&self) -> VideoResult<bool> {
        let mut complete = 0_i32;
        // SAFETY: Event-query result is a BOOL (i32); pointer and byte size are valid.
        match unsafe {
            self.query
                .GetData((&raw mut complete).cast(), 4, D3DGETDATA_FLUSH)
        } {
            Ok(()) => Ok(complete != 0),
            Err(error) if error.code().0.cast_unsigned() == 0x8876_021c => Ok(false),
            Err(error) => Err(backend_error("IDirect3DQuery9::GetData", error.code().0)),
        }
    }

    /// Waits for completion until `timeout` expires.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the query fails.
    pub fn wait(&self, timeout: Duration) -> VideoResult<WaitStatus> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if self.is_ready()? {
                return Ok(WaitStatus::Complete);
            }
            if deadline.is_none_or(|end| Instant::now() >= end) {
                return Ok(WaitStatus::TimedOut);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn create_ex_device(
    d3d9: &IDirect3D9Ex,
    adapter: u32,
    window: HWND,
    behavior: u32,
    present: &mut D3DPRESENT_PARAMETERS,
) -> VideoResult<IDirect3DDevice9> {
    let mut device = None;
    // SAFETY: Output storage and presentation parameters are valid; a null full-screen mode means
    // windowed creation using the adapter's current display mode.
    unsafe {
        d3d9.CreateDeviceEx(
            adapter,
            D3DDEVTYPE_HAL,
            window,
            behavior,
            present,
            ptr::null_mut(),
            &raw mut device,
        )
    }
    .map_err(|error| backend_error("IDirect3D9Ex::CreateDeviceEx", error.code().0))?;
    let device = device.ok_or_else(|| null_interface("IDirect3D9Ex::CreateDeviceEx"))?;
    device
        .cast()
        .map_err(|error| backend_error("IDirect3DDevice9Ex::cast", error.code().0))
}

fn create_legacy_device(
    adapter: u32,
    window: HWND,
    behavior: u32,
    present: &mut D3DPRESENT_PARAMETERS,
) -> VideoResult<(IDirect3D9, IDirect3DDevice9)> {
    // SAFETY: D3D_SDK_VERSION is the SDK's compile-time version constant.
    let d3d9 = unsafe { Direct3DCreate9(D3D_SDK_VERSION) }
        .ok_or_else(|| null_interface("Direct3DCreate9"))?;
    set_display_format(&d3d9, adapter, present)?;
    let mut device = None;
    // SAFETY: Output storage and presentation parameters are valid for device creation.
    unsafe {
        d3d9.CreateDevice(
            adapter,
            D3DDEVTYPE_HAL,
            window,
            behavior,
            present,
            &raw mut device,
        )
    }
    .map_err(|error| backend_error("IDirect3D9::CreateDevice", error.code().0))?;
    let device = device.ok_or_else(|| null_interface("IDirect3D9::CreateDevice"))?;
    Ok((d3d9, device))
}

fn set_display_format(
    d3d9: &IDirect3D9,
    adapter: u32,
    present: &mut D3DPRESENT_PARAMETERS,
) -> VideoResult<()> {
    let mut mode = D3DDISPLAYMODE::default();
    // SAFETY: `mode` is writable output storage for the selected adapter's display mode.
    unsafe { d3d9.GetAdapterDisplayMode(adapter, &raw mut mode) }
        .map_err(|error| backend_error("IDirect3D9::GetAdapterDisplayMode", error.code().0))?;
    present.BackBufferFormat = mode.Format;
    Ok(())
}

fn get_video_service<T: Interface>(
    manager: &IDirect3DDeviceManager9,
    iid: &GUID,
) -> VideoResult<T> {
    // SAFETY: OpenDeviceHandle returns a live manager-owned handle, closed after GetVideoService.
    let handle = unsafe { manager.OpenDeviceHandle() }.map_err(|error| {
        backend_error("IDirect3DDeviceManager9::OpenDeviceHandle", error.code().0)
    })?;
    let mut service = ptr::null_mut();
    // SAFETY: IID is the requested COM interface IID and `service` is writable output storage.
    let result = unsafe { manager.GetVideoService(handle, iid, &raw mut service) }
        .map_err(|error| backend_error("IDirect3DDeviceManager9::GetVideoService", error.code().0));
    // SAFETY: This closes the handle opened immediately above, regardless of service success.
    let close_result = unsafe { manager.CloseDeviceHandle(handle) };
    result?;
    close_result.map_err(|error| {
        backend_error("IDirect3DDeviceManager9::CloseDeviceHandle", error.code().0)
    })?;
    if service.is_null() {
        return Err(null_interface("IDirect3DDeviceManager9::GetVideoService"));
    }
    // SAFETY: GetVideoService succeeded for `iid` and transferred one COM reference.
    Ok(unsafe { T::from_raw(service) })
}

fn discover_capabilities(
    service: &IDirectXVideoDecoderService,
) -> VideoResult<Vec<D3d9DecodeCapability>> {
    let mut count = 0;
    let mut guids = ptr::null_mut();
    // SAFETY: The service writes the count and a COM-allocated GUID array to valid output pointers.
    unsafe { service.GetDecoderDeviceGuids(&raw mut count, &raw mut guids) }
        .map_err(|error| backend_error("GetDecoderDeviceGuids", error.code().0))?;
    if guids.is_null() || count == 0 {
        if !guids.is_null() {
            // SAFETY: The DXVA2 service allocated this output using COM task memory.
            unsafe { CoTaskMemFree(Some(guids.cast())) };
        }
        return Ok(Vec::new());
    }
    // SAFETY: The service returned `count` initialized GUID elements.
    let profiles = unsafe { std::slice::from_raw_parts(guids, count as usize) }.to_vec();
    // SAFETY: The GUID list uses the COM task allocator per DXVA2 API contract.
    unsafe { CoTaskMemFree(Some(guids.cast())) };

    let mut capabilities = Vec::new();
    for decoder_guid in profiles {
        let target_formats = decoder_render_targets(service, &decoder_guid)?;
        for (profile, bit_depth, output_format) in codec_formats() {
            if !known_profiles(profile, bit_depth).contains(&decoder_guid) {
                continue;
            }
            let supported =
                d3d_format(output_format).is_some_and(|format| target_formats.contains(&format));
            if supported {
                capabilities.push(D3d9DecodeCapability {
                    profile,
                    bit_depth,
                    chroma_sampling: ChromaSampling::Yuv420,
                    output_format,
                    decoder_guid,
                });
            }
        }
    }
    Ok(capabilities)
}

fn decoder_render_targets(
    service: &IDirectXVideoDecoderService,
    decoder_guid: &GUID,
) -> VideoResult<Vec<D3DFORMAT>> {
    let mut count = 0;
    let mut formats = ptr::null_mut();
    // SAFETY: GUID comes from the driver's advertised decoder profiles and output pointers are
    // writable; the returned array is released with the COM allocator after copying.
    unsafe { service.GetDecoderRenderTargets(decoder_guid, &raw mut count, &raw mut formats) }
        .map_err(|error| backend_error("GetDecoderRenderTargets", error.code().0))?;
    if formats.is_null() || count == 0 {
        if !formats.is_null() {
            // SAFETY: The DXVA2 service allocated this array with COM task memory.
            unsafe { CoTaskMemFree(Some(formats.cast())) };
        }
        return Ok(Vec::new());
    }
    // SAFETY: The service returned `count` initialized D3DFORMAT entries.
    let result = unsafe { std::slice::from_raw_parts(formats, count as usize) }.to_vec();
    // SAFETY: DXVA2 allocates this returned array with the COM task allocator.
    unsafe { CoTaskMemFree(Some(formats.cast())) };
    Ok(result)
}

fn codec_formats() -> [(CodecProfile, BitDepth, PixelFormat); 7] {
    [
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
    ]
}

fn known_profiles(profile: CodecProfile, bit_depth: BitDepth) -> Vec<GUID> {
    match (profile, bit_depth) {
        (
            CodecProfile::AvcBaseline | CodecProfile::AvcMain | CodecProfile::AvcHigh,
            BitDepth::Eight,
        ) => vec![
            GUID::from_u128(0x1b81be68_a0c7_11d3_b984_00c04f2e73c5),
            GUID::from_u128(0x1b81be69_a0c7_11d3_b984_00c04f2e73c5),
            GUID::from_u128(0x604f8e68_4951_4c54_88fe_abd25c15b3d6),
        ],
        (CodecProfile::HevcMain, BitDepth::Eight) => {
            vec![GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0)]
        }
        (CodecProfile::HevcMain10, BitDepth::Ten) => {
            vec![GUID::from_u128(0x107af0e0_ef1a_4d19_aba8_67a163073d13)]
        }
        (CodecProfile::Av1Main, BitDepth::Eight | BitDepth::Ten) => {
            vec![GUID::from_u128(0xb8be4ccb_cf53_46ba_8d59_d6b8a6da5d2a)]
        }
        _ => Vec::new(),
    }
}

pub(crate) fn d3d_format(format: PixelFormat) -> Option<D3DFORMAT> {
    match format {
        PixelFormat::Nv12 => Some(D3DFORMAT(u32::from_le_bytes(*b"NV12"))),
        PixelFormat::P010 => Some(D3DFORMAT(u32::from_le_bytes(*b"P010"))),
        PixelFormat::Bgra8 => Some(D3DFMT_A8R8G8B8),
        PixelFormat::Rgb10A2 => None,
    }
}

fn video_description(size: FrameSize, format: PixelFormat) -> DXVA2_VideoDesc {
    DXVA2_VideoDesc {
        SampleWidth: size.width(),
        SampleHeight: size.height(),
        SampleFormat: DXVA2_ExtendedFormat {
            Anonymous: DXVA2_ExtendedFormat_0 {
                value: DXVA2_SampleProgressiveFrame.0.cast_unsigned(),
            },
        },
        Format: match d3d_format(format) {
            Some(format) => format,
            None => D3DFORMAT(0),
        },
        InputSampleFreq: DXVA2_Frequency {
            Numerator: 30,
            Denominator: 1,
        },
        OutputFrameFreq: DXVA2_Frequency {
            Numerator: 30,
            Denominator: 1,
        },
        ..DXVA2_VideoDesc::default()
    }
}

fn null_interface(operation: &'static str) -> VideoError {
    backend_error(operation, 0x8000_4003_u32.cast_signed())
}

fn backend_error(operation: &'static str, code: i32) -> VideoError {
    let kind = crate::error::backend_kind_from_hresult(code);
    VideoError::new(kind, operation, "DXVA2/D3D9 operation failed").with_backend_code(code)
}

#[cfg(test)]
mod tests {
    use super::{D3DFMT_A8R8G8B8, codec_formats, d3d_format, known_profiles};
    use crate::{BitDepth, CodecProfile, PixelFormat};

    #[test]
    fn maps_dxva2_profiles_to_ffmpeg_codec_profiles() {
        for (profile, bit_depth, _) in codec_formats() {
            assert_ne!(known_profiles(profile, bit_depth).len(), 0);
        }
        assert_eq!(
            known_profiles(CodecProfile::AvcHigh, BitDepth::Eight).len(),
            3
        );
        assert_eq!(
            known_profiles(CodecProfile::HevcMain10, BitDepth::Eight).len(),
            0
        );
        assert_eq!(
            known_profiles(CodecProfile::Av1Main, BitDepth::Eight),
            known_profiles(CodecProfile::Av1Main, BitDepth::Ten)
        );
    }

    #[test]
    fn maps_nv12_p010_and_bgra_dxva_surface_formats() {
        assert_eq!(
            d3d_format(PixelFormat::Nv12).map(|value| value.0),
            Some(u32::from_le_bytes(*b"NV12"))
        );
        assert_eq!(
            d3d_format(PixelFormat::P010).map(|value| value.0),
            Some(u32::from_le_bytes(*b"P010"))
        );
        assert_eq!(d3d_format(PixelFormat::Bgra8), Some(D3DFMT_A8R8G8B8));
    }
}

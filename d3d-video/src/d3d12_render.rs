//! D3D12 BGRA resource copy and HWND swap-chain presentation.

use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_RESOURCE_BARRIER, D3D12_RESOURCE_BARRIER_0,
    D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, D3D12_RESOURCE_BARRIER_FLAG_NONE,
    D3D12_RESOURCE_BARRIER_TYPE_TRANSITION, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE,
    D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_TRANSITION_BARRIER, ID3D12CommandAllocator,
    ID3D12CommandList, ID3D12GraphicsCommandList, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter1, IDXGIFactory2, IDXGIFactory4, IDXGISwapChain1,
    IDXGISwapChain3,
};
use windows::core::Interface;

use crate::d3d_render::{D3dRender, PresentMode, PresentResult, RenderParameters, Rotation};
use crate::d3d12_device::{D3d12CompletionToken, D3d12Device, D3d12Queue};
use crate::d3d12_scale::{D3d12ScaleFrame, D3d12Scaler};
use crate::{
    D3dBackend, D3dScale, FrameInfo, FrameSize, GpuFrame, PixelFormat, ScaleFilter, ScaleRequest,
    VideoError, VideoErrorKind, VideoResult, WaitStatus,
};

/// BGRA resource submitted for D3D12 presentation.
///
/// The caller must ensure the resource is in `COPY_SOURCE` state when rendering. The completion
/// token must cover the work that produced its contents; the frame retains the resource until that
/// token completes. This currently accepts BGRA only and performs no YUV conversion or scaling.
pub struct D3d12RenderFrame {
    resource: ID3D12Resource,
    info: FrameInfo,
    completion: D3d12CompletionToken,
    resource_state: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
}

impl D3d12RenderFrame {
    /// Wraps a single-sample BGRA texture and the fence token for its producer.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for incompatible metadata/device/resource or a backend error if the
    /// resource device cannot be queried.
    pub fn from_resource(
        resource: ID3D12Resource,
        info: FrameInfo,
        completion: &D3d12CompletionToken,
    ) -> VideoResult<Self> {
        Self::from_resource_with_state(resource, info, completion, D3D12_RESOURCE_STATE_COPY_SOURCE)
    }

    fn from_resource_with_state(
        resource: ID3D12Resource,
        info: FrameInfo,
        completion: &D3d12CompletionToken,
        resource_state: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
    ) -> VideoResult<Self> {
        if info.format != PixelFormat::Bgra8 {
            return Err(render_error(
                VideoErrorKind::Unsupported,
                "D3d12RenderFrame::from_resource",
                "D3D12 copy renderer currently accepts BGRA8 resources only",
            ));
        }
        // SAFETY: Resource is live and GetDesc returns its immutable creation descriptor.
        let desc = unsafe { resource.GetDesc() };
        let mut resource_device = None;
        // SAFETY: Resource is live and output storage receives its owning COM device.
        unsafe { resource.GetDevice(&raw mut resource_device) }
            .map_err(|error| backend_error("ID3D12Resource::GetDevice", error.code().0))?;
        if resource_device.as_ref() != Some(completion.device())
            || desc.Width != u64::from(info.size.width())
            || desc.Height != info.size.height()
            || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
            || desc.SampleDesc.Count != 1
            || desc.MipLevels != 1
            || desc.DepthOrArraySize != 1
            || !matches!(
                resource_state,
                D3D12_RESOURCE_STATE_COMMON | D3D12_RESOURCE_STATE_COPY_SOURCE
            )
        {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d12RenderFrame::from_resource",
                "resource must be a single-sample, single-mip BGRA texture matching frame size and completion device",
            ));
        }
        Ok(Self {
            resource,
            info,
            completion: completion.clone(),
            resource_state,
        })
    }

    /// Wraps a BGRA output from `D3d12Scaler` for direct queue presentation.
    ///
    /// The scaler returns its output to COMMON state, which permits transfer to the renderer's
    /// direct queue. Other frame states and formats are rejected.
    ///
    /// # Errors
    ///
    /// Returns invalid-state if the frame lacks a completion token or has not returned to COMMON,
    /// and invalid-input for a non-BGRA frame.
    pub fn from_scale_frame(frame: &crate::d3d12_scale::D3d12ScaleFrame) -> VideoResult<Self> {
        if frame.subresource() != 0 || frame.info().format != PixelFormat::Bgra8 {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d12RenderFrame::from_scale_frame",
                "frame must be a BGRA8 output on subresource zero",
            ));
        }
        let completion = frame.completion_token().ok_or_else(|| {
            render_error(
                VideoErrorKind::InvalidState,
                "D3d12RenderFrame::from_scale_frame",
                "scaled frame has no producer completion token",
            )
        })?;
        let texture = frame.texture()?;
        if texture.state() != Some(D3D12_RESOURCE_STATE_COMMON) {
            return Err(render_error(
                VideoErrorKind::InvalidState,
                "D3d12RenderFrame::from_scale_frame",
                "video-process output texture must be in COMMON state",
            ));
        }
        Self::from_resource_with_state(
            texture.resource().clone(),
            *frame.info(),
            &completion,
            D3D12_RESOURCE_STATE_COMMON,
        )
    }

    #[must_use]
    pub fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }
}

impl GpuFrame for D3d12RenderFrame {
    fn backend(&self) -> D3dBackend {
        D3dBackend::D3d12
    }

    fn info(&self) -> &FrameInfo {
        &self.info
    }

    fn is_ready(&self) -> VideoResult<bool> {
        self.completion.is_ready()
    }

    fn wait(&mut self, timeout: Duration) -> VideoResult<WaitStatus> {
        self.completion.wait(timeout)
    }
}

impl Drop for D3d12RenderFrame {
    fn drop(&mut self) {
        match self.completion.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) | Err(_) => {
                std::mem::forget(self.resource.clone());
            }
        }
    }
}

/// Copies a BGRA D3D12 texture into an HWND flip-model swap chain.
///
/// The resource must match the target size and be in `COPY_SOURCE` state. This is a presentation
/// primitive; YUV conversion and scaling are not implemented here.
pub struct D3d12Renderer {
    device: D3d12Device,
    queue: D3d12Queue,
    allocator: ID3D12CommandAllocator,
    command_list: ID3D12GraphicsCommandList,
    swap_chain: IDXGISwapChain3,
    target_size: FrameSize,
    failed: bool,
    scaler: Option<D3d12Scaler>,
}

impl D3d12Renderer {
    /// Creates a direct queue, copy command list, and BGRA swap chain for a non-null HWND.
    ///
    /// # Errors
    ///
    /// Returns invalid-input for a null HWND or a backend error when D3D12/DXGI setup fails.
    pub fn new(device: D3d12Device, hwnd: HWND, target_size: FrameSize) -> VideoResult<Self> {
        if hwnd.0.is_null() {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d12Renderer::new",
                "HWND must not be null",
            ));
        }
        let queue = device.create_queue(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        // SAFETY: Allocator type matches the direct command queue used by this renderer.
        let allocator = unsafe {
            device
                .device()
                .CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT)
        }
        .map_err(|error| backend_error("Create direct command allocator", error.code().0))?;
        // SAFETY: Command-list type and allocator match; copy operations need no PSO.
        let command_list = unsafe {
            device
                .device()
                .CreateCommandList::<_, _, ID3D12GraphicsCommandList>(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None::<&windows::Win32::Graphics::Direct3D12::ID3D12PipelineState>,
                )
        }
        .map_err(|error| backend_error("Create direct command list", error.code().0))?;
        // SAFETY: Newly created command lists begin open and must be closed before first reset.
        unsafe { command_list.Close() }
            .map_err(|error| backend_error("Close initial direct command list", error.code().0))?;

        // D3D12 devices do not expose IDXGIDevice. Resolve the owning adapter through its LUID.
        // SAFETY: The process factory is returned as an owned COM interface.
        let factory4: IDXGIFactory4 =
            unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
                .map_err(|error| backend_error("CreateDXGIFactory2", error.code().0))?;
        // SAFETY: The device's adapter LUID identifies the adapter that created this D3D12 device.
        let adapter: IDXGIAdapter1 = unsafe {
            factory4.EnumAdapterByLuid::<IDXGIAdapter1>(device.device().GetAdapterLuid())
        }
        .map_err(|error| backend_error("IDXGIFactory4::EnumAdapterByLuid", error.code().0))?;
        // SAFETY: Adapter is live and returns an owned factory interface.
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| backend_error("IDXGIAdapter1::GetParent", error.code().0))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: target_size.width(),
            Height: target_size.height(),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
        };
        // SAFETY: The direct queue, HWND and initialized swap-chain descriptor are live.
        let swap_chain: IDXGISwapChain1 = unsafe {
            factory.CreateSwapChainForHwnd(queue.queue(), hwnd, &raw const desc, None, None)
        }
        .map_err(|error| backend_error("IDXGIFactory2::CreateSwapChainForHwnd", error.code().0))?;
        let swap_chain: IDXGISwapChain3 = swap_chain.cast().map_err(|error| {
            backend_error("IDXGISwapChain1::cast IDXGISwapChain3", error.code().0)
        })?;
        Ok(Self {
            device,
            queue,
            allocator,
            command_list,
            swap_chain,
            target_size,
            failed: false,
            scaler: None,
        })
    }

    /// Scales/converts a D3D12 decode or processing frame and presents it to the HWND.
    ///
    /// NV12/P010 and BGRA input use the D3D12 video processor; output is BGRA before the swap
    /// chain copy. Rotation uses the adapter's D3D12 video-processor orientation support.
    ///
    /// # Errors
    ///
    /// Returns unsupported, invalid-input, backend, timeout, or device-lost errors when processing
    /// or presentation fails.
    pub fn render_scaled_frame(
        &mut self,
        frame: &D3d12ScaleFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        if crate::d3d_render::can_direct_present(
            frame.info(),
            frame.sample_aspect_ratio(),
            self.target_size,
            parameters,
        ) {
            let render_frame = D3d12RenderFrame::from_scale_frame(frame)?;
            let copy_parameters = RenderParameters {
                destination: None,
                aspect_mode: crate::AspectMode::Stretch,
                rotation: Rotation::Degrees0,
                background: parameters.background,
                present_mode: parameters.present_mode,
            };
            return self.render_frame(&render_frame, &copy_parameters);
        }
        if self.scaler.is_none() {
            self.scaler = Some(D3d12Scaler::create(&self.device)?);
        }
        let scaling_engine = self.scaler.as_mut().ok_or_else(|| {
            render_error(
                VideoErrorKind::InvalidState,
                "D3d12Renderer::render_scaled_frame",
                "video scaler initialization did not complete",
            )
        })?;
        scaling_engine.set_background_color(parameters.background);
        let request = ScaleRequest {
            output_size: self.target_size,
            output_format: PixelFormat::Bgra8,
            source_rect: None,
            destination_rect: parameters.destination,
            aspect_mode: parameters.aspect_mode,
            rotation: parameters.rotation,
            filter: ScaleFilter::DriverDefault,
            output_color: Some(crate::color::bgra8_output_color()),
        };
        let scaled = scaling_engine.scale(frame, &request)?;
        let render_frame = D3d12RenderFrame::from_scale_frame(&scaled)?;
        let copy_parameters = RenderParameters {
            destination: None,
            aspect_mode: crate::AspectMode::Stretch,
            rotation: Rotation::Degrees0,
            background: parameters.background,
            present_mode: parameters.present_mode,
        };
        self.render_frame(&render_frame, &copy_parameters)
    }

    /// Presents a caller-provided BGRA D3D12 resource without running a video-process pass.
    ///
    /// # Errors
    ///
    /// Returns unsupported or backend errors when the resource does not match the target or the
    /// queue cannot submit/present it.
    pub fn render_bgra_frame(
        &mut self,
        frame: &D3d12RenderFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        self.render_frame(frame, parameters)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "command recording, fence retention and present must remain ordered as one submission"
    )]
    fn render_frame(
        &mut self,
        frame: &D3d12RenderFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        if self.failed {
            return Err(render_error(
                VideoErrorKind::InvalidState,
                "D3d12Renderer::render",
                "a previous submission failure requires recreating the renderer",
            ));
        }
        if frame.info.size != self.target_size
            || parameters.destination.is_some()
            || parameters.rotation != Rotation::Degrees0
        {
            return Err(render_error(
                VideoErrorKind::Unsupported,
                "D3d12Renderer::render",
                "copy presentation requires matching frame/target sizes, no destination rectangle, and zero rotation",
            ));
        }
        let mut resource_device = None;
        // SAFETY: Source resource and output slot are live for this synchronous COM query.
        unsafe { frame.resource.GetDevice(&raw mut resource_device) }
            .map_err(|error| backend_error("D3D12 source GetDevice", error.code().0))?;
        if resource_device.as_ref() != Some(self.device.device()) {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d12Renderer::render",
                "source resource and renderer belong to different D3D12 devices",
            ));
        }
        frame
            .completion
            .wait(Duration::from_secs(30))
            .and_then(|status| {
                if status == WaitStatus::Complete {
                    Ok(())
                } else {
                    Err(render_error(
                        VideoErrorKind::Timeout,
                        "D3d12Renderer::render",
                        "producer completion timed out",
                    ))
                }
            })?;
        if let Err(error) = wait_idle(&mut self.queue) {
            self.failed = true;
            return Err(error);
        }
        // SAFETY: Previous direct-queue work is complete, so allocator/list reset is safe.
        unsafe { self.allocator.Reset() }
            .map_err(|error| backend_error("ID3D12CommandAllocator::Reset", error.code().0))?;
        // SAFETY: The same allocator was reset and no PSO is needed for resource copies.
        unsafe { self.command_list.Reset(&self.allocator, None) }
            .map_err(|error| backend_error("ID3D12GraphicsCommandList::Reset", error.code().0))?;
        // SAFETY: Swap-chain current index identifies a live BGRA resource on this device.
        let backbuffer: ID3D12Resource = unsafe {
            self.swap_chain
                .GetBuffer(self.swap_chain.GetCurrentBackBufferIndex())
        }
        .map_err(|error| backend_error("IDXGISwapChain1::GetBuffer", error.code().0))?;
        let mut before = transition_barrier(
            &backbuffer,
            D3D12_RESOURCE_STATE_PRESENT,
            D3D12_RESOURCE_STATE_COPY_DEST,
        );
        // SAFETY: Backbuffer is in PRESENT state after the previous Present, and the barrier
        // references a live resource with a stable descriptor for synchronous recording.
        unsafe {
            self.command_list
                .ResourceBarrier(std::slice::from_ref(&before));
        };
        // SAFETY: ResourceBarrier synchronously copied the descriptor; release its owned COM ref.
        unsafe { std::mem::ManuallyDrop::drop(&mut (*before.Anonymous.Transition).pResource) };
        if frame.resource_state != D3D12_RESOURCE_STATE_COPY_SOURCE {
            record_transition(
                &self.command_list,
                &frame.resource,
                frame.resource_state,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
        }
        // SAFETY: Both textures are same-sized BGRA resources; caller guarantees source is
        // COPY_SOURCE and frame completion has been waited before this command is recorded.
        unsafe { self.command_list.CopyResource(&backbuffer, &frame.resource) };
        if frame.resource_state != D3D12_RESOURCE_STATE_COPY_SOURCE {
            record_transition(
                &self.command_list,
                &frame.resource,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                frame.resource_state,
            );
        }
        let mut after = transition_barrier(
            &backbuffer,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PRESENT,
        );
        // SAFETY: This restores the swap-chain resource to PRESENT before queue execution.
        unsafe {
            self.command_list
                .ResourceBarrier(std::slice::from_ref(&after));
        };
        // SAFETY: ResourceBarrier synchronously copied the descriptor; release its owned COM ref.
        unsafe { std::mem::ManuallyDrop::drop(&mut (*after.Anonymous.Transition).pResource) };
        // SAFETY: Command list was reset above and all commands have valid live resources.
        unsafe { self.command_list.Close() }
            .map_err(|error| backend_error("ID3D12GraphicsCommandList::Close", error.code().0))?;
        let list: ID3D12CommandList = self
            .command_list
            .cast()
            .map_err(|error| backend_error("ID3D12GraphicsCommandList::cast", error.code().0))?;
        let token = match self.queue.execute_with_completion(&[list]) {
            Ok(token) => token,
            Err(submission) => {
                if submission.submitted {
                    std::mem::forget(frame.resource.clone());
                    self.failed = true;
                }
                return Err(submission.error);
            }
        };
        match token.wait(Duration::from_secs(30)) {
            Ok(WaitStatus::Complete) => {}
            Ok(WaitStatus::TimedOut) => {
                std::mem::forget(frame.resource.clone());
                self.failed = true;
                return Err(render_error(
                    VideoErrorKind::Timeout,
                    "D3d12Renderer::render",
                    "copy submission did not complete before the timeout",
                ));
            }
            Err(error) => {
                std::mem::forget(frame.resource.clone());
                self.failed = true;
                return Err(error);
            }
        }
        let sync_interval = match parameters.present_mode {
            PresentMode::VSync => 1,
            PresentMode::Immediate => 0,
        };
        // SAFETY: Swap chain is live and interval is a valid DXGI present setting.
        let result = unsafe { self.swap_chain.Present(sync_interval, DXGI_PRESENT(0)) };
        if result.0 == 0x087a_0001 {
            Ok(PresentResult::Occluded)
        } else if result.is_ok() {
            Ok(PresentResult::Presented)
        } else {
            Err(backend_error("IDXGISwapChain1::Present", result.0))
        }
    }
}

impl D3dRender for D3d12Renderer {
    type Frame = D3d12ScaleFrame;

    fn target_size(&self) -> FrameSize {
        self.target_size
    }

    fn resize(&mut self, size: FrameSize) -> VideoResult<()> {
        if size == self.target_size {
            return Ok(());
        }
        wait_idle(&mut self.queue)?;
        // SAFETY: Queue is idle and no backbuffer references are stored between calls.
        unsafe {
            self.swap_chain.ResizeBuffers(
                0,
                size.width(),
                size.height(),
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        }
        .map_err(|error| backend_error("IDXGISwapChain1::ResizeBuffers", error.code().0))?;
        self.target_size = size;
        Ok(())
    }

    fn render(
        &mut self,
        frame: &Self::Frame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        self.render_scaled_frame(frame, parameters)
    }
}

fn transition_barrier(
    resource: &ID3D12Resource,
    before: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
    after: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: std::mem::ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn record_transition(
    command_list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
    after: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATES,
) {
    let mut barrier = transition_barrier(resource, before, after);
    // SAFETY: Caller ensures recorded state matches the resource and descriptor remains live.
    unsafe { command_list.ResourceBarrier(std::slice::from_ref(&barrier)) };
    // SAFETY: Command recording copied the descriptor; release the helper's COM reference.
    unsafe { std::mem::ManuallyDrop::drop(&mut (*barrier.Anonymous.Transition).pResource) };
}

fn wait_idle(queue: &mut D3d12Queue) -> VideoResult<()> {
    if queue.wait_idle(Duration::from_secs(30))? == WaitStatus::TimedOut {
        return Err(render_error(
            VideoErrorKind::Timeout,
            "D3d12Renderer",
            "GPU queue did not become idle before the timeout",
        ));
    }
    Ok(())
}

fn backend_error(context: &'static str, hresult: i32) -> VideoError {
    VideoError::new(
        crate::error::backend_kind_from_hresult(hresult),
        context,
        format!("HRESULT 0x{:08X}", hresult.cast_unsigned()),
    )
}

fn render_error(kind: VideoErrorKind, context: &'static str, message: &'static str) -> VideoError {
    VideoError::new(kind, context, message)
}

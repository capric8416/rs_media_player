//! DXVA2 surface stretch and additional-swap-chain presentation to an HWND.

use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D9::{
    D3DBACKBUFFER_TYPE_MONO, D3DDEVICE_CREATION_PARAMETERS, D3DDISPLAYMODE, D3DMULTISAMPLE_NONE,
    D3DPRESENT_INTERVAL_DEFAULT, D3DPRESENT_INTERVAL_IMMEDIATE, D3DPRESENT_PARAMETERS,
    D3DPRESENTFLAG_VIDEO, D3DSURFACE_DESC, D3DSWAPEFFECT_DISCARD, D3DTEXF_LINEAR,
    IDirect3DSwapChain9,
};

use crate::d3d_render::{D3dRender, PresentMode, PresentResult, RenderParameters, Rotation};
use crate::d3d9_decode::D3d9DecodedFrame;
use crate::d3d9_device::D3d9Device;
use crate::d3d9_scale::D3d9Scaler;
use crate::{
    D3dScale, FrameSize, PixelFormat, ScaleFilter, ScaleRequest, VideoError, VideoErrorKind,
    VideoResult, WaitStatus,
};

/// Presents DXVA2 decoder or processor surfaces through an HWND-bound D3D9 swap chain.
///
/// NV12/P010 frames are converted to BGRA through the adapter's DXVA2 video processor before the
/// swap-chain blit. The adapter must advertise the requested processor output format.
pub struct D3d9Renderer {
    device: D3d9Device,
    swap_chain: IDirect3DSwapChain9,
    hwnd: HWND,
    target_size: FrameSize,
    present_mode: PresentMode,
    scaler: Option<D3d9Scaler>,
}

impl D3d9Renderer {
    /// Wraps a DXVA2 device, queries its backbuffer dimensions and creates a swap chain for `hwnd`.
    ///
    /// # Errors
    ///
    /// Returns invalid-input or backend errors if the window or swap chain is invalid.
    pub fn new(device: D3d9Device, hwnd: HWND) -> VideoResult<Self> {
        if hwnd.0.is_null() {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d9Renderer::new",
                "HWND must not be null",
            ));
        }
        // SAFETY: Device is live and index/type select its mono backbuffer.
        let backbuffer = unsafe { device.device().GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO) }
            .map_err(|error| backend_error("IDirect3DDevice9::GetBackBuffer", error.code().0))?;
        let mut desc = D3DSURFACE_DESC::default();
        // SAFETY: Backbuffer is live and `desc` is writable output storage.
        unsafe { backbuffer.GetDesc(&raw mut desc) }
            .map_err(|error| backend_error("IDirect3DSurface9::GetDesc", error.code().0))?;
        let target_size = FrameSize::new(desc.Width, desc.Height)?;
        let present_mode = PresentMode::VSync;
        let swap_chain = create_swap_chain(&device, hwnd, target_size, present_mode)?;
        Ok(Self {
            device,
            swap_chain,
            hwnd,
            target_size,
            present_mode,
            scaler: None,
        })
    }

    fn wait_idle(&self) -> VideoResult<()> {
        match self
            .device
            .signal_completion()?
            .wait(Duration::from_secs(30))?
        {
            WaitStatus::Complete => Ok(()),
            WaitStatus::TimedOut => Err(render_error(
                VideoErrorKind::Timeout,
                "D3d9Renderer::wait_idle",
                "GPU work did not complete before the 30-second timeout",
            )),
        }
    }

    fn set_present_mode(&mut self, mode: PresentMode) -> VideoResult<()> {
        if mode == self.present_mode {
            return Ok(());
        }
        self.wait_idle()?;
        let swap_chain = create_swap_chain(&self.device, self.hwnd, self.target_size, mode)?;
        self.swap_chain = swap_chain;
        self.present_mode = mode;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "input conversion, destination blit, present and queue synchronization form one rendering transaction"
    )]
    fn render_frame(
        &mut self,
        frame: &D3d9DecodedFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        if frame.completion.wait(Duration::from_secs(30))? == WaitStatus::TimedOut {
            return Err(render_error(
                VideoErrorKind::Timeout,
                "D3d9Renderer::render",
                "decoder surface did not become ready before the presentation timeout",
            ));
        }
        if parameters.rotation != Rotation::Degrees0 {
            return Err(render_error(
                VideoErrorKind::Unsupported,
                "D3d9Renderer::render",
                "D3D9 StretchRect renderer supports zero-degree rotation only",
            ));
        }
        self.set_present_mode(parameters.present_mode)?;
        let converted = if matches!(frame.info.format, PixelFormat::Nv12 | PixelFormat::P010) {
            if self.scaler.is_none() {
                self.scaler = Some(D3d9Scaler::new(self.device.clone())?);
            }
            let scaler = self.scaler.as_mut().ok_or_else(|| {
                render_error(
                    VideoErrorKind::InvalidState,
                    "D3d9Renderer::render",
                    "DXVA2 scaler initialization did not complete",
                )
            })?;
            Some(scaler.scale(
                frame,
                &ScaleRequest {
                    output_size: self.target_size,
                    output_format: PixelFormat::Bgra8,
                    source_rect: None,
                    destination_rect: parameters.destination,
                    aspect_mode: parameters.aspect_mode,
                    rotation: Rotation::Degrees0,
                    filter: ScaleFilter::DriverDefault,
                    output_color: Some(crate::color::bgra8_output_color()),
                },
            )?)
        } else {
            None
        };
        let source_frame = converted.as_ref().unwrap_or(frame);
        let render_surface = frame.surface.clone();
        let render_lease = frame
            .surface_lease
            .as_ref()
            .map(crate::frame::SurfaceLease::clone_for_pending_work);
        let (source, destination) = if converted.is_some() {
            let (_, visible_destination) = crate::d3d12_scale::compute_rotated_rectangles(
                frame.info.size,
                self.target_size,
                None,
                parameters.destination,
                parameters.aspect_mode,
                frame.sample_aspect_ratio(),
                Rotation::Degrees0,
            )?;
            // The scaler has already rendered the video into this rectangle. Copy only that area
            // so the backbuffer's requested background remains visible in uncovered regions.
            (visible_destination, visible_destination)
        } else {
            crate::d3d12_scale::compute_rotated_rectangles(
                frame.info.size,
                self.target_size,
                None,
                parameters.destination,
                parameters.aspect_mode,
                frame.sample_aspect_ratio(),
                Rotation::Degrees0,
            )?
        };
        // SAFETY: Swap chain is live and index/type select its current mono backbuffer.
        let backbuffer = unsafe { self.swap_chain.GetBackBuffer(0, D3DBACKBUFFER_TYPE_MONO) }
            .map_err(|error| backend_error("IDirect3DSwapChain9::GetBackBuffer", error.code().0))?;
        let background = pack_argb(
            parameters.background.alpha,
            parameters.background.red,
            parameters.background.green,
            parameters.background.blue,
        );
        // SAFETY: Backbuffer is a live renderable surface; null rectangle selects the full target.
        unsafe {
            self.device
                .device()
                .ColorFill(&backbuffer, std::ptr::null(), background)
        }
        .map_err(|error| backend_error("IDirect3DDevice9::ColorFill", error.code().0))?;
        // SAFETY: Source and destination surfaces belong to this device; rectangles are validated.
        let stretch_result = unsafe {
            self.device.device().StretchRect(
                &source_frame.surface,
                &raw const source,
                &backbuffer,
                &raw const destination,
                D3DTEXF_LINEAR,
            )
        };
        if let Err(error) = stretch_result {
            if let Err(wait_error) = self.wait_idle() {
                std::mem::forget(render_surface);
                if let Some(lease) = render_lease {
                    std::mem::forget(lease);
                }
                if let Some(converted) = converted {
                    std::mem::forget(converted);
                }
                return Err(wait_error);
            }
            return Err(backend_error(
                "IDirect3DDevice9::StretchRect",
                error.code().0,
            ));
        }
        if let Err(error) = self.wait_idle() {
            std::mem::forget(render_surface);
            if let Some(lease) = render_lease {
                std::mem::forget(lease);
            }
            if let Some(converted) = converted {
                std::mem::forget(converted);
            }
            return Err(error);
        }
        // SAFETY: Swap chain is live and the caller-supplied HWND is its associated target.
        unsafe {
            self.swap_chain.Present(
                std::ptr::null(),
                std::ptr::null(),
                self.hwnd,
                std::ptr::null(),
                0,
            )
        }
        .map_err(|error| backend_error("IDirect3DSwapChain9::Present", error.code().0))?;
        Ok(PresentResult::Presented)
    }
}

impl D3dRender for D3d9Renderer {
    type Frame = D3d9DecodedFrame;

    fn target_size(&self) -> FrameSize {
        self.target_size
    }

    fn resize(&mut self, size: FrameSize) -> VideoResult<()> {
        if size == self.target_size {
            return Ok(());
        }
        self.wait_idle()?;
        let swap_chain = create_swap_chain(&self.device, self.hwnd, size, self.present_mode)?;
        self.swap_chain = swap_chain;
        self.target_size = size;
        Ok(())
    }

    fn render(
        &mut self,
        frame: &Self::Frame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        self.render_frame(frame, parameters)
    }
}

fn create_swap_chain(
    device: &D3d9Device,
    hwnd: HWND,
    size: FrameSize,
    present_mode: PresentMode,
) -> VideoResult<IDirect3DSwapChain9> {
    let mut creation = D3DDEVICE_CREATION_PARAMETERS::default();
    // SAFETY: Device is live and creation parameters are writable output storage.
    unsafe { device.device().GetCreationParameters(&raw mut creation) }.map_err(|error| {
        backend_error("IDirect3DDevice9::GetCreationParameters", error.code().0)
    })?;
    let mut display_mode = D3DDISPLAYMODE::default();
    // SAFETY: Adapter ordinal came from the live device and display mode is writable output.
    unsafe {
        device
            .d3d9()
            .GetAdapterDisplayMode(creation.AdapterOrdinal, &raw mut display_mode)
    }
    .map_err(|error| backend_error("IDirect3D9::GetAdapterDisplayMode", error.code().0))?;
    let interval = match present_mode {
        PresentMode::VSync => D3DPRESENT_INTERVAL_DEFAULT,
        PresentMode::Immediate => D3DPRESENT_INTERVAL_IMMEDIATE,
    };
    let mut parameters = D3DPRESENT_PARAMETERS {
        BackBufferWidth: size.width(),
        BackBufferHeight: size.height(),
        BackBufferFormat: display_mode.Format,
        BackBufferCount: 1,
        MultiSampleType: D3DMULTISAMPLE_NONE,
        MultiSampleQuality: 0,
        SwapEffect: D3DSWAPEFFECT_DISCARD,
        hDeviceWindow: hwnd,
        Windowed: true.into(),
        EnableAutoDepthStencil: false.into(),
        AutoDepthStencilFormat: windows::Win32::Graphics::Direct3D9::D3DFORMAT(0),
        Flags: D3DPRESENTFLAG_VIDEO,
        FullScreen_RefreshRateInHz: 0,
        PresentationInterval: interval.cast_unsigned(),
    };
    let mut swap_chain = None;
    // SAFETY: Parameters match this device and the target HWND; output storage remains writable.
    unsafe {
        device
            .device()
            .CreateAdditionalSwapChain(&raw mut parameters, &raw mut swap_chain)
    }
    .map_err(|error| {
        backend_error(
            "IDirect3DDevice9::CreateAdditionalSwapChain",
            error.code().0,
        )
    })?;
    swap_chain.ok_or_else(|| {
        backend_error(
            "IDirect3DDevice9::CreateAdditionalSwapChain",
            0x8000_4003_u32.cast_signed(),
        )
    })
}

fn pack_argb(alpha: f32, red: f32, green: f32, blue: f32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped and rounded to the inclusive 0..=255 range"
    )]
    fn channel(value: f32) -> u32 {
        u32::from((value.clamp(0.0, 1.0) * 255.0).round() as u8)
    }
    (channel(alpha) << 24) | (channel(red) << 16) | (channel(green) << 8) | channel(blue)
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

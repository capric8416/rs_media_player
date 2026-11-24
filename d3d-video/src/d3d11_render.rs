//! D3D11 video-processor presentation to an HWND-backed swap chain.

use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_VIDEO_COLOR, D3D11_VIDEO_COLOR_0,
    D3D11_VIDEO_COLOR_RGBA, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CAPS,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_ROTATION, D3D11_VIDEO_PROCESSOR_ROTATION_90,
    D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270,
    D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Texture2D, ID3D11VideoContext1,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorOutputView,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIFactory2,
    IDXGISwapChain1,
};
use windows::core::Interface;

use crate::d3d_render::{D3dRender, PresentMode, PresentResult, RenderParameters, Rotation};
use crate::d3d11_decode::D3d11DecodedFrame;
use crate::d3d11_device::{D3d11Device, dxgi_format};
use crate::{FrameSize, PixelFormat, VideoError, VideoErrorKind, VideoResult, WaitStatus};

/// HWND renderer using D3D11's video processor for YUV-to-RGB conversion and presentation.
pub struct D3d11Renderer {
    device: D3d11Device,
    swap_chain: IDXGISwapChain1,
    target_size: FrameSize,
}

impl D3d11Renderer {
    fn wait_idle(&self) -> VideoResult<()> {
        let completion = self.device.signal_completion()?;
        match completion.wait(std::time::Duration::from_secs(30))? {
            WaitStatus::Complete => Ok(()),
            WaitStatus::TimedOut => Err(render_error(
                VideoErrorKind::Timeout,
                "D3d11Renderer::wait_idle",
                "GPU work did not complete before the 30-second timeout",
            )),
        }
    }

    fn wait_for_render(&self, frame: &D3d11DecodedFrame) -> VideoResult<()> {
        let retained_texture = frame.texture.clone();
        let retained_lease = frame
            .surface_lease
            .as_ref()
            .map(crate::frame::SurfaceLease::clone_for_pending_work);
        if let Err(error) = self.wait_idle() {
            std::mem::forget(retained_texture);
            if let Some(lease) = retained_lease {
                std::mem::forget(lease);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Creates a flip-model BGRA swap chain for a non-null window handle.
    ///
    /// # Errors
    ///
    /// Returns invalid-input or backend errors when the HWND or swap chain is unavailable.
    pub fn new(device: D3d11Device, hwnd: HWND, target_size: FrameSize) -> VideoResult<Self> {
        if hwnd.0.is_null() {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d11Renderer::new",
                "HWND must not be null",
            ));
        }
        let dxgi_device: IDXGIDevice = device
            .device()
            .cast()
            .map_err(|error| backend_error("ID3D11Device::cast IDXGIDevice", error.code().0))?;
        // SAFETY: The DXGI device is live and GetAdapter returns an owned COM interface.
        let adapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| backend_error("IDXGIDevice::GetAdapter", error.code().0))?;
        // SAFETY: The adapter is live and the parent factory is retained by the returned COM object.
        let factory: IDXGIFactory2 = unsafe { adapter.GetParent() }
            .map_err(|error| backend_error("IDXGIAdapter::GetParent", error.code().0))?;
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
        // SAFETY: Device, HWND and descriptor are valid for synchronous swap-chain creation.
        let swap_chain = unsafe {
            factory.CreateSwapChainForHwnd(device.device(), hwnd, &raw const desc, None, None)
        }
        .map_err(|error| backend_error("IDXGIFactory2::CreateSwapChainForHwnd", error.code().0))?;
        Ok(Self {
            device,
            swap_chain,
            target_size,
        })
    }

    fn copy_bgra_frame(
        &self,
        frame: &D3d11DecodedFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: The frame retains a live texture and the descriptor is writable output storage.
        unsafe { frame.texture.GetDesc(&raw mut source_desc) };
        if source_desc.Width != self.target_size.width()
            || source_desc.Height != self.target_size.height()
            || source_desc.MipLevels != 1
            || source_desc.ArraySize != 1
            || source_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || source_desc.SampleDesc.Count != 1
        {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d11Renderer::copy_bgra_frame",
                "direct presentation requires a target-sized single-sample BGRA texture",
            ));
        }
        // SAFETY: The swap chain is live and buffer zero is its current BGRA texture.
        let backbuffer: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0) }
            .map_err(|error| backend_error("IDXGISwapChain1::GetBuffer", error.code().0))?;
        // SAFETY: Source and destination are same-sized, single-sample BGRA textures owned by the
        // same D3D11 device. CopyResource records a GPU copy and retains no borrowed pointers.
        unsafe {
            self.device
                .immediate_context()
                .CopyResource(&backbuffer, &frame.texture);
        }
        let sync_interval = match parameters.present_mode {
            PresentMode::VSync => 1,
            PresentMode::Immediate => 0,
        };
        // SAFETY: The swap chain is live and the sync interval is a valid DXGI present setting.
        let result = unsafe { self.swap_chain.Present(sync_interval, DXGI_PRESENT(0)) };
        self.wait_for_render(frame)?;
        if result.0 == 0x087a_0001 {
            Ok(PresentResult::Occluded)
        } else if result.is_ok() {
            Ok(PresentResult::Presented)
        } else {
            Err(backend_error("IDXGISwapChain1::Present", result.0))
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the D3D11 processor setup and synchronous blit form one ordered transaction"
    )]
    fn render_frame(
        &self,
        frame: &D3d11DecodedFrame,
        parameters: &RenderParameters,
    ) -> VideoResult<PresentResult> {
        if crate::d3d_render::can_direct_present(
            &frame.info,
            frame.sample_aspect_ratio(),
            self.target_size,
            parameters,
        ) {
            return self.copy_bgra_frame(frame, parameters);
        }
        if !matches!(
            frame.info.format,
            PixelFormat::Nv12 | PixelFormat::P010 | PixelFormat::Bgra8
        ) {
            return Err(render_error(
                VideoErrorKind::Unsupported,
                "D3d11Renderer::render",
                "D3D11 video processor input requires NV12, P010 or BGRA8",
            ));
        }
        if matches!(
            frame.info.color.transfer,
            crate::TransferCharacteristic::Smpte2084 | crate::TransferCharacteristic::AribStdB67
        ) {
            return Err(render_error(
                VideoErrorKind::Unsupported,
                "D3d11Renderer::render",
                "HDR-to-SDR tone mapping is not implemented for BGRA presentation",
            ));
        }
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: Frame retains a live texture and the descriptor is writable output storage.
        unsafe { frame.texture.GetDesc(&raw mut texture_desc) };
        if texture_desc.Format != dxgi_format(frame.info.format) {
            return Err(render_error(
                VideoErrorKind::InvalidInput,
                "D3d11Renderer::render",
                "frame format metadata does not match its texture",
            ));
        }
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            InputWidth: texture_desc.Width,
            InputHeight: texture_desc.Height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 30,
                Denominator: 1,
            },
            OutputWidth: self.target_size.width(),
            OutputHeight: self.target_size.height(),
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: Initialized content descriptor remains valid for the duration of the call.
        let enumerator = unsafe {
            self.device
                .video_device()
                .CreateVideoProcessorEnumerator(&raw const content)
        }
        .map_err(|error| backend_error("CreateVideoProcessorEnumerator", error.code().0))?;
        let rotation = d3d11_rotation(parameters.rotation);
        if parameters.rotation != Rotation::Degrees0 {
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            // SAFETY: The enumerator is live and `caps` is writable output storage.
            unsafe { enumerator.GetVideoProcessorCaps(&raw mut caps) }
                .map_err(|error| backend_error("GetVideoProcessorCaps", error.code().0))?;
            if caps.FeatureCaps
                & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION
                    .0
                    .cast_unsigned()
                == 0
            {
                return Err(render_error(
                    VideoErrorKind::Unsupported,
                    "D3d11Renderer::render",
                    "video processor does not advertise stream rotation",
                ));
            }
        }
        check_format(
            &enumerator,
            dxgi_format(frame.info.format),
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0.cast_unsigned(),
        )?;
        check_format(
            &enumerator,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT
                .0
                .cast_unsigned(),
        )?;
        // SAFETY: Enumerator is a live interface created for this device and content description.
        let processor = unsafe {
            self.device
                .video_device()
                .CreateVideoProcessor(&enumerator, 0)
        }
        .map_err(|error| backend_error("CreateVideoProcessor", error.code().0))?;
        let input_color_space =
            crate::d3d12_scale::checked_color_space(frame.info.format, frame.info.color)?;
        let output_color_space = crate::d3d12_scale::checked_color_space(
            PixelFormat::Bgra8,
            crate::color::bgra8_output_color(),
        )?;
        let video_context: ID3D11VideoContext1 =
            self.device.video_context().cast().map_err(|error| {
                backend_error(
                    "ID3D11VideoContext::cast ID3D11VideoContext1",
                    error.code().0,
                )
            })?;
        // SAFETY: The context, processor and color-space descriptions belong to this D3D11 device.
        unsafe {
            video_context.VideoProcessorSetStreamColorSpace1(&processor, 0, input_color_space);
            video_context.VideoProcessorSetOutputColorSpace1(&processor, output_color_space);
        }
        let source_size = frame.info.size;
        let (rotated_source_rect, destination_rect) =
            crate::d3d12_scale::compute_rotated_rectangles(
                source_size,
                self.target_size,
                None,
                parameters.destination,
                parameters.aspect_mode,
                frame.sample_aspect_ratio(),
                parameters.rotation,
            )?;
        let source_rect = unrotate_rect(rotated_source_rect, source_size, parameters.rotation)?;
        let mut input_view = None;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            // Zero selects the resource's DXGI format. A DXGI_FORMAT enum value is not a FOURCC.
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: u32::try_from(frame.surface_index).map_err(|_| {
                        render_error(
                            VideoErrorKind::InvalidInput,
                            "D3d11Renderer::render",
                            "input array slice exceeds D3D11 range",
                        )
                    })?,
                },
            },
        };
        // SAFETY: Texture, enumerator, and initialized descriptor belong to this D3D11 device.
        unsafe {
            self.device.video_device().CreateVideoProcessorInputView(
                &frame.texture,
                &enumerator,
                &raw const input_desc,
                Some(&raw mut input_view),
            )
        }
        .map_err(|error| backend_error("CreateVideoProcessorInputView", error.code().0))?;
        let input_view = input_view.ok_or_else(|| {
            backend_error(
                "CreateVideoProcessorInputView",
                0x8000_4003_u32.cast_signed(),
            )
        })?;
        // SAFETY: The swap chain is live and buffer zero is its current BGRA output texture.
        let output_texture: ID3D11Texture2D = unsafe { self.swap_chain.GetBuffer(0) }
            .map_err(|error| backend_error("IDXGISwapChain1::GetBuffer", error.code().0))?;
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view = None;
        // SAFETY: Swap-chain texture and processor enumerator belong to this D3D11 device.
        unsafe {
            self.device.video_device().CreateVideoProcessorOutputView(
                &output_texture,
                &enumerator,
                &raw const output_desc,
                Some(&raw mut output_view),
            )
        }
        .map_err(|error| backend_error("CreateVideoProcessorOutputView", error.code().0))?;
        let output_view: ID3D11VideoProcessorOutputView = output_view.ok_or_else(|| {
            backend_error(
                "CreateVideoProcessorOutputView",
                0x8000_4003_u32.cast_signed(),
            )
        })?;
        let background = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                RGBA: D3D11_VIDEO_COLOR_RGBA {
                    R: parameters.background.red,
                    G: parameters.background.green,
                    B: parameters.background.blue,
                    A: parameters.background.alpha,
                },
            },
        };
        // SAFETY: Processor and initialized RGBA background value are valid for this call.
        unsafe {
            self.device
                .video_context()
                .VideoProcessorSetOutputBackgroundColor(&processor, false, &raw const background);
        };
        // SAFETY: Processor and rectangles are valid. The pointers remain valid through each call.
        unsafe {
            self.device
                .video_context()
                .VideoProcessorSetStreamSourceRect(
                    &processor,
                    0,
                    true,
                    Some(&raw const source_rect),
                );
            self.device.video_context().VideoProcessorSetStreamDestRect(
                &processor,
                0,
                true,
                Some(&raw const destination_rect),
            );
            self.device.video_context().VideoProcessorSetStreamRotation(
                &processor,
                0,
                parameters.rotation != Rotation::Degrees0,
                rotation,
            );
        }
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input_view.clone())),
            ..Default::default()
        };
        // SAFETY: Live processor and views are from this device; stream owns a cloned COM ref until
        // after the synchronous blit. It is manually released immediately after the call.
        let blit_result = unsafe {
            self.device.video_context().VideoProcessorBlt(
                &processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        drop(ManuallyDrop::into_inner(std::mem::replace(
            &mut stream.pInputSurface,
            ManuallyDrop::new(None),
        )));
        if let Err(error) = blit_result {
            self.wait_for_render(frame)?;
            return Err(backend_error("VideoProcessorBlt", error.code().0));
        }
        let sync_interval = match parameters.present_mode {
            PresentMode::VSync => 1,
            PresentMode::Immediate => 0,
        };
        // SAFETY: The swap chain is live and the sync interval is a valid DXGI present setting.
        let result = unsafe { self.swap_chain.Present(sync_interval, DXGI_PRESENT(0)) };
        self.wait_for_render(frame)?;
        if result.0 == 0x087a_0001 {
            Ok(PresentResult::Occluded)
        } else if result.is_ok() {
            Ok(PresentResult::Presented)
        } else {
            Err(backend_error("IDXGISwapChain1::Present", result.0))
        }
    }
}

impl D3dRender for D3d11Renderer {
    type Frame = D3d11DecodedFrame;

    fn target_size(&self) -> FrameSize {
        self.target_size
    }

    fn resize(&mut self, size: FrameSize) -> VideoResult<()> {
        self.wait_idle()?;
        // SAFETY: No swap-chain buffer views are retained between calls; new size is nonzero.
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
        self.render_frame(frame, parameters)
    }
}

fn check_format(
    enumerator: &ID3D11VideoProcessorEnumerator,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    required: u32,
) -> VideoResult<()> {
    // SAFETY: The enumerator is live and format is a validated swap-chain/video input format.
    let support = unsafe { enumerator.CheckVideoProcessorFormat(format) }
        .map_err(|error| backend_error("CheckVideoProcessorFormat", error.code().0))?;
    if support & required != required {
        return Err(render_error(
            VideoErrorKind::Unsupported,
            "D3d11Renderer::render",
            "video processor does not support this input/output format",
        ));
    }
    Ok(())
}

fn d3d11_rotation(rotation: Rotation) -> D3D11_VIDEO_PROCESSOR_ROTATION {
    match rotation {
        Rotation::Degrees0 => D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY,
        Rotation::Degrees90 => D3D11_VIDEO_PROCESSOR_ROTATION_90,
        Rotation::Degrees180 => D3D11_VIDEO_PROCESSOR_ROTATION_180,
        Rotation::Degrees270 => D3D11_VIDEO_PROCESSOR_ROTATION_270,
    }
}

fn unrotate_rect(rect: RECT, source_size: FrameSize, rotation: Rotation) -> VideoResult<RECT> {
    let width = i32::try_from(source_size.width()).map_err(|_| {
        render_error(
            VideoErrorKind::InvalidInput,
            "D3d11Renderer::render",
            "source width exceeds video-processor rectangle range",
        )
    })?;
    let height = i32::try_from(source_size.height()).map_err(|_| {
        render_error(
            VideoErrorKind::InvalidInput,
            "D3d11Renderer::render",
            "source height exceeds video-processor rectangle range",
        )
    })?;
    Ok(match rotation {
        Rotation::Degrees0 => rect,
        Rotation::Degrees90 => RECT {
            left: rect.top,
            top: height - rect.right,
            right: rect.bottom,
            bottom: height - rect.left,
        },
        Rotation::Degrees180 => RECT {
            left: width - rect.right,
            top: height - rect.bottom,
            right: width - rect.left,
            bottom: height - rect.top,
        },
        Rotation::Degrees270 => RECT {
            left: width - rect.bottom,
            top: rect.left,
            right: width - rect.top,
            bottom: rect.right,
        },
    })
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

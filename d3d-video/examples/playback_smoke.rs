#![cfg(windows)]

use std::error::Error;
use std::io::{Read, Seek};
use std::process::ExitCode;

use d3d_video::{
    AspectMode, D3dDecode, D3dRender, D3dScale, FrameSize, Mp4VideoPlayback, PlaybackOptions,
    PresentMode, RenderParameters, Rotation, ScaleFilter, ScaleRequest,
};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG,
    PM_REMOVE, PeekMessageW, PostQuitMessage, RegisterClassW, SW_SHOW, ShowWindow,
    TranslateMessage, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;
const SURFACE_COUNT: u32 = 16;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("playback_smoke: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let backend = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("usage: cargo run -p d3d-video --example playback_smoke -- <d3d9|d3d11|d3d12> <file.mp4> [track-index]")?;
    let path = arguments.next().ok_or("missing MP4 path")?;
    let track_number = arguments
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if arguments.next().is_some() {
        return Err("too many arguments".into());
    }

    let hwnd = create_test_window()?;
    let size = FrameSize::new(WINDOW_WIDTH, WINDOW_HEIGHT)?;
    let options = PlaybackOptions {
        track_number,
        scale_request: ScaleRequest {
            output_size: size,
            output_format: d3d_video::PixelFormat::Bgra8,
            source_rect: None,
            destination_rect: None,
            aspect_mode: AspectMode::Contain,
            rotation: Rotation::Degrees0,
            filter: ScaleFilter::DriverDefault,
            output_color: None,
        },
        render_parameters: RenderParameters {
            present_mode: PresentMode::VSync,
            ..RenderParameters::default()
        },
        max_metadata_bytes: 64 * 1024 * 1024,
        max_sample_bytes: 32 * 1024 * 1024,
    };
    let source = std::fs::File::open(path)?;

    match backend.as_str() {
        "d3d9" => {
            let device = d3d_video::d3d9_device::D3d9Device::create(
                d3d_video::d3d9_device::D3d9DeviceOptions::default(),
            )?;
            let decoder = d3d_video::D3d9DecodeSession::new(device.clone(), SURFACE_COUNT)?;
            let scaler = d3d_video::D3d9Scaler::new(device.clone())?;
            let renderer = d3d_video::D3d9Renderer::new(device, hwnd)?;
            run_playback(source, decoder, scaler, renderer, options)?;
        }
        "d3d11" => {
            let device = d3d_video::d3d11_device::D3d11Device::create(
                d3d_video::d3d11_device::D3d11DeviceOptions::default(),
            )?;
            let decoder = d3d_video::D3d11DecodeSession::new(device.clone(), SURFACE_COUNT)?;
            let scaler = d3d_video::D3d11Scaler::new(device.clone())?;
            let renderer = d3d_video::D3d11Renderer::new(device, hwnd, size)?;
            run_playback(source, decoder, scaler, renderer, options)?;
        }
        "d3d12" => {
            let device = d3d_video::d3d12_device::D3d12Device::create(
                d3d_video::d3d12_device::D3d12DeviceOptions::default(),
            )?;
            let decoder = d3d_video::D3d12DecodeSession::new(device.clone(), SURFACE_COUNT)?;
            let scaler = d3d_video::D3d12Scaler::create(&device)?;
            let renderer = d3d_video::D3d12Renderer::new(device, hwnd, size)?;
            run_playback(source, decoder, scaler, renderer, options)?;
        }
        _ => return Err("backend must be d3d9, d3d11, or d3d12".into()),
    }
    Ok(())
}

fn run_playback<R, D, S, P>(
    source: R,
    decoder: D,
    scaler: S,
    renderer: P,
    options: PlaybackOptions,
) -> Result<(), Box<dyn Error>>
where
    R: Read + Seek,
    D: D3dDecode,
    S: D3dScale<Frame = D::Frame>,
    P: D3dRender<Frame = D::Frame>,
{
    let mut playback = Mp4VideoPlayback::open(source, decoder, scaler, renderer, options)?;
    println!("indexed {} decode-order samples", playback.sample_count());
    let mut presented = 0_usize;
    loop {
        if !pump_messages()? {
            println!("window closed; stopping playback");
            return Ok(());
        }
        let step = playback.step()?;
        presented += step.frames_presented;
        if step.end_of_stream {
            break;
        }
        if presented > 0 && presented.is_multiple_of(120) {
            println!(
                "sample {}/{}; presented {presented} frames",
                playback.position(),
                playback.sample_count()
            );
        }
    }
    let tail = playback.drain()?;
    presented += tail.frames_presented;
    println!("finished; presented {presented} frames");
    Ok(())
}

fn create_test_window() -> Result<HWND, Box<dyn Error>> {
    // SAFETY: A null module name requests this process's loaded executable module.
    let module = unsafe { GetModuleHandleW(None)? };
    let instance = HINSTANCE(module.0);
    let window_width = i32::try_from(WINDOW_WIDTH)?;
    let window_height = i32::try_from(WINDOW_HEIGHT)?;
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: w!("RustD3dPlaybackSmoke"),
        ..WNDCLASSW::default()
    };
    // SAFETY: `class` remains valid for the call and references a static UTF-16 class name.
    let atom = unsafe { RegisterClassW(&raw const class) };
    if atom == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: Class registration succeeded; null parent/menu/creation data are valid for a top-level test window.
    let hwnd = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
            w!("RustD3dPlaybackSmoke"),
            w!("Rust D3D playback smoke test"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            window_width,
            window_height,
            None,
            None,
            Some(instance),
            None,
        )?
    };
    // SAFETY: `hwnd` is the live window returned above.
    // SAFETY: `hwnd` is the live window returned above.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    Ok(hwnd)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        // SAFETY: This callback runs on the window thread and requests termination of its message
        // loop after the test window has been destroyed.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: The callback forwards the exact parameters supplied by Windows.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn pump_messages() -> Result<bool, Box<dyn Error>> {
    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is writable storage and a null HWND polls the current thread queue.
        if unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.0 == 0 {
            return Ok(true);
        }
        if message.message == WM_QUIT {
            return Ok(false);
        }
        // SAFETY: `message` was filled by PeekMessageW and is valid for dispatch.
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

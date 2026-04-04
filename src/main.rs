#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals, dead_code)]

mod bindings {
    include!(concat!(env!("OUT_DIR"), "/uvc_bindings.rs"));
}

use std::{io::Write, ptr, sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering}, sync::Mutex, thread};

use anyhow::{Context, Result};
use clap::Parser;
use gphoto2::Context as CameraContext;
use usb_gadget::{
    default_udc,
    function::video::Format,
    Class, Config, Gadget, Id, Strings,
};

#[derive(Parser, Debug)]
#[command(version, about)]
/// Stream Canon EOS camera as a USB webcam via UVC gadget.
struct Args {
    /// Drop into focus control TUI (signals running daemon)
    #[arg(long)]
    focus: bool,
}

const PID_FILE: &str = "/run/eos-stream.pid";

static PREVIEW_WIDTH: AtomicU32 = AtomicU32::new(1280);
static PREVIEW_HEIGHT: AtomicU32 = AtomicU32::new(720);
const FPS: u16 = 30;

static FRAME_NUM: AtomicU32 = AtomicU32::new(0);

struct CameraState {
    ctx: CameraContext,
    camera: gphoto2::Camera,
}

static CAMERA: Mutex<Option<CameraState>> = Mutex::new(None);
static PLACEHOLDER_JPEG: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static STREAMING: AtomicBool = AtomicBool::new(false);

static FOCUS_NEAR: AtomicI32 = AtomicI32::new(0);
static FOCUS_FAR: AtomicI32 = AtomicI32::new(0);

fn handle_focus_signal(sig: libc::c_int) {
    if sig == libc::SIGUSR1 {
        FOCUS_NEAR.fetch_add(1, Ordering::Relaxed);
    } else {
        FOCUS_FAR.fetch_add(1, Ordering::Relaxed);
    }
}

fn focus_client() -> Result<()> {
    let pid = std::fs::read_to_string(PID_FILE)
        .with_context(|| format!("No daemon running (can't read {PID_FILE})"))?
        .trim().parse::<i32>()
        .with_context(|| "Invalid pid file")?;
    if unsafe { libc::kill(pid, 0) } != 0 {
        anyhow::bail!("Daemon pid {pid} not running");
    }
    let term = console::Term::stderr();
    eprintln!("Focus control (daemon pid {pid})");
    eprintln!("  +  focus nearer");
    eprintln!("  -  focus farther");
    eprintln!("  ^C quit");
    loop {
        match term.read_char()? {
            '+' => {
                unsafe { libc::kill(pid, libc::SIGUSR1); }
                eprint!(".");
                std::io::stderr().flush()?;
            }
            '-' => {
                unsafe { libc::kill(pid, libc::SIGUSR2); }
                eprint!(".");
                std::io::stderr().flush()?;
            }
            '\x03' => break,
            _ => {}
        }
    }
    eprintln!();
    Ok(())
}

/// Called from C when the host starts streaming.
#[no_mangle]
pub extern "C" fn rust_camera_start() -> i32 {
    let guard = CAMERA.lock().unwrap();
    if guard.is_none() {
        eprintln!("rust_camera_start: no camera connected");
        return -1;
    };
    FRAME_NUM.store(0, Ordering::Relaxed);
    STREAMING.store(true, Ordering::Relaxed);
    0
}

/// Lower the mirror when the host stops streaming.
#[no_mangle]
pub extern "C" fn rust_camera_stop() {
    let guard = CAMERA.lock().unwrap();
    STREAMING.store(false, Ordering::Relaxed);
    let Some(state) = guard.as_ref() else { return };
    match state.camera.config_key::<gphoto2::widget::ToggleWidget>("viewfinder").wait() {
        Ok(vf) => {
            let _ = vf.set_toggled(false);
            if let Err(e) = state.camera.set_config(&vf).wait() {
                eprintln!("Failed to lower mirror: {}", e);
            }
        }
        Err(e) => eprintln!("viewfinder config not found: {}", e),
    }
}

/// Called from C (rust-source.c) to fill a buffer with MJPEG data.
#[no_mangle]
pub extern "C" fn rust_fill_jpeg(buf: *mut u8, max_size: u32) -> u32 {
    if !STREAMING.load(Ordering::Relaxed) {
        return fill_placeholder(buf, max_size);
    }

    let n = FRAME_NUM.fetch_add(1, Ordering::Relaxed);
    let guard = CAMERA.lock().unwrap();
    let Some(state) = guard.as_ref() else {
        eprintln!("rust_fill_jpeg: no camera connected");
        return 0;
    };

    let near = FOCUS_NEAR.swap(0, Ordering::Relaxed);
    let far = FOCUS_FAR.swap(0, Ordering::Relaxed);
    for (count, choice) in [(near, "Near 1"), (far, "Far 1")] {
        for _ in 0..count {
            if let Ok(focus) = state.camera.config_key::<gphoto2::widget::RadioWidget>("manualfocusdrive").wait() {
                let _ = focus.set_choice(choice);
                let _ = state.camera.set_config(&focus).wait();
            }
        }
    }

    let frame = match state.camera.capture_preview().wait() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("capture_preview failed: {}", e);
            return fill_placeholder(buf, max_size);
        }
    };
    let data = match frame.get_data(&state.ctx).wait() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("get_data failed: {}", e);
            return fill_placeholder(buf, max_size);
        }
    };
    drop(guard);

    let len = data.len().min(max_size as usize);

    if n == 0 {
        let _ = std::fs::write("/tmp/test_frame.jpg", &data);
        eprintln!("First camera frame: {} bytes -> /tmp/test_frame.jpg", data.len());
    }

    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), buf, len);
    }

    len as u32
}


fn fill_placeholder(buf: *mut u8, max_size: u32) -> u32 {
    let jpeg = PLACEHOLDER_JPEG.lock().unwrap();
    let len = jpeg.len().min(max_size as usize);
    if len == 0 {
        return 0;
    }
    unsafe {
        ptr::copy_nonoverlapping(jpeg.as_ptr(), buf, len);
    }
    len as u32
}

fn cleanup_old_gadgets() {
    let gadget_dir = std::path::Path::new("/sys/kernel/config/usb_gadget");
    let Ok(entries) = std::fs::read_dir(gadget_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let udc_path = path.join("UDC");
        if udc_path.exists() {
            eprintln!("Cleaning up old gadget: {}", path.display());
            let _ = std::fs::write(&udc_path, "\n");
            thread::sleep(std::time::Duration::from_millis(500));
        }
        let _ = usb_gadget::remove_all();
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.focus {
        return focus_client();
    }

    // Register signal handlers for focus control
    unsafe {
        libc::signal(libc::SIGUSR1, handle_focus_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGUSR2, handle_focus_signal as *const () as libc::sighandler_t);
    }

    let _ = std::fs::write(PID_FILE, format!("{}", std::process::id()));

    static QUIT: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        QUIT.store(true, Ordering::SeqCst);
    })?;

    // Connect camera and probe preview resolution (kept alive for the lifetime of the process)
    let ctx = CameraContext::new()?;
    let camera = ctx.autodetect_camera().wait()
        .with_context(|| "Failed to discover camera for probing")?;
    let camera_model_name = camera.abilities().model().to_string();
    eprintln!("Camera connected: {}", camera_model_name);

    // Query camera metadata for USB gadget identity
    let gadget_product = camera.config_key::<gphoto2::widget::TextWidget>("cameramodel").wait()
        .map(|w| w.value())
        .unwrap_or_else(|_| camera_model_name.clone());
    let gadget_manufacturer = camera.config_key::<gphoto2::widget::TextWidget>("manufacturer").wait()
        .map(|w| format!("{} (via eos-stream)", w.value()))
        .unwrap_or_else(|_| {
            let mfr = camera_model_name.split_whitespace().next().unwrap_or("Unknown");
            format!("{} (via eos-stream)", mfr)
        });
    let gadget_serial = camera.config_key::<gphoto2::widget::TextWidget>("eosserialnumber").wait()
        .or_else(|_| camera.config_key::<gphoto2::widget::TextWidget>("serialnumber").wait())
        .map(|w| w.value())
        .unwrap_or_else(|_| "0000000".to_string());
    eprintln!("USB gadget identity: manufacturer=\"{}\" product=\"{}\" serial=\"{}\"",
        gadget_manufacturer, gadget_product, gadget_serial);

    let frame = camera.capture_preview().wait()?;
    let data = frame.get_data(&ctx).wait()?;
    let reader = image::ImageReader::new(std::io::Cursor::new(&data))
        .with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    PREVIEW_WIDTH.store(w, Ordering::Relaxed);
    PREVIEW_HEIGHT.store(h, Ordering::Relaxed);
    eprintln!("Preview resolution: {}x{}", w, h);
    // Generate red placeholder JPEG for prefill before camera is in live view
    {
        let mut img = image::RgbImage::new(w, h);
        for p in img.pixels_mut() {
            *p = image::Rgb([180, 0, 0]);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).expect("failed to encode placeholder");
        *PLACEHOLDER_JPEG.lock().unwrap() = buf.into_inner();
    }
    // Lower mirror after probing
    if let Ok(vf) = camera.config_key::<gphoto2::widget::ToggleWidget>("viewfinder").wait() {
        let _ = vf.set_toggled(false);
        let _ = camera.set_config(&vf).wait();
    }
    *CAMERA.lock().unwrap() = Some(CameraState { ctx, camera });

    let width = PREVIEW_WIDTH.load(Ordering::Relaxed);
    let height = PREVIEW_HEIGHT.load(Ordering::Relaxed);

    loop {
        if QUIT.load(Ordering::SeqCst) {
            break;
        }

        cleanup_old_gadgets();

        eprintln!("Setting up UVC gadget...");

        let mut builder = usb_gadget::function::video::UvcBuilder::default();
        // Work around usb-gadget crate bug: Frame::new() computes intervals as
        // 1_000_000_000/fps (nanoseconds) instead of 10_000_000/fps (100ns units).
        let frame = usb_gadget::function::video::UvcFrame::new(
            width, height, Format::Mjpeg,
            [10_000_000 / FPS as u32],  // 333333 for 30fps
        );
        builder.frames = vec![frame];
        builder.processing_controls = Some(0);
        builder.camera_controls = Some(0);
        let (_uvc, uvc_func) = builder.build();

        let udc = default_udc().context("No UDC found. Is dwc2 overlay enabled?")?;
        let gadget = Gadget::new(
            Class::MISCELLANEOUS_IAD,
            Id::new(0x1d6b, 0x0104),
            Strings::new(&gadget_manufacturer, &gadget_product, &gadget_serial),
        )
        .with_config(Config::new("UVC Config").with_function(uvc_func))
        .bind(&udc)
        .context("Failed to bind gadget to UDC")?;

        eprintln!("Gadget bound to {}", udc.name().to_string_lossy());

        let soft_connect = format!("/sys/class/udc/{}/soft_connect", udc.name().to_string_lossy());
        if let Err(e) = std::fs::write(&soft_connect, "connect") {
            eprintln!("soft_connect: {} (may already be connected)", e);
        } else {
            eprintln!("Forced USB soft connect");
        }
        thread::sleep(std::time::Duration::from_secs(1));

        unsafe {
            let fc = bindings::configfs_parse_uvc_function(ptr::null());
            if fc.is_null() {
                eprintln!("Failed to parse UVC function config, retrying...");
                drop(gadget);
                let _ = usb_gadget::remove_all();
                thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }

            let src = bindings::rust_video_source_create();
            if src.is_null() {
                bindings::configfs_free_uvc_function(fc);
                drop(gadget);
                let _ = usb_gadget::remove_all();
                eprintln!("Failed to create video source, retrying...");
                thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }

            let mut events: bindings::events = std::mem::zeroed();
            bindings::events_init(&mut events);

            let stream = bindings::uvc_stream_new((*fc).video);
            if stream.is_null() {
                bindings::video_source_destroy(src);
                bindings::configfs_free_uvc_function(fc);
                drop(gadget);
                let _ = usb_gadget::remove_all();
                eprintln!("Failed to create UVC stream, retrying...");
                thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }

            bindings::uvc_stream_set_event_handler(stream, &mut events);
            bindings::uvc_stream_set_video_source(stream, src);
            bindings::uvc_stream_init_uvc(stream, fc);

            eprintln!("UVC stream ready, waiting for host ({}x{} @ {}fps)...", width, height, FPS);

            let events_send = &mut events as *mut bindings::events as usize;
            thread::spawn(move || {
                while !QUIT.load(Ordering::SeqCst) {
                    thread::sleep(std::time::Duration::from_millis(100));
                }
                eprintln!("\nStopping...");
                bindings::events_stop(events_send as *mut bindings::events);
            });

            bindings::events_loop(&mut events);

            eprintln!("Event loop exited, cleaning up...");
            bindings::uvc_stream_delete(stream);
            bindings::video_source_destroy(src);
            bindings::events_cleanup(&mut events);
            bindings::configfs_free_uvc_function(fc);
        }

        drop(gadget);
        let _ = usb_gadget::remove_all();

        if QUIT.load(Ordering::SeqCst) {
            break;
        }

        eprintln!("USB disconnected, restarting in 2s...");
        thread::sleep(std::time::Duration::from_secs(2));
    }

    let _ = std::fs::remove_file(PID_FILE);
    eprintln!("Done.");
    Ok(())
}

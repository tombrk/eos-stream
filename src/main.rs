#![allow(non_snake_case, dead_code)]

use std::{
    io, mem, ptr, slice, sync::Arc, sync::Mutex, thread,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use console::Term;
use gphoto2::{widget::RadioWidget, Context as CameraContext};
use nix::sys::mman;
use usb_gadget::{
    default_udc,
    function::video::{Format, Frame, Uvc},
    Class, Config, Gadget, Id, Strings,
};

#[derive(Parser, Debug)]
#[command(version, about)]
/// Stream Canon EOS camera as a USB webcam via UVC gadget.
///
/// Use +/- keys to adjust focus.
struct Args {}

// ---- V4L2 / UVC ioctl definitions (not in any Rust crate) ----

// v4l2 buffer types & memory
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_MEMORY_MMAP: u32 = 1;

// v4l2 event types
const V4L2_EVENT_PRIVATE_START: u32 = 0x08000000;

// UVC-specific event IDs (linux/usb/g_uvc.h)
const UVC_EVENT_FIRST: u32 = V4L2_EVENT_PRIVATE_START + 0;
const UVC_EVENT_CONNECT: u32 = UVC_EVENT_FIRST + 0;
const UVC_EVENT_DISCONNECT: u32 = UVC_EVENT_FIRST + 1;
const UVC_EVENT_STREAMON: u32 = UVC_EVENT_FIRST + 2;
const UVC_EVENT_STREAMOFF: u32 = UVC_EVENT_FIRST + 3;
const UVC_EVENT_SETUP: u32 = UVC_EVENT_FIRST + 4;
const UVC_EVENT_DATA: u32 = UVC_EVENT_FIRST + 5;
const UVC_EVENT_LAST: u32 = UVC_EVENT_FIRST + 5;

// USB setup request fields
const USB_TYPE_CLASS: u8 = 0x20;
const USB_RECIP_INTERFACE: u8 = 0x01;
const USB_DIR_IN: u8 = 0x80;

// UVC class-specific request codes
const UVC_SET_CUR: u8 = 0x01;
const UVC_GET_CUR: u8 = 0x81;
const UVC_GET_MIN: u8 = 0x82;
const UVC_GET_MAX: u8 = 0x83;
const UVC_GET_DEF: u8 = 0x87;

// UVC VS interface control selectors
const UVC_VS_PROBE_CONTROL: u8 = 0x01;
const UVC_VS_COMMIT_CONTROL: u8 = 0x02;

// ioctl numbers (from videodev2.h)
nix::ioctl_readwrite!(vidioc_subscribe_event, b'V', 90, v4l2_event_subscription);
nix::ioctl_readwrite!(vidioc_dqevent, b'V', 89, v4l2_event);
nix::ioctl_readwrite!(vidioc_reqbufs, b'V', 8, v4l2_requestbuffers);
nix::ioctl_readwrite!(vidioc_querybuf, b'V', 9, v4l2_buffer);
nix::ioctl_readwrite!(vidioc_qbuf, b'V', 15, v4l2_buffer);
nix::ioctl_readwrite!(vidioc_dqbuf, b'V', 17, v4l2_buffer);
nix::ioctl_write_ptr!(vidioc_streamon, b'V', 18, i32);
nix::ioctl_write_ptr!(vidioc_streamoff, b'V', 19, i32);
nix::ioctl_readwrite!(vidioc_s_fmt, b'V', 5, v4l2_format);

// UVC gadget ioctl for sending response
const UVCIOC_SEND_RESPONSE: i32 = 0x40406301u32 as i32; // _IOW('c', 1, struct uvc_request_data)

// ---- Kernel structures (trimmed to what we need) ----

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct v4l2_event_subscription {
    type_: u32,
    id: u32,
    flags: u32,
    reserved: [u32; 5],
}

#[repr(C)]
#[derive(Copy, Clone)]
struct v4l2_event {
    type_: u32,
    // union u - 64 bytes, we'll interpret per event type
    u: [u8; 64],
    pending: u32,
    sequence: u32,
    timestamp: [u64; 2], // struct timespec
    id: u32,
    reserved: [u32; 8],
}

impl Default for v4l2_event {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

// USB control request (8 bytes, embedded in UVC_EVENT_SETUP)
#[repr(C)]
#[derive(Copy, Clone, Default, Debug)]
struct usb_ctrlrequest {
    bRequestType: u8,
    bRequest: u8,
    wValue: u16,
    wIndex: u16,
    wLength: u16,
}

// UVC streaming control (used for probe/commit) - 48 bytes per UVC 1.5 spec
#[repr(C)]
#[derive(Copy, Clone)]
struct uvc_streaming_control {
    bmHint: u16,
    bFormatIndex: u8,
    bFrameIndex: u8,
    dwFrameInterval: u32,
    wKeyFrameRate: u16,
    wPFrameRate: u16,
    wCompQuality: u16,
    wCompWindowSize: u16,
    wDelay: u16,
    dwMaxVideoFrameSize: u32,
    dwMaxPayloadTransferSize: u32,
    dwClockFrequency: u32,
    bmFramingInfo: u8,
    bPreferedVersion: u8,
    bMinVersion: u8,
    bMaxVersion: u8,
    bUsage: u8,
    bBitDepthLuma: u8,
    bmSettings: u8,
    bMaxNumberOfRefFramesPlus1: u8,
    bmRateControlModes: u16,
    bmLayoutPerStream: u64,
}

impl Default for uvc_streaming_control {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

// UVC request data for UVCIOC_SEND_RESPONSE
#[repr(C)]
#[derive(Copy, Clone)]
struct uvc_request_data {
    length: i32,
    data: [u8; 60],
}

impl Default for uvc_request_data {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct v4l2_requestbuffers {
    count: u32,
    type_: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
    reserved2: [u32; 3], // pad to match kernel struct, some versions differ
}

#[repr(C)]
#[derive(Copy, Clone)]
struct v4l2_buffer {
    index: u32,
    type_: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp_sec: u64,
    timestamp_usec: u64,
    // timecode
    timecode: [u32; 4],
    sequence: u32,
    memory: u32,
    // union m
    m_offset: u32, // for mmap
    _m_pad: u32,
    length: u32,
    reserved2: u32,
    // request_fd or reserved
    request_fd: i32,
}

impl Default for v4l2_buffer {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
struct v4l2_format {
    type_: u32,
    // union fmt - use pix (v4l2_pix_format) which is the biggest at ~200 bytes
    fmt: [u8; 200],
}

impl Default for v4l2_format {
    fn default() -> Self {
        unsafe { mem::zeroed() }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct v4l2_pix_format {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    // ycbcr_enc or hsv_enc union
    encoding: u32,
    quantization: u32,
    xfer_func: u32,
}

const NUM_BUFS: usize = 2;
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const FPS: u16 = 30;

fn main() -> Result<()> {
    let _ = Args::parse();

    eprintln!("Setting up UVC gadget...");

    // --- 1. Create USB gadget with UVC function ---
    let (uvc, uvc_func) = Uvc::new(vec![
        Frame::new(WIDTH, HEIGHT, vec![FPS], Format::Mjpeg),
    ]);

    let udc = default_udc().context("No UDC found. Is dwc2 overlay enabled?")?;
    let _gadget = Gadget::new(
        Class::MISCELLANEOUS_IAD,
        Id::new(0x1d6b, 0x0104), // Linux Foundation, Multifunction Composite Gadget
        Strings::new("EOS Stream", "Canon EOS Webcam", "0000001"),
    )
    .with_config(Config::new("UVC Config").with_function(uvc_func))
    .bind(&udc)
    .context("Failed to bind gadget to UDC")?;

    eprintln!("Gadget bound to {}", udc.name().to_string_lossy());

    // --- 2. Find the UVC video device ---
    let status = uvc.status();
    eprintln!("UVC status: {:?}", status);

    // Find /dev/videoN created by the UVC gadget
    let video_path = find_uvc_video_device()?;
    eprintln!("UVC video device: {}", video_path);

    let uvc_fd = nix::fcntl::open(
        video_path.as_str(),
        nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_NONBLOCK,
        nix::sys::stat::Mode::empty(),
    )
    .context("Failed to open UVC video device")?;

    // --- 3. Subscribe to UVC events ---
    for event_type in [
        UVC_EVENT_SETUP,
        UVC_EVENT_DATA,
        UVC_EVENT_STREAMON,
        UVC_EVENT_STREAMOFF,
    ] {
        let mut sub = v4l2_event_subscription {
            type_: event_type,
            ..Default::default()
        };
        unsafe { vidioc_subscribe_event(uvc_fd, &mut sub) }
            .context("Failed to subscribe to UVC event")?;
    }
    eprintln!("Subscribed to UVC events");

    // --- 4. Connect to camera ---
    eprintln!("Connecting to camera...");
    let cam_ctx = CameraContext::new()?;
    let camera = cam_ctx
        .autodetect_camera()
        .wait()
        .context("Failed to discover camera")?;

    let focus = camera
        .config_key::<RadioWidget>("manualfocusdrive")
        .wait()?;

    let camera = Arc::new(Mutex::new(camera));

    // Focus control thread
    let cam = camera.clone();
    thread::spawn(move || -> Result<()> {
        let term = Term::stderr();
        loop {
            let res = match term.read_char()? {
                '+' => {
                    focus.set_choice("Near 1")?;
                    cam.lock().unwrap().set_config(&focus).wait()
                }
                '-' => {
                    focus.set_choice("Far 1")?;
                    cam.lock().unwrap().set_config(&focus).wait()
                }
                _ => Ok(()),
            };
            if let Err(err) = res {
                eprintln!("set focus: {}", err)
            }
        }
    });

    // --- 5. Event loop ---
    eprintln!("Waiting for host to connect...");

    let mut probe = uvc_streaming_control::default();
    probe.bFormatIndex = 1;
    probe.bFrameIndex = 1;
    probe.dwFrameInterval = 10_000_000 / FPS as u32; // 100ns units
    probe.dwMaxVideoFrameSize = (WIDTH * HEIGHT * 2) as u32;
    probe.dwMaxPayloadTransferSize = 3072;
    probe.bmFramingInfo = 0x03;

    let mut commit = probe;
    let mut streaming = false;

    // mmap'd buffer info
    let mut buf_ptrs: Vec<(*mut u8, usize)> = Vec::new();

    let pollfd = libc::pollfd {
        fd: uvc_fd,
        events: libc::POLLIN | libc::POLLPRI,
        revents: 0,
    };

    loop {
        let mut pfd = pollfd;
        let ret = unsafe { libc::poll(&mut pfd as *mut _, 1, 1000) };
        if ret < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            bail!("poll error: {}", e);
        }
        if ret == 0 {
            // timeout - if streaming, try to push a frame
            if streaming {
                push_frame(&camera, &cam_ctx, uvc_fd, &buf_ptrs)?;
            }
            continue;
        }

        // Dequeue events
        if pfd.revents & libc::POLLPRI != 0 {
            let mut event = v4l2_event::default();
            while unsafe { vidioc_dqevent(uvc_fd, &mut event) }.is_ok() {
                match event.type_ {
                    UVC_EVENT_SETUP => {
                        let ctrl: usb_ctrlrequest =
                            unsafe { ptr::read(event.u.as_ptr() as *const _) };
                        handle_setup(uvc_fd, &ctrl, &mut probe, &mut commit)?;
                    }
                    UVC_EVENT_DATA => {
                        let data: uvc_streaming_control =
                            unsafe { ptr::read(event.u.as_ptr() as *const _) };
                        // Host sent us a commit - update our commit state
                        commit = data;
                        eprintln!(
                            "UVC DATA: format={} frame={} interval={}",
                            data.bFormatIndex, data.bFrameIndex, data.dwFrameInterval
                        );
                    }
                    UVC_EVENT_STREAMON => {
                        eprintln!("STREAMON");
                        // Set format on the V4L2 output device
                        set_v4l2_format(uvc_fd, WIDTH, HEIGHT)?;
                        // Allocate and map buffers
                        buf_ptrs = alloc_buffers(uvc_fd)?;
                        // Start streaming
                        let buf_type = V4L2_BUF_TYPE_VIDEO_OUTPUT as i32;
                        unsafe { vidioc_streamon(uvc_fd, &buf_type) }
                            .context("STREAMON failed")?;
                        streaming = true;
                        // Push first frames
                        for _ in 0..buf_ptrs.len() {
                            push_frame(&camera, &cam_ctx, uvc_fd, &buf_ptrs)?;
                        }
                    }
                    UVC_EVENT_STREAMOFF => {
                        eprintln!("STREAMOFF");
                        streaming = false;
                        let buf_type = V4L2_BUF_TYPE_VIDEO_OUTPUT as i32;
                        let _ = unsafe { vidioc_streamoff(uvc_fd, &buf_type) };
                        // Unmap and free buffers
                        for (ptr, len) in &buf_ptrs {
                            unsafe {
                                mman::munmap(
                                    std::ptr::NonNull::new(*ptr as *mut _).unwrap(),
                                    *len,
                                ).ok();
                            }
                        }
                        buf_ptrs.clear();
                        free_buffers(uvc_fd)?;
                    }
                    _ => {
                        eprintln!("Unknown UVC event: {}", event.type_);
                    }
                }
            }
        }

        // If streaming and output device is ready for more data
        if streaming && (pfd.revents & libc::POLLOUT != 0) {
            push_frame(&camera, &cam_ctx, uvc_fd, &buf_ptrs)?;
        }
    }
}

fn handle_setup(
    fd: i32,
    ctrl: &usb_ctrlrequest,
    probe: &mut uvc_streaming_control,
    commit: &mut uvc_streaming_control,
) -> Result<()> {
    let cs = (ctrl.wValue >> 8) as u8;
    let mut resp = uvc_request_data::default();

    match ctrl.bRequest {
        UVC_SET_CUR => {
            // Host is going to send data - just ack with max length
            resp.length = ctrl.wLength as i32;
            // Data will arrive as UVC_EVENT_DATA
        }
        UVC_GET_CUR => {
            let src = if cs == UVC_VS_COMMIT_CONTROL { commit } else { probe };
            let bytes = unsafe {
                slice::from_raw_parts(src as *const _ as *const u8, mem::size_of::<uvc_streaming_control>())
            };
            let len = bytes.len().min(resp.data.len()).min(ctrl.wLength as usize);
            resp.data[..len].copy_from_slice(&bytes[..len]);
            resp.length = len as i32;
        }
        UVC_GET_MIN | UVC_GET_MAX | UVC_GET_DEF => {
            // Return probe as min/max/default (we only support one format)
            let bytes = unsafe {
                slice::from_raw_parts(probe as *const _ as *const u8, mem::size_of::<uvc_streaming_control>())
            };
            let len = bytes.len().min(resp.data.len()).min(ctrl.wLength as usize);
            resp.data[..len].copy_from_slice(&bytes[..len]);
            resp.length = len as i32;
        }
        _ => {
            // Stall unknown requests
            resp.length = -34; // -ERANGE to stall
        }
    }

    // Send response via ioctl
    unsafe {
        let ret = libc::ioctl(fd, UVCIOC_SEND_RESPONSE, &resp as *const _);
        if ret < 0 {
            let e = io::Error::last_os_error();
            eprintln!("SEND_RESPONSE failed: {}", e);
        }
    }

    Ok(())
}

fn set_v4l2_format(fd: i32, width: u32, height: u32) -> Result<()> {
    let mut fmt = v4l2_format {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        ..Default::default()
    };

    let pix = v4l2_pix_format {
        width,
        height,
        pixelformat: u32::from_le_bytes(*b"MJPG"),
        sizeimage: width * height * 2, // generous
        field: 1, // V4L2_FIELD_NONE
        colorspace: 8, // V4L2_COLORSPACE_JPEG
        ..Default::default()
    };

    unsafe {
        ptr::copy_nonoverlapping(
            &pix as *const _ as *const u8,
            fmt.fmt.as_mut_ptr(),
            mem::size_of::<v4l2_pix_format>(),
        );
        vidioc_s_fmt(fd, &mut fmt).context("S_FMT failed")?;
    }

    Ok(())
}

fn alloc_buffers(fd: i32) -> Result<Vec<(*mut u8, usize)>> {
    let mut req = v4l2_requestbuffers {
        count: NUM_BUFS as u32,
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        memory: V4L2_MEMORY_MMAP,
        ..Default::default()
    };
    unsafe { vidioc_reqbufs(fd, &mut req) }.context("REQBUFS failed")?;

    let mut bufs = Vec::new();
    for i in 0..req.count {
        let mut buf = v4l2_buffer {
            index: i,
            type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
            memory: V4L2_MEMORY_MMAP,
            ..Default::default()
        };
        unsafe { vidioc_querybuf(fd, &mut buf) }.context("QUERYBUF failed")?;

        let len = buf.length as usize;
        let ptr = unsafe {
            mman::mmap(
                None,
                std::num::NonZeroUsize::new(len).unwrap(),
                mman::ProtFlags::PROT_READ | mman::ProtFlags::PROT_WRITE,
                mman::MapFlags::MAP_SHARED,
                &std::os::fd::BorrowedFd::borrow_raw(fd),
                buf.m_offset as i64,
            )
            .context("mmap failed")?
        };

        bufs.push((ptr.as_ptr() as *mut u8, len));
    }

    Ok(bufs)
}

fn free_buffers(fd: i32) -> Result<()> {
    let mut req = v4l2_requestbuffers {
        count: 0,
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        memory: V4L2_MEMORY_MMAP,
        ..Default::default()
    };
    unsafe { vidioc_reqbufs(fd, &mut req) }.ok();
    Ok(())
}

static mut NEXT_BUF: usize = 0;

fn push_frame(
    camera: &Arc<Mutex<gphoto2::Camera>>,
    ctx: &CameraContext,
    uvc_fd: i32,
    buf_ptrs: &[(*mut u8, usize)],
) -> Result<()> {
    if buf_ptrs.is_empty() {
        return Ok(());
    }

    // Try to dequeue a buffer first (except for initial fill)
    let mut dq_buf = v4l2_buffer {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        memory: V4L2_MEMORY_MMAP,
        ..Default::default()
    };
    let buf_idx = unsafe {
        match vidioc_dqbuf(uvc_fd, &mut dq_buf) {
            Ok(_) => dq_buf.index as usize,
            Err(_) => {
                // No buffer available to dequeue, use next in sequence
                let idx = NEXT_BUF;
                NEXT_BUF = (NEXT_BUF + 1) % buf_ptrs.len();
                idx
            }
        }
    };

    // Capture a frame from the camera
    let frame = camera.lock().unwrap().capture_preview().wait()?;
    let data = frame.get_data(ctx).wait()?;

    let (ptr, max_len) = buf_ptrs[buf_idx];
    let len = data.len().min(max_len);

    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
    }

    // Queue the buffer
    let mut qbuf = v4l2_buffer {
        index: buf_idx as u32,
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        memory: V4L2_MEMORY_MMAP,
        bytesused: len as u32,
        ..Default::default()
    };
    unsafe { vidioc_qbuf(uvc_fd, &mut qbuf) }.context("QBUF failed")?;

    Ok(())
}

fn find_uvc_video_device() -> Result<String> {
    // Look through /sys/class/video4linux/ for a device backed by the UVC gadget
    for entry in std::fs::read_dir("/sys/class/video4linux")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        // Check if this is a gadget UVC device by looking at the function name
        let function_path = entry.path().join("function_name");
        if function_path.exists() {
            let func = std::fs::read_to_string(&function_path).unwrap_or_default();
            if func.trim() == "uvc" {
                return Ok(format!("/dev/{}", name));
            }
        }

        // Fallback: check the device driver symlink
        let driver_path = entry.path().join("device/driver");
        if driver_path.is_symlink() {
            let target = std::fs::read_link(&driver_path).unwrap_or_default();
            let driver_name = target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if driver_name.contains("uvc") || driver_name.contains("g_webcam") {
                return Ok(format!("/dev/{}", name));
            }
        }
    }

    bail!("No UVC gadget video device found in /sys/class/video4linux/")
}

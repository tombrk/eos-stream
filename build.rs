use std::env;
use std::path::PathBuf;

fn main() {
    let uvc_gadget = PathBuf::from("uvc-gadget");
    let lib_dir = uvc_gadget.join("lib");
    let inc_dir = uvc_gadget.join("include");

    println!("cargo:rerun-if-changed=src/uvc_bindings.h");
    println!("cargo:rerun-if-changed=build.rs");

    // Compile uvc-gadget C library
    cc::Build::new()
        .include(&inc_dir)
        .include(inc_dir.join("uvcgadget"))
        .include(&lib_dir)
        // config.h defines
        .define("HAVE_DIRENT_H", "1")
        .define("HAVE_GLOB", "1")
        // Sources (skip libcamera C++ and mjpeg_encoder)
        .file(lib_dir.join("configfs.c"))
        .file(lib_dir.join("events.c"))
        .file(lib_dir.join("stream.c"))
        .file(lib_dir.join("uvc.c"))
        .file(lib_dir.join("v4l2.c"))
        .file(lib_dir.join("video-buffers.c"))
        .file(lib_dir.join("video-source.c"))
        .file(lib_dir.join("test-source.c"))
        .file(lib_dir.join("jpg-source.c"))
        .file(lib_dir.join("slideshow-source.c"))
        .file(lib_dir.join("timer.c"))
        .file("src/rust-source.c")
        .warnings(false)
        .compile("uvcgadget");

    // Generate Rust bindings for the public API
    let bindings = bindgen::Builder::default()
        .header("src/uvc_wrapper.h")
        .clang_arg(format!("-I{}", inc_dir.display()))
        .clang_arg(format!("-I{}", inc_dir.join("uvcgadget").display()))
        .clang_arg(format!("-I{}", lib_dir.display()))
        .clang_arg("-DHAVE_DIRENT_H=1")
        .clang_arg("-DHAVE_GLOB=1")
        .allowlist_function("configfs_parse_uvc_function")
        .allowlist_function("configfs_free_uvc_function")
        .allowlist_function("uvc_stream_new")
        .allowlist_function("uvc_stream_delete")
        .allowlist_function("uvc_stream_init_uvc")
        .allowlist_function("uvc_stream_set_event_handler")
        .allowlist_function("uvc_stream_set_video_source")
        .allowlist_function("uvc_stream_enable")
        .allowlist_function("events_init")
        .allowlist_function("events_loop")
        .allowlist_function("events_stop")
        .allowlist_function("events_cleanup")
        .allowlist_function("test_video_source_create")
        .allowlist_function("test_video_source_init")
        .allowlist_function("jpg_video_source_create")
        .allowlist_function("jpg_video_source_init")
        .allowlist_function("rust_video_source_create")
        .allowlist_function("video_source_destroy")
        .allowlist_type("events")
        .allowlist_type("uvc_stream")
        .allowlist_type("uvc_function_config")
        .allowlist_type("video_source")
        .derive_default(true)
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("uvc_bindings.rs"))
        .expect("Couldn't write bindings");
}

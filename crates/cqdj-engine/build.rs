// Compiles the vendored ft8_lib sources plus our small C shim.
fn main() {
    let lib = "vendor/ft8_lib";
    let sources = [
        "ft8/constants.c",
        "ft8/crc.c",
        "ft8/decode.c",
        "ft8/encode.c",
        "ft8/ldpc.c",
        "ft8/message.c",
        "ft8/text.c",
        "fft/kiss_fft.c",
        "fft/kiss_fftr.c",
        "common/monitor.c",
    ];

    let mut build = cc::Build::new();
    build.include(lib).include("csrc").warnings(false).flag_if_supported("-std=gnu11");
    // ft8_lib prints progress to stderr; silence it inside the app.
    build.define("LOG_PRINTF(...)", Some(""));
    for s in sources {
        let path = format!("{lib}/{s}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }
    build.file("csrc/cqdj_ft8.c");
    println!("cargo:rerun-if-changed=csrc/cqdj_ft8.c");
    println!("cargo:rerun-if-changed=csrc/cqdj_ft8.h");
    build.compile("cqdj_ft8");

    // libm is part of libc on macOS; on Linux it must be linked explicitly.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        println!("cargo:rustc-link-lib=m");
    }
}

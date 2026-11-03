//! Build script.
//!
//! Two jobs:
//!  1. compile `ui/appwindow.slint` into Rust modules (consumed by
//!     `slint::include_modules!()` in `src/main.rs`);
//!  2. on `x86_64-pc-windows-msvc`, embed `app.manifest` into the executable so
//!     the app is Per-Monitor DPI V2 aware and requests no elevation.
//!
//! Why link-args instead of a `.rc` resource compiler: `link.exe` accepts
//! `/MANIFESTINPUT:` directly, which removes the need for an external resource
//! compiler (and its Visual Studio dependency) from the offline build. The flag
//! is appended after rustc's own `/MANIFEST:NO`, so it wins.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=ui/appwindow.slint");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/appwindow.slint", config)
        .expect("failed to compile ui/appwindow.slint");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    )
    .join("app.manifest");

    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        manifest.display()
    );
    // No compatibility warnings about missing UAC data in the log.
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");

    // Fail loudly at build time rather than shipping a silently DPI-unaware app.
    if !manifest.is_file() {
        panic!("app.manifest is missing at {}", manifest.display());
    }
}

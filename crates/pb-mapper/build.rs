//! With `desktop`, the remote desktop's macOS binding links ScreenCaptureKit
//! strongly, which macOS before 12.3 lacks: this crate's examples and tests
//! link it weakly, so they still start there. A binary of a crate that
//! depends on this one passes `-C link-arg=-Wl,-weak_framework,ScreenCaptureKit`
//! itself to do the same; a build script can only link its own package.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let macos = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos");
    if macos && std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }
}

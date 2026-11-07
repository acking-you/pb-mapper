//! Windows N and KN without the Media Feature Pack, and Windows Server without
//! the feature, have no mfplat.dll. Delay-loaded, it no longer stops this DLL
//! from loading there; pb-mapper-desktop checks `media_foundation()` first.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        println!("cargo:rustc-link-arg-cdylib=/DELAYLOAD:mfplat.dll");
        println!("cargo:rustc-link-arg-cdylib=delayimp.lib");
    }
}

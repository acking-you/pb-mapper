//! macOS before 12.3 has no ScreenCaptureKit, and its binding links the
//! framework strongly, so a binary that links this crate would not start
//! there, though it never calls the framework below 12.3 (classes are looked
//! up at run time, and `capture::available` checks the system first). This
//! crate's own tests, examples and benchmarks link it weakly. A build script
//! cannot do that for the binaries of the crates that depend on this one:
//! the `pb-mapper` facade does it for its examples and tests, the app's FFI
//! library for itself, and an SDK consumer that runs below macOS 12.3 passes
//! `-C link-arg=-Wl,-weak_framework,ScreenCaptureKit` to its own link.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");
    }
}

//! Explicit Windows ordinary-desktop input acceptance. Uses its own window,
//! local loopback relay and expiring credential by default. An explicit
//! --relay-config PATH uses a deployed relay with a new expiring test scope;
//! --tcp overrides its carrier and --consumer-server HOST:PORT selects an
//! already-configured consumer route (for example an existing SSH forward).
//! Through a v2 viewer that presents what it receives, it checks Unicode text,
//! text with a line break and a tab typed once each, physical keys typed by
//! the window's layout (and reports what its input method composes from
//! them), a click, a drag-start press on that spot that Windows must not merge
//! into a double click, and the release of that press and of a held physical
//! key when the viewer disconnects.
//! Run: cargo run -p pb-mapper-desktop --example input_acceptance -- --run-owned-window
//! Do not interact with the desktop during the short run. Foreground/geometry
//! changes permanently abort new input. Cleanup still releases held input.
#[cfg(windows)]
mod acceptance;
#[cfg(windows)]
mod fixture;
#[cfg(windows)]
mod guarded;
#[cfg(windows)]
#[path = "../probe_support/mod.rs"]
mod probe_support;

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--run-owned-window") {
        println!("Use --run-owned-window for the explicit local SDK/native input acceptance.");
        return Ok(());
    }
    acceptance::run()
}
#[cfg(not(windows))]
fn main() {
    println!("This explicit native-input acceptance requires Windows.");
}

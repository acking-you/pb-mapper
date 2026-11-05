//! Privileged capture/input and explicitly authorized background desktop hosting.
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    #[cfg(windows)]
    if let Err(error) = pb_mapper_desktop::helper::run() {
        eprintln!("desktop helper: {error}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("The desktop helper requires Windows");
        std::process::exit(1);
    }
}

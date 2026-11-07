//! Read-only host capability/capture probe. No listener or input injection.
use pb_mapper_desktop::{capabilities, displays, native_backend};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string(&capabilities())?);
    for display in displays()? {
        println!("{}", serde_json::to_string(&display)?);
        let mut backend = native_backend(display.id)?;
        let start = std::time::Instant::now();
        let mut bytes = 0;
        for _ in 0..10 {
            bytes += backend.capture()?.len();
        }
        println!(
            "10 captures: elapsed_ms={}, total_jpeg_bytes={bytes}",
            start.elapsed().as_millis()
        );
    }
    Ok(())
}

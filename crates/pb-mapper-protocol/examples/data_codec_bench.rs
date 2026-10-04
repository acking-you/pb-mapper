//! Release-mode comparison of legacy and v2 setup and steady-state encryption.
//! Run with `cargo run --release -p pb-mapper-protocol --example data_codec_bench`.

use std::{error::Error, hint::black_box, time::Instant};

use pb_mapper_core::codec::{Decryptor, Encryptor};
use pb_mapper_protocol::{data::DataCodec, secure::HeaderProtocol};

fn measure(v2: bool, size: usize, iterations: usize) -> Result<f64, Box<dyn Error>> {
    let codec = if v2 {
        DataCodec::negotiate([42; 32], Some(2), HeaderProtocol::V2)?
    } else {
        DataCodec::legacy([42; 32])
    };
    if size == 0 {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(black_box(codec).endpoint_codecs()?);
        }
        return Ok(start.elapsed().as_secs_f64() * 1e9 / iterations as f64);
    }
    let (_, mut writer) = codec.endpoint_codecs()?;
    let (mut reader, _) = codec.relay_codecs()?;
    let mut bytes = vec![42; size + 16];
    let start = Instant::now();
    for _ in 0..iterations {
        let tag = writer
            .encrypt(black_box(&mut bytes[..size]))
            .map_err(|_| "encrypt failed")?;
        bytes[size..].copy_from_slice(tag.as_ref());
        black_box(
            reader
                .decrypt(black_box(&mut bytes))
                .map_err(|_| "decrypt failed")?,
        );
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(bytes[..size].iter().all(|&byte| byte == 42));
    Ok(elapsed * 1e9 / iterations as f64)
}

fn main() -> Result<(), Box<dyn Error>> {
    for (size, iterations) in [(0, 200_000), (256, 500_000), (16_384, 20_000)] {
        let mut samples = [Vec::new(), Vec::new()];
        // Warm both paths, then alternate order to limit clock/thermal bias.
        for v2 in [false, true] {
            black_box(measure(v2, size, iterations / 10)?);
        }
        for round in 0..9 {
            for index in [round % 2, 1 - round % 2] {
                samples[index].push(measure(index == 1, size, iterations)?);
            }
        }
        for sample in &mut samples {
            sample.sort_by(f64::total_cmp);
        }
        let legacy = samples[0][4];
        let v2 = samples[1][4];
        println!(
            "{{\"payload_bytes\":{size},\"iterations\":{iterations},\"samples\":9,\"legacy_median_ns\":{legacy:.2},\"v2_median_ns\":{v2:.2},\"change_percent\":{:.2}}}",
            (v2 / legacy - 1.0) * 100.0,
        );
    }
    Ok(())
}

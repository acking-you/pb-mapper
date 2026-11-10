//! Release-mode cost of the framed paths, per frame: write frames into memory,
//! then read them back.
//!
//! - `process`: plain frames keyed by the process credential, as
//!   `get_header_msg_reader`/`get_header_msg_writer` frame them.
//! - `session`: plain frames keyed by a session key, as every relay and tunnel
//!   leg keys them.
//! - `datagram`: encrypted datagrams, as a codec UDP leg sends them.
//! - `stream`: encrypted stream chunks, as a codec TCP leg forwards them.
//!
//! Run with `cargo run --release -p pb-mapper-protocol --example framing_bench`.

use std::{error::Error, hint::black_box, time::Instant};

use pb_mapper_core::checksum::set_process_msg_header_key;
use pb_mapper_core::test_support::{TEST_ADMIN_KEY, TEST_ADMIN_KEY_BYTES};
use pb_mapper_protocol::data::DataCodec;
use pb_mapper_protocol::forward::{
    CodecDatagramReader, CodecDatagramWriter, CodecForwardReader, CodecForwardWriter,
    DatagramReader, DatagramWriter, ForwardReader, ForwardWriter,
};
use pb_mapper_protocol::secure::HeaderProtocol;
use pb_mapper_protocol::{MessageReader, MessageWriter, NormalMessageReader, NormalMessageWriter};

/// Frames per sample: at most this many, and at most 32 MiB of them.
const FRAMES: usize = 20_000;
const SAMPLE_BYTES: usize = 32 << 20;
const SAMPLES: usize = 9;

#[derive(Clone, Copy)]
enum Path {
    Process,
    Session,
    Datagram,
    Stream,
}

impl Path {
    fn name(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Session => "session",
            Self::Datagram => "datagram",
            Self::Stream => "stream",
        }
    }
}

/// Nanoseconds per frame to write a sample of frames of `size` bytes, then
/// to read them back.
async fn measure(
    path: Path,
    size: usize,
    wire: &mut Vec<u8>,
) -> Result<(f64, f64), Box<dyn Error>> {
    let frames = FRAMES.min(SAMPLE_BYTES / size);
    let payload = vec![0x5a; size];
    let codec = DataCodec::negotiate([42; 32], Some(2), HeaderProtocol::V2)?;
    let (_, encoder) = codec.endpoint_codecs()?;
    let (decoder, _) = codec.relay_codecs()?;
    let key = TEST_ADMIN_KEY_BYTES;
    wire.clear();
    let start = Instant::now();
    match path {
        Path::Process | Path::Session => {
            let mut writer = NormalMessageWriter::new(wire);
            if matches!(path, Path::Session) {
                writer = writer.with_checksum_key(key);
            }
            for _ in 0..frames {
                writer.write_msg(black_box(&payload)).await?;
            }
        }
        Path::Datagram => {
            let mut writer = CodecDatagramWriter::new(wire, encoder).with_checksum_key(key);
            for _ in 0..frames {
                writer.send(black_box(&payload)).await?;
            }
        }
        Path::Stream => {
            let mut writer = CodecForwardWriter::new(wire, encoder).with_checksum_key(key);
            for _ in 0..frames {
                writer.write(black_box(&payload)).await?;
            }
        }
    }
    let write = start.elapsed().as_secs_f64();
    let mut input = wire.as_slice();
    let start = Instant::now();
    match path {
        Path::Process | Path::Session => {
            let mut reader = NormalMessageReader::new(&mut input);
            if matches!(path, Path::Session) {
                reader = reader.with_checksum_key(key);
            }
            for _ in 0..frames {
                black_box(reader.read_msg().await?);
            }
        }
        Path::Datagram => {
            let mut reader = CodecDatagramReader::new(&mut input, decoder).with_checksum_key(key);
            for _ in 0..frames {
                black_box(reader.recv().await?);
            }
        }
        Path::Stream => {
            let mut reader = CodecForwardReader::new(&mut input, decoder).with_checksum_key(key);
            for _ in 0..frames {
                black_box(reader.read().await?);
            }
        }
    }
    let read = start.elapsed().as_secs_f64();
    assert!(input.is_empty(), "every frame was read back");
    let per_frame = |seconds: f64| seconds * 1e9 / frames as f64;
    Ok((per_frame(write), per_frame(read)))
}

fn median(samples: &mut [f64]) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn main() -> Result<(), Box<dyn Error>> {
    set_process_msg_header_key(Some(TEST_ADMIN_KEY))?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(async {
        let mut wire = Vec::with_capacity(SAMPLE_BYTES + FRAMES * 24);
        let cases = [
            (Path::Process, 16),
            (Path::Process, 1024),
            (Path::Session, 16),
            (Path::Session, 1024),
            (Path::Datagram, 64),
            (Path::Datagram, 1200),
            (Path::Stream, 1024),
            (Path::Stream, 8192),
        ];
        for (path, size) in cases {
            // Warm up, then take the median of the samples.
            measure(path, size, &mut wire).await?;
            let (mut writes, mut reads) = (Vec::new(), Vec::new());
            for _ in 0..SAMPLES {
                let (write, read) = measure(path, size, &mut wire).await?;
                writes.push(write);
                reads.push(read);
            }
            println!(
                "case={}-{size}B write_ns={:.2} read_ns={:.2}",
                path.name(),
                median(&mut writes),
                median(&mut reads)
            );
        }
        Ok(())
    })
}

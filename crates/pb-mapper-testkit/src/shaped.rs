//! A TCP hop that holds bytes for a delay and limits their rate, adjustable
//! while connections run.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// How a [`shaped_link`] shapes its traffic, in both directions alike.
#[derive(Default)]
pub struct Shape {
    /// Every byte is held this long.
    pub delay_ms: AtomicU64,
    /// Bytes per second; 0 is unlimited.
    pub rate: AtomicU64,
}

/// A TCP hop in front of `relay`, shaped by `shape` while connections run.
/// Each direction queues at most 256 reads, so the sender's own flow control
/// bounds what is in flight. Bytes stay in order. A connection the relay
/// refuses is closed, and the hop goes on accepting, as a network would.
pub async fn shaped_link(relay: SocketAddr, shape: Arc<Shape>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((near, _)) = listener.accept().await {
            let Ok(far) = tokio::net::TcpStream::connect(relay).await else {
                continue;
            };
            near.set_nodelay(true).unwrap();
            far.set_nodelay(true).unwrap();
            let (near_read, near_write) = near.into_split();
            let (far_read, far_write) = far.into_split();
            tokio::spawn(shaped_copy(near_read, far_write, shape.clone()));
            tokio::spawn(shaped_copy(far_read, near_write, shape.clone()));
        }
    });
    addr
}

async fn shaped_copy(mut read: OwnedReadHalf, mut write: OwnedWriteHalf, shape: Arc<Shape>) {
    let (send, mut receive) = tokio::sync::mpsc::channel::<(tokio::time::Instant, Vec<u8>)>(256);
    let incoming = async move {
        loop {
            let mut bytes = vec![0; 16 * 1024];
            match read.read(&mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(size) => bytes.truncate(size),
            }
            if send
                .send((tokio::time::Instant::now(), bytes))
                .await
                .is_err()
            {
                break;
            }
        }
    };
    let outgoing = async move {
        // When the link is free to carry the next packet.
        let mut free = tokio::time::Instant::now();
        while let Some((read_at, bytes)) = receive.recv().await {
            let delay = Duration::from_millis(shape.delay_ms.load(Ordering::Relaxed));
            tokio::time::sleep_until(read_at + delay).await;
            for packet in bytes.chunks(4096) {
                let rate = shape.rate.load(Ordering::Relaxed);
                if rate > 0 {
                    free = free.max(tokio::time::Instant::now());
                    tokio::time::sleep_until(free).await;
                    free += Duration::from_secs_f64(packet.len() as f64 / rate as f64);
                }
                if write.write_all(packet).await.is_err() {
                    return;
                }
            }
        }
        let _ = write.shutdown().await;
    };
    tokio::join!(incoming, outgoing);
}

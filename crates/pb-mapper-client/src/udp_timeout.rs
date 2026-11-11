//! The UDP session timeout, which uni-stream keeps for the whole process.
//!
//! uni-stream 0.3.5 keeps it in a `static mut` that it reads once, unguarded,
//! when the process makes its first UDP stream, and keeps for every UDP
//! stream after. A connect tunnel writes 120 s there as it starts; a
//! publisher's UDP sessions take uni-stream's 20 s unless a connect tunnel
//! started first. Written on one thread while another made the first UDP
//! stream, it could be read half written: as no time at all, among others.
//!
//! So the write, and every UDP stream this crate makes, go through one
//! [`Gate`]: a connect tunnel as it starts, a publisher as it dials its UDP
//! service. Whichever comes first decides, as it did, so each process keeps
//! the timeout it had; the write and the read are just never at once. A dial
//! that fails made no stream and decides nothing. A connect tunnel's own UDP
//! streams come from a listener it binds after.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;

/// A connect tunnel's UDP sessions end after this long without a datagram.
const CONNECT_UDP_TIMEOUT: Duration = Duration::from_secs(120);

static GATE: Gate = Gate::new();

/// Orders the one write of the timeout against what may be its first read.
struct Gate {
    /// Set once the timeout is written or a publisher's UDP stream is made:
    /// nothing writes it after.
    decided: AtomicBool,
    /// Held shared by each dial that may make the first stream, and alone by
    /// the write.
    undecided: RwLock<()>,
}

impl Gate {
    const fn new() -> Self {
        Self {
            decided: AtomicBool::new(false),
            undecided: RwLock::const_new(()),
        }
    }

    /// Runs `write` unless the timeout is decided, with no dial under way.
    async fn write_once(&self, write: impl FnOnce()) {
        if self.decided.load(Ordering::Acquire) {
            return;
        }
        let _alone = self.undecided.write().await;
        if !self.decided.load(Ordering::Acquire) {
            write();
            // Only now: whoever sees it decided skips the lock, and must not
            // do so while the write is still under way.
            self.decided.store(true, Ordering::Release);
        }
    }

    /// Runs `dial`, which decides the timeout if it makes a stream.
    async fn dial<T, E>(&self, dial: impl Future<Output = Result<T, E>>) -> Result<T, E> {
        if self.decided.load(Ordering::Acquire) {
            return dial.await;
        }
        let _shared = self.undecided.read().await;
        let stream = dial.await?;
        self.decided.store(true, Ordering::Release);
        Ok(stream)
    }
}

/// A connect tunnel is starting: UDP streams made from now on time out after
/// [`CONNECT_UDP_TIMEOUT`], unless a publisher of this process made one
/// first.
pub(crate) async fn connect_tunnel_starting() {
    GATE.write_once(|| uni_stream::udp::set_custom_timeout(CONNECT_UDP_TIMEOUT))
        .await;
}

/// A publisher's dial of its service. One of a UDP service may make the
/// process's first UDP stream, which reads the timeout: nothing writes it
/// meanwhile, or after a dial that made one.
pub(crate) async fn publisher_dial<T, E>(
    is_datagram: bool,
    dial: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    if is_datagram {
        GATE.dial(dial).await
    } else {
        dial.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A publisher's dial that made no stream read nothing, so a connect
    /// tunnel started after it still writes its timeout; one that made a
    /// stream has read it, and nothing writes after.
    #[tokio::test]
    async fn only_a_dial_that_made_a_stream_decides() {
        let gate = Gate::new();
        gate.dial(async { Err::<(), ()>(()) }).await.unwrap_err();
        let mut written = false;
        gate.write_once(|| {
            // Undecided until written, to whoever would skip the lock on it.
            assert!(!gate.decided.load(Ordering::Acquire));
            written = true;
        })
        .await;
        assert!(written);

        let gate = Gate::new();
        gate.dial(async { Ok::<(), ()>(()) }).await.unwrap();
        gate.write_once(|| panic!("written after a stream read it"))
            .await;
    }
}

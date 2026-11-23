//! The thread that serves a host's video, raised above the owner's ordinary
//! programs while it streams: thread C, and the helper's capture thread. When
//! other programs keep every core busy, its capture ticks, encoder submits
//! and input injection then need not wait for a core. Above normal on
//! Windows, the user-interactive QoS class on macOS (where the capture queue
//! it serves already runs), nothing elsewhere. This is about contention for
//! the CPU, not timer coalescing, which thread C's waiter handles on macOS
//! (`platform/macos/waiter.rs`).
//!
//! Only H.264 and H.265 raise it: the OS encodes those off this thread, which
//! coordinates. A JPEG is grabbed, scaled and compressed on this thread, and
//! that work stays at the owner's priority. Thread C is a blocking thread of
//! the embedder's runtime, so the guard puts back exactly what it found, on
//! the thread that raised it, as soon as the session stops capturing one of
//! those codecs and at the latest when it ends, before the thread returns to
//! the pool. Every call may fail harmlessly: the thread then keeps its
//! priority.

use crate::{VideoCodec, platform::priority};
use std::marker::PhantomData;

/// The calling thread raised while this lives. Not `Send`: only the thread
/// it raised can put its priority back.
pub(crate) struct ThreadPriority {
    /// What to put back. `None` raised nothing: the thread was already at
    /// or above the raised level, or the OS refused.
    before: Option<priority::Before>,
    _thread: PhantomData<*const ()>,
}
impl ThreadPriority {
    pub fn raise() -> Self {
        Self {
            before: priority::raise(),
            _thread: PhantomData,
        }
    }

    /// Whether the thread that serves `codec` is raised while it streams.
    pub fn raised_for(codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::H264 | VideoCodec::H265 => true,
            VideoCodec::Jpeg => false,
        }
    }
}
impl Drop for ThreadPriority {
    fn drop(&mut self) {
        if let Some(before) = self.before.take() {
            priority::restore(before);
        }
    }
}

#[cfg(all(test, any(windows, target_os = "macos")))]
mod tests {
    use super::ThreadPriority;
    use crate::platform::priority::testing::{HIGHER, LOWER, RAISED, current, set};

    /// From a level below the raised one, on a thread of its own: raised
    /// while the guard lives, and that exact level again once it drops.
    #[test]
    fn raises_while_held_and_restores_on_drop() {
        std::thread::spawn(|| {
            assert!(set(LOWER));
            let raised = ThreadPriority::raise();
            assert_eq!(current(), RAISED);
            drop(raised);
            assert_eq!(current(), LOWER);
        })
        .join()
        .unwrap();
    }

    /// A thread already at or above the raised level is never lowered, while
    /// the guard lives or after.
    #[test]
    fn never_lowers_a_thread_already_above() {
        std::thread::spawn(|| {
            assert!(set(HIGHER));
            let raised = ThreadPriority::raise();
            assert_eq!(current(), HIGHER);
            drop(raised);
            assert_eq!(current(), HIGHER);
        })
        .join()
        .unwrap();
    }
}

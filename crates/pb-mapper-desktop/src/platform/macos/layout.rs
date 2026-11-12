//! The Mac's keyboard layout, for the named letters and digits
//! ([`keys::Layout`]). macOS matches a shortcut on the layout key
//! equivalents use: the current ASCII-capable one, so a Russian layout's
//! Command-C is matched on the Latin layout behind it. That is the layout
//! read here.
//!
//! The Text Input Sources calls run on the main thread only. So the table is
//! read there, asynchronously, and published for thread C, which per key
//! loads one atomic and copies the table only when it changed:
//! - at every session's start, which waits for it at most `FIRST_READ`;
//! - again whenever the owner switches input source, which a distributed
//!   notification on the main run loop says.
//!
//! Until a read arrives, and in a host whose main thread runs no run loop
//! (a command-line SDK host, a test), every named key takes its ANSI place.
//! A read asked for while an earlier one waits on the main queue is not
//! asked again, so such a host waits once, not at every session.

use super::keys::Layout;
use objc2_core_foundation::{
    CFData, CFDictionary, CFNotificationCenter, CFNotificationSuspensionBehavior, CFRetained,
    CFString, CFType,
};
use parking_lot::Mutex;
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

/// How long a session's start waits for the main thread's read.
pub(crate) const FIRST_READ: Duration = Duration::from_millis(200);

/// `kUCKeyActionDisplay`, `kUCKeyTranslateNoDeadKeysMask`, and `shiftKey`
/// as `UCKeyTranslate` takes a modifier state (`>> 8`).
const DISPLAY: u16 = 3;
const NO_DEAD_KEYS: u32 = 1;
const SHIFT: u32 = 0x02;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: &CFString) -> *const c_void;
    fn UCKeyTranslate(
        layout: *const c_void,
        code: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead_keys: *mut u32,
        capacity: usize,
        length: *mut usize,
        text: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
    static kTISPropertyUnicodeKeyLayoutData: &'static CFString;
    static kTISNotifySelectedKeyboardInputSourceChanged: &'static CFString;
}

/// The layout last read; `None` before the first read, or where it could
/// not be read.
static PUBLISHED: Mutex<Option<Layout>> = parking_lot::const_mutex(None);
/// Moves at every read published.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// A read waits on the main queue.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The input source notification is observed.
static WATCHING: AtomicBool = AtomicBool::new(false);

/// How many reads were published: a change says to copy [`current`] again.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// The layout to press named keys through.
pub(crate) fn current() -> Layout {
    PUBLISHED.lock().unwrap_or_else(Layout::ansi)
}

/// Ask the main thread to read the layout, waiting for it at most `wait`.
/// Never longer: an earlier read still waiting on the main queue is not
/// asked again, and not waited for.
pub(crate) fn request(wait: Duration) {
    if ASKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let (read, done) = mpsc::sync_channel(1);
    dispatch2::DispatchQueue::main().exec_async(move || {
        publish();
        ASKED.store(false, Ordering::Release);
        let _ = read.send(());
    });
    let _ = done.recv_timeout(wait);
}

/// On the main thread: read and publish the layout, and from the first read
/// on, watch the owner's input source.
fn publish() {
    let layout = read();
    *PUBLISHED.lock() = layout;
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(center) = CFNotificationCenter::distributed_center() {
        // SAFETY: a static callback and a static name; the observer is an
        // address that only identifies it, and it is never removed.
        unsafe {
            center.add_observer(
                (&raw const WATCHING).cast(),
                Some(switched),
                Some(kTISNotifySelectedKeyboardInputSourceChanged),
                std::ptr::null(),
                CFNotificationSuspensionBehavior::DeliverImmediately,
            );
        }
    }
}

/// The owner switched input source: on the main run loop.
unsafe extern "C-unwind" fn switched(
    _: *mut CFNotificationCenter,
    _: *mut c_void,
    _: *const CFString,
    _: *const c_void,
    _: *const CFDictionary,
) {
    publish();
}

/// What the current ASCII-capable layout's keys type, on the main thread;
/// `None` where it has no Unicode key layout.
fn read() -> Option<Layout> {
    // SAFETY: on the main thread; a Create-rule source, released when the
    // wrapper drops.
    let source: CFRetained<CFType> = unsafe {
        CFRetained::from_raw(NonNull::new(
            TISCopyCurrentASCIICapableKeyboardLayoutInputSource().cast(),
        )?)
    };
    // SAFETY: a Get-rule property of `source`, which outlives its use here.
    let data = unsafe {
        TISGetInputSourceProperty(
            CFRetained::as_ptr(&source).as_ptr().cast(),
            kTISPropertyUnicodeKeyLayoutData,
        )
    };
    // SAFETY: the property is a CFData or null.
    let data = unsafe { data.cast::<CFData>().as_ref() }?;
    let layout = data.byte_ptr().cast::<c_void>();
    // SAFETY: reads the keyboard type.
    let keyboard = u32::from(unsafe { LMGetKbdType() });
    let typed = |code: u16, shifted: bool| {
        let (mut dead_keys, mut length, mut text) = (0, 0, [0u16; 4]);
        // SAFETY: `layout` is the source's key layout data, alive while
        // `source` is; the outputs are locals of the stated sizes.
        let status = unsafe {
            UCKeyTranslate(
                layout,
                code,
                DISPLAY,
                if shifted { SHIFT } else { 0 },
                keyboard,
                NO_DEAD_KEYS,
                &mut dead_keys,
                text.len(),
                &mut length,
                text.as_mut_ptr(),
            )
        };
        (status == 0 && length == 1)
            .then(|| char::from_u32(u32::from(text[0])))
            .flatten()
    };
    let read = Layout::of(typed);
    drop(source);
    Some(read)
}

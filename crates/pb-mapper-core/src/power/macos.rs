use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

pub(super) struct Activity(Retained<ProtocolObject<dyn NSObjectProtocol>>);
// SAFETY: NSProcessInfo accepts its opaque activity token back on any thread.
unsafe impl Send for Activity {}

impl Activity {
    pub(super) fn new(reason: &str) -> Self {
        objc2::rc::autoreleasepool(|_| {
            // UserInitiated prevents App Nap and idle system sleep, including
            // on battery. Keeping the display lit is a separate option.
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiated,
                &NSString::from_str(reason),
            );
            Self(token)
        })
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: the matching begin returned this token, ended once.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) }
        });
    }
}

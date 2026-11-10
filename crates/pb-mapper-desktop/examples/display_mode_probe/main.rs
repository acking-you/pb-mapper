//! The display-mode acceptance (contract §10.5 item 4) on the computer this
//! runs on. It CHANGES THIS COMPUTER'S DISPLAY MODE, dynamically (never in
//! the registry), and puts it back: run it only where the computer's owner
//! agreed, or in the VM of `scripts/test/windows-helper-acceptance.md`.
//!
//! ```text
//! display_mode_probe --list
//! display_mode_probe --restore [--state-dir DIR]
//! display_mode_probe <relay-config.json> [--fit WxH] [--state-dir DIR]
//!                    [--owner-change] [--fullscreen-check]
//! ```
//!
//! `--list` prints every display's mode and changes nothing. `--restore`
//! replays the probe's journal, then puts any display that does not show its
//! registry mode back to it.
//!
//! The configuration is the `deployed` example's (`server_address`,
//! `msg_header_key`, `relay_transport`): use a local relay with a throwaway
//! key. The host allows control, which a mode change needs; the probe sends
//! no input, and has no call that could. Its journal lives in `--state-dir`
//! (by default a directory in the temporary directory), never in the app's.
//! A journal an earlier, interrupted run left is replayed first, and the run
//! refuses to start while one cannot be put back; only then is every
//! display's mode recorded. The cases, after each of which the display must
//! be back at its recorded mode:
//!
//! 1. Fit local, then Original;
//! 2. Fit local, then the viewer disconnects;
//! 3. Fit local, then the host revokes control;
//! 4. Fit local on a host in a child process, which is killed; a host
//!    started again puts the display back from the journal.
//!
//! Fit local asks for `--fit` (1920x1080 by default). It must change the
//! mode: by default, on a display that 1920x1080 would leave as it is, the
//! probe fits the largest smaller mode the display offers instead, and an
//! explicit `--fit` that changes nothing fails. A pass always means a mode
//! was changed and put back.
//!
//! `--owner-change` adds a fifth: Fit local, then the probe changes the mode
//! itself as the owner would, and the viewer disconnects; the host must
//! leave the owner's mode. `--fullscreen-check` asks whether Windows puts a
//! `CDS_FULLSCREEN` change back when the process that made it exits
//! (contract R13). Neither is part of a run on a computer whose owner agreed
//! only to the five documented paths.
//!
//! At the end, after any failure, and on Ctrl-C, the child host is killed
//! and every display is put back to what was recorded. While that cannot be
//! done (the lock screen or a UAC prompt shows), the probe waits for the
//! ordinary desktop, trying every 2 s for up to ten minutes, and says what
//! is pending and how to run `--restore`. Its last lines are the modes
//! after, the input messages it sent (0), and `display_mode_probe passed`,
//! or the run fails with what went wrong.

#[cfg(windows)]
mod modes;
#[cfg(windows)]
mod probe;
#[cfg(windows)]
#[path = "../probe_support/mod.rs"]
mod probe_support;

#[cfg(not(windows))]
fn main() {
    eprintln!("display_mode_probe changes a Windows display's mode; it runs on Windows only");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(probe::run())
}

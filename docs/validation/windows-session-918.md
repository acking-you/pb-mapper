# Windows session startup repair (build 918)

2026-11-06, Windows 11 Enterprise 10.0.26300, Rust 1.99.1,
Flutter 3.44.9. The reported Android symptom was repeated
`Connection reset by peer (os error 104)` before receiving a desktop.

## Correlated evidence

- The installed 917 helper was Running as LocalSystem. Its UI reported
  `waiting_for_owner_session`; no capture worker existed.
- The Windows session APIs reported active console 1, `WTSActive`, unlocked,
  with a nonempty user and valid login time. The returned extended-session
  structure was the expected 232 bytes.
- Read-only SSH diagnostics on the deployed relay showed CLI 0.9.8,
  `active/running`, and a zero systemd restart count for that process. Logs
  during the phone's attempts repeatedly reported `subscribe_key_missing`:
  `desktop:my-computer` was not registered. No relay deployment was changed.
- The service's `AdjustTokenPrivileges` call supplied `PreviousState` but a
  null `ReturnLength`. An isolated duplicate of the local user's token reproduced
  failure with Win32 error 998 (`ERROR_NOACCESS`). Supplying the output pointer
  succeeded with error 0 and returned length 16. This probe did not change the
  current process token or installed service token.

The service could not enter its temporary privilege scope to query the owner
login. The supervisor discarded that error with `.ok()`, classified it as an
absent owner, and never registered the desktop endpoint. Retrying on the phone
could not create that missing endpoint. Upgrading the relay alone cannot fix it.

## Change and verification

The privilege scope supplies the required output pointer, captures the last
error immediately, and preserves scoped restoration. Both session checking and
worker launch use this path. The owner-session query now distinguishes a missing
or different login (`Ok(None)`) from Windows API failures. The supervisor cancels
unverified active/pending hosting, keeps the saved authorization, reports the
failure and continues retrying. It never relaxes owner/session identity checks.

[AdjustTokenPrivileges documentation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-adjusttokenprivileges)
specifies when `ReturnLength` may be null and how to restore `PreviousState`.

- `cargo fmt --all -- --check`: passed.
- Desktop/FFI all-target/all-feature Clippy with warnings denied: passed.
- `cargo test -p pb-mapper-desktop --all-features --lib helper:: -- --nocapture`:
  12 passed. The added privilege test uses an independent token: enable succeeds,
  dropping the scope restores the disabled state, and a later partial-scope
  failure also restores it. The added supervisor test cancels a pending start on
  query failure without claiming logout or losing the saved grant; actual absence
  still yields the waiting state.
- Windows release FFI/helper and the normal Flutter application compiled. The
  complete `0.10.2+918` bundle passed ZIP CRC and component hash checks, and its
  executable started with the bundled new FFI. Launching this portable UI does
  not itself upgrade the installed privileged helper.

The privileged installed service still needs local installation of the new
helper. Actual service-owned capture, UAC/lock-screen input and resumed phone
streaming have not yet been observed with this build. The Android 914 APK remains
compatible; this repair is entirely on the Windows host.

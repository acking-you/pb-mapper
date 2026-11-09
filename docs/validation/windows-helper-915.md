# Issue #50 Windows helper identity repair (build 915)

2026-11-06, Windows 11 Enterprise 10.0.26300, Rust 1.99.1,
Flutter 3.44.9. This fixes the reported `0x80070005` before desktop sharing.

## Reproduction and cause

Read-only checks against the user's installed build 914 found:

- `PbMapperDesktop` was Running as LocalSystem, with automatic startup.
- Both the capture and background-management named pipes could be opened with
  the production client access mask; the kernel reported the service PID.
- `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` for that PID failed with
  Win32 error 5, while the same query for the protected UI succeeded.

The UI therefore could not verify the pipe server's protected executable path.
The readiness card checked installation, SCM state and caller authorization,
but omitted that live process check, incorrectly reporting Ready.

## Repair

Before publishing Running, the helper merges an ACE granting only
`PROCESS_QUERY_LIMITED_INFORMATION` to its configured owner into its existing
process DACL. Existing ACL entries remain. The UI still checks the kernel pipe
PID against SCM and verifies the protected image while holding a process
handle. No pipe-authentication check is removed.

The status API now reports `identity_verified` using the same image-query
routine. Capture and background-management admission require it. The preparation
card offers Install / update when it fails, including in the new portable
bundle, instead of redirecting the user to the broken old protected app.

Windows documents the distinction between process query and control rights in
[Process Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights).
The implementation uses the OS [SetEntriesInAclW](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setentriesinaclw)
merge operation rather than replacing the service ACL with a hand-built one.

## Verification

- `cargo fmt --all -- --check` passed.
- Focused desktop/FFI all-target/all-feature Clippy with warnings denied passed.
- `cargo test -p pb-mapper-desktop --all-features --lib helper:: -- --nocapture`:
  10 passed. The new test restricts only an isolated child process, reproduces
  access denied, runs the production ACL merge and verifies image querying
  succeeds. Process termination, full query, memory read/write, thread creation,
  handle duplication and synchronization remain denied. No installed-service
  ACL is changed by the test.
- `cargo test -p pb-mapper-ffi --all-features --lib desktop::tests::`: seven
  passed, including rejection before management IPC when identity is unknown
  while preserving a previously enabled background grant.
- Flutter analyzer passed; `desktop_host_readiness_test.dart` and
  `desktop_test.dart`: six passed. The failed identity check disables full
  sharing and offers repair; verified identity enables the normal flow.
- Fresh release FFI queried the still-installed old service via the product
  capabilities API: `installed=true`, `running=true`, `identity_verified=false`,
  with an explicit process-identity/update error containing `0x80070005`.
- Windows x64 release FFI, helper and the normal Flutter application compiled;
  the executable reports `0.10.2+915`.

Build 915 changes the Windows host and its setup UI. The existing build 914
Android APK remains compatible. The installed privileged service has not been
replaced during these checks: upgrading needs the local Windows consent flow.
Actual UAC clicks, lock-screen password input and service-hosted capture remain
unverified; the isolated permission test does not establish those outcomes.

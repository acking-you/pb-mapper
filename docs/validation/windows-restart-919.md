# Windows helper installation restart — build 919

The previous installation flow copied the application into a protected release
directory, then offered a separate **Open installed app** action. That spawned
another UI and left the original one open and unable to manage the helper.

Build 919 replaces that flow with **Install / update and restart app**. A
successful installer exit is followed by an unelevated restart worker from the
new protected bundle. It validates installation ownership, opens the original
UI process handle, and acknowledges readiness over its inherited output pipe.
Only then does the UI exit. The worker waits for that exact process to exit,
revalidates the installed owner and files, and launches the UI on its desktop
page. Readiness is bounded to ten seconds; exit waiting is bounded to one minute.
Preparation failure or cancelled installation keeps the original UI open.

No password, invitation, relay credential or arbitrary executable path crosses
the restart command line. Existing protected-file and service IPC checks remain
in place. Saved settings and the separate background service grant survive the
UI restart. Service-owned sharing no longer disables helper update maintenance.
Temporary UI-owned sharing still needs to stop before installation.

## Verification

Windows 11, Rust 1.99.1, Flutter 3.44.9:

- Focused desktop/FFI Clippy with all features and targets: passed.
- Helper unit tests: 13 passed, including the actual Windows process wait,
  failed acknowledgement, owner/session checks and scoped privilege restoration.
- Flutter analyzer: passed.
- Four Flutter test files: 25 passed. Coverage includes cancellation, repeated
  installation clicks, restart completion, startup navigation, desktop lifecycle
  and recovery. An earlier invocation included a nonexistent test filename; the
  corrected invocation above passed.
- Windows Release FFI, helper and Flutter app: built successfully.
- Release helper smoke probe: the real unelevated executable verified the local
  protected owner, emitted its ready byte and remained waiting for the original
  process. The owned probe worker was terminated before that process exited;
  the installed service was not changed by this probe.
- Packaged archive passed ZIP CRC validation and hashes of all four principal
  native binaries matched the staged bundle.
- The previous issue-50 test UI was stopped and build 919 launched with
  `--desktop`. Read-only UI inspection confirmed the desktop page, saved relay
  and authorization metadata, and the single install/update/restart action.

The installed service was not upgraded during this verification. Windows showed
a firewall permission prompt on launch; it was not automated. The complete
administrator-consent → service update → automatic UI replacement sequence and
actual phone/UAC control remain pending local OS acceptance. This is not a claim
that issue #50 is complete. Build 919 also includes the session-query repair
documented in [build 918 validation](windows-session-918.md); Android build 914
and the original private invitation remain compatible.

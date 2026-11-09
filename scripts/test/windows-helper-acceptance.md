# Windows VM helper acceptance

Run this on an isolated Windows 11 VM with its normal console, one existing
administrator account and one existing standard account. Keep the original UAC
policy. Use the complete normal Windows ZIP; the helper, UI and FFI must belong
to the same build. A VM needs a working Windows capture/encoder path before it
can establish end-to-end helper video acceptance; record any missing virtual
GPU/codec support as a separate blocker.

`windows-helper-acceptance.ps1` only collects evidence. It never installs or
starts a service, elevates, changes policy, locks the session, records credentials
or injects input. `Baseline`, `Installed`, `Disabled` and `Uninstalled` refuse a
physical or unrecognized machine. Its JSON is **not** a passing test result.

## Prepare and collect

From ordinary 64-bit PowerShell in the repository on the host:

```powershell
./scripts/test/windows-helper-acceptance.ps1 -Stage HostInventory `
  -BundleZip ./target/issue50-preview/pb-mapper-windows-x64.zip `
  -OutputDirectory ./target/issue50-vm-acceptance
```

Use an already-permitted script execution environment. If the configured
PowerShell execution policy rejects the script, record that failure without
changing the policy or using an execution-policy bypass.

Transfer the ZIP and collector/runbook through the VM's existing supported
shared-folder or guest-agent channel. Do not create administrator access or
retrieve passwords for this test. Before testing, take a VM checkpoint using
already-authorized VM management. Run the following in the VM's signed-in
console session, with output beneath a dedicated test directory:

```powershell
./windows-helper-acceptance.ps1 -Stage Baseline `
  -BundleZip ./pb-mapper-windows-x64.zip -OutputDirectory ./evidence
```

Record the VM name, hypervisor, test build/ZIP SHA-256, guest OS build, account
type, controller OS/build, display resolution, transport and start/end times in
`observations.md`. Keep relay credentials and desktop invitations out of the
evidence. The collector records current policy values; a missing registry value
is unknown, not proof of a default. The expected Windows defaults are
`EnableLUA=1`, `PromptOnSecureDesktop=1`, `ConsentPromptBehaviorAdmin=5`, and
`ConsentPromptBehaviorUser=3`.
[Microsoft's UAC policy reference](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/settings-and-configuration/)
documents these settings. Do not change them to make a test pass.

## Acceptance matrix

Mark each row `PASS`, `FAIL` or `NOT RUN`, with timestamps and actual evidence.
The human responsible for the VM deliberately handles any OS consent or
credential prompt. Automation must not click consent, enter passwords, disable
secure desktop, or bypass an authorization boundary. A credential-prompt test can
be cancelled without revealing or recording a password.

| Case | Action in the VM / controller | Required observation |
| --- | --- | --- |
| Ordinary baseline | Start the portable UI, leave helper disabled, share view-only, then explicitly enable and request control. | Current frames and intended ordinary-window input work. View-only never admits input. Record codec/capture failures independently. |
| Initial installation | In the normal UI choose Install / update and restart app; local human deliberately completes Windows consent. | Installer returns success. `Installed` evidence shows `PbMapperDesktop` Running, LocalSystem, the protected release executable, and configuration owner matching the intended sharing user. |
| Protected app | Open the installed app from the UI; enable the helper and share. Try helper opt-in from the original portable app too. | Installed app remains unprivileged and can use the service. Portable app cannot impersonate the authorized executable. Record process session IDs and protected ACLs; the collector does not itself prove ACL correctness. |
| Administrator UAC desktop | From the VM console request an elevation that actually prompts under default policy. A human may cancel it after observation. | Helper-enabled viewer shows live secure-desktop frames, not a stale Default desktop. A click queued before the switch is not applied on the prompt. Record transition/recovery and errors. No automated consent. |
| Standard-user credential desktop | Sign into the existing standard account. Install/authorize for this account using the UI and deliberate local administrator authentication; use the installed app. Request a real elevation. | The viewer shows the credential prompt without storing/recording a password. Cancel locally. The account has not become an administrator and the UI remains unprivileged. |
| Helper video codec | With the helper sharing, connect a viewer that decodes H.265 (codec Auto), then one set to H.264, and switch the console to UAC and back during each. | The first session's quality panel reports H.265 where the VM's GPU encodes it (the worker probes it) and H.264 otherwise; the second reports H.264. Video and the pointer continue across the secure desktop in both. Record the GPU and the codec of each session. |
| No-helper secure desktop | Repeat the prompting operation with helper opt-in disabled. | Viewer clearly reports an inaccessible/stale desktop and admits no input; cached ordinary-desktop frames do not appear live. |
| Secure-desktop input | With a human operating the controller, exercise harmless navigation or Cancel on a real prompt; do not approve elevation or type credentials. | Observe actual secure-desktop input behavior and recovery. Merely displaying the prompt is not input acceptance. |
| Held-input release | In a disposable text/drawing window hold a modifier or drag using the controller, then separately end control, revoke input, disconnect, and stop sharing. | The target no longer has held keys/buttons, no delayed input is replayed, and viewing/control state matches the selected action. Record each boundary separately. |
| Same-owner lock and password entry | Enable the authorized helper, hold input in a disposable target, then lock the VM using its local console. The human controller requests fresh control on the current lock-screen frame and enters the test account's password without recording it. | Old input is released at the transition. The real lock screen is visible and the human can unlock the same login. Record lock/Winlogon transitions and recovery; do not infer this from UAC viewing. Automation never enters the password. |
| Logoff / user switch | With the helper active, locally log off or switch to a different existing account. | Capture and input stop for the old login. Neither a recycled session ID nor a different user inherits its grant. Background hosting waits for the authorized owner's login and requires a fresh viewer/control session. |
| Background UI exit | Explicitly enable Windows background hosting, then close the entire UI process. Reopen the installed UI. | The service keeps the same invitation and active host; the reopened UI shows its actual state and can stop/revoke it. This must work beyond merely hiding a tray window. |
| Background restart / sleep | With background auto-start enabled, restart the service/VM using the authorized local management path; separately test sleep followed by local wake. | Hosting waits for the approved user's login, then recovers without a UI process. It works again after wake; no claim that S3/S4 hosts serve frames while asleep. No old input is replayed. |
| Background stop / revoke | Stop sharing, restart the service, then repeat with invitation revocation. | Stop prevents automatic restart. Revoke destroys the stored grant, and the old invitation cannot reconnect. A persistence failure is visible and does not silently re-enable input. |
| Helper disable | Use the UI's Disable helper action with deliberate OS consent, then collect `Disabled`. | Service is Stopped and startup Disabled; helper-backed sharing fails clearly and no worker remains. |
| Helper re-enable / crash recovery | Explicitly re-enable a disabled helper in the UI. In the disposable VM, separately simulate a service failure using its already-authorized management path. | Re-enable restores automatic startup; abnormal failures use 5/15/60-second SCM restart delays. Normal Stop/Disable does not resurrect the host. Record whether a normal OS restart was needed for the non-crash flag. |
| Update / reinstall | Install the same complete build again through the UI; collect `Installed`. Repeat after explicitly disabling the service, and once over a helper of the previous protocol (6, `.v6` pipes). | A previously running service returns Running and the new protected app can share; a disabled/stopped service stays disabled/stopped until explicitly enabled. A protocol 6 helper is offered Update and never used until updated. No stale process is silently authorized. |
| Remote resolution through the helper | With the helper sharing and control allowed, the controller takes control and picks Display → Remote resolution → Fit local, then Original; then Fit local again and Disconnect; then Fit local, Make view-only on the host. | Each change shows on the VM console and in the viewer within a few seconds, with no reconnect; Original, the disconnect and the revocation each put back the VM's own mode. While changed, the host card says so and its Restore works without control. After a VM restart the registry mode is the original one. Record the VM's modes and the controller's display. |
| Remote resolution, killed worker | With a fitted mode, end the capture worker (`pb-mapper-desktop-helper.exe` in the user's session) through the VM's already-authorized management path. | The service runs `helper restore <session>`, and the console returns to its own mode without a viewer; `display-modes-<session>.json` under the protected installation is gone afterwards. Repeat by stopping the service instead: its next start puts the mode back. Record `service.log`. |
| Remote resolution on the secure desktop | With a fitted mode, request an elevation that prompts; cancel it locally. | While the prompt shows, a mode change is refused with a notice and the fitted mode stays; video continues through the switch. After the cancel, Original works. No automated consent. |
| Remote resolution at logoff | With a fitted mode, log off locally, then sign in again. | The next login shows the original mode; no journal is left, and no other user inherits a changed mode. Record whether the worker's restore or Windows put it back. |
| Display mode probe | In the VM, with a local relay and a throwaway key, run `display_mode_probe <config.json> --owner-change --fullscreen-check` from the build's examples. | It prints `display_mode_probe passed`, every display's mode before and after, and the `CDS_FULLSCREEN` line, which decides contract R13. It sends no input. |
| Uninstall | Use the UI's Uninstall action with deliberate OS consent; collect `Uninstalled`. | Service and authorization configuration are absent. In-use files may await a normal reboot as documented. No reboot is forced. |

Collect the named stage after each lifecycle action, for example:

```powershell
./windows-helper-acceptance.ps1 -Stage Installed -OutputDirectory ./evidence
```

For standard-account installation the intended owner's SID must be that account,
even if another administrator supplies the local consent. Run the collector as
the sharing account. Do not treat RDP/enhanced-session disconnection as a console
lock test; the helper deliberately requires the active console session.

Machines requiring Ctrl+Alt+Del/SAS and fresh pre-login boot screens are not
supported by this implementation. Do not weaken those policies to pass.

Record screenshots before password entry and after cancellation, controller
errors, and bounded helper logs when needed. Redact account names and private
invitation material before sharing evidence. Revert the dedicated checkpoint at
the end only if it contains no unrelated work. Failure to obtain an authorized
VM console or administrator consent leaves these rows `NOT RUN`; unit tests and
ordinary-desktop tests cannot substitute for them.

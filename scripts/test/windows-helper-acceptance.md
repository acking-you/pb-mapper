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
| Initial installation | In the normal UI choose Install / update helper; local human deliberately completes Windows consent. | Installer returns success. `Installed` evidence shows `PbMapperDesktop` Running, LocalSystem, the protected release executable, and configuration owner matching the intended sharing user. |
| Protected app | Open the installed app from the UI; enable the helper and share. Try helper opt-in from the original portable app too. | Installed app remains unprivileged and can use the service. Portable app cannot impersonate the authorized executable. Record process session IDs and protected ACLs; the collector does not itself prove ACL correctness. |
| Administrator UAC desktop | From the VM console request an elevation that actually prompts under default policy. A human may cancel it after observation. | Helper-enabled viewer shows live secure-desktop frames, not a stale Default desktop. A click queued before the switch is not applied on the prompt. Record transition/recovery and errors. No automated consent. |
| Standard-user credential desktop | Sign into the existing standard account. Install/authorize for this account using the UI and deliberate local administrator authentication; use the installed app. Request a real elevation. | The viewer shows the credential prompt without storing/recording a password. Cancel locally. The account has not become an administrator and the UI remains unprivileged. |
| No-helper secure desktop | Repeat the prompting operation with helper opt-in disabled. | Viewer clearly reports an inaccessible/stale desktop and admits no input; cached ordinary-desktop frames do not appear live. |
| Secure-desktop input | With a human operating the controller, exercise harmless navigation or Cancel on a real prompt; do not approve elevation or type credentials. | Observe actual secure-desktop input behavior and recovery. Merely displaying the prompt is not input acceptance. |
| Held-input release | In a disposable text/drawing window hold a modifier or drag using the controller, then separately end control, revoke input, disconnect, and stop sharing. | The target no longer has held keys/buttons, no delayed input is replayed, and viewing/control state matches the selected action. Record each boundary separately. |
| Lock revocation | Hold input in the disposable target, then lock the VM using its local console. Locally unlock afterward. | No lock/login-screen control or capture is offered. Existing input is released and authority revoked; unlocking cannot silently restore the old control grant. Fresh explicit admission is required. |
| Helper disable | Use the UI's Disable helper action with deliberate OS consent, then collect `Disabled`. | Service is Stopped and startup Disabled; helper-backed sharing fails clearly and no worker remains. |
| Update / reinstall | Install the same complete build again through the UI; collect `Installed`. | Service returns Running and new protected app can share. No stale process is silently authorized. |
| Uninstall | Use the UI's Uninstall action with deliberate OS consent; collect `Uninstalled`. | Service and authorization configuration are absent. In-use files may await a normal reboot as documented. No reboot is forced. |

Collect the named stage after each lifecycle action, for example:

```powershell
./windows-helper-acceptance.ps1 -Stage Installed -OutputDirectory ./evidence
```

For standard-account installation the intended owner's SID must be that account,
even if another administrator supplies the local consent. Run the collector as
the sharing account. Do not treat RDP/enhanced-session disconnection as a console
lock test; the helper deliberately requires the active console session.

Record screenshots before password entry and after cancellation, controller
errors, and bounded helper logs when needed. Redact account names and private
invitation material before sharing evidence. Revert the dedicated checkpoint at
the end only if it contains no unrelated work. Failure to obtain an authorized
VM console or administrator consent leaves these rows `NOT RUN`; unit tests and
ordinary-desktop tests cannot substitute for them.

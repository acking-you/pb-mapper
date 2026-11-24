# Embedded remote desktop (issue #50)

The optional Rust SDK component and existing Flutter UI share a Windows or Mac
screen through an existing pb-mapper deployment. The implementation includes
GPU H.265 and H.264 video, native decoding and rendering on every viewer
platform the app runs on, an opt-in Windows helper, and explicitly remembered
host authorization. The relay needs no desktop-specific upgrade or additional
rendezvous service.

**Issue #50 is not complete.** The helper has not passed administrator-consent
and standard-user credential-prompt acceptance under the default secure-desktop
policy. The development process is not elevated; it cannot approve its own UAC
prompt. Android validation uses the Windows machine's Android VM, as requested
by the user. This does not establish physical-phone or iOS support, independent
cross-NAT operation, or the same presentation rate on every target device. The
macOS host and viewer have streamed on an M3 Pro. iOS device testing remains
outstanding, and the Linux viewer has run
only in WSL2 (see
[Measurements and remaining acceptance](#measurements-and-remaining-acceptance)).

The performance milestone is **1080p direct viewing at or near 90 presented
FPS**. Both endpoints prefer pb-mapper's existing direct path, with pre-commit
relay fallback. A viewer asks for the remote display's own size by default, up
to 3840×2160 (720p, 1080p and 1440p are choices kept per computer), fitted
inside the largest picture of the decoder it offers first, at its display's
refresh rate (at most 60 FPS on Android unless a maximum frame rate is chosen)
and a quality preset; the host chooses frame rate and bitrate within them. Both
ends prefer H.265 where their hardware has it, and fall back to H.264: a phone
whose H.265 decoder stops at 1080p keeps H.265 and is sent 1080p. Balanced, the
default, starts at about 4 Mbit/s for a 1080p picture (6.2 at 1440p, 11.3 at
4K, by the pixel scaling below) and never exceeds 60 FPS; only Smooth goes
higher. A 60 Hz phone or VM cannot demonstrate 90 displayed FPS. Relay sessions
may reduce frame rate and bitrate to preserve responsiveness; 90 FPS is not a
relay acceptance requirement.

Known limits of the native default: the bounds are landscape, so a portrait
display is fitted into them (1440×2560 is sent at 1214×2160) until the decoder
probes test rotated sizes; a display larger than 3840×2160 is fitted into 4K;
and a 4K host at 60 FPS has not been measured, since the development PC's
display is 2560×1440.

**The owner accepted real-device fluidity on 2026-11-10**, lifting PR #51's
merge block: after testing the stage7 build (Windows `8f800b3`, Android
`ae3df37`) they reported that everything works. The receipt-credit pipeline
below replaces the presentation-credit pipeline that stuttered on a physical
phone; the acceptance rests on the owner's own use, not on the instrumented
phone run proposed under Performance. For the block's history, see
[the block and its evidence](validation/desktop-input-backpressure.md).

## Use the UI

Remote desktop is a page of its own, between Tunnels and Relay; on a phone it
is the Remote tab. How the page and the viewer are laid out is in
[the UI design](ui-design.md#remote-desktop).

1. Configure both apps for the deployed relay and a namespace they can both
   access. Use a dedicated scoped relay credential for the controller; never
   transfer the relay administrator credential to a phone.
2. On Windows, open **Remote desktop** and choose **Turn on remote control**.
   This single action prepares the authorized helper, persistent service,
   input permission and automatic restoration. Complete any Windows consent
   yourself; a successful installation restarts the unprivileged UI on this
   page and resumes the saved operation. **Share the ordinary desktop only**
   skips the helper, and with it UAC and the lock screen. On a Mac, see
   [Sharing a Mac](#sharing-a-mac): the card has one **Share this computer**
   button and first asks for the permissions macOS requires. The card selects
   the shared display; switching it reconnects the current viewer. While it
   shares, **More** renames the service, for example to
   `desktop:my-computer`. Names start with `desktop:` and contain only ASCII
   letters, digits, `.`, `_`, `:`, or `-`, with a maximum of 128 bytes. A
   slash is invalid for scoped relay services.
3. Copy the invitation privately, or show its QR code to a phone: anyone
   holding it can view this desktop, and control it while **View and
   control** is chosen. It contains no relay credential. The card shows it
   masked; the QR code hides again when you leave the page or the app.
4. On the controller, open Remote desktop and choose **Add computer** (on a
   phone, **Scan to add** or **Paste invitation**). The app recognises the
   host's service and fingerprint before connecting, and refuses a pb-mapper
   setup string. **Connect** (or Enter in the name) opens the viewer.
   **Remember this computer**, on by default, keeps it in the list once its
   first picture arrives; **Relay only** is under **More options**. The UI
   requests the host's permitted control only after a fresh frame is actually
   presented; the SDK's explicit control-grant boundary is unchanged.
5. Drive it with a mouse and keyboard, or by touch; see
   [Gestures, keys and modifiers](#gestures-keys-and-modifiers). The
   **Shortcuts** menu sends the system shortcuts this computer would keep
   for itself.
6. **Display** (on a phone, **More**) chooses the resolution and the maximum
   frame rate for this computer, and the codec for this device; each
   connects again at once. It also turns on the quality monitor, a small
   readout of speed, frame rate, delay, bitrate, codec and resolution over
   the picture.
7. **View** keeps watching without control; **Disconnect** ends the session.
   **Back** returns to the list with the session paused. On the host, the
   viewer's line offers **Make view-only** and **Disconnect viewer**, and
   **Stop sharing** is always within reach while a viewer is connected (see
   [Who is viewing](#who-is-viewing-and-a-hidden-window)).

There is one shared display, which up to four viewers may watch or control at
once (a fifth connection is refused as busy). Input uses the shared
display's physical coordinates, DPI, letterboxing and viewer transform,
including negative monitor origins. A stale screen disables input while
keeping the texture visible. The Windows GPU capture backend currently rejects
rotated displays instead of presenting incorrect geometry. The pointer travels
beside the video: its position always, and its shape where both ends list
`cursor_shape` (see [Input and cursor](#input-and-cursor)). On a desktop
viewer in control, this computer's own pointer takes the host's shape over the
picture, so one pointer shows: a shape this computer's system has as its own
pointer, and any other (an app's own cursor, and on a Mac the busy, help, move
and diagonal resize pointers) as the host's bitmap made this computer's
cursor, which moves with the mouse and changes without waiting for a Flutter
frame. A phone paints the shape at the picture's scale. A host without shapes
(0.11.0) shows the generic arrow, as before.
When more than one viewer controls at once and both ends list `peer_cursors`,
each viewer also sees the others' active pointers in distinct session colours.

### Sharing a Mac

A Mac shares from the app itself, on macOS 12.3 or later (ScreenCaptureKit);
below that, the Remote page says only Windows and macOS 12.3 or later can
share. There is no helper or background service: hosting runs while the app
runs, and resumes with the app when **Share this computer** was left on.

- macOS asks the owner for two permissions, which the host card lists while
  they are missing, each with **Allow in System Settings**: that asks macOS,
  which prompts the first time, and otherwise opens Privacy & Security on the
  right page. **Screen Recording** is needed to share at all and takes effect
  once the app is reopened; **Accessibility** is needed for control and takes
  effect at once. When Accessibility is missing, the app offers view-only
  sharing. Returning from System Settings refreshes the host card; while the
  card lists a missing permission or finds no display it also checks every
  10 s, so a grant made with the window still visible beside System Settings
  shows without switching back. Nothing checks them on a clock otherwise. A
  start
  without Screen Recording fails with
  `PERMISSION_MISSING`; an input without Accessibility ends control
  (`input_failed`, naming Accessibility), because macOS would drop the events
  silently, and viewing goes on.
- For a local build alongside the installed release, use
  `make build-ui-macos-local`. Open `pb-mapper-local.app` and grant Screen
  Recording to that copy. **Show this app in Finder** selects the running
  bundle so you can add it with **+** in System Settings. An ad-hoc signed
  build may need a new grant after recompilation.
- The app is not sandboxed since 0.12.0. The App Sandbox denies posting input
  to other apps, and an app that controls others cannot be sandboxed; it
  ships outside the App Store, signed with Developer ID and the Hardened
  Runtime, with no entitlement in release. Neither permission needs an
  entitlement or a usage key, and no `Info.plist` key was added for hosting.
  Leaving the sandbox moves where the app keeps its data, from
  `~/Library/Containers/com.pb-mapper.ui/Data/Library/` to `~/Library/`: at
  its first launch outside the sandbox the app copies its configuration
  directory (tunnels, the relay credential, settings), its support directory
  and its preferences out of the container, once, and leaves the container
  as it was, for a downgrade.
- A remembered grant is sealed with a key kept in the login Keychain (see
  [Remembered authorization](#remembered-authorization-and-unattended-use)).
- Locking the Mac, or switching to another user, shows the viewer the secure
  desktop state and stops input; the picture returns after unlocking. The
  lock screen and the login window cannot be shared, and **Lock remote
  computer** is not offered for a Mac.
- While a viewer watches, the app holds off App Nap and keeps the display
  awake. A display that has gone to sleep is lit when a viewer connects or
  resumes, or when capture gets no picture, and one turned off while a
  viewer watches once it has stayed off for a second, as a touch at the Mac
  would light it: ScreenCaptureKit captures nothing of a sleeping display,
  and a viewer gets input only for a picture it was shown, so it could not
  wake the Mac itself
  ([Waking a sleeping Mac display](#waking-a-sleeping-mac-display)).
  A locked Mac stays dark: when macOS asks for the password as soon as the
  display turns off, a display that slept also locked the Mac, and the
  viewer sees the secure desktop state. With a MacBook's lid closed, only an
  external display is lit. The thread that paces capture waits on timers
  the kernel does not coalesce: on a CI runner whose ordinary timers ran up
  to 134 ms late, that kept a 60 fps session at 60 frames a second
  ([Timers on a Mac host](#timers-on-a-mac-host)).

The M3 Pro hardware run covered screen capture and view-only streaming. CI also tests it
on a macOS runner: the VideoToolbox encoder (keyframes only when asked, each
with its parameter sets), the display list, and reading the system pointer
off the main thread in a window-server session. The app with the FFI library
that carries this host also builds there. Permission prompts, input injection, mode changes and Keychain persistence
still need separate hardware acceptance.

## Gestures, keys and modifiers

### Touch

A phone or tablet starts in trackpad mode; the input-mode button switches to
direct touch, and a long press on it shows the gestures.

| Gesture | Trackpad (default) | Direct touch |
| --- | --- | --- |
| Slide one finger | Moves the pointer | Drags from where it started |
| Tap | Left click at the pointer, sent at once | Left click where tapped |
| Long press (500 ms), then lift | Right click at the pointer, on release | Right click where pressed, on release |
| Long press, then push | Scrolls at the pointer, both ways | Scrolls the window under the finger |
| Double-tap, hold (300 ms) and move | Drags | — (a double tap is a double click) |
| Three-finger swipe up or down | Scrolls, a notch per 24 px | Scrolls |
| Two fingers | Pan and pinch the picture here (1× to 8×); nothing is sent | Same |
| Two-finger tap | Nothing | Nothing |

**The long-press joystick.** A finger held still for 500 ms sends nothing: a
haptic press, and its ball becomes a joystick, a 120 px ring fixed where the
finger rests and the 44 px ball under the finger, kept inside the ring.
Lifted without leaving a 10 px dead zone, the press is a right click. Pushed
out of it, the ball opens a wheel burst (in direct touch after moving the
remote pointer to the press point, so the wheel reaches the window under the
finger; in trackpad mode at the pointer), and notches flow every 16 ms on the
gesture machine's own deadline, on each axis whose offset passes the dead
zone, at 2 notches a second just past it rising on a quadratic to 25 at the
ring; fractions carry over. The ball steers the view as a browser's
autoscroll does: pushed up, the page scrolls up, towards its top; pushed
right, it scrolls right. The host's wheel is positive down and right, so a
notch takes the sign of the ball's offset, the opposite of the three-finger
swipe, where the content follows the fingers. Back in the dead zone nothing
flows but the burst stays open; the lift ends it and clicks nothing. A second
finger ends it and pans and pinches as two fingers always do; so do a
pointer cancel, a mode change or focus loss. Three-finger scrolling stays as
a fallback, though phones may take three fingers for themselves. Latched
modifiers wrap the whole burst, so a latched Ctrl zooms the remote page.

On a phone the picture fits between the side gutters or bars, which stand
on dark strips; zoomed in, it pans with its edges stopping at the gutters,
so every edge of the remote display can be brought clear of the controls.
Direct touch ignores a finger that lands outside that frame, on what a
strip covers.

Changes from the PR #51 preview builds:

- Right click was a two-finger tap. It is a long press, and a two-finger tap
  now does nothing.
- Drag was a press held before moving. In trackpad mode it is a double tap
  whose second touch is held, then moved.
- Scrolling was a toolbar button. It is a long press and a push, the
  joystick above, with the three-finger swipe kept as a fallback; a long
  press right-clicks when it is lifted rather than as its timer fires.
- Trackpad is the default mode, and a tap clicks at once rather than after a
  wait for a possible double tap.
- Zoom never changes mode: two fingers always move the picture, never the
  remote pointer.

**The drag-start guard.** The second tap of a double-tap-and-hold would reach
Windows as a double click on the item about to be dragged. The viewer sends
that press no earlier than 520 ms after the first click, and the host holds
any press marked as a drag start until the OS double-click time (plus 20 ms,
at most 600 ms) has passed since a click on the same spot. A finger lifted
before then still drags: press, move and release go together, and the host
re-checks the held press's authorisation when it lets it go. A quick second
tap is a second click, which is a double click.

With a mouse or a touchpad, buttons and the wheel go to the remote computer
(a notch per 30 px of scrolling); Ctrl with the wheel, or a touchpad pinch,
zooms the picture here. A Windows touchscreen uses the desktop viewer with
direct touch.

### Modifiers

The modifier bar above a phone's keyboard latches Ctrl, Alt, Shift and Win
(⌃, ⌥, ⇧ and ⌘ toward a Mac, whose copy and paste keys read ⌘C and ⌘V):
tap once to arm one for the next key, click, drag or scroll, tap again to
lock it, and tap a locked one to release it. Every key or click sent with
modifiers goes as one sequence — the modifiers down, the action, the
modifiers up — so nothing stays held on the host. Latches clear on reconnect,
when the viewer closes, and when the app loses focus. **Release all keys**
in Shortcuts releases whatever this viewer holds on the host.

### Keyboards

A hardware keyboard, including a Bluetooth one on a phone, types through one
of two input methods, chosen per computer under **Shortcuts → Input method**
(on a phone, in the panel's Shortcuts):

- **This computer's** (the default). This device's input method composes and
  the text it commits is sent; Chinese, Japanese and Korean input methods
  work as in any text field here. A desktop viewer attaches a text-input
  client while the picture has the keyboard and this viewer controls; the
  composition shows next to the remote pointer and the candidate window is
  placed there. While the input method composes, Enter, Backspace, the
  arrows and Esc belong to it.
- **The remote computer's.** Keys go by their place on the keyboard (a USB
  HID usage, `Input::PhysicalKey`), so the host's layout and input method
  type them: a German host's layout makes Z of a US key's Y. On a Windows
  host Lang1 and Lang2 (the Korean Hangul and Hanja keys, a Mac's かな and
  英数) land where Windows' layouts put them, E0 F2 and E0 F1: Hangul and
  Hanja under a Korean layout, the input method on and off under a Japanese
  one. This needs a
  host that lists `physical_keys` (0.12 on Windows and macOS); toward an older
  one the menu says so, and letters, digits and Space go as keys and the rest
  as text. On Windows the viewer window's own input method is turned off
  (`ImmAssociateContextEx`) while the picture has the keyboard, and back on
  when it loses it, when the mode changes and when the viewer closes; on
  macOS, Linux and Android the picture sees keys before the input method
  does, so it is left out without that.

In either mode:

- A modifier waits for what it modifies. With this computer's input method,
  Shift, Ctrl, Alt and Win reach the host only with the next key, click or
  wheel while they are held, or at once while a button or another key is
  already held there; one pressed and released alone sends nothing, so
  Shift, Ctrl+Shift, Alt+Shift, Ctrl+Space, Win+Space and Caps Lock stay this
  computer's input-method and layout switches. With the remote computer's,
  Shift, Ctrl and Alt go at once, since its input method needs their taps,
  and the host's Win still waits (a Mac's Ctrl, where Ctrl and Cmd swap), so
  a lone Win tap opens neither Start menu; a modifier that goes at once
  takes one still waiting ahead of it, so Win+Shift+S arrives in order.
  Modifiers held down are real presses and releases, never wrapped around
  another key, latched ones included, because the host does not count
  presses.
- F1–F12, Enter, Esc, Tab, Backspace, Delete, Insert, Home, End, Page Up,
  Page Down and the arrows go as keys; with a modifier latched, as one
  wrapped tap. Tab never moves focus out of the picture.
- A letter, digit or space typed while Ctrl, Alt or Win is held is a
  shortcut: Ctrl+S, Alt+F and Win+E reach the host as such. The letter or
  digit names the host's key that types it, not a place: a Windows virtual
  key, and on a Mac the key its layout types that character with (behind a
  layout of another script, the ASCII-capable one macOS matches shortcuts
  on), so Cmd+A selects all on a French AZERTY Mac instead of pressing the
  place of Q there, which would be Cmd+Q. On layouts where
  AltGr (Ctrl+Alt) types a character, such as German, French or Polish, that
  character is typed, with no Ctrl or Alt sent; on an Apple keyboard Option
  with a printable key types its character and makes no chord.
- To a host that lists `physical_keys`, keys the vocabulary cannot name go by
  place in the local mode too: Ctrl or Alt with punctuation (Ctrl+/, Ctrl+-,
  Ctrl+[), PrintScreen, Pause, the Menu key and F13–F24. A Windows host
  presses Pause under a Ctrl it holds as Break, as a keyboard does, which a
  console takes as Ctrl+Break. Caps Lock, Num Lock,
  Scroll Lock, the numpad's digits and the input-method keys stay this
  computer's in the local mode; a 0.11.0 host gets none of them.
- Cmd and Ctrl swap places when exactly one side is an Apple device, on by
  default and per computer (**Cmd acts as Ctrl**, or **Ctrl acts as Cmd**):
  a Mac's or iPad's Cmd+C is Ctrl+C on a Windows host, and a Windows viewer's
  Ctrl+C is Cmd+C on a Mac. The host's system is known from its `os_*`
  capability.
- Shortcuts that this computer's system takes first — Alt+Tab, the Windows
  key, Win+L, Win+Space, Ctrl+Alt+Del — never reach the app in a window; use
  the Shortcuts menu, whose **Switch input method** sends Win+Space to a
  Windows host and Ctrl+Space to a Mac. Ctrl+Alt+Del is unsupported, since
  `SendInput` cannot produce it. **Lock remote computer** is the host's own
  command: it needs control, and says so when locking fails.
- In full screen on Windows, while the picture has the keyboard, this viewer
  controls and the app is active, a low-level keyboard hook
  (`ui/windows/runner/desktop_key_capture.cpp`, channel
  `pb_mapper/key_capture`) takes this computer's own shortcuts before its
  system does, as RustDesk's grab does, and sends them to the host alone: the
  Windows key and Alt (not AltGr), each with every key pressed while it is
  held (Alt+Tab, Alt+F4, Alt alone, Win+D, Win+Space), Esc under Ctrl
  (Ctrl+Esc, Ctrl+Shift+Esc), F10 (except while this computer's input
  method composes: a Japanese one's half-width letters) and Print Screen.
  Unlike RustDesk's, it leaves every other key to the window, so this
  computer's input method still composes in the local mode, and Caps Lock,
  Num Lock and Scroll Lock stay this computer's. A Windows key or Alt taken
  this way goes at once, so a lone tap opens the host's Start menu or menu
  bar. A key Flutter's tables do not name goes by its Windows codes, as
  Flutter's embedder names it. The hook runs on a thread of its own at high
  priority, since Windows removes a low-level hook that keeps it waiting,
  and takes a key only while the window is in front. A key it took goes up
  through the viewer wherever it is released; every key it holds goes up on
  the host when capture stops (leaving full screen, a menu or dialog taking
  the keyboard, the app going inactive) and when the secure desktop takes
  the keyboard (Win+L, Ctrl+Alt+Del, a UAC prompt), whose ups no hook sees.
  Full screen is window_manager's own flag, read back after each change,
  and the app's caption buttons follow the viewer's reading of it: on
  Windows the plugin sends its enter and leave events only from and to a
  maximized window. Ctrl+Alt+Del and Win+L, which Windows handles before any
  hook, act on this computer too. A Ctrl or Shift pressed just before a key
  the hook takes reaches the viewer through Flutter's key queue, which waits
  on the app, and the taken key does not: in a busy app it can overtake
  them, and the host then gets it without them.
- Toward a Mac (`os_macos`) the menu offers the Mac's own chords, named as
  a Mac names them: **Switch app** ⌘Tab, **Force Quit** ⌥⌘Esc, **Spotlight**
  ⌘Space and **Switch input method** ⌃Space. It leaves out Show desktop,
  whose Mac shortcut is a function key that laptops send as a media key and
  owners rebind, and Ctrl+Alt+Del, which a Mac does not have. A host that
  names no system gets the Windows chords.
- Everything held is released when the picture loses the keyboard; a
  modifier still waiting was never sent and needs no release.

The setting is kept by the app on this device (`viewer_keyboard`, by
remembered computer, at most 64), not with the computer in the vault, since it
describes this device's keyboard. **Type text…** on a desktop and the soft
keyboard on a phone send what the input method commits in both modes, turn
Backspace into deletions and Enter into Enter; a physical keyboard on a phone
follows the rules above while the soft keyboard is open. While a modifier is
latched the first 16 typed characters go as keys, so the bar's Ctrl followed
by C on the soft keyboard sends Ctrl+C. A host types text as one Enter for
each `\n`, `\r` or `\r\n` and one Tab for each `\t`; a Windows host before
this version typed each twice.

## Viewer states

| State | What the viewer shows |
| --- | --- |
| Connecting | Its steps — finding the host, going direct or through the relay, checking its identity, the first picture — with Cancel |
| Live | The picture; the toolbar's chip says when it is delayed |
| Reconnecting | The last picture greyed, a countdown (1, 2, 4, 8, then 15 s) and Retry now; a busy host is retried the same way, and when the relay answers without listing the computer's service the viewer says it isn't sharing right now (only while the relay and credential are those it was opened through; after a settings change it says nothing). After two minutes of failures the session ends |
| Waiting for the remote desktop | The last picture, and why: a UAC prompt or the lock screen without the helper, or no desktop at all |
| Recovering the picture | The last picture while a new keyframe comes |
| Host identity changed | An opaque card. Retries stop, and nothing replaces the saved identity except **Paste new invitation**, which opens the add flow for that computer |
| Ended | Why — sharing stopped, the host ended the session, the invitation was revoked or refused, a different pb-mapper version, the picture could not be decoded, two minutes of failed retries (the computer isn't sharing, couldn't be reached, or another device is still viewing it) — with Reconnect where it can help, Retry after a decoder failure, and Paste new invitation for a revoked or refused one |

Only the overlay for a changed identity is opaque; the others keep the last
picture visible. Input goes to the host only while the picture is fresh and
this viewer holds control: every other state pauses it, and nothing queued
is replayed afterwards. The protocol-v2 handshake refuses a peer of another
version, so a PR #51 preview build on either side ends in the version
message, never mid-session.

### Leaving the picture

**Back** keeps the session but pauses it: the host stops capturing, control
ends, and the list shows the computer as Viewing with **Back to picture**,
which resumes from a fresh keyframe. A hidden or minimised window, or a phone
in the background, pauses it the same way. A session left paused ends after
ten minutes, because it holds the host's only viewer slot and every other
viewer would be told the computer is busy; closing the window to the tray
counts. A session whose host identity changed holds no slot, so it keeps its
card until you act. If Android froze the app and the host closed the session
meanwhile, the viewer reconnects once, at once, on a return within those ten
minutes.
For a remembered computer, leaving the picture or disconnecting saves the
last frame as its thumbnail.

## Who is viewing, and a hidden window

While a viewer is connected, the host shows it in three places:

- a capsule in the title bar of every page and of its own viewer — Being
  viewed or Being controlled, with **Disconnect viewer** and **Stop
  sharing**;
- a pill above everything else where no title bar can be reached: first-run
  setup, a full-screen viewer, and any open dialog;
- the tray: the active icon, a tooltip that leads with who is watching, and
  both actions in its menu.

Both actions work even while another host action is still running.
**Disconnect viewer** is final for that session: the viewer is told the host
ended it and does not reconnect by itself. **Quit** from the tray stops the
app's own sharing first, so its viewer is told sharing stopped; the service's
sharing continues.

A sharing computer usually has its window closed to the tray, where an
in-window capsule cannot be seen. So when a viewer arrives while the window is
hidden or minimised, the window comes back without taking focus, once per
viewer session; closing it again is respected. It is not kept on top. When
the Windows service shares with the app not running at all, nothing shows a
viewer: the service cannot draw in the user's session. Keep the app in the
tray to see who is connected.

The app's own host tells the window when a viewer arrives or leaves, or its
session changes, through the change event `desktopHost`; the window then
reads the host at once. So while nobody is connected it asks nothing on a
clock but a backstop every 30 s, hidden or not. While a viewer is connected
it reads the host every 2 s, every second on Remote desktop, for what the
viewer panel shows. The Windows service's host sends no such event, so while
the service shares the window reads it on that same clock, viewer or not.

## Remote resolution

The desktop viewer's Display menu holds **Remote resolution**: the remote
display's own modes and **Fit local**, which change the *remote computer's*
display mode, not only the picture. The phone viewer has no such menu; its
screen says nothing about the size a desktop should run at.

- A host offers it (the `display_mode` capability) where its platform changes
  modes: Windows, in an ordinary session or through the helper, and macOS.
  A viewer that did not list the capability is never sent the modes, and its
  requests are protocol errors.
- The state record carries the display's modes: the current one, the
  original the host found, and up to 64 sizes at the current depth and
  orientation, none below 800×600, largest first. The menu appears only while
  this viewer holds control, since a mode change is as intrusive as input.
- **Fit local** asks for this computer's display size when the remote lists
  it, else the largest listed size inside it within 2 % of its shape.
  **Original** asks the host to put back the mode it found.
- The change is dynamic (`ChangeDisplaySettingsExW` without
  `CDS_UPDATEREGISTRY`), so the registry, and the next login, keep the owner's
  mode. Before changing anything the host journals the original:
  `display-modes.json` in the app's state directory, or the helper's
  `display-modes-<session>.json` under its protected installation.
- For 3 s after a change the host reports `display_changing` while its capture
  restarts (the viewer waits as for a capture restart) and refuses another
  change with a `display_mode_failed` notice.
- The host puts the display back when the session ends (disconnect, Stop
  sharing, the host's exit), when control is revoked (Make view-only), and when
  the owner presses **Restore** on the host card, which says the remote
  resolution was changed; that needs no control. Releasing control, a paused
  viewer and a fresh grant keep it. If the owner changed the mode in the
  meantime, the host leaves the owner's choice and forgets its entry. A mode
  of the same size is the owner's too when its refresh rate (by more than
  1 Hz), depth or, on a Mac, scale differs: a HiDPI mode and its
  low-resolution twin are one pixel size.
- A host that ended without putting it back (a crash, a killed app or worker)
  puts it back later from the journal: the app at its first desktop call, a
  host when it starts and after each session, retrying while the display
  refuses (the secure desktop shows, say), from every 2 s backing off to once
  a minute, and warning once per failure. A journal entry keeps the scope a
  dynamic mode lives in, the login and the monitor (its logon time and the
  monitor's device interface name on Windows): an entry from another login,
  for another monitor now under the same `\\.\DISPLAYn`, or for a display
  that is gone is dropped, never replayed. A helper worker that exits with
  modes outstanding exits with code 3, and the service then runs `helper
  restore <session>`, which tries for 10 minutes; the service does the same
  for every journal left at its start. The service ends that run, and waits
  until it is gone, only once another worker in the session has started and
  been verified, just before it attaches it; the worker replays the journal
  itself, and one that fails to attach hands it back to a new run. The two
  never act on one journal. Hosts in one process share a claim per display,
  with the first original and the latest mode set: the last holder puts the
  original back, whichever host set the mode the display shows. Two
  unrelated processes changing one display at once is a known limit.
- A remembered computer keeps **Fit local** (never a fixed size, which the
  next display may not list) and applies it again whenever this viewer gains
  control of it.
- On macOS a mode is a pixel size: the HiDPI and 1× modes of one size are one
  entry, at the owner's current scale where the display offers it, else the
  lower one, at the current refresh rate where offered, else the fastest. The
  change goes through `CGConfigureDisplayWithDisplayMode` with
  `kCGConfigureForAppOnly`, so macOS itself puts the mode back when the app
  ends, a crash included; the journal entry's scope is the process, so a dead
  process's entry is dropped, never replayed. **Original** and the restores
  above put back the exact mode found (pixels, scale and refresh). Where the
  display no longer offers it, that restore fails, as a failed restore does
  on Windows: the display stays changed and journalled and is tried again
  after the session, a viewer that asked hears `display_mode_failed`, and
  while the session lasts the host card still offers **Restore**; no other
  display is touched, and the app's end puts the mode back in any case. The mode list is unit-tested
  in CI; changing a mode has not run on a Mac.

The SDK side is `Viewer::set_display_mode(ModeTarget)`, which answers the mode
it asked for or a `DisplayModeError` (`Closed`, `NotOffered`,
`NotControlling`, `NoFit`, `NotListed`), the viewer status's `modes` and
`host_os`, `Host::restore_display_mode()`, `HostStatus::display_mode` while
the display is changed, and `recover_display_modes(state_dir)`. The wire
messages are `display_mode {width, height}` and `restore_display_mode {}`.
`examples/display_mode_probe/` is the hardware acceptance; it changes the
display it runs on, so run it only where the owner agreed (see
[the acceptance runbook](../scripts/test/desktop-ui-acceptance.md)).

## Remembered computers

A controller can remember the computers it views, so it reconnects without
pasting an invitation again:

- A computer is remembered only after its first picture, from the live session,
  which has already passed the certificate pin. A wrong or unreachable
  invitation leaves nothing behind.
- The vault is one encrypted file per app profile beside the configuration:
  `desktop-computers.dpapi` under current-user DPAPI on Windows; on Android
  `desktop-computers.bin`, sealed with AES-256-GCM under a data key that an
  Android Keystore key wraps (`desktop-computers.key`, unwrapped through the
  `pb_mapper/keystore` channel once per process). Other platforms cannot
  remember computers, and the add form says so. At most 50 are kept. The
  file is `{"version":2,…}`: a computer keeps a resolution only when one was
  chosen, so a later change of the default needs no migration, and may keep
  **Fit local** as its remote resolution. A version 1 file, which wrote 1080p
  whether chosen or not, reads its 1080p (or none) as the default; it is
  written back as version 2.
- Invitations never return to Dart. The list carries names, services,
  fingerprints and when each was last used; opening one connects by its id,
  and Rust looks the invitation up.
- Online status comes from the relay's service list, which the app already
  polls. A computer saved under another relay address or credential reads
  Other relay and opens only after switching back in Settings.
- A changed host identity never touches the saved computer; only **Paste new
  invitation** replaces its invitation, once the new session is remembered:
  at its first picture with **Remember this computer** on, or when its row (on
  a phone, its card) is remembered later. Until then the session is a computer
  of its own, and the saved one keeps its record and last picture.
- **Remove this computer** deletes its invitation and its last picture.
- Saved computers that can no longer be opened (DPAPI after a restored
  profile or a password reset, a damaged file) are offered as **Clear saved
  computers** on the Remote page, which deletes them and their last pictures.
  A read that fails only for now (the file is locked, say) offers nothing and
  is tried again when the app next starts. On Android the vault starts over
  by itself only when the Keystore key is gone or no longer opens its wrapped
  copy, and only once a new key has been sealed; any other Keystore failure
  leaves it for the next start.
- Android cloud backup and device-to-device transfer exclude the app's
  configuration directory, so they restore neither the vault nor the relay
  credential nor the tunnels.

## Remembered authorization and unattended use

**Turn on remote control** is the explicit host authorization for persistent
viewing and input. The Windows service owns the TLS identity, 256-bit invitation
token, selected display and permissions. The grant is bound to the exact relay
address and credential namespace ID. The controller keeps its private
invitation itself, or lets the app remember it in its encrypted vault (see
[Remembered computers](#remembered-computers)).

The invitation is a bearer capability, not a certificate identifying a particular
phone. Changing a relay or namespace does not silently reuse it elsewhere. Input
permission is separately revocable. Every connection starts without active
control; the UI requests a new grant only after displaying a current frame.
Reconnect never replays queued pointer or keyboard actions. A certificate
mismatch stops automatic retries and never replaces the pinned identity.

| Action | Result |
| --- | --- |
| Turn on remote control | Starts the service-owned host with input and automatic restoration enabled; reuses its invitation; a viewer of the app's own sharing reconnects by itself when the service is ready (installed, running, current and verified for this installed app, so nothing needs installing or updating) and takes over the same invitation (the same relay, namespace and service); otherwise it ends with sharing stopped |
| Stop sharing | Closes sessions and disables automatic sharing; the same invitation resumes on the next explicit Start sharing, while Settings name the same relay and namespace (see [Changing the relay](#changing-the-relay)) |
| Close the app | Stops its viewer; service-owned sharing continues |
| Upgrade legacy app-owned sharing | Transfers the saved identity if no same-scope service identity exists; an existing service identity remains authoritative |
| Revoke invitation and stop sharing | Deletes remembered trust, closes the current host, and requires a new invitation for future sharing; its viewer is told the invitation was revoked, whether the app or the service hosts |

On a Mac the app's remembered grant is sealed with AES-256-GCM under a random
key kept as a generic password in the login Keychain (`trust::Keychain`,
service "pb-mapper desktop trust"), created at the first seal; revoking
deletes the sealed file, not the key. A development build that is not signed
may ask once for access to that Keychain item.

Legacy app-owned grants and a pending installation handoff use current-user
DPAPI in `desktop-trust.dpapi` beneath the existing configuration directory.
A copy Windows cannot unlock (after a password reset, or a profile moved to
another machine) or that is invalid is not retried in the background, only
read again on each visit to the page: the Remote desktop card says why and
offers **Revoke the saved invitation**, which deletes it. Any other failure to
read it, such as a file locked for a moment, is retried: at startup with
bounded backoff, and by the page at its next visit.
Successful service transfer removes the legacy copy. A pending handoff retries
temporary IPC failures with bounded backoff; it never installs a helper without
the explicit Enable action. Stop or Revoke cancels the pending restoration.
SDK embedders own their storage:
`HostIdentity::encode_secret` contains private material and must go in an OS vault
or encrypted storage, never a log, public configuration, or source control.

The installed **Windows background service** saves the relay configuration, invitation
identity, display and permissions using machine DPAPI in a SYSTEM/admin-only
file. The approved installed UI manages it through a separate authenticated,
bounded local pipe; ordinary desktop sessions still use endpoint-pinned TLS and
the existing pb-mapper direct/relay transport. The service retains no Windows
login password. Stopping disables future automatic sharing; revoking destroys
the saved grant. Closing the UI merely stops its viewer and any temporary host.
The UI stops any local publisher before transferring ownership. Repeating the
same configuration does not restart an active host or rotate its identity.

Process-based proxy and firewall rules must allow `pb-mapper-desktop-helper.exe`
as well as `pb_mapper_ui.exe`: background sharing opens its relay connections in
the helper. A rule for the UI alone does not cover the service. With the QUIC
carrier, the helper needs UDP access to the relay's advertised QUIC port and
TCP access to its configured discovery port. For example, a TUN proxy with UDP
forwarding disabled can leave the UI connected while silently dropping the
helper's QUIC handshake. Add the helper to the same permitted route as the UI,
or explicitly select a working relay carrier before configuring background
sharing. pb-mapper does not change another application's routing rules.

A viewer that chose **Settings → Direct connections → LAN transport → TCP**
reaches a host on its LAN over TCP only where the host's firewall lets TCP in:
the host opens a TCP listener, on its direct port's number, the first time
such a viewer asks, and offers it on its LAN addresses only. A viewer with
evidence of a shared LAN gets that listener on every interface, whichever
relay the two use; only a viewer on the same computer, under a relay on
loopback, gets one bound to loopback alone, which raises no prompt. On
Windows that first listener on every interface raises the firewall prompt on
the host's desktop for `pb_mapper_ui.exe`; the background service gets no
prompt, and nothing adds a rule for either. Until TCP is allowed, the session
takes QUIC on the LAN within 300 ms, as it would have without the choice, and
the viewer stops asking for a while (10 s, doubling to 5 min).

Background startup errors retain the registration carrier, phase and failure
in the UI status and in the bounded protected
`%ProgramFiles%\pb-mapper-desktop\service.log`. `background_start_failed`
records failed attempts; `background_registered` confirms relay readiness.
If a viewer resets while the relay reports that its desktop service is not
registered, resolve that publisher startup failure before diagnosing capture.

The service waits for the installation owner's active console login, including
that same login while locked, and retries failed relay/capture startup with
bounded backoff. Logoff or switching away from that owner closes active capture
and input. A subsequent viewer still needs fresh explicit control admission;
old input is never replayed. A fresh boot with no owner login remains
`waiting_for_owner_session`. A Windows session/privilege query failure is reported
separately with its failing API/error and retried; it must not masquerade as an
owner who has not signed in. Such failures cancel active/pending hosting while
retaining the saved authorization. Waking from sleep is different from remote input:
S3/S4 cannot serve video while suspended, and no Wake-on-LAN facility or power
policy change is included. Real service-restart, sleep/resume and locked-session
password entry remain acceptance requirements, not consequences of unit tests.

The service looks for that login once a second, but takes the login's token
and compares its SID only before it starts the host, and while it holds no
attested login for the console's session: at first, when the console changes
hands, and when a recheck fails. The console's session id, which the service
reads without calling another process, shows a user switch, a remote
connection or a new console session at once; while it is unchanged, one
session query every 5 s confirms that the same login (session, logon time,
user and domain) still holds the console. A logoff followed by a new login at
the same session id is therefore seen within 5 s rather than 1 s. Capture
never relies on this: it attests the owner's login in full when it starts and
every 100 ms while it runs. The app, for its part, checks the installation
and the helper's readiness once each time it reads the service's state, and
asks with what that check verified; on every connection it still verifies
that the pipe's server is the running service, as the service verifies the
app.

### Changing the relay

Changing the relay or credential in Settings does not move a host that is
already published. A sharing or paused host stays on the relay and namespace
it was published on until it is moved; the service, which restores its host
when Windows starts, restores it there too, and the app's own startup never
resumes a host on another relay. While Settings name another relay, or a
credential of another namespace (another temporary credential, or the
administrator key in place of a temporary one), the host card names the relay
the host is on and offers **Share on the current relay**, which asks first.
Moving configures the service again with
the same service name, display and permissions through the relay, credential
and carrier now in Settings (an app-owned host starts again the same way). The
invitation is never reused on another relay or namespace, so moving creates a
new one, and computers that saved the old one must add this computer again.
The FFI enforces the question: it refuses a resume that would move the
service's host (`RELAY_CHANGED`) unless the app says the owner confirmed it, so
a card that has not read the new settings yet cannot move it unasked.
**Start sharing** always configures the service from the current settings: on
the same relay and namespace it keeps the invitation, and also takes a changed
carrier, a changed *Try the local network first* or a renewed credential. A rotated administrator key is the same
namespace, so the card does not flag it: a sharing host keeps the retired key
until **Stop sharing** and **Start sharing** configure it from the settings
again. When the service registers, its status and
`service.log` name the relay it registered on.

## Windows helper and UAC boundary

The ordinary backend stays at the user's integrity level. It checks the active
input desktop and reports an inaccessible secure/locked desktop; it does not
keep a cached Default desktop apparently live. Elevated windows can reject its
input. The separate helper is the implementation path for elevated-window/UAC
access, **pending actual OS acceptance**.

Windows remote-control preparation checks
that the helper is installed, running, that this application is authorized,
and that its live process image matches the protected installation. An identity
query failure requires repairing/updating the helper and is never shown as ready.
Use **Turn on remote control** and deliberately complete Windows' local
administrator consent when required. Installation automatically replaces the current
application and returns to the remote desktop page, preserving saved settings
and background sharing authorization. There is no separate "open installed app"
step. An unelevated worker verifies the protected installation and owner, opens
a handle to the old process, and acknowledges readiness before the UI exits.
It launches the replacement only after that process exits, with a bounded wait.
Cancellation or preparation failure leaves the original window open. Sharing
selects the helper automatically when ready; maintenance remains available for
service-owned sharing even when background management loses its connection.
The SDK ordinary-desktop backend remains available to embedders and does not
support UAC or the lock screen. The first
elevation of a portable app may need a person locally. A phone cannot approve a
desktop the unprivileged app cannot access. No UAC policy is changed, no prompt
is auto-consented, and no administrator password is retained.

The Flutter process remains unprivileged. Temporary sharing owns networking in
that process; explicit background hosting owns a narrow desktop endpoint in the
Windows service, independently of Flutter or an agent harness:

- The installer copies the complete UI/helper bundle to a unique protected
  release directory under `%ProgramFiles%\pb-mapper-desktop`. Owner and DACL are
  applied at object creation; unsafe ACLs, parent traversal and reparse points
  are rejected. Copying and cleanup have depth, file-count and size bounds.
- The `PbMapperDesktop` LocalSystem service runs in Session 0 and accepts local
  named-pipe IPC only. Its ACL grants the approved user read/write access without
  permission to create another pipe-server instance. Kernel-reported process
  identity, protected executable path, user SID and active console session are
  checked; the client verifies the installed service as well. Before reporting
  Running, the service merges only `PROCESS_QUERY_LIMITED_INFORMATION` for the
  approved owner into its existing process DACL. This lets the unelevated UI
  query the service executable; it adds no process-control or memory access.
- For an authorized connection, the service launches one restricted SYSTEM
  worker in the active interactive session. Temporary launch privileges are
  restored, a kill-on-close job bounds the child lifetime, and the worker uses a
  separate SYSTEM/admin-only pipe. The capture worker has no remote listener,
  arbitrary command, file, clipboard or credential API. Persistent-host
  management accepts only the protected installed UI and approved owner SID.
- The worker can bind its capture/input thread to Default or the secure desktop
  within the approved console login, including its locked state. The worker
  checks session ID, login time and user identity before context/input. On the
  ordinary desktop it reuses one answer for at most 4 ms, and never across a
  desktop switch or a change of active console session. On the Winlogon
  desktop, where UAC and the lock UI share a name, it asks every time. A reused
  session ID or another user's login cannot inherit the old admission.
  Desktop-switch and lock-state changes
  advance a generation independently of resolution. Capture and final input
  injection check that generation; an old click cannot be replayed after a
  detected desktop transition. Failed input releases are retained for retry.
- Only a current rendered frame renews held-input state. Disconnect, permission
  revocation, session change, renderer staleness and local stop release input;
  the service gives the worker a bounded exit interval before its job is closed.

Each answer of the worker, with the unit or the shape it carries, crosses
two pipes: the worker's private pipe to the service, and the helper pipe on
to the app. Both carry `u32 BE length | bytes` records (`oop/ipc.rs`): a
request or an answer as JSON of at most 8 KiB, and after each answer a byte
record of up to 4 MiB. The worker writes an answer and its byte record in
one vectored write, and the app each request in one. mio, from 1.2.5, makes
such a write one overlapped `WriteFile` instead of one per length and per
body, copying the slices into its write buffer as it copied each write
before, so joining them copies nothing more. A pipe keeps that buffer for
its next write only up to 1 MiB: a larger write, an answer whose unit is
over about 1 MiB, allocates its buffer anew each time, where 1.2.4 kept one
of any size. mio 1.2.5 also makes each pipe read as large as the buffer it
fills, from 4 KiB up to 64 KiB; 1.2.4 read what a buffer it kept held,
4 KiB at the two ends that read units. Each pipe lets 256 KiB wait in either
direction (`helper/transfer.rs`), so a write that fits beside what already
waits completes without waiting for its reader. The service relays toward
the app through a 256 KiB buffer, reading on while a write is in flight so
that the next write takes all it gathered, and toward the worker through
64 KiB, so each of its writes stays under that 1 MiB. The records, and so
the helper protocol, are unchanged. `examples/pipe_relay_bench.rs` times a
unit through two pipes and such a relay, and counts the writes and reads it
took; its 3.7 MB unit is the one whose write allocates on 1.2.5. No Windows
run of it is recorded yet.

Installation is serialized, replaces configuration atomically, waits for the
service to report Running, and attempts to restore the previous configuration
and startup state if an update fails. A rollback failure is reported and files
are retained for repair. This is not a claim of crash-atomic replacement of the
SCM database and filesystem together.

Abnormal service exits use SCM recovery delays of 5, 15 and then 60 seconds,
reset after 24 hours. Normal Stop exits do not request recovery. Windows may
apply the non-crash failure flag after the next OS restart. Disable and uninstall
disable startup before stopping, so an already queued SCM restart cannot undo
that action. **Turn on remote control** explicitly restores automatic service
startup, even when it must first install or update the helper, in the same
consent prompt. Updating a disabled or stopped installation from the helper
row preserves that state.

The helper row offers **Update** when the installed helper speaks another
helper protocol (`HELPER_PROTOCOL`), which this app never uses, and also when
it is another build of this protocol: its executable is not byte-identical
(SHA-256, read again only when its size or time changes) to the one bundled
beside the running app. Another build still works, but the background host
runs in the service and its capture worker, so a new build's host reaches
background sessions only once it is installed. **Start sharing** for a paused
background host whose helper is not ready, or is another build, goes through
setup as **Turn on remote control** does: one consent prompt installs, updates
or enables it, the app restarts into the installed copy when it installed one,
and the host resumes there. The app's startup restore never installs or asks.

A helper this app cannot ask still hosts the grant it saved: one of another
protocol, whose pipes are not this app's, or one installed from another copy
of the app, which serves only that copy. Both follow every upgrade, since the
installer reopens the app from where it is installed, never from the
service's protected copy. The app then sees the grant without reading it (its
file exists in the protected installation; only SYSTEM and administrators may
read it), and the card shows it as the service's: sharing while the service
runs, with its settings and invitation unknown, and the helper's update as
the one action. The update keeps the grant, policy, identity and invitation
alike, and the service answers the app with it once the app has restarted
into the new copy. **Turn on remote control** and **Start sharing** are
refused over such a grant (`HELPER_OUTDATED`, `HELPER_FOREIGN_APP`), since
configuring it from here would replace its view-only, relay-only or paused
policy, and under another service name its invitation.

Protocol 10 is the current one, with the pipes
`pb-mapper.desktop-helper.v10` and `pb-mapper.desktop-background.v10`. It
adds `submitted` to the answer of a `Video` request that captures: `false`
when the worker's source had no new picture to submit, so the host's thread C
keeps its tick armed and asks again (see
[Host: capture and encoding](#host-capture-and-encoding)). It is sent only
then; absent, the capture took the tick, or spent it on a full encoder.
Protocol 9 added `HostStatus.viewers` to the background host's status, for
concurrent sessions.

Protocol 8 added the pointer's shape and keys by place. The first
`Request::CursorShape` turns the worker's shape reads on; from then on every
request that observes the pointer (context, capture, video, shape) also reads
the shape and answers `shape_serial`, which the worker advances only when the
shape's content changes, and the host asks for the shape again (one more round
trip per new shape, none per tick) only when it moved. The answer carries a
`ShapeHeader` (kind, size, hotspot) with the bitmap as its byte record, and
both ends check its bounds. `Request::Input` may carry `Input::PhysicalKey`,
and the `Start` caps say whether the worker reads shapes and injects physical
keys.

Protocol 7 added display modes ([Remote resolution](#remote-resolution)):
`Start` and `Attach` name the display's device (`\\.\DISPLAY1`), which finds
it again when its
monitor handle went stale after a mode change; `DisplayModes`,
`SetDisplayMode` and `RestoreDisplayMode` requests, the last also on the
background pipe for the host card's Restore; and the worker's `restore
<session>` mode. A response carries the failure's kind instead of the client
reading it from the text, and the `Start` response carries what the worker's
backend can do (`caps`). A worker that exits with a mode still changed exits
with code 3. `Start` and the display-mode requests may take 20 s instead of
the 3 s of every other call, on both ends: a switch blocks while the display
retrains, and a reset may be two. A call that still overruns ends the
session, and the worker's backend, dropped with it, puts the mode back or
leaves it journalled.

Protocol 6 made the video possibly H.265, which a protocol 5 helper cannot
parse; `Request::H265` asks the worker whether it encodes H.265 on its
display, which it probes in the interactive session; and the worker answers
each input with the pointer as the input left it, so the host never reports
the controller's own move back (see [Input and cursor](#input-and-cursor)).
The worker accepts an encoder bitrate up to 80 Mbit/s. Protocol 5 added the
background host's `lan_first`, the app's *Try the local network first* when
sharing started, written only when off, so a grant saved with the default
stays readable by an older helper. The platform split
([Platforms](#platforms)) kept protocol 6: the request and response types
moved to the neutral `oop` module unchanged. A saved grant that does not say
`allow_direct` reads as allowed, the default everywhere.

Disable stops the service and disables its startup. Uninstall stops/deletes the
service and removes its authorization configuration. Upgrade preserves files
used by an older open UI until the next normal reboot; uninstall deletes what
it can and schedules in-use binaries for that reboot. Unique release paths keep
those delayed deletes separate from a later reinstall. No restart is initiated.
Bounded protected `installation.log`/`service.log`/`service-host.log`
diagnostics may remain; the last is the background host's own log
([Diagnostics](#diagnostics)).

Required UAC acceptance still includes both administrator consent and a
standard-user credential prompt, with OS version, account type, helper state,
default secure-desktop policy and observed failures recorded. Lock-screen password
entry for the same logged-in owner is implemented but unverified on a Windows VM.
Pre-login boot, secure-attention sequences and control across user switching are
unsupported. In particular, SendInput cannot generate Ctrl+Alt+Del, and no
SoftwareSASGeneration or other security policy is changed.

## SDK ownership and transport

`pb_mapper::desktop` comes from optional `pb-mapper-desktop`, above
`pb-mapper-client` in the crate DAG. Ordinary forwarding/relay builds do not
include desktop capture, codec or input dependencies. Flutter opts in; the SDK
does not require Flutter or a POCOX runtime.

- `Host::builder(HostOptions::new(service))` starts a host:
  `.identity(saved)` resumes an explicitly authorized `HostIdentity` (discard
  it and stop its host to revoke durable access), `.admission(..)` takes the
  write lease below, and `.backend(factory)` captures with an embedder's
  backend instead of the platform's; `Host::start(client, options)` is the
  plain case. The platform's own host refuses before registering where
  `host_support().host` is false (its `reason` says why: on neither Windows
  nor macOS 12.3 or later, without Media Foundation, or on a Mac without
  Screen Recording, which `host_support().permissions` and
  `request_permission` cover), or where `Access::Privileged` asks for a
  helper that is not ready (Windows only). `HostOptions` is non-exhaustive:
  `new` takes every path default from `RegisterOptions::default()`, so a
  host is reached on a shared
  LAN first (QUIC, or TCP when the viewer asks and `lan_tcp` allows), then
  directly, then through the relay. `display` is a `DisplaySelection`:
  primary, a runtime id, or the device name (`\\.\DISPLAY1`), which survives a
  reboot or a mode change. `lan_first` only orders the host's own probes,
  since the viewer dials and so chooses the path. The app passes its Settings
  switch, read when sharing starts. `Host::restart`, `switch_display` and
  `rename` host again under the same identity, source, admission and live
  control policy, the old registration stopped first;
  `privileged::background::adopt` hands a running host to the helper service
  under its invitation.
- `Host::status` returns a `HostStatus`: the state and its reason, whether
  control is allowed, the display and desktop kind, the capabilities, and the
  connected viewer (`HostViewer`: since when, direct or relay, controlling,
  paused, quality preset). `end_session` ends the current viewer's session and
  keeps hosting. `stop` ends hosting, and `stop_with` tells the viewer why
  (`restarting` for a display switch, or when Turn on remote control hands the
  app's sharing to a ready service under the same invitation; `revoked` for
  revoked trust, whether the app or the service hosts). A stop returns once the
  viewer has read `Stopped` and closed, or after 1 s.
  `set_control_allowed` revokes or restores control live. `Host::changes`
  wakes, carrying nothing, after anything `status` shows changed: a viewer
  arrived (before its backend has started) or left, a session's state,
  control, display, pause, quality or reported path, the control policy, or
  a stop; a burst wakes it once. A viewer's `since_ms` moves without a wake.
  A waiting host bumps nothing, so an embedder that shows the host needs no
  clock while nobody is connected. `Host::watch` samples `status` on a clock
  instead.
- A backend is constructed, used and dropped on its dedicated capture/input
  worker. `Backend` deliberately has no `Send` requirement: COM objects and
  desktop bindings remain on their owner thread. The factory itself is
  `Send + Sync`. Do not move a live backend between threads.
  `configure_video(limits, nominal_fps)` runs once per session and again only
  for a new desktop context, a display change or an encoder recovery, never for
  a rate change. `capture_video` drains finished output and submits at most one
  capture at the rate it is given; `drain_video` and `pending_video` let the
  worker collect an asynchronous encoder's output between ticks, and
  `video_full()` (false by default) names a bound of the encoder's own below
  the worker's, at which the worker skips a due tick without a capture, as
  at its own (a Mac host's VideoToolbox at two units in flight). Each unit's
  `VideoFormat` carries `gop`, the GOP the encoder read back, so a unit relayed
  by the helper reports it as well. `describe(probe_h265)` runs right after
  the backend is built and within the 3 s before the host answers the viewer:
  first unprobed, for the display it captures (which sizes the first IDR) and
  its `BackendCaps` (lock, the OS, display modes, physical keys, pointer
  shapes), from which the session's `Accepted`
  capabilities are built, and its encoder's `rate_model`, which the session's
  QoS starts from (`nominal` unless the encoder budgets per second; see
  [QoS](#qos-frame-rate-and-bits-per-frame)); then, only for a viewer that
  decodes H.265 while no H.265 encoder has failed in this process, probed, for
  H.265 alone, so a slow
  probe never costs the display. A description that misses the 3 s uses
  what its launcher falls back to. `bind(waker)` hands a pushing source the
  session's wake, and `drain_poll()` says whether thread C polls the encoder
  (`Some`, every 2 ms while units are pending) or waits to be woken (`None`).
  `submitted_video()` says whether the last capture took the tick, or spent
  it on a full encoder; one that had no new picture leaves the tick armed,
  and thread C asks a polled source again every `capture_poll()` (by default
  `drain_poll()`) while the desktop is active, and a period later while it
  is still. `submitted_picture()` says when the display presented the
  picture the last capture submitted and how many presents it covers (a
  `Presented`), which thread C anchors its next tick on; the default,
  `None`, keeps the even grid, as do repeats and undated pictures.
  `video_paused(paused)` is called once when every viewer has paused and
  once, before the next capture, when one resumes.
  `capture` (a JPEG still) is optional. `cursor` reports the pointer as the
  inputs the backend applied left it, and `cursor_shape`, asked only when the
  viewer listed `cursor_shape` and the backend's `BackendCaps::cursor_shapes`
  says it can, its shape when it changed (a `CursorShape`). A failure
  carries a `FailureKind` (`failure_kind(&error)`): secure desktop, display
  lost, encoder failed, permission missing (`missing_permission` says which),
  ...;
  a sticky link failure returns `NotConnected`, which ends the session instead
  of retrying. `native_backend(&selection)` is this platform's own backend, an
  opaque `NativeBackend` an embedder may decorate.
- `ControlAdmission::acquire` provides an optional embedder-controlled write
  lease. POCOX can pause its Computer Use writer when a human explicitly requests
  control. Lease drop releases admission on end-control, revocation, disconnect
  or local stop. Observation never calls this hook. Integrating that hook into
  POCOX itself remains separate work.
- `Viewer::connect(&client, &invitation, options, ViewerConfig { codecs, limits,
  quality, progress })` uses `Client::connect_tcp` and the existing
  registration, namespace, direct commit and pre-commit relay fallback; its
  `ConnectOptions.lan_first`, which the app takes from Settings at each
  connect, decides whether a shared LAN is tried first. A session decides
  its path once and keeps it: one that took the relay stays on it. A
  fallback that says the path cannot be punched (the punch after a grant
  found no way through, negotiating outran its 3 s, or the relay offers no
  direct) pauses direct for that client's later `connect_tcp` calls to the
  same computer once the relay has carried the session (10 s, doubling to
  5 min, cleared by a direct success or a network change), so a reconnect
  goes straight to the relay instead of spending the setup budgets (3 s to
  a grant, 2 s to punch) on direct again. A call that found the host away
  or without direct, or could not reach the relay, pauses nothing, so the
  reconnect after the host is back tries direct. The client keeps these
  pauses for 64 computers at most. It
  returns once the host accepted, or fails with a typed `ConnectError`: identity
  changed, version mismatch, busy, denied, invalid request, cancelled, transport
  or protocol. `VideoLimits::for_viewer(refresh_hz)` derives the limits:
  1920×1080 at the refresh rate, up to 120 FPS, and 60 FPS on Android whatever
  mode the phone reports at connect (it may be a power-saving 30 or 48 Hz; the
  presenter asks for the highest mode while the viewer shows).
  These are requested limits, not measured delivery or display guarantees.
  `set_quality` switches the preset live without reconnecting, and `pause` and
  `resume` stop and restart video while the viewer is hidden. The private
  loopback pair retains its bound listener until accepted. Established
  application streams do not reconnect or replay input automatically at the
  SDK transport layer. The UI owns a separate bounded recovery policy described
  below.
- Video has exactly one consumer: `viewer.video_sink().attach()` returns a
  `VideoConsumer`. Its `next(timeout, capacity)` blocks the calling thread,
  never a tokio worker, and returns a unit, `NeedCapacity` (the unit stays
  queued for a larger buffer), `Timeout`, `Paused` or `Closed`. The consumer
  reports each frame it actually showed with `presented(generation, frame_id)`,
  asks for an IDR with `keyframe(reason)`, reports a decoder failure and its
  text with `failed(message)` (logged, counted toward giving up, and appended
  to the session's end message), and names its decoder with `report`.
- A host's `invitation()` encodes as `pbd1.` text (`Invitation::encode`,
  `Invitation::decode`): the service, the SHA-256 fingerprint of the host
  certificate and the bearer token. Decoding also accepts the earlier JSON
  invitation and pins the same certificate.
- TLS 1.3 runs inside both direct and relayed tunnels, with the ALPN
  `pb-mapper/desktop/2`. The invitation pins the exact host certificate; rustls
  also checks its validity, name and signature. The separate random capability
  is sent only after host verification and is compared in constant time. TLS
  early data is disabled. A relay credential alone cannot authorize capture or
  input, and a failed invitation does not start a capture backend. A peer of
  another desktop protocol version fails the handshake with `VersionMismatch`
  ("update pb-mapper on both computers"), never mid-session.
- A session is one TLS connection, an ordered byte stream each way, and it
  carries everything: video gets no QUIC stream per frame, nor one apart from
  control. A lost packet holds back what follows it in that direction until
  QUIC or TCP retransmits it, but the transport keeps receiving and buffering
  those later bytes and delivers them the moment the retransmission lands. With
  an infinite GOP and no long-term references, a P-frame after a lost one
  cannot be decoded before the lost one arrives, whatever stream carries it.
  Only a keyframe or a JPEG picture, which refers to nothing before it, could
  reach the decoder sooner on a stream of its own; that rare case does not
  outweigh keeping probes in order with the video and the one stream the relay
  and LAN TCP can carry. Input,
  receipts and probe echoes travel the other way and never wait behind lost
  video, and probes stay in order with the video whose queue they measure. The
  relay pairs one stream per session, and a relay TCP leg or LAN TCP carries
  only one.
- Each direct UDP socket, the host's and the viewer's, asks the system for an
  8 MiB receive buffer when it binds, trying each size on a throwaway socket
  first and keeping only one that grows the buffer, halving down to 256 KiB and
  never asking for less than it has; Linux caps it at `net.core.rmem_max`, and
  the debug trace `direct UDP receive buffer` records what each socket got.
  Send buffers, the relay's sockets and congestion control (Quinn's Cubic) are
  unchanged.
- Input binds to a desktop/display generation and to a presentation the viewer
  validated less than two seconds ago. `Viewer::input(generation, &inputs)`
  sends a batch of up to 16, stamps the presented frame and consecutive
  sequence numbers itself, and fails with a typed `InputError` (`NotFresh`,
  `NotControlling`, `Congestion`, `Closed`, `Invalid`, and `NotOffered` for
  an `Input::PhysicalKey` toward a host that did not list `physical_keys`,
  nothing of the batch sent). `Input` and `Key` are `#[non_exhaustive]`. The
  host checks the same rule against its own ledger and rejects duplicate or
  reordered sequences.
  Permission revisions, desktop transitions and capture failures invalidate old
  input. Transport receipts, probes, cursor traffic and network heartbeats never
  grant or renew it.
- `Viewer::status` returns a `ViewerStatus` (state and reason, control,
  freshness, video, decoder and the statistics below), and `Viewer::view` an
  allocation-free per-frame snapshot (generation, presented frame, cursor,
  the number of its shape, and flags) for a renderer; `Viewer::cursor_shape`
  resolves that number to the `CursorShape` while the viewer holds it.
- Explicit `stop` cancels and joins the endpoint's worker/tasks and stops its
  registration/connection. Drop initiates cancellation. Coordinator loss or
  relay credential revocation does not forcibly close an established direct
  endpoint; local desktop stop does.

## Platforms

The host is split by platform at compile time; the engine, the session,
QoS, credit, the protocol, the viewer and the SDK name no operating system.
Windows and macOS are implemented. The macOS host is tested in CI, with
capture and view-only streaming also exercised on an M3 Pro. This
section is the guide for the next ones (Linux, Android), in the spirit of
RustDesk's per-platform services but monomorphised: no per-frame dynamic
dispatch.

### The two seams

- `Backend` (`backend.rs`, public) is a session's picture source and input
  sink, owned by its thread C and never `Send`. Embedders implement it; the
  helper's client (`oop::client::RemoteBackend`) and the platform glue
  implement it too. Thread C is `Engine<B: Backend, K: Clock>`, so every
  tick, input and clock read is a static call. The one dynamic call on a
  host's session path is `Launch::prepare`, once per admitted session
  (`launch.rs`); `Box<dyn Backend>` still works, through a forwarding impl
  that `clippy::missing_trait_methods` keeps complete.
- `Platform` (`platform/mod.rs`, crate-private) names a session's parts as
  associated types: `Desktop` (the binding and geometry, with a scope held
  around every OS call), `Video` (capture and encode fused, as a
  `VideoSource`), `Input` (an `Injector`), `Cursor`, `Modes`
  (`DisplayModes`; `NoModes` where a display's mode cannot change) and
  `Still`; plus `Grant`, consent and resources acquired once per host by an
  async `acquire` (a portal dialog, MediaProjection consent), never per
  session. `PlatformBackend<P>` (`native.rs`) is the neutral glue: the desktop
  checks around each capture, pointer read and input, the release obligation
  of held input (`input/held.rs`), input validation, pointer confinement
  (`input/cursor.rs`) and the video source's lifetime live there once.
  Compile-time facts are associated consts (`Injector::LOCK`,
  `Injector::PHYSICAL_KEYS`, `CursorSource::MODE`, `CursorSource::SHAPES`,
  `VideoSource::DRAIN` and `CAPTURE_POLL`, which an open source may change
  at run time through `drain_poll` and `capture_poll`); what varies by
  machine is a method (`can_lock`, `mode`, `available`), which `describe`
  reports per session. A `VideoSource` dates the picture it last submitted
  (`presented`), may serve new limits without a rebuild (`retune`), and
  hears when every viewer paused (`paused`). Failures carry
  a `FailureKind`, never a text to parse. `Injector::text` never sees `\n`,
  `\r` or `\t`: the neutral `Input::typed` cuts text into runs and taps Enter
  and Tab through the held-key obligation, so no platform can type a line
  break twice. `CursorShape::readable` bounds every pointer image before a
  pixel of it is read (1024 a side); a larger one is not read, and the
  viewer keeps the last shape. `CursorSource::shape` is asked only when the
  session negotiated `cursor_shape`, on thread C's 16 ms pointer tick (the
  helper's worker likewise), and only while the pointer shows on the shared
  display. It reads a bitmap only when the OS says the pointer's image
  changed, or 250 ms after a read that failed (`ShapeReads`), and returns
  physical pixels, straight RGBA8, at most 256 a side (`CursorShape::fitted`
  scales a larger one down).

`platform::Current` is the build's platform: `windows::Windows`,
`macos::Macos`, else `unsupported::Unsupported`, whose parts are uninhabited
and which refuses before a host registers (`HostSupport::reason` is
`platform`). The only platform `cfg`s above `platform/` are the viewer's
Android 60 FPS rule, the public `windows` module, the `privileged` facade's
Windows operations, the macOS Keychain sealer in `trust`, and lint
allowances for neutral code (`codec`, `oop`, the cursor scaler) that not
every platform uses. The pure parts of `platform/macos/` (points and pixels,
key codes, click counts, mode lists, un-premultiplying, when a sleeping
display is lit) build and are tested on every platform.

```
crates/pb-mapper-desktop/src/
  backend.rs  native.rs  launch.rs  modes.rs     the seams and the glue
  codec/{annexb,avcc,pipeline}.rs                Annex B and AVCC/HVCC, encoder rules
  input/{held,cursor}.rs                         release obligation, confinement, shapes, settling moves
  oop/{proto,ipc,client,server}.rs               the out-of-process backend
  platform/mod.rs  unsupported.rs  waiter.rs     traits, Current, facade
  platform/windows/{desktop,input,pointer,cursor,still,waiter,timer,mf,dpapi,modes,win32}.rs
  platform/windows/video/                        capture, scale and encode, fused
  platform/windows/helper/                       service, install, worker, pipes
  platform/macos/{mod,cf,capture,encoder,video,desktop,geometry,input,clicks,
    keys,pixels,pointer,cursor,modes,mode_list,still,timer,waiter,power,wake,
    keychain}.rs
  privileged/                                    public helper facade
```

| Part | Windows | macOS |
| --- | --- | --- |
| `Desktop` | `WinDesktop`: `OpenInputDesktop` checks on the ordinary desktop, or the helper worker's Winlogon binding; `GetMonitorInfoW` geometry, found again by `\\.\DISPLAYn` when the handle went stale; per-monitor DPI scope | `CGDirectDisplayID` from `CGGetActiveDisplayList`, named by its UUID, which finds it again; `CGDisplayBounds` × the current mode's pixels per point; a session that is not on the console, or a locked screen, is the secure desktop; capture and the pointer reuse a check that passed within 16 ms until the session's own mode change, input never |
| `Video` | the DXGI → D3D11 video processor → Media Foundation encoder, one fused object (`platform/windows/video/`) that thread C polls: drained every 2 ms while it holds units, and, after a capture with no new present, asked again every 2 ms for 250 ms after an injected input or a new picture, and once a period otherwise | ScreenCaptureKit (NV12, scaled by the system to the encoder's size, no cursor, queue depth 6) → `VTCompressionSession`, zero copy, one fused object that pushes (`DRAIN = None`): the output callback queues the unit and wakes thread C |
| `Input` | enigo; `SendInput` with `KEYEVENTF_SCANCODE` for physical keys (a HID usage to its set-1 scancode); absolute moves as one `SendInput` event each (see [Pointer moves on Windows](#pointer-moves-on-windows)); `LockWorkStation` | `CGEvent` at the HID tap: pointer and wheel in the bound display's points, click counts for double clicks, named letters and digits at the key the Mac's layout types them with, other keys and physical keys by `kVK_*` place, modifiers as flag changes with left and right apart, text as Unicode string events of at most 20 UTF-16 units; no lock (`LOCK = false`) |
| `Cursor` | `GetCursorInfo`, separate, the injected pixel until it shows a move; the shape once per new `HCURSOR`: its kind from the `LoadCursorW` standard handles, its bitmap from `GetIconInfo` and `GetDIBits` (colour, alpha or AND mask; monochrome AND/XOR, an inverting pixel drawn black outlined in white) | `CGEventGetLocation`, separate, the injected pixel until it shows a move; the shape when the window server's cursor seed moves (a private `CGSCurrentCursorSeed`, looked up with `dlsym`; without it at most every 500 ms): `NSCursor.currentSystemCursor` drawn at the display's scale, a kind only where it equals a standard cursor pixel for pixel, else `custom` |
| `Modes` / `Still` | `WinModes` (dynamic changes only; see [Remote resolution](#remote-resolution)) / xcap JPEG | Core Graphics modes for this app only (`kCGConfigureForAppOnly`) / `SCScreenshotManager` and ImageIO JPEG, macOS 14 and later |
| `Grant` | none | the Screen Recording grant; Accessibility is checked at each input |

### Pointer moves on Windows

A Windows host injects each move as input: one `SendInput` event with
`MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`, and
`MOUSEEVENTF_MOVE_NOCOALESCE` while a button it pressed is held, marked as
its other input is. Low-level mouse hooks and raw-input readers see it, the
idle timer counts it, and while a button is held Windows posts each move
rather than merging them into the next `WM_MOUSEMOVE`, so a stroke drawn
remotely keeps its points. The cost is the other side of that: a program
that handles each `WM_MOUSEMOVE` slowly while a button is held (a heavy
window resized by its frame) gets every move too, where a physical mouse's
moves merge, and can go on moving after the stroke ends; how much it trails,
if at all, has not been measured on a device. Raw input gets it as an
absolute move (`MOUSE_MOVE_ABSOLUTE`), not as a delta. Through 0.12.1 the
host placed the pointer with `SetCursorPos`, which is no input at all. The
pixel is scaled to 0–65535 over the virtual desktop that `GetSystemMetrics`
reports at that move, in thread C's per-monitor DPI scope: the least value
that falls on that pixel when Windows truncates the scaled coordinate, which
falls there too when it rounds, as a unit test checks for every pixel of
1920-, 3840- and 7680-pixel axes. The system applies a move after
`SendInput` returns, so the cursor reports the injected pixel until
`GetCursorInfo` shows it or 250 ms pass after the last move, as a Mac host
does after `CGEventPost`; the controller's own move still comes back as its
echo.

`SendInput` takes nothing while another thread blocks input or when the
host's thread may not send input to the input desktop, and UIPI drops a
host's input to a window of higher integrity (an elevated program in front
of an app that is not elevated), which `SendInput` need not report. A move
`SendInput` refuses is placed with `SetCursorPos` at once, and so is every
move after it while the window then in the foreground stays there. A dropped
move shows only when its settle window ends: if the pointer is still where
the read before the window's first move found it, more than a pixel from a
target inside the cursor's clip, the moves never took effect, and the host
places the pointer for every move from then on while the window then in the
foreground stays there. Over an elevated window, then, a stroke leaves the
host's pointer where it was until the controller's pointer first rests for
250 ms, and the controller's pointer goes back to it; the moves after that
arrive. Either way the host warns `desktop_moves_placed` once for that
window, with its token's session, integrity level and UIAccess and the Win32
error of a refusal. A pointer that moved during the window (a move that
landed a pixel off, the host's own mouse) says nothing of the moves. A
target outside the clip (`ClipCursor`, a game's window) is a clamp, which
placing would not change. A program that puts the pointer back itself, as a
game that recentres it does, can look the same; its window then gets placed
moves, as every window did before. The desktop service's worker runs as
SYSTEM, above every window's integrity level, with the same injector.

### Adding an operating system

1. `platform/<os>/` implementing `Platform` and its parts; dependencies under
   `[target.'cfg(..)'.dependencies]`.
2. An arm of `Current` (Linux instead picks X11 or Wayland once per host).
3. Its `HostSupport`: whether it can host now, and why not.
4. A CI job running the crate's tests on that OS, and an acceptance
   procedure on hardware.
5. New wire vocabulary only behind a capability: `os_<os>` already exists;
   a port adds what it needs, such as `cursor_embedded`, `PermissionMissing`
   or `ConsentRevoked` states, or a portal's display picker.

A capture and encoder that are separate on a platform join statically:
`Pipeline<C: Capturer, E: Encoder<Frame = C::Frame, Device = C::Device>>`
implements `VideoSource`, so a CPU copy between them would be a type error.
These traits arrive with the first split platform (X11), not before:

```rust
trait Capturer { type Grant; type Frame; type Device;
    fn open(grant: &Self::Grant, display: &Display, waker: &Waker) -> Result<Self>;
    fn device(&self) -> &Self::Device;
    fn acquire(&mut self) -> Result<Acquired<Self::Frame>>; } // New | Unchanged | WouldBlock
trait Encoder { type Frame; type Device; const DRAIN: Option<Duration>;
    fn encodes(device: &Self::Device, codec: VideoCodec) -> bool;
    fn open(device: &Self::Device, size: VideoSize, codec: VideoCodec,
            limits: VideoLimits, rate: VideoRate) -> Result<Self>;
    fn set_rate(&mut self, rate: VideoRate) -> Result<()>;
    fn submit(&mut self, frame: &Self::Frame, keyframe: bool, duplicate: bool) -> Result<()>;
    fn drain(&mut self) -> Result<Option<EncodedFrame>>;
    fn pending(&self) -> usize; fn codec(&self) -> VideoCodec; }
```

- **Linux, X11** (the first split platform): MIT-SHM capture plus XDamage,
  pulled like Windows; VA-API encode (openh264 as the fallback); input through
  enigo's XTest backend and `XWarpPointer`; RandR modes through x11rb; xcap
  stills. Lock is per desktop environment, so `can_lock` asks at run time.
- **Linux, Wayland**: one portal session (ScreenCast and RemoteDesktop) as the
  `Grant`, with a restore token sealed by the host's trust store so the dialog
  appears once; PipeWire DMA-BUFs into VA-API (or one GStreamer pipeline), a
  pushing source (`DRAIN = None`) that wakes thread C; input through the
  portal or libei; the cursor embedded or separate as the portal grants;
  `NoModes`; the portal picks the display.
- **macOS** is implemented (the table above), as planned here except that
  keys are posted from thread C rather than on the main queue, so no enigo.
  Named letters and digits go through a table of the Mac's layout
  (`platform/macos/layout.rs`): the Text Input Sources calls run on the main
  thread only, so the main queue reads it at each session's start, which
  waits for it at most 200 ms, and again at each input-source switch, which
  a distributed notification on the main run loop says; thread C only loads
  the table. A key goes up where it went down. Without a main run loop (a
  command-line SDK host), or until the read arrives, named keys keep their
  ANSI places. Its `HostSupport` carries
  `permissions`, and a missing one is `PermissionMissing`; the shared encoder
  rules (late output, first-submit and output waits, liveness, H.265 failure)
  live in `codec/pipeline.rs` for both platforms. Thread C waits on a kqueue
  timer marked critical (`platform/macos/waiter.rs`), as it waits on a
  high-resolution waitable timer on Windows: see
  [Timers on a Mac host](#timers-on-a-mac-host).
- **Android**: MediaProjection's VirtualDisplay writes into MediaCodec's input
  surface, so the platform implements `VideoSource` directly and has no
  still (`NoStill`) and no modes; input through an AccessibilityService;
  no cursor (`CursorMode::None`); consent once per host, as the `Grant`.

A privileged agent on another OS (a root systemd unit for the greeter, a
LaunchDaemon with a LaunchAgent) serves `PlatformBackend<P>` through `oop`
over its own transport and peer check; the protocol, framing, client and
server are already neutral and tested over an in-memory duplex on every
target. Hosting on a new OS needs nothing from viewers; viewing on one needs a
native decoder in that Flutter runner, attached to the existing video consumer
exports.

## Video pipeline and performance model

The pipeline keeps a slow phone, decoder or display from stalling the host, and
sends no faster than the network delivers. It follows RustDesk's `video_qos.rs`
where that fits. Seven rules carry it:

1. **Send credit comes from transport receipts, never from presentation.** The
   viewer's network read task writes a cumulative receipt as soon as a video
   record has been read, including one its sink later drops. Nothing between
   the network and the screen (FFI, Dart, decoder, compositor) holds credit
   back.
2. **Capture runs at the QoS pace, anchored on the display's presents.** It
   never pauses or resumes as flow control. A tick is skipped only while two
   units wait for send credit or the encoder holds four (two on a Mac
   host's VideoToolbox). A picture the
   backend dates places the next tick a period after its present, up to a
   quarter period early and at most 1/64 period earlier than a period after
   the last tick, and only a picture of a single present moves it earlier:
   the long-run rate stays within 64/63 of the pace, and a display at least
   twice as fast as the pace is taken at the pace. An undated picture, a
   repeat or a skip keeps an even grid. After a stall two captures are never
   closer than three quarters of a period.
3. **QoS changes frame rate and bitrate only, from probe delay.** A probe once a
   second, four a second on a short path, is answered by the viewer's network
   task. Resolution never adapts: it changes only with a new session, a display
   switch or an explicit choice.
4. **Infinite GOP.** IDRs are sent only on demand and are rate-limited on both
   sides. Requests are deferred and coalesced, never dropped.
5. **The viewer owns presentation lag.** Its sink keeps only the newest
   decodable frames; when the decoder falls behind it asks for an IDR and
   drains what queued until that IDR supersedes it, and it caps the host's
   frame rate below what the decoder can take.
6. **Input is authorised only by presentation.** A validated presentation is
   the only thing that admits input. Input is drained ahead of capture on the
   one desktop-bound thread, and the receive task never waits for it.
7. **The cursor leaves the video path.** It is sampled separately and never
   echoed to the operator who is moving it.

These bounds keep the pipeline responsive; they do not establish a fixed frame
rate on arbitrary hardware or networks.

### Host: capture and encoding

Each session has one desktop-bound thread, which owns the backend, its encoder,
input injection and the desktop binding, and two network tasks: one reads the
viewer's messages and never waits for that thread, and one writes video,
cursor, probes and statistics. A blocked write of a large IDR never delays the
reader.

The thread drains the input lane, makes at most one capture or drain call, and
drains the lane again. Capture is paced at the QoS pace (`pacer.rs`): a tick
is consumed whether it fires or is skipped, and after a stall two captures
are never closer than three quarters of a period. A capture whose picture
the backend dated (`Backend::submitted_picture`) anchors the next tick on
that picture's present, as airhand's pacer does: the next may be taken up to
a quarter period before the tick, and the tick is a period after the
present, but never more than 1/64 period earlier than a period after the
last one. A display presenting at the pace, or slightly faster or slower,
is therefore taken as each present lands; a picture of a single present that
had to wait for the early window moves the timeline 1/64 period earlier per
picture until the presents land in it, so the long-run rate stays within
64/63 of the pace. A picture covering several presents that waited, from a
display faster than the pace, moves nothing: a display at least twice as
fast is taken at the pace itself, and one between once and twice as fast at
up to 64/63 of it. Without dates, on an even grid, a display a little faster
than the pace drifts ahead of its ticks, each picture waiting longer, until
one is replaced before a capture takes it. A skip, a repeat and an undated
picture keep the even grid, which keeps its long-run rate (120 FPS on a
165 Hz display is not rounded down). In process, a Windows host dates a
picture by its present and a Mac host by its arrival from ScreenCaptureKit;
the helper's answer carries no date yet, so a host behind the helper keeps
the grid. A tick that finds no new picture stays armed, so the next picture
is taken as it appears rather than a tick later. ScreenCaptureKit, which
pushes, wakes the thread for it. DXGI cannot, in the app's process or
behind the helper (whose answer says when a capture found nothing new), so
it is asked again, never at once: every 2 ms (`DRAIN_POLL`, or the
backend's `capture_poll`) for 250 ms (`HOT_WINDOW`) after an input the
thread injected or a new picture, when the next present is likely, and
once a period otherwise, so a still desktop costs no more capture calls
than the grid. An injected input also brings a poll due a period later in to
2 ms. Anchoring adds such polls on an active desktop: after a dated picture
the next tick opens three quarters of a period later, and DXGI is asked
every 2 ms from then until the next present lands, where a grid at the
display's own rate mostly found a present already waiting at its tick.
`desktop_capture_pacing`'s `empty_polls_per_s` and `video_bench`'s
`polls_per_s` count them. A change of the viewers' limits that changes only
the frame rate keeps the capture source where it can serve the new limits as
it is (the synthetic test platform's can; the Windows and Mac sources are
still built again), and asks for an IDR either way. Thread C passes each
change of limits to the backend as it happens, during an outage too, so a
recovery, which repeats the current limits, always builds a fresh source.
A capture that finds
the encoder without room spends its tick. A tick is skipped (and
counted in `skipped_ticks`) only while two units in the hand-off queue wait for
send credit, which is transport backpressure, or while the encoder (or the
helper) holds four, or a bound of the source's own below four
(`Backend::video_full`, a Mac host's two below), never for presentation.
The two bounds are separate
because a unit stays in the encoder for its latency: with one shared bound of
two, an encoder that kept each unit 40 ms delivered 40 FPS at a pace of 60, and
one that kept it 60 ms delivered 30. Four cover a latency of nearly four
periods, and a fast encoder never holds more than one, so it adds no latency.
The bound is fixed rather than derived from the measured dwell, because a
queueing encoder's dwell grows with its depth. A Mac host holds VideoToolbox
to two units in flight (`VT_IN_FLIGHT`), finished units not counted, and
thread C skips a tick that finds two as one that finds four: where
it encodes asynchronously (a size that is not a multiple of 16, such as a
MacBook's built-in display at its default resolution) and the pace outruns
it, the extra two only queued. On an M6 at a pace of 120, in a test loop
that paces as thread C does, a 3024x1964 session's units took 18 ms (H.264)
and 17 ms (H.265) from submit to output at the median with two in flight,
against 41 and 39 ms with four, and a 3340x2160 session's 23 and 21 ms
against 49 and 47 ms, at the same frame rate. Where it encodes inside the call,
one unit is in flight at a time either way
([measurements](benchmarks/2026-11-24-macos-vt-submit/README.md)). The
encoder measures each
unit's dwell, from submit to output (the `encode_us` it returns with the
unit), and thread C reports the median of the last second as
`encoder_dwell_ms`. A static screen is resubmitted about every 250 ms as a
duplicate, which unique-frame rates exclude. A Mac host does not leave a
last picture its encoder dropped to that repeat: it sends it again at the
next tick, as a new picture
([VideoToolbox rate control on a Mac host](#videotoolbox-rate-control-on-a-mac-host)). The hand-off queue holds at most
six units and 8 MiB, which both bounds together never reach. When the queue's oldest unit
has waited more than a second, the queue is dropped and replaced by an IDR.
The wait counts from when the unit was ready or from the last IDR
receipt, whichever is later, and time spent behind an IDR still in flight
never counts, since that IDR is already the resync. The desktop context is
checked at most every 100 ms and around each capture and pointer read; the
injection boundary re-checks it for every input. On a Mac each check reads the
login's session and the display's geometry from WindowServer, so the checks
around a capture or a pointer read take one that passed within the last 16 ms
as their answer. A failed check is never reused; every input, the 100 ms check
and a display mode change read WindowServer anew, and the session's own mode
change or restore leaves nothing to reuse. A lock, another login taking the
screen, or a display changed from outside the session, can therefore stop
capture and the pointer up to 16 ms later than input. A test loop that made the
same reads through the same reuse, paced as a moving picture at 60 FPS with the
pointer sampled, read WindowServer for 61 of its 245 checks a second instead of
all of them; it ran on a Mac whose screen was locked, so it stood in for
`MacDesktop`'s own check, which a lock fails, and a run on an unlocked Mac is
still to be made
([measurements](benchmarks/2026-11-24-mac-session-check/README.md)). A failure
that ends streaming may be a desktop switch, so it is retried at once. A
failure before any unit since the last recovery is retried every 200 ms, and
each retry captures right after its context check.

Windows hosting uses DXGI Desktop Duplication, D3D11 textures and GPU NV12
conversion, followed by a low-latency Media Foundation hardware encoder, H.265
or H.264 (see [H.265 and H.264](#h265-and-h264)), with no B-frame reordering.
Each duplication frame is released at once, so no update is missed between
ticks. A new image is dated by `LastPresentTime`, a performance counter
reading turned into an `Instant` by its age against
`QueryPerformanceCounter` (one over a second old dates nothing), and the
presents DXGI folded into it (`AccumulatedFrames`) are summed while it waits
for the encoder; a fresh picture's sample time and duration follow its
present, kept strictly increasing, and a repeat's follow the submit. The
first image of a new duplication is copied whatever `LastPresentTime` says,
and left undated, so a still desktop shows at once. Hosting requires Media
Foundation and a compatible hardware H.264 MFT on the capture GPU, and
reports an error when that path is unavailable; without Media Foundation
the capabilities report no host, a host refuses to start, and the app
restores no remembered sharing grant (it keeps it for a start where Media
Foundation is installed) and retries nothing. H.265 is used
only where the GPU encodes it. JPEG remains an SDK compatibility path at up to
8 FPS; there is no promise that every hardware failure silently falls back to
JPEG. The encoder runs at the session's nominal frame rate (the viewer's
`max_fps`) and is never rebuilt for a frame rate, bitrate or preset change:
the bitrate is set live, and sample times follow the real interval between
presents.
GPU scaling preserves aspect ratio and never upscales; input and cursor
coordinates still use the physical `Display` geometry.

macOS hosting uses
ScreenCaptureKit, which scales the
display to the encoder's size and delivers NV12 (BT.709) without the cursor;
its output queue keeps the newest complete picture, with when it arrived,
and wakes thread C when one becomes available; that arrival dates the
picture for thread C's pacing. Thread C submits that IOSurface at the
negotiated pace without a copy. On a still screen it can reuse the last
picture for a requested keyframe or the 250 ms liveness repeat. Thread C
only hands the picture over: each encoder has its own submit thread, at the
default QoS class as thread C is, which makes every call on
the VideoToolbox session, the encode with its `ForceKeyFrame` option and the
bitrate with its cap, one at a time and with no lock held across a call,
and thread C passes it a picture or a bitrate through a one-slot mailbox. A
session whose size is a multiple of 16 (1440p, 4K) encodes inside
`VTCompressionSessionEncodeFrame` and calls back before it returns, so before
the submit thread the call held thread C for the whole encode: on an M6, at
the median, 5.0–5.2 ms at 1920x1200, 7.2–7.5 ms at 2560x1440 and
14.3–15.0 ms at 3840x2160, against 3–5 µs to hand the picture over now. A
submit that finds the submit thread still busy (a picture in the mailbox,
or a call under way) leaves its tick armed and any keyframe asked for, and
counts as an empty poll in `desktop_capture_pacing`; the thread wakes thread C
when it finishes that call, and thread C takes the newest picture then
rather than at the next tick, so a session paced faster than the encoder
gets the encoder's rate (3840x2160 paced at 120 gave 65 units a second with
H.264 and 68 with H.265, within 1% of what the blocking call gave). Until
that wake, the source names a time
250 ms away as its next tick, so thread C waits for it rather than spin. At
most two units are in flight in VideoToolbox (`VT_IN_FLIGHT`, the one in
the mailbox among them; a unit out and not yet drained is not), and thread
C asks the source before it captures and skips a tick that finds two,
counted in `skipped_ticks` as at the gate above. A new bitrate is set by
the submit thread before its next encode, or at once while it idles, and a
bitrate or encode VideoToolbox refuses reaches thread C at its next drain as
the codec's failure. Dropping the encoder lets the submit thread finish its
call, drop a picture still in the mailbox, invalidate the session and end;
the drop waits at most 250 ms, after which a thread still inside a call is
left to finish on its own (`macos_encoder_left_running`), holding what its
callbacks read. The session
runs in real time with no frame reordering, an unbounded GOP (`gop` reports the
read-back), IDRs as a per-frame `ForceKeyFrame` option and the bitrate set
live. Both codecs request low-latency rate control when available and retry
with standard rate control if VideoToolbox rejects the configuration. Low
latency may give a picture a QP up to 51, and a newest picture VideoToolbox
dropped is sent again at the next tick
([VideoToolbox rate control on a Mac host](#videotoolbox-rate-control-on-a-mac-host)).
H.265 needs the hardware encoder. H.264 prefers it and takes VideoToolbox's
software encoder where there is none, as on a virtual Mac. The output
callback converts the length-prefixed sample to Annex B, with the parameter
sets in front of a keyframe, queues the unit and wakes thread C, so nothing
polls. The late-output, stall, liveness and H.265-failure rules are the
Windows encoder's (`codec/pipeline.rs`). No picture for 3 s is a capture
failure. While a session captures, the process holds off App Nap and keeps
the display awake, and thread C waits on timers the kernel does not coalesce
([Timers on a Mac host](#timers-on-a-mac-host)); a display that sleeps when
capture starts or resumes is lit first, and one that turns off under the
stream is lit again
([Waking a sleeping Mac display](#waking-a-sleeping-mac-display)).

The GOP is infinite. Before its media types are set, the encoder tries GOP
lengths `u32::MAX`, `0x7FFF_FFFF`, 65 535 and 3 600 and keeps the first one it
reads back unchanged, so a vendor that clamps or ignores the setting is
detected; the read-back is reported as `gop`. An IDR comes with a new encoder
(session, display or context change, recovery), a new generation (a control
grant or revoke, an input failure, a new desktop, or a capture failure that
ended streaming), a resume, a viewer's request, or a hand-off queue that
overflowed or went stale. Requests other than a resume or a new generation
wait until 300 ms after the previous IDR. A request is answered once its IDR
is handed to the encoder, so an IDR still being encoded is never asked for
twice. Encoder output that took more than 250 ms is discarded, and output
resumes at the next IDR. The time counts from the unit's submit or from the
output before it, whichever is later: the encoder serves units in order, so a
unit is not late for waiting behind the others the gate admits, and a slow
encoder delivers fewer frames rather than none. Nothing periodic, and no frame
rate, bitrate, preset or congestion change, produces an IDR.

In a process with no visible window, which is the background service and the
app when hidden, Windows 11 fires every timer on its 15.6 ms tick and ignores
`timeBeginPeriod` unless the process first opts out of power throttling for
timer resolution. That is every tokio timer: the writer's credit recheck and
QUIC's pacing and ACK timers. While a session captures, its thread C holds a
process-wide guard that opts out of timer and execution-speed throttling and
asks for 1 ms; the helper's capture worker holds one while the host asks it
for video. The last guard released restores the earlier throttling state and
the period, so a paused or idle host costs no power, and a failed call only
leaves timers coarse. Measured here, a 2 ms tokio sleep took 15.7 ms, 3.5 ms
under the guard and 15.6 ms after it. Thread C paces on its own
high-resolution waitable timer either way.

While a session captures, a Windows host also keeps the display and the system
awake (`platform/windows/power.rs`). A dark display gives Desktop Duplication
no new pictures, so a viewer that only watches would freeze at the owner's
display-off timeout; a laptop that sleeps ends the session; and a viewer's
pointer moves (`SetCursorPos`) are not input, so they reset neither idle
timer. Each capture call (the encoder's open and submit, or a JPEG still) and
each read of the pointer records its time with one atomic store, in the app's
process and in the helper's capture worker, which runs in the owner's console
session. A watchdog thread, started by the first capture and parked with no
timeout while nothing captures, takes one power request with
`PowerRequestDisplayRequired` and `PowerRequestSystemRequired` at the first
capture after an idle spell. It turns a dark display on once at the same time,
with `SetThreadExecutionState(ES_DISPLAY_REQUIRED)` and no `ES_CONTINUOUS`, so
that a session that starts or resumes on one gets its first picture without
anyone at the host. It clears the request 2 s after the last capture
(`awake::LINGER`): a pause, a disconnect or a lost helper. While a session runs
unpaused, thread C asks for a capture at least once a second, and reads the
pointer every 16 ms (through the helper, the worker reads it at each desktop
check and video request) even while the network or the encoder holds capture
back, so a stall does not let the request lapse. When to take and clear it is
`awake::Watchdog`, which the unit tests drive through whole sessions with an
injected clock. `powercfg /requests` lists the request under DISPLAY and
SYSTEM while it is held. A sleep the owner chooses, the power button or a lid
set to sleep still end it. The helper protocol is unchanged: its worker runs
the same capture.

### Waking a sleeping Mac display

The activity a capturing session holds keeps the display from sleeping; it
does not light one that already sleeps, as when the owner let it sleep, put
it to sleep, or a pause released the activity. A sleeping display gives
ScreenCaptureKit no picture, and a viewer gets input only for a picture it
was shown, so such a session would show nothing, and rebuild its capture,
until someone touched the Mac; a display put to sleep under a running
stream would leave its viewer on the last picture, repeated. A sleeping
display is not among the active displays (`CGGetActiveDisplayList`), but it
is still online and keeps its bounds and its mode. The host lights it by
declaring the local user active (`IOPMAssertionDeclareUserActivity` with
`kIOPMUserActiveLocal`, as `caffeinate -u` does), which turns the displays
on and holds off their sleep for the owner's display-sleep time
(`platform/macos/power.rs`):

- when a session's parts open, if any online display sleeps; it then waits
  up to 2 s, looking every 100 ms, for the shared display to be active and
  awake, so that capture starts with a picture. The wait stays under the 3 s
  a new session has to describe itself;
- when capture opens, a recovery's included, and at the first capture after
  a resume: a sleeping display fails the capture as `CaptureFailed` ("the
  display is asleep; waking it"), and thread C's retry, every 200 ms, opens
  ScreenCaptureKit once the display draws;
- when an open capture got no picture for 3 s, or ScreenCaptureKit did not
  list the display, while it sleeps: the same failure and retry;
- when an open stream had no new picture for 500 ms: at a repeat of its last
  picture, at most every 500 ms, it looks whether the display sleeps (one
  Core Graphics read while it is awake), and lights it with the same
  failure and retry;
- before each JPEG still, for a viewer with no decoder: a sleeping display
  fails the still the same way.

Each of these runs only after the session check passed: a locked Mac, or
one whose screen another login has, stays dark, as its picture is the
secure desktop. A display that turns off under a running capture (found
asleep within 1 s of being found awake by a new picture, a look or a
still) may be turning off as the Mac locks, when macOS asks for the
password as soon as the display turns off. It is lit only once it has been
found asleep for 1 s, each look after a session check, which gives a lock
that follows the display turning off a second to be reported first; how
soon macOS reports it is still to be tried on a device. Until then the
stream repeats its last picture, and a still fails. The rules are pure
(`platform/macos/wake.rs`) and tested on every platform: a declaration goes
out at most once every 2 s, however often capture retries, and none is made
for the built-in panel while the lid is closed (`AppleClamshellState` of
`IOPMrootDomain`), which macOS would light for a few seconds and turn off
again, over and over; an external display is still lit. The power
manager's assertion id is kept and passed back with each declaration, as
its header asks, and released with the session's activity once the
declaration is 2 s old; one younger, whose display may still be lighting,
is left to end at the power manager's own timeout. Each declaration is
logged as `desktop_display_wake` (debug, target
`pb_mapper_desktop::session`), and one the power manager refuses as
`desktop_display_wake_failed`, with its `IOReturn`, as a warning.

### Timers on a Mac host

macOS coalesces timers: the kernel may fire one late by a share of its
interval. For the test processes of a GitHub `macos-latest` runner (a
virtual Mac, not a real Mac), which no app puts in front, that share was
several times the interval, over four CI rounds:

| Wait | Ended late, median | Ended late, worst seen |
| --- | --- | --- |
| 2 ms, condition variable or `thread::sleep` | 5.5–16 ms | 114 ms |
| 16.7 ms (a 60 fps tick), condition variable | 7.8–107 ms | 134 ms |
| 2 ms, tokio sleep | 8.6–17 ms | 35 ms |
| 16.7 ms, tokio sleep | 18–96 ms | 136 ms |
| 2 ms, kqueue timer marked critical | 0.04–1.1 ms | 1.8 ms |
| 16.7 ms, kqueue timer marked critical | 0.06–0.42 ms | 3.6 ms |

On the condition variable thread C paced a 60 fps session at 13–28 frames a
second. An `NSProcessInfo` activity with `LatencyCritical`, an override of
the task's latency QoS to tier 0 (accepted, with the task's base tiers
unspecified) and the thread's QoS class up to user-interactive changed none
of it. A kqueue timer marked `NOTE_CRITICAL` is exempt from coalescing, so
thread C waits on one, with a user event for the wake
(`platform/macos/waiter.rs`); on the runner the session then paced at
60 frames a second, a capture every 16.7 ms at the median. The timer guard
(`platform/macos/timer.rs`) holds the activity, which keeps App Nap off and
the display awake; it does not stop coalescing.

tokio's timers stay coalesced. On the relay path that costs no frames:
B-send writes video when the hand-off notifies it, and probes and receipts
are stamped with the time they are written and read, so a late timer never
counts as network delay. What runs late is periodic: the QoS tick (250 ms),
the statistics, the spacing of probes and the credit recheck, a fallback
behind receipts. QUIC's pacing, ACK and loss timers, on a direct session or a
relay reached over QUIC, can run late too, which would slow a large IDR or a
retransmission. How late any of these run on a real Mac, with the app in
front, behind other windows or hidden, is not measured.

A Mac viewer that is not the frontmost app may see the same lateness in its
presentation throttle, its Dart input timers and its decoder thread's waits;
that is not measured either.

### VideoToolbox rate control on a Mac host

A Mac host encodes with VideoToolbox's low-latency rate control, which
drops a picture it cannot fit into its budget at its highest QP.

**The QP bound.** A low-latency session sets
`kVTCompressionPropertyKey_MaxAllowedFrameQP` to 51, the top of the 8-bit
H.264 and H.265 range; left to its own bound, low latency drops pictures
that this bound lets it code. The key is of macOS 12.0 and is looked up
at run time; an encoder that refuses it keeps its own bound. The fallback,
standard rate control, does not set it. `macos_encoder_opened` logs whether
the session took it (`max_qp`).

In a test loop on an M6, fresh full-screen noise at 1920x1080, 4 Mbit/s and
60 FPS dropped 90 of 180 pictures with the bound. Without it, H.264 dropped
169 and H.265 135. At 2560x1440 the counts were 90 against 158 to 162. At
120 FPS the bound likewise kept about every other picture, against 22 to 90
of about 360 without it. Scrolling text, and the busy pattern (text with
image blocks under fresh grain), dropped none either way in steady state.
In the 1.5 s after an IDR they dropped about as many either way, 15 to 37
pictures a window, while noise dropped half its pictures with the bound
against about three quarters without it.

The host counts the pictures VideoToolbox drops. Thread C traces them as
`macos_encoder_dropped`, at most once a second, from its drain.

**Sending a dropped picture again.** While low latency pays an IDR back on a
slow link, it drops about every other picture. A later picture replaces a
dropped one. When the dropped one was the last change (a typed character,
the end of a scroll), nothing replaces it, and the viewer would keep the
picture before until the 250 ms liveness repeat.

So when VideoToolbox drops the newest unit handed over, the encoder wakes
thread C, and the next tick sends the last picture again. The rules
(`codec/resubmit.rs`):

- Only the newest unit counts. A drop with a later unit in flight, the
  mailbox's included, is left to that unit, and so is one reported just
  before thread C hands the next unit over.
- A picture the encoder kept before is never sent again.
- A picture is sent again at most twice; a new picture starts the count
  again. After that the liveness repeat carries it.
- A newer picture that arrived meanwhile goes instead.

Which unit is a duplicate is decided as the encoder keeps it: only one whose
picture a unit before it carried out. A resubmit, or a repeat of a picture
whose every unit was dropped, is the viewer's first sight of that picture,
and counts toward its `received_fps` and the host's unique frames.

In the test loop: an IDR of a still 1920x1080 screen of text, at 60 FPS,
then 1 to 12 new pictures. At 1 Mbit/s the last new picture took 250 ms or
more, or never came within 600 ms, in 5 of 12 trials instead of 10, for
each codec. In 5 trials a resubmit brought it within 23 to 40 ms. At 2
Mbit/s, 1 trial of 12 took that long before and after, and at 4 Mbit/s
none did.

**What was measured and left as it was.** Three further changes were
measured on the same Mac and not made:

- *Starting a session at `timestamp`.* Low-latency H.264 budgets per second.
  Low-latency H.265, set to bits per frame × the pace, spent 0.02 to 0.04 of
  its budget at 5 FPS at 1440p and at 10 and 5 FPS at 4K, and so stopped
  changing the picture. Under `timestamp` an IDR at 10 FPS also dropped 16
  to 18 of the 20 pictures after it. Mac sessions still start at `nominal`;
  see [Encoder rate model per GPU](#encoder-rate-model-per-gpu).
- *Standard rate control above low latency's knee.* At a nominal 120 FPS,
  low latency held 119 to 120 pictures a second at 2560x1440, 106 to 113
  at 2560x1664, 91 to 93 at 3024x1964 and 60 to 63 at 3840x2160. Without
  the data-rate cap, standard rate control held 118 to 120 at every size
  but on the busy pattern at 4K (103 to 104). But, set to the target, it
  coded scrolling text 11 to 15 dB below low latency while spending 3.8 to
  7.1 times its bits a second, and it overspent the busy pattern by a
  third at 3024x1964 whatever mean it was set to. With the cap it fell to
  71 to 78 a second after 3 s at 3024x1964 and 3840x2160. The planned
  switch's conditions also held at 2560x1664, where low latency kept 106
  to 113. H.265 stays on low latency at every size.
- *Holding the old cap for a second after a bitrate cut.* A cut from 8 to 2
  Mbit/s dropped no picture, and the first unit after it was within 1.2
  times its new budget. So a new bitrate still sets its cap at once.

The tests are ignored tests in `platform/macos/vt_measure.rs` and
`platform/macos/tests.rs`: `video_toolbox_drops`, `video_toolbox_resubmit`,
`video_toolbox_rate_model_follow_h264` and `_h265`, `video_toolbox_120` and
`video_toolbox_rate_step`. They pace a real session on IOSurface pictures
and need no Screen Recording grant. Those in `vt_measure.rs` paint each new
picture into a pool of eight surfaces as it is handed over, as
ScreenCaptureKit fills its own; the follow-pace sweeps paint each picture
onto one of three surfaces in turn
([measurements](benchmarks/2026-11-24-macos-rate-model/README.md)). A live
session's effect is still to be seen on a Mac host, behind a shaped link.

### H.265 and H.264

A viewer that decodes H.265 says so with the `h265` capability and still
offers only `h264` or `jpeg` in `codecs`: an older host refuses a codec it does
not know but ignores a capability. A host answers `Accepted { codec: h265 }`
when the viewer listed `h265` and the session's backend encodes H.265;
otherwise it chooses as before, H.264 when offered, else JPEG. `h265` in
`codecs` is an invalid request. The ALPN does not change, so earlier v2 builds
and this one still connect to each other, on H.264.

The host asks the session's own backend. Thread C builds it as soon as the
viewer holds the slot, and `Accepted` waits for its answers, at most 3 s in
all (the viewer waits 5 s for `Accepted`): first the display it captures,
which every viewer waits for and which sizes the first IDR's budget (see
[QoS](#qos-frame-rate-and-bits-per-frame)), then whether it encodes H.265,
which only a viewer that offers H.265 waits for. A later answer leaves that
session on H.264. The Windows backend probes the GPU that drives its display, by adapter LUID, once
per process: it enumerates hardware HEVC encoder MFTs for that adapter,
activates the first that takes this pipeline's settings, with Main 4:2:0
output and NV12 input at 1920×1080@60, and starts it streaming without
encoding a frame. Through the helper the worker answers (`Request::H265`): it
probes the same way in the interactive session, whose displays the
background host in session 0 cannot see. Measured here, a probe took 320 to
350 ms in a fresh process (951 ms the first time after a build) and 0.4 ms
once kept; the session's encoder then took about 200 ms to build, against
280 ms for a cold H.264 one, so most of the probe is start-up the encoder
would pay anyway.

The H.265 encoder takes the H.264 encoder's settings: low latency, CBR, an
infinite GOP, IDRs only on demand and the same bits per frame, which show as a
better picture rather than a lower rate. Its output type is
`MFVideoFormat_HEVC` with the Main profile, which allows B-frames: an encoder
that implements `CODECAPI_AVEncMPVDefaultBPictureCount` must accept 0 and read
0 back, or it is not used. NVIDIA's HEVC Encoder MFT answers `E_NOTIMPL` to
that property; there the required low-latency mode keeps B-frames out, and
every output is still checked against reordering.

Every access unit is Annex B. An H.265 unit is a keyframe when it holds an
IRAP picture, NAL types 16 to 21 (`(b0 >> 1) & 0x3f`; NVIDIA's IDRs are
IDR_W_RADL, 19), and its parameter sets are the VPS (32), SPS (33) and PPS
(34); H.264 keeps its IDR (5), SPS (7) and PPS (8). The encoder keeps the last
parameter sets, in-band or from `MF_MT_MPEG_SEQUENCE_HEADER`, and puts them in
front of a keyframe that comes without them, after its access unit delimiter.
NVIDIA writes them in-band at every IDR, after the delimiter. Whatever treated
H.264's IDRs specially treats H.265's alike: the IDR's send-credit headroom,
probe tainting, on-demand IDRs and their rate limits.

When a session's H.265 encoder fails to build or to encode (an error of the
codec itself, rather than of the capture around it, including one the helper
reports), the host turns H.265 off for the rest of its process and ends that
session with `capture_failed`; the viewer's reconnect then gets H.264. Since
the probe encodes no frame, an encoder that takes no input within 3 s of being
built is such a failure: one that never asked for input, or had an image and
no free sample for it. Only an encoder that asked while capture had no desktop
image blames the capture, which is retried. The failure is logged as `desktop_h265_failed`, and each probe as
`desktop_h265_probe`, on `pb_mapper_desktop::session`.

### Send credit

The host admits a unit against a window computed before each admission:

```text
t      = base RTT (100 ms until the first probe) + 100 ms
frames = clamp(ceil(pace × t), 3, 64)
bytes  = clamp(2 × expected bitrate × t / 8, 64 KiB, 4 MiB)
```

The base RTT is the minimum of recent probes, never a smoothed RTT, so queueing
never enlarges the window. At 60 FPS, a 20 ms base RTT and 4 Mbit/s that is 8
frames and 120 KB. A unit is admitted when nothing is in flight (whatever its
size, so a session always progresses), when it fits both bounds, or when it is
an IDR and no other IDR is in flight: one IDR may go over the byte window and
one frame past the frame window, and never waits for the whole flight to
drain. Its sequence number is booked before the write, so a receipt ahead of
the last booked sequence is a protocol error.

When receipts show a standing queue, the byte window follows the delivery rate.
The ack queue is the median receipt delay above the base RTT over the last
250 ms, or over the newest three receipts when fewer came in that time: a slow
link receipts seldom, and its queue must not vanish between two receipts. It is
zero before three receipts. Above 100 ms, once receipts have been measured for
500 ms, `bytes` becomes 1.25 × the delivery rate of the last 500 ms × `t`, at
least 32 KiB and at most the normal window. An application-limited flow has no
queue and is never clamped. The clamp bounds the queue between probes; frame
rate and bitrate decisions stay with the probe. JPEG sessions use a fixed
window of four pictures and 2 MiB.

Receipts reach only the credit ledger and the QoS controller. They never reach
the input ledger, the backend or the control lease, and no authorisation method
takes a sequence number. In the QoS controller they also feed a receipt-rate
estimator that runs in shadow: it is traced (see
[Diagnostics](#diagnostics)) and no frame rate, bitrate or credit decision
reads it.

### QoS: frame rate and bits per frame

The host writes a probe in stream order between video messages, so it measures
downstream queueing; one is outstanding at a time. It writes one a second, and
one every 250 ms while the base RTT is under 100 ms (a short path). The base
RTT is the minimum over the last 60 s, and it resets when the viewer reports a
path change. A probe written behind an IDR in flight is tainted: it updates
the RTT but never the frame rate. A 3 s window with only tainted samples may
step bits per frame down on their worst delay, never up. The queue delay is
the median of the last three untainted RTTs (of two, the lower) minus the base
RTT, so one probe that a Wi-Fi stall held moves it not at all.

Frame rate follows RustDesk's delay steps on that delay, scaled by
`max(1, ceiling / 30)`. Below 50 ms it climbs, with a quick step after three
good samples, or at every sample on a short path, so a cut there recovers in
about a second. From 50 to 150 ms it climbs only slowly or holds. From 150 ms
it falls to `fps × 150 / delay`, but not below the preset's minimum; from
200 ms it falls to the minimum or below. A cut needs evidence beyond the
probes' median: either this probe and the one before both saw 150 ms or more,
and then RustDesk's step applies in full, so a sustained queue reaches the
floor at its second sample; or the receipts witness a standing queue (the ack
queue below is 100 ms or more), and then a cut at most halves the frame rate.
Without either it holds. Balanced starts at its ceiling instead of its start
value when the first probe after the preset is applied returns within 50 ms;
the startup cap still holds the pace for its second. The controller works in
bits per frame, so a frame-rate cut sheds bandwidth at once. Every 3 s it
scales bits per frame by
the window's worst delay: ×1.15, ×1.10 or ×1.05 below 50, 100 or 150 ms when
the picture changed (at least six unique frames), and ×0.95, ×0.90, ×0.85 or
×0.80 below 200, 300 or 500 ms or above. An increase adds at most 150 kbit/s.
The result stays between the preset's floor and maximum. Every bits-per-frame
figure is given at 1080p and scaled by the encoded pixels relative to 1080p
to the power 0.75, within 0.4 and 3: 0.54 at 720p, 1.54 at 1440p and 2.83 at
4K, since a picture's bits grow slower than its pixels. A session starts from
the size the encoder will produce, the display the backend reported before
`Accepted` fitted inside the viewer's bounds, so the first IDR has the
session's budget even when the viewer asked for more than the display has
(1440p or Native for a 1080p display); the first unit's size corrects it, and
without a reported display the session starts from the bounds.

| Preset | Frame rate (min / start / ceiling) | Bits per frame at 1080p (start / max / floor) | At 60 FPS (30 for Quality) |
| --- | --- | --- | --- |
| Quality | 8 / 24 / 30 | 200 000 / 400 000 / 50 000 | 6 Mbit/s, up to 12, floor 1.5 |
| Balanced (default) | 10 / 45 / 60 | 66 667 / 133 333 / 16 667 | 4 Mbit/s, up to 8, floor 1 |
| Smooth | 12 / 50 / 120 | 50 000 / 100 000 / 12 500 | 3 Mbit/s, up to 6, floor 0.75 |

The ceiling is also bounded by the viewer's `max_fps`. A viewer that lists
the `fps_override` capability chose that `max_fps` itself: it is then every
preset's ceiling, Quality's included, while each preset keeps its own start
and minimum. The pace the host captures at is the controller's frame rate,
bounded by the ceiling and the viewer's fps cap. It is held to
`min(start, 15 × scale)` for 1 s after the
session starts and after a resume, to 2 FPS while the viewer is unresponsive,
and to 8 FPS for JPEG. A viewer is unresponsive while its probe echo, or a
receipt it owes, is more than 2 s late. That cuts only the output pace and
leaves the controller's state untouched; the next receipt or probe echo clears
it, and the session never closes because of it. Changing the preset
mid-session restarts the controller at the preset's start values, without an
IDR, an encoder reconfiguration or a resolution change. Pausing sets the pace
to 0.

The encoder bitrate depends on how the encoder budgets. A per-frame-budget MFT
(the `nominal` rate model, the default) gets `bits per frame × nominal fps` and
spends one frame's budget per frame, so a lower pace sheds bandwidth at once.
An encoder that budgets per second of presentation time (the `timestamp`
model) gets `bits per frame × pace`. Each session starts at the model its
backend declares (`BackendCaps::rate_model`, `nominal` unless set), and every
viewer of a room starts at the same one; today every platform's encoder, and
every session through the Windows helper, starts at `nominal`. A `nominal`
session whose encoder budgets per second would overspend at a reduced pace,
by about the nominal rate over the pace. At every 250 ms QoS tick the host
adds up what the encoder produced since the last one: frames, unique frames,
non-IDR bits, and the bits expected for those non-IDR frames at the highest
per-frame budget the encoder was given during the tick. That budget is the
encoder bitrate over the nominal rate, which is bits per frame unless the
encoder bitrate's bounds clamp it, and every change of bits per frame raises
the tick's highest, so a cut never makes the frames encoded before it look
like an overshoot. A tick that comes less than half a tick after the last is
folded into the next. When the last 2 s of ticks were each paced at no more
than 0.75 × the nominal rate at both their ends, held at least six unique
frames between them, and produced non-IDR bits above
`max(1.3, min(0.6 × nominal / mean pace, 2))` × the bits expected for them,
where the mean pace is the frames produced over those 2 s, the session
latches to the `timestamp` model for good and traces
`desktop_rate_model_latched`; nothing latches a session back to `nominal`.

A per-second budget that its pictures fill overshoots by about
`nominal / pace`, so it latches within 2.25 s of the pace falling: at the
eighth tick after the one the pace fell in, which began at the higher pace
and does not count. One whose pictures fill only part of it latches as long
as it sends more than that threshold, so at any reduced pace when it sends
more than twice what is expected. The middle term keeps a per-frame encoder
whose moving picture alone holds its frames above budget, such as NVIDIA's at
pace 5 below, from latching, but only up to twice the budget, and only up to
1.3 times it at a mean pace of 0.46 × the nominal rate or more. A per-frame
encoder whose frames stay above the threshold at a reduced pace still
latches, within about 2 s; that becomes possible once bits per frame have
fallen to the preset floor. Fewer than 3 pictures a second (the unresponsive
viewer's 2 FPS, or capture waiting for credit on a link the encoder
overfills) never make six unique frames in 2 s, so they latch nothing until
pictures flow faster again. The encoder bitrate stays between 256 kbit/s and
80 Mbit/s, which the encoder and the helper check too, and nothing raises it
because the frame rate was cut.
At 4K, Quality's maximum bits per frame give the encoder 68 Mbit/s at a
nominal 60 FPS, and the 80 Mbit/s bound at 120.

### Encoder rate model per GPU

Measured on the development machine below with its NVIDIA GeForce RTX 5070 Ti
(driver 32.0.16.1088, "NVIDIA H.264 Encoder MFT", 1920×1080, nominal 60 FPS):

- GOP: every candidate sets and reads back unchanged before the media types, so
  sessions run with a GOP of `u32::MAX` (4294967295). After streaming starts,
  the same setting is accepted and silently ignored (the read-back stays at the
  default of 60 frames), which is why it is set first and read back.
- Keyframes: 2 in 60 s of `video_bench 60 60 --animate` (the initial IDR and
  one forced at 1.5 s), 2 across a 60 → 5 → 60 FPS pace sweep, and 1 in 8 s of
  fresh full-frame noise: no scene-cut or periodic IDR.
- Rate model: `nominal`. With synthetic content at 6 Mbit/s, produced bitrate
  at pace 60 / 30 / 15 / 5 was 6.16 / 2.78 / 1.52 / 0.39 Mbit/s against the
  nominal model's 6 / 3 / 1.5 / 0.5. The output was byte-identical under three
  timestamp schemes, so this MFT ignores sample times for rate control. The
  worst overshoot (1.03) is far from the 1.3 latch.
- `video_bench --pace 60,30,15,5` (the moving pattern, nominal 60, 4 Mbit/s):
  produced/expected 0.962 / 0.995 / 1.008 / 1.040, within the ±25% bound. At
  nominal 120 and 8 Mbit/s: 0.834 / 0.954 / 0.993 / 1.096. At nominal 120 and
  only 4 Mbit/s the pattern's own minimum size decides the bitrate (up to 1.79
  at pace 5), which is the content, not a timestamp model. The latch asks for
  more than twice the expected bits there (its pace term, 0.6 × 120 / 5 =
  14.4, capped at 2), so such a run does not latch; a false latch would lower
  each frame's budget while the pace is reduced, so acceptance runs on Smooth
  still watch `rate_model`.
- The creation bitrate is honoured (4 → 4.05 and 16 → 15.97 Mbit/s), and a live
  bitrate change reads back exactly. The mean-bitrate read-back is stale until
  the first live change and is not evidence of the target.

The same GPU's "NVIDIA HEVC Encoder MFT" (`video_bench --codec h265`):

- Rate model: `nominal` too. `--pace 60,30,15,5` at nominal 60 and 4 Mbit/s
  gave produced/expected 0.942 / 0.974 / 1.009 / 1.171, within the ±25% bound.
- GOP `u32::MAX`, read back; 2 keyframes in 10 s (the initial IDR and the one
  forced at 1.5 s, which came 3.6 ms after it was asked for). The B-frame
  property answers `E_NOTIMPL`; every output came in order.
- 1080p from the 2560×1440 desktop with the moving pattern: 59.6 unique
  capture and encode FPS, a mean 1.65 ms encode, one unit in flight, IDRs of
  168 to 178 KB. The SDK backend (`--backend`) delivered 58.1 FPS.
- 4K (`--exact --max-width 3840 --max-height 2160`, the desktop upscaled,
  since this display is 1440p): 59.7 FPS at a mean 3.1 ms encode, IDRs of
  194 to 262 KB at 34 Mbit/s.
- FFmpeg decodes each stream without error as HEVC Main, 4:2:0, with no
  B-frames (`has_b_frames=0`) and every frame counted. NVIDIA writes an access
  unit delimiter, then the VPS, SPS and PPS, then the IDR.
- The largest units, against `wire::MAX_PAYLOAD` (4 MiB): fresh full-screen
  noise (`--noise`) encoded at 4K and 80 Mbit/s, the encoder's bound, gave
  IDRs of 3.74 MB at a nominal 60 FPS, 2.96 MB at 120 and 3.93 MB at 30
  (H.264: 3.63 MB at 60 and at 30). Quality at 4K gives the encoder at most
  34, 68 or 80 Mbit/s at a nominal 30, 60 or 120 FPS, so no IDR it can ask
  for reached the limit, though the noise at 60 came within 11% of it. The
  limit stays 4 MiB. An access unit above it is an encoder error: capture
  unavailable for H.264, and for H.265 the end of the session and of H.265
  for the process.

AMD and Intel encoders have not been measured: this machine's AMD iGPU drives
no display, so its encoder is unreachable from the capture adapter. Run
`video_bench --pace 60,30,15,5` and `video_bench 60 60 --animate`, each with
`--codec h264` and `--codec h265`, on such a host before relying on the
`nominal` default there; the latch handles a per-second-budget encoder.

A Mac host's VideoToolbox encoder starts at `nominal` as well. The ignored
tests `video_toolbox_rate_model_h264` and `video_toolbox_rate_model_h265`
(`platform/macos/tests.rs`) measure it as `video_bench --pace` measures Media
Foundation: each codec at 1080p, 1440p and 4K, with low-latency rate control
and without, at a nominal 60 FPS and 4 Mbit/s, paced at 60, 30, 15 and 5 FPS
on synthetic IOSurface pictures covered in fresh grain, so no Screen
Recording grant is needed. They print, for each step, the pictures submitted
and the units out, the encoder's dwell and the bitrate against both models;
the fitted model (the same ±25% classifier, `codec/rate_fit.rs`); the largest
unit after a 1 s pause; each encoder's `macos_encoder_opened` line; and a
summary line a case. A case that fits neither model is reported, not failed.
Run them in release mode on a quiet Mac, one at a time:
`cargo test --release -p pb-mapper-desktop --lib -- --ignored --nocapture --test-threads=1 video_toolbox_rate_model_h26`.
Their pacing follows a Mac host's submit gate (two units in flight, a busy
submit thread leaving the tick armed).

#### Apple M6

`video_toolbox_rate_model_follow_h264` and `_h265` ask what a session would
spend under each model. Each codec runs at 1080p, 1440p and 4K, with low
latency and without, at a nominal 60 FPS. Bits per frame are Balanced's
start scaled by the size (`bpf`). After a step at 60, two sweeps run at 30,
15, 10 and 5 FPS:

- `nominal` keeps the bitrate at bpf × 60, as a session starting there does;
- `timestamp` sets bpf × the pace, live.

They run as the tests above do, with the filter
`video_toolbox_rate_model_follow`, about 5.5 minutes each.

Each step's spend a second, over bpf × the pace (what QoS means to spend),
with low latency, at 30 / 15 / 10 / 5 FPS
([measurements](benchmarks/2026-11-24-macos-rate-model/README.md#rate-model)):

| Codec | Size | `nominal` | `timestamp` |
| --- | --- | --- | --- |
| H.264 | 1920x1080 | 2.00 / 3.95 / 5.95 / 11.80 | 1.01 / 0.99 / 0.98 / 0.77 |
| H.264 | 2560x1440 | 1.98 / 4.02 / 5.98 / 11.94 | 0.98 / 1.01 / 0.86 / 1.05 |
| H.264 | 3840x2160 | 1.99 / 3.93 / 5.90 / 11.64 | 1.00 / 0.99 / 1.04 / 1.04 |
| H.265 | 1920x1080 | 2.00 / 3.83 / 5.73 / 11.75 | 0.81 / 0.80 / 1.19 / 2.38 |
| H.265 | 2560x1440 | 1.83 / 3.61 / 5.33 / 10.74 | 0.92 / 0.92 / 1.36 / 0.02 |
| H.265 | 3840x2160 | 1.83 / 3.34 / 4.99 / 12.20 | 0.58 / 1.13 / 0.04 / 0.02 |

Low-latency H.264 budgets per second: started at `timestamp`, it would spend
what QoS means at every pace. Low-latency H.265 does not:

- At its lowest paces it put out units that changed next to nothing, so the
  picture stood still.
- At 1080p and 5 FPS it spent 2.4 times its budget.

An IDR is also costly at a low pace under `timestamp`. After an IDR at
10 FPS, 16 to 18 of the next 20 pictures were dropped, and at 5 FPS 9 of 10,
for both codecs at every size; under `nominal`, 0 to 3 and 0 to 1. A
per-second budget of bpf × the pace spreads the IDR's payback over seconds.

Without low latency, `timestamp` stayed between 0.56 and 1.03 of the budget
for H.264, and between 0.19 and 1.08 for H.265.

So a Mac host still starts at `nominal`. A session whose encoder overspends
at a reduced pace is moved by the latch. Another chip, or a later macOS,
needs these tests run again before a Mac host's start changes.

Two more ignored tests time the hand-over itself. `video_toolbox_submit_blocking`
paces a session as thread C does, on eight pictures painted before the run so
that the pacing thread does nothing else, with Balanced's starting bits per
frame scaled by the size, while a reader thread queues an input every
millisecond and wakes it; the pacing thread takes the queued inputs before and
after each submit, as thread C drains its lane. At 1920x1080, 1920x1200,
2560x1440, 3328x2160 and 3840x2160, both codecs, paces 60 and 120, it prints
pictures submitted and units out a second, ticks spent on units in flight and
ticks left armed for a busy submit thread, the submit call (p50, p99, max), how
long a queued input waited (p50, p99, max) and the dwell (p50, p95).
`video_toolbox_in_flight` runs the same cases at 3024x1964, 3340x2160 and
1920x1080 with the gate at four units and at two. Run each as the rate-model
tests are run, by name
([measurements](benchmarks/2026-11-24-macos-vt-submit/README.md)).

### Viewer: newest decodable frame, fps cap and recovery

The viewer's read task writes the receipt for each video record before it hands
the record to the session's `VideoSink`, so nothing on the screen side delays
it. The sink keeps only what is still worth decoding:

- An arriving IDR supersedes everything queued, marked as a discontinuity.
- When the oldest queued unit is older than 250 ms with at least three queued,
  and the consumer last took an IDR more than 500 ms ago, the consumer fell
  behind: the sink opens a backlog episode and asks for an IDR, once. It
  keeps the queue, so a consumer back from a stall drains it and shows newer
  and newer pictures instead of a frozen one, until the IDR arrives and
  supersedes what is left; the episode also ends once the oldest queued unit
  is younger than 125 ms. Before the consumer takes its first IDR, no episode
  opens: what queued is the hand-off to a consumer still attaching.
- Overflow (32 units or 8 MiB) drops the queue to the next IDR at any time and
  asks for one. That bounds memory and the lag shown.
- After an overflow, a new generation or a pause, a P-frame that arrives before
  the IDR is dropped. A new generation starts at an IDR. JPEG keeps only the
  newest picture.

Backlog also lowers the host's frame rate. Every 250 ms while streaming, from
500 ms after the consumer took its first IDR, the fps cap looks at the queue
and at what the consumer takes:

- A tick in which the consumer took units and never found the queue empty kept
  it busy. Its pulls over the busy ticks among the last eight, once there are
  four, are its capacity: what it can decode, which presentation alone cannot
  tell.
- The turnaround is the time from handing a unit out to its accepted
  presentation. A decoder that queues inside itself (the Linux runner keeps up
  to four units in its GStreamer pipeline, and Android's MediaCodec holds a
  few) hides a backlog from the sink's queue, but its turnaround rises by a
  decode time for each unit it holds. The turnaround is late when its median
  over the last second exceeds its least over the last ten seconds by more
  than two frame intervals at the cap, and at least 40 ms.

More than two queued on two consecutive ticks, a backlog episode or an overflow
from then on, or a late turnaround on two consecutive ticks, lowers the cap to
`floor(capacity × 0.9)` direct or `× 0.8` relayed, at least 5, at most once a
second. Without a measured capacity it uses what the consumer presented in the
last second, and then at most halves the cap at once. The cap remembers the
capacity it lowered to.

After 3 s with at most one queued and the turnaround on time, the cap rises
once a second: by 5 at a 30 fps ceiling and in proportion above it (10 at 60,
20 at 120), or by a quarter of the cap when that is more, but no further than
95% of a remembered capacity. Above that a rise is a probe of a tenth of the
capacity, watched for 3 s. Two queued with the oldest waiting more than two
frame intervals at the probed cap (at least 40 ms), a late turnaround, a
backlog episode or the consumer busy four ticks in a row take it back at once,
whatever the 1 s spacing, and the next probe waits 3 s, doubling to 60 s.
Within 500 ms of the consumer taking an IDR, whose decode may be slow at any
rate, neither the queue nor a busy tick counts against a probe. A pause, or
the host not streaming, during the watch takes the probe back at once too,
without counting it as failed. A probe that holds resets that wait, and the
next may follow at once. The
capacity is forgotten when a probe holds at 1.1 times it or more, and on a
new generation, a new picture size or a preset change. A consumer takes at
least one unit per median turnaround, so when that rate is 1.1 times the
capacity or more it becomes the capacity: a decoder that got faster, or one
only its presentations had measured, is not held below what it shows it can
take, while probes above it keep their wait. At the preset's ceiling the cap
is removed. Only backlog lowers it, so a phone that keeps up, or a static
desktop, never ratchets the host down.

One limiter covers every keyframe request: the decoder's, the sink's backlog
episodes and overflow drops, and the user's. While waiting for an IDR it asks at once (or after a 1 s hold that
follows a new generation, a host recovery or a resume), then after 0.5, 1, 2
and 4 s, and every 4 s after that, until an IDR's header arrives, even while
its payload is still in transit; until that IDR has landed, the hold and the
backoff still gate other requests. A user request skips the backoff but not
the 500 ms minimum. Units handed out with nothing presented for 700 ms also
ask for one. Five decoder errors in 30 s with nothing presented end the
session with `decoder_failed`. No request is sent while paused or while the
host reports capture unavailable.

`pause` revokes control, tells the host to stop capturing and clears the sink;
probes are still echoed and pings sent, so the host sees a live, paused viewer.
`resume` restarts at an IDR with the startup pace. A presentation is accepted
only for a unit the sink handed out in the current generation, less than 2 s
after it arrived, newer than the last one, and not while paused. A rejected
presentation is never forwarded.

### Input and cursor

The viewer queues input in order behind a revoke barrier: at most 512 events or
256 KiB, with consecutive pointer moves coalescing into the newest. A barrier (a
release, a grant or a revoke) discards only the input queued before it, never
input submitted after the call that set it. It sends a
pending presentation ahead of input, and revokes control rather than grow the
queue. The host's reader checks the control policy and pushes input onto a
bounded lane without waiting. The desktop-bound thread drains that lane before
and after its single backend call, so input never waits behind capture or
encoding. It applies each event only after checking its sequence, the display
geometry and generation, control, and a presentation its own ledger recorded
within 2 s. Nothing queued before a stop is executed, and reconnect never
replays queued pointer or keyboard actions. A full lane ends control
(`input_congestion`) rather than inject a burst the user no longer expects; the
display-mode commands in it still run, the owner's restore among them and a
viewer's checked against control as the lane leaves it (refused after the
viewer's own release or pause, as without an overflow), and the owner's restore
is queued even past a full lane.

`input_wait`, from the reader's enqueue to the start of execution, is traced as
p50/p99/max each second. The acceptance gate is p99 ≤ 5 ms on the ordinary
backend and ≤ 15 ms through the helper, with moving content. On a Mac host
the backend call stays short because VideoToolbox runs on the encoder's own
submit thread
([Host: capture and encoding](#host-capture-and-encoding)): while thread C
made the call itself, an encode at a size that is a multiple of 16 held
queued input for as long as it took. In a test loop on an M6 that paced a
session as thread C does and queued an input every millisecond, an input's
wait at the 99th percentile was 5.0–5.3 ms at 1920x1200, 7.3–7.7 ms at
2560x1440 and 14.9–17.4 ms at 3840x2160 before (each case's median of
three runs), and 40–53 µs at every size
after, in all but two of the 60 runs (one run each of two cases waited 3.3
and 4.4 ms while the loop was not in a call;
[measurements](benchmarks/2026-11-24-macos-vt-submit/README.md)). A live session's
`input_wait` on such a display is still to be measured. If the gate
fails, the next step is a dedicated injection thread with its own desktop
binding, a process-wide registry that maps each binding epoch to a generation
(an older epoch fails closed), and a second helper pipe lane. It is not built;
build it only on measured evidence.

A press that starts a drag waits out the OS double-click time (plus 20 ms, at
most 600 ms) after a click on the same spot, so the two never merge into a
double click. Held commands are re-authorised when they are released.

The desktop-bound thread samples the cursor every 16 ms while capturing (never
while paused or unavailable) and at once reports every sample unlike its last
report, except the controlling viewer's own move coming back: the viewer draws
its pointer where it put it, and an echo arriving behind the video would drag
it back. A shown pointer at the position the thread last injected, in the same
display pixels, is that echo, whatever it showed before, until a report finds
the pointer elsewhere; a later return to that spot is someone else's move, and
is reported. A hidden pointer carries no position and is never an echo.
Comparing with the last report, not the last sample, reports a move the OS
stopped short (a `ClipCursor`, an edge) where the pointer stopped. Nothing is
held back and sent late. A backend reports the pointer as every
input it applied left it: the helper's worker, which observes it during the
capture call, also answers each input with the pointer, so its report is
never older than the last move. A new generation, a display change included,
always reports where the pointer is, and a viewer that does not control sees
every change. The writer sends each report once, outside the video queue,
after its generation's `State`. Observing the pointer, its shape included,
never renews input authorisation.

A report can still cross the controller's moves on the wire. Sampled before
the thread took a move, it reaches the viewer after the move left; and since
that move's echo is never reported, taking the report would leave the viewer's
pointer where the host's is not: the remote user bumps the mouse during a
stroke, or a key press hides the pointer just before a tap. So each report
carries an input watermark, `input_sequence`: the sequence of the last input
the thread had taken off the lane when it sampled, applied or not, held input
it dropped included. A viewer lists the `cursor_sequence` capability to get
it, and the host writes it to no other, since an older viewer rejects a field
it does not know. The viewer remembers the generation and sequence of the last
move it wrote to the wire (never one a barrier, a pause or a release dropped
from its queue). While it controls, a report of that generation below that
sequence is set aside, its position and its visibility alike. The first report
of a generation is always taken, and so is every report while the viewer does
not control, or from a host that sends no watermark. Because a clamp or a
hidden pointer may have been reported only before the move, the thread, once
it has taken a move, reports the next sample that is not that move's echo even
when it equals the last report, at the newer watermark: the clamp, or the
pointer still hidden, reaches the viewer with the move behind it.

**Concurrent controllers and peer pointers.** Up to four viewers (`MAX_VIEWERS`)
share one capture and encode loop. A viewer can join with the current codec
whenever it can decode it. The shared encoder uses the lowest frame-rate target
among unpaused viewers, including decoder-backlog caps. Pausing or leaving
removes that viewer's limit. Capture continues while any viewer has send capacity.
A slow viewer can drain its queued pictures and resumes at a keyframe after a
gap. A single viewer receives the owned frame buffer directly, and two or more
share the same encoded bytes. Each viewer has its own input lane,
presentation ledger, drag-start hold and held-input set: releasing or
disconnecting one controller lifts only keys and mouse buttons no remaining
controller still holds, and a button or wheel event re-aims the OS pointer to
that controller's last move if another controller moved it in between. Viewers
and hosts that list `peer_cursors` receive `PeerCursor` updates outside the
video queue — each controller assigned a distinct colour (`0..4`) for its
session — while OS cursor echoes from another controller's move are suppressed
for `peer_cursors` observers so one move never draws two pointers. With a
single controller, no `PeerCursor` message is sent and the viewer mounts no
extra compositor layer.

**The pointer's shape.** A viewer that renders shapes, and a host whose
backend reports them, list `cursor_shape`; the host sends shapes only when
both did, and otherwise writes exactly the bytes of a 0.11.0 host. The shape
is a `cursor_shape {id, kind, hot_x, hot_y, width, height}` record followed by
one payload of straight (not premultiplied) RGBA8, `width × height × 4`
bytes, the only payload besides video; it is sent with its bitmap even for a
standard kind, since a phone paints the bitmap and a desktop uses the kind.
Each side is 1 to 256 pixels (a host scales a larger pointer down) and the
hotspot lies inside; anything else, or a payload of another length, is a
protocol error. `kind` is one of `arrow`, `ibeam`, `hand`, `wait`,
`progress`, `crosshair`, `help`, `not_allowed`, `move`, `resize_ns`,
`resize_ew`, `resize_nwse`, `resize_nesw`, `up_arrow` and `custom` (a shape
the host cannot name); the list grows only with a new capability. `id` is a
slot below 64: a record defines or overwrites its slot, B-send keeps the
host's table of what the viewer read, reusing the slot of an equal image and
overwriting the one named longest ago, and the viewer keeps exactly 64. Once
a shape is known, every `cursor` report names its slot in `shape`, written
only to a `cursor_shape` viewer. A new shape is reported even at the
controller's own move, and the viewer takes every report's shape while it
sets aside only stale positions. Thread C asks the backend for the shape only
when the session negotiated it, on the 16 ms tick, and compares content, so
an equal image under a new OS handle sends nothing. A failed read is the
debug event `desktop_cursor_shape_failed` and keeps the last shape. The
viewer hands Dart a number per definition in the view (`cursor_shape`), and
the FFI's hot op `cursor_shape` returns that definition when the number
changes. The number is a `u16` that goes round after 65 535 definitions; a
new one passes over every number a slot holds and the one last asked for,
so the number Dart shows keeps its definition, and Dart keeps decoded
shapes by content, never by number.

**The viewer's pointer.** While a desktop viewer controls, the local pointer
over the picture is one `RemoteShapeCursor` per viewer
(`remote_cursor_layer.dart`), so Flutter's mouse tracker keeps its session for
as long as the pointer stays over the picture. That session shows the shape
itself, the moment the host's shape, the picture's scale or whether the host's
pointer is under the local one changes: no widget rebuilds, and no frame is
waited for, where a `MouseRegion` takes a new cursor only in the post-frame
pass of the frame that rebuilt it. A kind this computer's system has goes to
the engine's `activateSystemCursor`. Flutter's macOS embedder knows no busy,
progress, help, move or diagonal resize pointer and shows those as the arrow,
so on a Mac they take the bitmap path, as do `up_arrow` and `custom`
everywhere. The bitmap path scales the host's bitmap to the picture (one host
pixel as 0.35 to 2 logical pixels, at most 128 logical pixels a side, times
the device pixel ratio, area-averaged in premultiplied space) and hands it to
the runner's `pb_mapper/desktop_pointer` channel
(`ui/lib/src/ffi/desktop_pointer.dart`): `cursor_create {id, rgba, width,
height, hot_x, hot_y, scale}` in straight RGBA and physical pixels, `scale`
being the device pixel ratio the bitmap was made for, `cursor_set {id}` and
`cursor_delete {id}`. The viewer keeps 16 such cursors, by content, by scale
to within 2 % and by device pixel ratio (the same pixels at another ratio are
another size on macOS and Linux), and deletes the oldest past that, all of
them when the host's shape is forgotten (another viewer), and all when the
viewer closes. A `cursor_create` for an id a runner holds replaces that
cursor: a hot restart starts Dart's ids again while the runner keeps its
cursors. Each runner shows the cursor on its platform thread at once:

- Windows (`ui/windows/runner/desktop_pointer.cpp`): `CreateIconIndirect`
  from a 32-bpp top-down DIB section with its alpha and a 1-bpp AND mask from
  it, `SetCursor`, and `DestroyIcon`, kept until another cursor shows when it
  is still the current one. The engine answers `WM_SETCURSOR` over the client
  area without setting a cursor, so this one stays until Dart activates a
  system cursor.
- macOS (`ui/darwin/DesktopPointer.swift`, in the macOS runner only):
  `NSCursor(image:hotSpot:)` from the bitmap at `scale`, its size and
  hotspot in points. It tells `FlutterView` through its
  `didUpdateMouseCursor:`, so the view's `cursorUpdate:` restores this
  cursor rather than the last system one; an engine without that method gets
  a local `cursorUpdate` monitor that keeps it instead.
- Linux (`ui/linux/runner/desktop_pointer.cc`): `gdk_cursor_new_from_surface`
  from a cairo surface at `scale` (a whole number, as GDK takes it), set on
  the toplevel's `GdkWindow`, where the engine sets its own cursors. GDK
  keeps a hotspot in whole logical pixels on X11 and Wayland, and a Wayland
  cursor buffer must be a multiple of its scale, so transparent pixels pad
  the bitmap on the left and top until the hotspot is on a logical pixel's
  corner, and on the right and bottom to whole logical pixels.

A call that fails (no runner, or one that cannot make the cursor) paints the
shape under a hidden pointer for the rest of that viewer, as a viewer did
before. Viewing only, with the host's pointer elsewhere, and on a phone, the
shape is painted as before.

### Constants

| What | Value |
| --- | --- |
| Records | JSON header ≤ 8 KiB; video payload 1 B to 4 MiB |
| Helper pipes | 256 KiB may wait each way; the service relays through 256 KiB toward the app and 64 KiB toward the worker; one write per answer with its byte record |
| Timeouts | handshake 3 s; host read 3 s; viewer header 4 s; writes 3 s plus the message's time at 1 Mbit/s; `Stopped` best effort within 1 s; the host reads on until the viewer closes, within the same second |
| Viewer ping / host statistics | after 1 s idle / every 1 s |
| Credit window | `t` = base RTT (100 ms until measured) + 100 ms; 3 to 64 frames; 64 KiB to 4 MiB; delivery clamp ≥ 32 KiB once the ack queue exceeds 100 ms |
| Host hand-off | 6 units, 8 MiB, 1 s oldest, not counting a wait behind an IDR in flight; capture skipped at 2 waiting for credit or 4 in the encoder (2 in flight in a Mac host's VideoToolbox) |
| Mac encoder | low latency with QP up to 51; a dropped newest picture sent again at the next tick, at most twice a picture; dropped pictures traced at most once a second |
| Armed tick | a tick with no new picture stays armed; ScreenCaptureKit wakes thread C; DXGI, in process or through the helper, is asked again every 2 ms for 250 ms after an injected input or a new picture (an input brings a pending poll in to 2 ms), and once a period otherwise |
| Capture pacing | after a dated picture, the next tick a period after its present, up to ¼ period early and, for a picture of a single present, at most 1/64 period earlier than a period after the last tick (long-run rate ≤ 64/63 × pace); a picture of several presents that waited keeps the tick a period after the last; ¾ period floor after a stall; a present over a period old counts as undated, and DXGI dates no picture presented over 1 s ago |
| Probe / unresponsive | every 1 s, every 250 ms while the base RTT is under 100 ms / after 2 s, pace 2 FPS |
| QoS | delay the median of 3; RTT window 60 s; a cut needs 2 probes in a row at ≥ 150 ms, or an ack queue ≥ 100 ms and then at most halves; Balanced starts at its ceiling after a first probe under 50 ms; tick 250 ms; bits-per-frame window 3 s; rate-model latch over the last 2 s of ticks, each at ≤ 0.75 × nominal at both ends, with ≥ 6 unique frames and non-IDR bits > max(1.3, min(0.6 × nominal / mean pace, 2)) × expected at each tick's highest per-frame budget |
| Process timers | 1 ms, with power throttling opted out, while a session captures |
| IDR | host minimum interval 300 ms; viewer minimum 500 ms, backoff to 4 s; 1 s hold after a discontinuity |
| Viewer sink | 32 units, 8 MiB; backlog at 250 ms with ≥ 3 queued, drained until the IDR or a head under 125 ms; IDR grace 500 ms; stall watchdog 700 ms |
| fps cap | tick 250 ms; capacity from ≥ 4 busy ticks of the last 8; × 0.9 direct, × 0.8 relay; minimum 5; after 3 s, +5 per 30 fps of ceiling or a quarter of the cap, up to 95% of the capacity; probes of 10% watched 3 s (not by the queue within the IDR grace; a pause takes one back), backoff 3 s doubling to 60 s; turnaround late past 2 frame intervals (≥ 40 ms) over its 10 s floor; ≥ 1 s between changes but for a probe taken back |
| Input | viewer queue 512 events or 256 KiB, batches of 16; host lane 512; presentation fresh for 2 s; presentations sent at most every 100 ms except ahead of input |
| Cursor | sampled every 16 ms; the controller's own move never reported; an injected move is the pointer until the OS shows it, at most 250 ms after the last (`SETTLE`); a report older than the controller's last move set aside; a shape read only when `cursor_shape` was negotiated, the pointer shows on the shared display and the OS pointer changed (a failed read again after 250 ms), an image over 1024 px a side never, 1 to 256 px a side sent, 64 slots on each end |
| Codec | `Accepted` waits at most 3 s for the backend's display and H.265 answer; one probe per GPU and process; encoder bitrate 256 kbit/s to 80 Mbit/s; bits per frame × (pixels / 1080p)^0.75, within 0.4 and 3 |

### Diagnostics

`ViewerStatus.stats`, fed by the host's once-a-second statistics, defines the
numbers the UI, the acceptance tool and these docs use:

| Name | Definition |
| --- | --- |
| `rtt_ms` | The last probe round trip, transport only |
| `link` | On a direct path, the viewer tunnel's link, sampled once a second: `kind` (`lan`, `internet`, `private`), `family`, `via`, `peer`, `local_ip`, QUIC's own `rtt_ms` and `min_rtt_ms` (the network alone, rounded up), `setup_ms` and `same_lan`; null on the relay |
| `queue_delay_ms` | The median of the last three untainted probe RTTs minus the base RTT |
| `picture_latency_ms` | Median over 2 s of host time (encoder submit to socket write) + `rtt_ms / 2` + viewer time (network receipt to compositor consumption). Each side uses its own clock and the network leg is an estimate; only a camera measures glass to glass. |
| `presented_fps` | Accepted presentations per second |
| `received_fps` | Non-duplicate video records per second |
| `preset`, `max_fps` | The preset this session asked for, and its `limits.max_fps`, which bounds every preset's ceiling, or replaces it with `fps_override` |
| `skipped_per_s` | Host capture ticks skipped per second, for transport backpressure or a full encoder, between the last two `Stats`: over at least the host's 1 s interval, however close together they arrive, and never above `max_fps` |
| `encoder_dwell_ms` | The host's median time a unit stays in its encoder, or the helper's, from submit to output as the encoder measures it, over the last second; null from a host that reports none |
| `sent_bitrate` | What the host sent over the last second, in bit/s |
| `speed_rx_bps`, `speed_tx_bps` | Speed, as RustDesk counts it: bytes per second the session's tunnel carried each way over the last second, TLS and every message included. A session over a test stream has no tunnel, and reports the video payload it received and nothing sent |
| `codec`, `width`, `height` | The codec the host answered (`h265`, `h264` or `jpeg`) and the encoded size |
| `ack_queue_ms` | The host's receipt-based queue delay (the ack queue under send credit) |
| `present_interval` | p50/p95/p99/max of the intervals between accepted presentations over the last 60 s, with the gaps over 100 and 250 ms |
| `degraded` | For 3 s: queue delay ≥ 150 ms, picture latency ≥ 250 ms, or (at a pace of 10 or more) fewer than half the pace presented |

The host writes `encoder_dwell_ms` in its `Stats` only to a viewer that listed
the `encoder_dwell` capability in `Authenticate`: a strict viewer of an older
build rejects a field it does not know. A viewer reads its absence as null.

Where the frame rate goes is then one comparison at a time: a capture rate
below the pace is the host's source or its gate (`skipped_per_s`,
`encoder_dwell_ms`); a pace below `max_fps` is the controller, the viewer's cap
or the preset's ceiling (Quality's is 30 unless the viewer chose a frame
rate); received below capture is the transport (`ack_queue_ms`); presented
below received is the viewer's decoder or display.

Thread C also traces each second it captured as `desktop_capture_pacing` on
`pb_mapper_desktop::feedback` (debug): `wait_p50_us` and `wait_p95_us`, from a
dated picture's present to the capture call that took it; `dated_share`, the
captures whose picture was dated; and per second `captures_per_s` (captures
that spent their tick), `presents_per_s` (the presents their pictures
covered), `coalesced_per_s` (those beyond one per picture: presents no
capture took) and `empty_polls_per_s` (captures that found nothing new, and
on a Mac host those that found the encoder's submit thread busy). A
Mac host traces `macos_capture_timing` there too, once a second while it
submits: ScreenCaptureKit's `complete_per_s`, `idle_per_s` and
`replaced_per_s` (pictures a newer one replaced before thread C took them),
and `display_to_arrival_p50_us` and `display_to_arrival_p95_us`, from
`SCStreamFrameInfoDisplayTime` to the picture's arrival on the stream's
queue. Its encoder's submit thread traces `macos_encoder_load` there as
well, after the first call a second or more after its last line:
`seconds` (the window's length), `call_p50_us` and `call_p99_us` (its encode
calls, which thread C waited out before the thread took them), `units`
(the encodes it made), and from thread C's ticks `gate_ticks` (ticks
skipped for two units in flight, which `skipped_ticks` counts too),
`armed_refusals` (submits that found the thread
busy and left their tick armed) and `peak_in_flight`. A Mac host logs the
first picture of each stream as `macos_capture_layout` on
`pb_mapper_desktop::session` (debug): its width and height, the pixels it
says it extends by on each side (`extended_left`, `extended_right`,
`extended_top`, `extended_bottom`), each plane's rows and bytes per row
(`luma_rows`, `luma_bytes_per_row`, `chroma_rows`,
`chroma_bytes_per_row`) and its IOSurface's size (`surface_bytes`), which
say whether ScreenCaptureKit pads its pictures to a multiple of 16, as
VideoToolbox would then read them. A Windows encoder logs its first acquire as
`windows_capture_first_acquire` on `pb_mapper_desktop::session` (debug):
`last_present_time`, `accumulated_frames` and whether a desktop image came
with it, which it also keeps in its statistics for `video_bench`'s
`first_acquire_*`. The app's own host writes these debug events when
`RUST_LOG` enables their targets; the background service's log keeps its
targets at info, and its capture helper logs nothing.

The host traces each second as `desktop_feedback_interval` on
`pb_mapper_desktop::feedback` (debug): `action`, `fps`, `pace_fps`,
`viewer_cap`, `paused`, `unresponsive`, `bpf`, `bitrate`, `encoder_bitrate`,
`sent_bitrate`, `rate_model`, `rtt_ms`, `baseline_ms`, `delay_ms`, `tainted`,
`ack_queue_ms`, `standing_ms`, `busy_bps`, `used_bps`, `would_cut`,
`window_frames`, `window_bytes`, `window_clamped`,
`outstanding_frames`, `outstanding_bytes`, `idr_inflight`, `skipped_ticks`,
`encoder_dwell_ms`, `keyframes`, `gop`, `input_wait_p50_us`,
`input_wait_p99_us` and
`input_wait_max_us`. The presentation-credit fields `quality`, `waiting`,
`recovering`, `desired_window_bytes` and `last_ack_*` are gone.

`standing_ms`, `busy_bps`, `used_bps` and `would_cut` come from the
receipt-rate estimator, which only records what a receipt-driven rate control
would see; nothing acts on them. It keeps the receipts of H.264 and H.265
video from the second up to the newest one, at most 240, each with the delay of the newest
unit it covered and the bytes it covered, and forgets them on a path change
and on a resume. A frame interval is one at the current pace.

| Field | Definition |
| --- | --- |
| `standing_ms` | The least receipt delay above the base RTT over the newest receipts: at least three, spanning at least two frame intervals and 50 ms (at most 500 ms), so a burst a few milliseconds long never makes a queue on its own |
| `busy_bps` | The median delivery rate (of two, the lower) over the newest eight receipt pairs across which the link stayed busy, the later unit's delay above the base RTT being at least the pair's gap; none below three. Receipts less than a quarter of a frame interval apart are compression, not delivery: such a pair is no sample, and its later receipt's bytes count toward the pair before it |
| `used_bps` | What the receipts of the second up to the newest covered, from the first of them to the last; none over less than 100 ms. For up to a second after the last receipt the trace repeats it, so a stall shows as a `used_bps` that stops changing and then disappears, not as one that falls |
| `would_cut` | A receipt since the last trace showed a standing queue above 40 ms once each unit's own transfer at the busy rate (at most a frame interval) is taken off, with a busy rate known, no unit in its window behind an IDR, and outside a stall: the burst of receipts less than a quarter of a frame interval apart that follows a receipt gap of at least three frame intervals |

Without a receipt in the last second the first three are absent, and without a
base RTT `standing_ms` and `busy_bps` are.

Each written unit is traced as `desktop_frame` on `pb_mapper_desktop::frames`
(trace level), and the viewer traces each accepted presentation as
`desktop_frame_view` with the same `seq`. `deployed --feedback-log` enables
both host targets.

Each session's lifecycle is logged at info on `pb_mapper_desktop::session`:
`desktop_host_accepted`, `desktop_host_rejected`, `desktop_host_session_ended`
(reason, detail, seconds, frames, keyframes), `desktop_viewer_connected` and
`desktop_viewer_ended` (reason, detail, seconds, keyframes,
`keyframes_requested`, `backlog_drops`); a host also logs each H.265 probe as
`desktop_h265_probe` (usable, ms, detail) and warns `desktop_h265_failed`
when an H.265 encoder fails and H.265 is turned off. A Windows host warns
`desktop_moves_placed` (win32: the error of a `SendInput` that refused a
move, none when moves were dropped; token: the session, integrity level and
UIAccess of its process) when `SendInput` refused a move or its injected
moves stopped taking effect, and it places the pointer instead while that
window stays in front
([Pointer moves on Windows](#pointer-moves-on-windows)). A Mac host logs each
encoder it opens as `macos_encoder_opened` (codec, `low_latency`, width,
height, fps, hardware, gop, `data_rate_limits`: whether VideoToolbox took
the cap of 1.5 × the mean over 1 s, and `max_qp`: whether a low-latency
session took the QP bound of 51), after
`macos_encoder_low_latency_refused` when it retried without low-latency rate
control. It traces the pictures VideoToolbox dropped as
`macos_encoder_dropped` (debug): `dropped` since the last line, over
`seconds`, and the encoder's `total`, at most once a second and only when
there were any. It warns `macos_encoder_left_running` (`waited_ms`) when an
encoder's submit thread was still inside a VideoToolbox call 250 ms after
the encoder was dropped, and was left to finish on its own. The app also warns
`desktop_connect_failed` with the typed failure, and a native decoder's
failure is warned as `desktop_decoder_error` on `pb_mapper_desktop::frames`
with its text as the detail. Each line names itself in an `event` field, and
free text is Debug-formatted so that a newline in it cannot split the line.
No line carries a token, invitation or fingerprint.

The background service writes its host's lines to `service-host.log`, beside
`service.log` in the protected installation root (SYSTEM and administrators
write it, users may read it): `pb_mapper_desktop::session`,
`pb_mapper_desktop::feedback` and `pb_mapper_client::direct` at info, which
are the session lifecycle above, `desktop_rate_model_latched`, and
`session_path_selected` with the path, the service and the peer address. No
other target is written; none of these logs an invitation, a key or a
credential. The once-a-second `desktop_feedback_interval` is debug and is not
written; the viewer's statistics carry those numbers. At 1 MiB the file
becomes `service-host.log.1`, replacing the previous one.

### The quality panel

The viewer's connection-quality panel (the toolbar's chip, or the path and
frame-rate button in a phone's gutter) shows these figures in four groups,
and scrolls within a landscape phone's height. A row whose figure is not
reported, by an older host, on the relay or on a platform without it, is
left out; while the connection is down the core rows show dashes.

- **Network**: the path (LAN direct or direct, with the address family, from
  `link.kind` and `link.family`; or the relay and its carrier), the QUIC
  link's own round trip (`link.rtt_ms`, with its lowest) beside the stream's
  in-band one (`rtt_ms`, with its queueing), Speed each way, and the bitrate
  received and, from the host, sent.
- **Picture**: the picture delay with its host and viewer ends, the rate
  presented, received and captured by the host, the encoded resolution, the
  codec (H.265, H.264 or JPEG) with its chroma, and keyframes with those the
  viewer asked for.
- **Host**: the preset with its ceiling (Quality stays at 30 by design, so a
  30 under it is expected; Balanced reaches 60, Smooth 120, each within the
  viewer's display limit; a maximum frame rate the viewer chose is every
  preset's ceiling instead; a JPEG session stops at 8 whatever the preset, so
  its pace stays under a higher target), pace and target with any viewer
  cap, capture ticks skipped a second, the encoder's dwell, its target
  bitrate and rate model, and the adaptation.
- **Decoder**: while the panel is open, the store reads the native decoder's
  `stats` once a second and turns two reads into rates and, for each timing,
  its mean over the pictures between the reads. A row shows only where the
  platform reports its figure:
  - Android: decoded and dropped pictures a second, the mean from a
    picture's release to the surface to Flutter consuming it, surface credits
    in use, low-latency decoding and the refresh rate asked for.
  - Windows: the counts, the release-to-consume mean, the convert time
    (Convert and copy: from opening a decoded picture to the GPU finishing
    its copy), the fastest display's refresh, and low latency only where the
    decoder's codec API confirms it. Its decoder thread's CPU comes as
    cycles, which the panel does not show.
  - macOS and iOS: the counts, the decode time (VideoToolbox decoding a unit,
    keyframes included), the release-to-consume mean, the decoder thread's
    CPU as a share of one core, VideoToolbox's real-time mode as low latency,
    and the refresh of the display the viewer window is on (macOS) or the
    device's (iOS).
  - Linux: the counts, the release-to-consume mean and the fastest display's
    refresh.

  The Mac's decode time and Windows' convert time measure different spans,
  so each has its own row. A present time (`timings.present`) gets a row
  too, for a runner that presents pictures itself; none does in this
  revision. Closing the panel drops the decoder's timing accumulators and
  its CPU reading.

Read in order, the rates place a loss: capture below pace is the host's
source or its capture gate; pace below the ceiling is the QoS controller
(its startup hold included) or the viewer's cap; received below capture is
the transport; presented below received is this device's presenter.

The chip on the toolbar, and the phone's quality button, show the speed
received. The quality monitor, off at first and remembered per device, keeps
RustDesk's few figures (speed, frame rate, delay, target bitrate, codec,
chroma, resolution) small over the picture's top right and takes no touch.

### The app's video path

The viewer's overlays report the states in [Viewer states](#viewer-states).
After a stall the app asks for a keyframe first. A failed stream, or five
seconds without video while the viewer shows and the session streams
(counted from the later of the last video and the session streaming again
after a pause or unavailable capture), opens a fresh authenticated session
with the same target and path preference after 1, 2, 4, 8 and 15 seconds.
While direct is paused after a fallback on a path that cannot be punched,
those sessions take the relay at once; a retry while the host was away
pauses nothing, so the first session after it is back tries direct.
Retrying stops once failures have gone on for two minutes, counted from the
first since the last live picture and never while the viewer is hidden; the
session then ends, saying why, with Reconnect. Capture-unavailable states wait
for the remote desktop instead of retrying around missing Windows permissions.
Disconnect cancels recovery. A hidden viewer never reconnects; when it shows
again, a session the host closed meanwhile reconnects at once. A retry
restores previously requested control only after a fresh picture and host
permission. No queued clicks or keys are replayed. A remembered computer
reconnects by its id; a pasted invitation that was not remembered stays in
memory only.

Windows decodes H.265 or H.264 with Media Foundation into a D3D11 texture
it shares with Flutter ([Windows presentation](#windows-presentation)), or
takes JPEG where Media Foundation is not installed. Android decodes
with `MediaCodec` into a `SurfaceTexture` on Flutter's OpenGL renderer,
macOS and iOS with VideoToolbox into IOSurface-backed BGRA pixel buffers
handed to a `FlutterTexture`, and Linux with GStreamer into the decoder's
own NV12 or I420 pictures, which the runner converts to RGBA on the GPU into
an `FlTextureGL`.
Capture, encoding, decoding and blocking FFI calls stay off the Dart UI
isolate, and pinch and pan keep the same texture and decoder. On Android a
decoder reset is acknowledged by the codec worker, keeps the Surface, and cannot
close a newer codec; after a stall the renderer drops obsolete decoded pictures
but keeps compressed references. The decoders pull video from the session's
`VideoSink` themselves, so compressed video never crosses Dart: a Kotlin feeder
thread pulls through JNI on Android, and on every other platform a decoder
thread of the runner through the `pb_mapper_desktop_video_*_v1` C exports,
found in the library Dart loaded (`GetProcAddress` on Windows; `dlsym` on
macOS from the bundled dylib opened with `RTLD_NOLOAD`, on iOS from the
process, on Linux from the loaded `libpb_mapper_ffi.so` found with
`dl_iterate_phdr`), never a second copy: the instance token must match
Dart's. Each reports its presentations to `VideoConsumer::presented`
directly, when the compositor took the picture. Dart reads only status,
geometry, statistics and the latest cursor, through the per-frame view leaf.
A device with no decoder, Windows without Media Foundation and Linux without
GStreamer's decoders included, takes JPEG, which Dart decodes off the UI
isolate. Video bytes never become JSON or base64.

### What the viewer decodes

Before it connects, the app asks the native side through the
`pb_mapper/desktop_video` channel's `decoders` method. The method takes no
arguments and never creates a decoder, because a bad driver can hang there. It
answers a map: `h264` and `h265`, each either `{max_width, max_height}` (the
largest of the viewer's resolution bounds that codec decodes at) or null, and
`display_max_hz`, the fastest refresh among the displays as they are set now.
An error, a missing channel, or a probe that has not answered within 2 s
counts as H.264 at 1080p on Android and as no decoder everywhere else, where
the session then takes JPEG, which Dart decodes, for the attempts made
meanwhile; the app keeps its later answer. (Before 0.12 every platform took
H.264 there, which offered a desktop a decoder it might not have.) An Android
reply without a hardware AVC decoder still counts as H.264 at 1080p. The app
passes the answer to `connect` as
`decoders`, and the SDK's `ViewerConfig::for_decoders` makes the offer: H.265
(the `h265` capability) wherever this device has its decoder, then H.264, else
JPEG, with the limits fitted inside the largest picture of the decoder offered
first (`VideoLimits::clamped`, which compares long edge with long edge). A
computer that answers H.265 with H.264 is connected again at once without
H.265 when the H.264 decoder cannot take those limits. The Display menu
disables a chosen size whose picture of the remote display, once known, does
not fit, and says how large the default goes here when it is limited.

On Windows the runner probes on its own thread, one probe at a time, and calls
that arrive meanwhile share its answer. H.264 is reported, up to 3840×2160,
wherever Media Foundation is installed, since the in-box decoder takes every
bound. H.265 is reported where two conditions hold. First, `MFTEnumEx` finds a
synchronous video decoder MFT for `MFVideoFormat_HEVC`, which the HEVC Video
Extensions register and an unpackaged app finds. Second, Flutter's adapter has
DXVA `HEVC_VLD_MAIN` to NV12. Its bound is the largest one the driver lists a
decoder configuration for. The refresh comes from the active display paths as
an exact fraction (59.94, not 59), so Windows and Android report the same mode
alike. A process outside the console session, such as one in a remote
session, cannot query the paths and reads the display modes' whole hertz
instead; the app allows a hertz of slack, so 59 still offers 60.

On macOS and iOS VideoToolbox has no size query, so
the bounds are by device class: H.264 up to 3840×2160 on every Mac, where
VideoToolbox decodes in software when the hardware cannot, and on iOS
devices with HEVC hardware (A9 and later), else 1920×1080; H.265 up to
3840×2160 where `VTIsHardwareDecodeSupported` says so, else none. The refresh
is `NSScreen.maximumFramesPerSecond` (the fastest screen; before macOS 12 the
fastest display mode's rate) or `UIScreen.maximumFramesPerSecond`, 60 when
nothing is known.

On Linux the module looks in GStreamer's registry and builds nothing: H.264
is offered when any decoder factory for it exists, a hardware one (VA-API,
NVDEC, V4L2) at the largest viewer bound its caps allow, a software one at
1920×1080; H.265 only with a hardware decoder, as on Windows and Android.
A session's decoder obeys the same rule (`Allows` in `desktop_video_gst.cc`):
none decodes H.265, or H.264 above 1080p, in software. The refresh is the fastest
GDK monitor's. Without the module, without GStreamer, without decoder
plugins, or under `FLUTTER_LINUX_RENDERER=software`, which has no external
textures, `decoders` answers none and the session takes JPEG.

On Android, `DecoderProbe` reads `MediaCodecList(REGULAR_CODECS)` off the main
thread. It takes hardware decoders only: `isHardwareAccelerated` on API 29 and
later, otherwise not `OMX.google.*` or `c2.android.*`. For `video/avc` and
`video/hevc` it reports the largest bound that `areSizeAndRateSupported`
accepts at 60 fps. The refresh is the fastest rate `DisplayRefresh` would hold.

**Units.** `PbDesktopVideoMetaV1.flags` and the JNI meta carry `FLAG_H265`
(bit 4) on an H.265 unit. A unit carrying neither that bit nor the JPEG bit is
H.264. The 56-byte layout and its asserts are unchanged. A native consumer
configures its decoder from the unit's codec and rebuilds it when the codec or
the size changes, which happens only at an IDR.

On Windows, H.264 stays the in-box MFT and H.265 is the first decoder
`MFTEnumEx` offers, activated from its `IMFActivate`. The activation is shut
down after the transform is released, on every path, including a constructor
that fails partway. Either decoder must be Direct3D 11 aware, and it runs in
low-latency mode. The aperture check lets an H.265 decoder align its surface
to 64 pixels, against H.264's 16, but the visible rectangle must still equal
the authorized frame.

On Android the codec thread configures `video/hevc` or `video/avc` from the
unit. The key frame's parameter sets become the codec-specific data: for
H.264, the SPS and PPS as csd-0 and csd-1; for H.265, the VPS, SPS and PPS
together as csd-0.

The FFI's `sps_size` reads an H.265 SPS (NAL type 33), cropped by its
conformance window, for the geometry check. The test-source player recognises
an H.265 clip by the VPS in its first unit and treats IRAP pictures (NAL types
16 to 21) as its keyframes.

When a picture fails to decode as H.265, the app connects again at once on
H.264 and does not offer H.265 again until it restarts; only a failure on
H.264 too ends the session.

### Windows presentation

The decoder thread, on its own D3D11 device on Flutter's adapter, converts
each decoded NV12 picture to BGRA with the video processor into a private
texture, then copies it in one short copy into the one texture it shares
with Flutter. The shared handle stays the same for the decoder's life, and
Flutter's `ExternalTextureD3d` imports a texture again only when its handle
changes, so it imports this one once. The converter's view of the private
texture is made once per converter; a frame opens only its decoded picture.

Flutter reads the shared texture on its own device, calls the release
callback as soon as it has bound it, before any GPU read, and takes no keyed
mutex, so nothing orders its read after the decoder's copy. The thread
therefore publishes a picture, and tells Flutter to draw it, once a D3D11
event query says the GPU has run the copy, or after `kConversionWait`
(100 ms) so that a GPU that slow does not stop the picture. Meanwhile it
sleeps on an auto-reset event the GPU sets after the copy
(`IDXGIDevice2::EnqueueSetEvent`). The query stays the authority: a signal
an earlier frame left only wakes the thread early. Should a wait run out
although the copy had finished, or fail, every decoder polls the query
instead for the rest of the run, yielding between polls, and the runner
says so once in the debugger output
(`Desktop video: the GPU completion event`). The thread runs above normal
priority, below the keyboard hook's thread, so it gets its core back as soon
as it is woken. A tear remains possible: the next frame's copy can overlap Flutter's
read of the current one, as nothing orders the two devices, and that
Flutter frame then shows parts of both.

There is no native overlay to bypass Flutter's compositor. Flutter paints
the grey still picture, the peers' and the local cursor, the touch indicator
and every overlay above the texture (`viewer_stage.dart`), Flutter on
Windows has no platform views to put a native swap chain beneath them, and
a native window over the picture would hide them all.

Besides the counts, the `stats` method of the `pb_mapper/desktop_video`
channel reports, while a read asks for timings, `timings.surface_consume`
(from a picture's release to Flutter acquiring it) and `timings.convert`
(from opening a decoded picture for the converter to the GPU finishing the
copy, the wait and any decode work still queued on the GPU included), each
`{samples, mean_ms, max_ms}` since the timings began. It always reports
`refreshHz` (the fastest display's refresh, read as `decoders` reads it),
and while a decoder thread exists `decoderCycles` (its CPU cycles since it
started, from `QueryThreadCycleTime`). It reports `lowLatency` only when
the decoder's `ICodecAPI` took `CODECAPI_AVLowLatencyMode`, and otherwise
leaves it out: the decoder's attribute store, through which the mode is
also set, takes any value.

### Android presentation and decoder

The codec worker releases a decoded picture to the `SurfaceTexture` while the
surface has a credit, and it has two: at most two pictures released and not yet
consumed. The texture's queue is FIFO and Flutter's `updateTexImage` latches the
oldest, one per frame. Its frame-consumed callback carries the latched
timestamp, unique per decoded picture, so each consumption returns that
picture's credit and is reported to Rust as presented. While a credit is free,
pictures go to the surface in decode order; only with both in use does a newer
decoded picture supersede the one waiting. A phone's loop from release to latch
is longer than a vsync: the release must reach Flutter's scheduler before the
frame begins, and the latch comes during that frame's raster. With one credit,
each picture waited for the previous one's latch, so at 60 Hz the loop alone
held a 60 FPS stream to 30. Replaying measured 60 FPS arrivals through both
rules at 60 Hz, with 8 ms before the vsync and 10 ms after it, gave 30.1
presented FPS with one credit and 59.9 with two, and 8.6 ms less mean latency.
Where the loop fits in a vsync, the second credit added 3.5 ms; in steady
streaming it can add at most one frame.

Flutter folds every frame request made before a frame begins into that frame,
so a second picture released before the frame that latches the first would
wait for the next announcement. A consumption that leaves a picture queued
therefore asks the engine for another frame. `FlutterRenderer` has no public
way to ask; `TextureFrames.kt` sits in its package to reach the package-private
method its texture entries use, so a Flutter upgrade that changes it fails the
build. A latched timestamp also returns the credits of older pictures, which
the surface can then never latch; they count as dropped. The decoder never
drops a rendered picture (`KEY_ALLOW_FRAME_DROP` 0), and a newest picture that
something else drops is caught by the 2 s watchdog on the oldest released one,
which recovers at a keyframe.

The decoder is configured with hints: `KEY_LOW_LATENCY` (API 30 and later),
realtime `KEY_PRIORITY` 0, `KEY_OPERATING_RATE` at `Short.MAX_VALUE` and
`KEY_FRAME_RATE` 60, so a decoder sized for playback does not idle its clocks
between pictures. A hinted start that fails for any reason retries the same
decoder once without them, and a hinted codec that fails or stalls before its
first picture has its successor start without them; either way they stay off
for that codec on that texture. The other codec keeps them: a texture runs
H.265 and then the H.264 a failed H.265 session falls back to, whose decoder
never failed with them. `lowLatency` in the native stats says whether the codec
configured last ran with them (Android 11 and later, which have the low-latency
hint), and the acceptance tool's case F turns them all off.

While a texture has a source attached that is not paused, the window asks for
the display's fastest mode at its current resolution (`preferredDisplayModeId`):
at 120 Hz a picture released just after a frame began waits half as long for
the next one. The window's own preference returns when the last such texture
is paused, loses its source or is disposed. A source that closes or fails ends
its feeder, which tells the texture, even though Dart keeps the texture for its
last picture and for Reconnect; destroying the activity disposes every texture.
It costs battery only while video shows. The `stats` method of the
`pb_mapper/desktop_video` channel reports `credits` and `surfacePending` (both
the pictures released and not yet consumed) and `refreshHz` (the mode asked
for while viewing, otherwise the display's current one).

### macOS, iOS and Linux decoders

Both follow the Windows runner's rules: an older generation is dropped; a new
generation or a discontinuity waits for a keyframe, and a P-frame meanwhile
is dropped with a `keyframe_v1` request; a decoder is built or rebuilt only at
a keyframe, for a new codec or size; a size over 3840×2160 is refused before
any decoder sees it; `report_v1` names each configuration and `failed_v1`
each failure unless the source is paused; at most two textures. A stream
whose VUI names no colour matrix is read as BT.709, which every pb-mapper
host encodes.

**macOS and iOS** share `ui/darwin/DesktopVideo.swift`. An M3 Pro decoded
H.264 and H.265 through this runner; iOS has build coverage in CI. One
decoder thread per attached source pulls
with a 50 ms timeout and decodes synchronously in real time, so nothing is
reordered or held back. The runner writes 4-byte NAL lengths into the
received buffer when its slices are contiguous with room for each length.
Other layouts are packed in a reusable buffer. The runner keeps the parameter
sets of the last keyframe. A keyframe whose sets differ makes a new format
description and session. Pixel buffers come from VideoToolbox's pool; a unit costs the
`CMBlockBuffer` and `CMSampleBuffer` VideoToolbox requires. The newest picture
waits for the compositor, which takes it in `copyPixelBuffer`: the first time
it takes a picture of the current revision is the presentation receipt, and
a picture replaced before that counts as dropped. A session the system takes
back (`kVTInvalidSessionErr`, `kVTVideoDecoderNotAvailableNowErr`, as when
iOS backgrounds the app) is rebuilt at the next keyframe and does not count
as a failure. Log lines start with `PbDesktopVideo:`.

The `stats` method of the `pb_mapper/desktop_video` channel reports, besides
the counts, `lowLatency` (VideoToolbox took real-time mode), `hardware` once
a decoder is configured, and `refreshHz`. On macOS that is the refresh of the
display the Flutter window is on, read at each call so that it follows the
window to another display, or the fastest display's while the window is on
none; on iOS it is the device's. While a read asks for timings it adds
`timings.surface_consume` (from a picture's publication to the compositor
taking it) and `timings.decode` (`VTDecompressionSessionDecodeFrame`,
keyframes included), each `{samples, mean_ms, max_ms}` since the timings
began, and `decoderCpuMs`: the CPU time of the texture's decoder threads
(`CLOCK_THREAD_CPUTIME_ID`), which each thread reads itself after each unit
it decodes, so it trails by at most that unit. Each attach starts a thread,
and an ended thread's whole time stays in the figure, so it only grows and
two reads across a reconnect still give the CPU spent between them. Without
timings no thread reads a clock, and stopping them drops all three.

The iOS app links the whole FFI static library and keeps its symbols
(`-force_load`, `-Wl,-export_dynamic` in
`ui/ios/Flutter/PbMapperFfi.xcconfig`): Dart and the decoder find every
symbol by name at run time, so before this the linker took none of the
library and every lookup would have failed. CI's `ios-app` job
builds the app without signing and checks that it exports the FFI's entry
points. The iOS FFI is built without netdev, which does not compile for iOS
(libc defines its `in6_ifreq` for macOS only): there, LAN-first ranks
interfaces by their names and address ranges, and the app maps no router
port (`PortMapping::Auto` never asks, `Always` is refused), as an SDK built
without `port-mapping` does.

**Linux** (`ui/linux/runner/desktop_video.cc` for the channel and the
texture, `desktop_video_gl.cc` for the GPU conversion; the GStreamer module
`libpb_mapper_desktop_video.so`, built from `desktop_video_gst.cc` when the
GStreamer development files are present and loaded with `dlopen` at the
first probe or session, so the app starts without it; module and runner
share the C interface `desktop_video_module.h`, whose ABI number, 2, rejects
a stale module). Each source's decoder thread pulls straight into pooled
`GstBuffer`s (one copy of the compressed data) and feeds `appsrc →
h264parse/h265parse → decoder → appsink`, pulling the next unit only while
fewer than four are inside the pipeline, so a backlog stays in Rust's sink,
which drops to an IDR. Decoders are chosen from the registry, hardware first
(VA-API, NVDEC, V4L2) and then by rank; one that fails to start is skipped,
and one that fails or stalls before its first picture is left out for the
rest of the run (and from later probes), so the next keyframe falls back to
the next decoder the probe's rule allows. When none is left, every keyframe
is a decoder failure: the session ends as `decoder_failed`, and an H.265
one connects again on H.264. A
decoder with units inside and no picture for 2 s is rebuilt with a keyframe
request, as on Android. The appsink takes the decoder's own picture: NV12
or I420 in system memory, which a software decoder and NVDEC give, or, with
GStreamer 1.24 or later (whose DMA_DRM caps say a dma-buf's format and DRM
modifier), NV12 in dma-bufs of the layouts Flutter's EGL display imports
plane by plane as R8 and GR88 textures, as GStreamer's VA-API decoders
give them. The runner reads those layouts from the display Flutter renders to
(`eglGetPlatformDisplayEXT` on GDK's display) before each decoder starts;
`PB_MAPPER_VIDEO_DMABUF=0` keeps every picture in system memory. A decoder
whose pad templates offer neither gets `videoconvert` to NV12. The
allocation query answers that GstVideoMeta is understood, so a decoder hands
over padded rows or a 1088-row surface as it keeps them instead of copying,
and that three pictures stay out of its pool: the newest, the one on screen
and one the GPU may still read. The texture is an `FlTextureGL`: its
`populate`, on Flutter's raster thread with Flutter's GL ES context current,
takes the newest picture and draws it once into an RGBA8 `GL_TEXTURE_2D`
through a framebuffer. Only then does it present the picture, which is the
presentation receipt: a picture it cannot convert earns none, and reaches
Rust as a decoder error at most once a second, so a GL context that can show
nothing ends the session as `decoder_failed` rather than leave a still
picture while the host takes input. A system-memory picture's planes are
uploaded with `glTexSubImage2D` (R8 and RG8, or LUMINANCE and
LUMINANCE_ALPHA on GL ES 2 without `GL_EXT_texture_rg`): 1.5 bytes a pixel
instead of the 4 an RGBA upload takes. The picture on screen stays out of the
decoder's pool until the next one is converted, so that a snapshot can
convert it; it is one of the three the allocation answer counts. A dma-buf
picture's planes are imported once per pooled buffer as EGLImages and
sampled where the decoder wrote them; an EGL fence after the draw returns
the buffer to the decoder once the GPU has read it, at most two being held.
An imported plane keeps its buffer alive, so the imports go when the picture
size or layout changes or the pictures come in system memory, and any not
sampled for 120 conversions goes too, 64 at most being kept. The shader
takes the matrix and range from the caps (Kr and Kb, limited or full; a
stream that names none is BT.709 limited) and samples chroma nearest, as
GStreamer's converter does without chroma resampling. It saves and restores
every GL binding and switch it touches, as Flutter's own compositor does.
Before the first picture the texture is one black pixel; a picture that
cannot be converted leaves the last one on screen. A dma-buf that cannot be
imported (no `GL_OES_EGL_image`, or a layout the driver refuses) makes the
module rebuild the pipeline in system memory at the next keyframe, which it
asks for; that refusal is no decoder error. When the texture is disposed,
its GL objects (the RGBA texture, the planes, the imports) are deleted at
once in a context of their own that shares Flutter's, made current with no
surface on a thread of its own, since the GTK thread that finalizes the
texture may have GDK's context current; where EGL cannot make one, the next
conversion in Flutter's context deletes them. `snapshot` scales and converts
a system-memory picture with GStreamer's converter; a dma-buf picture, which
only the GPU reads, is read back at 320 pixels wide for it, at most once a
second, and once more a second after the last read when a picture shown
since is not in it, which a timer brings `populate` back for. A snapshot
whose still is older than the picture on screen asks for it again and waits
up to 100 ms for Flutter's next frame, keeping the older still if the
texture is no longer drawn. `stats` adds, while a
read asks for timings, `timings.decode` (from a unit entering the pipeline
to its picture reaching the appsink), `timings.convert` (the conversion
on the raster thread) and `timings.surface_consume` (from the module's
`frame_ready` for a newer picture to `populate` taking it; a raster pass
already under way can take a picture before its `frame_ready` runs, and that
sample then counts from an earlier one, or is skipped), each
`{samples, mean_ms, max_ms}`, and always `refreshHz` (the fastest monitor's,
read as `decoders` reads it), `dmabuf` (pictures arrive as dma-bufs) and
`videoconvert` (the pipeline converts). Without timings `frame_ready` reads
no clock. `G_MESSAGES_DEBUG=pb-mapper-video` shows the module's log,
including the caps each decoder gives, so whether dma-bufs were negotiated;
`GST_DEBUG=3` shows GStreamer's. A viewer needs GStreamer's base and
bad plugins and a decoder: gst-libav (or openh264 for H.264) in software,
the VA-API driver, or NVIDIA's driver; the user guide lists the packages.
The release bundles no GStreamer library: `scripts/build/check-linux-bundle.sh`
fails a bundle without the module, with a GStreamer or ORC library inside, or
whose FFI or module does not load.

## Measurements and remaining acceptance

Development environment: Windows 11 Enterprise 10.0.26300, Ryzen 9 9950X,
NVIDIA RTX 5070 Ti, 2560×1440 display at 165 Hz; Rust 1.99.1 and Flutter 3.44.9.
The [UI acceptance procedure](../scripts/test/desktop-ui-acceptance.md) measures
native texture consumption through the product viewer and decoder.

### This revision: platforms

What each platform's new code has been through:

- **Windows host.** It ran on the machine above. `input_acceptance
  --run-owned-window` drove the product backend over a local relay, into a
  window it owns:
  - the shapes it read were arrow, I-beam, resize and a custom monochrome
    cursor, the last matching pixel for pixel through a viewer;
  - `Text("a\nb\tc")` read back `a\r\nb\tc`, where the old path typed
    `a\r\n\r\nb\t\tc`;
  - physical keys typed `hi!` by the active layout;
  - through the same keys, the active input method (WeType on this PC)
    composed `ni`;
  - held keys were released at the disconnect.
- **Windows app.** The real app (the acceptance entry of
  `ui/tool/desktop_viewer_acceptance.dart`) viewed `desktop_ui_host`, a host
  that replays a recorded clip in five bands, one pointer shape each, and logs
  every input it applies (see
  [the acceptance procedure](../scripts/test/desktop-ui-acceptance.md)). It
  ran on this machine over the relay on loopback, so the host was a replayed
  clip, not a real remote desktop.
  - Passed:
    - the picture through the native texture at 60 FPS;
    - viewing without control, the host's shape (arrow, I-beam, resize, hand
      and the custom bitmap, pixel for pixel) painted at the host's pointer;
    - in control with the host's pointer elsewhere, the same;
    - a host without `cursor_shape` drew the generic arrow;
    - the Shortcuts menu for a Windows host and for a Mac host (the swap
      entry, and **Switch input method** as Win+Space or Ctrl+Space);
    - **Type text…**.
  - Passed with the real pointer and real keystrokes, through the active
    input method (WeType, in its Chinese mode):
    - in control, the local pointer took the host's arrow, I-beam, resize and
      hand, and hid over the custom shape with the bitmap painted at it, so
      one pointer showed; outside the picture it was the arrow again. This
      run predates native pointer shapes, which make the custom shape the
      local pointer itself and have not run on Windows (see **Native pointer
      shapes** below);
    - in the local mode, `nihao` composed with the candidates at the remote
      pointer and sent nothing, and Space sent exactly `你好`;
    - Enter and Backspace stayed with the input method while it composed,
      and went as keys after;
    - Chinese punctuation arrived as `，` and `。`, a lone Shift sent
      nothing, Ctrl+C arrived as the chord, and Shift or Ctrl went before a
      click;
    - in the remote mode, letters went as physical keys and nothing composed
      here: the window's input method was detached;
    - the input method came back in **Type text…** opened from the remote
      mode, on switching back to the local mode, and after the app was
      closed in the remote mode and opened again.
  - Passed with input posted to the app's view, which bypasses the input
    method: in the remote mode, Shift went at once and a lone Win not at
    all. An earlier run posted all its input this way while pending Windows
    Firewall prompts dimmed the screen. It found that the local mode lost
    every printable key on Windows: the embedder refuses a text-input client
    that names no view. The client now names its view, as `EditableText`
    does.
  - Not run: a real second computer as the host, and Microsoft Pinyin or any
    input method other than WeType.
- **Android viewer.** The Android emulator ran the same entry against the
  same host.
  - Passed: the painted shapes, standard and custom; trackpad strokes moving
    the host's pointer; the soft keyboard's committed text and Backspace.
  - The emulator presented only 5 FPS at 640×360, and not at 720p.
  - Pinch zoom could not be injected, so the bitmap's growth with zoom was
    not seen; its 32-logical-pixel minimum was.
  - Not run: composition by a Pinyin soft keyboard, which adb cannot drive.
- **Helper.** The helper at protocol 8 is tested over an in-memory pipe
  with a synthetic worker, not installed.
- **Linux viewer.** It ran in WSL2 (WSLg) on this machine, through the RTX
  5070 Ti by `/dev/dxg` with NVDEC, and with gst-libav. A clip harness
  outside Flutter passed every check: receipts, discontinuities,
  pause/resume, a size change mid-stream, a 1.27 MB keyframe, and H.265. The
  real app ran against a host replaying recorded clips. The figures and
  their limits are in
  [the 2026-11-12 record](benchmarks/2026-11-12-remote-desktop-platforms/README.md#the-viewers).
  VA-API, V4L2 and a real Linux desktop have not run. That run predates
  the GPU conversion, which is built and whose clip harness runs only in
  CI, on llvmpipe; its dma-buf path (GStreamer 1.24 with a VA-API decoder)
  has not run.
- **macOS host.** Compiled, linked, linted and unit-tested on a macOS
  runner in CI. The unit tests cover the VideoToolbox encoder, the display
  list, the system pointer read off the main thread in a window-server
  session, and thread C's waiter, whose 2 ms waits end less than 2 ms late
  at the median. On the same runner the session tests pace a 60 fps session
  at 60 frames a second ([Timers on a Mac host](#timers-on-a-mac-host)). Its
  pure parts are also tested on Windows and Linux. On an M3 Pro the native
  host captured both a Retina panel and an external display, and streamed
  the external display to the Mac viewer over a loopback relay. Its
  VideoToolbox rate control (the QP bound, the resubmit of a dropped
  picture) ran on an M6 against synthetic pictures
  ([the 2026-11-24 record](benchmarks/2026-11-24-macos-rate-model/README.md)).
  Not run: a Mac host behind a shaped 4 Mbit/s link playing video (the
  viewer's `received_fps` against `macos_encoder_dropped`), and one behind
  2 Mbit/s taking control and typing at once (the echo's delay).
- **macOS and iOS viewers.** CI builds the macOS app and the unsigned iOS
  app with the Swift decoder. The iOS app links the FFI static library and
  exports its entry points. The M3 Pro viewer decoded H.264 and H.265 in
  hardware and wrote a thumbnail on disconnect. iOS device testing is pending.
- **Native pointer shapes.** The desktop viewers' host bitmaps as their own
  cursors (`pb_mapper/desktop_pointer`, see
  [The viewer's pointer](#input-and-cursor)) have run only on a Mac.
  - The Dart side ran in widget tests, with the engine's and the runner's
    channels mocked and the target platform set to Windows, macOS and Linux.
  - The macOS runner ran in a release build of `ui/tool/pointer_latency.dart`
    ([the 2026-11-24 record](benchmarks/2026-11-24-native-cursor/README.md)).
    It makes the calls the cursor session makes and times them against the
    rebuild they replace, and times the painted pointer the native one
    replaces. A synthetic mouse inside the window stood for the pointer, so
    nothing there shows how a cursor looks.
  - The Windows and Linux runners build only in CI, and were checked here
    only against stub headers; the Linux runner's hotspot padding also ran
    against fake cairo and GDK calls.
  - Not run anywhere: a real pointer over `desktop_ui_host`'s bands on any
    viewer, the macOS one included (the host bitmap's look, size and hotspot,
    and a busy band showing the host's hourglass); on Windows, a custom shape
    and a Mac host's open hand as the cursor, and
    `GetGuiResources(GR_USEROBJECTS)` across 200 shape changes; on Linux,
    X11 and Wayland, at scale 1 and 2; and a high-speed camera over a
    painted and a native pointer following the mouse.
- **Older peers.** A host and a viewer of this revision each ran against
  the other side built from the tree before it (`151ed8d7`, 0.11.0's desktop
  protocol), with the SDK's synthetic host (`desktop_loopback --serve`) and
  `desktop_control`. Both directions connected, took control and applied
  moves, a click, text and Enter, with no protocol error. Shapes and
  physical keys toward an older peer are covered by tests: the 0.11 bytes for
  a viewer without `cursor_shape`, and the viewer's refusal toward a host
  without `physical_keys`.
- **iOS FFI library.** It is built without netdev.
  - Why: netdev 0.46.3 uses `libc::in6_ifreq` for every Apple target, but
    libc defines that type for macOS only. So no iOS FFI had compiled since
    netdev came in, in 0.10.0.
  - The cost: the iOS app ranks LAN interfaces by name and address range,
    and maps no router port.
  - This lasts until netwatch takes a netdev that gates the type (netdev
    0.47) or libc defines it for iOS.

The [M3 Pro hardware record](benchmarks/2026-11-17-macos-desktop/README.md)
covers the macOS viewer, host and window lifecycle. The data paths were measured before
and after on Windows in
[the 2026-11-12 record](benchmarks/2026-11-12-remote-desktop-platforms/README.md):
- unchanged allocations per video unit, with and without pointer input;
- no change beyond noise in the H.264 and H.265 encoders' calls.

The pointer-shape path, when negotiated, has not been measured.

### This revision: receipt credit

No phone or Windows viewer run of the receipt-credit pipeline has been recorded
yet. Its evidence so far is the encoder bench above and the synthetic cases
below. A session runs on the preset remembered for its computer. Balanced, the
default, never exceeds 60 FPS, so only a Smooth run on a Windows viewer above
60 Hz compares with the earlier builds' Windows rates below.

The ignored `slow_link` cases
(`cargo test -p pb-mapper-desktop -- --ignored --nocapture slow_link`) use a
real relay and TLS, a shaped viewer link, a synthetic asynchronous H.264 encoder
(8 ms latency unless stated, output lost after 250 ms, a per-frame budget of
`bitrate / nominal`) and a simulated decoder. They observe the pipeline, not a
GPU or real rendering. One release-build run on this machine, with other builds
running, and the timer guard held by each session; "before" is the same case
on the build before the split capture gate, the median delay and the guard:

| Case | Result |
| --- | --- |
| 18 ms one way, unlimited (`lan_rtt_keeps_60`) | Pace at the ceiling 0.98 s after connecting (5.97 s before); 59.9 received FPS over the following 10 s; RTT 39 ms (52 ms before, when the shaped link's own timers ran at 15.6 ms) |
| Three 600 ms stalls 6 s apart on that link (`isolated_stall_does_not_collapse_fps`) | The pace stayed at 60 through each (before: 60 → 5, 24 → 5 and 12 → 6, never back at 55 within 6 s) |
| Encoder holding each picture 45 ms (`encoder_latency_of_45ms_keeps_60`) | 60.0 received FPS, 0 skipped ticks (before: 40.0 received FPS at a pace of 60, 332 ticks skipped) |
| Fixed 350 ms RTT | Pace 60 (the ceiling); bitrate 4.91 Mbit/s from a 4.0 start; 60 presented and 59.8 received FPS; one keyframe; one size (1920×1080); the encoder configured once |
| 1 Mbit/s for 8 s after 6 s unlimited | Window clamped 1.0 s after the limit; lowest pace 10; lowest expected bitrate 645 kbit/s; back to at least 3.2 Mbit/s 1.0 s after the limit lifted (8.0–11.1 s before, when fps recovered by a quick step every three seconds) |
| 1 Mbit/s with IDRs of 162 KiB | One keyframe, none requested; the encoder bitrate fell to 2.77 Mbit/s; 19.9 received FPS |
| Decoder taking 67 ms a frame | Host pace held at a mean 13.4 FPS by the viewer's fps cap, while the delay controller stayed at 60 FPS and the encoder bitrate at 7.9 Mbit/s: presentation lag never reaches congestion control |
| 256 KiB/s link, 64 KiB frames, 30 s | Streaming throughout at both ends; capture held back (575 skipped ticks) instead of queueing, the link full at 2.09 Mbit/s received; control 8 ms, release 9 ms, revoke 3.5 ms (61, 47 and 15.7 ms before the timer guard); host stop 348 ms, which includes waiting for the viewer to read `Stopped` |

Two further cases time a fresh picture on a 1 Mbit/s link with the encoder
holding each picture 45 ms: `requested_idr_on_a_slow_link`, where the
decoder asks for an IDR every 2 s and discards what arrives before it, and
`idr_another_viewer_asked_for_on_a_slow_link`, where a second viewer on an
unlimited link asks instead. Each reports how long every IDR took to arrive
and the P-frames that came first, the viewer's requests, its sink's discards
and the host's dropped frames and IDRs. No run of them is recorded yet.

Two more run the slow presenter and the bandwidth limit on an encoder that
budgets per second (the bitrate over the time since the previous capture, at
most 1 s, so a picture after idle carries the idle's bits too). Each reports
when the host latched the `timestamp` model, from the first capture or from
the limit, and from the start of the last stretch the encoder was asked for
45 FPS (0.75 × the nominal 60) or less, and the sent bitrate over the
expected one before and after the latch.
`per_second_encoder_latches_under_an_fps_cap` asserts the latch within 2.5 s
of that stretch's start and the expected bitrate after it.
`per_second_encoder_on_a_bandwidth_limit` asserts only that the session
recovers: there the per-second pictures fill the 1 Mbit/s link, capture waits
for credit and the pace falls below 3 FPS, so no 2 s of ticks holds six
frames until pictures flow again after the limit, and by then the pace is
climbing back. Every case on a per-frame budget asserts that nothing latched.
The
[rate-model latch measurements](benchmarks/2026-11-24-rate-model-latch/README.md)
record them before and after the per-tick latch.
Six cases cover the viewer's fps cap and its backlog, all on simulated
decoders: `slow_decoder_speeds_up` (67 ms a frame for 20 s, then 5 ms),
`pipelined_slow_consumer` (four units held inside, as the Linux runner holds
them, decoded at 67 ms each), `fast_consumer_at_120` (10 ms a frame at 120
FPS), `injected_backlog_at_120_recovers` (one second at 20 ms a frame in a
120 FPS session on the short path) and `consumer_stall_drains` with its
1 Mbit/s twin (a 2 ms decoder that stalls once for 400 ms). The 67 ms decoder
case now runs 60 s. [Viewer fps cap measurements](benchmarks/2026-11-24-viewer-fps-cap/README.md)
has three runs of each before and after the cap measured its decoder.

`video_bench 10 60 --animate --max-width 1920 --max-height 1080` still reaches
59.9 unique capture and encode FPS at a mean 1.9 ms encode with one unit in
flight, and 58.2 FPS through the full backend (`--backend`). With
`--codec h265` it reaches 59.6 FPS at a mean 1.65 ms, and 58.1 FPS through the
backend; [Encoder rate model per GPU](#encoder-rate-model-per-gpu) has the 4K
runs.

Proposed phone criteria, which the owner decides: on a 60 Hz phone over direct
Wi-Fi with Balanced, presented FPS p50 ≥ 55; present interval p95 ≤ 25 ms and
p99 ≤ 50 ms; after a 5 s warm-up no gap over 250 ms and at most one over 100 ms
a minute; picture latency p50 ≤ 80 ms with a camera check; keyframes equal to
one plus those requested; never unresponsive; and `input_wait` within its gate.
Relay and cellular runs must show no burst-and-stall pattern, at least 30 FPS
through the relay, and no resolution change. The owner lifted the merge block
on 2026-11-10 on their own use of the stage7 build, without such a run.

### Earlier builds: presentation credit

The final build 922 native Windows run measured **84.75 presented FPS** at
1920×1080 over direct, with 89.91 unique capture/receive FPS and mean frame ACK
17.35 ms over 60 samples. The final Android VM relay smoke measured 14.24 FPS
and 427.10 ms mean ACK over 60 samples; neither run had errors, reconnects or SDK
queue loss. Android had no decoder recovery, and normal disconnect left the host
waiting without a TLS error. See
[`validation/desktop-922.md`](validation/desktop-922.md) and its counter evidence
for the environment, longer diagnostic baselines and remaining limits.

The earlier 0.10.2-based Windows revision measured **89.58 native presented FPS
at 1920×1080 over the actual direct path**, after a five-second warmup in a
30.05-second run. Unique capture/receive averaged 90.00 FPS; steady-state samples
had no viewer errors or SDK/native queue drops. A later reviewed revision with
an always-visible moving pattern measured **85.19 presented FPS**, 89.19 unique
capture/receive FPS and zero SDK/native drops. Their counters, environment and
binary hashes are preserved in
[`validation/windows-direct-1080p90.json`](validation/windows-direct-1080p90.json)
and
[`validation/windows-direct-1080p90-reviewed.json`](validation/windows-direct-1080p90-reviewed.json).
These are independent evidence for those older revisions on one Windows machine,
not a performance pass for the current package. The installed build 922's recent
69–77 FPS self-view observation was not a controlled 90 FPS benchmark.

The build 922 Android diagnostic baseline uses Android 36 x86_64 emulator
`emulator-5580`, `c2.goldfish.h264.decoder`, a 60 Hz virtual display and host OpenGL
on the RTX 5070 Ti (`-gpu host`). Each path ran for approximately 120 seconds,
without concurrent builds. These runs used the eight-frame admission window and
33 ms ACK coalescing. Their congestion reference was kept in build 922 and the
subsequent reference-mean experiment was reverted; receipt credit replaced all
of it. These are diagnostic baselines, not a physical-phone performance pass.

| Android VM path | Native presented FPS | Submit-to-consume mean | Surface release-to-consume mean | Reconnects / SDK drops / native recoveries |
| --- | --- | --- | --- | --- |
| Direct | 17.78 | 89.00 ms | 48.42 ms | 0 / 0 / 0 |
| Forced relay | 15.94 | 87.20 ms | 50.53 ms | 0 / 0 / 0 |

Both runs adapted mostly to 640×360. The native renderer skipped 1,076 / 489
obsolete decoded pictures respectively; these are distinct from compressed SDK
queue loss. Final native-presented and transmitted-ACK generation/frame IDs
matched, with no ACK errors and one application keyframe request per run.
Local reports are `target/issue50-922/direct-window8-ack33.json` and
`target/issue50-922/relay-window8-ack33.json`; neither contains controller setup.
Flutter build time averaged about 2.7 ms and raster time 15.7–16.8 ms. After
warmup, additional Flutter draws beyond new video presentations were fewer than
one per second. No fixed application 20 FPS limit or dominant repeated-rebuild
cost was established. Surface delivery, GPU work and Flutter scheduling remain
partially combined in the timings; physical-phone 60 FPS is unverified.

A 120-second experiment replaced the lifetime-minimum reference with a low-load
interval mean. It restored substantially higher resolution but reduced the VM's
native presented rate from 15.94 to 13.66 FPS, raised sampled mean ACK delay from
384.3 to 439.6 ms, and increased decoder configurations from 5 to 17. That
experiment was not in build 922. The one-second averaging of congestion
observations, eight-frame bound and exclusive recovery-IDR credit were kept in
build 922; this revision removed them with the presentation credit.

### Lifecycle, input and transport

Installed build 922 passed a persistent-host lifecycle check: one Enable action
left `PbMapperDesktop` Running with Automatic startup. After fully terminating
the UI and reopening the exact protected installed executable, sharing appeared
automatically, the service retained PID 99536, and the copied invitation's SHA-256
matched the value before UI exit. Stop removed the sharing banner and restored
Enable; enabling again resumed sharing within seconds without another UAC prompt
for the already-authorized installation. Replacing the clipboard with a marker
before copying again confirmed that the unchanged invitation was freshly copied.
That first check used a candidate. On 2026-11-06 the final helper, FFI, UI and
Dart AOT files were installed and matched the delivered package hashes. The
upgrade restarted the service as PID 97260 and restored sharing with the same
invitation. Final UI direct and forced-relay viewing passed. With all desktop
test UI processes exited, a separate production-FFI viewer received fresh video
over both paths using a TCP relay carrier. Stop also persisted across a full UI
exit/reopen; Enable restored the same invitation without another UAC prompt.
See [the final installation evidence](validation/desktop-922-installed.json)
and [its limits](validation/desktop-922.md#final-protected-service-update-2026-11-06).
This verifies upgrade-driven service replacement and UI exit/relaunch, not OS
reboot, standalone SCM restart, sleep/resume or remote control of a UAC prompt.

Build 922's Android VM run of the PR #51 gestures, since replaced (see
[Gestures, keys and modifiers](#gestures-keys-and-modifiers)), passed against a
benign owned Windows target with event counters: touch tap, hold-drag,
two-finger right click and pinch, plus trackpad hold-drag/right click. Text
insertion and scrolling passed in the preceding input run. These were actual
controller-to-target checks; they do not establish physical touchscreen or
secure-desktop behavior. The current gestures have not yet reached a target;
they are item 7 of
[Stage 3](../scripts/test/desktop-ui-acceptance.md#stage-3-full-acceptance).

Supporting experiments remain separate from UI presentation acceptance. Earlier
split-TLS transport probes measured 43.62 / 32.26 Mbit/s with a 194 ms median ping
round trip through the deployed TCP relay, and 42.98 / 38.63 Mbit/s with a
238.80 ms median using a separate SSH consumer exit. Logs are
`target/desktop-transport-deployed.log` and
`target/desktop-transport-ssh-exit.log`. They demonstrate carrier capacity and two
relay entry paths, not two independent NATs. The earlier SDK/fixture and simulated
presentation-delay results are not current Android UI rates. A forced 350 ms
GPU-output pause recovered on an independent frame; all 172 exported 1280×720
frames decoded without error (`target/desktop-pause-recovery.log`).

Older synthetic-source / real-TLS tests separately exercised slow-reader and
presentation-feedback recovery on the presentation-credit pipeline. Their logs
are `target/desktop-feedback-acceptance-optimized.log` and
`target/desktop-slow-tls-acceptance.log`; the `slow_link` cases above replace
those tests. These do not measure image quality or physical presentation.
Deployed probes use dedicated
expiring scopes and an explicit TCP override without changing saved application
routing; QUIC carrier and independent cross-network UI acceptance remain separate.

| Capability | Current evidence / limit |
| --- | --- |
| Windows ordinary desktop | Capture and native video paths exercised on the machine above |
| Input / Unicode / touch mapping | Build 922's PR #51 gestures and text/scroll reached a benign Windows target from the Android VM; the current gestures, physical-device and secure-desktop acceptance outstanding |
| Direct and forced relay | Android VM runtime baselines above and deployed TCP experiments recorded; independent cross-network UI acceptance outstanding |
| Android viewer | Native MediaCodec/Flutter texture path exercised in an Android VM; physical phone validation outstanding |
| iOS viewer | Native VideoToolbox path; the app, linking the FFI static library (built without netdev, so no port mapping on iOS), is built unsigned in CI with its exports checked; device validation outstanding |
| macOS viewer | VideoToolbox hardware decoding of H.264 and H.265 on an M3 Pro; thumbnail written on disconnect |
| Linux viewer | Native GStreamer path ran in WSL2 (WSLg) with NVDEC and gst-libav, against a host replaying recorded clips; CI builds the app and decodes clips with software decoders; VA-API, V4L2 and a real Linux desktop outstanding |
| macOS host | Compiled, linked and unit-tested in CI on a macOS runner (the encoder, the display list, the pointer read in a window-server session, thread C's waiter, a 60 fps session paced at 60 frames a second); view-only 1080p H.265 streaming on an M3 Pro |
| Pointer shapes | Windows host's shapes read over an owned window through a viewer, the custom one pixel for pixel; the Windows app and the Android emulator paint the host's shapes; in the Windows app, against a host replaying a clip, the real local pointer took the host's shapes in control, one pointer showing; the bitmap's growth with Android pinch zoom not seen; macOS cursor shape fidelity awaits hardware comparison |
| Keyboard modes and physical keys | Windows host injection, line breaks and the active input method's (WeType) composition through physical keys checked in an owned window; in the Windows app, against a host replaying a clip, real keystrokes through WeType checked: composition at the remote pointer to exactly the committed text, Enter and Backspace while composing, punctuation, the modifier rules, the remote mode's physical keys with the local input method detached, and the input method coming back; the Shortcuts menu checked with input posted to the app's view; a real second computer, Microsoft Pinyin and other input methods, and a Pinyin soft keyboard on Android outstanding |
| Elevated windows / secure-desktop UAC | Helper implemented; default-policy administrator and standard-user prompt acceptance outstanding |
| Remembered app-owned host policy | Current-user DPAPI; requires signed-in user and running app |
| Windows service-owned background host | Final build 922 passed upgrade restart, live direct/relay delivery with its UI exited, and persistent Stop/Enable with the same invitation; OS boot, standalone SCM restart and sleep/resume acceptance pending |
| Mac display lit when it sleeps | The wake limiter, the closed-lid rule and the 1 s left to a display that turns off under capture are unit-tested on every platform, and the online display list, the lid read and finding the shared display run in the macOS CI job; a session that starts on a sleeping display, a resume after the display slept, a display put to sleep during a session (H.264 and JPEG), `pmset -g assertions`, a locked Mac staying dark (whether the lock is reported within 1 s of the display turning off) and a MacBook with its lid closed, outstanding on a device |
| Windows display and system kept awake while capturing | The keeper's timing and the watchdog's loop are unit-tested on every platform and its power request is created in the Windows CI job; a viewer past the display-off timeout, `powercfg /requests` during capture, a network stall and a pause, and waking a dark display, in the app and through the service, outstanding on a device |
| Same owner's locked login / password entry | Session identity and desktop transition handling implemented; actual password-entry acceptance pending |
| Pre-login boot, SAS, control across user switching | Unsupported; service waits for its approved owner's login |
| Rotated GPU capture, Linux hosts, macOS before 12.3, Windows hosts without Media Foundation | Unsupported |
| Audio, clipboard, file transfer, recording, multiple viewers | Not implemented |

Background operation follows each platform's own constraints. Linux needs its
own host capture/input backend and user-session service integration. macOS
hosts from the app only: lock-screen and login-window hosting would need a
LaunchDaemon and LaunchAgent pair serving `PlatformBackend` over `oop`, and
unattended hosting on macOS 15, which asks the owner to confirm screen
capture again periodically, would need Apple's persistent-content-capture
entitlement; neither is implemented. Android remains
a controller here. Mobile hosting is not implemented: Android screen projection
requires session consent and a media-projection foreground service, and iOS
does not provide unlimited general background execution for a remote-control
host. See [Android media projection](https://developer.android.com/media/grow/media-projection)
and [Apple background execution](https://developer.apple.com/documentation/BackgroundTasks/choosing-background-strategies-for-your-app).
No all-platform background-host acceptance is claimed.

Focused verification includes desktop/FFI tests for identity pinning, durable
identity/revocation, DPAPI corruption, helper ACLs, input generations, receipt
credit, delay-based QoS, the viewer's sink, fps cap and recovery, the rule that
receipts never grant input, ownership and teardown, plus Flutter desktop
interaction tests.
Run the required workspace fmt/Clippy and relevant networking/SDK suites before
delivery. Passing mocks or an isolated codec benchmark cannot complete UAC,
physical-device, independent-network, sustained slow-link or 60+ FPS UI acceptance.

The helper now releases held input and desktop-affine encoder resources before
`SetThreadDesktop`, retries failed binding without committing a new generation,
and handles a desktop-name change even if its event was missed. State-boundary
tests do not establish actual UAC operation. The local host is a physical Windows
machine and Hyper-V inventory returns an authorization failure; no accessible
Windows guest was identified. The running Android VM cannot validate Windows
secure desktop. Use the read-only collector and explicit acceptance matrix in
[`scripts/test/windows-helper-acceptance.md`](../scripts/test/windows-helper-acceptance.md);
host evidence is in `target/issue50-vm-acceptance/`. No UAC policy was changed and
no consent dialog was approved by automation.

The earlier `input_acceptance --run-owned-window` probe independently read back
Chinese, Unicode and emoji and observed mouse input through a dedicated TCP
relay (`target/desktop-native-input-acceptance.log`). Its alternate SSH-entry
attempt aborted with `applied=0` when the owned target lost foreground; that
preserves the input boundary but does not pass cross-network input acceptance
(`target/desktop-native-input-cross-egress.log`).

## Components, builds and licensing

The reference checkout is `../rustdesk` at
`49516c98574f117eea05e10f65c580c37b144e6c`, with its hbb_common submodule. RustDesk's
AGPL code and networking runtime are not copied or linked into this MIT project.
The implementation follows capture/codec/service techniques without adopting a
second rendezvous or networking runtime.

Dependencies include windows-capture 2.0.1 (MIT), windows-service 0.8.1
(MIT OR Apache-2.0), enigo 0.6.1 (MIT), xcap 0.9.8 (Apache-2.0) for monitor/JPEG
compatibility, image (MIT/Apache-2.0), and tokio-rustls/rustls. rustls is accessed
through `quinn::rustls`; versions are pinned in workspace dependencies. The
macOS host binds ScreenCaptureKit, VideoToolbox, Core Graphics, ImageIO,
AppKit and the Keychain through the `objc2` family (MIT, under
`[target.'cfg(target_os = "macos")'.dependencies]`), not enigo. Video
uses installed Windows Media Foundation/driver codecs, Android MediaCodec,
Apple's VideoToolbox, and on Linux the system's GStreamer (LGPL) and its
plugins, which the app loads but never bundles; no FFmpeg executable or
libraries are bundled (gst-libav, where the user installed it, is the
system's). The macOS FFI library links ScreenCaptureKit weakly
(`-weak_framework`, set by `ui/native/pb_mapper_ffi/build.rs`), so the app
still starts on macOS 10.15 to 12.2, where it cannot share. The desktop
crate's and the SDK facade's own tests and examples link it weakly too, in
their build scripts; a build script cannot link another crate's binary, so
an SDK binary that should start below 12.3 passes
`-C link-arg=-Wl,-weak_framework,ScreenCaptureKit` itself. objc2 looks a
class up at run time, never through a static class reference, so no class
newer than the running system stops a binary from starting either; the
code checks the system or the class (`SCScreenshotManager`, macOS 14, for
stills) before it calls one. The app and the
FFI DLL delay-load `mfplat.dll`. Where it is missing (Windows N/KN without the Media
Feature Pack, Windows Server without the feature), the app still starts, this
computer cannot share (a host refuses to start, so a remembered grant is not
restored either) and the Remote page says why, and viewing falls back to JPEG.

Build Windows with `make build-pb-mapper-ffi-windows` (FFI DLL and optional helper),
then the normal Flutter Windows build. Keep the entire
`ui/build/windows/x64/runner/Release` bundle, including DLLs, helper and data.
The helper binary is built from `pb-mapper-desktop` with `--features windows-helper`;
it is not loaded as a privileged entry point into the Flutter process.

Build macOS with `make build-ui-macos-release` and iOS with
`make build-pb-mapper-ffi-ios`, then Xcode (`ui/ios/Runner.xcworkspace`);
CI's `macos-app` and `ios-app` jobs build both, the iOS one unsigned. For Linux,
`make build-ui-linux-release` builds the GStreamer module when `pkg-config`
finds `gstreamer-1.0`, `gstreamer-app-1.0`, `gstreamer-video-1.0` and
`gstreamer-allocators-1.0` (Debian and Ubuntu: `libgstreamer1.0-dev
libgstreamer-plugins-base1.0-dev`), and leaves it out otherwise, so that
build views over JPEG; the runner links libepoxy, which `libgtk-3-dev`
brings. `scripts/test/linux-desktop-video.sh [--software]` decodes recorded
clips through the module outside Flutter and converts every picture with
the runner's GL code in a GL ES context with no surface (it needs GStreamer,
ffmpeg and EGL with GL ES; Mesa's llvmpipe does without a GPU). It checks
that the texture is an RGBA8 `GL_TEXTURE_2D` of the picture's size, that it
matches GStreamer's converter on the same picture at 40 dB PSNR or more,
that both snapshots have content, that a software decoder's I420 needs no
`videoconvert`, and that the conversion and its still leave every GL
binding and switch as they were set before. It does the same with GL ES 2's
paths (LUMINANCE planes and padded rows, which Mesa, answering ES 2 with
ES 3.2, never takes by itself), forced on a converter of their own, which
must then delete its GL objects from a thread with no context. It checks
that a picture acquired and not shown earns no receipt and reaches Rust as
a decoder error at most once a second, and that a second decoder kept in
system memory, playing the same clip, converts the same frames to the same
picture at 40 dB or more: on a hardware run whose pictures are dma-bufs,
that is what checks their import. Then it prints, for the 720p and 1080p
clips (and 4K where a decoder takes it) offered at 60 and 120 fps, decoded
and presented pictures a second, the mean decode time and every thread's
CPU per decoded picture.
On llvmpipe the GL work runs on the CPU too, so CI's figures are no
measure of a GPU.

For Android, rebuild the FFI for the intended ABI before Flutter: ARM64 for a
matching phone, or x86_64 for the development VM. For example,
`cargo ndk -o ui/native/android -t x86_64 build -p pb-mapper-ffi --release`, then
`flutter build apk --release --split-per-abi --target-platform android-x64` from
`ui/`; a phone takes `-t arm64-v8a` and `android-arm64`. Build split per ABI, as
the release does: a split APK's versionCode is the build number plus 4000 on
x86_64 or 2000 on ARM64, as the product's is, while a universal build carries
the bare build number, which `adb install -r` refuses over the product as a
downgrade.
Local build artifacts do not constitute a published release. The
`capture_probe`, `video_bench` and `deployed` examples provide bounded development
probes; the deployed probe revokes its dedicated temporary credential on normal
completion and removes its controller setup file. Never publish that file.
`video_bench --pace 60,30,15,5` classifies the encoder's rate model (on a Mac,
the ignored `video_toolbox_rate_model_*` tests classify VideoToolbox's) and, like
`video_bench 60 60 --animate`, prints its GOP and keyframe count, and the
largest keyframe and access unit. `--codec h265` encodes H.265 and first
prints the host's probe; `--exact` encodes exactly `--max-width` ×
`--max-height`, upscaling a smaller display; `--noise` fills the display with
fresh noise, the worst case for a unit's size.

References: [issue #50](https://github.com/acking-you/pb-mapper/issues/50),
[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput),
[Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata),
[RustDesk elevation bootstrap](https://rustdesk.com/docs/en/client/windows/windows-portable-elevation/).

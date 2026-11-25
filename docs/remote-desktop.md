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
keeping the texture visible. A Windows host shares a rotated display (portrait,
or upside down) upright, as its desktop is laid out, where the GPU's video
processor can rotate; one that cannot refuses it, as every rotated display was
before. The pointer travels
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

With a mouse or a touchpad, buttons and the wheel go to the remote computer;
Ctrl with the wheel, or a touchpad pinch, zooms the picture here. A Windows
touchscreen uses the desktop viewer with direct touch.

A notch of the wheel here is one notch there. Each Flutter embedder reports
a notch as a distance of its own, which the viewer divides back out: on
Windows `lines × 100 / 3` physical pixels, cut to a whole number, where
`lines` is how many lines a notch scrolls (`SPI_GETWHEELSCROLLLINES`, which
the runner reads as it starts and answers as `wheel_lines` on
`pb_mapper/desktop_pointer`), so 100 / DPR logical pixels at the default 3;
40 logical pixels on macOS and 53 on Linux; elsewhere 30. Before, every
viewer took 30 px for a notch, so one notch of a Windows viewer at 100 %
reached the host as 3 or 4 notches, and those of Mac and Linux viewers in
1, 1, 2 and 1, 2, 2, 2 patterns. A touchpad's travel goes as it moves to a
host that lists `wheel_hires` (Windows and macOS after 0.12.1):
`Input::Scroll` in 1/120 of a notch, 1.2 units a logical pixel, so a notch is
100 px of travel, marked `continuous`; a wheel's notch goes there as 120
units, not continuous. Toward an older host the travel goes in notches of
30 px. Whatever falls short of a whole step waits for the next event of its
kind, a wheel's apart from a touchpad's travel and its fling, so neither
shortens the other's next step; an event the other way drops it first, and
so do a new picture generation and lost focus or control. A host that
injects in steps of its own (a Mac's lines and points, notches toward an
injector without fine scrolling) keeps what falls short of one the same
way, per axis, and drops it whenever it releases every held key and
button, as it does when control ends, the picture goes stale or the
desktop's generation moves on.

Flutter passes on no momentum of the system's (macOS drops its momentum
phase, Windows its DirectManipulation inertia, and GTK has none), so a
touchpad fling would stop on the host the moment the fingers lift. The
viewer carries it on itself: it tracks the pan's velocity as this platform's
scroll views do, and a lift faster than 50 px/s goes on, frame by frame, as
those scroll views decelerate: on a Mac or iPad by UIKit's 0.998 a
millisecond until it slows below 20 device pixels a second, and on Windows,
Linux and Android along Android's curve, which comes to a stop at the time
it computes for the fling's speed. A fling of 3 750 px/s travels about
1 870 px over 3 s on a Mac and about 1 930 px over 1.2 s elsewhere (the
viewer's tests, on a fake clock). Fingers back on the touchpad, a new pan, a
click, a wheel, focus or control lost and a new picture generation stop it,
and a pinch flings nothing.

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

Each answer of the worker, with the unit or the shape it carries, and each
record it pushes between its answers (protocol 12, below), crosses two
pipes: the worker's private pipe to the service, and the helper pipe on to
the app. Both carry `u32 BE length | bytes` records (`oop/ipc.rs`): a
request, an answer or a push as JSON of at most 8 KiB, and after each
answer and each pushed unit a byte record of up to 4 MiB. The worker writes
an answer, or a pushed unit, and its byte record in one vectored write, and
the app each request in one. mio, from 1.2.5, makes
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
64 KiB, so each of its writes stays under that 1 MiB. That change left the
records, and so the helper protocol, as they were.
`examples/pipe_relay_bench.rs` times a
unit through two pipes and such a relay, pushed as protocol 12 sends it
(from the worker's write until the app holds it) or, with `--mode answer`,
answered as protocol 11 did (from the request), and counts the writes and
reads it took; its 3.7 MB unit is the one whose write allocates on 1.2.5.
No Windows run of it is recorded yet.

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

Protocol 13 uses the pipes `pb-mapper.desktop-helper.v13` and
`pb-mapper.desktop-background.v13`. It adds `keep_awake` to the background
host settings and a `set_keep_awake` management operation. While background
sharing is enabled, the service can hold a system power request between
viewers. Stop and revoke release it. Older grants default to off.

Starting with protocol 12, the worker pushes between its answers what the app
would otherwise have to ask for again and again, so the app's thread C never polls the worker.
Every record the worker writes names its `kind` (`Record` in
`oop/proto.rs`): `reply`, an answer, its response's fields beside the tag;
`unit`, a unit its encoder finished, with the desktop generation its
capture was checked against and the units the encoder still holds, its
bytes the next record; `failed`, a failure its encoder met as the worker
collected its output, which thread C takes in the units' order as a drain's
failure; `pending`, a lower pending count when the encoder discarded a late
output with nothing to send; `fresh`, that its source wants the tick the app's last capture
left armed: it published a picture, dated by its age where it can be, or a
repeat came due (liveness, a keyframe), or its encoder asked for input;
and `pointer`, the pointer on the desktop it last reported, with the newest
shape's number once the app asked for shapes. `Request::Stream` says
whether thread C wants video: while it does, the worker pushes `fresh`, at
most once per armed tick, and `pointer`, and holds fine timers; when every
viewer pauses it stops, and its capture thread stops looking. Units are
pushed either way. `Request::Video` always captures: `submit` and `wait_ms`
are gone, and with them the drain's long poll. Its answer dates the picture
it submitted (`presented_ago_us`, `presented_count`), which the app dates
by its own receipt of the answer, a little later than the present by the
answer's way through the pipes, and its pacing anchors on it. A `Video`
request no longer reads the pointer: it checks the desktop twice, around
the capture, where it checked five times. Its `Request::Input` may also
carry `Input::Scroll`, scrolling in 1/120 of a notch (see
[Input and cursor](#input-and-cursor)), which the worker injects as
`SendInput` wheel data, and the `Start` answer's caps say whether the worker
scrolls that finely (`hires_wheel`), which the host then lists to its viewers
as `wheel_hires`; both ends refuse a scroll beyond 12 000 units each way.

The worker's loop (`server::work`) waits on one wake that the exchange,
its encoder's event watcher and its capture thread share: it serves the
request that came first, then pushes each unit its encoder finished, a
`fresh` when its source wants the armed tick (`Backend::capture_idle_until`
of its own backend says so), and the pointer every 16 ms (once a period at
a faster pace, down to 8 ms) and 2 ms and 8 ms after an input moved it,
when the pointer or its shape's number differs from what the app holds,
as its last push or an answer that carried the pointer left it: an
input's answer reports the move it applied, so a move the system then
stopped short is pushed where the pointer stopped. On the app's side one
I/O thread owns the pipe and the runtime that drives it: it writes each
call's request, and reads every record in order, handing an answer to the
waiting call, queueing a unit or a failure for thread C, which takes them
with no pipe call and is woken for each, raising a flag for `fresh`, and
keeping the pointer, so an answer read late never undoes a push that came
after it. A pushed pointer that changed what thread C would read wakes it
too (`Backend::cursor_changed`), and thread C samples it at once rather
than at its own next sample, so a pointer the viewer did not move (the
host user's, an app's) waits for the worker's sampling and the pipes, not
for thread C's sampling as well. Both ends check what they read: a unit
within 4 MiB, and of the generation thread C expects when it takes it
(another's is dropped); at most 64 units pending, and at most six pushed
and not yet taken, two more than thread C lets in flight; a pointer inside
the display the worker last reported (one outside is not taken); an age within 1 s (an
older one is no date); and no unknown kind or field. A broken record ends
the session, as a lost pipe does. Each pushed unit also carries when the
worker collected it, on the system clock both processes read, from which
the app's once-a-second debug event `desktop_helper_push` (target
`pb_mapper_desktop::feedback`) reports the delay to its receipt (p50, p95,
maximum), with the answers, which are the round trips, units, fresh
pictures and pointers it received per second, and each fresh picture's age
as the worker wrote its notice. How much sooner a unit, a picture and an
input arrive on a Windows computer, and what it saves of the app's, the
service's and the worker's processor time, is still to be measured there.

Protocol 11 added `Request::Adapter`, which the host sends only for a
viewer that decodes H.265, just before it would ask `Request::H265`: the
worker answers with the
GPU that drives its display and that GPU's driver (`adapter`: the adapter's
vendor, device, subsystem and revision ids from DXGI, and the user-mode
driver's version). The answer to `Request::H265` names the GPU in `adapter`
too, but only when that answer is the worker's probe of it; the host keeps
the answer under that GPU for later workers (see
[H.265 and H.264](#h265-and-h264)). The worker sends a key only with a vendor
and a driver version, and the host refuses an answer whose key lacks either;
a worker that cannot name its GPU leaves `adapter` out. `Start` is
unchanged, so a session whose viewer does not decode H.265 costs the worker
no DXGI query for it.

Protocol 10 added `submitted` to the answer of a `Video` request that
captures: `false` when the worker's source had no new picture to submit, so
the host's thread C keeps its tick armed and asks again (see
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
  it on a full encoder; one that had no new picture, or whose encoder has
  not asked for input yet, leaves the tick armed, and thread C asks a
  polled source again every `capture_poll()` (by default `drain_poll()`)
  while the desktop is active or the encoder still holds units, and a
  period later otherwise. `submitted_picture()` says when the display presented the
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
  connect, decides whether a shared LAN is tried first. A connection
  decides its path once and keeps it; a session moves to another path only
  by a standby on it taking its place ([Standby sessions](#standby-sessions)).
  A fallback that says the path cannot be punched (the punch after a grant
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
  presenter asks for the highest mode while the viewer shows). A fractional
  rate rounds to whole frames, so a 59.94 or 119.88 Hz display asks 60 or
  120. Afterwards `set_display_refresh(hz)` follows the display the viewer's
  window is on, within `limits.max_fps`, with the existing `FpsCap` (see the
  fps cap below).
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
  Send buffers and the relay's sockets are unchanged.
- Congestion control is the sender's own and never crosses the wire. The
  host's end of a direct QUIC session sends its video under
  `VIDEO_CONGESTION` in `host.rs`, which it passes to the hidden
  `Client::register_with_sender_congestion`; the viewer's end, relayed
  sessions and LAN TCP keep their own controllers. It is Quinn's Cubic.
  BBR was measured against it on loopback
  ([host direct congestion control](benchmarks/2026-11-24-host-direct-bbr/README.md)):
  on paths with 0.1–1% random loss and 60 or 120 ms round trips the receipts
  held its rate at 4.8 to 8.2 Mbit/s against Cubic's 0.7 to 4.8, whose cut
  on every loss capped them, and in all but one run it shortened the wait
  for a requested keyframe; but behind a 5 Mbit/s
  policer it lost more packets and kept a requested keyframe waiting longer
  (a median 157.8 ms against 111.5), even with the receipts setting the
  rate: its window grew to megabytes, so Quinn's pacing, which follows the
  window, no longer spaced a burst. Each end's `DirectLink::congestion`, and
  the `congestion` field of `session_path_selected` (`os` over TCP), say
  which controller it sends under.
- Input binds to a desktop/display generation and to a presentation the viewer
  validated less than two seconds ago. `Viewer::input(generation, &inputs)`
  sends a batch of up to 16, stamps the presented frame and consecutive
  sequence numbers itself, and fails with a typed `InputError` (`NotFresh`,
  `NotControlling`, `Congestion`, `Closed`, `Invalid`, and `NotOffered` for
  an `Input::PhysicalKey` toward a host that did not list `physical_keys` or
  an `Input::Scroll` toward one that did not list `wheel_hires`, nothing of
  the batch sent). A host's backend says it injects `Input::Scroll` with
  `BackendCaps::hires_wheel`. `Input` and `Key` are `#[non_exhaustive]`. The
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

### Standby sessions

A session's connection keeps its path for its life, and a path can die while
the other lives: UDP blocked or throttled mid-session, a NAT on the host's
side that rebinds (QUIC moves no server), a lost port mapping. Without a
standby such a session ends at the viewer's 4 s read timeout, and the host
ends its side at 3 s, which empties a single viewer's room: capture pauses
and a changed display mode is put back before the app reconnects. And a
session that took the relay because direct fell back for a moment stays on
the relay. A standby is a second, ordinary session of the same viewer to
the same host, on the other path, that waits to take the first one's place.

- **Wire.** Both ends list `standby`. A host gives a session whose viewer
  listed it a ticket, `Accepted::ticket`: 32 bytes from ring's system
  generator, base64url, which names the pair. A standby authenticates as
  any viewer, with `Authenticate::standby_for` set to it. The host checks
  the token first, in constant time, then the ticket, in constant time over
  SHA-256 digests against every pair's; a ticket no pair holds is denied, and
  so is the right ticket beside a wrong token. Each field is written only to
  a peer that listed the capability and left out when unset; the ALPN stays
  `pb-mapper/desktop/2`. A standby's own `Accepted` carries no ticket.
- **Host.** A standby takes a standby's admission permit (`MAX_VIEWERS` of
  them), never a slot's, and a pair holds one (a second is `busy`). It runs
  paused, on its pair's codec, and thread C knows nothing of it: it causes no
  keyframe, shows in no `HostStatus` and has no peer pointer; it hears
  `State`, `Stats` and probes, and answers them. Its `Resume` promotes it:
  under the room's lock it takes the pair's slot, with the slot's admission
  permit and the ticket, and the session it replaces ends without a word
  (`End::Closed`). Thread C releases the replaced session's held input,
  failing closed, and hands the newcomer the control it had, and with it the
  admission lease, which is never let go meanwhile. The room never empties,
  so a changed display mode stays as it is until the pair's last session
  ends, which puts it back once. The resume IDR is forced; a `Path` resets
  the QoS baseline only when the route changed, as the standby measured its
  own all along. A pair's session whose connection ends (closed, failed or
  silent) while its standby stands by hands it the place, still paused,
  costing no keyframe; one the host ends for a reason (`Stopped`) ends its
  standby with it.
- **Viewer.** `Viewer::connect` with `PathPolicy::Auto`, to a host that gave
  a ticket, opens a standby once the session streams. Beside a session that
  runs direct across the internet (QUIC, punched or through a port mapping,
  or between private addresses off the LAN, as over a VPN; never one within
  a LAN) it opens one on the relay. Beside a session the
  relay carries after direct fell back for a while (its negotiation timed
  out or failed, or a grant found no path, or a pause left direct out), or
  after its direct connection died and a relay standby took its place, it
  opens a direct-only one through the hidden `Client::connect_tcp_direct`,
  which never takes the relay: one consumer and endpoint across attempts,
  under the client's pauses for the computer, never again after the relay
  or the host said direct is not offered, and counting no fallback. A relay
  standby that fails is opened again after 1 s, doubling to 30 s; a direct
  one at the end of the pause its failure set, or after 10 s doubling to
  5 min, or at once on a network change; each doubling wait is jittered
  down to as little as three quarters of it. A ticket the host denied, or a
  standby the host ended itself, ends the viewer's standbys.
- **Taking the place.** The live connection gives way when it fails or
  closes, or when it has read no byte for max(1.5 s, 4 × base RTT) while the
  standby read a whole message in that time; a direct standby takes the
  place as soon as it commits. The viewer drops what it had queued for the
  old host session (input, receipts, presentations, a mode change), writes
  `Resume` first on the standby, then the choices the new session has yet to
  hear: control (asked for again if the app wants it, revoked if not), a
  pause, the preset, the fps cap. Its status, view and video sink stay what
  they are: the sink drops what it held and hands out next an IDR marked as
  a discontinuity, and no presentation of the old session admits input. The
  old connection stays open, reading and dropping what still comes, until
  its host session ends, at most 3 s, so the host hears the `Resume` before
  the close. `Viewer::metrics` follows the live connection: a
  `subscribe_metrics` stream taken before a switch ends with it. The trace
  events `desktop_viewer_promoted` (cause, route, outage_ms) and
  `desktop_viewer_standby` (route, outcome) count how often either happens.
- **Costs.** One more session per viewer while a standby stands by: one more
  relay stream for every viewer whose session runs direct across the
  internet, counted against the namespace's stream budget like any other,
  carrying its stats once a second, its probes and their echoes, and pings
  (6.8 kbit/s both ways over loopback, where probes run every 250 ms; see
  below). One IDR per switch.
- **What it does not cover.** A change of the viewer's own network
  (Wi-Fi to cellular, a VPN coming up) kills both connections: the app
  reconnects as before. RelayOnly viewers and hosts that do not list
  `standby` are unchanged. No FFI, Dart or helper-pipe change:
  `HostStatus` keeps its shape.

Measured in process ([standby session measurements](benchmarks/2026-11-24-standby-session/README.md)):
with the live path holding every byte while the relay lives, the promoted
session's first picture came 1568 ms after the block (p50 of 10, max 1585)
and input was taken at 1572 ms (max 1591), where without a standby the
session ended at the read timeout, 3991 ms after it (p50, at the base
commit), before any reconnect; a relayed session moved to direct with
exactly one IDR and no gap between pictures over 34 ms. Real NATs and
firewalls are still to be run.

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
  session negotiated `cursor_shape`, at thread C's pointer samples and the
  shape looks after a controller's move (see
  [Input and cursor](#input-and-cursor); the helper's worker likewise), and
  only while the pointer shows on the shared display. It reads a bitmap only when the OS says the pointer's image
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
| `Video` | the DXGI → D3D11 video processor → Media Foundation encoder, one fused object (`platform/windows/video/`) that pushes: a capture thread that thread C arms when its tick finds nothing new waits in `AcquireNextFrame` for the next present, converts it at once and wakes thread C (`capture_poll()` is `None`), and a watcher thread waits for the MFT's events and wakes thread C for each output and for an input request it waits for (`drain_poll()` is `None`; an MFT without `IMFShutdown` is drained every 2 ms instead) | ScreenCaptureKit (NV12, scaled by the system to the encoder's size, no cursor, queue depth 6, its minimum interval between pictures from the display's refresh and the pace) → `VTCompressionSession`, zero copy, one fused object that pushes (`DRAIN = None`): the output callback queues the unit and wakes thread C |
| `Input` | enigo; `SendInput` with `KEYEVENTF_SCANCODE` for physical keys (a HID usage to its set-1 scancode); absolute moves as one `SendInput` event each (see [Pointer moves on Windows](#pointer-moves-on-windows)); `Input::Scroll` as one `SendInput` of `MOUSEEVENTF_WHEEL` (the vertical units, negated) and `MOUSEEVENTF_HWHEEL` (the horizontal) for the axes that move, its units Windows' own (`WHEEL_DELTA` 120 a notch); `LockWorkStation` | `CGEvent` at the HID tap: pointer and wheel in the bound display's points, `Input::Scroll` that is continuous as a pixel scroll of a point per 1.2 units marked `kCGScrollWheelEventIsContinuous`, and otherwise as lines, a line per 120 units, what falls short of a point or a line kept until a release, click counts for double clicks, named letters and digits at the key the Mac's layout types them with, other keys and physical keys by `kVK_*` place, modifiers as flag changes with left and right apart, text as Unicode string events of at most 20 UTF-16 units; no lock (`LOCK = false`) |
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
   host's VideoToolbox); an encoder that keeps filling to four caps the pace
   near what it delivers. A picture the
   backend dates places the next tick a period after its present, up to a
   quarter period early and at most 1/64 period earlier than a period after
   the last tick, and only a picture of a single present moves it earlier:
   the long-run rate stays within 64/63 of the pace, and a display at least
   twice as fast as the pace is taken at the pace. An undated picture, a
   repeat or a skip keeps an even grid. After a stall two captures are never
   closer than three quarters of a period.
3. **QoS changes frame rate and bitrate only, from receipts and probe delay.**
   Transport receipts cut bits per frame within a round trip and lower the
   frame rate only below the preset's cut floor; a probe once a second, four a
   second on a short path, answered by the viewer's network task, remains for
   severe or unexplained delay. Resolution never adapts: it changes only with
   a new session, a display switch or an explicit choice.
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
165 Hz display is not rounded down). A Windows host dates a picture by its
present, in the app's process and behind the helper, whose answer gives
the picture's age (protocol 12), and a Mac host by its arrival from
ScreenCaptureKit. A tick that finds no new picture stays armed, so the next
picture is taken as it appears rather than a tick later. ScreenCaptureKit,
which pushes, wakes the thread for it, and so does the Windows encoder's
capture thread, which such a tick arms and which waits for the display's
next present (see the Windows host below): in the app's process directly,
and behind the helper through the worker, which tells the app once its
source wants the armed tick, whose I/O thread wakes thread C. Thread C
never polls DXGI, nor the worker. A source that cannot push (none ships;
the synthetic test platform has one) is asked again, never at once: every
2 ms (`DRAIN_POLL`, or the backend's `capture_poll`) for 250 ms
(`HOT_WINDOW`) after an input the thread injected or a new picture, when
the next present is likely, and while the encoder still holds units, which
may be what kept the tick, and once a period otherwise, so a still desktop
costs no more capture calls than the grid. An injected input also brings a
poll due a period later in to 2 ms. `desktop_capture_pacing`'s
`empty_polls_per_s` and `video_bench`'s `polls_per_s` count the captures
that left the tick armed, together with the asks of a Windows encoder that
had not asked for input yet (`video_bench`'s `waiting` counts those apart)
and a Windows encoder's refinement repeats: the captures that armed the
capture thread or sent a refinement repeat, at most one a tick on an active
desktop, and on a still desktop an arming after each 250 ms repeat besides
a burst's repeats. Behind the helper each of them is a round trip through
the pipes: on a desktop that keeps changing, a tick that finds nothing new
arms the worker's source, and a capture takes the picture the worker then
tells of; an encoder that has not asked for input yet costs one more each
time the worker's source names its retry (2 ms later) or its input request
comes. How many round trips a second that makes on a Windows computer is
still to be measured (`desktop_helper_push`'s `replies_per_s`). A change
of the viewers' limits that changes only the frame rate keeps the capture source
where it can serve the new limits as it is (a Mac host's can while the
encode size stays the same, and the
synthetic test platform's; the Windows source is still built again), and
asks for an IDR either way. Thread C passes each
change of limits to the backend as it happens, during an outage too, so a
recovery, which repeats the current limits, always builds a fresh source.
A capture that finds
the encoder without room (four units, or no free sample) spends its tick.
One whose encoder has room but has not asked for input yet keeps it armed
(`Submit::Waiting`, which captures nothing): some Windows MFTs ask for the
next input only once thread C has collected the last output, and spending
the tick there would leave the next picture, of an encode longer than a
period, for the tick after. A tick is skipped (and
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
queueing encoder's dwell grows with its depth.

An encoder slower than the pace that still takes input, as a Media
Foundation encoder serial at its bottleneck can, fills to the four anyway:
each unit waits behind the three before it, and the ticks it skips space the
frames unevenly. Thread C therefore counts, each second, the due ticks it
skips for four units in the encoder (or the helper), its full ticks, apart
from transport skips, with the units the encoder returned and their median
dwell (`worker/throughput.rs`). A second is full when it had a full tick and
the encoder held three units or more on average over it (the units a second
times their median dwell, by Little's law): however close to the pace the
encoder is, it then skips a tick now and then, while a heavy picture skips
ticks with most units waiting behind none, and a stall returns too few units
to hold three. A full second caps the pace at 0.95 × the units that second,
no lower than the highest minimum frame rate of the viewers' presets, and
only below what the encoder returned, so that its queue drains. A guard
then judges the cap
by the first second, 2 s or more after it, that loads it (returns 0.9 × the
cap or more), later while the three queued units drain at the units a second
the cap leaves spare (4 s for an encoder of 20 a second, at most 8 s):
unless
its median dwell fell below 0.6 × that of the second that set the cap,
latency rather than throughput filled the encoder (a pipelined encoder holds
as many units at any pace near its throughput), and the cap lifts, with none
set again for 30 s, a hold that doubles with each lift in a row up to 8 min.

The cap then holds what it measured. After three loaded seconds since the
last full one, a median dwell that shows the encoder serving a step (a
twentieth, at least 1) more than the cap, 0.95 ÷ the dwell, raises the cap
to that, since a unit's dwell is at least its service time; it stops short
of the cap the encoder last filled at, unless it shows a step more than that
too. Only 30 s after a measurement does a probe raise the cap by a step each
such second, since the dwell may understate what a pipelined encoder serves.
A full second while capped, at a probe past the encoder or with heavier
pictures, lowers the cap to 0.95 × what the encoder returned, and the next
probe waits twice as long, up to 4 min. Seconds that do not load the cap,
as on a still desktop, and a pace asked at or below it, as in a viewer's
startup hold, leave it as it is. It clears once it reaches the pace the
viewers ask for, and a rebuilt encoder (new limits, desktop or codec, or a
recovery) starts over. Thread C paces within the cap at once and publishes
it to each viewer's session, whose QoS takes it into its pace, as it takes
the viewer's cap, at its next update (within a 250 ms tick): the expected
bitrate, a receipt cut, a timestamp encoder's bitrate and `Stats`'
`pace_fps` follow the frames actually made, while thread C is still asked
for the pace without the cap, which tells it when the cap no longer holds
anything back. Nothing new crosses the wire or the helper pipe. A Mac host's
gate keeps its count below four, so it never caps there; NVIDIA's HEVC
encoder took a mean 3.1 ms a 4K unit
([Encoder rate model per GPU](#encoder-rate-model-per-gpu)), inside a period
at 120. Whether an AMD or Intel encoder at 4K or 1440p and 120 FPS fills
up is unmeasured; `desktop_encoder_cap` (see
[Diagnostics](#diagnostics)) shows it where it does. On this Mac, a
synthetic encoder serving a unit each 11 ms at a pace of 120 over an 18 ms
path went from a dwell of 39 or 40 ms in every second and 29 skipped ticks a
second to 11 or 12 ms at a cap of 85 with no tick skipped, in every second
but the two or three of the first probe, 30 s after the cap, which filled
the encoder again for one second at 38 ms and 5 or 6 skipped ticks; it
received 85.7 to 85.8 FPS instead of 90.9
([measurements](benchmarks/2026-11-24-encoder-throughput-cap/README.md)).

A Mac host holds VideoToolbox
to two units in flight (`VT_IN_FLIGHT`), finished units not counted, and
thread C skips a tick that finds two as one that finds four: where
it encodes asynchronously (pictures whose buffers do not cover a multiple of
16, as a MacBook's built-in display at its default resolution may give,
which a Mac host now codes padded to one unless padding is off: see below)
and the pace outruns
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
duplicate, which unique-frame rates exclude; a Windows encoder that refines
(below) first repeats a picture that just stopped changing a few times at
the capture pace. A Mac host does not leave a
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
The encoder is asynchronous: it asks for each input
(`METransformNeedInput`) and announces each output
(`METransformHaveOutput`) as an event. A watcher thread, started once the
encoder streams, is the only reader of those events: it waits for each,
counts it, and wakes thread C for each output, which thread C then
collects, and for an input request a waiting tick waits for, which thread
C then answers with a capture, so neither waits for a poll and thread C
no longer wakes every 2 ms while a unit is in the encoder. While it waits
for a request, thread C still asks again every 2 ms; a request that came
while thread C was still finishing the capture that found none is
answered at once all the same. A drain collects at most one output, and
none after the encoder changed its output type or for an output it
discards; while an output so announced is still uncollected, thread C
drains again every 2 ms, since no further event will wake it for that
one. The watcher takes a
failed event, or more requests or outputs than the encoder may have, for
the encoder's failure, which thread C's next drain returns. Dropping the
encoder shuts it down (`IMFShutdown`), which ends the watcher's wait, and
joins it, at most 100 ms, before the encoder's interfaces and Media
Foundation go. A watcher whose wait the shutdown did not end is left
behind, with the encoder it holds, and every later encoder of the process
gets no watcher, so that at most one is left behind however often the
encoder is built again. An encoder without `IMFShutdown`, or without a
watcher, has its events read at each drain, every 2 ms while it holds
units. Behind
the helper, the worker's encoder has its watcher and its capture thread
too, and they wake the worker's loop (protocol 12): it collects each output
as it is announced and pushes it to the app, whose thread C its I/O thread
wakes, and a capture request that finds nothing new arms the worker's
capture thread, which converts the next present as it comes; the worker
then tells the app, which asks for it. The capture thread stops looking
when every viewer pauses (`Request::Stream`), and once no request has
armed it for 2 s.
An encoder that holds nothing and
has asked for no input for 2 s after it last took or gave a unit is
stalled, as one that holds a unit for 2 s is.
DXGI pushes through a capture thread of the encoder's own
(`capture_thread.rs`, on the neutral `codec/present_waiter.rs`). Thread C
builds the duplication, the video processor and the encoder, then hands the
duplication and the video processor to that thread, whose first call puts
it on thread C's desktop (`SetThreadDesktop` with thread C's
`GetThreadDesktop`: the helper worker's bound input desktop, Winlogon's
included; in the app's process the one it already has), before any COM or
D3D call. When thread C's tick finds no new picture, it hands the thread a
free NV12 sample from the encoder's pool (with none free it takes nothing,
`Full`) and arms it. Armed, the thread first takes a frame DXGI already
holds, then waits in `AcquireNextFrame`, at most 16 ms at a time. A frame
with a new present is converted at once, straight from the duplication's
surface into that sample by the D3D11 video processor, which the GPU is
handed (`Flush`) before the frame goes back to DXGI; the thread then
publishes the picture, disarms itself and wakes thread C, whose tick,
already due, submits it. A frame that only moved the pointer is released and
waited past, and wakes nothing. Unarmed (once it published, until thread
C's next tick finds nothing new; after a repeat; while every viewer has
paused; or once thread C has not armed it again for 2 s, as when its
caller went without saying so), the thread waits for thread C, and DXGI
folds the desktop's updates into its next frame, which the next arming
takes at once. A picture that waited 2 ms or more before a submit takes
it, as one does while thread C skips ticks for backpressure, across a
pause, or after a repeat went first, may have a newer present behind it,
which the thread, disarmed as it published, has not looked for: it is held
back, and the thread is armed to look past it. A newer picture then goes
in its place, carrying the held one's presents, and otherwise the held one
goes once the look found nothing newer, or 32 ms after the arming without
that answer. Each frame is released as
soon as it is converted, so no update is missed between ticks, and no
full-size BGRA copy of the desktop is made (an HDR desktop's tone map,
below, draws one). Both threads hold the device's
own lock (`ID3D11Multithread`) across a picture's work on its one immediate
context. A video processor that refuses the duplication's surface (it fails
to make a view of it or to convert from it) has each picture copied out of
it first for the rest of that encoder, as every picture was before, and
logs `desktop_capture_copied` once. The last submitted sample is kept, so a
repeat on a still desktop (the 250 ms liveness repeat, a refinement repeat,
or a keyframe) copies its NV12 picture into a fresh sample instead of
converting again;
the encoder never writes the samples it takes. A repeat goes only once the
armed thread has looked and found nothing newer, which it wakes thread C to
say when thread C waits for it, or 32 ms after its arming (or after it began
converting a present it found) without that answer, so that a keyframe, or
the first submit after a pause, takes a present DXGI already holds rather
than repeating an older picture, as the polled capture did. A present the
thread finds as thread C disarms it is still converted and published, and
looked past at the next tick. The pool holds six, and a sample is taken from
it only while the encoder has room for one more unit: the three it then
holds at most, the last one once it is out, a picture held back or a
repeat's copy (never both), and the one the capture thread holds for its
next picture. A failure of the capture thread (DXGI's access lost on a
desktop switch, say) is published, wakes thread C, and fails its next
submit, which builds the capture again. Dropping the encoder stops and
joins the capture thread, which ends within one 16 ms wait, before the
encoder, its samples and Media Foundation go; one that has not ended within
100 ms is left behind and logged (`desktop_capture_thread_stuck`). Behind
the helper, a desktop switch drops the encoder, and so joins its capture
thread, before the worker binds the new desktop. A new image is dated by
`LastPresentTime`, a performance counter reading turned into an `Instant`
by its age against `QueryPerformanceCounter` (one over a second old dates
nothing), and carries the presents DXGI folded into it
(`AccumulatedFrames`); a fresh picture's sample time and duration follow its
present, kept strictly increasing, and a repeat's follow the submit. The
first image of a new duplication is converted whatever `LastPresentTime`
says, and left undated, so a still desktop shows at once. Hosting requires Media
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
coordinates still use the physical `Display` geometry. A rotated display
(`DXGI_OUTDUPL_DESC::Rotation` 90, 180 or 270; unspecified counts as none)
is captured as scanned out: its display mode is read upright, the size of
its output's desktop rectangle, and its pictures as that size turned back,
as WebRTC's Desktop Duplication capturer requires (a mode given the other
way round is told apart by that rectangle, `scale::Rotation::scanned_out`).
The video processor turns each picture clockwise by the same angle
(`VideoProcessorSetStreamRotation`, as Microsoft's Desktop Duplication
sample maps the rotation), so the video has the upright size the `Display`
already describes and its input mapping is unchanged. The video processor
is set up before the encoder, so a GPU whose video processor lacks the
rotation cap (`D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION`) fails that
capture as before, without opening an encoder each time it is retried; a
first frame whose texture is not the size expected fails it with both sizes
named, and `desktop_video_encoder_opened` logs the `mode` and the
`captured` size. No device has shown each angle upright yet. A desktop one
pixel over the even encoded size in either dimension, such as 1365×767
under 1080p bounds, is cropped rather than scaled by a hair under 1, which
would blur every pixel: the video processor copies it 1:1 and leaves out the
upright picture's right column or bottom row (`scale::crop_only`). Input and
the pointer still map the video across the whole display, so in the half of
the picture nearest that edge a click lands one display pixel further
towards it: the viewer cannot tell that crop from a scaling without a new
field on the wire, and a host that mapped clicks back could no longer reach
the display's last column or row. The ignored test
`the_video_processor_turns_crops_and_scales_as_the_geometry_says` (in
`converter.rs`; `cargo test -p pb-mapper-desktop --all-features --lib
converter -- --ignored` on a Windows machine with a GPU) checks the turn,
the crop and a downscale on each hardware adapter's video processor with a
synthetic picture, without a rotated display. The video processor is asked for
speed (`D3D11_VIDEO_USAGE_OPTIMAL_SPEED`); quality goes on, for a downscaled
picture only, per adapter vendor where a device run shows sharper text for
under 1 ms of processor time, and none has yet. `video_bench --vp-usage
quality` or `speed` asks for one at every size (`EncoderOptions::vp_usage`). H.264 is asked for
in a High profile with CABAC, with Baseline as the fallback, and its SPS
goes out saying that no picture is reordered (see
[H.265 and H.264](#h265-and-h264)). The encoder takes
`CODECAPI_AVEncCommonQualityVsSpeed` 0 and no `CODECAPI_AVScenarioInfo`;
`video_bench --qvs` and `--scenario display-remoting` set them
(`EncoderOptions`), and a default changes, per adapter vendor and size,
only where a device sweep earns it
([Encoder rate model per GPU](#encoder-rate-model-per-gpu)). An encoder
built with `EncoderOptions::gpu_priority` (`video_bench --gpu-priority`;
off in a session until a device run under a GPU-bound load shows a gain)
raises its capture device's GPU thread priority to 7 and holds the
process's GPU scheduling class at `HIGH`, never `REALTIME`, which can
starve the compositor: the first such encoder of the process reads the
class and raises it, the last one puts it back unless something else
changed it meanwhile (`platform/windows/gpu_priority.rs`). A positive
device priority needs the privilege to raise a base priority
(`SeIncreaseBasePriorityPrivilege`) enabled, which the guard does for its
calls where the process's token holds it, as an elevated process's does,
and undoes after them. An unelevated app's token does not hold it, and
neither does the helper's capture worker's, which `helper/child.rs`
restricts to no privileges: there both calls go without it. Whether that
worker should keep this one privilege is a separate decision, for after
the device run. Each call may fail harmlessly, and what each did is logged
once per process as `desktop_gpu_priority`.

A large change at once, a scroll or a window switch, is coded only as well
as one frame's share of the bitrate allows by an encoder that budgets each
frame (NVIDIA's MFTs, measured `nominal` in
[Encoder rate model per GPU](#encoder-rate-model-per-gpu)). Each repeat of
the unchanged picture is expected to code up to one more frame's share of
what that left out, so at the liveness repeat's 4 Hz text sent blurred
would sharpen slowly; whether a repeat sharpens it on a given encoder, and
how fast, is what the still-refinement device run measures. The Windows
encoder can therefore refine a still picture with a short burst of repeats
(`codec/refine.rs`, its schedule; `Encoder` in `video/mod.rs`). A picture
is due a burst once a fresh unit (a new picture's or a keyframe's) has
filled at least three quarters of a frame's budget (the bitrate over the
nominal frame rate) since the picture last finished refining. 200 ms after
the last new picture or keyframe (`REFINE_AFTER`, under the 250 ms
liveness interval), the last picture is submitted again a period of the
pace apart, each repeat a duplicate copied from the kept sample. A typed
character or a caret blink codes far under its budget and never starts a
burst on its own; it can only resume one that the change before it left
unfinished, within that change's 15 repeats. A refinement repeat is a
sample that leaves thread C's tick, and the capture thread, armed
(`Submit::Refined`, which the backend reports as a capture that left the
tick armed, and the helper's answer as `submitted: false`, as protocol 10
already says), and asks the capture thread again as an arming does, so it
never stops looking during a burst. A present that lands during a burst
is therefore taken at once rather than a tick later; inside the encoder it
can still wait behind the one repeat being coded. The burst passes the
encoder-depth and send-credit gates as any capture does, and a pace whose
period is 250 ms or longer runs none. Its repeats count among the frames
and bits the rate-model latch reads, as liveness repeats do, so only an
encoder that spends more than a frame's budget on them pushes a session
towards the latch. It ends at the first of: two outputs in a row under
512 bytes (`REFINE_DONE_BYTES`), or two that did not lower the QP the
encoder reports (`MFSampleExtension_VideoEncodeQP`, read only from
refinement repeats' units), once it has had three outputs (`REFINE_MIN`:
right after an IDR or a large unit a rate control can starve the first
repeats, small and at a QP that does not move, before the picture
converged); a reported QP of 18 or less (`REFINE_QP_FLOOR`); 15 repeats of
the picture (`REFINE_MAX`), counted from the new picture's unit that filled
its budget, however often new pictures interrupt its bursts or keyframes
of it (the IDRs viewers ask for on a still desktop) start them again; or
1 s of them (`REFINE_MAX_TIME`). A new picture or a keyframe ends a burst,
and outputs older than its first repeat say nothing of it; a picture a new
one interrupted refines again once it stands still, with what is left of
its 15 repeats, and so does one whose keyframe filled its budget. Then the
250 ms liveness repeat goes on as before. A liveness repeat ends the
picture's claim to a burst: at a pace too slow for one the liveness
repeats refine it instead, and a pace that rises later starts no burst on
the picture that stood still meanwhile. The cap, the QP floor and the
200 ms are starting values that the device runs set. In a session an
encoder refines only where `REFINE_ENCODERS` lists its adapter vendor and
its MFT's name, once a device run showed that its repeats of a still
picture raise PSNR and fall under 512 bytes once it converged; none is
listed yet, so a session keeps the liveness repeat alone. An encoder that
pads its constant bitrate, or budgets by the sample times, stays off:
thread C's rate-model latch never reaches the encoder.
`desktop_video_encoder_opened` logs `refine`. `EncoderOptions::refinement`
(`video_bench --refine on` or `off`) forces either, and `video_bench --text
--freeze <ms>` scrolls a page of text and freezes it to compare them; the
[still-refinement record](benchmarks/2026-11-24-still-refinement/README.md)
lists the device runs, and records why a Mac host has no burst.

With Windows HDR (or Auto Color Management) on, Windows composes the desktop
in FP16, linear scRGB in which 1.0 is 80 nits, with SDR content at the
display's SDR white level, the SDR content brightness setting. A
duplication that takes only BGRA8 gets DXGI's own conversion of that
desktop, which is expected to leave the level out, so that SDR content
would arrive brighter or darker than the display shows it; that conversion
varies by driver, and no run here has recorded it yet. The duplication now
lists FP16 first (`DuplicateOutput1` with `R16G16B16A16_FLOAT`, then
`B8G8R8A8_UNORM`), which DXGI hands out only for a desktop composed in
FP16: an SDR desktop still arrives in BGRA8, and its pictures take the
same path as before, with nothing more read or drawn. Where the
duplication's mode is FP16, thread C reads the display's SDR white level
as it builds the encoder (`QueryDisplayConfig`, the active path whose
source has the output's GDI device name, then
`DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL` for its target), so a
change of the setting reaches the next encoder. Where Windows tells no
level, as it may not to a process outside the console session, the
duplication is made again in BGRA8 only, which DXGI converts as before,
rather than the tone map guessing the level. The first frame's texture,
not the duplication's mode, decides whether pictures are tone mapped: for
an FP16 one that the mode did not announce, the capture thread reads the
level itself, and where Windows tells none there either, the capture fails
and FP16 duplication goes off for the process. As the first FP16 picture
arrives, the capture thread builds the tone map
(`platform/windows/video/tonemap.rs`), compiling its shaders
(`tonemap.hlsl`) with `D3DCompile` once per process: `d3dcompiler_47.dll`,
which ships with Windows, is loaded from System32 for that and let go, so
nothing imports it and a session that never sees an HDR desktop never
loads it. Each FP16 picture is then drawn, while DXGI holds the frame, by
one full-screen triangle into a BGRA8 texture of the tone map's: divided
by the SDR white level, anything brighter than SDR white (HDR video, a
game) brought down to it by one gain on all three channels, which keeps its
hue but not its highlights' detail, a colour outside the sRGB gamut
clipped to it, and encoded sRGB, so SDR content keeps the values it has on
an SDR desktop. The draw is recorded on a deferred context once per
texture DXGI hands out and replayed on the immediate context with that
context's state restored (`ExecuteCommandList`), since the video processor
and the encoder queue their work there too; the video processor then
converts the texture, turning and scaling it, as it converts any, and
direct conversion stays BGRA8's. A tone map that fails alike each time
turns FP16 duplication off for the process and logs
`desktop_capture_hdr_failed` once: shaders that cannot be compiled (no
compiler, say) at once, and a tone map that fails to build, or to draw and
convert a picture, on a device still present, three captures in a row with
no picture between. That capture fails, and its retry duplicates in BGRA8,
which DXGI converts as before. A failure on a device that was removed (a
driver reset or update) does not count: the retry builds the tone map
again on the new device. `desktop_video_encoder_opened` logs `duplication`
(the formats the duplication lists), `hdr` (its mode was FP16) and
`sdr_white_nits`, and `desktop_capture_hdr` the tone map's build
(`compiled`, `build_ms`). The test
`an_fp16_desktop_is_drawn_as_the_sdr_picture_the_display_shows` (in
`tonemap.rs`) compiles the shaders and draws known FP16 pixels on WARP,
which every Windows has, so CI runs it; no device with HDR on has shown the
shared picture yet. `video_bench --greys` measures it there, against
`--sdr-duplication`, the BGRA8-only duplication as before.

macOS hosting uses
ScreenCaptureKit, which scales the
display to the encoder's size and delivers NV12 (BT.709) without the cursor;
its output queue keeps the newest complete picture, with when it arrived,
and wakes thread C when one becomes available; that arrival dates the
picture for thread C's pacing. ScreenCaptureKit delivers a picture only at
one of the display's refreshes, and at most once per minimum interval
(`minimumFrameInterval`). The host sets that interval from the display's
refresh and the session's pace (`platform/macos/cadence.rs`): half a
refresh short of every n-th refresh, (n − 0.5) / refresh, where n is the
pace's own multiple when the refresh is one within 1% (2 at a pace of 60 on
a 120 or 119.88 Hz display), so that pictures come at the pace and thread C
takes each as it arrives, and otherwise 1, every refresh, while the refresh
is below eight times the pace, and floor(refresh / pace / 4) above. The
refresh is the display mode's rate, or, where the mode reports none (a
built-in panel may), the nominal period of a display link on the display,
read again at most once a second. With neither, the interval stays what it
was before for every display, 0.9 / `max_fps`, and does not follow the pace.
That interval is not a whole number of refreshes on a 75, 100, 144 or
165 Hz display, and a gate that holds each picture to it rounds it up to the
next whole number of refreshes, which passes fewer pictures than the frame
rate there. Whether ScreenCaptureKit gates that strictly, and so what frame
rate the new interval gains on such a display, is still to be measured on
one (the ignored test `sck_cadence` measures it). On such a display the new
interval lets every refresh through, where the old one let fewer through if
the gate is strict; what that costs the window server and the GPU is
unmeasured too.
No interval is longer than that one before, 0.9 / `max_fps`: n is at most
the largest whole number of refreshes whose (n − 0.5) / refresh fits in it,
and at least 1. A change on screen that ScreenCaptureKit holds back for the
interval must still arrive at the next refresh it allows; until a display
shows that it does (the ignored test `sck_held_back_change`), no change
waits longer for its picture than it did before, so a slower pace, the
unresponsive pace of 2 included, slows the stream only that far. The
interval follows the pace live, through `updateConfiguration` with one
change in flight and only the newest waiting for it: a faster pace applies
at once, a slower one once it has held for a second, a new refresh or
frame-rate limit at once. A change ScreenCaptureKit refuses fails the
stream at thread C's next take, as a stop does, and thread C opens another
at the interval it wants. While every viewer is paused the stream runs at
the longest interval allowed, and a resume asks for the pace's interval
before thread C takes the resume's keyframe, which still comes from the
last picture at once. A change of the
viewers' limits that leaves the encode size as it is (a viewer at another
refresh joining, pausing, resuming or leaving) keeps the stream and the
VideoToolbox session: the stream takes the interval for the new frame-rate
limit, the session the limit as its expected frame rate, best effort, and
thread C still asks for the IDR the change needs. A start, or a retry,
finds the display among ScreenCaptureKit's displays and on-screen windows
only, without the desktop's, rather than every window of every app: the
filter of a whole display excludes no window, so the picture is the same.
From macOS 14 the display fills the encoder's frame (`preservesAspectRatio`
off): the encoder's size, rounded down to even pixels, may miss the
display's shape by up to two pixels, which a picture keeping its shape left
as a dark row or column along an edge while viewers map the pointer across
the whole frame. Before macOS 14 the picture keeps its shape on the black
background. Stills share the configuration. A size that is not a multiple
of 16 both ways, as a 14-inch MacBook Pro's 3024x1964 and a 13-inch
MacBook Air's 2940x1912 at Native, 1920x1080, or a display fitted to a
viewer's limits (3024x1964 fitted to 1440p is 2216x1440), is coded in a
frame padded to the next multiple of 16 (`platform/macos/frame.rs`:
3024x1968, 2944x1920, 1920x1088, 2224x1440). The stream is that size,
ScreenCaptureKit draws the display into its top left at the content's
size in pixels (`destinationRect`, filled as above), the rest takes the
black background, and the session codes the padded frame. VideoToolbox
encodes a picture inside its call where the picture's buffer covers a
multiple of 16 both ways, by its own size or with the pixels it says it
extends by, and any other in the background: on an M6 its call took 4.7
to 13.1 ms at the median for the first, by size, and 21 to 38 µs for the
second. A padded frame's pictures always cover one. Each keyframe's SPS
crops the padding away ([H.265 and H.264](#h265-and-h264)) and the units
say the content's size, so a viewer shows the content alone and maps its
pointer across it, as before. On an M6, in a loop paced as thread C paces
with two units in flight, a padded frame's units took 1.1 to 2.4 ms less
from submit to output at 60 FPS than the content coded at its own size,
at the same frame rate (the median of five rounds; at 3024x1964, 10.71
against 12.88 ms with H.264 and 10.38 against 12.64 ms with H.265); at a
pace of 120, 0.95 to 1.35 ms less at 1920x1080 and 2216x1440, and 5.7 to
9.4 ms less at 2940x1912 and above, which the encoder does not keep up
with, at from 2.8 fewer to 0.4 more units a second
([measurements](benchmarks/2026-11-24-mac-vt-padded-capture/README.md)).
Pictures of the content's own size whose buffers said they extend to the
padded size took as long, at as many units a second: where
ScreenCaptureKit's own pictures of a size already extend so
(`macos_capture_layout`, below), VideoToolbox already encoded them inside
its call, and padding gains nothing there. A host pads nothing where
ScreenCaptureKit cannot place its picture, nor for the rest of its process
once a session's SPS would not crop. What ScreenCaptureKit's own pictures
of these sizes are, and that it draws a padded frame as asked, is still to
be seen on a device. Thread C submits the newest
picture's IOSurface at the
negotiated pace without a copy. On a still screen it can reuse the last
picture for a requested keyframe or the 250 ms liveness repeat; it runs no
refinement burst, since VideoToolbox gives a repeat the bits of the time
since the last unit, and a burst made the picture converge no sooner in
the [still-refinement record](benchmarks/2026-11-24-still-refinement/README.md). Thread C
only hands the picture over: each encoder has its own submit thread, at the
default QoS class as thread C is, which makes every call on
the VideoToolbox session, the encode with its `ForceKeyFrame` option and the
bitrate with its cap, one at a time and with no lock held across a call,
and thread C passes it a picture or a bitrate through a one-slot mailbox. A
picture whose buffer covers a multiple of 16 both ways (1440p, 4K, a
padded frame) is encoded inside `VTCompressionSessionEncodeFrame`, which
calls back before it returns, so before
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
asks for 1 ms; the helper's capture worker holds one while the host streams
(`Request::Stream`). The last guard released restores the earlier throttling state and
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
pointer at least every 16 ms (through the helper, the worker reads it as
often while the host streams) even while the network or the encoder holds capture
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

Each session through the helper runs in a fresh worker, which would probe
cold every time, while the host that asks it (the app, or the background
host in the service) outlives its workers. So the host keeps each worker's
probe answer, for the rest of its process, under the GPU it probed (helper
protocol 11): the vendor, device, subsystem and revision ids of the adapter
that drives the display, and its user-mode driver's version, which DXGI
reports for `IDXGIDevice`. Not its LUID, which names an adapter only until
the system restarts and says nothing of its driver. For a viewer that
decodes H.265 the host first asks the worker to name that GPU
(`Request::Adapter`, which enumerates the adapters and their outputs and
reads the driver's version, without a probe); a later worker on the same GPU
and driver is then not asked about H.265, so its viewer's `Accepted` does not
wait for a probe, and the session's encoder is the first H.265 encoder that
worker builds. A viewer that does not decode H.265 has the worker name
nothing. A driver update changes the key, and the next worker probes again.
A no is kept as a yes is, but only a probe's, as the backend in the app
keeps its probe's: the worker names the GPU with its answer only when that
answer is its probe of that GPU, so a no it gave because its display could
not be described, or no adapter was found for it, holds for that session
alone. An H.265 encoder that fails through the helper drops the answer kept
for its GPU, as H.265 goes off for the process anyway (below). A worker that
cannot name its GPU, or whose driver reports no version, is asked every
time. The host keeps answers for at most 8 GPUs, dropping the oldest, and
logs each answer it gives a session as `desktop_h265_helper` (usable; named:
whether the worker named its GPU; kept: whether an earlier worker gave the
answer; ms, including both requests). How much sooner a later session
reaches `Accepted` and its first picture, and what naming the GPU adds, have
not been measured on a Windows device yet.

The H.265 encoder takes the H.264 encoder's settings: low latency, CBR, an
infinite GOP, IDRs only on demand and the same bits per frame, which show as a
better picture rather than a lower rate. Its output type is
`MFVideoFormat_HEVC` with the Main profile, which allows B-frames: an encoder
that implements `CODECAPI_AVEncMPVDefaultBPictureCount` must accept 0 and read
0 back, or it is not used. NVIDIA's HEVC Encoder MFT answers `E_NOTIMPL` to
that property; there the required low-latency mode keeps B-frames out, and
every output is still checked against reordering.

A Windows host asks for H.264 in a High profile with CABAC, as a Mac host's
VideoToolbox session already writes it, rather than in Baseline, whose CAVLC
entropy coding and 4×4-only transform spend more bits on the same picture.
It tries Media Foundation's Constrained High
(`eAVEncH264VProfile_UCConstrainedHigh`), then High, each with
`CODECAPI_AVEncH264CABACEnable` set first, best effort; an encoder that
refuses the output type, or then the NV12 input type, in both is built in
Baseline, with CABAC taken back. High allows B-frames, so H.265's rule
applies: an encoder that implements `CODECAPI_AVEncMPVDefaultBPictureCount`
must take 0 and read 0 back, or it is built in Baseline, and one that does
not relies on the required low-latency mode and the check of every output.
Every H.264 encoder took Baseline before, and none may lose H.264 to High,
which has no software fallback: a High failure turns High off for the rest
of the process (`H264_HIGH_FAILED`, `desktop_h264_high_failed`). An
encoder that takes its High types and then will not start streaming, or
reads a B-frame count above 0 back once it has them, is activated again at
once, a new instance, in Baseline. One that fails before its first output
(an input or output it refuses, or a stall), or whose output is reordered,
fails that capture, as before, and the capture's retry, which builds the
encoder again at once, builds it in Baseline. The latch lasts as long as
the process that owns the encoder: the app, for a computer shared from the
app; behind the desktop service, each connection's capture worker, so
there every connection to a GPU whose encoder reorders or fails in High
pays that one failed capture. `video_bench --h264-profile base` asks for
Baseline. Nothing about this
reaches the wire or the helper pipe: every viewer already decodes a Mac
host's High stream. How much sharper a High picture is at the same bitrate,
on which GPU, has not been measured yet
([Encoder rate model per GPU](#encoder-rate-model-per-gpu)).

A decoder that finds no `bitstream_restriction` in an SPS's VUI may take the
stream for one that reorders pictures and hold pictures back before it shows
them. A Windows host's H.264 SPS goes out rewritten when its VUI has no such
restriction, or one that allows reordering: `max_num_reorder_frames` 0 and
`max_dec_frame_buffering` its reference frames, with a VUI holding only that
restriction where it had none, and everything else, the constraint flags
included, as the encoder wrote it. A restriction added where there was none
states the rest as VideoToolbox writes its own: motion vectors may cross
picture boundaries at the length an absent restriction allows
(`log2_max_mv_length_*` 15), and
both size denominators are 0, which states no bound where an absent
restriction would infer 2 and 1 (`codec/sps.rs`, through
`annexb::ParameterSets`). It is read once each time the encoder writes a
different SPS, in its media type or in band, never for a unit that repeats
it; a unit that carries it gets the rewrite in its place, and a keyframe
that comes without it gets the rewrite in front. An SPS the parser cannot
read whole, or whose rewrite does not read back as written, goes as the
encoder wrote it. Baseline's SPS is rewritten alike. VideoToolbox's
low-latency SPS already says `max_num_reorder_frames` 0, and a Mac host
sends its own. Which Windows encoders write an SPS that needs the rewrite
has not been checked on a device. Its tests read and write back, byte for
byte, SPS that VideoToolbox wrote on this project's Mac and SPS built with
timing, an HRD, scaling matrices, picture order type 1 and field coding;
compare each rewrite with one built by an independent reader and writer;
and, on a Mac, have Core Media build a format description from each, at the
original's dimensions (`platform/macos/tests.rs`)
([record](benchmarks/2026-11-24-windows-encoder-settings/README.md)).

A Mac host that codes a padded frame (see the Mac host in
[Host: capture and encoding](#host-capture-and-encoding)) sends each
keyframe's SPS cropped to the content: H.264's `frame_cropping` and H.265's
`conformance_window`, widened on the right and at the bottom by the
padding, in chroma samples (two pixels each, in the 4:2:0 frames every
host codes), as an encoder's own SPS crops a 1080p picture's last eight
rows (`codec::sps::crop_sets`). Nothing else changes: H.264's SPS is read
and written whole, as for the reorder rewrite, and H.265's is read through
its profile, tier and level (each sub-layer's that is present), its chroma
format and its picture size to the window, and the rest, VUI and
extensions included, is copied as the bits it was; the VPS and PPS go as
VideoToolbox wrote them. The units still say the content's size, so
nothing else reaches the wire. The SPS is cropped once for each format
VideoToolbox writes, which it kept for a whole session in a test here,
and the cropped sets go in front of each keyframe, as before; no other
unit carries them, and nothing more runs per unit. The cropped sets are,
byte for byte, those VideoToolbox writes for a session at the content's
own size, whose SPS crops the same rows and columns: at the bottom for
1920x1080, at the right for 2216x1440 and at both for 2940x1912, so a Mac
host already sent right-edge crops at widths that are not a multiple of
16, and no viewer gets a header it did not get before. An SPS that would not
show the content exactly (not 4:2:0 frames, one that shows less than the
content, an odd number of rows or columns to crop) fails that capture as a
capture failure, not as the codec's, so H.265 stays on, and turns padding
off for the rest of the process (`macos_encoder_crop_failed`): thread C's
retry codes the content's own size, as before. The tests crop parameter
sets VideoToolbox wrote here at 1920x1088, 3024x1968 and 3344x2160 (H.264
with low-latency rate control and without, H.265 with it), check every
other field and set unchanged, and have Core Media read each crop at the
content's size; a padded session's keyframe carries the parameter sets a
session at the content's size writes, at those three sizes in each
configuration (where the session does not say it encodes in software, as
a virtual machine's may); a padded 2940x1912 frame, grey with a white
last row and column, that a padded session encoded decodes through
VideoToolbox at 2940x1912 with that row and column at its edges, in H.264
and H.265 ([record](benchmarks/2026-11-24-mac-vt-padded-capture/README.md)).

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
t      = base RTT (100 ms until the first probe) + rebase + 100 ms
frames = clamp(ceil(pace × t), 3, 64)
bytes  = clamp(2 × expected bitrate × t / 8, 64 KiB, 4 MiB)
```

The base RTT is the minimum of recent probes, never a smoothed RTT, so queueing
never enlarges the window. The rebase is zero until the QoS controller moves
its baseline onto a longer route (see below). At 60 FPS, a 20 ms base RTT and
4 Mbit/s that is 8 frames and 120 KB. A unit is admitted when nothing is in
flight (whatever its size, so a session always progresses), when it fits both
bounds, or when it is an IDR and no other IDR is in flight: one IDR may go over
the byte window and one frame past the frame window, and never waits for the
whole flight to drain. Its sequence number is booked before the write, so a
receipt ahead of the last booked sequence is a protocol error.

When receipts show a standing queue, the byte window follows the delivery rate.
The ack queue is the median receipt delay above the base RTT over the last
250 ms, or over the newest three receipts when fewer came in that time: a slow
link receipts seldom, and its queue must not vanish between two receipts. It is
zero before three receipts. Above 100 ms, once receipts have been measured for
500 ms, `bytes` becomes 1.25 × the delivery rate of the last 500 ms × `t`, at
least 32 KiB and at most the normal window. While a receipt episode lasts (see
[QoS](#qos-frame-rate-and-bits-per-frame)) `bytes` follows the busy rate the
controller cut for instead, the same way and at once, without waiting for an
ack queue. An application-limited flow has no queue and is never clamped. A
unit the clamped window refuses, with no IDR in flight, marks the session
blocked until the next receipt, which the receipts' controller reads as
congestion from 10 ms of standing queue. After a rebase of the QoS controller's
baseline (a route that became longer), `t` grows by the rebase and the ack
queue counts only what lies above it; of two updates of the rebase that race
between B-send and B-recv, the ledger keeps the newer. JPEG sessions use a
fixed window of four pictures and 2 MiB.

A viewer and a host that both list `arrival_time` time the receipts one way.
The viewer stamps each receipt with when its reader finished reading the
newest unit it covers, payload included, and each probe echo with when it read
the probe, in microseconds on its own session clock. The host pairs a stamp
with when it booked the unit or wrote the probe (the way toward the viewer)
and with when the receipt came back (the way back: the viewer's writer and its
uplink). The two clocks share no epoch, so each one-way delay carries a
constant offset, and only its rise above its least over the last 60 s (kept
one per 250 ms, in at most 256 entries reserved once) is read: that rise is a
queue. Receipts and probes share the forward least; only receipts keep the
reverse one. With arrival times the ack queue is the median of the receipts'
forward queues, with no base RTT taken off: the unit's own transfer is in it,
as it always was in the receipt delay, so the 100 ms limit means what it did,
and nothing on the way back is. `t` grows by the least of the newest three
reverse queues, at most 1 s:

```text
t = base RTT + 100 ms + min(newest three reverse queues, 1 s)
```

A busy uplink at the viewer holds its receipts back without queueing a byte
toward it; the longer `t` lets the window cover that wait instead of starving
admission, and one late receipt never widens it. In the slow-link suite, 300 ms
queued on the viewer's uplink alone left the pace at 60 FPS and the window
unclamped at 27 frames, where both probes and receipts had taken it to 4 FPS
and a 32 KiB window
([measurements](benchmarks/2026-11-24-arrival-time/README.md)).

A stamp is an observation, never authority. One that goes backwards for its
kind is ignored, and so is each direction whose delay lies more than 3 s above
its least. The two directions are judged apart: a queue longer than that on
the viewer's uplink still leaves the way toward the viewer timed, so a queue
there still clamps the window. A stamp that puts one direction more than 3 s
above its least and the other more than 3 s below its own is off, not queued,
and is ignored whole, so it never drags a least down. Only a malformed message
breaks the protocol. The least resets on a path change and on a resume. The
frame and byte maximums, the IDR's headroom and JPEG's window stay as above.

Receipts reach only the credit ledger and the QoS controller. They never reach
the input ledger, the backend or the control lease, and no authorisation method
takes a sequence number. In the QoS controller they drive the rate (see
[QoS](#qos-frame-rate-and-bits-per-frame)).

### QoS: frame rate and bits per frame

Transport receipts drive the rate, after airhand's per-frame acks: a standing
queue cuts bits per frame within a round trip and keeps the frame rate, which
falls only once bits per frame reach the preset's cut floor, and then only to
what the link delivers at that floor. The probe controller below remains for
severe or unexplained delay. This reverses decision D5 of the overhaul plan
(the probe adjusts fps and bitrate) and CONTRACT-perf R13 (receipts only clamp
the byte window), at the owner's direction to give every platform the best
performance airhand's design inspires; the deviation is recorded in
[DECISIONS.md](remote-desktop-overhaul/plan/DECISIONS.md).

Each receipt of H.264 or H.265 video that is not paused updates three figures
over the newest second of receipts. The standing queue is the least delay
above the baseline over the newest receipts (at least three, spanning two
frame intervals and 50 ms, at most 500 ms), each less its unit's own transfer
at the busy rate and at most a frame interval; while no busy rate is known, a
frame interval. The busy rate is the median delivery rate over the newest
eight receipt pairs across which the link stayed busy (the later unit's delay
above the baseline at least the pair's gap); pairs less than a quarter of a
frame interval apart are compression and count toward the pair before. The
used rate is what the receipts covered over the second. The baseline is the
probes' base RTT, or a rebased one (below); with arrival times each receipt
is timed by the viewer's stamp and its delay is its unit's queue toward the
viewer, so nothing on the way back counts.

The queue is congested above 40 ms, or above 10 ms while the session is
blocked (see [Send credit](#send-credit)). Congested, a receipt:

- inside a hold decides nothing;
- with the queue draining (5 ms below the one before it, or below 0.8 × the
  episode's last) holds for a round trip and the queue;
- after two cuts that left the queue within 1.1 × the last rebases, once the
  link delivers no more than 1.1 × what the last cut asked for: the queue is
  a longer route, not a rate the host sends. While the link delivers more,
  the queue is the host's backlog draining, or an encoder that did not follow
  the cuts;
- without a busy rate decides nothing: there is no blind cut, and the probe
  controller remains;
- otherwise cuts the expected rate to `min(0.85 × busy, 0.9 × used)`, never
  below the preset's floor at its minimum pace. A cut to within 5% of what
  the encoder sends, as the host measured its frames (below), only holds
  again, and counts toward no rebase: the encoder has yet to follow the last
  one, or the host is at its floor.

After two cuts, a queue between 10 and 40 ms that stays where they left them
(within 1.1 × the last, and not draining) rebases as well, on the same
condition: a light picture's frame interval, longer at the lower pace, hides
part of a route's queue below 40 ms.

A cut holds the next decision for a round trip and the queue, between 100 ms
and 1 s, and while the used rate stays above 1.1 × the cut's target, up to
1 s, so an encoder whose output trails its bitrate is not cut again before it
follows. Receipts behind an IDR in flight decide nothing. A stall (a receipt
gap of at least three frame intervals whose first receipt waited through half
of it; a still picture's idle gap carries no delay) never cuts: the receipts
after it, compressed or each with less delay than the one before, are what it
held. Stalls that recur, three ending within 2 s or two that cover 15% of it,
are congestion and cut to 0.85 × the used rate; after two such cuts that left
them as they were they are excused until they stop.

A cut lands in bits per frame, down to the preset's cut floor, at the pace
before the startup cap. Below the cut floor the pace is capped at what the
cut carries at what a frame then costs, down to the preset's minimum; below
that, bits per frame fall again, to the preset's floor. For Balanced and
Smooth the cut floor is the floor; Quality keeps half its start (100 000 bits
a frame at 1080p) before its pace falls, for sharpness. The pace cap, like
the startup cap, clamps only the output.

A frame costs its budget unless the host measured otherwise. Every QoS tick
records the non-IDR frames and bits the encoder produced; over the newest two
ticks, each with at least two such frames and none encoded at a higher budget
than now with frames above it (the frames from before a cut), the bits per
frame are what the encoder spends. A light picture, which spends less than
its budget, costs what it spends, so a cut of 15% takes 15% of its pace
rather than most of it. An encoder that spends more than 1.25 × its budget in
both ticks, without its excess falling to 0.8 × the tick before's as a VBV
following a cut does, is at its content floor: a lower budget sheds nothing,
so its frames cost what they spend, and the receipts are told it sends that
much. Such a cost at most halves the pace a cut. An encoder that budgets per
second also makes larger frames once the window holds captures back: under
the timestamp model it outspends only if its bits per second also exceed
1.25 × its bitrate, and under the nominal model a lower pace sheds none of
its bits until the rate-model latch finds it, below.

The QoS tick raises the rate while the receipts are fresh and show no hold,
episode, recurring stalls or queue of 10 ms, and no rebase came in the last
second: 15% a second, or 4% while the rate is within 0.9 × the busy rate of the
last cut, plus 100 kbit/s a second, applied once it adds 5%. That capacity is
forgotten once a raise passes 1.2 × it, or, for an encoder that spends more
than expected, the rate the link carries does. A light picture, which uses less
than half the expected rate, raises only the pace cap, while one is set: it
costs little at the full pace, and a queue it builds is cut within a round
trip. An encoder that spends more than it is asked for is raised as any other:
the link carries what it sends. A raise lifts the pace cap first, so the frame
rate comes back before sharpness (for Quality, bits per frame below its cut
floor come back first), then bits per frame, up to the preset's maximum. An
episode lasts from a cut until the queue is under 10 ms. Once it has ended and
the queue has stayed under 10 ms for 500 ms, raises climb at 50% a second
toward 0.9 × the expected rate congestion began at. A climb that a cut stops
short of that falls back to 15% and 4% a second until a raise passes 1.2 × the
capacity, which shows the link carries more, as when a limit lifts; then it
climbs again. A start that a closed session's rate lowered climbs back to the
preset's start at the same 50% a second, a light picture's too, until its
first cut (see the next section).

For 2 s after a cut, a rebase or a drain, while receipts come, the probe
controller yields its frame rate cuts to the receipts: a delay from 150 to 600
ms holds its fps. From 600 ms, and whenever the receipts do not own the queue,
its steps below apply as before. An episode whose holds change nothing, as at
the preset's floor, hands the frame rate back to the probes 2 s after the
receipts last acted. A rebase raises the probes' baseline by as much as the
receipts' own (their least delay, less each unit's transfer, which every lower
receipt lowers further), at most 150 ms, until their 60 s base reaches it or a
probe shows the longer route gone, and the credit window follows it; a path
change, a resume and a preset change end it. Both controllers so read one
baseline, and a queue can creep up behind rebases by at most 150 ms and the
probes' steps. While receipts come, they own bits per frame: the 3 s window
below steps them only in a window without one (a still picture, a pause, a JPEG
session).

The host writes a probe in stream order between video messages, so it measures
downstream queueing; one is outstanding at a time. It writes one a second, and
one every 250 ms while the base RTT is under 100 ms (a short path). The base
RTT is the minimum over the last 60 s, and it resets when the viewer reports a
path change. A probe written behind an IDR in flight is tainted: it updates
the RTT but never the frame rate. A 3 s window with only tainted samples may
step bits per frame down on their worst delay, never up. The queue delay is
the median of the last three untainted RTTs (of two, the lower) minus the base
RTT, so one probe that a Wi-Fi stall held moves it not at all.

With arrival times (see [Send credit](#send-credit)) the delay is one-way:
each untainted probe's sample is its queue toward the viewer, the median of
the last three is the delay, a tainted probe's queue is what a window with no
other sample steps on, and the receipts' witness below is their forward ack
queue. A round trip counts as at least 10 ms, which on a shorter path hides
the first (10 ms − its least round trip) of any queue; the one-way samples and
the forward ack queue hide as much, so a queue toward the viewer moves the
controller exactly as the round trip's did. A queue on the viewer's uplink (a
cloud backup, a photo sync or a video call on the viewer's side, or a cellular
uplink's scheduling) then lengthens the round trip without moving the frame
rate or bits per frame, while a queue toward the viewer cuts as before. The
round trip still sets the base RTT that sizes the window, the short path and
Balanced's fast start. An echo whose stamp is no sample counts toward the base
RTT and moves nothing else. A round trip over 3 s is no sample of the path,
with or without arrival times, but the queue toward the viewer that its stamp
shows still is one: an uplink queue longer than that leaves a queue toward the
viewer cutting as before.

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
Without either it holds. Balanced H.264 and H.265 start at the ceiling
instead of the start value at the first untainted probe after the preset is
applied, whatever its round trip: the receipts cut within a round trip if the
path cannot carry it. The startup cap still holds the pace for its second. The
controller works in bits per frame, so a frame-rate cut sheds bandwidth at
once. Every 3 s in which no receipt came it scales bits per frame by
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
bounded by the ceiling, the viewer's fps cap and thread C's cap at its
encoder's throughput
([Host: capture and encoding](#host-capture-and-encoding)). It is held to
`min(start, 15 × scale)` for 1 s after the
session starts and after a resume, to 2 FPS while the viewer is unresponsive,
and to 8 FPS for JPEG. A viewer is unresponsive while its probe echo, or a
receipt it owes, is more than 2 s late. That cuts only the output pace and
leaves the controller's state untouched; the next receipt or probe echo clears
it, and the session never closes because of it. Changing the preset
mid-session restarts the controller at the preset's start values, without an
IDR, an encoder reconfiguration or a resolution change. Pausing sets the pace
to 0.

An H.264 or H.265 session that its transport ended (the viewer closed it,
the connection failed, or the viewer went silent for 3 s) leaves the host one
record: its bits per frame relative to 1080p, its preset, and when it ended.
A session of the same preset and such a codec that opens a room within 2 min
of that end, by both the monotonic clock and the wall clock (a Mac's
monotonic clock stands still while it sleeps), starts its bits per frame at
the lower of the preset's start and that record, scaled to its own picture;
its frame rate starts as above, Balanced's fast start included. A JPEG
session neither takes nor leaves a record, since bits per frame do not size
its pictures. A viewer that joins a room already streaming starts at the
preset's start: the room's encoder runs at the least of its viewers' rates,
so a record would lower the picture of every viewer already watching. The
record never raises a start, and a later such end replaces it. A session the
host ended (Disconnect, a stop, a restart, a revoke, a codec change, a
protocol error or a capture failure) leaves none, and so does a session a
record lowered that nothing of its own has moved since (no cut or step down
by either controller, no raise of its bits per frame, and no preset change):
the record keeps its own end, which a quiet session never renews. The host traces `desktop_start_rate_remembered` when a
record lowers a start.

A start a record lowered climbs back to the preset's start, a still or light
picture's too, at the recovery's 50% a second plus 100 kbit/s a second, from
0.5 s after the startup cap once the receipts' queue has stayed under 10 ms
for 500 ms: by then a link that cannot carry the full pace has queued, and the
first cut, or the probes' first step down, ends the climb before it begins.
From there the session runs as any does.

A viewer that reconnects over the link that ended its session so sends its
first IDR, and its startup second, at about the rate that link last carried
rather than queueing them behind the link: in the slow-link suite's reconnect
on a 1 Mbit/s link, with that suite's synthetic encoder, the reconnect's
first picture was presented 105 to 109 ms after `Viewer::connect` instead of
303 to 311 ms, its IDR was 8 332 bytes instead of 33 332, and the host's ack
queue was 0 ms at each of its first three once-a-second samples instead of
peaking at 263 to 266 ms. Its frame rate paid for that: started at the floor,
its first cut had no bits per frame left to take and lowered the pace to
36 FPS in its second interval, and its sampled pace averaged 44.8 to 47.3 FPS
over intervals 2 to 7 against 47.0 to 48.2 before. Any other viewer of that
preset that opens a room within those 2 min starts as low, its first IDR
included, and climbs back: in the same case a viewer on an unlimited link
reached 80% of Balanced's start 4.0 s after it connected (7.0 s while such a
start climbed only at the raises' 15% a second), where without a record it
starts above that
([measurements](benchmarks/2026-11-24-reconnect-start-rate/README.md)). A
viewer that reconnects before the host saw its old session end joins the room
that session is still in and starts at the preset's start, though the room's
encoder runs at the old session's rate until that one times out. The record
lives as long as the host: a restart, such as a display switch, starts
without one.

The encoder bitrate depends on how the encoder budgets. A per-frame-budget MFT
(the `nominal` rate model, the default) gets `bits per frame × nominal fps` and
spends one frame's budget per frame, so a lower pace sheds bandwidth at once.
An encoder that budgets per second of presentation time (the `timestamp`
model) gets `bits per frame × pace`. Each session starts at the model its
backend declares (`BackendCaps::rate_model`, `nominal` unless set), and every
viewer of a room starts at the same one; today every platform's encoder, and
every session through the Windows helper, starts at `nominal`. A `nominal`
session whose encoder budgets per second would overspend at a reduced pace,
by about the nominal rate over the pace: a receipt cut of bits per frame lowers
its bitrate, but the receipts' pace cap below the cut floor sheds nothing on
it until the latch. At every 250 ms QoS tick the host
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

The H.264 profile, quality against speed, the display-remoting scenario and
the GPU priority have not been measured on any GPU yet. The
[record](benchmarks/2026-11-24-windows-encoder-settings/README.md) lists
the runs, per vendor, in release mode: `video_bench 10 60 --codec h264
--h264-profile base --text --bitrate 4000000 --output h264.base
--dump-source src.nv12` against `--h264-profile high`, compared by
ffmpeg's PSNR and SSIM against the dumped source, with IDR sizes, encode
time and dwell, and the `--pace 60,30,15,5` sweep still `nominal` within
±25%; `--qvs 0,33,50,66,100`, with and without `--scenario
display-remoting`, for H.264 and H.265 at 1080p and 4K; and `video_bench
30 60 --animate` beside a GPU-saturating 3D load with and without
`--gpu-priority`, elevated and not, hardware-accelerated GPU scheduling on
and off. A live session, in the app or through the helper, takes
`EncoderOptions::default()` from `NativeBackend` (`platform/windows/mod.rs`),
so its arms need a build with `gpu_priority` set there; through the helper
the capture worker holds no privileges, so that arm tests the scheduling
class alone. A quality-against-speed value or the scenario
becomes a vendor's default at a size only where it raises PSNR at equal
bitrate by 0.3 dB or more (or cuts bytes at equal QP by 5% or more) while
the median encode rises by 1 ms or less at 4K60, dwell stays within a
quarter of the frame period at the session's largest size and pace (4.2 ms
at 4K60), and the rate model stays `nominal`. The GPU priority goes on by
default only if it lowers dwell p95 or present-to-output time under that
load without lowering the load's own frame rate by more than 5%.

Still refinement has not been measured on any GPU either, so
`REFINE_ENCODERS` lists none. Per vendor, NVIDIA first, in release mode
at 1080p and 4 Mbit/s: `video_bench 6 60 --text --freeze 2000 --bitrate
4000000 --max-width 1920 --max-height 1080 --refine off --output off.h264
--dump-source off.nv12` against `--refine on`, then each with `--codec
h265`: each unit after the freeze (its `still_unit` line) by size, QP and
ffmpeg's PSNR against the last new picture's source, and how long the
frozen page takes to come within 0.5 dB of the picture it converges to.
An encoder joins the list only where a burst brings that sooner and its
repeats fall under 512 bytes once the picture converged (no filler); the
same runs set the QP floor, the cap and the 200 ms. In a session with
refinement on (a build that lists the encoder), one paced at three
quarters of nominal or less, where the rate-model latch reads, whose page
scrolls and stops again and again must not latch: `video_bench --pace`
cannot show this, as its content changes every refresh and so runs no
burst. A session typing a character every 250 to 400 ms on a still page
must start no burst and keep its present-to-submit p95, and one with two
viewers, one of them lagging, must not cycle between the lagging viewer's
IDRs and bursts. The
[still-refinement record](benchmarks/2026-11-24-still-refinement/README.md)
lists them.

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

The display the viewer's window is on bounds all of this: a frame that
display has no refresh for is superseded unseen. The app on macOS, Windows
and Linux connects at the fastest display's refresh rate (the `decoders`
probe's `display_max_hz`, or the window's display where that is faster), so
that `limits.max_fps` allows the fastest screen the window may move to. It
then tells the viewer the rate of its window's display, the hot op
`display_refresh` (`Viewer::set_display_refresh`): once after connect, when
that differs from the connect's, and whenever it changes, because the window
moved to another display, a display's mode changed or the app came back to
the front. What goes after connect is the window's display as it is then: a
change while the connect ran is not lost. The rate rounds and clamps as at
connect, 30 to 120, so 59.94 Hz asks 60. Below `limits.max_fps` it bounds
what the host is sent under every preset: the backlog cap within it, and
with no backlog cap the rate itself, not `limits.max_fps`, which would clear
the host's limit. Backlog lowers beneath it and raises stop at it: a
consumer keeping up at that rate says nothing of a faster one, so beneath a
slower display a backlog cap is neither raised past it, probed nor removed,
and when the display gets faster the host gets the backlog cap, from which
raises and probes go on as before: where the queue stayed short at the
slower rate, the first raise, up to 95% of a known capacity, goes at once.
A new rate goes to the host at the next 250 ms tick with the existing
`FpsCap`, whatever the backlog's hold and spacing, and also while paused or
before the consumer is judged; a change undone before that tick sends
nothing. So a Smooth session whose window moves from a 120 Hz to a 60 Hz
display stops encoding, sending and decoding the frames the display would
drop, and returns to 120 when it moves back, without a reconnect, or, if its
consumer fell behind earlier, to its backlog cap. Balanced and Quality,
whose ceilings are 60 and 30, change only on a display slower than that,
but the host keeps the display's limit for a switch to Smooth. A frame rate
the user chose (`fps_override`) ignores the display: the frames it brings
are fresher. Android ignores it, as it ignores the display at connect, and
iOS sends none.

Where the window's display rate comes from:

- macOS: Flutter's view display (`FlutterView.display.refreshRate`) and its
  metrics changes. The macOS embedder updates the view's display when the
  window moves to another screen and the display list when a screen's mode
  changes, and both are metrics changes; the rate is the display link's
  nominal one, rounded. A metrics change that leaves the rate, such as each
  step of a resize, sends nothing.
- Linux: the Linux embedder names the view's display again only when the
  view's size or scale changes (its `fl_view.cc` says it cannot tell a move
  between monitors) and reads the monitors' rates only when one is added or
  removed, so the app asks the runner's `refresh_hz`
  (`pb_mapper/desktop_video`, no arguments): the monitor with the most of
  the window (`gdk_display_get_monitor_at_window`) and its rate, exact, in
  millihertz (`gdk_monitor_get_refresh_rate`). The runner calls
  `display_changed` on the same channel when the window's `configure-event`
  finds it on another monitor, a monitor is added or removed, or a monitor's
  `refresh-rate` changes. X11 sends `configure-event` on a move; Wayland
  tells GTK 3 nothing of a move between outputs of one scale, so there a
  move is seen at the next resize, scale or mode change, or return to the
  front. The app reads `refresh_hz` again then too, and when Flutter's rate
  changes, waiting at most 200 ms for it before it takes Flutter's.
- Windows: Flutter's display reads the mode's whole hertz
  (`dmDisplayFrequency`), 59 for a 59.94 Hz mode, so the app asks the
  runner's `refresh_hz` (`pb_mapper/desktop_video`, no arguments): the
  monitor of Flutter's view (`MonitorFromWindow`, nearest for a window
  across two), its GDI device name (`GetMonitorInfoW`), and the active
  display path whose source has that name
  (`QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)`,
  `DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME`), whose target rate is exact
  (60000/1001); without the paths, as outside the console session, the
  mode's whole hertz. The app waits at most 200 ms for it, then takes
  Flutter's. The runner calls `display_changed` on the same channel when the
  view moves onto another monitor (`WM_MOVE`) or any display's mode
  changes (`WM_DISPLAYCHANGE`), and the app reads `refresh_hz` again then,
  when Flutter's rate changes, and when it comes back to the front. Asking
  60 instead of 59 no longer holds a 60 Hz host a frame a second below its
  display's rate while content moves; asking 120 instead of 119 does the
  same at 120.

The app asks a runner `refresh_hz`, and listens for its `display_changed`,
only where its `decoders` answer said `window_refresh: true`, as the Windows
and Linux runners' do; elsewhere, and where the probe failed, it takes
Flutter's rate, so nothing waits on a runner that cannot answer (a widget
test's, for one). Both runners call `display_changed` only once the app has
asked `refresh_hz`, which it does from its first connect on, after it
listens; before that nothing reaches Dart.

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

Toward a host that listed `keyframe_seq`, a request made while predicted
frames are of no use before the next IDR also names the last seq the reader
had read (`after_seq`): the sink's own after a gap or an overflow, a native
decoder's after a gap or a failure, which the runners drop predicted frames
after until their next keyframe, the Linux runner's after a stall, which
rebuilds its pipeline at the next keyframe, and the stall watchdog's, sent
while the decoder takes frames but presents none. A backlog episode keeps
decoding its queue and a user's request drops nothing, so neither names one.
The host asks its encoder for the IDR as before. Unless it has admitted an IDR
after that seq, which is then on its way to the viewer, B-send also drops this
session's predicted units before they get a seq, as it drops a stale
generation's, until it admits the next IDR, and counts them in
`dropped_frames`: the link stops carrying frames the viewer would discard, and
the IDR reaches it sooner. Only an IDR ends the skip. Every predicted unit
after the first one dropped refers to a picture the viewer never got, so a
decoder that is not waiting for a keyframe, as the Windows and macOS runners
are not after the watchdog's request, is never given one it cannot decode. If
no IDR has been admitted 2 s into a skip, B-send asks the encoder for one
again, and again every 2 s. Receipts outrank keyframe requests in the
viewer's writer, which is why the request names the seq. Other viewers of a
room and the host's own resyncs are untouched, and a seq beyond the last one
sent is a protocol error.

`pause` revokes control, tells the host to stop capturing and clears the sink;
probes are still echoed and pings sent, so the host sees a live, paused viewer.
`resume` restarts at an IDR with the startup pace. A presentation is accepted
only for a unit the sink handed out in the current generation, less than 2 s
after it arrived, newer than the last one, and not while paused. A rejected
presentation is never forwarded.

### Input and cursor

The viewer queues input in order behind a revoke barrier: at most 512 events or
256 KiB, with consecutive pointer moves coalescing into the newest, and
consecutive scrolls (`Input::Scroll`) of one generation and continuity adding
up into one, within a scroll's 12 000 units, so a touchpad's stream of events
never fills the queue while the uplink stalls. A barrier (a
release, a grant or a revoke) discards only the input queued before it, never
input submitted after the call that set it, and a scroll never adds itself to
one queued before a barrier: it then goes alone. It sends a
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

The desktop-bound thread samples the cursor while capturing (never while
paused or unavailable) every 16 ms, or once a period of a faster pace, down to
8 ms (8.3 ms at 120 FPS): a pointer a viewer does not draw itself (the host
user's, an app's, another controller's that the OS stopped short) moves at the
session's frame rate. After it applies a move from a controller that reads
shapes, it also looks at the pointer's shape 2 ms later, and once more 8 ms
after the move for a slower app: the hand over a link, the I-beam over text or
the resize arrow at a border that the move brought reaches the viewer then,
not at the next sample (`ShapeLook`). At most one look is pending, and the
newest move owns the looks to come: a look an earlier move left keeps its time
where that is sooner, so moves closer than 2 ms apart never put a look off, and
once it ran the newest move still gets its own two, even when thread C was
busy past it. A new shape found meanwhile cancels none of them, since it may
answer an earlier move. A look reports only a new shape, as a sample would, and
starts the sample clock again; otherwise it reports nothing, so a viewer gets
a position sooner than the clock only together with a new shape. Through the
helper the shape still crosses the pipe only when the worker numbered a new
one, which it reads with each pointer it samples (as often as thread C
does, and 2 ms and 8 ms after a move it applied) and pushes with the
pointer; a pushed change wakes thread C, which samples it at once
(`Backend::cursor_changed`) and starts its sample clock again from it.

The desktop-bound thread reports at once every sample unlike its last report,
except the controlling viewer's own move coming back: the viewer draws
its pointer where it put it, and an echo arriving behind the video would drag
it back. A shown pointer at the position the thread last injected, in the same
display pixels, is that echo, whatever it showed before, until a report finds
the pointer elsewhere; a later return to that spot is someone else's move, and
is reported. A hidden pointer carries no position and is never an echo.
Comparing with the last report, not the last sample, reports a move the OS
stopped short (a `ClipCursor`, an edge) where the pointer stopped. Nothing is
held back and sent late. A backend reports the pointer as every
input it applied left it: the helper's worker, which pushes it as it moves,
also answers each input with the pointer, in order with its pushes, so its
report is never older than the last move. A new generation, a display change included,
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
removes that viewer's limit. On a Mac host, a join, pause, resume or leave
that changes only the frame-rate limit keeps the capture and the encoder
running, and the other viewers' picture with them; a Windows host builds
them again. Capture continues while any viewer has send capacity.
A slow viewer can drain its queued pictures and resumes at a keyframe after a
gap. A single viewer receives the owned frame buffer directly, and two or more
share the same encoded bytes. Each viewer has its own input lane,
presentation ledger, drag-start hold and held-input set: releasing or
disconnecting one controller lifts only keys and mouse buttons no remaining
controller still holds, and a button, wheel or scroll event re-aims the OS pointer to
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
when the session negotiated it, at each pointer sample and shape look, and
compares content, so an equal image under a new OS handle sends nothing. A failed read is the
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

**The viewer's reads and frames.** Dart reads the view leaf (the host's
pointer, its shape number and the peer pointers) in every frame Flutter draws,
from a 16 ms timer, and as each 4 ms move gate closes while this viewer moves
the pointer (`desktop_store.dart`). The cursor layer repaints only when what
it paints changes: `RemoteCursorLayer` follows the viewport, the cursor, the
pointer's look and the display, and asks its painter to repaint only when what
it would draw now is not what it last drew, which nothing painted becoming
nothing painted never is. So a hover while it paints nothing, which is a
controller's whenever the local pointer shows the host's shape, asks for no
frame: over a still picture, moving the mouse costs the viewer no Flutter
frame, and the move gate's read picks up the shape the host gives the pointer
for the move, every 4 ms while the pointer moves and by the timer once it
rests. The painter reads what to draw as it paints, so the frame that lays the
stage out anew, where the viewport moves the picture but tells its listeners
only after the frame, draws the cursor where the picture now shows it. A read
that finds a peer pointer moved, the timer's
included, asks for a frame, whose read asks for the next while the pointer
keeps moving; one that finds it still asks for none. A moving peer pointer is
so read at the display's rate, and a still one on the timer only.

### Constants

| What | Value |
| --- | --- |
| Records | JSON header ≤ 8 KiB; video payload 1 B to 4 MiB |
| Helper pipes | 256 KiB may wait each way; the service relays through 256 KiB toward the app and 64 KiB toward the worker; one write per answer with its byte record |
| Timeouts | handshake 3 s; host read 3 s; viewer header 4 s; writes 3 s plus the message's time at 1 Mbit/s; `Stopped` best effort within 1 s; the host reads on until the viewer closes, within the same second |
| Viewer ping / host statistics | after 1 s idle / every 1 s |
| Standby | one a pair, `MAX_VIEWERS` a host; the live connection gives way after max(1.5 s, 4 × base RTT) without a byte while the standby read a message in that time, or at once when it fails; a committed direct standby at once; silence checked every 100 ms, whether a standby is wanted every 1 s once streaming; a replaced connection kept up to 3 s; a relay standby again after 1 s doubling to 30 s, a direct one after its pause, or 10 s doubling to 5 min, the doubling waits jittered down to ¾ |
| Credit window | `t` = base RTT (100 ms until measured) + a rebase + 100 ms; 3 to 64 frames; 64 KiB to 4 MiB; delivery clamp ≥ 32 KiB once the ack queue exceeds 100 ms, or at once to the busy rate while a receipt episode lasts |
| Host hand-off | 6 units, 8 MiB, 1 s oldest, not counting a wait behind an IDR in flight; capture skipped at 2 waiting for credit or 4 in the encoder (2 in flight in a Mac host's VideoToolbox) |
| Encoder cap | a rolled second is full with a full tick (a due tick skipped for 4 in the encoder) and 3 or more units inside on average (units a second × median dwell); uncapped, a full one caps at max(the viewers' presets' highest minimum, ⌊0.95 × units a second⌋) where that is below the units a second; judged by the first second loading the cap (0.9 × it or more) at least max(2, ⌈3 ÷ (units a second − cap)⌉ + 1) rolls later, at most 8, whose median dwell not below 0.6 × the capping second's lifts it, none again for 30 s, doubling with each lift in a row up to 8 min; capped, 3 loaded seconds since a full one raise it to ⌊0.95 ÷ median dwell⌋ when that is a twentieth (≥ 1) more, below the cap it last filled at unless a twentieth above that too, and from 30 s after a measurement a probe adds a twentieth (≥ 1) each loaded second, that hold doubling with each fill up to 4 min; a full second lowers it to ⌊0.95 × units a second⌋; unloaded seconds and a pace asked at or below it leave it; cleared at the asked pace; reset when the encoder is rebuilt |
| Mac encoder | low latency with QP up to 51; a dropped newest picture sent again at the next tick, at most twice a picture; dropped pictures traced at most once a second |
| Mac capture interval | (n − 0.5) / refresh: n the pace's multiple where the refresh is one within 1%, else 1 below eight times the pace and floor(refresh / pace / 4) above, at most the most refreshes that fit in 0.9 / `max_fps`, at least 1; 0.9 / `max_fps` with no refresh; a slower pace after 1 s, a faster one, a new refresh (read each second), frame-rate limit or pause at once; the longest allowed while every viewer is paused; one change in flight, a refused one failing the stream |
| Armed tick | a tick with no new picture, or whose Windows encoder has not asked for input, stays armed; ScreenCaptureKit wakes thread C, and in process so does the Windows encoder's capture thread for the next present, which it waits for at most 16 ms an `AcquireNextFrame` while armed, and the encoder's input request (asked again every 2 ms meanwhile); a repeat due, or a published picture taken 2 ms or more after it was published, waits up to 32 ms after the capture thread's arming for its answer that nothing newer waits; a capture thread not armed again (or asked again by a refinement repeat) for 2 s stops looking; through the helper the worker's capture thread and encoder wake its loop, which tells the host when its source wants the armed tick (protocol 12), and the host asks then, or 250 ms after the arming at the latest; a source that cannot push is asked again every 2 ms for 250 ms after an injected input or a new picture and while the encoder holds units (an input brings a pending poll in to 2 ms), and once a period otherwise |
| Still refinement (Windows, encoders a device run listed: none yet) | from 200 ms after the last new picture or keyframe, once a fresh unit since the picture last finished refining filled ¾ of a frame's budget, a repeat a period of the pace apart, the tick and the capture thread left armed; ends after 3 outputs at two in a row under 512 B or two that did not lower the QP, at a QP of 18 or less, or at 15 repeats of the picture (from a new picture's unit that filled its budget, keyframes of it included) or 1 s; none at a pace of 4 or less, nor after a liveness repeat until a fresh unit fills its budget again; then liveness every 250 ms |
| Capture pacing | after a dated picture, the next tick a period after its present, up to ¼ period early and, for a picture of a single present, at most 1/64 period earlier than a period after the last tick (long-run rate ≤ 64/63 × pace); a picture of several presents that waited keeps the tick a period after the last; ¾ period floor after a stall; a present over a period old counts as undated, and DXGI dates no picture presented over 1 s ago |
| Probe / unresponsive | every 1 s, every 250 ms while the base RTT is under 100 ms / after 2 s, pace 2 FPS |
| QoS | delay the median of 3; RTT window 60 s; a cut needs 2 probes in a row at ≥ 150 ms, or an ack queue ≥ 100 ms and then at most halves, and below 600 ms none while the receipts own the queue (2 s after a cut, rebase or drain); Balanced H.264 and H.265 start at the ceiling at the first probe; tick 250 ms; bits-per-frame window 3 s, stepping only without receipts; rate-model latch over the last 2 s of ticks, each at ≤ 0.75 × nominal at both ends, with ≥ 6 unique frames and non-IDR bits > max(1.3, min(0.6 × nominal / mean pace, 2)) × expected at each tick's highest per-frame budget |
| Receipt rate control | standing queue over ≥ 3 receipts spanning ≥ 2 frames and 50 ms (≤ 500 ms), less each unit's transfer (≤ a frame); busy rate the median of ≤ 8 busy pairs (≥ 3); congested above 40 ms, or 10 ms when blocked; cut to min(0.85 × busy, 0.9 × used), or 0.85 × used for stalls that recur (3 in 2 s, or 2 covering 15%); hold max(RTT + queue, 100 ms) and while used > 1.1 × target, ≤ 1 s; drain at 5 ms or 0.8 ×; rebase after 2 cuts within 1.1 × (above 40 ms, or from 10 ms after them) while used ≤ 1.1 × the last cut's target, at most 150 ms for the probes; cut floor Quality 100 000 bits a frame, others the floor; a frame's cost the encoder's measured bits per non-IDR frame over 2 ticks of ≥ 2 frames when less than its budget, or above 1.25 × it in both and not falling to 0.8 × its excess (under the timestamp model also above 1.25 × its bitrate's bits), such a cost at most halving the pace a cut; raise 15%/s, 4%/s within 0.9 × capacity, + 100 kbit/s/s, in 5% steps, with used ≥ 0.5 × expected (below, the pace cap alone), capacity cleared at 1.2 ×; recovery 50%/s toward 0.9 × the pre-congestion rate after 500 ms under 10 ms |
| Process timers | 1 ms, with power throttling opted out, while a session captures |
| IDR | host minimum interval 300 ms; viewer minimum 500 ms, backoff to 4 s; 1 s hold after a discontinuity |
| Viewer sink | 32 units, 8 MiB; backlog at 250 ms with ≥ 3 queued, drained until the IDR or a head under 125 ms; IDR grace 500 ms; stall watchdog 700 ms |
| fps cap | tick 250 ms; capacity from ≥ 4 busy ticks of the last 8; × 0.9 direct, × 0.8 relay; minimum 5; after 3 s, +5 per 30 fps of ceiling or a quarter of the cap, up to 95% of the capacity; probes of 10% watched 3 s (not by the queue within the IDR grace; a pause takes one back), backoff 3 s doubling to 60 s; turnaround late past 2 frame intervals (≥ 40 ms) over its 10 s floor; ≥ 1 s between changes but for a probe taken back; within the window's display rate (rounded, 30 to 120), a change of which goes at the next tick, and beneath which a backlog cap is held, not raised, probed or removed |
| Input | viewer queue 512 events or 256 KiB, batches of 16, a lone move replacing a queued tail move and a lone scroll adding to a queued tail scroll of its generation and continuity; host lane 512; presentation fresh for 2 s; presentations sent at most every 100 ms except ahead of input; `Wheel` at most 100 notches each way, `Scroll` (to a host that lists `wheel_hires`) 120 units a notch, at most 12 000 each way; a viewer's notch the embedder's (Windows `lines × 100 / 3` physical px cut to whole, macOS 40 and Linux 53 logical px, else 30), a touchpad's travel 1.2 units a logical px (30 px a notch toward an older host); a fling from 50 px/s, at most 8 000, on a Mac or iPad until below 20 device px/s, elsewhere for the time Android's curve computes for its speed |
| Cursor | sampled every 16 ms, or once a period of a faster pace down to 8 ms, and behind the helper also as the worker pushes a change; the shape looked at 2 ms and 8 ms after a shape-reading controller's newest move, one look pending, a sooner one an earlier move left kept; the controller's own move never reported; an injected move is the pointer until the OS shows it, at most 250 ms after the last (`SETTLE`); a report older than the controller's last move set aside; a shape read only when `cursor_shape` was negotiated, the pointer shows on the shared display and the OS pointer changed (a failed read again after 250 ms), an image over 1024 px a side never, 1 to 256 px a side sent, 64 slots on each end |
| Codec | `Accepted` waits at most 3 s for the backend's display and H.265 answer; one probe per GPU and process; encoder bitrate 256 kbit/s to 80 Mbit/s; bits per frame × (pixels / 1080p)^0.75, within 0.4 and 3; Windows H.264 in Constrained High, then High, then Baseline, `QualityVsSpeed` 0, no scenario |

### Diagnostics

`ViewerStatus.stats`, fed by the host's once-a-second statistics, defines the
numbers the UI, the acceptance tool and these docs use:

| Name | Definition |
| --- | --- |
| `rtt_ms` | The last probe round trip, transport only |
| `link` | On a direct path, the viewer tunnel's link, sampled once a second: `kind` (`lan`, `internet`, `private`), `family`, `via`, `peer`, `local_ip`, QUIC's own `rtt_ms` and `min_rtt_ms` (the network alone, rounded up), `setup_ms` and `same_lan`; null on the relay |
| `queue_delay_ms` | The median of the last three untainted probe RTTs minus the base RTT; with arrival times, of their queues toward the viewer |
| `picture_latency_ms` | Median over 2 s of host time (encoder submit to socket write) + `rtt_ms / 2` + viewer time (network receipt to compositor consumption). Each side uses its own clock and the network leg is an estimate; only a camera measures glass to glass. |
| `presented_fps` | Accepted presentations per second, duplicates included: from a Windows host whose encoder refines, it rises for a moment after a change stops, as a burst of refinement repeats is presented |
| `received_fps` | Non-duplicate video records per second |
| `preset`, `max_fps` | The preset this session asked for, and its `limits.max_fps`, which bounds every preset's ceiling, or replaces it with `fps_override` |
| `skipped_per_s` | Host capture ticks skipped per second, for transport backpressure or a full encoder, between the last two `Stats`: over at least the host's 1 s interval, however close together they arrive, and never above `max_fps` |
| `encoder_dwell_ms` | The host's median time a unit stays in its encoder, or the helper's, from submit to output as the encoder measures it, over the last second; null from a host that reports none |
| `sent_bitrate` | What the host sent over the last second, in bit/s |
| `speed_rx_bps`, `speed_tx_bps` | Speed, as RustDesk counts it: bytes per second the session's tunnel carried each way over the last second, TLS and every message included. A session over a test stream has no tunnel, and reports the video payload it received and nothing sent |
| `codec`, `width`, `height` | The codec the host answered (`h265`, `h264` or `jpeg`) and the encoded size |
| `ack_queue_ms` | The host's receipt-based queue delay (the ack queue under send credit) |
| `forward_queue_ms`, `reverse_queue_ms` | With arrival times: the receipts' queue toward this viewer (the ack queue), and the median of the newest three receipts' queues on their way back, where a busy upload at the viewer queues; null from a host that reads no arrival times |
| `present_interval` | p50/p95/p99/max of the intervals between accepted presentations over the last 60 s, with the gaps over 100 and 250 ms; a refinement burst after a change stops adds intervals of a period where the liveness repeat had 250 ms |
| `degraded` | For 3 s: queue delay ≥ 150 ms, picture latency ≥ 250 ms, or (at a pace of 10 or more) fewer than half the pace presented |

The host writes `encoder_dwell_ms` in its `Stats` only to a viewer that listed
the `encoder_dwell` capability in `Authenticate`, and `forward_queue_ms` and
`reverse_queue_ms` only to one that listed `arrival_time`: a strict viewer of
an older build rejects a field it does not know. A viewer reads its absence as
null.

Where the frame rate goes is then one comparison at a time: a capture rate
below the pace is the host's source or its gate (`skipped_per_s`,
`encoder_dwell_ms`); a pace below `max_fps` is the controller, the viewer's cap,
the host's cap at its encoder's throughput (`desktop_encoder_cap`) or the
preset's ceiling (Quality's is 30 unless the viewer chose a frame rate);
received below capture is the transport (`ack_queue_ms`); presented below
received is the viewer's decoder or display.

Thread C also traces each second it captured as `desktop_capture_pacing` on
`pb_mapper_desktop::feedback` (debug): `wait_p50_us` and `wait_p95_us`, from a
dated picture's present to the capture call that took it; `dated_share`, the
captures whose picture was dated; and per second `captures_per_s` (captures
that spent their tick), `presents_per_s` (the presents their pictures
covered), `coalesced_per_s` (those beyond one per picture: presents no
capture took), `empty_polls_per_s` (captures that left the tick armed:
they found nothing new, a Windows encoder that had not asked for input
yet, or on a Mac host the encoder's submit thread busy, or they sent a
refinement repeat) and `full_ticks_per_s` (due ticks skipped for four units
in the encoder or the helper). It traces each change of its cap at the
encoder's throughput as `desktop_encoder_cap` there: `action` (`capped`,
`kept`, `lifted`, `cleared` or `reset` at info, so the background service's
host log keeps them too; `raised` and `lowered`, which a probe brings, at
debug), `cap` (absent once there is none), `asked_fps` (the least pace the
viewers ask for, before the cap), the rolled second's `full_ticks`, `units`
and `window_ms` (none of these for `reset`, which a rebuilt encoder brings),
and `dwell_before_us`, the median dwell of the second that set the cap,
with, for `kept` and `lifted`, `dwell_after_us`, that of the second the
guard judged by. Behind the Windows helper the dates come from the
worker's answers, and the app's link to the helper traces
`desktop_helper_push` there, once a second while records come:
`replies_per_s` (answers, so the pipe's round trips), `units_per_s`,
`fresh_per_s` and `pointers_per_s` (the worker's pushes), `refused` (pushed
pointers outside the display), `receipt_p50_us`, `receipt_p95_us` and
`receipt_max_us`, from the worker's collection of each pushed unit to its
receipt in the app, on the system clock both read, and `fresh_age_p50_us`
and `fresh_age_p95_us`, a fresh picture's age as the worker wrote its
notice. A
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
busy and left their tick armed) and `peak_in_flight`. A Mac host logs each
stream it opens as `macos_capture_opened` on `pb_mapper_desktop::session`
(info): its `width` and `height`, the frame it codes them in
(`coded_width` and `coded_height`, larger where it pads the size to a
multiple of 16), the display's `refresh_hz` (the mode's
`mode_hz`, else the display link's `link_period_us`), the `pace_fps` and
`max_fps` it opened at, and the interval it started at: `n` refreshes
(none with no refresh), `interval_us`, `paced` (whether `n` is the pace's
own multiple, so that pictures come at the pace), and, only then,
`drift_ppm`, how much faster `n` display-link periods are than thread C's
period at that pace, in parts per million (positive: a display that
drifted ahead of an even grid). Each change of the interval is
`macos_capture_interval` there (debug), with `interval_us`, `n` and
`paused`, and a change ScreenCaptureKit refuses is
`macos_capture_interval_refused` (warn, with `interval_us` and `detail`),
after which the stream fails and thread C opens another. Its first picture gives
`macos_capture_started` there (debug): `lookup_us`, finding the display;
`start_us`, the rest of the start; and `first_picture_us`, from the
start's beginning to the first complete picture. It also logs the
first picture of each stream as `macos_capture_layout` on
`pb_mapper_desktop::session` (debug): its width and height, the pixels it
says it extends by on each side (`extended_left`, `extended_right`,
`extended_top`, `extended_bottom`), each plane's rows and bytes per row
(`luma_rows`, `luma_bytes_per_row`, `chroma_rows`,
`chroma_bytes_per_row`) and its IOSurface's size (`surface_bytes`), which
say whether ScreenCaptureKit's pictures extend to a multiple of 16, as
VideoToolbox then reads them, encoding them inside its call; a padded
frame's picture has its coded size. The ignored `sck_padded_capture` test
(`platform/macos/tests.rs`) reads the same of the main display's pictures
at 3024x1964, 2940x1912 and 1920x1080, unpadded and padded, checks that
a padded picture's padding is black, and times VideoToolbox's encode call
on each. A Windows encoder's capture thread logs its first acquire as
`windows_capture_first_acquire` on `pb_mapper_desktop::session` (debug):
`last_present_time`, `accumulated_frames` and whether a desktop image came
with it, which the encoder also keeps in its statistics for `video_bench`'s
`first_acquire_*`, with the thread's `AcquireNextFrame` calls
(`acquires`). Dropping the encoder logs how long its capture thread took to
join (`desktop_capture_thread_joined`, debug: `join_us`), or warns
`desktop_capture_thread_stuck` when it did not end within 100 ms and was
left behind. A session's encoder logs, as it starts, whether its
events are pushed (`windows_encoder_events`, debug: `encoder`, `pushed`;
the H.265 probe's encoder, which nothing waits on, logs nothing), and
warns `windows_encoder_watcher_failed` when its watcher thread could not
start (the events are then polled) and `windows_encoder_watcher_stuck`
when its shutdown did not end the watcher's wait within 100 ms (the thread
is left behind, and later encoders poll their events). Its statistics,
which `video_bench` prints, count the submits that found no input request
(`waiting`) and, while its events are pushed, how long each output waited
from its announcement to the start of the drain's call that collected it
(`have_output_to_collect_us`). The app's own host writes these debug events when
`RUST_LOG` enables their targets; the background service's log keeps its
targets at info, and its capture helper logs nothing.

The host traces each second as `desktop_feedback_interval` on
`pb_mapper_desktop::feedback` (debug): `action`, `fps`, `pace_fps` (within
the encoder's cap), `viewer_cap`, `encoder_cap` (the cap at the encoder's
throughput, as the session's QoS last read it), `paused`, `unresponsive`,
`bpf`, `bitrate`, `encoder_bitrate`,
`sent_bitrate`, `rate_model`, `rtt_ms`, `baseline_ms`, `delay_ms`, `tainted`,
`ack_queue_ms`, `forward_queue_ms` and `reverse_queue_ms` (with arrival
times), `rate_fps`, `rebase_ms`, `standing_ms`, `busy_bps`, `used_bps`,
`cuts`, `episode`, `window_frames`, `window_bytes`, `window_clamped`,
`outstanding_frames`, `outstanding_bytes`, `idr_inflight`, `skipped_ticks`,
`encoder_dwell_ms`, `keyframes`, `gop`, `input_wait_p50_us`,
`input_wait_p99_us` and
`input_wait_max_us`. The presentation-credit fields `quality`, `waiting`,
`recovering`, `desired_window_bytes` and `last_ack_*` are gone, and so is the
shadow estimator's `would_cut`, now that its cuts are made (`cuts`).

`rate_fps`, `rebase_ms`, `standing_ms`, `busy_bps`, `used_bps`, `cuts` and
`episode` come from the receipt-driven rate control (see
[QoS](#qos-frame-rate-and-bits-per-frame)). It keeps the receipts of H.264 and
H.265 video from the second up to the newest one, at most 240, each with the
delay of the newest unit it covered and the bytes it covered, and forgets them
on a path change, a preset change and a resume. A frame interval is one at the
current pace. With arrival times it times each receipt by the viewer's stamp
instead of its arrival at the host, and a receipt's delay is its forward
queue, so neither the way back nor what bunches receipts on it is in any
figure below; a receipt whose stamp was no sample is left out.

| Field | Definition |
| --- | --- |
| `rate_fps` | The pace cap a receipt cut set below the preset's cut floor, from the delivery rate at that floor; absent without one |
| `rebase_ms` | What a rebase added to the probes' baseline, at most 150 ms; absent without one |
| `standing_ms` | The least receipt delay above the baseline over the newest receipts, each less its unit's own transfer at the busy rate (at most a frame interval; a frame interval without a busy rate): at least three, spanning at least two frame intervals and 50 ms (at most 500 ms), so a burst a few milliseconds long never makes a queue on its own |
| `busy_bps` | The median delivery rate (of two, the lower) over the newest eight receipt pairs across which the link stayed busy, the later unit's delay above the baseline being at least the pair's gap; none below three. Receipts less than a quarter of a frame interval apart are compression, not delivery: such a pair is no sample, and its later receipt's bytes count toward the pair before it |
| `used_bps` | What the receipts of the second up to the newest covered, from the first of them to the last; none over less than 100 ms. For up to a second after the last receipt the trace repeats it, so a stall shows as a `used_bps` that stops changing and then disappears, not as one that falls |
| `cuts` | The receipt cuts since the session began |
| `episode` | A receipt episode is under way: from a cut until the standing queue is under 10 ms |

Without a receipt in the last second `standing_ms`, `busy_bps` and `used_bps`
are absent, and without a base RTT the first two are.

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
when an H.265 encoder fails and H.265 is turned off; one that shares
through the Windows helper logs each H.265 answer it gives a session as
`desktop_h265_helper` (usable, named, kept, ms). A Windows host logs
each encoder it opens as `desktop_video_encoder_opened`: `encoder`,
`codec`, the adapter's PCI `vendor` (0x10de NVIDIA, 0x8086 Intel, 0x1002
AMD), `width`, `height`, `fps`, `gop`, and what the encoder read back:
`rate_control` (0 is CBR), `quality_vs_speed`, `scenario` (1 is display
remoting), `b_frames` (none from one that does not implement the
property), `cabac`, `profile` (the output type's: 257 Constrained High, 100
High, 66 Baseline, H.265's 1 Main), the SPS's `sps_profile`, and
`sps_reorder`, what the SPS said of reordering and what is sent: `0`,
`unstated_rewritten_to_0`, `<n>_rewritten_to_0`, `unread`, or `unknown`
before its media type carried an SPS; `sps`, that SPS in hex as the
encoder wrote it, empty before one; `gpu_priority`; and `refine`, whether
it refines a still picture with a burst of repeats. An encoder whose
media type carried no SPS logs `desktop_video_encoder_sps` (`encoder`,
`sps_profile`, `sps_reorder`, `sps`) once, at the first it writes in band.
The H.265 probe's encoder logs neither. It warns
`desktop_h264_high_failed` (`encoder`, `detail`: what failed) when an
H.264 encoder built in a High profile failed in it and H.264 is Baseline
for the rest of the process, and logs `desktop_gpu_priority` (whether the
privilege to raise a base priority was enabled and, if not, the error;
whether the device took the priority and, if not, the error; the
scheduling class before, and whether it was raised) once per process, when
an encoder first asks for it. These come from the process that owns the
encoder: behind the helper
that is its capture worker, which logs nothing, so `service-host.log` does
not have them. A Windows host warns
`desktop_moves_placed` (win32: the error of a `SendInput` that refused a
move, none when moves were dropped; token: the session, integrity level and
UIAccess of its process) when `SendInput` refused a move or its injected
moves stopped taking effect, and it places the pointer instead while that
window stays in front
([Pointer moves on Windows](#pointer-moves-on-windows)). A Mac host logs each
encoder it opens as `macos_encoder_opened` (codec, `low_latency`, width,
height, `coded_width`, `coded_height`, fps, hardware, gop,
`data_rate_limits`: whether VideoToolbox took
the cap of 1.5 × the mean over 1 s, and `max_qp`: whether a low-latency
session took the QP bound of 51), after
`macos_encoder_low_latency_refused` when it retried without low-latency rate
control. A new frame-rate limit the encoder keeps running for, which the
session refuses as its expected frame rate, is
`macos_encoder_fps_refused` (debug: fps, detail), and the session keeps the
rate it had. It traces the pictures VideoToolbox dropped as
`macos_encoder_dropped` (debug): `dropped` since the last line, over
`seconds`, and the encoder's `total`, at most once a second and only when
there were any. It warns `macos_encoder_left_running` (`waited_ms`) when an
encoder's submit thread was still inside a VideoToolbox call 250 ms after
the encoder was dropped, and was left to finish on its own, and
`macos_encoder_crop_failed` (codec, width, height, `coded_width`,
`coded_height`) when a padded session's SPS would not crop to its content,
after which the host pads no more. The app also warns
`desktop_connect_failed` with the typed failure, and a native decoder's
failure is warned as `desktop_decoder_error` on `pb_mapper_desktop::frames`
with its text as the detail. Each line names itself in an `event` field, and
free text is Debug-formatted so that a newline in it cannot split the line.
No line carries a token, invitation or fingerprint.

The background service writes its host's lines to `service-host.log`, beside
`service.log` in the protected installation root (SYSTEM and administrators
write it, users may read it): `pb_mapper_desktop::session`,
`pb_mapper_desktop::feedback` and `pb_mapper_client::direct` at info, which
are the session lifecycle above, `desktop_rate_model_latched`,
`desktop_start_rate_remembered`, and `session_path_selected` with the path,
the service, the peer address and the congestion controller the host's end
sends under (`congestion`). No other target is written; none of these
logs an invitation, a key or a credential. The once-a-second
`desktop_feedback_interval` is debug and is not written; the viewer's
statistics carry those numbers. At 1 MiB the file becomes
`service-host.log.1`, replacing the previous one.

### The quality panel

The viewer's connection-quality panel (the toolbar's chip, or the path and
frame-rate button in a phone's gutter) shows these figures in four groups,
and scrolls within a landscape phone's height. A row whose figure is not
reported, by an older host, on the relay or on a platform without it, is
left out; while the connection is down the core rows show dashes. Above the
groups, while the session goes without a decoder an install would add, it
shows that install hint (see "What the viewer decodes").

- **Network**: the path (LAN direct or direct, with the address family, from
  `link.kind` and `link.family`; or the relay and its carrier), the QUIC
  link's own round trip (`link.rtt_ms`, with its lowest) beside the stream's
  in-band one (`rtt_ms`, with its queueing), from a host that reads this
  device's arrival times the queue to this device with any on the way back
  (`forward_queue_ms`, `reverse_queue_ms`: an upload here queues on the way
  back without slowing the picture), Speed each way, and the bitrate received
  and, from the host, sent.
- **Picture**: the picture delay with its host and viewer ends, the rate
  presented, received and captured by the host, the encoded resolution, the
  codec (H.265, H.264 or JPEG) with its chroma and, under it while connected,
  the name of the decoder the viewer reported (a Linux viewer's names the
  render node it decodes on, a Windows viewer's its adapter), and keyframes
  with those the viewer asked for.
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
    its copy), the refresh of the display the viewer window is on, exactly
    (59.94, the rate the viewer follows), and low latency only where the
    decoder's codec API confirms it. Its decoder thread's CPU comes as
    cycles, which the panel does not show.
  - macOS and iOS: the counts, the decode time (VideoToolbox decoding a unit,
    keyframes included), the release-to-consume mean, the decoder thread's
    CPU as a share of one core, VideoToolbox's real-time mode as low latency,
    and the refresh of the display the viewer window is on (macOS) or the
    device's (iOS). A Mac app built with the layer presenter
    (`--dart-define=presenter=layer`) shows the present time (from the end
    of a picture's decode to the end of its commit to the WindowServer) in
    place of release to consume; its dropped pictures include, while the
    panel is open, those committed beyond one for each refresh of the
    display, which no refresh showed.
  - Linux: the counts, the release-to-consume mean and the refresh of the
    display the viewer window is on (the rate the viewer follows).

  The Mac's decode time and Windows' convert time measure different spans,
  so each has its own row. A present time (`timings.present`) gets a row
  too, for a runner that presents pictures itself: the Mac's layer
  presenter, when built in. Closing the panel drops the decoder's timing
  accumulators and its CPU reading.

Read in order, the rates place a loss: capture below pace is the host's
source or its capture gate; pace below the ceiling is the QoS controller
(its startup hold included), the viewer's cap or the host's cap at its
encoder's throughput; received below capture is
the transport; presented below received is this device's presenter. The
Mac's layer presenter, when built in, presents every picture it decodes,
so there the decoder's dropped figure, not the presented rate, shows the
pictures the display had no refresh for.

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
handed to a `FlutterTexture` (a Mac app built with
`--dart-define=presenter=layer` instead has its decoder thread commit the
decoder's own 420v pictures to the viewer's own Core Animation layer, shown
through a platform view), and Linux with GStreamer into the decoder's
own NV12 or I420 pictures, which the runner converts to RGBA on the GPU into
an `FlTextureGL` (a Linux app built with `--dart-define=presenter=plane`
instead has the GTK thread convert them and draw them on a GL plane under
the Flutter view, where the viewer leaves a hole, whenever GTK draws the
window; [the Linux video plane](#the-linux-video-plane)).
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
directly, when the compositor took the picture, or, with the Mac's layer
presenter, when the decoder thread committed it to the WindowServer, or,
with the Linux plane, when the GTK thread took and converted it. Dart
reads only status, geometry, statistics and the latest cursor, through the
per-frame view leaf.
A device with no decoder, Windows without Media Foundation and Linux without
GStreamer's decoders included, takes JPEG, which Dart decodes off the UI
isolate. Video bytes never become JSON or base64.

### What the viewer decodes

Before it connects, the app asks the native side through the
`pb_mapper/desktop_video` channel's `decoders` method. The method takes no
arguments and never creates a decoder, because a bad driver can hang there. It
answers a map: `h264` and `h265`, each either `{max_width, max_height}` (the
largest of the viewer's resolution bounds that codec decodes at) or null,
`display_max_hz`, the fastest refresh among the displays as they are set now,
and, from the Windows and Linux runners, `window_refresh: true`: the runner
answers `refresh_hz` and calls `display_changed` (see the fps cap's
display rule below). The Windows runner also answers `installable`, the
codecs this computer's GPU decodes that no installed decoder takes (see the
install hints below).
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
decoder configuration for. Where the adapter has `HEVC_VLD_MAIN` but no
decoder MFT is registered, `installable` lists `hevc`. The refresh comes from
the active display paths as
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

On Linux the module looks in GStreamer's registry and sysfs and builds
nothing: H.264 is offered when any decoder factory for it exists, a hardware
one (VA-API, NVDEC, V4L2) at the largest viewer bound its caps allow, a
software one at 1920×1080; H.265 only with a hardware decoder, as on Windows
and Android. The decoder it names is the first a session would try, one on
the GPU that draws the window ahead of one on another GPU (see the Linux
viewer below).
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

**Install hints.** Where an install would add a decoder, the viewer says so
at the top of the quality panel and under Display → Codec, with **Don't show
again**, which `ViewerPrefs` keeps per device (`viewer_decoder_hints_dismissed`).
Dart reads what is missing from the `decoders` answer
(`DesktopDecoders.installable` and `missing`, `installHint`):

- Windows, `installable` with `hevc`, on an H.264 session with the codec on
  Auto: the HEVC Video Extensions, Microsoft's paid Store app, whose button
  opens Microsoft Store product `9NMZLZ57R3T7`: in the Store app
  (`ms-windows-store://pdp/?ProductId=…`) where its scheme is registered
  (`canLaunchUrl`, which reads the registry and opens nothing; Windows may
  answer a scheme nothing handles with its own dialog rather than fail),
  else on the Store's web page. A PC whose maker included the extensions has
  their MFT and no hint. Installed, their MFT is what the probe finds once
  the app runs again (the store probes once a run).
- Linux, from an answer the runner gave (not a failed probe, nor under the
  software renderer, nor from an app built without its decoder module, the
  bundle's `lib/libpb_mapper_desktop_video.so`, where an install changes
  nothing): no `h264` is GStreamer's decoders missing, hinted on a JPEG
  session under either codec preference, with the command that installs the
  user guide's "Always" and "Software decoding" rows on Debian and Ubuntu,
  Fedora or Arch, read from `os-release` (`ID`, then `ID_LIKE`); no `h265`
  is no hardware decoder for it, hinted on an H.264 session on Auto, whose
  line names the video driver and, on Intel and AMD before GStreamer 1.24,
  GStreamer's VA-API plugin. Both link to the guide's package table, and the
  module, like the store, looks once a run.
- An H.265 decoder that failed and was dropped for the run is not missing.

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
`refreshHz`, the exact refresh of the display Flutter's view is on, as
`refresh_hz` answers it (the fastest display's, read as `decoders` reads it,
where that says nothing), and while a decoder thread exists `decoderCycles` (its CPU cycles since it
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
`CMBlockBuffer` and `CMSampleBuffer` VideoToolbox requires. A session the
system takes back (`kVTInvalidSessionErr`, `kVTVideoDecoderNotAvailableNowErr`,
as when iOS backgrounds the app) is rebuilt at the next keyframe and does not
count as a failure. Log lines start with `PbDesktopVideo:`.

How a picture reaches the screen is the viewer's presenter, which the
channel's `create` takes as `presenter`. iOS and the Mac take a texture:
VideoToolbox decodes into BGRA, and the newest picture waits for the
compositor, which takes it in `copyPixelBuffer`: the first time it takes a
picture of the current revision is the presentation receipt, and a picture
replaced before that counts as dropped. A Mac app built with
`--dart-define=presenter=layer` takes the layer presenter instead, which
stays opt-in until a run with the display on has measured its time to the
screen and checked it there
([the macOS viewer procedure](../scripts/test/desktop-ui-acceptance.md#macos-viewer)).
VideoToolbox decodes into its own 420v (NV12, video range), which skips its
conversion pass to BGRA, and the decoder thread commits each picture of the
current revision to the viewer's own Core Animation layer as soon as it is
decoded, in an explicit transaction of its own: no hop to the main thread
and no Flutter frame.
That commit to the WindowServer is the presentation receipt, so every
picture decoded earns one. The layer keeps the last three pictures out of
VideoToolbox's pool, since the WindowServer may still composite the earlier
ones. At configuration the format description is tagged sRGB transfer,
BT.709 primaries and the stream's matrix (BT.709 when it names none),
whatever the VUI says: every pb-mapper host puts sRGB values through the
BT.709 matrix without converting their transfer, and Flutter shows the BGRA
conversion's values as sRGB, so Core Animation shows the texture path's
colours, within 2 levels on eight test patches rendered offscreen, where a
BT.709 transfer would lift a mid-grey from 128 to 139
([measurements](benchmarks/2026-11-24-mac-viewer-layer/README.md)).
VideoToolbox carries the tags onto each picture's IOSurface; should the
first picture of a decoder lack them, the runner tags every picture itself
and logs it once. A snapshot takes the picture committed last, whose tags
also give the thumbnail the picture's colours: the texture's BGRA, tagged
BT.709, comes out up to 11 levels lighter through the same conversion.

Flutter shows the layer through the platform view
`pb_mapper/desktop_video_view`: an `AppKitView` made with the texture's id,
in the texture's place, which Flutter clips and transforms as it did the
texture, so pinch, pan and zoom stay Flutter's, and above which Flutter
draws the cursor and the overlays. The view's own layer hosts the
presenter's, which moves to a new view should Flutter make one. The view
takes no keyboard focus and handles no pointer event, so that clicks,
scrolls and gestures, the first click into an inactive window included, go
up the responder chain to the Flutter view. Over the picture the view, not
the Flutter view, is the one AppKit sends a cursor update, as when a menu
closes or the app comes back to the front, and the Flutter view restores
its own cursor only where it is that view itself; so the view sets the
cursor the Flutter view was told last, Flutter's or the pointer plugin's
(read from the engine's `_lastCursor`, and left as it is should an engine
keep none). The reconnect's grey still, which no Flutter filter reaches
there, is the layer's own: the channel's `grey` shows a colour-blend layer,
which takes the picture's luminance, and a black one at half opacity, which
halves it. Rendered offscreen they come within 9 levels of the Flutter
filter's grey, whose BT.709 luma weights the blend mode does not use. While
a source streams to the view on screen, a display link asks, from macOS 14,
a display whose refresh varies, such as a ProMotion panel, for its fastest
rate, which Flutter's own frames no longer ask for; while the stats ask for
timings it also counts as dropped the pictures committed beyond one for
each refresh since its last callback, as pictures no refresh showed (a
CVDisplayLink counts before macOS 14). It counts refreshes, not callbacks,
because the NSView display link calls back on the main thread, which
Flutter's UI thread shares: a busy one misses callbacks, not refreshes. A
display of fixed refresh with the timings off, and one whose refresh varies
before macOS 14 with the timings off, run no link. None of the view's
behaviour on a lit screen has been seen yet: the picture's way up and
colours there, the grey still as the WindowServer composites it, the cursor,
clicks, scrolls and gestures through the responder chain, live resizing and
a ProMotion panel are what the run with the display on checks.

With the layer presenter, the Display menu also offers **Sharpen enlarged
picture**: Off, Low, Medium or High, one choice for the device
(`viewer_enhance` in the viewer's preferences), Off until chosen, offered
only for a picture the runner presents itself and sent at once as the
channel's `enhance {textureId, level}`. The Linux video plane takes the
same choice ([its scaling](#the-linux-video-plane)). It follows airhand's enhancement
(`client/src/lib/enhance.ts`). While the level is not Off and the picture
shows on screen at least its own size on both axes, but no larger than its
screen, the decoder thread hands each picture to a presenter thread of its
own through a slot of one, the newest winning, and returns at once: it never
waits for the GPU or a drawable. The whole picture is drawn, not the part a
zoom shows, so a picture that fills the screen goes back to the content
layer as soon as it is zoomed in. The size on screen is the view's layer
bounds in the window's root layer, which takes in the sublayer transform
that Flutter's zoom puts on the view's container, times the backing scale,
with each edge moved to the nearest edge between device pixels. Flutter
places the view at fractional pixels; the metal layer takes that rect on
whole pixels and its drawable exactly that many pixels, so the WindowServer
composites it one texel a pixel and does not filter, and blur, it again.
While the level is not Off, the view measures it again as Flutter sets its
frame, which Flutter does each frame it composites the view in, once it
placed the view's ancestors; as the zoom transform changes, which it
observes; in `layout()`, which is where a view new in the window has joined
its layer tree; and as its backing scale, window or occlusion change. The
metal layer moves at once, in the transaction Flutter moves the view in;
the size and the switch between layers follow in the platform thread's next
turn, so that a frame that sets the view's frame before its zoom is acted
on only once. The presenter thread wraps the decoder's
420v planes for Metal without a copy (`CVMetalTextureCache`). A first pass,
skipped at 1:1, upscales luma by Catmull-Rom from nine bilinear taps, kept
within its four nearest texels so that it never rings, takes chroma
bilinear, and converts through the picture's matrix with no transfer
conversion, as the content layer shows it. A second pass sharpens luma
alone, as AMD FidelityFX CAS does, with airhand's peaks (-0.06, -0.1,
-0.14): less where the 3×3 around a pixel already has contrast, which
limits overshoot beside an edge but does not prevent it, and never chroma,
which 4:2:0 carries at half the resolution. It draws into a `CAMetalLayer`
(BGRA8, sRGB, three drawables, the size on screen) beside the content
layer: the presenter thread takes a drawable first, which may wait, and
only then the newest picture, and presents in a transaction once the GPU
has scheduled the work. That present is the picture's receipt, and
`timings.present` runs to it, unless the activation ended meanwhile (the
layer is hidden by then) or the picture is no newer than the last one
acknowledged, which Rust's sink would reject anyway; a picture replaced in
the slot before it was drawn counts as dropped. Until its first present the
decoder thread commits each picture to the content layer too, so the
picture keeps moving while the shaders compile (110 ms the first time on
the M6). The metal layer then takes the content layer's place, in one
platform-thread transaction, and pictures go to it alone; when the
enhancement stops the content layer takes the newest picture in the
transaction that shows it again, so the picture never blanks. Each word
from the presenter thread names the enhancement it came from, so one from
an enhancement released since changes nothing. The presenter thread keeps
the last picture and draws it again when the size on screen or the level
changes, though the host sends nothing while the desktop is still. A
picture shown smaller than its own size, a window not visible, three
pictures the GPU failed in a row, two seconds without a drawable or no
Metal device leave the picture to the content layer; at Off none of it
exists, neither the thread, nor any GPU state, nor the watch on the view.
The shaders compile from source at run time, so neither runner project
carries a `.metal` file. A snapshot still takes the decoded picture. While
the metal layer shows the picture, `stats` adds `enhanced` (`level`,
`width`, `height`). Offscreen on an M6 with the runner's own code, both
passes took 0.52 ms of GPU time a picture for 1080p shown at 3024×1701 and
0.94 ms at 4096×2304, the sharpening alone 0.08 ms for 1080p at 1:1, the
same at each level; at 1:1 with no sharpening the colours matched the
content layer's exactly on eight patches, and over text the mean luma
gradient rose 10 to 14 % over the content layer's linear filter from the
upscale alone and 15 to 20 % at High, while a 16 to 235 edge reached 13 to
237 at Low and 9 to 241 at High; in a window, the metal layer's frame
landed on whole pixels for a view at fractional points, moved and zoomed
([measurements](benchmarks/2026-11-24-mac-viewer-enhance/README.md)). The
picture on a lit screen, its time to the glass, a letterboxed picture whose
edge falls on a half pixel, a pinch, a pan or a resize while the desktop
is still, and the WindowServer's work with the metal layer are what the run
with the display on checks.

While the stats ask for timings, the runner marks Instruments points of
interest (subsystem `pb-mapper.desktop-video`), each with the picture's
generation and frame: `Decoded` as a picture leaves the decoder, `Commit`
around the layer presenter's commit or its enhancement's present,
`Enhance` from the enhancement encoding its GPU work to the GPU scheduling
it, and `Consumed` when Flutter's compositor takes a texture's picture. They go to the system log whether or
not a trace records them, hence only while timed. Animation Hitches records
them once the Points of Interest instrument is added
(`xcrun xctrace record --template 'Animation Hitches' --instrument 'Points of Interest'`),
which puts them in one trace with the display's frames.

The `stats` method of the `pb_mapper/desktop_video` channel reports, besides
the counts, `lowLatency` (VideoToolbox took real-time mode), `hardware` once
a decoder is configured, and `refreshHz`. On macOS that is the refresh of the
display the Flutter window is on, read at each call so that it follows the
window to another display, or the fastest display's while the window is on
none; on iOS it is the device's. While a read asks for timings it adds
`timings.decode` (`VTDecompressionSessionDecodeFrame`, keyframes included)
and, for a texture, `timings.surface_consume` (from a picture's publication
to the compositor taking it) or, for the layer presenter, `timings.present`
(from the end of a picture's decode to the end of its commit), each
`{samples, mean_ms, max_ms}` since the timings
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
share the C interface `desktop_video_module.h`, whose ABI number, 3, rejects
a stale module). Each source's decoder thread pulls straight into pooled
`GstBuffer`s (one copy of the compressed data) and feeds `appsrc →
h264parse/h265parse → decoder → appsink`, pulling the next unit only while
fewer than four are inside the pipeline, so a backlog stays in Rust's sink,
which drops to an IDR. Decoders are chosen from the registry, hardware first
(VA-API, NVDEC, V4L2), and among those the ones on the GPU that draws the
window first, then those whose GPU is unknown, then those on another GPU;
then by rank. One that fails to start is skipped,
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
`{samples, mean_ms, max_ms}`, and always `refreshHz` (the refresh of the
monitor the view's window is on, as `refresh_hz` answers it, or the fastest
monitor's, read as `decoders` reads it, where GDK says nothing of it),
`dmabuf` (pictures arrive as dma-bufs) and `videoconvert` (the pipeline
converts). Without timings `frame_ready` reads
no clock. `G_MESSAGES_DEBUG=pb-mapper-video` shows the module's log,
including the caps each decoder gives, so whether dma-bufs were negotiated;
at each probe the GPU that draws (`Presenting GPU`) and every render node
with its driver; and, for each probe and decoder start, a codec's decoders
in the order they are tried, each with where it decodes;
`GST_DEBUG=3` shows GStreamer's. A viewer needs GStreamer's base and
bad plugins and a decoder: gst-libav (or openh264 for H.264) in software,
the VA-API driver (before GStreamer 1.24 with gstreamer-vaapi, since the va
plugin's decoders have no rank there), or NVIDIA's driver; the user guide
lists the packages.
The release bundles no GStreamer library: `scripts/build/check-linux-bundle.sh`
fails a bundle without the module, with a GStreamer or ORC library inside, or
whose FFI or module does not load.

On the GTK thread, before the probe and before each decoder starts, the
runner asks Flutter's EGL display which GPU it renders on
(`EGL_EXT_device_query`): the device's render node
(`EGL_EXT_device_drm_render_node`), else its primary node
(`EGL_EXT_device_drm`), which sysfs maps to its render node. The module
places each hardware decoder from its factory's name and sysfs only, so
placing them loads no plugin and opens no device, which would wake a GPU
that sleeps:
- A codec's va factory on every GPU but one carries its render node in its
  name (`varenderD129h264dec`). The plain one (`vah264dec`) is on a GPU the va
  plugin may open (one this process may open, with any VA driver before
  GStreamer 1.24, and from 1.24 with Intel's or Mesa's, NVIDIA's only under
  `GST_VA_ALL_DRIVERS`) that has no factory of its own for the codec. Which
  GPU has the plain name changed with GStreamer's version: 1.20 gives it to
  the first GPU, by device file, that decodes the codec, and 1.24 and later
  to the first the plugin opens, by device file, so there it is also before
  every GPU with a named factory; 1.22 gives it to the first the plugin
  opened in udev's order, which follows the PCI devices instead, so there,
  as for a version the module does not know, nothing narrows it further.
  When more than one GPU could have it, its GPU is unknown, though it is
  another GPU's when the one that draws cannot be among them.
- NVDEC (nvcodec) runs on NVIDIA's driver alone: with one GPU on it, it is
  that one, and it is never a GPU another driver runs. Which of several
  NVIDIA GPUs a factory is on takes CUDA's numbering, and asking CUDA would
  wake them all, so with several it is unknown.
- gstreamer-vaapi's decoders use the display server's GPU, which is the one
  that draws unless the app is offloaded to another (`DRI_PRIME`,
  `prime-run`); theirs, like V4L2's and any other plugin's, is unknown.

So on a hybrid laptop whose integrated GPU draws the window, VA-API there goes
before NVDEC, so that the NVIDIA GPU can stay asleep and every picture stays
on the GPU that shows it, where its dma-bufs can be imported (not yet tried
on such a laptop); NVDEC stays the fallback before software. An app
offloaded to the NVIDIA GPU takes NVDEC first, and on two VA GPUs the one
that draws takes its own factory. Before GStreamer 1.24 (Ubuntu 22.04's
1.20, Debian 12's 1.22) the va plugin registers its decoders with no rank,
so they are no candidates, and VA-API on the GPU that draws is
gstreamer-vaapi's (`gstreamer1.0-vaapi`), whose GPU is unknown but which
still goes before NVDEC on another GPU; without that package such a laptop
still decodes with NVDEC, as before. With no GPU known (an EGL without these
extensions, a software renderer such as llvmpipe) the order is hardware,
then rank, as before. The module itself opens no device, but GStreamer
does when it loads a plugin: the va plugin opens every render node it finds
when its first decoder is built, and a registry scan (the first run, or
after a plugin changes) loads each plugin it scans; whether that wakes a
sleeping GPU for a moment has not been measured. A decoder's name, which
the session reports to the quality panel, says where it runs:
`vah264dec on renderD128`, or
`nvh264dec (NVDEC H.264 Decoder) on renderD129, another GPU`. It leaves out
the factory's long name when the whole would pass 63 bytes, as with a va
decoder, whose long name names its driver's device.

#### The Linux video plane

Flutter's Linux embedder (3.44) has no vsync source: it frames on a fixed
1/60 s timer. A texture's picture therefore waits for that timer, which runs
unsynchronised with the display, then for Flutter's raster, `populate` and
composite, then for GTK's frame clock; a 120 or 144 Hz monitor shows at most
60 pictures a second, and on X11 every Flutter frame reads the whole window
back. A Linux app built with `--dart-define=presenter=plane` asks `create`
for the video plane instead (`ui/linux/runner/desktop_video_plane.cc`). It
stays opt-in until a Linux device has measured it
([the Linux viewer procedure](../scripts/test/desktop-ui-acceptance.md#linux-viewer)).

The runner puts the Flutter view in a `GtkOverlay` over a `GtkGLArea`
(`PbPlaneArea`, opaque, rendering at every draw), which stays hidden.
`create` answers `{textureId, plane}`. The first texture that asks gets the
plane, if its GL area realizes without error in GL ES or in desktop GL 3.2
or later and builds its program; never under `FLUTTER_LINUX_RENDERER=software`.
A second texture, and every refusal, stays on Flutter's texture.

A plane texture's `frame_ready` never marks a Flutter frame. It flags the
picture and, once for each picture that waits, wakes the GTK thread at high
priority, which queues a draw of the placed rectangle. GDK's frame clock
then renders the area. The compositor paces that clock at the display's
refresh on Wayland and on an X11 desktop whose compositor reports its frames
(`_NET_WM_FRAME_DRAWN`, as GNOME's does); on any other X11 desktop GDK paces
it at its default 1/60 s, so the plane draws at most about 60 pictures a
second there, though a picture still skips Flutter's timer and the window's
read-back. More than 60 a second also needs a session that sends them:
Smooth, or a frame-rate limit above 60 (Balanced's ceiling is 60). A waiting
picture is taken and converted in GDK's context (the converter above, in
GLSL 1.50 with R8 and RG8 planes and a vertex array object of its own on
desktop GL), which is its receipt, and drawn into the placed rectangle, cut
to its clip, through the filter its scale calls for (below). A redraw that
Flutter or an overlay causes takes
nothing and draws the last picture again. On Wayland GDK's context is EGL on
the display Flutter uses, so dma-bufs are imported as for the texture; on
X11 GDK 3 draws through GLX, so the decoder is told this presenter imports
none, and every picture comes in system memory. A picture that no frame
takes within 100 ms, as when the compositor stops the frames of a covered
window or one on another workspace, is taken by a timer and kept for the
next frame, so its receipt still comes well inside Rust's 700 ms stall
watchdog.

The plane shows, and the view's background turns fully transparent (which
FlView then does not paint), only while a viewer has placed a visible
picture; both go back when it hides the plane or disposes the texture, and
outside a session nothing changes. The GL area's input window is hidden, so
every pointer event still reaches the Flutter view.

The viewer draws a `NativeVideoHole` in the texture's place: a repaint
boundary, never raster-cached, that paints its rectangle with
`BlendMode.clear`, which clears the ground painted under it down to the
window; the cursors and overlays paint over it as before. At each composite
it works out from the layers above it where it lands, in physical pixels
and cut to their clips, and when that changed it sends
`place {textureId, visible, x, y, w, h, clipX, clipY, clipW, clipH}` after
the frame. `visible` is false when the hole was not composited (a covering
route, or the stage gone), or lies under an opacity, a filter or a save
layer (a route transition), where no hole reaches the window, or under a
rotation, which the plane cannot follow; the video does not show during a
route transition. The placement reaches the GTK thread after Flutter's
frame, so while zooming or panning the picture can trail the overlays by a
frame. `grey` puts the plane through the grey still's matrix, and the grey
still asks for it as for the Mac's layer.

The plane scales the converted picture (RGBA at its own size) into the
placed rectangle by the scale s from the picture's pixels to the screen's
(`PlanPlane` in `desktop_video_gl.cc`), drawing only the part in view, and
`stats` names the filter its last draw used as `planeFilter`:

- **Smaller on either axis** (`area`): always, whatever the level, an area
  filter, so that text shrunk into a window does not alias as bilinear's
  skipped pixels did: nine bilinear taps over the pixels each screen pixel
  covers, averaged, spread so that together they weigh the picture as the
  exact area average does (sqrt((1/s² − 1)/8) pixels apart, a third of the
  span at s = 1/3). Below s = 1/3, where the context mipmaps the picture
  (desktop GL, GL ES 3, or GL ES 2 with `GL_OES_texture_npot`), the taps
  read its mipmaps trilinearly a third of the span apart, each from the
  level a third of the span wide (`areaMipmapped`); the mipmaps are made
  once for each new picture, never for a redraw, and the picture's own
  minifying filter is left linear for the converter's still. The Flutter
  texture is unchanged: Flutter scales it bilinear.
- **At least its own size**, with Sharpen at Off: bilinear (`bilinear`),
  which at s = 1 is a one-to-one copy, as before.
- **With Sharpen at Low, Medium or High**, as on the Mac and after airhand:
  at s = 1 the picture's luma is sharpened (`sharpen`); larger, the visible
  part and a pixel around it are first enlarged into a texture of the
  plane's by Catmull-Rom from nine bilinear taps, clamped to the four
  nearest pixels so that it never rings beside text, and that is then
  sharpened one to one into the window (`upscaleSharpen`). The sharpening
  is CAS on luma alone, with airhand's peaks (−0.06, −0.1, −0.14), every
  channel moved by the luma's change; its neighbours stop at the enlarged
  region's edge. Unlike the Mac's, it keeps working when a zoom takes the
  picture past the window, since only the part in view is enlarged.

The plane works on RGB, since the converter has already made it; the Mac's
enhancement reads the decoder's planes. The shaders come from one source
in GLSL 1.50 for desktop GL and GLSL ES 1.00 for GL ES, as the converter's
do, and a filter whose shader the context cannot build is logged once and
drawn bilinear instead; the plane itself never fails for it. So is an
enlargement whose texture the GPU cannot make, which is not asked for
again at that size or larger, only at a smaller one, until Sharpen is
turned off or the texture is unbound. `enhance`
redraws the picture at once, though the host sends nothing while the
desktop is still; a new texture starts at Off, and the enlargement's
texture goes when Sharpen is turned off or the texture is unbound. While
the stats are timed, on desktop GL 3.3 or later, `planeScaleGpuP50Ms` and
`planeScaleGpuP95Ms` give the GPU time of the passes that draw the picture
(timer queries, read a frame later without waiting); unbinding the texture
drops them, so the next viewer's start from nothing.

`stats` adds `refreshIntervalMs`, the refresh interval the newest of GDK's
recent frames carries (what an X11 compositor reports, or on Wayland the
mode of the output, where GDK puts 1/60 s for an output it does not know)
and left out when none carries one, and, while timed, `planeIntervalP50Ms`
and `planeIntervalP95Ms`: the intervals between the frames that drew a newer
picture, from GDK's presentation times or, where the compositor reports
none, its frame times. `timings.surface_consume` then runs from
`frame_ready` to the plane taking the picture. On X11 every redraw of the
plane makes FlView upload its last frame again, which above 60 Hz may cost
more than the read-back it saves; that is for the device run to show.
Flutter runs Dart on the GTK thread, where the plane draws, so a long Dart
frame holds the plane's next picture back; `PB_MAPPER_UI_THREAD=separate`
gives Dart a thread of its own
(`FL_UI_THREAD_POLICY_RUN_ON_SEPARATE_THREAD`) for an A/B under Dart load,
and becomes the default only if that run shows the plane's p95 interval
improve.

None of this has run on a Linux device yet. The clip harness checks the
plane's drawing pixel by pixel (placement, clip, orientation, grey) in its
GL ES context and, with a converter of the plane's, in a desktop GL 3.2
core context, and there that each scale and level draws with its filter,
whose output matches a CPU reference of its arithmetic
(`ui/linux/test/plane_reference.h`), and that the mipmaps are made again
for another picture in the same texture but not for a redraw; and CI
builds the runner. The runner's GL code also ran on a Mac's OpenGL 4.1 core profile,
the desktop GL path, where every filter matched its reference, the area
filters came closer to a Lanczos-3 reference than bilinear at 0.75x to
0.25x, the enhancement raised edge contrast towards Lanczos-3's, and the
enlargement and sharpening of 1080p to 2560x1440 took 0.47 ms of GPU time
a picture; its GLSL ES 1.00 shaders compiled in WebGL
([measurements](benchmarks/2026-11-24-linux-plane-scaling/README.md)). When the Flutter pin moves to 3.50
or later, whose renderers differ (a Wayland subsurface, X11's
`fl_opengl_frame`), check that Flutter's buffer keeps its alpha over the
plane before shipping.

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
  has not run. Nor has the order by the GPU that draws on a machine with
  more than one GPU: CI checks it as a pure function of made-up GPUs and
  sysfs trees, and its llvmpipe names no GPU. The video plane, opt-in with
  `--dart-define=presenter=plane`, has not run on Linux at all: CI builds
  it, its clip harness draws through the plane's code in GL ES and desktop
  GL on llvmpipe, and its Dart hole ran in widget tests. Its frame rate and
  time to the screen against the texture's on 60 and 144 Hz monitors, under
  GNOME and KDE on Wayland and GNOME and Xfce on X11, the picture through a
  transparent Flutter view, clicks over it, a covered window's receipts and
  the separate UI thread are outstanding; they decide whether it becomes
  the Linux default. Its scaling ran on a Mac's OpenGL, with its GL ES
  shaders compiled in WebGL
  ([measurements](benchmarks/2026-11-24-linux-plane-scaling/README.md));
  its look on a Linux screen, its GPU time on Linux drivers and an
  unchanged picture rate with it are outstanding.
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
  The Mac's layer presenter, opt-in with `--dart-define=presenter=layer`,
  ran on an M6 against `desktop_ui_host` replays at 1080p, 1440p and 4K at
  60 frames a second and at 1080p and 4K at 120, with the session locked
  and the display asleep
  ([measurements](benchmarks/2026-11-24-mac-viewer-layer/README.md)). Its
  time to the screen against the texture's, the picture on a screen, its
  colours and grey still there, the pointer over it, the first click into
  an inactive window, live resizing and a ProMotion panel are outstanding;
  they decide whether it becomes the Mac's default. Its **Sharpen enlarged
  picture** ran only on that M6 with the display asleep, in a harness built
  from the runner's own code
  ([measurements](benchmarks/2026-11-24-mac-viewer-enhance/README.md)):
  the GPU passes' time and output offscreen, and the metal layer's
  placement on whole pixels, the hand-off, the switch between layers and
  the presents in a window. Its look and cost on a lit screen are
  outstanding.
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
- **Scrolling.** A wheel's notch as a host notch, `Input::Scroll` and the
  touchpad fling (see [Touch](#touch)) have run only in tests on a Mac.
  - The Dart side ran in widget tests with the target platform set to
    Windows (at 100, 125, 150 and 200 %, 1, 3 and 7 lines a notch), macOS and
    Linux: one notch event made exactly one host notch, or 120 units toward
    a host that lists `wheel_hires`, and a quarter notch 30; a touchpad's
    travel went as one continuous scroll per event, 360 units for 300 px,
    and what it left short of a step took nothing from a wheel's notch; a
    fling decayed, and every way of stopping it stopped it.
  - The host side ran in Rust tests: a scroll crosses the wire, the helper
    pipe (protocol 12) and the synthetic platform's injector as it was sent,
    a viewer refuses one toward a host without `wheel_hires`, queued
    scrolls add up, thread C re-aims the pointer before a scroll, and a
    release drops what a scroll kept short of a step. The Windows
    `SendInput` data's signs are a unit test that runs only on Windows; the
    Windows runner's `wheel_lines` builds only in CI.
  - Not run anywhere: a real wheel and touchpad on any viewer against
    `desktop_ui_host`, whose `host_input` lines show each `Scroll`; on a
    Windows host, a window logging `WM_MOUSEWHEEL` deltas while a Mac
    viewer's touchpad scrolls, a classic Win32 list box and a Java Swing
    list under continuous input (if either fails to scroll, a viewer setting
    to send notches instead is the planned answer); on a Mac host, an
    `NSView` logging `scrollWheel:` deltas, `hasPreciseScrollingDeltas` and
    whether the event is continuous; Windows and Linux touchpad flings.
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
recovers: there the per-second pictures fill the 1 Mbit/s link, since a pace
cap sheds nothing at the nominal model and only a cut of bits per frame
does. Every case on a per-frame budget asserts that nothing latched.
The
[rate-model latch measurements](benchmarks/2026-11-24-rate-model-latch/README.md)
record them before and after the per-tick latch.

The table above predates receipt-driven rate control, which changes several
of its rows: on the 1 Mbit/s limit the pace now stays at 43 FPS or more with
bits per frame at Balanced's floor, the expected bitrate is back at 3.2 Mbit/s
4 or 5 s after the lift, and the 350 ms round trip reaches the ceiling 0.8 s
after connecting. Four cases measure it: `moderate_limit_keeps_60` (3 Mbit/s for
10 s, asserting a pace of 55 or more from 1 s in and 55 received FPS over the
last 5 s), `sudden_drop` (5 Mbit/s once bits per frame reach their maximum,
asserting a peak ack queue of 300 ms and 100 ms from 2 s in),
`added_route_delay` (60 ms more each way, asserting at most two receipt cuts,
a rebase after any, and the encoder bitrate back within 15 s) and
`recurring_stalls_cut` (300 ms stalls every second, asserting that the encoder
bitrate falls);
`isolated_stall_does_not_collapse_fps` asserts no receipt cut and no fall of
the encoder bitrate.
[Receipt-driven rate control measurements](benchmarks/2026-11-24-receipt-rate-control/README.md)
has five runs of the suite before and after.
Six cases cover the viewer's fps cap and its backlog, all on simulated
decoders: `slow_decoder_speeds_up` (67 ms a frame for 20 s, then 5 ms),
`pipelined_slow_consumer` (four units held inside, as the Linux runner holds
them, decoded at 67 ms each), `fast_consumer_at_120` (10 ms a frame at 120
FPS), `injected_backlog_at_120_recovers` (one second at 20 ms a frame in a
120 FPS session on the short path) and `consumer_stall_drains` with its
1 Mbit/s twin (a 2 ms decoder that stalls once for 400 ms). The 67 ms decoder
case now runs 60 s. [Viewer fps cap measurements](benchmarks/2026-11-24-viewer-fps-cap/README.md)
has three runs of each before and after the cap measured its decoder.
`reconnect_on_a_slow_link` stops a viewer after 8 s on a 1 Mbit/s link,
connects another over it 3 s later for 8 s, and 3 s after that one stops
connects a third on an unlimited link. It reports, for each session, the time
from `Viewer::connect` to its first presented picture, that picture's bytes and
the host's ack queue in its first three trace intervals, and how long the
third took to reach 80% of Balanced's start; it asserts that each picture
came, each first picture was an IDR, the reconnect's IDR was smaller than the
first session's, and the third climbed within 20 s.
[Reconnect start rate measurements](benchmarks/2026-11-24-reconnect-start-rate/README.md)
has five runs before and after a reconnect started at the rate its last
session ended at, and five on a first version whose lowered start climbed
back only at the receipts' plain raise.
`lan_rtt_keeps_120` runs a 120 FPS session on the 18 ms path, first with a
synthetic encoder that serves a unit each 11 ms, one at a time, while it
takes four, then with one that keeps each unit 45 ms but takes many. It
reports, from 8 s to 48 s, past the first encoder's first probe and the
second's second cap, the encoder's dwell, the received rate, the pace and
the skipped ticks, then each `desktop_encoder_cap`, and asserts for the
first a dwell of 25 ms or less at the median and in all but 5 of the 40
seconds at 75 FPS or more received, and 75 FPS or more for the second.
[Encoder throughput cap measurements](benchmarks/2026-11-24-encoder-throughput-cap/README.md)
has three runs before and after the cap.
`direct_quic_on_lossy_links` carries the session over one QUIC connection
with the direct transport instead of the shaped TCP link: the testkit's
`lossy_quic_pair` through its `lossy_udp` hop, the host's end dialing the
host's listener for the stream the viewer opens, as a direct provider does,
and sending under Cubic and then BBR. It runs on 60 and 120 ms round trips
with 0.1% and 1% random loss each way, and on 60 ms behind a 5 Mbit/s
policer; after 6 s on a link the viewer asks for an IDR every 2 s for 20 s.
Each run reports P-frame and IDR latency from capture to the unit received
whole, the wait from each request to its IDR, the received rate, the host's
mean sent bitrate and pace and its rate cuts, and quinn's packets sent and
lost, congestion events and window on the host's end; it asserts only that
each session stayed up.
[Host direct congestion control](benchmarks/2026-11-24-host-direct-bbr/README.md)
has three runs of it.

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
| macOS viewer | VideoToolbox hardware decoding of H.264 and H.265 on an M3 Pro; thumbnail written on disconnect; the opt-in layer presenter's commits and receipts in replays on an M6 with its display asleep, and its picture enhancement's GPU passes and presents in a harness there; the layer on screen outstanding |
| Linux viewer | Native GStreamer path ran in WSL2 (WSLg) with NVDEC and gst-libav, against a host replaying recorded clips; CI builds the app and decodes clips with software decoders; VA-API, V4L2 and a real Linux desktop outstanding |
| macOS host | Compiled, linked and unit-tested in CI on a macOS runner (the encoder, the display list, the pointer read in a window-server session, thread C's waiter, a 60 fps session paced at 60 frames a second); view-only 1080p H.265 streaming on an M3 Pro |
| Pointer shapes | Windows host's shapes read over an owned window through a viewer, the custom one pixel for pixel; the Windows app and the Android emulator paint the host's shapes; in the Windows app, against a host replaying a clip, the real local pointer took the host's shapes in control, one pointer showing; the bitmap's growth with Android pinch zoom not seen; macOS cursor shape fidelity awaits hardware comparison |
| Keyboard modes and physical keys | Windows host injection, line breaks and the active input method's (WeType) composition through physical keys checked in an owned window; in the Windows app, against a host replaying a clip, real keystrokes through WeType checked: composition at the remote pointer to exactly the committed text, Enter and Backspace while composing, punctuation, the modifier rules, the remote mode's physical keys with the local input method detached, and the input method coming back; the Shortcuts menu checked with input posted to the app's view; a real second computer, Microsoft Pinyin and other input methods, and a Pinyin soft keyboard on Android outstanding |
| Elevated windows / secure-desktop UAC | Helper implemented; default-policy administrator and standard-user prompt acceptance outstanding |
| Remembered app-owned host policy | Current-user DPAPI; requires signed-in user and running app |
| Windows service-owned background host | Final build 922 passed upgrade restart, live direct/relay delivery with its UI exited, and persistent Stop/Enable with the same invitation; OS boot, standalone SCM restart and sleep/resume acceptance pending |
| Mac display lit when it sleeps | The wake limiter, the closed-lid rule and the 1 s left to a display that turns off under capture are unit-tested on every platform, and the online display list, the lid read and finding the shared display run in the macOS CI job; a session that starts on a sleeping display, a resume after the display slept, a display put to sleep during a session (H.264 and JPEG), `pmset -g assertions`, a locked Mac staying dark (whether the lock is reported within 1 s of the display turning off) and a MacBook with its lid closed, outstanding on a device |
| Mac capture interval, pause, start, kept capture and filled frame | The interval rule, a strict gate over every common refresh, that no interval outlasts the one before, when it changes and the one change in flight are unit-tested on every platform; the filled-frame flag and the live expected frame rate in the macOS CI job; the kept source for a frame-rate change in the worker tests; on an M6 with its screen locked, the display lookup took a p50 of 13.6 to 13.7 ms with the on-screen windows only against 33.1 ms with every window ([measurements](benchmarks/2026-11-24-mac-capture-cadence/README.md)). ScreenCaptureKit's gate (`sck_cadence`), whether it delivers a change it held back (`sck_held_back_change`, which decides whether a slower pace and a pause may slow the stream further), the frame rate, GPU and WindowServer cost before and after, a last picture arriving within one interval, the start with many windows on screen, two viewers with one pausing, and the filled edge outstanding on a device |
| Viewer following its window's display | The cap's display rule (lowered and raised at the next tick, backlog beneath it, a recovery sending the display's rate, a backlog cap held beneath a slower display and raised from on a faster one, a chosen frame rate ignoring it), the rounding of 59.94 and 119.88, the FFI op, and the app following its window with a fake runner and through a connect are tested on every platform; in a loopback session on an M6 a Smooth host followed `Viewer::set_display_refresh(60.0)` and back, standing in for a window move ([measurements](benchmarks/2026-11-24-viewer-refresh/README.md)). The Windows and Linux runners compile in CI only; a window moved between displays and a display's mode switched on a Mac, a Windows viewer on a 59.94 or 119.88 Hz mode against a 60 or 120 Hz host, and the Windows and Linux runners' `display_changed` on a move (X11 and Wayland) and a mode change are outstanding on a device |
| Windows display and system kept awake while capturing | The keeper's timing and the watchdog's loop are unit-tested on every platform and its power request is created in the Windows CI job; a viewer past the display-off timeout, `powercfg /requests` during capture, a network stall and a pause, and waking a dark display, in the app and through the service, outstanding on a device |
| Same owner's locked login / password entry | Session identity and desktop transition handling implemented; actual password-entry acceptance pending |
| Pre-login boot, SAS, control across user switching | Unsupported; service waits for its approved owner's login |
| Rotated displays on a Windows host | Turned upright by the video processor where it has the rotation cap; how the mode's size is read and the region each turn keeps are unit-tested on every platform; each angle upright, clicks landing where aimed, the converter's ignored GPU test and a GPU without the cap outstanding on a device |
| Linux hosts, macOS before 12.3, Windows hosts without Media Foundation | Unsupported |
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
measure of a GPU. Before the clips, `desktop_video_device_test` checks the
order the module tries decoders in for made-up GPUs (a hybrid laptop either
way round, two VA GPUs, three under each GStreamer version's naming, two
NVIDIA ones, none known) and reads GPUs from a sysfs tree of its own.

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
fresh noise, the worst case for a unit's size. Its loop waits as thread C
does, on the encoder's events, and prints how often it woke
(`loop_wakes_per_s`) with the encoder's `waiting` submits and the wait of
each output from its announcement to its collection
(`have_output_to_collect_us`); `--poll-events` builds the encoder without
its watcher and drains it every 2 ms, the comparison's baseline. `--text`
covers the display with a still page of ClearType text; `--h264-profile
base|high`, `--qvs`, `--scenario display-remoting`, `--gpu-priority` and
`--vp-usage quality|speed` build the encoder with those `EncoderOptions`;
`--dump-source` writes the NV12 picture each submit gave the encoder, the
reference for ffmpeg's PSNR and SSIM. It prints the encoder's read-back as an
`encoder_opened` line, with the encoder's own SPS in hex (`encoder_sps`),
and each unit's dwell and the time from its present to its output at p50
and p95; and a `capture` line: whether new pictures were converted straight
from the duplication's surface (`direct`) or copied out of it first
(`copied`), whether the video processor was asked for quality, and the
repeats copied from the last picture, with their time apart from the
submits that converted a capture. `--copy-capture` copies each new picture
out of the duplication first, as before direct conversion, the baseline of
that comparison in the same build, and `--gpu-time` brackets each picture's
GPU work (the conversion, with that copy where there is one, or a repeat's
copy) with D3D11 timestamp queries and prints their p50, p95 and mean as a
`gpu_time` line, with an HDR desktop's tone map timed apart (`tone_map_*`);
a video processor on another engine of the GPU may not lie wholly inside
the span, which GPUView or PIX tells. `--greys` covers the display with 16
still bands of grey and prints each band's luma in the last picture the
encoder was given beside the luma its grey must become, which on a Windows
HDR desktop, at each SDR content brightness, shows whether SDR content
arrives as bright as on an SDR desktop; `--sdr-duplication` duplicates in
BGRA8 only, as before the tone map, the baseline of that comparison. With
`RUST_LOG` set (`pb_mapper_desktop::session=info`) the host's own events,
such as `desktop_gpu_priority`, go to stderr.

References: [issue #50](https://github.com/acking-you/pb-mapper/issues/50),
[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput),
[Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput),
[Windows DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata),
[RustDesk elevation bootstrap](https://rustdesk.com/docs/en/client/windows/windows-portable-elevation/).

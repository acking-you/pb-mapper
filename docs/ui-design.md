# Flutter UI design

The Flutter app in `ui/` follows the design language of POCOX (Pocket-Codex):
calm graphite neutrals, ink-black primary actions, and one blue "signal" taken
from the logo. This document holds the durable rules: what the app shows, how
it is laid out, and how the galaxy view is drawn.

## Principles

- **One question per screen.** The app has six places — Overview, Tunnels,
  Remote desktop, Relay, Logs, Settings — and everything else opens over one
  of them. The remote viewer is the one exception: while it shows, it is the
  whole window.
- **Colour is meaning.** Neutrals carry no hue; controls are ink. Green is a
  running tunnel, amber retrying, red failed, grey stopped. The logo blue marks
  only this computer and live tunnels. Selection is position, never colour.
- **At rest a form shows names and values.** What a field is for appears under
  it while it has focus; advanced switches wait behind "More options".
- **An action reports on its own control.** Start becomes a spinner while it
  runs; the outcome arrives as one toast once the tunnel leaves its retry loop.

## Information architecture

| Page | What it answers | Code |
|---|---|---|
| Overview | Where everything is, at a glance: the galaxy | `pages/overview_page.dart` |
| Tunnels | What this computer shares and what it connects to | `pages/tunnels_page.dart` |
| Remote desktop | Who can see this computer, and which computers it can see | `pages/remote/` |
| Relay | Whether the relay answers, who this computer is to it, what it holds, and what an administrator can change | `pages/relay_page.dart`, `pages/relay/` |
| Logs | What the Rust side said | `pages/logs_page.dart` |
| Settings | Where the relay is, this computer's credential, whether it tries the local network first, theme, language | `pages/settings_page.dart` |

Sharing (`register`) and connecting (`connect`) are one editor
(`pages/tunnel_editor.dart`) with a Share/Connect switch: key, local address,
protocol, and the rest folded away. A service picked on the relay — in the
galaxy or the list — opens the editor with its key filled in.

First run (`pages/setup_flow.dart`) asks where the relay is, then what to do
first, then shows that one form. Its clipboard import reads the same setup
string as Settings, fills the relay fields and stages the carrier and
keep-alive preference until the user continues. Every step can be skipped; the app opens on
the galaxy, where a new tunnel spirals out of its star.

On a phone the bottom bar has room for five: Logs opens from a row in
Settings, with a back button (and Android Back) to return there. On
Android, Back at the root sends the app to the background, as Home does, so
tunnels and a paused session keep running. (On Android 16 the system decides
this itself, and does it only for an app opened from the launcher.)

All pages read one `AppStore` (`state/app_store.dart`), kept fresh by the Rust
change stream and a slow relay check. The tray reads the same store instead of
polling the FFI on its own. The remote desktop has its own store,
`DesktopStore` (`state/desktop_store.dart`), whose views — this computer as a
host, the open session, its statistics — each notify only their own
listeners, so a live picture never rebuilds a page.

### The Relay page

Tunnels is about this computer; Relay is about the relay. Its head answers
three things at once — does it answer, how fast, and who am I to it — with the
counts that matter beside them. Below it, sections:

- **Services**: every service the relay lists, searchable, with Connect, and
  whether it is this computer's own; the connections the relay holds for each.
- **Connections**: the control connections the relay holds, the direct
  sessions it has heard about, and the raw routing table folded away.
- **Credentials** and **Security**, with the administrator key only.
- **This computer**: the relay this computer can run itself, and its key.

The page asks the relay only while it is on screen (`state/relay_store.dart`,
every 10 s), since it asks several questions at once. Every question and
administrator request goes through one FFI export, `pb_mapper_relay_call_json`,
on the SDK's `Client` and `Admin`, so the app can do what the `admin` command
can.

**Nothing is hidden behind the key's shape.** The page asks the relay who this
computer's credential is, and shows the administrator's sections when the
relay says administrator. A temporary credential sees what it can use, and a
line saying the administrator key brings the rest. Issuing a credential is the
page's primary action; a new one is shown once, as itself and as a setup
string for the computer it is for. Rotating the key and resetting every
credential ask first — the reset needs a box ticked. Rotating erases every
temporary credential as well, since the relay starts a new credential state
with the new key, and both its card and its confirmation say so. It switches
this computer to the new key once the relay is known to have taken it; if the
relay's answer is lost and it cannot be asked again, the new key is shown to
keep and this computer stays on the old one.

Services are told apart by namespace as well as name, as the relay routes
them: an administrator sees two credentials' `cache` as two rows, and retires
each in its own namespace. With the administrator key every page sees every
namespace — the galaxy, the Tunnels count, the editor's suggestions — and
connects to any of them in one click: the connection subscribes in that
credential's namespace, remembers it, and says whose service it is. A
credential's service carries its owner's tag; the administrator's own carry
none. A connection is still known on this computer by its service's name, so
connecting to a second `cache` in another namespace replaces the first.

**Test answers in words.** Settings' Test, and Save, end in one toast: the
latency and service count, or the relay's own reason it did not answer.

### Which way the traffic goes

The app's tunnels run on the SDK's `Registration` and `Connection`, the
handles the CLI runs, so they go direct as the CLI does: each session of a
connection tries a direct path first and falls back to the relay before a
byte is sent, and a share lets connections reach it directly. The path is
chosen per session — a connection can hold some sessions of each — so a
tunnel's path is what its sessions are doing now, or when none is open, how
the last attempt ended.

- **A chip** beside each running tunnel's state, `PathChip`
  (`widgets/path.dart`): **Direct** in the signal blue with a bolt, for a
  path of this computer's own past the relay; **LAN** in the success green
  with the router, when that path stays within one local network (the SDK's
  `DirectLink.kind`; internet and private paths stay **Direct**), its
  tooltip adding the family, the other computer's address, QUIC's round
  trip and the setup time; **Relay** and **Relay only** in
  grey with the relay's tower; both counted when a connection holds both;
  **Going direct…** breathing while a path is being set up. A share counts
  every session that reaches it, relayed or direct, so its chip says
  Direct, LAN, Relay or both, as a connection's does. On a phone the chip
  takes the room left of the actions.
- **A route** in the galaxy's details panel, `PathRoute`: this computer and
  the service at either end; the relay on the line when traffic crosses it,
  set below an arc that clears it when it goes direct. Under it, one
  sentence: what the path means, how long a direct one took to set up, and
  when it is the relay after all, why — the relay offers no direct, the
  sharing computer declines it, or the two networks could not reach each
  other — and that later connections try again.
- **The arcs** in the galaxy, below.
- **A switch** under the editor's More options: *Try direct first* for a
  connection, *Allow direct connections* for a share. Off is
  `--relay-only`; it is saved with the tunnel and kept through an edit.
  *Port mapping* sits set in beneath it, since it only acts while direct
  is on: Off, Auto or Always, as a segmented control like the protocol's.
  Auto, the default, asks the router to map the direct UDP port only once a
  punch has failed; while direct is off the choice is kept, held and faded.
- **Diagnostics**, for a running tunnel, from its menu or its details
  panel (`pages/tunnel_diagnostics.dart`): its carrier, relay and sessions as
  pills, its port mapping as it stands, and its events, newest first, as they
  happen. Each event carries the tunnel's diagnostics, and the SDK's metrics
  are sampled every second while the panel is open, so the relay's round
  trip, the sessions on each path, the live link and the bytes each path
  carried (up and down) stay current with nothing kept up by the app itself.
  A tunnel started with *Try the local network first* off says so in a
  quiet pill. A session's opening and closing are counted, not listed. A
  mapping the router will not make is amber, never red: like a relayed path,
  it is no fault of the tunnel's.

Blue keeps its one meaning: this computer and what it reaches by itself. A
relayed path is not a fault, so it is never amber or red; why it is the
relay is in words, on hover and in the panel.

The FFI reports each running tunnel's `path` (`PathInfo` in
`native/pb_mapper_ffi/src/state.rs`) from its handle's diagnostics, and a
task per tunnel emits a change event when its state or which paths it uses
changes, so the window follows a path as it is chosen. The embedded relay
offers direct as `pb-mapper server` does, over UDP on its TCP port.

### Trying the local network first

When the other computer is on the same Wi-Fi or LAN, a direct path tries
that network first: its addresses are probed first, and paths off it wait
briefly for it. It is a property of this computer, not of a tunnel or of the
relay, so it is one switch in Settings — **Direct connections**, *Try the
local network first*, on by default — beside the relay but outside its form:
it is saved the moment it is flipped, needs no Save, and asks the relay
nothing (`AppConfig.lan_first`, through `pb_mapper_update_config`). Not in
the setup string, which carries the relay's settings to another computer,
nor in first-run setup or the tunnel editor.

Each tunnel, remote-desktop connection and sharing start takes it as it is
then; what is running keeps what it started with. Flipping it offers, in a
dialog like the carrier's, to start again the tunnels Rust names in the
save's `lanFirst`: those on the saved relay, under the saved credential,
that may go direct and started with the other setting. A tunnel still on an
earlier relay or credential is left alone, as the carrier move leaves it,
since a start would move it onto these. Later leaves them until they next
start, and a failure to start one says which in a toast. A remote-desktop
host, local or in the background service, takes it at the next Start
sharing.

Under it, **LAN transport** (局域网传输, key `settings-lan-transport`) is a
QUIC / TCP segmented control, QUIC by default: what a path within such a
network runs over, for what this computer dials (`AppConfig.lan_transport`,
saved at once the same way; the three config writes are serialised in
`AppStore`, so one never undoes another). Its help says that TCP needs the
other computer's firewall to let TCP in and falls back to QUIC within
300 ms otherwise, and that as the shared end this computer serves whichever
the other end asks for. A path across the internet is always QUIC, so the
control says nothing about it. Changing it reuses LAN-first's dialog, for
the connections Rust names in `lanTransport`; a remote-desktop viewer takes
it at its next connect. A path chip on TCP reads **LAN · TCP** (局域网 ·
TCP), in Tunnels, in the galaxy's panel, and in the remote viewer's chip,
overlay and quality panel; QUIC, the usual carrier, is never named. Two
ends on one computer meet over loopback, which is no LAN, so their TCP
path reads **Direct · TCP**. TCP
keeps its figures in the kernel, so a TCP link's only round trip is its
handshake's: the viewer's quality panel leaves out the network round trip
for it rather than show a stale number as QUIC's.

### How this computer reaches the relay

What crosses the relay rides a carrier: TCP, a connection per stream, or
QUIC, every stream layered on one encrypted UDP connection. It is a property
of this computer and its relay, like the address and the credential, so it
lives with them — in Settings, and in the setup string — not in a tunnel's
form. Each tunnel keeps the carrier it started with.

- **Settings** has a TCP/QUIC switch under the credential, and under it one
  line of what the relay last said: the UDP port it offers QUIC on, beside a
  green dot; in the caution colour, that it does not, when QUIC is chosen;
  otherwise what QUIC is. The line speaks only for the relay the address
  field still names.
- **Saving a new carrier** moves nothing by itself. Once the relay has
  answered over the new carrier — never before, since moving tunnels to one
  that does not get through would stop them — a dialog names how many
  running tunnels are still on the old one and offers to start them again;
  Later leaves them until they next start.
- **The Relay page** shows the carrier beside the relay's state, quiet and
  neutral: it is a way there, not a state to watch. Over QUIC the figures
  gain the measured round trip and packet loss, and a line of fine print the
  connections, streams and reuse, MTU and window, and the last QUIC error
  until a connection opens. All of it comes from this computer's own
  connections, so reading it asks the relay nothing. Over TCP, a relay that
  offers QUIC gets one quiet line with Use QUIC beside it.
- **A tunnel's path chip** names the carrier only when it is QUIC and the
  traffic crosses the relay: *Relay · QUIC*. TCP is what a relay is reached
  over unless said, and a direct path is QUIC of its own.
- **This computer's relay** can offer QUIC on a UDP port of its own, next
  to the TCP port; the TCP port's UDP twin already carries direct sessions.

The window's own questions — the relay check, the Relay page, the remote
desktop — go over the chosen carrier through one client kept per relay
setting, which shares its QUIC connection with the tunnels. Whether the
relay offers QUIC is asked over TCP beside them, so Settings can say so even
when QUIC does not get through — once per relay setting, and again on Test,
since a relay from before QUIC logs the question. The exception is a check
over QUIC that gets no answer at all: then TCP is asked at once, and a relay
that answers there and offers QUIC is reported as its UDP port not getting
through — usually a firewall that opened only TCP — not as a relay that is
silent.

### Remote desktop

The Remote desktop page (`pages/remote/`) holds both ends: a card for this
computer as a host, and the computers it views. A wide page sets them side
by side, a narrower one stacks them. A phone shows the list and a note that
it can view and control but not be controlled: only Windows shares its
desktop for now. On Windows without Media Foundation the host card is
replaced by a note saying sharing needs the Media Feature Pack; viewing
still works, over JPEG.

- **This computer** (`host_card.dart`). Off, one action turns remote
  control on: Windows' consent for the helper service when needed, then a
  private invitation. Sharing, the card says who is connected and what they
  may do, with Make view-only and Disconnect viewer on that line; then who
  the invitation lets in (View only or View and control), the invitation,
  the shared display, how sharing outlives the window and the UAC helper's
  state, with Stop sharing at its foot and revoke, rename and helper
  maintenance under More.
- **The invitation stays a secret on screen.** It shows masked, Copy puts
  it on the clipboard, and its QR code (`invite_qr.dart`) appears only when
  asked for and hides again when the page is left or the app goes inactive.
- **Remote computers** (`computer_list.dart`, `computer_list_compact.dart`).
  Each remembered computer with its state: Online while the relay lists its
  service (the list the app already polls), Not sharing, or Other relay when
  it was saved under another relay or credential. The open session's
  computer reads Viewing, with Back to picture. Only one remote computer is
  open at a time, so opening another asks first. A phone features the one
  used last, with its last picture and a More that renames, removes or
  remembers it, above the others, and offers Scan to add and Paste
  invitation; with none yet, it first says where an invitation comes from.
  A wide list shows a row's More on hover, and also while focus is within
  the row, so the keyboard and screen readers reach it; on a touch screen,
  which has no hover, it is always shown.
- **Adding** (`add_computer.dart`) recognises an invitation before
  connecting: it shows the host's service and the fingerprint the
  connection will be held to, never the token; a name refuses an invitation
  pasted into it, since names are shown in clear. On a desktop the dialog
  takes the paste at once, selects the recognised name, and connects on
  Enter. A pb-mapper setup string is
  named as one and sent to Settings; a computer already in the list, or one
  whose identity changed, is said so and never merged in silently. On a
  phone or tablet narrower than the rail the camera reads the QR code
  (`invite_scanner.dart`). Remember is on by default, but a computer is
  remembered only after its first picture, so a wrong invitation leaves
  nothing behind.
- **Where they are kept.** Rust keeps remembered computers in an encrypted
  vault (`desktop_computers.rs`): DPAPI on Windows, and on Android a key the
  Android Keystore wraps. Their invitations never come back to Dart; a
  computer is opened by its id. Other platforms cannot remember, and the add
  form says why. A computer's last picture is a small image in the app's
  private storage, deleted with it. Saved computers that cannot be decrypted
  or decoded are said so, with Clear saved computers, rather than as a
  device that cannot remember.
- **Desktop services are not tunnels.** A `desktop:` service is left out of
  Tunnels, the galaxy and the editor's suggestions; the Relay page lists it
  without Connect.

### The viewer

Viewing is a mode of the app, not a page: while the viewer shows it takes
the whole window in place of the rail and the page
(`pages/remote/viewer/viewer_screen.dart`), so there is only ever one. Back
returns to the list and keeps the session, paused: the host stops capturing
and the list offers Back to picture. A session left paused, by Back or by a
hidden or minimised window, ends after ten minutes, since it holds the
host's only viewer slot; one whose host identity changed holds no slot and
keeps its card until the user acts.

Both layouts share one stage (`viewer_stage.dart`): the picture, the remote
cursor and the overlays. Nothing opaque covers the picture while a frame can
still arrive. Connecting lists its steps on a card over a dim scrim; a lost
connection greys the last picture under a pill with the countdown and Retry
now, says when the relay it was opened through no longer lists the computer,
and ends after two minutes of failures; a secure or missing remote desktop is
said over the last picture. Only
a changed host identity is opaque: it stops reconnecting and offers Paste
new invitation for a remembered computer. An ended session says why, and
offers Reconnect where that can help.

- **Desktop** (`desktop_viewer.dart`): a dark title bar with the computer's
  name, the sharing capsule when this computer is being viewed too, and the
  caption buttons in their dark tone. Below it, a floating toolbar: the
  connection chip (path, round trip, frame rate and the speed received, each
  in a fixed cell so the centred bar never shifts; it shows or hides the
  quality panel), View and Control, Display (scaling, picture preference,
  then Resolution, **Remote resolution**, Maximum frame rate and Codec as
  submenus that name the current choice, then the quality panel and the
  quality monitor),
  Shortcuts, More, full screen and Disconnect. It is pinned at first;
  unpinned, it folds to a handle 2 s after the pointer leaves it, and never
  while one of its menus is open. In control, a hint says that keyboard and
  mouse now go to the remote computer, and that the system shortcuts this
  computer keeps for itself are in the Shortcuts menu.
- **Phone and tablet** (`touch_viewer.dart`): immersive, and landscape on a
  phone. The controls stand in the black gutters beside the picture — Back,
  the path, frame rate and speed, and the input mode on one side; Keyboard,
  Shortcuts, Right click and More on the other. More holds what Display
  holds on a desktop, each submenu laid flat as a section of its own. A
  tablet in portrait gets
  bars, and a screen the picture fills gets plates over it. The picture fits
  between side gutters or bars, not under them (the viewport's frame), and
  they stand on dark strips that let touches through: zoomed in, what lies
  under a strip is dimmed, readable controls stay over a white page, and
  every edge of the picture can be panned clear. Overlays keep to the fitted
  picture between the gutters. The keyboard brings a modifier bar (Ctrl,
  Alt, Shift, Win, Esc, Tab, the F keys, arrows and paging) and, while the
  picture is zoomed or covered, a minimap. A long press turns the finger
  into a joystick, a ring and a ball, which scrolls the way it is pushed;
  lifted in place it right-clicks. The quality button opens the full
  connection figures, which scroll on a landscape phone. Direct touch can
  add a virtual mouse. The first session opens the gesture sheet by itself;
  a long press on the input mode opens it again.

**What the picture is made of.** The picture preference changes live. The
resolution (Default: the remote display's own size, up to 4K and up to what
this device's decoder takes; or 720p, 1080p, 1440p) and the maximum frame
rate (Auto, or 30, 60, 90 or 120 up to the display's highest refresh rate)
belong to each remembered computer, as the preference does,
and are fixed for a session, so choosing one connects again at once, keeping
the picture and the choice to view only; the menus say so. A frame rate
chosen over Auto replaces every preset's own ceiling, Quality's 30 too. The
codec is the device's: Auto offers H.265 wherever this device has a hardware
decoder for it, and the session's size is fitted inside the largest picture
of the decoder offered first (asked of the platform at the first
connect, never by creating a decoder, and waited for at most 2 s: until a
probe that has not answered by then does, sessions are H.264), H.264 offers
only that. A computer that answers an offer of H.265 with H.264 is not
offered it again for the rest of the session. A session never asks for more
than its codec's decoder takes: one not offered H.265 asks for the largest
size up to the choice that the H.264 decoder takes, and one offered it at a
size only H.265 decodes here and answered with H.264 connects again at once
that way; the computer keeps the choice. A resolution the decoder of the
codec the session would carry cannot take is shown disabled with the reason.
When a picture fails to decode as H.265 the session connects again at once
on H.264, and H.265 is not offered again until the app restarts; only a
failure on H.264 too ends the session.

**Remote resolution** (远程分辨率, desktop only) changes the remote
computer's display mode, not the picture: Fit local (适配本机) asks for this
display's size, or the nearest size the remote lists; Original (原始分辨率)
puts its own back; the remote's own sizes follow. It shows only while this
viewer controls a host that offers it, since a mode change is as intrusive
as input, and the phone has none. The change lands within a few seconds
without reconnecting; for 3 s after it the viewer waits as for any capture
restart. A computer remembers Fit local, never a size, and applies it again
on gaining control. On the host, the Remote desktop card says while its
display is changed and offers Restore, which needs no viewer. The host puts
the mode back by itself when the session ends or control is revoked, never
on a pause, a release or a fresh grant.

**Figures at a glance.** Speed is RustDesk's: everything the session moves
each way, per second, as the tunnel counts it. The chip and the phone's
quality button show the speed received; the quality panel adds Speed both
ways, the encoded resolution, the codec (H.265, H.264 or JPEG) and its
chroma. The quality monitor, off at first and remembered for the device,
puts RustDesk's few figures (speed, frame rate, delay, target bitrate, codec,
chroma, resolution) small and translucent at the picture's top right — left
of the quality panel while that is open, under the minimap on a phone — and
takes no touch, so a stroke may start under it.

**The remote cursor** is the viewer's own: strokes, taps and the mouse move
it, the arrow is drawn there, and no wait hands it back to the host. The
store keeps it for the session (`DesktopCursor`), not the viewer on screen,
so a viewer opened again after Back resumes it where the last one left it.
It follows a host report only where the report says something new: the
first one of a connection or a display, any report while viewing only, and
else a report that changed and lies more than a pixel from where this
viewer put it — the remote user, a program or a clamp moved it.
The host reports nothing it injected for this viewer, so a lagging report
can no longer pull the arrow, or the next stroke, back. Each report also
says which of this viewer's inputs it reflects, and while this viewer
controls, one taken before its newest move, which crossed that move on the
way, is set aside: it neither moves nor hides the arrow. A hidden cursor
hides the arrow until this viewer moves it. A last move refused because the
picture went stale is sent with the next fresh picture.

How touch, the mouse and keys become remote input is in
[the remote desktop's documentation](remote-desktop.md#gestures-keys-and-modifiers).

### When this computer is viewed

While a viewer is connected to this computer, Disconnect viewer and Stop
sharing are always within reach (`shell/sharing_indicator.dart`):

- **A capsule** in the title bar of every page and of the viewer — Being
  viewed or Being controlled, then the two actions. It fits its bar's width:
  label and buttons from a 960 px bar (the default window's sheet is 968 px
  on Windows and Linux, 960 on macOS), buttons alone from 640 px, below that
  a dot with a menu. The Remote page's host card carries the same two
  actions instead.
- **A pill above every route**, where no capsule can be reached: first-run
  setup, the full-screen viewer, and any dialog whose barrier covers the
  title bar; the capsule under it steps aside, so one set shows.
- **The tray**: its active icon, a tooltip that leads with who is watching,
  and both actions in its menu. A sharing computer usually sits in the tray
  with its window closed, so when a viewer arrives the window comes back
  without taking focus. It is not kept on top.

Both actions work while another host action is still running. When the
Windows service shares with the app closed nothing is shown: the service
cannot draw in the user's session.

## Layout

The window is layered as in POCOX (`theme/glass.dart`):

1. `AppBackdrop` — the window's ground, lit softly from above;
2. the rail (`shell/navigation.dart`), straight on the backdrop, its selection
   a pane of glass that glides between rows;
3. `ContentSheet` — the page; grouped grey for pages of cards. On macOS
   (and a wide tablet) it floats 8 px inside the window, rounded all round;
   on Windows and Linux it runs to the window's top, right and bottom edges,
   rounded and edged only where it leaves the backdrop, down its left side
   from the top-left corner;
4. `Glass` — translucent panels over the page: the galaxy's summary, its
   details panel, toasts.

The native title bar is hidden on desktop. The page's own title bar is window
chrome: its empty space drags, and on Windows and Linux the caption buttons
sit over its end (`shell/window_chrome.dart`). macOS keeps its traffic lights.

The caption buttons are where Windows puts its own and look like them: 46 px
wide, square-cornered, flush in the window's top-right corner, on the page's surface
rather than inside a card on the backdrop. Like Windows' in a tall title bar
that holds controls, they are as tall as the bar under them — 58 px beside
the rail, 54 on a compact window, 36 over the viewer, Windows' 32 while
loading and on the setup screen — so their glyphs sit on the title's row.
They are one set for the app's whole life (`WindowControls`, drawn in
`app.dart` above the navigator): a page switch neither moves nor fades them,
a new bar's height is theirs in the next frame rather than grown into, and a
dialog's barrier dims the rest of the title bar but never them. A menu that
opens from the title bar, the sharing capsule's on a narrow window, opens
over them in the root overlay, as Windows' menus open over its own. Each
plate is a whole number of device pixels, so the hover fills reach both edges
without a fringe at 125 % and 150 %; the close button's red runs into the
corner, which Windows rounds itself, and a maximised window's close sits
where the pointer stops at the screen's corner. Their glyphs are the primary
ink, as Windows' are, and dim while the window is not the active one, laid on
as one layer so the close's crossing does not darken. The page's actions keep 16 px
clear of them. Their glyphs are drawn, not icons: 10 px, a one-pixel stroke,
snapped to device pixels so they stay sharp at 125 % and 150 %.

On Windows a restored window resizes from a 4 px band along its top edge,
over the caption buttons too, as native windows do: the hidden title bar
leaves Windows next to none of its own there. Its other edges are Windows'.
A known gap: hovering Maximize does not offer Snap Layouts, which Windows
shows only over a button it hit-tests natively; buttons Flutter draws would
need that hit-testing added to the runner. Win+Z, which Windows handles for
any window with a maximise box, should still open them; the acceptance
procedure checks it.

Below 720 px (900 px on a touch screen) the rail becomes a bottom bar and the
page fills the window. Cards reflow by the width they get, not by a global
breakpoint.

The viewer is dark whatever the app's theme, so a remote picture is never
framed in white (`theme/viewer_palette.dart`). Its chrome is translucent
graphite over the picture rather than the app's glass, since a blur over a
live video would be redrawn every frame; `viewerTheme` gives its menus,
dialogs and tooltips the same palette. A phone's gutters stand on
`ViewerPalette.gutter`, black at 80 %, the lightest that keeps even
secondary text above 4.5:1 over a white page. What the finger draws over the
picture (the trackpad ring, the joystick's ring and ball) carries both a
light and a dark edge, so it shows over white, black and anything between. Over its title bar the caption buttons
take its dark tone and its 36 px height; in full screen they hide with it.

Type is Google Sans Flex for prose (Segoe UI on Windows) and Geist Mono for
keys, addresses and logs; Chinese falls back to the bundled Noto Sans SC subset
on Windows and Linux and to PingFang SC on Apple platforms
(`theme/fonts.dart`).

Icons are Lucide only, through `theme/app_icons.dart`, named for what each
means here; no Material `Icons.*`. Four sizes (`AppIconSize`): 16 beside a
label or inline, 18 in buttons and menus, 20 in the rail and the bottom bar,
24 for empty states and tiles. Weight follows size: the 400 stroke up to
20 px, where the 300 stroke thins to grey, and the 300 stroke (the `…Large`
constants) from 24 px. An icon takes the colour of its text; status is told
by coloured dots, and blue stays for this computer and live traffic.

## The galaxy

`galaxy/` draws pb-mapper's topology as a disk in true
perspective, after POCOX's galaxy:

- **Stars**: this computer (the signal blue) and the relay (white; a dim ember
  while it does not answer). They sit either side of the disk's centre so the
  camera frames the pair.
- **Planets**: around this computer, the services it shares and the
  connections it holds (a connection wears a thin ring); around the relay, the
  services it lists that this computer does not share. Up to six share a ring,
  outer rings turning slower. Every position comes from a stable hash, so a
  planet keeps its place between visits.
- **Arcs**: a running tunnel is an arc of light from its planet, with
  particles travelling along it. Through the relay, it falls into the
  relay's star, fading as it goes; a connection that goes direct is lit end
  to end over a soft glow, its particles quicker, and lands past the relay
  on the planet of the service it reaches. One with sessions on both has
  both. The legend shows the two arcs beside the colours.
- **Motion**: a new planet spirals out of its star; a change of state sends a
  ripple round its ring; a removed one fades where it was. Reduced motion
  stills all of it.

Drag turns the disk, scroll or pinch zooms, a click picks a body and opens its
details (start/stop, edit, connect), and the arrow keys and Enter walk the
bodies. After a few seconds without input the camera glides back.

### Geometry in Rust

Per-frame geometry — projection, every orbit split into the runs behind and in
front of its star, planet positions with their entrances and ripples, the arcs
and their particles, the background stars, depth order and label placement —
is computed in Rust by `pb-mapper-galaxy` (`ui/native/pb_mapper_galaxy`), a
dependency-free crate based on POCOX's galaxy engine. Rust and Dart implement
the same near-plane clipping and keep their versioned buffer layouts in step.

`pb-mapper-ffi` exports it as one additive symbol:

```c
int64_t pb_mapper_galaxy_frame_v1(const double *scene, uint32_t scene_len,
                                  const double *frame, uint32_t frame_len,
                                  float *out, uint32_t capacity);
```

It is a leaf call on buffers the app owns — no allocation to free, no lock, no
callback, panics caught — returning the values written, minus the length
needed when `out` is short, or -1 for malformed input. The buffers carry
their layout's version (`VERSION`, 2 since an arc can end on a planet), so a
library and a Dart side of different layouts refuse each other rather than
misread. The scene is encoded only
when it changes; each frame writes 13 values (size, camera, clock, selection)
and reads the result in place. None of the JSON-envelope exports and none of the
SDK crates changed.

Where the library is not loaded (`flutter test`), `computeFrameInDart`
(`galaxy/galaxy_frame.dart`), a step-for-step port, fills the same buffer.
`test/galaxy_test.dart` compares the two on the built library over normal and
close-up camera, clock and selection combinations, within 1e-3 relative; keep
the crate and the port in step and keep that test passing.

`cargo run -p pb-mapper-galaxy --example galaxy_bench --release` times the
geometry call for typical and busy scenes.

Geometry clips guide, orbit and tunnel segments against the camera's near plane;
the painter clips their projections to the viewport
and submits guide and orbit lines in batches. Each canvas owns a backdrop
image, cached per size and theme, and a bounded sprite atlas for the spheres,
glows and shadows. The atlas uses the original vector layers on its first
frame and where an image cannot be made; cached layers keep the same ordering
and per-layer opacity. Oversized plane glows fall back to vectors. Hidden
canvases release their images, label layouts and native buffers; pending
backdrop images are discarded if the canvas has gone away.

Motion and camera easing resume for interaction or scene changes, then stop
once the finite transitions and camera have settled. Hidden applications
and disabled `TickerMode` pause the galaxy; visible unfocused windows retain
their images and can render changes. Reduced-motion users still get the
delayed return to the overview. The isolated profile-mode renderer at
`ui/tool/galaxy_resource_probe.dart` reports idle and selection frame timing,
CPU and memory without starting tunnels or reading settings.

## Tests and previews

- `test/app_test.dart`: the shell, first run, and that no page overflows on a
  320 px phone in Chinese.
- `test/tunnels_test.dart`, `test/editor_test.dart`, `test/settings_test.dart`,
  `test/relay_test.dart`: each page's flows against `FakePbMapperApi`, which
  plays the relay's administrator by default.
- `relay.rs`'s own tests run every relay operation against a live relay from
  `pb-mapper-testkit`.
- `test/store_test.dart`: the store's reads, edits and rollback.
- `test/galaxy_test.dart`: the scene, the Dart frame, Rust parity, and keyboard
  selection.
- `test/planet_atlas_test.dart`, `test/galaxy_lines_test.dart` and
  `test/galaxy_render_lifecycle_test.dart`: cached/vector image parity, line
  clipping, resource bounds, backgrounding, interaction and reduced motion.
- `test/title_bar_test.dart`: the caption buttons flush in the window's
  corner and as tall as the bar on screen, the sheet's colour in the corner
  pixel, restored at 125 % and maximised; the top edge that resizes a
  restored window; the compact layout, a widened one taking the taller bar
  at once, the viewer and the setup screen; a dialog's barrier leaving them
  uncovered, the capsule's menu opening over them, and a page switch leaving
  them still; macOS's inset sheet.
- Remote desktop, against `FakeDesktopApi` and an in-memory vault:
  `remote_page_test.dart` (the add flow never shows an invitation, enabling
  and stopping a share, the QR hiding), `sharing_test.dart` (the capsule and
  the pill), `remote_desktop_test.dart` and `remote_computers_test.dart`
  (remembering at the first picture, pausing, forgetting),
  `viewer_chrome_test.dart` (the toolbar, the overlays, input only while
  live), `desktop_store_test.dart` (the H.265 offer and its fallbacks, a
  probe that does not answer, the choices that connect again, the last move
  kept for a fresh picture), and the input core's pure tests —
  `touch_gesture_test.dart`, `remote_input_test.dart` (the cursor stays the
  viewer's own, a reopened viewer's too),
  `remote_text_input_test.dart`, `remote_surface_test.dart`.

To look at the design without a device, render the pages with their real
typefaces to PNGs:

```sh
cd ui
PB_MAPPER_UI_PREVIEW_DIR=/tmp/pbm-preview flutter test test/preview_test.dart
```

The remote desktop previews run through the whole app with a remote
computer's picture painted by `test/remote_screen_fixture.dart`. The ones
shaped for a desktop render as Windows, where a computer can share, in
Segoe UI when the machine has it; elsewhere their words are boxes.

## Updates

Settings › About has **Check for updates**, and the app also looks on its own
about twice a day unless the user turns that off; a newer release is announced
once, and nothing is downloaded until the user presses Update. The flow follows
proxy-everything's (`ui/lib/src/updates/`):

1. **Find.** GitHub's release listing, newest `vX.Y.Z-ui` that is neither a
   draft nor a prerelease. Not newer than the app's own version (its pubspec
   `version`, read through `package_info_plus`): up to date, nothing else is
   fetched.
2. **Trust.** Its `release-manifest.json` must carry an Ed25519 signature
   (`release-manifest.json.sig`) by the release key, checked in Rust with
   `ring` through `pb_mapper_ed25519_verify_v1`, and must describe that same
   version, so an older genuinely signed manifest cannot stand in for it. A
   release without one is offered only as a link to its page.
3. **Fetch.** The one package for this device, hashed as it streams in; the
   size and SHA-256 must match the manifest's, or it is deleted.
4. **Apply**, with the platform's own installer. What each platform does, and
   why, is in [UI updates and mobile releases](ui-updates.md).

   Debug builds stop after step 3 rather than replace themselves.

### Releasing with updates

- `ui/pubspec.yaml`'s build-name is the workspace version. Bump it with
  `Cargo.toml`: `test/version_test.dart` fails on a mismatch, and the release
  workflow refuses a tag that does not match it. An app built with a stale
  version would offer the same update forever. The build number is a separate
  counter that only grows; `docs/ui-updates.md` records the next one.
- `release-ui.yml`'s last job, `publish-manifest`, downloads the release's
  desktop packages, writes and signs the manifest with
  `scripts/release/ui_release_manifest.py`, verifies the signature against
  `scripts/release/update_signing_key.pub.pem`, and attaches both files. It
  needs the repository secret `UPDATE_SIGNING_KEY`: the private key, in PEM.
  Android APKs are built and signed locally, attached afterwards, and added to
  the manifest by `publish-android.yml`, as `docs/ui-updates.md` describes.
- The public key is also compiled into the app (`updatePublicKey` in
  `lib/src/updates/release.dart`). Rotating the key means shipping a release
  signed with the old key that carries the new public key, before any release
  is signed with the new one.

## The README poster

`docs/assets/poster.png` and `poster.zh-CN.png` are not drawn by hand: as in
POCOX, `test/gen_poster_test.dart` renders the real pages from fixture data —
the galaxy in the dark, the Tunnels page behind it, a phone connecting to a
service on the relay — and sets them beside the name and what pb-mapper does.
Change the app and the poster follows; change the words in that file. To
regenerate both:

```sh
cd ui
PB_MAPPER_REGEN_POSTER=1 flutter test test/gen_poster_test.dart
```

It is opt-in because the PNGs differ slightly from one platform's rasterizer to
another's, and it turns the test harness's shadow suppression off, since a
poster wants the shadows the app really casts.

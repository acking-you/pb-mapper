# Flutter UI design

The Flutter app in `ui/` follows the design language of POCOX (Pocket-Codex):
calm graphite neutrals, ink-black primary actions, and one blue "signal" taken
from the logo. This document holds the durable rules: what the app shows, how
it is laid out, and how the galaxy view is drawn.

## Principles

- **One question per screen.** The app has five places — Overview, Tunnels,
  Relay, Logs, Settings — and everything else opens over one of them.
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
| Relay | Whether the relay answers, who this computer is to it, what it holds, and what an administrator can change | `pages/relay_page.dart`, `pages/relay/` |
| Logs | What the Rust side said | `pages/logs_page.dart` |
| Settings | Where the relay is, this computer's credential, theme, language | `pages/settings_page.dart` |

Sharing (`register`) and connecting (`connect`) are one editor
(`pages/tunnel_editor.dart`) with a Share/Connect switch: key, local address,
protocol, and the rest folded away. A service picked on the relay — in the
galaxy or the list — opens the editor with its key filled in.

First run (`pages/setup_flow.dart`) asks where the relay is, then what to do
first, then shows that one form. Every step can be skipped; the app opens on
the galaxy, where a new tunnel spirals out of its star.

All pages read one `AppStore` (`state/app_store.dart`), kept fresh by the Rust
change stream and a slow relay check. The tray reads the same store instead of
polling the FFI on its own.

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
  path of this computer's own past the relay; **Relay** and **Relay only** in
  grey with the relay's tower; both counted when a connection holds both;
  **Going direct…** breathing while a path is being set up. A share knows
  only the sessions that reached it directly, so it shows a chip only for
  those, or for Relay only. On a phone the chip takes the room left of the
  actions.
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

Blue keeps its one meaning: this computer and what it reaches by itself. A
relayed path is not a fault, so it is never amber or red; why it is the
relay is in words, on hover and in the panel.

The FFI reports each running tunnel's `path` (`PathInfo` in
`native/pb_mapper_ffi/src/state.rs`) from its handle's diagnostics, and a
task per tunnel emits a change event when its state or which paths it uses
changes, so the window follows a path as it is chosen. The embedded relay
offers direct as `pb-mapper server` does, over UDP on its TCP port.

## Layout

The window is layered as in POCOX (`theme/glass.dart`):

1. `AppBackdrop` — the window's ground, lit softly from above;
2. the rail (`shell/navigation.dart`), straight on the backdrop, its selection
   a pane of glass that glides between rows;
3. `ContentSheet` — the rounded page; grouped grey for pages of cards;
4. `Glass` — translucent panels over the page: the galaxy's summary, its
   details panel, toasts.

The native title bar is hidden on desktop. The page's own title bar is window
chrome: its empty space drags, and on Windows and Linux it carries the caption
buttons (`shell/window_chrome.dart`). macOS keeps its traffic lights.

Below 720 px (900 px on a touch screen) the rail becomes a bottom bar and the
page fills the window. Cards reflow by the width they get, not by a global
breakpoint.

Type is Google Sans Flex for prose (Segoe UI on Windows) and Geist Mono for
keys, addresses and logs; Chinese falls back to the bundled Noto Sans SC subset
on Windows and Linux and to PingFang SC on Apple platforms
(`theme/fonts.dart`). Icons are Lucide at its 300 stroke (`theme/app_icons.dart`).

## The galaxy

`galaxy/` draws pb-mapper's topology as a slowly turning disk in true
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
dependency-free crate whose arithmetic and buffer layouts are POCOX's galaxy
engine unchanged.

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
`test/galaxy_test.dart` compares the two on the built library over sixteen
random camera, clock and selection combinations, within 1e-3 relative; keep
the crate and the port in step and keep that test passing.

`cargo run -p pb-mapper-galaxy --example galaxy_bench --release` times the
call: about 11 µs per frame (p50) for a typical scene and 20 µs for a busy one
on a desktop CPU.

The painter only issues draw calls. The nebula behind the disk
(`shaders/nebula.frag`) is rendered once per size and theme into an image.
POCOX additionally draws glows and spheres from a cached sprite atlas; at
pb-mapper's scene sizes the vector spheres are cheap enough, and the atlas is
the next step if raster time ever shows up in a profile.

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

To look at the design without a device, render the pages with their real
typefaces to PNGs:

```sh
cd ui
PB_MAPPER_UI_PREVIEW_DIR=/tmp/pbm-preview FLUTTER_ROOT=$(dirname $(dirname $(which flutter))) \
  flutter test test/preview_test.dart
```

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
4. **Apply**, per platform:

   | Installed as | Update |
   |---|---|
   | Windows installer | Quit, then a hidden PowerShell waits for the process to exit, runs the Inno Setup installer with `/SILENT` (it elevates itself when the install is machine-wide), and opens the app again |
   | Windows portable zip, Linux zip | Saved to Downloads and shown |
   | Linux AppImage | Swapped in place (staged beside it, renamed over it) and reopened; saved to Downloads instead when its folder is not writable |
   | macOS | The disk image is saved to Downloads and opened, to drag into Applications: the DMGs are not Developer ID signed, so the app does not replace its own bundle |
   | Android | The APK opens in the browser. Release APKs are signed with CI's throwaway debug key, so the system would refuse to install one over another from inside the app |
   | iOS | None: the IPA is an unsigned development build |

   Debug builds stop after step 3 rather than replace themselves.

### Releasing with updates

- `ui/pubspec.yaml`'s `version` is the workspace version, with build number
  `major * 10000 + minor * 100 + patch` (Android's version code only grows).
  Bump it with `Cargo.toml`: `test/version_test.dart` fails on a mismatch, and
  the release workflow refuses a tag that does not match it. An app built with
  a stale version would offer the same update forever.
- `release-ui.yml`'s last job, `publish-manifest`, downloads the release's ten
  packages, writes and signs the manifest with
  `scripts/release/ui_release_manifest.py`, verifies the signature against
  `scripts/release/update_signing_key.pub.pem`, and attaches both files. It
  needs the repository secret `UPDATE_SIGNING_KEY`: the private key, in PEM.
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
PB_MAPPER_REGEN_POSTER=1 FLUTTER_ROOT=$(dirname $(dirname $(which flutter))) \
  flutter test test/gen_poster_test.dart
```

It is opt-in because the PNGs differ slightly from one platform's rasterizer to
another's, and it turns the test harness's shadow suppression off, since a
poster wants the shadows the app really casts.

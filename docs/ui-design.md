# Flutter UI design

The Flutter app in `ui/` follows the design language of POCOX (Pocket-Codex):
calm graphite neutrals, ink-black primary actions, and one blue "signal" taken
from the logo. This document holds the durable rules: what the app shows, how
it is laid out, and how the galaxy view is drawn.

## Principles

- **One question per screen.** The app has four places — Overview, Tunnels,
  Logs, Settings — and everything else opens over one of them.
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
| Tunnels | What this computer shares, what it connects to, what else the relay offers | `pages/tunnels_page.dart` |
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
- **Arcs**: a running tunnel is an arc of light from its planet to the relay,
  with particles travelling along it.
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
needed when `out` is short, or -1 for malformed input. The scene is encoded only
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
- `test/tunnels_test.dart`, `test/editor_test.dart`, `test/settings_test.dart`:
  each page's flows against `FakePbMapperApi`.
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

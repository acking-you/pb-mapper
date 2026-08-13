# Tray icons

Two designs live here, because the two platforms want opposite things.

- **macOS** uses `tray_*.png` (plus `@2x`/`@3x`), rendered from the black
  template art in `src/`. A template image is black plus alpha; the system
  tints it to match the menu bar, so the state has to be carried by shape
  rather than colour.
- **Windows** uses `tray_*.ico`, rendered from `src/win/*.svg`: a blue gradient
  tile, a white plug glyph, and a colour-coded state badge. The tray sits on a
  coloured taskbar and colour reads fine there.

## Sizes the `.ico` files must contain

Windows asks the app for `GetSystemMetricsForDpi(SM_CXSMICON, dpi)`, which is
`16 * dpi / 96`:

| Scaling | 100% | 125% | 150% | 175% | 200% | 225% | 250% | 300% |
| ------- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| Pixels  | 16   | 20   | 24   | 28   | 32   | 36   | 40   | 48   |

Every one of those needs its own entry. When an exact entry is missing,
`LoadImage` picks the nearest and stretches it with GDI, which has no smoothing
— that is what made the icon look blurry at 125%, where the files carried 16
and 24 but no 20. A 64 entry is kept as a master for anything wanting a larger
rendition.

## Regenerating

```
cd ui/tool
npm install
node gen_tray_icons.mjs
```

Each size is rasterised from the vector at its own scale rather than resampled
from a larger render, which is what keeps 16 and 20 legible.

## About the colour artwork

`src/win/*.svg` was reconstructed from the shipped rasters, which had no vector
source of their own. The geometry is measured rather than guessed and matches
closely: the tile inset (6.25%), corner radius (21.875% of the tile) and
gradient (`#1976D2` to `#42A5F5`) are the app icon's own constants from
`../../icon_design/app_icon_blue.svg`; the badge sits at 78.125% with a radius
of 15.625%; the cable was fitted to the centreline of the 64px raster. Mean
per-channel difference against that raster is under 2%.

The one deliberate departure is the badge highlight. The original was a
photoreal gloss — a fixed blue-grey blob that was identical across all three
states, so it was painted on top rather than blended — and at 16–20px it only
ever resolved into a smudge. It is now a flat radial gradient, which is what
the rest of the icon set looks like anyway.

If you redraw any of this, keep the glyph simple enough to survive 16px. The
cable curve and the prongs are already close to the limit there.

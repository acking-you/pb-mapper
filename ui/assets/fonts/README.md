# Bundled Chinese UI font

`NotoSansSC-*.subset.ttf` are subsets of **Noto Sans SC**, licensed under the
SIL Open Font License 1.1 — see `OFL.txt`, taken verbatim from
<https://github.com/notofonts/noto-cjk> (`Sans/LICENSE`). The OFL permits
bundling, redistribution and modification, which is what makes subsetting these
and committing them here allowed.

## Why they exist

Neither Segoe UI nor the Linux defaults have a Chinese glyph, so every Chinese
run is drawn by whatever the system falls back to. On Windows that is Microsoft
YaHei, which ships Light, Regular and Bold and nothing between — a run asking
for 500 comes back Regular while the Latin beside it gets Segoe UI Semibold,
which does exist, so the line renders at two weights. On Linux the answer
depends on how fontconfig was set up, so it is not even predictable.

macOS is left alone: it falls back to PingFang SC, which has six real weights
and needs no help. See `lib/src/common/app_typography.dart`, which names these
only in `fontFamilyFallback` — the primary family stays the platform's own, so
English text still looks native.

## Regenerating

```
cd ui/tool
npm install
node gen_cjk_subset.mjs <path-to-NotoSansSC-VF.ttf>
```

The source variable font is not checked in; it is 17 MB and only the subsets
are needed. Get it from the noto-cjk releases above.

Four weights, because Material asks for 400 and 500 on its own (body, and
`titleMedium` plus the three label sizes) and this app's headings use 600 and
700. Every weight has to be a real face or the shaper starts synthesising and
the problem comes back.

They are **static instances**, not the variable font, because Flutter never
maps `fontWeight` onto a `wght` axis — the only writes to that axis in the
framework are explicit `FontVariation`s inside the icon widgets. Shipping the
variable font directly would render every weight at its default instance, which
for this font is 100 (Thin).

## Coverage

Everything encodable in GB2312 (6763 hanzi), plus ASCII, CJK and fullwidth
punctuation, and every character appearing in `lib/l10n/*.arb` — about 7000 in
all, 2.1 MB per weight. Characters outside the subset fall through to the
system font, which is why `app_typography.dart` still lists one after this
family. Widening the subset is a one-line change in the generator, at roughly
300 bytes per character per weight.

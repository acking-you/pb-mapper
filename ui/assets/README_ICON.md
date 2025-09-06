App Icon Setup

Option A: Use the provided SVG designs (purple default)

We added three vector icon designs under `assets/icon_design/`:

- `app_icon_blue.svg`
- `app_icon_purple.svg` (default recommendation)
- `app_icon_green.svg`

Recommended: export the purple variant as 1024×1024 PNG to:

- Path: assets/app_icon.png
- Recommended: 1024x1024, no rounded corners, no transparency-heavy edges.

Export commands (choose one that fits your toolchain):

- Inkscape:
  inkscape assets/icon_design/app_icon_purple.svg -o assets/app_icon.png -w 1024 -h 1024

- rsvg-convert (librsvg):
  rsvg-convert -w 1024 -h 1024 -o assets/app_icon.png assets/icon_design/app_icon_purple.svg

- ImageMagick:
  magick convert -background none -resize 1024x1024 assets/icon_design/app_icon_purple.svg assets/app_icon.png

Option B: Provide your own PNG

Place your brand PNG at the same path `assets/app_icon.png` (1024×1024).

How to generate platform icons:

1) From the `ui/` directory:
   - Install deps: `flutter pub get`
   - Generate: `dart run icons_launcher:create`

This updates icons for Android, iOS, macOS, Windows, Linux, and Web.

Tips

- Keep important content centered; desktop icons can crop corners.
- Prefer high contrast; small sizes like 16x16 need clarity.
- If you need separate dark/light variants for Web, see `icons_launcher` docs.

Per‑platform guidance

- Android (adaptive icons):
  - Provide foreground (`assets/app_icon_fg.png`) and background color/image.
  - Foreground content ~432×432 centered, no padding; background solid color preferred.
  - Optional: `adaptive_monochrome_image` for Android 13+, and `notification_image` (pure white glyph on transparent background).
  - Configure in `pubspec.yaml` under `icons_launcher.platforms.android`.

- iOS:
  - 1024×1024 PNG without transparency. Set `remove_alpha_ios: true` if needed.
  - Apple recommends not relying on alpha for shape; iOS applies masks/rounding.
  - Configure `image_path` or use the base `image_path`.

- macOS:
  - 1024×1024 PNG; macOS will generate rounded squircle automatically.
  - Optional: provide a separate `image_path` if macOS needs a different treatment.

- Windows:
  - Start from 512×512 or 256×256 PNG. The tool generates a multi‑resolution `.ico`.
  - Ensure good contrast at small sizes (16×16/32×32).

- Linux:
  - 512×512 PNG, simple shape and bold contrast recommended.

- Web:
  - 512×512 PNG; optionally specify `favicon_path` (e.g., 32×32) and set `background_color`/`theme_color`.

Example overrides (uncomment in `pubspec.yaml`):

icons_launcher:
  image_path: assets/app_icon.png
  platforms:
    android:
      enable: true
      # adaptive_background_color: "#FFFFFF"
      # adaptive_foreground_image: assets/app_icon_fg.png
      # adaptive_monochrome_image: assets/app_icon_mono.png
      # notification_image: assets/notification_icon.png
    ios:
      enable: true
      # image_path: assets/app_icon_ios.png
      # remove_alpha_ios: true
    macos:
      enable: true
      # image_path: assets/app_icon_macos.png
    windows:
      enable: true
      # image_path: assets/app_icon_windows.png
    linux:
      enable: true
      # image_path: assets/app_icon_linux.png
    web:
      enable: true
      # image_path: assets/app_icon_web.png
      # favicon_path: assets/favicon.png
      # background_color: "#FFFFFF"
      # theme_color: "#1976D2"

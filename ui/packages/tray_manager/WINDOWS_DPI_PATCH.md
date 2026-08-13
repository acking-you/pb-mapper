# Vendored fork of tray_manager 0.5.3

Upstream: https://pub.dev/packages/tray_manager

Everything here is the published 0.5.3 source apart from the Windows plugin.
The `example/` app was dropped; nothing else was touched. Wired in through
`dependency_overrides` in `ui/pubspec.yaml`.

## Why

The tray icon was blurry on Windows. Three separate things in
`windows/tray_manager_plugin.cpp` contributed:

1. **`GetSystemMetrics(SM_CXSMICON)` is not per-monitor DPI aware.** On a
   mixed-DPI desktop it answers for the primary display, so the icon is sized
   for the wrong monitor. Replaced with `GetSystemMetricsForDpi(SM_CXSMICON,
   GetDpiForWindow(hwnd))`.

2. **`LoadImage` stretches with GDI when the `.ico` has no entry at the
   requested size**, and GDI's stretch does no filtering. Replaced with
   `LoadIconWithScaleDown` (comctl32, Vista+), falling back to `LoadImage` if
   it fails. This needs `comctl32` in `target_link_libraries` and
   `_WIN32_WINNT=0x0A00` for the DPI declarations, both added to
   `windows/CMakeLists.txt`.

   The companion fix lives in the app: `ui/assets/tray/*.ico` now carry an
   entry for every size Windows asks for. See `ui/assets/tray/README.md`.

3. **The icon was never reloaded when the DPI changed.** The plugin reloaded on
   `TaskbarCreated` and on power resume but not on `WM_DPICHANGED`, so dragging
   the window to a differently-scaled monitor left the shell upscaling the old
   bitmap. Added a `WM_DPICHANGED` branch.

Loading was factored out of `SetIcon` into `_ReloadIcon`, which the
`WM_DPICHANGED` branch also calls; the icon path is now kept in a member so
there is something to reload from. `_ReloadIcon` only destroys the old handle
once the new one has loaded, so a failure leaves the existing icon in place
rather than clearing the tray.

## Rebasing

`git diff` this directory against a fresh copy of the pub cache version. The
changes are confined to `windows/tray_manager_plugin.cpp` and
`windows/CMakeLists.txt`. If upstream picks these up, drop the
`dependency_overrides` block and this directory.

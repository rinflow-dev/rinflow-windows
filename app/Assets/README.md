# LinkCursor logo

`LinkCursor.svg` is the original `logo-E6-indigo 1.svg`, kept unchanged as the
source of truth. Its transparent 64 x 64 canvas includes the intended padding.
`LinkCursor.Dark.svg` changes only the indigo stem from `#1E1B4B` to `#E0E7FF`
for visibility on dark backgrounds; geometry and gradients are identical.

Regenerate the derived SVG and both ICOs after editing the original:

```powershell
cargo run --locked --manifest-path tools/icons/Cargo.toml
```

Run this from the repository root. Rust/resvg is needed only to regenerate;
normal builds use the checked-in assets. Each frame is rendered directly from
the vector at 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96 and 256 pixels.
Smaller frames use 32-bit BGRA DIBs with alpha and AND masks; 256px uses PNG.
There is no added backplate, cropping or stretching.

Integration:

- `app/LinkCursor.rc`: primary EXE icon (Explorer, shortcuts, Start menu).
- `app/AppIcon.h`: WinUI `AppWindow.SetIcon`, using an absolute path beside
  the executable, with a dark variant when the window theme changes.
- `app/App.xaml`: theme-aware SVG for the custom title bar and tray panel.
- `agent/build.rs`: the same ICOs embedded as resources 101 (primary/light)
  and 102 (dark). The tray loads the system small-icon size with
  `LoadIconMetric`, follows `SystemUsesLightTheme`, and reloads after theme
  or display changes and Explorer restarts. Owned icon handles are released.
- `installer/LinkCursor.iss`: setup icon; installed shortcuts and the
  uninstall entry use the primary icon embedded in `LinkCursor.exe`.

This is an unpackaged WinUI desktop application with an Inno Setup installer.
MSIX tile/Store logo assets do not apply to the current distribution.
Explorer's static embedded EXE/shortcut icon uses the original colorway;
the running window and tray can select a theme variant.

References:

- [Windows app icon sizes and transparency](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-construction)
- [WinUI AppWindow.SetIcon](https://learn.microsoft.com/en-us/windows/windows-app-sdk/api/winrt/microsoft.ui.windowing.appwindow.seticon)
- [Notification area icon DPI guidance](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area)

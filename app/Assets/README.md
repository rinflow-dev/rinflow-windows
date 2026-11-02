# Rinflow logo

Each theme has its own supplied artwork, the source of truth for the icons.
Both share one geometry on a transparent 64 x 64 canvas, which includes the
intended padding:

| File                  | For             | Original                      | Stem      | Cursor    |
| --------------------- | --------------- | ----------------------------- | --------- | --------- |
| `Rinflow.svg`      | light surfaces  | `logo-warm-ink-black-512.svg` | `#D9D4D1` | `#181717` |
| `Rinflow.Dark.svg` | dark surfaces   | `logo-warm-ink-white-512.svg` | `#4A4746` | `#F5F2F0` |

The cursor carries the contrast and the stem is deliberately quiet, so each
variant is only legible on its own theme: the cursor measures 16:1 or more on
its theme's surfaces and about 1.1:1 on the other's. Choosing the wrong variant
leaves only the stem visible.

Regenerate both ICOs after editing either SVG:

```powershell
cargo run --locked --manifest-path tools/icons/Cargo.toml
```

Run this from the repository root. Rust/resvg is needed only to regenerate;
normal builds use the checked-in assets. Each frame is rendered directly from
the vector at 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96 and 256 pixels.
Smaller frames use 32-bit BGRA DIBs with alpha and AND masks; 256px uses PNG.
There is no added backplate, cropping or stretching.

Integration, with the theme that selects the variant on each surface:

- `app/App.xaml`: SVG for the custom title bar and tray panel. Follows the
  window's theme, including the in-app setting.
- `app/AppIcon.h`: WinUI `AppWindow.SetIcon`, using an absolute path beside the
  executable. Appears on the taskbar and in Alt+Tab, so it follows the Windows
  theme (`SystemUsesLightTheme`) and reloads on `WM_SETTINGCHANGE`.
- `agent/build.rs`: the same ICOs embedded as resources 101 (light) and 102
  (dark). The tray loads the system small-icon size with `LoadIconMetric`,
  follows `SystemUsesLightTheme`, and reloads after theme or display changes
  and Explorer restarts. Owned icon handles are released.
- `app/Rinflow.rc`: the EXE's icons, 101 (index 0, light) and 102 (index 1,
  dark). Explorer always shows index 0 for the file itself.
- `installer/Rinflow.iss`: the setup program uses the light icon. Shortcuts
  and the uninstall entry use the EXE's icon for the Windows theme at install
  time; an upgrade picks again.

This is an unpackaged WinUI desktop application with an Inno Setup installer.
MSIX tile/Store logo assets, including the `altform-lightunplated` theme
variants, do not apply to the current distribution, so static shell icons
cannot follow a later theme change: a shortcut keeps the variant it was given
until the next install.

References:

- [Windows app icon sizes and transparency](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-construction)
- [App icon color contrast and theme assets](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-design)
- [WinUI AppWindow.SetIcon](https://learn.microsoft.com/en-us/windows/windows-app-sdk/api/winrt/microsoft.ui.windowing.appwindow.seticon)
- [Notification area icon DPI guidance](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area)

# Rinflow logo

Each theme has its own supplied artwork, the source of truth for the icons.
Both share one geometry on a transparent 512 x 512 canvas, which includes the
intended padding, and each is a single solid colour:

| File               | For            | Original                      | Colour    |
| ------------------ | -------------- | ----------------------------- | --------- |
| `Rinflow.svg`      | light surfaces | `logo-warm-ink-black-512.svg` | `#181717` |
| `Rinflow.Dark.svg` | dark surfaces  | `logo-warm-ink-white-512.svg` | `#FEFFFF` |

`Rinflow.Shell.svg` is the same mark in `#181717` on a `#F3F0EA` plate, for
surfaces that cannot follow the theme. Its corners follow Microsoft's 2px radius
at 48 x 48, and the plate is inset 1/16 so its edges land on whole pixels at the
sizes that are multiples of 16.

The black original is cropped tightly to 320 x 386. Here it sits on the same
square canvas as the white one, because the generator scales to a square.

Each variant measures 15:1 or more on its theme's surfaces and about 1.1:1 on
the other's, so **the wrong variant leaves nothing visible**. That is why every
surface below either follows the theme or uses the plated icon.

Regenerate the ICOs after editing any SVG:

```powershell
cargo run --locked --manifest-path tools/icons/Cargo.toml
```

Run this from the repository root. Rust/resvg is needed only to regenerate;
normal builds use the checked-in assets. Each frame is rendered directly from
the vector at 16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96 and 256 pixels.
Smaller frames use 32-bit BGRA DIBs with alpha and AND masks; 256px uses PNG.
The themed icons have no added backplate, cropping or stretching.

Integration, with the theme that selects the variant on each surface:

- `app/App.xaml`: SVG for the custom title bar and tray panel. Follows the
  window's theme, including the in-app setting.
- `app/AppIcon.h`: WinUI `AppWindow.SetIcon`, using an absolute path beside the
  executable. Appears on the taskbar and in Alt+Tab, so it follows the Windows
  theme (`SystemUsesLightTheme`) and reloads on `WM_SETTINGCHANGE`.
- `agent/build.rs`: the plated ICO as resource 100, which Explorer shows, and
  the themed ICOs as 101 (light) and 102 (dark). The tray loads the system small-icon size with `LoadIconMetric`,
  follows `SystemUsesLightTheme`, and reloads after theme or display changes
  and Explorer restarts. Owned icon handles are released.
- `app/Rinflow.rc`: the EXE's only icon, plated, shown by Explorer for the file
  and read by shortcuts and the uninstall entry.
- `installer/Rinflow.iss`: the setup program uses the plated icon. Shortcuts and
  the uninstall entry use the EXE's.

This is an unpackaged WinUI desktop application with an Inno Setup installer.
MSIX tile/Store logo assets, including the `altform-lightunplated` theme
variants, do not apply to the current distribution, so static shell icons
cannot follow a theme change. Microsoft allows a plate when the theming has to
be handled by the app; one that reads on both themes needs no per-theme choice.

References:

- [Windows app icon sizes and transparency](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-construction)
- [App icon color contrast and theme assets](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-design)
- [WinUI AppWindow.SetIcon](https://learn.microsoft.com/en-us/windows/windows-app-sdk/api/winrt/microsoft.ui.windowing.appwindow.seticon)
- [Notification area icon DPI guidance](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area)

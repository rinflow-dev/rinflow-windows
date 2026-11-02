# LinkCursor for Windows

[![CI](https://github.com/akarineren/linkcursor-windows/actions/workflows/ci.yml/badge.svg)](https://github.com/akarineren/linkcursor-windows/actions/workflows/ci.yml)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)

The Windows client for LinkCursor. It is two programs:

| Program | Language | Role |
| --- | --- | --- |
| `linkcursor-agent.exe` ([agent/](agent/)) | Rust | Always running. Links `linkcursor-core` directly, owns the tray icon, plays input from a controlling peer, draws the edge push-through effect, and serves a named pipe for the settings app. Keyboard and mouse capture will live here too, because input must never wait on UI or a garbage collector. |
| `LinkCursor.exe` ([app/](app/)) | C++/WinRT, WinUI 3 | The main window (devices, their arrangement around this PC, settings) and the tray panel. It runs only while one of its windows is open, one instance per user, so it costs no memory the rest of the time. |

```
 LinkCursor.exe (WinUI 3)  ── named pipe, JSON lines ──►  linkcursor-agent.exe (Rust)
   devices, arrangement,                                    ├ tray icon
   settings, tray panel                                     ├ arrangement (layout.json, display watcher)
                                                            ├ edge effect overlay (Direct2D + DirectComposition)
                                                            ├ linkcursor-core ── UDP 42420 / QUIC 42421 ──► other devices
                                                            ├ input injection (SendInput)
                                                            └ hooks to capture local input
```

Status:
- The agent discovers peers and connects to them.
- It plays the mouse and keyboard input of a peer that controls this PC (for example the macOS app).
- It controls a peer when the pointer is pushed through the edge it is placed at, with the push-through effect on this PC's side.
- Peers can be arranged around this PC's screens, and the arrangement syncs with them.
- The clipboard can follow control to and from peers (off by default).
  Copied files use Windows virtual files: crossing only publishes a
  description; pasting reads the contents in bounded chunks. Peers without
  file streams (builds older than them) cannot send files here: their offer
  is declined and the user is told to update that device. See [clipboard implementation](agent/docs/clipboard.md).
- The user's devices connect by themselves and reconnect after a drop (see [Arranging devices](#arranging-devices)).
- Every connection is encrypted (QUIC, TLS 1.3), and new devices can be required to pair with a six-digit code (see [Pairing and encryption](#pairing-and-encryption)). Devices with a core older than ABI 7 cannot connect.
- Sharing can be paused, crossing has a switch delay, corners and full-screen games can be excluded, and a shortcut locks the pointer to the current screen (see [Settings](#settings)).
- The edge effect can be tuned with a single-machine demo (development builds).

## Layout

```
build.cmd, build.ps1         one-command build from any terminal
agent/
  Cargo.toml                 depends on ../../linkcursor-core
  build.rs                   embeds linkcursor-agent.manifest (PerMonitorV2 DPI, asInvoker) and a
                             version resource (FileDescription "LinkCursor": the name on its
                             notifications, in Task Manager and in the firewall prompt)
  src/main.rs                single instance, core start-up, shutdown
  src/state.rs               folds core events into the snapshot the app renders
  src/ipc.rs                 named-pipe server
  src/services.rs            what IPC requests act on (core, arrangement, effect, demo)
  src/displays.rs            monitors in logical points, display-change watcher, physical portals
  src/arrangement.rs         saved placements and known devices (%LOCALAPPDATA%\LinkCursor\layout.json)
  src/effect.rs              effect parameters (%LOCALAPPDATA%\LinkCursor\effect.json)
  src/spring.rs              damped spring with SwiftUI-style response/damping
  src/overlay.rs             click-through overlay that draws the edge effect
  src/edge.rs                edge thread: raw-input push tracking at the portals, crossing, the demo,
                             the lock shortcut
  src/capture.rs             while driving a peer: low-level hooks, hidden parked cursor, return
  src/clipboard.rs           the Windows clipboard ↔ the core: text, HTML (CF_HTML), PNG (and CF_DIB)
  src/settings.rs            preferences (%LOCALAPPDATA%\LinkCursor\settings.json), start at sign-in
  src/tray.rs                notification-area icon: opens the tray panel; menu with 打开主窗口 / 退出
  src/identity.rs            device ID (%LOCALAPPDATA%\LinkCursor\device-id) and name
  src/trust.rs               paired devices (%LOCALAPPDATA%\LinkCursor\trusted.json)
  src/power.rs               sleep, wake and IP address changes, reported to the core
  src/inject.rs              plays a controlling peer's input with SendInput (or through the input helper)
  src/hider.rs               hides the cursor where a controlling peer's pointer left it
  src/elevated.rs            the agent's end of the input helper's pipe
  src/keymap.rs              USB HID usage → Windows scan code
  src/win.rs                 Win32 helpers: mutex, user SID, pipe security
input/                       input beyond a normal process (elevated windows, UAC prompts, lock screen)
  src/lib.rs                 pipe protocol shared with the agent, SendInput helpers
  src/bin/input.rs           linkcursor-input.exe: LocalSystem helper in the user's session
  src/bin/service.rs         linkcursor-service.exe: the LinkCursorInput service that starts it
  src/win.rs, src/log.rs     process checks, pipe security, the machine-wide setting; logs
installer/
  LinkCursor.iss             Inno Setup script for the setup program
  build-installer.ps1        Release build + setup program
  ChineseSimplified.isl      Chinese messages for the setup program
app/
  LinkCursor.vcxproj         unpackaged, self-contained WinUI 3; also builds the Rust programs with Cargo
  Directory.Packages.props   NuGet versions (central package management)
  Main.cpp                   entry point: single instance, later launches handed to the running app
  App.xaml(.cpp)             shared styles; keeps the main window and the tray panel
  MainWindow.xaml(.cpp)      main window (Mica): the 设备 page, device list, 添加设备 dialog
  MainWindow.Arrangement.cpp the arrangement canvas: to-scale screens, dragging, 未放置 chips
  MainWindow.Settings.cpp    the 设置 page and the lock-shortcut dialog
  MainWindow.Pairing.cpp     the 配对 dialog (the pairing code)
  MainWindow.Effect.cpp      the edge animation page (development builds only)
  TrayWindow.xaml(.cpp)      the tray panel (acrylic, closes when it loses focus)
  DeviceModel.{h,cpp}        the user's / nearby devices from the agent's snapshot, and their rows
  AgentClient.{h,cpp}        named-pipe client; starts the agent if needed
  PeerItem.{idl,h,cpp}       row model for the device lists
```

`linkcursor-core` must be checked out next to this repository, as for the macOS app:

```
workspace/
  linkcursor-core/
  linkcursor-windows/
```

## Prerequisites

- Windows 10 1809 or later (Windows 11 recommended).
- [Rust](https://rustup.rs) with the MSVC toolchain (`stable-x86_64-pc-windows-msvc`). Add `aarch64-pc-windows-msvc` for ARM64 builds.
- Visual Studio 2026 or Build Tools 2026 with:
  - the **Desktop development with C++** workload (`Microsoft.VisualStudio.Workload.VCTools`) plus the MSVC x64/x86 tools and a Windows 11 SDK;
  - the **WinUI application development build tools** workload (`Microsoft.VisualStudio.Workload.UniversalBuildTools`) with **C++ WinUI app development build tools** and **C++ Universal Windows Platform tools**. The project uses `ApplicationType=Windows Store`, which needs these.

  For Build Tools from the command line:

  ```powershell
  winget install --id Microsoft.VisualStudio.BuildTools -e --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100 --add Microsoft.VisualStudio.Workload.UniversalBuildTools --add Microsoft.VisualStudio.ComponentGroup.UWP.VC.BuildTools --add Microsoft.VisualStudio.ComponentGroup.WindowsAppDevelopment.VC.BuildTools --add Microsoft.VisualStudio.Component.NuGet.BuildTools"
  ```
- For the setup program only: [Inno Setup 6](https://jrsoftware.org/isinfo.php) (`winget install JRSoftware.InnoSetup`; a per-user install is enough).

The Windows App Runtime is not needed on the machine: the app is self-contained (`WindowsAppSDKSelfContained`) and ships the runtime next to itself.

## Building

The application logo source and generated Windows icons live in
[app/Assets](app/Assets/README.md). They are shared by the main window, tray,
executables and installer; normal builds require no image-generation tools.

From any PowerShell or Command Prompt window:

```powershell
cd linkcursor-windows
.\build.cmd                              # Debug, x64
.\build.cmd -Configuration Release
.\build.cmd -Platform ARM64              # needs: rustup target add aarch64-pc-windows-msvc
.\build.cmd -SkipAgent                   # the WinUI app only
```

[build.cmd](build.cmd) runs [build.ps1](build.ps1) without changing the PowerShell execution policy. The script:

- finds Visual Studio or Build Tools and loads its developer environment;
- switches the console to UTF-8, so output from MSBuild, the C++ compiler and Cargo is never decoded with the wrong code page (for example 932 on a machine whose system locale is Japanese);
- runs MSBuild on `app\LinkCursor.vcxproj`. That restores NuGet packages, builds `LinkCursor.exe`, runs `cargo build` for the agent and for [input](input/) (the input helper and its service), and copies `linkcursor-agent.exe`, `linkcursor-input.exe` and `linkcursor-service.exe` next to the app. Everything ends up in `app\<Platform>\<Configuration>\LinkCursor\`. Release builds the Rust programs with `--release`.

Build messages are in Chinese by default. Pass `-Language en-US` (or another culture name) to change this. MSBuild and NuGet messages follow it right away. The C++ compiler only speaks the Visual Studio language packs that are installed, so add one for compiler messages too:

```powershell
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" modify `
    --installPath "${env:ProgramFiles(x86)}\Microsoft Visual Studio\18\BuildTools" --addProductLang zh-CN --passive
```

Inside a Developer PowerShell you can also call MSBuild directly: `msbuild app\LinkCursor.vcxproj -restore -p:Configuration=Debug -p:Platform=x64`.

For a debugging loop, use [dev.ps1](dev.ps1): `.\dev.ps1 build`, `.\dev.ps1 run` and `.\dev.ps1 stop`. Building never stops a running copy: [build.ps1](build.ps1) renames the files it holds open to `*.old` (Windows refuses to overwrite them but allows the rename), so `.\build.cmd` also works while LinkCursor runs. `make run` builds first, then stops the old copy, deletes the `*.old` files and starts the new build, keeping the agent in your terminal with its log. The settings window it opens does not start an agent of its own, so the terminal stays in charge of the agent. Ctrl+C, or closing a debug agent's console, quits the agent the same way as the tray's 退出: the tray icon goes away and peers see a clean disconnect.

A Release x64 build is about 64 MB of files, most of it the Windows App Runtime it carries; the setup program is about 16 MB. The agent is 5 MB, `LinkCursor.exe` 1 MB, the input helper 1 MB and the service 0.4 MB. The WebView2 DLL that the WinUI package brings in is not used and is left out of the setup program. At run time the agent uses about 13 MB of memory and no CPU while idle; the settings window uses about 120 MB, but only while it is open.

### Setup program

```powershell
.\installer\build-installer.ps1             # Release build, then the setup program
.\installer\build-installer.ps1 -SkipBuild  # package the existing Release build
```

[build-installer.ps1](installer/build-installer.ps1) runs the Release build and compiles [LinkCursor.iss](installer/LinkCursor.iss) with Inno Setup into `installer\output\LinkCursor-Setup-<version>-x64.exe` (the version comes from [agent/Cargo.toml](agent/Cargo.toml)). The setup program speaks English or Simplified Chinese ([ChineseSimplified.isl](installer/ChineseSimplified.isl), MIT, from [kira-96/Inno-Setup-Chinese-Simplified-Translation](https://github.com/kira-96/Inno-Setup-Chinese-Simplified-Translation)), picked from the Windows display language; for any other language it asks which one. Its own texts are `[CustomMessages]` in the script. It needs an administrator and:

- installs into `Program Files\LinkCursor`, where only administrators can change files (the input helper and the agent rely on that when they check each other);
- adds a firewall rule for the agent on private **and public** networks (Windows often marks a new Wi-Fi network public, and nothing would be found without saying why);
- registers and starts the `LinkCursorInput` service (`linkcursor-service.exe --install`) and writes the 被控时可操作管理员窗口、UAC 和锁屏 setting (a task, checked by default; see [Elevated windows, UAC prompts and the lock screen](#elevated-windows-uac-prompts-and-the-lock-screen));
- turns on starting at sign-in (a task) for the user who ran it (`linkcursor-agent.exe --launch-at-login on`, run as that user, not as the administrator);
- before replacing files (an upgrade), removes the service and closes LinkCursor; uninstalling removes the service, the firewall rule and the logs, and keeps each user's settings and pairings.

It is not code-signed yet, so SmartScreen warns before it runs; `SignTool` in the script is ready for a certificate.

To work on the agent alone:

```powershell
cd linkcursor-windows\agent
cargo build
cargo run          # debug builds keep a console window for logs
```

## Running

Start either program:

- `LinkCursor.exe` connects to the agent and starts `linkcursor-agent.exe` from the same folder if it is not running yet. It runs once per user: starting it again brings the running window back.
- `linkcursor-agent.exe` puts an icon in the notification area. Left-click it for the tray panel: the 键鼠共享 switch, the devices, **打开主窗口**, settings and quit. The right-click menu has **打开主窗口** and **退出**. Quitting stops the agent and the core.
- Closing the main window leaves the agent running in the tray, unless **关闭窗口时保留在托盘** is off; then it quits LinkCursor.

Both programs look next to themselves for the other one. Override this with `LINKCURSOR_AGENT_PATH` (used by the app) or `LINKCURSOR_APP_PATH` (used by the agent).

Installed with the setup program, the firewall is already set up. A build run from the source tree makes Windows Defender Firewall ask the first time the agent listens; allow it on private networks (and public ones if the network is marked public). LinkCursor uses UDP 42420 (discovery), UDP 42421 (connections) and one more UDP port the system picks for file and clipboard transfers. It uses no TCP. The prompt allows the program, which covers all of them. Other LinkCursor devices on the same network appear in the list within a few seconds.

## Arranging devices

The canvas on the **设备** page draws this PC's monitors (accent outline) and every placed peer to scale, in logical points, like the display settings of Windows and macOS. The model, snapping and sync rules are in the [core README](https://github.com/akarineren/linkcursor-core#arrangement).

- **The user's devices.** **我的设备** lists the devices this PC has been connected to (or placed), online or not, with their round-trip time. **添加设备** lists the other LinkCursor devices on the network; connecting one adds it. The **⋯** menu of a row connects or disconnects, places the device on a side, and **移除此设备** forgets it (disconnected, unplaced, unpaired; it shows up under 添加设备 again while online).
- **Staying connected.** Paired devices (and placed ones while **仅允许已配对设备** is off) connect by themselves whenever they are on the network, and again after a drop: the core does it (`auto_connect`, see the [core README](https://github.com/akarineren/linkcursor-core#how-it-works)), the agent has no timer of its own. **断开连接** holds until **连接** is chosen again here, or the other device connects; the other device does not reconnect by itself either, and its error bar stays quiet.
- **Sleep and network changes.** [power.rs](agent/src/power.rs) tells the core when this PC is about to sleep (`PowerRegisterSuspendResumeNotification`, `PBT_APMSUSPEND`): sessions end in order, so a Mac driving this PC gets control back at once. On waking, and when an IPv4 address comes or goes (`NotifyUnicastIpAddressChange`, after 0.8 s without further changes), the core finds and reconnects the devices right away. A device that vanishes without closing (it slept, or lost the network) shows **对方没有响应…** until it is back.
- **Placing a device.** The user's online devices that are not placed yet wait as **未放置** chips at the top of the canvas. Drag one next to this PC's screens. Placed devices can be dragged to another side, or along an edge to choose which part of it leads to them. This PC can be dragged too, once something is placed: that moves every placed peer the other way (each snapped back against this PC, the nearest one first). A press that does not move is not a drag. Screens are drawn to scale but inset by 3 pixels on sides facing another device, so devices that touch show a small gap; the monitors of one computer stay flush against each other, with square corners where they meet, and read as one shape.
- **Quick placement.** The **⋯** menu puts a device on the left, right, top or bottom, centred on that side, or removes it (**取消放置**, the only way to unplace: a drop outside the canvas still places the device).
- **Snapping.** While a device is dragged, a dashed outline shows where it will land: the agent answers `preview_placement` with the core's own snapping (`layout::snap_in_arrangement`, same displays and the other placed devices). The drop is then proposed to the core, which snaps it to that spot, and the canvas draws where it actually landed.
- **Three or more devices.** A device may also be dropped against another placed device instead of this PC (a row of three, whichever has the keyboard). The pointer then crosses from one device straight into the next where they touch. Every placed device must connect to this PC through others; one left floating after a change is moved back against something. Such a chained placement is not synced to the device it does not touch (see the [core README](https://github.com/akarineren/linkcursor-core#arrangement)).
- **Adding a device by address.** Where devices do not find each other (company or guest Wi-Fi, access points that keep clients apart, another subnet, a VPN), **添加设备** → **通过地址添加** takes an IP address or a device name. The core then sends its announcement straight there and the other device answers, so it shows up and connects as if it had been found ([manual.rs](https://github.com/akarineren/linkcursor-core/blob/main/src/runtime/manual.rs)); one of the two devices needing the other's address is enough. Each address shows **正在查找…**, **已找到 …**, **没有回应** or why the name could not be looked up. Saved as `manual_addresses`.
- **Device states.** A peer that is discovered but not connected says **未连接** under its name. A placed device that is offline is dimmed, says **离线** and keeps its place.
- **Sync.** A placement made on either device syncs to the other while they are connected, mirrored into its own space, and the newer edit wins.
- **Display changes.** The agent watches for monitor changes (`WM_DISPLAYCHANGE`, DPI and setting changes, plus a 2-second check for drivers that announce nothing). It hands the new monitors to the core, which tells peers and re-fits any placement that no longer touches the desktop.
- **Persistence.** Placements and the last known name, platform and displays of each device are saved to `%LOCALAPPDATA%\LinkCursor\layout.json` ([arrangement.rs](agent/src/arrangement.rs)), so placed devices still show while offline.
- **Portals.** The agent converts the touching segments into physical pixels per monitor ([displays.rs](agent/src/displays.rs)). The effect demo already uses them. Input capture (M2) will use them to decide where the cursor leaves this PC.

## Being controlled by another device

When a connected peer (for example a Mac) moves its cursor over to this PC, the agent plays that peer's input with `SendInput` ([inject.rs](agent/src/inject.rs)). The device list shows **正在控制本机** on that peer's row while that lasts (and the status dot's tooltip **正在被 … 控制键盘和鼠标**). Several peers can be connected at once (each row in the device list connects or disconnects one), but only one drives this PC at a time: when another peer starts driving it, the core tells the previous one to stop and ignores whatever that one still sends (see "One controller at a time" in the [core README](https://github.com/akarineren/linkcursor-core#how-it-works)).

Core handles everything that is the same on every platform. The agent only translates core's events to Windows:

| | Where | What |
| --- | --- | --- |
| Shortcuts | core (`remap.rs`) | **Mac 控制本机时的修饰键** (`modifier_map`) offers three choices. **Mac 习惯** (the default) swaps ⌘ and ⌃: ⌘C copies, and ⌃ is the Windows key. **保持键位** keeps keys where they are: ⌘ is the Windows key, as in Deskflow. **自定义** chooses for each of ⌘, ⌃ and ⌥ whether it plays as Ctrl, Win, Alt, Shift or nothing. Changes apply at once; keys held through a change are released first. |
| Control start/end | core | `remote_control_started` / `remote_control_ended` events. When control ends or the connection drops, core first releases every key and button the peer still holds. |
| Pointer motion | agent | Played as an absolute move on the virtual desktop: current position + delta × this monitor's scale. Windows does not accelerate it a second time, multi-monitor layouts and negative coordinates work, and sub-pixel movement is carried over. |
| Arrival | agent | A move flagged `LC_INPUT_FLAG_ABSOLUTE_POSITION` (sent by the Mac when the pointer crosses over) is converted from logical points to pixels through the monitor that contains it, so the cursor appears at the matching point of this PC's edge. |
| Keys | agent ([keymap.rs](agent/src/keymap.rs)) | Played by scan code. The key position counts, so this PC's layout and IME (for example Chinese Pinyin) decide the character. Pause and NumLock are played by virtual key. A key with no Windows scan code still types its character. |
| Scroll | agent | Trackpad points × 1.2 wheel units (Windows apps scroll about 100 DIPs per 120 units), and notched-wheel lines (10 points) become one Windows line (40 units). Fractions are carried over; horizontal scroll is supported. |
| Buttons | agent | Left, right, middle, back, forward. |
| Games | agent, core | A game or a pointer-lock web page hides the cursor and confines it (`ClipCursor`), and reads raw relative motion. While that is so, motion is played as relative moves instead, and the controlling device is told (`set_pointer_locked`, the core's `PointerLock` message) so it stops crossing at the edges: the camera keeps turning. When the game lets go, the controlling device puts the cursor back where it tracks it. |
| Staying awake | agent | While controlled, `SetThreadExecutionState` keeps the system and display on, since nobody touches this PC's own mouse or keyboard. See also [Presence](#presence). |
| No mouse attached | agent | Windows hides the cursor on a PC with no pointing device. The agent then turns on the MouseKeys accessibility feature, which makes Windows draw it (the same approach as Deskflow). It stays on afterwards, because turning it off can leave the cursor hidden. PCs with a mouse or touchpad are not touched. |
| After the peer leaves | agent ([hider.rs](agent/src/hider.rs)) | With **其他设备离开后隐藏光标** (`hide_cursor_on_leave`, on by default), when the last peer driving this PC moves its pointer away (`remote_control_ended`, reason `remote reset`), the cursor is hidden where it was left, as Universal Control, Deskflow and Mouse Without Borders do. A 1×1 topmost window that takes no focus and shows no cursor goes under it, and the hider thread's cursor display count drops below zero (`ShowCursor`), so another program's `SetCursor` does not bring it back; nothing under it gets hover effects. A low-level mouse hook, installed only meanwhile, shows the cursor at the first motion, button or wheel of this PC's own mouse, touchpad, pen or touch screen, before Windows routes that event, so the first click lands where it was aimed. Motion the agent itself played does not count: the last moves of the peer that left often land a little after its reset (the input service plays them asynchronously), so the window follows them and the cursor stays hidden. A peer driving this PC again, this PC starting to drive one, or turning the setting off show it too. A lost connection or a takeover leaves the cursor as it is. |

Every injected event carries `dwExtraInfo = 0x4C43494E` ("LCIN"). The capture hooks use it to skip input the agent itself played.

## Controlling another device

Push the pointer against the edge where a connected peer is placed (see [Arranging devices](#arranging-devices)). The edge resists with the push-through effect, then the pointer crosses: from there this PC's keyboard and mouse drive the peer.

| | Where | What |
| --- | --- | --- |
| Crossing | agent ([edge.rs](agent/src/edge.rs)) | The edge thread measures the push with raw input at the portals of connected peers, like the demo. After crossing, it sends the peer an absolute move to the matching point of its edge, 2 points inside. |
| While driving | agent ([capture.rs](agent/src/capture.rs)) | Low-level mouse and keyboard hooks, installed only while driving, swallow local input and send it through the core. The Windows cursor waits in the middle of its monitor, hidden by a tiny window with no cursor and a cursor display count below zero (`ShowCursor`, as Deskflow does), so another program setting a cursor meanwhile does not show it. Motion is measured from there, so Windows' own pointer speed and acceleration apply. Keys go by scan code (→ HID usage), auto-repeats are marked as repeats, and the wheel sends 10 points per notch. |
| Where the pointer is | core (`layout::RemotePointer`) | Tracks the pointer in the peer's space as the peer plays the same motion. It slides along edges that lead nowhere. Moving out through a portal ends the crossing at the matching local point, 2 points inside the edge. |
| Coming back | agent | The cursor reappears there with the effect's emerge animation. For the **连续切换免阻力** time, going back across needs no push. |
| Release | agent | **Ctrl+Alt+Esc** gives control back at once, just inside the edge the pointer left through. Losing the session does the same. The peer is told to release every key and button it still holds. |
| Keys held while crossing | agent | Keys and buttons already held when the pointer leaves (a drag across the edge, a held Shift) are released on this PC, not sent to the peer. |
| Moving on | agent, core | Where the driven device touches another placed device, the pointer moves straight into that one (`RemotePointer::step` reports the neighbour; the capture retargets the core). Modifiers held go along; other held keys are released on the old device. |
| Switch shortcuts | agent | **用快捷键切换设备** (`switch_hotkeys`, on by default): Ctrl+Alt+Shift+arrow keys (or Ctrl+Alt+arrow keys, `switch_modifiers`) jump to the device on that side of the current one, landing in the middle of its nearest display (`arrangement::World`). Ctrl+Alt+Shift is the default because few programs use it with the arrow keys: Ctrl+Alt alone adds cursors in VS Code and rotates the screen with some graphics drivers. Registered with `RegisterHotKey` while this PC has the pointer; the capture hooks take them while it drives a peer, and holding the modifiers on chains jumps. A combination another program registered first shows as a notice on the settings row (`switch_hotkeys_blocked`). |
| Speeds per device | agent | **鼠标和滚动…** in a device's **⋯** menu (`peer_input`): pointer speed 0.25–4×, scrolling speed 0.25–5×, reversed scrolling. Applied here while this PC drives that device, before the pointer is tracked, so the two never disagree on where it is. |
| Game on the peer | agent | While the peer says a game holds its pointer (`peer_pointer_lock`), every bit of motion goes to it and no edge leads anywhere; Ctrl+Alt+Esc and the switch shortcuts still work. |
| Taken over | core, agent | Only one device drives at a time. If a peer starts driving this PC while it drives another, or another device takes over the peer this PC drives, the core resets that peer and reports `control_revoked`. The edge thread then ends the capture. When a peer now drives this PC, the cursor stays where that peer puts it; otherwise it comes back as after Ctrl+Alt+Esc, with the emerge animation. Pushing the pointer into a peer while being driven takes this PC back: whoever drove it is told to stop. |

The demo and real crossings share the edge thread: a process can register raw mouse input for only one window. While the demo is on, portals wrap around instead of crossing.

Known limits:
- The secure desktop (Ctrl+Alt+Del, UAC prompts) cannot be hooked; it takes input back from the hooks.
- With mixed DPI across several monitors, logical monitor positions can leave gaps. A peer placed in such a gap is not reachable.

Windows of elevated programs (such as Task Manager), UAC prompts and the lock screen ignore input played by a normal process; installed with the setup program, they get it through the input helper (see [Elevated windows, UAC prompts and the lock screen](#elevated-windows-uac-prompts-and-the-lock-screen)). Scroll speeds and Mac function-key behavior still need checking against a real Mac.

## Elevated windows, UAC prompts and the lock screen

Windows keeps a normal process like the agent from playing input into windows of elevated programs (UIPI) and onto the Winlogon desktop, where UAC prompts and the lock screen live. As in Mouse Without Borders and Deskflow, a process running as LocalSystem does it instead. Unlike them, only the input runs with that privilege: the network, pairing, clipboard, files and UI stay in the agent, running as the user.

```
linkcursor-agent (user)  ── named pipe ──►  linkcursor-input (LocalSystem, in the user's session)
                                                   ▲ started, restarted on session change or exit
                                             linkcursor-service (LinkCursorInput service, automatic)
```

| | What |
| --- | --- |
| Service ([service.rs](input/src/bin/service.rs)) | Keeps one helper running in the session at the console: its own LocalSystem token moved to that session, `winsta0\default`, in a job that ends it with the service. Restarted after a failure (1 s). `--install` / `--uninstall` / `--elevated-input on\|off` are for the setup program and the settings page. |
| Helper ([input.rs](input/src/bin/input.rs)) | Plays what the agent sends: `SendInput` values (keys, buttons, wheel, absolute moves) and motion in pixels (it reads the pointer on the input desktop, which the agent cannot see there). Every 200 ms it checks which desktop has input; its injecting thread has no windows or hooks, so it follows with `SetThreadDesktop`. No network, no files. |
| Agent ([elevated.rs](agent/src/elevated.rs)) | Connects when the helper exists and hands it every event while the setting is on; otherwise plays input itself as before (a build from the source tree, the setting off). Windows then refuses input meant for a UAC prompt or the lock screen (`SendInput` fails with access denied); the agent logs why once and shows a notification saying what makes it work (the setup program, or turning the setting on), instead of failing silently. When a UAC prompt or the lock screen takes input while this PC drives a peer, the peer is given back so the prompt can be answered here. |
| Protocol ([lib.rs](input/src/lib.rs)) | Length-prefixed binary frames: `Hello`, `Input`, `MoveBy`, `RemoteControl` to the helper; `State { enabled, secure }` back. |
| Logs | `service.log` and `input.log` in `logs` beside the programs: `%ProgramFiles%\LinkCursor\logs` when installed. Not `%ProgramData%`, where any user could create the folder first and redirect LocalSystem's writes. |

Safeguards, since the helper acts with LocalSystem's rights:

- **The setting** (`HKLM\SOFTWARE\LinkCursor\ElevatedInput`, a setup task checked by default) can only be changed by an administrator: the settings switch runs `linkcursor-service.exe --elevated-input on|off` elevated (UAC asks). Off, the helper plays nothing.
- **Who talks to whom.** The pipe (`\\.\pipe\LinkCursor.Input.<session>`) admits only LocalSystem and the session's user, never remote clients, and is created as the first instance of its name, so nothing can take the name first. Each end checks the other's process: the helper accepts only `linkcursor-agent.exe` from its own folder in its own session, and the agent talks only to `linkcursor-input.exe` from its folder. Installed in Program Files, only administrators can put a program there.
- **The Winlogon desktop only while controlled.** UAC prompts and the lock screen get input only while a peer is driving this PC (the agent says so); other times only the user's desktop does.
- **No new network surface.** The helper accepts nothing from the network; whatever it plays came from a paired, encrypted session through the agent.

Not covered: the sign-in screen before anyone has signed in (the agent runs per user, so nothing is connected yet), and Ctrl+Alt+Del (a later `SendSAS` from the service).

**Trying it without installing.** `linkcursor-input.exe --dev` runs the helper as the current user from a build folder (it ignores the setting); the agent from the same folder connects to it. It cannot reach elevated windows or the Winlogon desktop that way, but the pipe, the checks and the routing are the same.

## Clipboard

Turn on **同步剪贴板和文件** on the **设置** page. The clipboard then follows control, as in Deskflow. When this PC starts driving a peer, or a peer driving it gives control back, the peer gets this PC's clipboard. The other way round works the same. The core sends contents only if they changed since the last exchange with that peer, which also keeps them from echoing back. The whole design, including file transfer plans, is in [linkcursor-core/docs/clipboard-and-files.md](https://github.com/akarineren/linkcursor-core/blob/main/docs/clipboard-and-files.md).

[clipboard.rs](agent/src/clipboard.rs) is the Windows end:

| What | How |
| --- | --- |
| Shared formats | Text (`CF_UNICODETEXT` ↔ UTF-8), HTML (the fragment of `"HTML Format"`, rebuilt into CF_HTML on write), images (`"PNG"`, or `CF_DIB` converted to PNG; written back as both). At most 8 MiB in all. |
| Changes | `AddClipboardFormatListener` on a message-only window on its own thread. A peer's clipboard written here is not reported back: the sequence number after the write is skipped. |
| Privacy | Contents marked with `ExcludeClipboardContentFromMonitorProcessing` or `Clipboard Viewer Ignore`, or `CanIncludeInClipboardHistory` = 0 (password managers), are never shared. |
| Copied files | Files copied in Explorer (`CF_HDROP`) are reported as a `text/uri-list`, with their names as text. The core sends a manifest, and the peer pulls the files while this PC runs. Files a peer copied land in `Downloads\LinkCursor` (folders kept, names never overwritten). They are then put on the clipboard as `CF_HDROP` with a copy drop effect, so Ctrl+V in Explorer copies them. |
| Receiving files | The core's own limit is set to 0, so every transfer waits for the agent. Up to 512 MB it is taken at once. A larger one asks first in a message box. While sharing is off it is refused. A failed transfer shows as the error bar. |
| Progress | Files on their way, either way, show as cards on the **设备** page with their progress and a button to stop them (the snapshot's `transfers`, from the core's `files_receiving`, `files_progress`, `files_sending` and `files_send_ended`; `cancel_transfer`). When files arrive, a notification says so; clicking it opens `Downloads\LinkCursor`. |
| Setting | Off by default. Saved to `%LOCALAPPDATA%\LinkCursor\settings.json`. The core always offers the clipboard and files to peers so the setting can change at run time. While it is off, the agent reports an empty clipboard, ignores peers' clipboards, clears any it receives so the core does not pass them on, and refuses offered files. |

## Edge push-through effect

Inspired by Universal Control. The effect is drawn on the device the cursor is moving into:

| Stage | What you see |
| --- | --- |
| Approach | While you push against the edge, a soft bump grows out of it under the pointer. It grows quickly at first and then slower, which is the resistance. Near the crossing point a pointer dot appears at its tip. |
| Release | Stop pushing, or pull away, and the bump springs back. |
| Pass | Once you have pushed far enough, the bump flicks outward and snaps shut as the cursor goes through. |
| Emerge | Where the cursor comes in, the edge flicks back with a slight wobble, the dot shrinks onto the pointer and a ring fades out. |

All motion uses a damped spring with SwiftUI-style `response` and damping ratio ([spring.rs](agent/src/spring.rs)). macOS can use the same values with `Spring(response:dampingRatio:)`, so both platforms move identically.

**Tuning it on one machine.** The tuning page exists in development (Debug) builds only, since the animation is going away in releases. Open it with the button next to the settings button on the **设备** page and turn on **效果演示**. The portals of the arrangement then act as if the placed devices sat beyond them and the screen wrapped around. With nothing placed, the whole right edge of the desktop stands in for a device:

- Push the mouse against a portal. The bump grows; when you have pushed far enough, the cursor comes out of the opposite edge, as it would on the other device.
- Push against that opposite edge to come back. Returning never has resistance.
- For the **连续切换免阻力** time after a crossing, crossing again needs no push either.
- **播放预览** plays the whole sequence on the primary monitor without touching the mouse.

Every slider takes effect on the next push. The demo stops when the settings window closes.

The parameters are saved to `%LOCALAPPDATA%\LinkCursor\effect.json` ([effect.rs](agent/src/effect.rs)). Units are platform-neutral so other clients can share the file format:

| Key | Default | Meaning |
| --- | --- | --- |
| `push_distance` | 260 | Raw mouse counts to push before the first crossing. This is unaccelerated mouse movement, so the feel depends on the mouse's DPI. |
| `release_delay` | 0.25 s | Time without pushing after which the bump springs back. |
| `warm_seconds` | 20 s | After a crossing, crossings within this time need no push. |
| `corner_dead_zone` | 48 pt | Distance from screen corners in which the edge does not react. |
| `spring_response` | 0.32 s | Spring response; smaller is snappier. |
| `spring_damping` | 0.78 | Damping ratio; 1 settles without overshoot, lower values wobble. |
| `bump_depth` / `bump_width` | 26 / 72 pt | How far the bump bulges in, and half its length along the edge. |
| `dot_size` | 18 pt | Diameter of the pointer dot. |
| `opacity` | 0.92 | Opacity of the material. |
| `ripple` | true | The ring where the cursor comes in. |
| `theme` | `auto` | `auto` follows the Windows app theme; `light` / `dark` force one. |

**How it is drawn.** [overlay.rs](agent/src/overlay.rs) owns a topmost, click-through window. It is created with `WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT` and covers the virtual desktop only while an effect is on screen; otherwise it is hidden. Each active spot on an edge is one DirectComposition visual with a small Direct2D surface: the bump outline, a Gaussian shadow, the dot and the ripple. Its own thread steps the springs once per compositor frame (`DwmFlush`) and sleeps when nothing moves, so an idle agent uses no CPU. The dot and ripple have a contrasting rim, like the system pointer, so they stay visible over both light and dark content.

**How the push is measured.** [edge.rs](agent/src/edge.rs) registers for raw mouse input (`RIDEV_INPUTSINK`). Raw input keeps reporting motion while the cursor is pinned against the edge. Only outer edges of the desktop count, meaning edges with no monitor beyond them, and only within a portal. A push crosses once it is far enough (`push_distance`) and has lasted the switch delay (see [Settings](#settings)); a push that is far enough crosses when the delay is up even if the pointer holds still. Sending the push progress to the peer, so it draws the approach on its own edge, is still to come (M3).

## Settings

The **设置** page. Preferences live in `%LOCALAPPDATA%\LinkCursor\settings.json` ([settings.rs](agent/src/settings.rs)); the agent applies every change at once.

| Setting | Key | How it works |
| --- | --- | --- |
| 共享键盘和鼠标 (设备 page, tray panel) | `sharing` | Off pauses both directions: the edge thread does not cross, a peer being driven is given back, and the core refuses peers (`Core::set_remote_control_allowed(false)`): one that drives this PC, or starts to, gets `ControlRevoke` and none of its input is played. Sessions and the clipboard stay. |
| 开机时启动 | (registry) | The `LinkCursor` value under `HKCU\…\CurrentVersion\Run` starts the agent at sign-in. The registry is the truth; it is not stored in `settings.json`. |
| 关闭窗口时保留在托盘 | `keep_in_tray` | Off: closing the main window also quits the agent. |
| 主题 | `theme` | `system`, `light` or `dark`, for both windows. |
| 语言 | `language` | `system` (Windows' display language: Simplified Chinese, English for every other one), `zh-CN` or `en-US`. The app reads it from `settings.json` before its first window and restarts itself on the settings page when it changes; the agent's own text (tray menu, notifications, errors) switches at once. See [Languages](#languages). |
| 切换延迟 | `switch_delay_ms` | 0–500 ms (default 150): how long a push must last before it crosses. Crossing back within the effect's warm time needs no push. |
| 屏幕角落不触发切换 | `corner_guard` | On: the effect's `corner_dead_zone` (48 pt) near each corner never crosses. |
| 全屏游戏时暂停 | `pause_in_fullscreen` | No new push starts while `SHQueryUserNotificationState` reports a Direct3D full-screen game, a full-screen app or presentation mode. |
| 用快捷键切换设备 | `switch_hotkeys`, `switch_modifiers` | On by default with Ctrl+Alt+Shift (`7`); Ctrl+Alt (`3`) is the other choice. The arrow keys with those modifiers move the pointer to the device on that side; see [Controlling another device](#controlling-another-device). |
| Mac 控制本机时的修饰键 | `modifier_map` | Mac 习惯 (⌘ and ⌃ swapped) by default. See [Being controlled by another device](#being-controlled-by-another-device). |
| 其他设备离开后隐藏光标 | `hide_cursor_on_leave` | On by default. See [Being controlled by another device](#being-controlled-by-another-device) (After the peer leaves). |
| 在其他设备上工作时保持唤醒 | `keep_awake` | On by default. See [Presence](#presence). |
| 一起锁定 | `lock_sync` | Off by default. See [Presence](#presence). |
| 通过地址添加 (添加设备) | `manual_addresses` | Up to 32 host names or IPv4 addresses (optionally `:port`, the discovery port). See [Arranging devices](#arranging-devices). |
| 鼠标和滚动 (device menu) | `peer_input` | `{"<device ID>": {"pointer_speed", "scroll_speed", "invert_scroll"}}`. See [Controlling another device](#controlling-another-device). |
| 诊断日志 | (none) | **打开日志文件夹** opens `%LOCALAPPDATA%\LinkCursor\logs`: `agent.log` ([log.rs](agent/src/log.rs)), moved aside to `agent.1.log` past 2 MB. It records connections, pairing, transfers and errors with the local time (never clipboard contents, keys or the pairing code), and panics. The input service and helper log to `%ProgramFiles%\LinkCursor\logs`. |
| 锁定到当前屏幕 | `lock_hotkey` | `{"modifiers": MOD_* bits, "key": virtual key}`, default Ctrl+Alt+L. Registered with `RegisterHotKey` on the edge thread; while driving a peer the capture hooks recognise it instead. Locked, the pointer neither crosses into a peer nor comes back from the one it drives (it slides along the edge); the main window says so. |
| 同步剪贴板和文件 | `clipboard` | See [Clipboard](#clipboard). |
| 端口 | `port` | The core's UDP port for connections (`null`: 42421). Applies after LinkCursor restarts; must not be the discovery port 42420. |
| 加密传输 (TLS) | (none) | Always on and not switchable: every connection is encrypted. Off only when the core could not start (no certificate or port). See [Pairing and encryption](#pairing-and-encryption). |
| 仅允许已配对设备 | `paired_only` | On (default): a device connects only after the users of both devices confirmed its pairing code once. Off: unknown devices connect without pairing (still encrypted). Applied at once: turning it on drops connected devices that are not paired. |
| 本机指纹 | (none) | The SHA-256 fingerprint of this PC's certificate, shortened; the copy button copies all of it. |
| 被控时可操作管理员窗口、UAC 和锁屏 | (machine-wide) | `HKLM\SOFTWARE\LinkCursor\ElevatedInput`, set by the setup program (on by default). Changing it asks for an administrator. Disabled when the input service is not installed. See [Elevated windows, UAC prompts and the lock screen](#elevated-windows-uac-prompts-and-the-lock-screen). |

## Presence

[presence.rs](agent/src/presence.rs), with the core's presence messages (see the [core README](https://github.com/akarineren/linkcursor-core#how-it-works)):

| | What |
| --- | --- |
| Activity | Every 5 s, if this PC's keyboard or mouse was used in the last 10 s (`GetLastInputInfo`), peers are told (`report_user_activity`; the core sends it at most every 20 s). The nudges below are not counted, so two devices never keep each other awake for ever. |
| Staying awake (`keep_awake`) | While a peer reports activity (`peer_active`), the display stays on (`SetThreadExecutionState`) and a mouse move of zero pixels every 30 s resets the idle timer that the screen saver and the idle lock go by, as Mouse Without Borders does. A minute after the last report it lets go. Not on a locked desktop. |
| Locking together (`lock_sync`) | When this PC is locked (`WM_WTSSESSION_CHANGE`), peers are told (`lock_peers`); a peer's `lock_requested` locks this PC (`LockWorkStation`). Each side follows its own setting, and a lock caused by a peer is not passed on again. |

## Languages

The app's text lives in `app\Strings\en-US\Resources.resw` and `app\Strings\zh-CN\Resources.resw`: XAML takes it through `x:Uid`, code through `LinkCursorApp::Text` / `Format` ([Strings.h](app/Strings.h)). English is the default language, so any language without its own file shows English. The agent keeps its few strings side by side in the code (`i18n::tr`, [i18n.rs](agent/src/i18n.rs)). Both follow the `language` setting. The development-only animation page is not translated.

## Pairing and encryption

The core does the protocol work (QUIC with TLS 1.3, certificates, the pairing code; see the [core README](https://github.com/akarineren/linkcursor-core#security)). The agent stores what it has to keep and the app asks the user.

| | Where | What |
| --- | --- | --- |
| Certificate | core | A self-signed certificate and its key, made on first start, in `%LOCALAPPDATA%\LinkCursor\identity.pem` (`CoreConfig::identity_path`). Peers know this PC by its fingerprint. Deleting the file gives this PC a new identity, and paired devices then refuse it until they pair again. |
| Paired devices | agent ([trust.rs](agent/src/trust.rs)) | Device ID → fingerprint, in `%LOCALAPPDATA%\LinkCursor\trusted.json`. Passed to the core at start (`trusted_peers`); updated on the core's `paired` and `unpaired` events. **移除此设备** also unpairs. |
| Pairing | core, app | When a device wants to pair, the core reports `pairing_requested` with a six-digit code computed from both certificates. The agent opens the main window, and the **配对** dialog ([MainWindow.Pairing.cpp](app/MainWindow.Pairing.cpp)) shows the code. **配对** accepts and waits for the other device; **取消** (or Esc) declines, and the other device says **对方拒绝了配对**. Two minutes without an answer ends it. |
| Device rows | app | A device waiting for its code to be confirmed says **等待配对确认…**. |
| Refusals | agent | A paired device that shows another certificate is refused; the error bar says its identity certificate changed and to remove and pair it again. When the other device refuses this PC (it removed it, or takes only paired devices), the error bar says so and suggests **连接** to pair again. Neither side then reconnects by itself. |

## Agent IPC protocol

The pipe is `\\.\pipe\LinkCursor.Agent.<user SID>`. Its ACL lets only the signed-in user and LocalSystem open it, and it rejects remote clients. Each message is one UTF-8 JSON object followed by `\n`.

App → agent:

| Message | Effect |
| --- | --- |
| `{"type":"hello","protocol":15}` | Sent first. The agent replies with an `error` if the protocol version differs. |
| `{"type":"connect","peer_id":"…"}` | Connect to a discovered peer. |
| `{"type":"disconnect","peer_id":"…"}` | Close the session with that peer. Sessions with other peers stay open. |
| `{"type":"set_placement","peer_id":"…","x":1440,"y":-11}` | Place a peer near this offset (logical points, this PC's space). The core snaps it to a valid spot and syncs it. |
| `{"type":"preview_placement","peer_id":"…","x":1440,"y":-11}` | Where `set_placement` would put it, without placing it. Answered with `{"type":"placement_preview","peer_id":"…","offset":{"x":…,"y":…}}` (`offset` is `null` when there is no room). |
| `{"type":"remove_placement","peer_id":"…"}` | Remove the peer's placement. |
| `{"type":"place_beside","peer_id":"…","side":"right"}` | Place a peer centred on one side: `left`, `right`, `top` or `bottom`. |
| `{"type":"set_clipboard_sync","enabled":true}` | Share the clipboard with peers, or stop; saved in `settings.json`. |
| `{"type":"update_settings","values":{"sharing":false}}` | Change some settings (keys of [Settings](#settings), plus `launch_at_login`). Invalid values are refused with an `error` that can be shown to the user. |
| `{"type":"forget_device","peer_id":"…"}` | Disconnect, unplace, unpair and stop listing a device. |
| `{"type":"accept_pairing","peer_id":"…"}` | The user confirmed the pairing code. |
| `{"type":"reject_pairing","peer_id":"…"}` | The user declined the pairing; the connection closes. |
| `{"type":"clear_error"}` | The user closed the error bar: `last_error` is cleared. |
| `{"type":"cancel_transfer","transfer":N}` | Stop files being received or sent (`transfers`). |
| `{"type":"quit"}` | Quit the agent, as the tray's 退出. |
| `{"type":"set_effect","params":{…}}` | Replace the effect parameters (keys as in `effect.json`; missing keys take their defaults) and save them. |
| `{"type":"reset_effect"}` | Restore the default parameters. |
| `{"type":"set_effect_demo","enabled":true}` | Start or stop the single-machine demo. The demo belongs to the connection that started it and stops when that connection closes. |
| `{"type":"effect_preview"}` | Play the effect once on the primary monitor. With `"hold":0.6` the bump freezes at that progress until the next effect command, which helps when inspecting it. |

Agent → app:

| Message | Meaning |
| --- | --- |
| `{"type":"hello","protocol":15,"agent_version":"0.1.0"}` | Sent once when the app connects. |
| `{"type":"state", …}` | The full snapshot, sent right after `hello` and again after every change. |
| `{"type":"error","message":"…"}` | A request failed. |

A `state` message looks like this:

- `sessions` lists every peer this PC is connecting or connected to, one entry per peer and possibly several. `status` is `connecting` or `connected`, `remote_control` is true while that peer drives this PC, `latency_ms` is the round trip (from the core's `latency` event, about once a second), and `paired` is true when the peer showed the certificate it was paired with.
- `elevated_input` is `{"available", "enabled"}`: the input helper is connected, and the machine-wide setting is on.
- `fingerprint` is this PC's certificate fingerprint (64 hex digits; `null` until the core has started). `paired_peers` lists the device IDs of paired devices. `pairing` is `null`, or the pairing waiting for the user: `{"peer_id", "name", "platform", "code", "accepted"}` (`accepted` once this side confirmed).
- `layout` is what the arrangement draws, in logical points:
  - `displays` are this PC's monitors.
  - `devices` lists every known peer, online or not. Each has its `displays` in its own space (a default 1440 × 900 until it describes them), its `offset` in this PC's space (`null` while not placed), the `side` of this PC it touches, `remembered` (connected at least once, or placed: one of the user's devices) and `last_seen_ms` (Unix milliseconds).
- `settings` holds the [Settings](#settings); `launch_at_login`, `control_port` (the port in use now), `locked` (the lock shortcut is on) and `agent_version` sit beside it.
- `last_error` is omitted when there is none. A session the user or the peer ended normally does not set it, nor do file transfers that were declined or replaced by a newer copy. The app shows it when it changes, and `clear_error` removes it once the user closes it.
- `quitting` turns true just before the agent exits because the user quit LinkCursor: the app closes too and does not start the agent again.
- `manual_addresses` lists the addresses the user added, as the core reports them: `{"address", "status"}` with `status` `searching`, `found` (with `peer_id` and `name`), `no_response`, or `unresolved` (with `error`).
- `switch_hotkeys_blocked` is true when another program took one of the switch shortcuts first.
- `transfers` lists files on their way: `{"transfer", "peer_id", "peer_name", "direction"` (`receiving` or `sending`), `"name"` (the first top-level item), `"items"`, `"count"` (files), `"done"`, `"total"}` (bytes).
- `effect` holds the current parameters.
- `effect_revision` changes only when the agent reloads them (at start-up or on `reset_effect`), not on the app's own `set_effect`. The app copies `effect` into its controls only when the revision changes, so a snapshot cannot move a slider the user is dragging.
- `demo_side` is `local` or `remote` while the demo runs.
- `clipboard_sync` is true while the clipboard is shared.

```json
{
  "type": "state",
  "device_id": "3f0c…",
  "device_name": "DESKTOP-1234",
  "core": "running",
  "peers": [{ "id": "a1b2…", "name": "MacBook Pro", "platform": "macos", "address": "192.168.1.20", "port": 42421 }],
  "sessions": [{ "peer_id": "a1b2…", "name": "MacBook Pro", "status": "connected", "remote_control": false }],
  "layout": {
    "displays": [{ "x": 0.0, "y": 0.0, "width": 1440.0, "height": 960.0, "scale": 2.0 }],
    "devices": [{
      "peer_id": "a1b2…", "name": "MacBook Pro", "platform": "macos",
      "displays": [{ "x": 0.0, "y": 0.0, "width": 1512.0, "height": 982.0, "scale": 2.0 }],
      "offset": { "x": 1440.0, "y": -11.0 },
      "side": "right"
    }]
  },
  "effect": { "push_distance": 260.0, "spring_response": 0.32, "spring_damping": 0.78, "bump_depth": 26.0, "…": "…" },
  "effect_revision": 1,
  "effect_demo": false,
  "demo_side": "local"
}
```

The agent sends whole snapshots instead of individual events, so the app never has to rebuild state from an event history. Bump `PROTOCOL_VERSION` in [ipc.rs](agent/src/ipc.rs) and `ProtocolVersion` in [AgentClient.cpp](app/AgentClient.cpp) together when a message changes incompatibly.

## Next steps

1. **Receive input (M1):** done; see [Being controlled by another device](#being-controlled-by-another-device). It still needs a pass with a real Mac for scroll speed and function keys.
2. **Arrangement:** done; see [Arranging devices](#arranging-devices). The macOS app has its own drag canvas.
3. **Send input (M2):** done; see [Controlling another device](#controlling-another-device). It still needs a pass with the real Mac for pointer speed and keys.
4. **Edge effect with a real peer (M3):** the renderer, tuning, screen info and portals exist. What remains in the core is `EdgeApproach` progress, `Enter` with the entry position on the portal, and `Return` when the cursor is pushed back. That lets the controlling device's push draw the approach on the target's edge. The same effect also has to be built on macOS from the shared parameters.
5. **Productize (M4):** the setup program, the self-contained runtime, firewall rules and the input service for elevated windows, UAC prompts and the lock screen are done (see [Setup program](#setup-program)). The agent's log file is done (see [Settings](#settings), 诊断日志). Remaining: code signing, Ctrl+Alt+Del through the service (`SendSAS`), and updates.
6. **Security:** done; see [Pairing and encryption](#pairing-and-encryption). Next: send pointer motion as QUIC datagrams, so a lost packet never holds up the ones after it.

## Security

All traffic between devices is encrypted with TLS 1.3, and each side knows the other's certificate. With **仅允许已配对设备** on (the default), only devices the user paired can connect. With it off, any LinkCursor device on the network can connect: encrypted, but nobody checked who it is. The pipe between the app and the agent is local to the signed-in user (see [Agent IPC protocol](#agent-ipc-protocol)).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security problems as described in [SECURITY.md](SECURITY.md).

## License

LinkCursor is licensed under the [Mozilla Public License 2.0](LICENSE). The third-party software it includes is listed in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).

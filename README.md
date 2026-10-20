# LinkCursor for Windows

The Windows client for LinkCursor. It is two programs:

| Program | Language | Role |
| --- | --- | --- |
| `linkcursor-agent.exe` ([agent/](agent/)) | Rust | Always running. Links `linkcursor-core` directly, owns the tray icon, plays input from a controlling peer, draws the edge push-through effect, and serves a named pipe for the settings app. Keyboard and mouse capture will live here too, because input must never wait on UI or a garbage collector. |
| `LinkCursor.exe` ([app/](app/)) | C++/WinRT, WinUI 3 | The settings window: nearby devices, their arrangement around this PC, and effect tuning. It starts only when opened (from the tray or directly) and exits when closed, so it costs no memory the rest of the time. |

```
 LinkCursor.exe (WinUI 3)  ── named pipe, JSON lines ──►  linkcursor-agent.exe (Rust)
   devices, arrangement,                                    ├ tray icon
   effect tuning                                            ├ arrangement (layout.json, display watcher)
                                                            ├ edge effect overlay (Direct2D + DirectComposition)
                                                            ├ linkcursor-core ── UDP 42420 / TCP 42421 ──► other devices
                                                            ├ input injection (SendInput)
                                                            └ (next) hooks to capture local input
```

Status:
- The agent discovers peers and connects to them.
- It plays the mouse and keyboard input of a peer that controls this PC (for example the macOS app).
- It controls a peer when the pointer is pushed through the edge it is placed at, with the push-through effect on this PC's side.
- Peers can be arranged around this PC's screens, and the arrangement syncs with them.
- The clipboard can follow control to and from peers (off by default).
- The edge effect can be tuned with a single-machine demo.

## Layout

```
build.cmd, build.ps1         one-command build from any terminal
agent/
  Cargo.toml                 depends on ../../linkcursor-core
  build.rs                   embeds linkcursor-agent.manifest (PerMonitorV2 DPI, asInvoker)
  src/main.rs                single instance, core start-up, shutdown
  src/state.rs               folds core events into the snapshot the app renders
  src/ipc.rs                 named-pipe server
  src/services.rs            what IPC requests act on (core, arrangement, effect, demo)
  src/displays.rs            monitors in logical points, display-change watcher, physical portals
  src/arrangement.rs         saved placements and known devices (%LOCALAPPDATA%\LinkCursor\layout.json)
  src/effect.rs              effect parameters (%LOCALAPPDATA%\LinkCursor\effect.json)
  src/spring.rs              damped spring with SwiftUI-style response/damping
  src/overlay.rs             click-through overlay that draws the edge effect
  src/edge.rs                edge thread: raw-input push tracking at the portals, crossing, the demo
  src/capture.rs             while driving a peer: low-level hooks, hidden parked cursor, return
  src/clipboard.rs           the Windows clipboard ↔ the core: text, HTML (CF_HTML), PNG (and CF_DIB)
  src/settings.rs            other preferences (%LOCALAPPDATA%\LinkCursor\settings.json)
  src/tray.rs                notification-area icon and menu
  src/identity.rs            device ID (%LOCALAPPDATA%\LinkCursor\device-id) and name
  src/inject.rs              plays a controlling peer's input with SendInput
  src/keymap.rs              USB HID usage → Windows scan code
  src/win.rs                 Win32 helpers: mutex, user SID, pipe security
app/
  LinkCursor.vcxproj         unpackaged WinUI 3; also builds the agent with Cargo
  Directory.Packages.props   NuGet versions (central package management)
  App.xaml / MainWindow.xaml settings window: devices, arrangement, effect tuning (Mica, Fluent controls)
  MainWindow.Arrangement.cpp the 排列 page: to-scale canvas, dragging, tray of unplaced devices
  AgentClient.{h,cpp}        named-pipe client; starts the agent if needed
  PeerItem.{idl,h,cpp}       row model for the device list
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
- The **Windows App Runtime 2.4 or later** (framework package `Microsoft.WindowsAppRuntime.2`). The app is framework-dependent and does not bundle it. Recent Windows 11 machines usually have it; otherwise install it with `winget install Microsoft.WindowsAppRuntime.2`. The minimum comes from the `Microsoft.WindowsAppSDK.Runtime` version in [Directory.Packages.props](app/Directory.Packages.props).

## Building

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
- runs MSBuild on `app\LinkCursor.vcxproj`. That restores NuGet packages, builds `LinkCursor.exe`, runs `cargo build` for the agent, and copies `linkcursor-agent.exe` next to the app. Everything ends up in `app\<Platform>\<Configuration>\LinkCursor\`. Release builds the agent with `--release`.

Build messages are in Chinese by default. Pass `-Language en-US` (or another culture name) to change this. MSBuild and NuGet messages follow it right away. The C++ compiler only speaks the Visual Studio language packs that are installed, so add one for compiler messages too:

```powershell
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" modify `
    --installPath "${env:ProgramFiles(x86)}\Microsoft Visual Studio\18\BuildTools" --addProductLang zh-CN --passive
```

Inside a Developer PowerShell you can also call MSBuild directly: `msbuild app\LinkCursor.vcxproj -restore -p:Configuration=Debug -p:Platform=x64`.

A Release x64 build is about 3.2 MB of files (1.2 MB zipped): the agent is 1.3 MB, `LinkCursor.exe` 0.6 MB and the Windows App SDK bootstrapper 0.4 MB. The other 0.8 MB is a WebView2 DLL that the WinUI package brings in and the app does not use. At run time the agent uses about 13 MB of memory and no CPU while idle; the settings window uses about 120 MB, but only while it is open.

To work on the agent alone:

```powershell
cd linkcursor-windows\agent
cargo build
cargo run          # debug builds keep a console window for logs
```

## Running

Start either program:

- `LinkCursor.exe` connects to the agent and starts `linkcursor-agent.exe` from the same folder if it is not running yet.
- `linkcursor-agent.exe` puts an icon in the notification area. Left-click it, or choose **打开 LinkCursor** from its right-click menu, to open the settings window. **退出** stops the agent and the core.

Both programs look next to themselves for the other one. Override this with `LINKCURSOR_AGENT_PATH` (used by the app) or `LINKCURSOR_APP_PATH` (used by the agent).

The first time the agent listens, Windows Defender Firewall asks for permission. Allow it on private networks; LinkCursor needs UDP 42420 and TCP 42421. Other LinkCursor devices on the same network appear in the list within a few seconds.

## Arranging devices

The **排列** page draws this PC's monitors (in the accent color) and every known peer to scale, in logical points, like the display settings of Windows and macOS. The model, snapping and sync rules are in the [core README](../README.md#arrangement).

- **Placing a device.** Discovered devices that are not placed yet wait under **未放置的设备**. Drag one onto the canvas next to this PC's screens. Placed devices can be dragged to another side, or along an edge to choose which part of it leads to them. Dropping a device outside the canvas removes its placement.
- **Quick placement.** On the **设备** page, each device's **位置** menu puts it on the left, right, top or bottom, centred on that side, or removes it (**不放置**).
- **Snapping.** The app only proposes where a device was dropped. The core snaps it to the nearest valid spot, and the canvas then draws where it actually landed.
- **Device states.** A connected device has an accent outline. A device that is discovered but not connected has a solid outline. A placed device that is offline has a dashed outline and keeps its place.
- **Sync.** A placement made on either device syncs to the other while they are connected, mirrored into its own space, and the newer edit wins.
- **Display changes.** The agent watches for monitor changes (`WM_DISPLAYCHANGE`, DPI and setting changes, plus a 2-second check for drivers that announce nothing). It hands the new monitors to the core, which tells peers and re-fits any placement that no longer touches the desktop.
- **Persistence.** Placements and the last known name, platform and displays of each device are saved to `%LOCALAPPDATA%\LinkCursor\layout.json` ([arrangement.rs](agent/src/arrangement.rs)), so placed devices still show while offline.
- **Portals.** The agent converts the touching segments into physical pixels per monitor ([displays.rs](agent/src/displays.rs)). The effect demo already uses them. Input capture (M2) will use them to decide where the cursor leaves this PC.

## Being controlled by another device

When a connected peer (for example a Mac) moves its cursor over to this PC, the agent plays that peer's input with `SendInput` ([inject.rs](agent/src/inject.rs)). The settings window shows **正在被 … 控制键盘和鼠标** while that lasts. Several peers can be connected at once (each row in the device list connects or disconnects one). Whichever peer is sending input is the one played.

Core handles everything that is the same on every platform. The agent only translates core's events to Windows:

| | Where | What |
| --- | --- | --- |
| Shortcuts | core (`remap.rs`) | Keys keep their position by default: a Mac's ⌘ is the Windows key, as in Deskflow. With the core's `shortcut_remap` on, ⌘ arrives as Ctrl instead (⌘C copies) and the Mac's Control stays Ctrl. The agent leaves it at the default. |
| Control start/end | core | `remote_control_started` / `remote_control_ended` events. When control ends or the connection drops, core first releases every key and button the peer still holds. |
| Pointer motion | agent | Played as an absolute move on the virtual desktop: current position + delta × this monitor's scale. Windows does not accelerate it a second time, multi-monitor layouts and negative coordinates work, and sub-pixel movement is carried over. |
| Arrival | agent | A move flagged `LC_INPUT_FLAG_ABSOLUTE_POSITION` (sent by the Mac when the pointer crosses over) is converted from logical points to pixels through the monitor that contains it, so the cursor appears at the matching point of this PC's edge. |
| Keys | agent ([keymap.rs](agent/src/keymap.rs)) | Played by scan code. The key position counts, so this PC's layout and IME (for example Chinese Pinyin) decide the character. Pause and NumLock are played by virtual key. A key with no Windows scan code still types its character. |
| Scroll | agent | Trackpad points × 1.2 wheel units (Windows apps scroll about 100 DIPs per 120 units), and notched-wheel lines (10 points) become one Windows line (40 units). Fractions are carried over; horizontal scroll is supported. |
| Buttons | agent | Left, right, middle, back, forward. |
| Staying awake | agent | While controlled, `SetThreadExecutionState` keeps the system and display on, since nobody touches this PC's own mouse or keyboard. |
| No mouse attached | agent | Windows hides the cursor on a PC with no pointing device. The agent then turns on the MouseKeys accessibility feature, which makes Windows draw it (the same approach as Deskflow). It stays on afterwards, because turning it off can leave the cursor hidden. PCs with a mouse or touchpad are not touched. |

Every injected event carries `dwExtraInfo = 0x4C43494E` ("LCIN"). The capture hooks use it to skip input the agent itself played.

## Controlling another device

Push the pointer against the edge where a connected peer is placed (see [Arranging devices](#arranging-devices)). The edge resists with the push-through effect, then the pointer crosses: from there this PC's keyboard and mouse drive the peer.

| | Where | What |
| --- | --- | --- |
| Crossing | agent ([edge.rs](agent/src/edge.rs)) | The edge thread measures the push with raw input at the portals of connected peers, like the demo. After crossing, it sends the peer an absolute move to the matching point of its edge, 2 points inside. |
| While driving | agent ([capture.rs](agent/src/capture.rs)) | Low-level mouse and keyboard hooks, installed only while driving, swallow local input and send it through the core. The Windows cursor waits in the middle of its monitor, hidden by a tiny window with no cursor. Motion is measured from there, so Windows' own pointer speed and acceleration apply. Keys go by scan code (→ HID usage), auto-repeats are marked as repeats, and the wheel sends 10 points per notch. |
| Where the pointer is | core (`layout::RemotePointer`) | Tracks the pointer in the peer's space as the peer plays the same motion. It slides along edges that lead nowhere. Moving out through a portal ends the crossing at the matching local point, 2 points inside the edge. |
| Coming back | agent | The cursor reappears there with the effect's emerge animation. For the **连续切换免阻力** time, going back across needs no push. |
| Release | agent | **Ctrl+Alt+Esc** gives control back at once, just inside the edge the pointer left through. Losing the session does the same. The peer is told to release every key and button it still holds. |
| Keys held while crossing | agent | Keys and buttons already held when the pointer leaves (a drag across the edge, a held Shift) are released on this PC, not sent to the peer. |

The demo and real crossings share the edge thread: a process can register raw mouse input for only one window. While the demo is on, portals wrap around instead of crossing.

Known limits:
- The secure desktop (Ctrl+Alt+Del, UAC prompts) cannot be hooked; it takes input back from the hooks.
- With mixed DPI across several monitors, logical monitor positions can leave gaps. A peer placed in such a gap is not reachable.

Known limits: windows of elevated programs (such as Task Manager), UAC prompts and the lock screen ignore injected input until the LocalSystem service exists (M4). Scroll speeds and Mac function-key behavior still need checking against a real Mac.

## Clipboard

Turn on **同步剪贴板** on the **设备** page. The clipboard then follows control, as in Deskflow. When this PC starts driving a peer, or a peer driving it gives control back, the peer gets this PC's clipboard. The other way round works the same. The core sends contents only if they changed since the last exchange with that peer, which also keeps them from echoing back. The whole design, including file transfer plans, is in [linkcursor-core/docs/clipboard-and-files.md](../linkcursor-core/docs/clipboard-and-files.md).

[clipboard.rs](agent/src/clipboard.rs) is the Windows end:

| What | How |
| --- | --- |
| Shared formats | Text (`CF_UNICODETEXT` ↔ UTF-8), HTML (the fragment of `"HTML Format"`, rebuilt into CF_HTML on write), images (`"PNG"`, or `CF_DIB` converted to PNG; written back as both). At most 8 MiB in all. |
| Changes | `AddClipboardFormatListener` on a message-only window on its own thread. A peer's clipboard written here is not reported back: the sequence number after the write is skipped. |
| Privacy | Contents marked with `ExcludeClipboardContentFromMonitorProcessing` or `Clipboard Viewer Ignore`, or `CanIncludeInClipboardHistory` = 0 (password managers), are never shared. |
| Copied files | Files copied in Explorer (`CF_HDROP`) are reported as a `text/uri-list`, with their names as text. The core sends a manifest, and the peer pulls the files while this PC runs. Files a peer copied land in `Downloads\LinkCursor` (folders kept, names never overwritten). They are then put on the clipboard as `CF_HDROP` with a copy drop effect, so Ctrl+V in Explorer copies them. |
| Receiving files | The core's own limit is set to 0, so every transfer waits for the agent. Up to 512 MB it is taken at once. A larger one asks first in a message box. While sharing is off it is refused. A failed transfer shows as the error bar. |
| Setting | Off by default, because traffic is not encrypted yet. Saved to `%LOCALAPPDATA%\LinkCursor\settings.json`. The core always offers the clipboard and files to peers so the setting can change at run time. While it is off, the agent reports an empty clipboard, ignores peers' clipboards, clears any it receives so the core does not pass them on, and refuses offered files. |

## Edge push-through effect

Inspired by Universal Control. The effect is drawn on the device the cursor is moving into:

| Stage | What you see |
| --- | --- |
| Approach | While you push against the edge, a soft bump grows out of it under the pointer. It grows quickly at first and then slower, which is the resistance. Near the crossing point a pointer dot appears at its tip. |
| Release | Stop pushing, or pull away, and the bump springs back. |
| Pass | Once you have pushed far enough, the bump flicks outward and snaps shut as the cursor goes through. |
| Emerge | Where the cursor comes in, the edge flicks back with a slight wobble, the dot shrinks onto the pointer and a ring fades out. |

All motion uses a damped spring with SwiftUI-style `response` and damping ratio ([spring.rs](agent/src/spring.rs)). macOS can use the same values with `Spring(response:dampingRatio:)`, so both platforms move identically.

**Tuning it on one machine.** Open the settings window, select **连接效果**, and turn on **效果演示**. The portals of the arrangement then act as if the placed devices sat beyond them and the screen wrapped around. With nothing placed, the whole right edge of the desktop stands in for a device:

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

**How the push is measured.** [demo.rs](agent/src/demo.rs) registers for raw mouse input (`RIDEV_INPUTSINK`). Raw input keeps reporting motion while the cursor is pinned against the edge. Only outer edges of the desktop count, meaning edges with no monitor beyond them, and only within a portal. The real implementation will move this into the input-capture code and send the push progress to the peer, which then draws the approach on its own edge.

## Agent IPC protocol

The pipe is `\\.\pipe\LinkCursor.Agent.<user SID>`. Its ACL lets only the signed-in user and LocalSystem open it, and it rejects remote clients. Each message is one UTF-8 JSON object followed by `\n`.

App → agent:

| Message | Effect |
| --- | --- |
| `{"type":"hello","protocol":5}` | Sent first. The agent replies with an `error` if the protocol version differs. |
| `{"type":"connect","peer_id":"…"}` | Connect to a discovered peer. |
| `{"type":"disconnect","peer_id":"…"}` | Close the session with that peer. Sessions with other peers stay open. |
| `{"type":"set_placement","peer_id":"…","x":1440,"y":-11}` | Place a peer near this offset (logical points, this PC's space). The core snaps it to a valid spot and syncs it. |
| `{"type":"remove_placement","peer_id":"…"}` | Remove the peer's placement. |
| `{"type":"place_beside","peer_id":"…","side":"right"}` | Place a peer centred on one side: `left`, `right`, `top` or `bottom`. |
| `{"type":"set_clipboard_sync","enabled":true}` | Share the clipboard with peers, or stop; saved in `settings.json`. |
| `{"type":"set_effect","params":{…}}` | Replace the effect parameters (keys as in `effect.json`; missing keys take their defaults) and save them. |
| `{"type":"reset_effect"}` | Restore the default parameters. |
| `{"type":"set_effect_demo","enabled":true}` | Start or stop the single-machine demo. The demo belongs to the connection that started it and stops when that connection closes. |
| `{"type":"effect_preview"}` | Play the effect once on the primary monitor. With `"hold":0.6` the bump freezes at that progress until the next effect command, which helps when inspecting it. |

Agent → app:

| Message | Meaning |
| --- | --- |
| `{"type":"hello","protocol":5,"agent_version":"0.1.0"}` | Sent once when the app connects. |
| `{"type":"state", …}` | The full snapshot, sent right after `hello` and again after every change. |
| `{"type":"error","message":"…"}` | A request failed. |

A `state` message looks like this:

- `sessions` lists every peer this PC is connecting or connected to, one entry per peer and possibly several. `status` is `connecting` or `connected`, and `remote_control` is true while that peer drives this PC.
- `layout` is what the 排列 page draws, in logical points:
  - `displays` are this PC's monitors.
  - `devices` lists every known peer, online or not. Each has its `displays` in its own space (a default 1440 × 900 until it describes them), its `offset` in this PC's space (`null` while not placed), and the `side` of this PC it touches.
- `last_error` is omitted when there is none. A session the user or the peer ended normally does not set it.
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
2. **Arrangement:** done; see [Arranging devices](#arranging-devices). The macOS app uses it with a side menu; it has no drag canvas yet.
3. **Send input (M2):** done; see [Controlling another device](#controlling-another-device). It still needs a pass with the real Mac for pointer speed and keys.
4. **Edge effect with a real peer (M3):** the renderer, tuning, screen info and portals exist. What remains in the core is `EdgeApproach` progress, `Enter` with the entry position on the portal, and `Return` when the cursor is pushed back. That lets the controlling device's push draw the approach on the target's edge. The same effect also has to be built on macOS from the shared parameters.
5. **Productize (M4):** single-instance settings window, MSIX packaging, a LocalSystem service for UAC prompts and the lock screen, firewall rules, code signing, and dropping the unused WebView2 DLL from the output.

## Security

Same as the core: traffic between devices is neither encrypted nor authenticated yet. Only use LinkCursor on networks you trust.

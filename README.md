# LinkCursor for Windows

The Windows client for LinkCursor. It is two programs:

| Program | Language | Role |
| --- | --- | --- |
| `linkcursor-agent.exe` ([agent/](agent/)) | Rust | Always running. Links `linkcursor-core` directly, owns the tray icon, draws the edge push-through effect, and serves a named pipe for the settings app. Keyboard and mouse capture and injection will live here too, because they must never wait on UI or a garbage collector. |
| `LinkCursor.exe` ([app/](app/)) | C++/WinRT, WinUI 3 | The settings window: nearby devices and effect tuning. It starts only when opened (from the tray or directly) and exits when closed, so it costs no memory the rest of the time. |

```
 LinkCursor.exe (WinUI 3)  ── named pipe, JSON lines ──►  linkcursor-agent.exe (Rust)
   devices, effect tuning                                   ├ tray icon
                                                            ├ edge effect overlay (Direct2D + DirectComposition)
                                                            ├ linkcursor-core ── UDP 42420 / TCP 42421 ──► other devices
                                                            └ (next) hooks, SendInput
```

Status: the agent starts the core, discovers peers and connects to them, and the edge effect can be tuned with a single-machine demo. Remote input is received but not yet injected, and local input is not yet captured, so the effect is not yet driven by a real peer.

## Layout

```
build.cmd, build.ps1         one-command build from any terminal
agent/
  Cargo.toml                 depends on ../../linkcursor-core
  build.rs                   embeds linkcursor-agent.manifest (PerMonitorV2 DPI, asInvoker)
  src/main.rs                single instance, core start-up, shutdown
  src/state.rs               folds core events into the snapshot the app renders
  src/ipc.rs                 named-pipe server
  src/services.rs            what IPC requests act on (core, effect, demo)
  src/effect.rs              effect parameters (%LOCALAPPDATA%\LinkCursor\effect.json)
  src/spring.rs              damped spring with SwiftUI-style response/damping
  src/overlay.rs             click-through overlay that draws the edge effect
  src/demo.rs                single-machine demo: raw-input push tracking, wrap-around
  src/tray.rs                notification-area icon and menu
  src/identity.rs            device ID (%LOCALAPPDATA%\LinkCursor\device-id) and name
  src/win.rs                 Win32 helpers: mutex, user SID, pipe security
app/
  LinkCursor.vcxproj         unpackaged WinUI 3; also builds the agent with Cargo
  Directory.Packages.props   NuGet versions (central package management)
  App.xaml / MainWindow.xaml settings window: devices and effect tuning (Mica, Fluent controls)
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

## Edge push-through effect

Inspired by Universal Control. The effect is drawn on the device the cursor is moving into:

| Stage | What you see |
| --- | --- |
| Approach | While you push against the edge, a soft bump grows out of it under the pointer. It grows quickly at first and then slower, which is the resistance. Near the crossing point a pointer dot appears at its tip. |
| Release | Stop pushing, or pull away, and the bump springs back. |
| Pass | Once you have pushed far enough, the bump flicks outward and snaps shut as the cursor goes through. |
| Emerge | Where the cursor comes in, the edge flicks back with a slight wobble, the dot shrinks onto the pointer and a ring fades out. |

All motion uses a damped spring with SwiftUI-style `response` and damping ratio ([spring.rs](agent/src/spring.rs)). macOS can use the same values with `Spring(response:dampingRatio:)`, so both platforms move identically.

**Tuning it on one machine.** Open the settings window, select **连接效果**, and turn on **效果演示**. The right edge of the desktop then acts as if another device sat beyond it and the screen wrapped around:

- Push the mouse against the right edge. The bump grows; when you have pushed far enough, the cursor comes out of the left edge, as it would on the other device.
- Push against the left edge to come back. Returning never has resistance.
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

**How the push is measured.** [demo.rs](agent/src/demo.rs) registers for raw mouse input (`RIDEV_INPUTSINK`). Raw input keeps reporting motion while the cursor is pinned against the edge. Only outer edges of the desktop count, meaning edges with no monitor beyond them. The real implementation will move this into the input-capture code and send the push progress to the peer, which then draws the approach on its own edge.

## Agent IPC protocol

The pipe is `\\.\pipe\LinkCursor.Agent.<user SID>`. Its ACL lets only the signed-in user and LocalSystem open it, and it rejects remote clients. Each message is one UTF-8 JSON object followed by `\n`.

App → agent:

| Message | Effect |
| --- | --- |
| `{"type":"hello","protocol":2}` | Sent first. The agent replies with an `error` if the protocol version differs. |
| `{"type":"connect","peer_id":"…"}` | Connect to a discovered peer. |
| `{"type":"disconnect"}` | Drop the current connection. |
| `{"type":"set_effect","params":{…}}` | Replace the effect parameters (keys as in `effect.json`; missing keys take their defaults) and save them. |
| `{"type":"reset_effect"}` | Restore the default parameters. |
| `{"type":"set_effect_demo","enabled":true}` | Start or stop the single-machine demo. The demo belongs to the connection that started it and stops when that connection closes. |
| `{"type":"effect_preview"}` | Play the effect once on the primary monitor. With `"hold":0.6` the bump freezes at that progress until the next effect command, which helps when inspecting it. |

Agent → app:

| Message | Meaning |
| --- | --- |
| `{"type":"hello","protocol":2,"agent_version":"0.1.0"}` | Sent once when the app connects. |
| `{"type":"state", …}` | The full snapshot, sent right after `hello` and again after every change. |
| `{"type":"error","message":"…"}` | A request failed. |

A `state` message looks like this:

- `connection.status` is `disconnected`, `connecting` (with `peer_id`) or `connected` (with `peer`).
- `last_error` is omitted when there is none.
- `effect` holds the current parameters.
- `effect_revision` changes only when the agent reloads them (at start-up or on `reset_effect`), not on the app's own `set_effect`. The app copies `effect` into its controls only when the revision changes, so a snapshot cannot move a slider the user is dragging.
- `demo_side` is `local` or `remote` while the demo runs.

```json
{
  "type": "state",
  "device_id": "3f0c…",
  "device_name": "DESKTOP-1234",
  "core": "running",
  "peers": [{ "id": "a1b2…", "name": "MacBook Pro", "platform": "macos", "address": "192.168.1.20", "port": 42421 }],
  "connection": { "status": "connected", "peer": { "id": "a1b2…", "name": "MacBook Pro", "platform": "macos", "address": "192.168.1.20", "port": 42421 } },
  "effect": { "push_distance": 260.0, "spring_response": 0.32, "spring_damping": 0.78, "bump_depth": 26.0, "…": "…" },
  "effect_revision": 1,
  "effect_demo": false,
  "demo_side": "local"
}
```

The agent sends whole snapshots instead of individual events, so the app never has to rebuild state from an event history. Bump `PROTOCOL_VERSION` in [ipc.rs](agent/src/ipc.rs) and `ProtocolVersion` in [AgentClient.cpp](app/AgentClient.cpp) together when a message changes incompatibly.

## Next steps

1. **Receive input (M1):** inject events from the core's input callback with `SendInput`: absolute moves with `MOUSEEVENTF_VIRTUALDESK` so Windows does not accelerate them a second time, scan-code keys mapped from USB HID usages, and high-resolution wheel. Map ⌘ to Ctrl when the peer is a Mac.
2. **Send input (M2):** `WH_MOUSE_LL` / `WH_KEYBOARD_LL` hooks on a dedicated thread, edge detection, parking and hiding the local cursor while controlling a peer, and a Ctrl+Alt+Esc release key.
3. **Edge effect with a real peer (M3):** the renderer and tuning exist. What remains is protocol v3 in the core: screen info, `EdgeApproach` progress, `Enter` with the entry position, and `Return` when the cursor is pushed back. That lets the controlling device's push draw the approach on the target's edge. The same effect also has to be built on macOS from the shared parameters.
4. **Productize (M4):** single-instance settings window, MSIX packaging, a LocalSystem service for UAC prompts and the lock screen, firewall rules, code signing, and dropping the unused WebView2 DLL from the output.

## Security

Same as the core: traffic between devices is neither encrypted nor authenticated yet. Only use LinkCursor on networks you trust.

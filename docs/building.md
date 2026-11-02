# Building and running

`rinflow-core` must be cloned next to this repository:

```sh
git clone https://github.com/rinflow-dev/rinflow-core.git
git clone https://github.com/rinflow-dev/rinflow-windows.git
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
[app/Assets](../app/Assets/README.md). They are shared by the main window, tray,
executables and installer; normal builds require no image-generation tools.

From any PowerShell or Command Prompt window:

```powershell
cd rinflow-windows
.\build.cmd                              # Debug, x64
.\build.cmd -Configuration Release
.\build.cmd -Platform ARM64              # needs: rustup target add aarch64-pc-windows-msvc
.\build.cmd -SkipAgent                   # the WinUI app only
```

[build.cmd](../build.cmd) runs [build.ps1](../build.ps1) without changing the PowerShell execution policy. The script:

- finds Visual Studio or Build Tools and loads its developer environment;
- switches the console to UTF-8, so output from MSBuild, the C++ compiler and Cargo is never decoded with the wrong code page (for example 932 on a machine whose system locale is Japanese);
- runs MSBuild on `app\Rinflow.vcxproj`. That restores NuGet packages, builds `Rinflow.exe`, runs `cargo build` for the agent and for [input](../input/) (the input helper and its service), and copies `rinflow-agent.exe`, `rinflow-input.exe` and `rinflow-service.exe` next to the app. Everything ends up in `app\<Platform>\<Configuration>\Rinflow\`. Release builds the Rust programs with `--release`.

Build messages are in Chinese by default. Pass `-Language en-US` (or another culture name) to change this. MSBuild and NuGet messages follow it right away. The C++ compiler only speaks the Visual Studio language packs that are installed, so add one for compiler messages too:

```powershell
& "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\setup.exe" modify `
    --installPath "${env:ProgramFiles(x86)}\Microsoft Visual Studio\18\BuildTools" --addProductLang zh-CN --passive
```

Inside a Developer PowerShell you can also call MSBuild directly: `msbuild app\Rinflow.vcxproj -restore -p:Configuration=Debug -p:Platform=x64`.

For a debugging loop, use [dev.ps1](../dev.ps1): `.\dev.ps1 build`, `.\dev.ps1 run` and `.\dev.ps1 stop`. Building never stops a running copy: [build.ps1](../build.ps1) renames the files it holds open to `*.old` (Windows refuses to overwrite them but allows the rename), so `.\build.cmd` also works while Rinflow runs. `make run` builds first, then stops the old copy, deletes the `*.old` files and starts the new build, keeping the agent in your terminal with its log. The settings window it opens does not start an agent of its own, so the terminal stays in charge of the agent. Ctrl+C, or closing a debug agent's console, quits the agent the same way as the tray's 退出: the tray icon goes away and peers see a clean disconnect.

A Release x64 build is about 64 MB of files, most of it the Windows App Runtime it carries; the setup program is about 16 MB. The agent is 5 MB, `Rinflow.exe` 1 MB, the input helper 1 MB and the service 0.4 MB. The WebView2 DLL that the WinUI package brings in is not used and is left out of the setup program. At run time the agent uses about 13 MB of memory and no CPU while idle; the settings window uses about 120 MB, but only while it is open.

### Setup program

```powershell
.\installer\build-installer.ps1             # Release build, then the setup program
.\installer\build-installer.ps1 -SkipBuild  # package the existing Release build
```

[build-installer.ps1](../installer/build-installer.ps1) runs the Release build and compiles [Rinflow.iss](../installer/Rinflow.iss) with Inno Setup into `installer\output\Rinflow-Setup-<version>-x64.exe` (the version comes from [agent/Cargo.toml](../agent/Cargo.toml)). The setup program speaks English or Simplified Chinese ([ChineseSimplified.isl](../installer/ChineseSimplified.isl), MIT, from [kira-96/Inno-Setup-Chinese-Simplified-Translation](https://github.com/kira-96/Inno-Setup-Chinese-Simplified-Translation)), picked from the Windows display language; for any other language it asks which one. Its own texts are `[CustomMessages]` in the script. It needs an administrator and:

- installs into `Program Files\Rinflow`, where only administrators can change files (the input helper and the agent rely on that when they check each other);
- adds a firewall rule for the agent on private **and public** networks (Windows often marks a new Wi-Fi network public, and nothing would be found without saying why);
- registers and starts the `RinflowInput` service (`rinflow-service.exe --install`) and writes the 被控时可操作管理员窗口、UAC 和锁屏 setting (a task, checked by default; see [Elevated windows, UAC prompts and the lock screen](design.md#elevated-windows-uac-prompts-and-the-lock-screen));
- turns on starting at sign-in (a task) for the user who ran it (`rinflow-agent.exe --launch-at-login on`, run as that user, not as the administrator);
- before replacing files (an upgrade), removes the service and closes Rinflow; uninstalling removes the service, the firewall rule and the logs, and keeps each user's settings and pairings.

It is not code-signed yet, so SmartScreen warns before it runs; `SignTool` in the script is ready for a certificate.

To work on the agent alone:

```powershell
cd rinflow-windows\agent
cargo build
cargo run          # debug builds keep a console window for logs
```

## Running

Start either program:

- `Rinflow.exe` connects to the agent and starts `rinflow-agent.exe` from the same folder if it is not running yet. It runs once per user: starting it again brings the running window back.
- `rinflow-agent.exe` puts an icon in the notification area. Left-click it for the tray panel: the 键鼠共享 switch, the devices, **打开主窗口**, settings and quit. The right-click menu has **打开主窗口** and **退出**. Quitting stops the agent and the core.
- Closing the main window leaves the agent running in the tray, unless **关闭窗口时保留在托盘** is off; then it quits Rinflow.

Both programs look next to themselves for the other one. Override this with `RINFLOW_AGENT_PATH` (used by the app) or `RINFLOW_APP_PATH` (used by the agent).

Installed with the setup program, the firewall is already set up. A build run from the source tree makes Windows Defender Firewall ask the first time the agent listens; allow it on private networks (and public ones if the network is marked public). Rinflow uses UDP 42420 (discovery), UDP 42421 (connections) and one more UDP port the system picks for file and clipboard transfers. It uses no TCP. The prompt allows the program, which covers all of them. Other Rinflow devices on the same network appear in the list within a few seconds.

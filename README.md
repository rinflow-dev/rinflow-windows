# Rinflow for Windows

English | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

Rinflow shares one keyboard and mouse between computers on a local network. Move the pointer past a screen edge to control the next device.

This is the Windows client. It works with the [macOS client](https://github.com/rinflow-dev/rinflow-macos) and is built on [rinflow-core](https://github.com/rinflow-dev/rinflow-core).

**Status:** early development. There are no binary releases yet. Known issues are listed in the [changelog](CHANGELOG.md).

## Features

- Control other devices, or be controlled by them, by crossing a screen edge
- Drag devices into place around your monitors
- Encrypted connections, with six-digit pairing
- Optional clipboard and file sync, off by default
- Optional input into administrator windows, UAC prompts and the lock screen

## Building

Requires Windows 10 1809 or later, Rust with the MSVC toolchain, and Visual Studio or Build Tools 2026 with the C++ and WinUI workloads. See [Building and running](docs/building.md) for details.

```powershell
git clone https://github.com/rinflow-dev/rinflow-core.git
git clone https://github.com/rinflow-dev/rinflow-windows.git
cd rinflow-windows
.\build.cmd -Configuration Release
.\installer\build-installer.ps1    # optional: the setup program, needs Inno Setup 6
```

## Usage

1. Start Rinflow on both devices, on the same network.
2. Click **Add device**, and check that both devices show the same six-digit code.
3. Drag the device to the side of your screen where it sits.
4. Push the pointer past that edge. **Ctrl+Alt+Esc** brings control back.

## Documentation

- [Building and running](docs/building.md)
- [Design](docs/design.md): how each feature works, settings and the agent's IPC protocol
- [Clipboard implementation](agent/docs/clipboard.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

[Mozilla Public License 2.0](LICENSE). Third-party software is listed in [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt).

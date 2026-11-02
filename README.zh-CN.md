# Rinflow for Windows

[English](README.md) | 简体中文 | [日本語](README.ja.md)

Rinflow 让局域网内的多台电脑共用一套键盘和鼠标：把指针移过屏幕边缘，就能控制下一台设备。

这是 Windows 客户端，可与 [macOS 客户端](https://github.com/rinflow-dev/rinflow-macos)配合使用，基于 [rinflow-core](https://github.com/rinflow-dev/rinflow-core) 构建。

**状态：** 早期开发阶段，暂无二进制发布版本。已知问题见[变更日志](CHANGELOG.md)。

## 功能

- 越过屏幕边缘即可控制其他设备，也可被其他设备控制
- 拖动设备，排列到显示器周围
- 加密连接，六位配对码
- 可选的剪贴板和文件同步，默认关闭
- 可选的管理员窗口、UAC 弹窗和锁屏输入

## 构建

需要 Windows 10 1809 或更高版本、MSVC 工具链的 Rust，以及带 C++ 和 WinUI 工作负载的 Visual Studio 或 Build Tools 2026。详情见[构建和运行](docs/building.md)。

```powershell
git clone https://github.com/rinflow-dev/rinflow-core.git
git clone https://github.com/rinflow-dev/rinflow-windows.git
cd rinflow-windows
.\build.cmd -Configuration Release
.\installer\build-installer.ps1    # 可选：生成安装程序，需要 Inno Setup 6
```

## 使用

1. 在同一网络的两台设备上启动 Rinflow。
2. 点击**添加设备**，确认两边显示相同的六位配对码。
3. 把设备拖到它实际所在的屏幕一侧。
4. 把指针推过该边缘。按 **Ctrl+Alt+Esc** 收回控制。

## 文档（英文）

- [构建和运行](docs/building.md)
- [设计](docs/design.md)：各功能的工作方式、设置项和 agent 的 IPC 协议
- [剪贴板实现](agent/docs/clipboard.md)

## 参与贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。报告安全漏洞见 [SECURITY.md](SECURITY.md)。

## 许可证

[Mozilla Public License 2.0](LICENSE)。第三方软件见 [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt)。

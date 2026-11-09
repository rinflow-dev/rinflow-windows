# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Automatic updates: Rinflow finds a new version, downloads it in the background and checks its signature, and installs it when you choose **Install and restart** (Windows asks for an administrator once). **Check for updates automatically** turns the daily check off. Versions before this one have to install it by hand once.

### Changed

- Settings, the arrangement, paired devices and this PC's device ID are kept in one file, `config.json`, with a backup of the previous version. The files of 0.0.1 are moved into it on the first start.
- Only your own devices (connected once, or placed) are remembered across restarts, not every device seen on the network.
- Setup lets Rinflow quit by itself before an upgrade, so connected devices see a clean disconnect.

### Fixed

- The main window could close unexpectedly when a device added by address went offline.
- When the background service kept ending right after it started, the app restarted it endlessly. It now waits longer each time, and after the third time says so and offers **Retry**.
- Copying very large text or images could end Rinflow.
- Saving the arrangement could briefly stall the pointer while controlling another device.
- One invalid value in the settings no longer resets all the others, and a settings file that cannot be read at the moment is no longer overwritten.

## [0.0.1] - 2026-11-05

First public version.

### Added

- Control a nearby Mac or PC by pushing the pointer through a screen edge, and be controlled by one
- Drag canvas for arranging devices around this PC's monitors
- Pairing with a six-digit code, and encrypted connections
- Optional clipboard and file sync (off by default)
- Input into elevated windows, UAC prompts and the lock screen through an optional service
- English, Simplified Chinese and Japanese interface, and light and dark themes
- Setup programs for x64 and ARM64

### Known issues

- The setup program is not code-signed, so SmartScreen warns before it runs.
- There are no automatic updates. **Check for updates** opens the releases page.
- Files copied from another device paste only into programs that understand Windows virtual files, such as File Explorer.
- A copied image file is sent as a file, so it cannot be pasted as a picture into programs such as chat apps. A screenshot on the clipboard works.
- Images over the 8 MiB clipboard limit are not sent from this PC.
- If the clipboard stays busy in another program, a sync can fail without telling the user.

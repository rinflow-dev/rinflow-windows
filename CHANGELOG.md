# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.0.2] - 2026-11-16

### Added

- Automatic updates: Rinflow finds a new version, downloads it in the background and checks its signature, and installs it when you choose **Install and restart** (Windows asks for an administrator once). **Check for updates automatically** turns the daily check off. Versions before this one have to install it by hand once.
- An install script, `scripts/install.ps1`: it finds out whether Windows runs on x64 or ARM64, downloads the matching setup program from the latest release, checks its SHA-256 and starts it.

### Changed

- A steadier pointer while this PC is busy: the threads that capture and play input run ahead of ordinary work, Windows no longer treats Rinflow as a background program to slow down, and motion that piled up is played as one move instead of one by one.
- The log records every second in which the link lost packets or delivered input late (`link_quality`), to tell why a pointer stuttered.
- Settings, the arrangement, paired devices and this PC's device ID are kept in one file, `config.json`, with a backup of the previous version. The files of 0.0.1 are moved into it on the first start.
- Only your own devices (connected once, or placed) are remembered across restarts, not every device seen on the network.
- Setup lets Rinflow quit by itself before an upgrade, so connected devices see a clean disconnect.
- Shorter descriptions in the settings and in the add-device dialog. The encryption row shows only while the core is not running, since connections are always encrypted.
- The icon of the program, its shortcuts and the setup program has no plate behind it any more: the mark alone, with a white outline that keeps it visible on a dark taskbar.

### Fixed

- The main window could close unexpectedly when a device added by address went offline.
- Dragging a screen in the arrangement could move another device instead, when that device has monitors of different sizes and its empty corner lay over the screen.
- After a drop, the arrangement no longer jumps: what was dragged stays where it was let go, also when it is this PC.
- While this PC drives another device, a change to the arrangement (made on either device) now applies at once: the pointer stays where it is and comes back through the new edge, instead of the old one until the next crossing.
- When the background service kept ending right after it started, the app restarted it endlessly. It now waits longer each time, and after the third time says so and offers **Retry**.
- Copying very large text or images could end Rinflow.
- Saving the arrangement could briefly stall the pointer while controlling another device.
- One invalid value in the settings no longer resets all the others, and a settings file that cannot be read at the moment is no longer overwritten.
- While controlling another device, the keyboard and mouse often came back to this PC with the notice that Windows had stopped Rinflow from taking them. Rinflow now takes them again by itself and carries on.
- Setup could stop at a page asking to close **Connected User Experiences and Telemetry**, a part of Windows that happened to be reading Rinflow's files. Setup now closes only Rinflow.

### Removed

- The animation at the screen edge (the bump while pushing, and the ring where the pointer comes back). Crossing itself is unchanged.
- The **Port** setting. Devices find each other's port by themselves, and the Mac app never had the setting. A port already set stays in `config.json`.

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

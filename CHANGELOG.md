# Changelog

Notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [0.1.0] - Unreleased

First public version.

### Added

- Control a nearby Mac or PC by pushing the pointer through a screen edge, and be controlled by one
- Drag canvas for arranging devices around this PC's monitors
- Pairing with a six-digit code, and encrypted connections
- Optional clipboard and file sync (off by default)
- Input into elevated windows, UAC prompts and the lock screen through an optional service
- English and Simplified Chinese interface, and light and dark themes
- Setup program for x64

### Known issues

- The setup program is not code-signed, so SmartScreen warns before it runs.
- There are no automatic updates. **Check for updates** opens the releases page.
- Files copied from another device paste only into programs that understand Windows virtual files, such as File Explorer.
- A copied image file is sent as a file, so it cannot be pasted as a picture into programs such as chat apps. A screenshot on the clipboard works.
- Images over the 8 MiB clipboard limit are not sent from this PC.
- If the clipboard stays busy in another program, a sync can fail without telling the user.

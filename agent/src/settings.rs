//! Agent preferences other than the effect and the arrangement, saved to
//! `%LOCALAPPDATA%\LinkCursor\settings.json`. Starting at login is not kept
//! here: the `Run` registry value is the truth (see [`autostart`]).

use crate::i18n::{self, tr};
use crate::identity;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// `RegisterHotKey` modifier bits.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// The discovery port of linkcursor-core; the control port must differ.
const DISCOVERY_PORT: u16 = 42420;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Share the clipboard with peers. Off by default: clipboards can hold
    /// passwords, and a device that is not paired could be on the other end.
    pub clipboard: bool,
    /// Share the keyboard and mouse at all: cross into peers, and let peers
    /// drive this PC. Off pauses both; sessions and the clipboard stay.
    pub sharing: bool,
    /// Closing the settings window leaves the agent (and its tray icon)
    /// running. Off quits the agent with the window.
    pub keep_in_tray: bool,
    /// The settings app's theme: `system`, `light` or `dark`.
    pub theme: String,
    /// How long the pointer has to push against a portal before it crosses.
    pub switch_delay_ms: u32,
    /// Screen corners never cross (the size is the effect's dead zone), so
    /// the Start button, menus and hot corners stay usable.
    pub corner_guard: bool,
    /// No crossing while a full-screen game or presentation runs.
    pub pause_in_fullscreen: bool,
    /// Toggles "locked to the current screen": no crossing either way.
    pub lock_hotkey: Hotkey,
    /// The core's control port (UDP); `None` uses the core's default.
    /// Applies after a restart.
    pub port: Option<u16>,
    /// Only paired devices may connect: others pair first (both users check
    /// a code).
    pub paired_only: bool,
    /// What a Mac's modifier keys play as while it drives this PC (see
    /// linkcursor-core `remap.rs`). By default Command and Control swap: ⌘C
    /// copies, and the Mac's Control is the Windows key. The settings page
    /// offers that, keeping keys where they are, or a choice per key.
    pub modifier_map: linkcursor_core::ModifierMap,
    /// When a peer that drove this PC moves its pointer away, this PC's
    /// cursor is hidden where it was left, until this PC's own mouse moves
    /// or a peer drives it again (see `hider.rs`).
    pub hide_cursor_on_leave: bool,
    /// The arrow keys with [`Settings::switch_modifiers`] move the pointer
    /// to the device on that side.
    pub switch_hotkeys: bool,
    /// The modifiers of the switch shortcuts: one of [`SWITCH_MODIFIER_CHOICES`].
    /// Ctrl+Alt+Shift by default, which few programs use with the arrow keys
    /// (Ctrl+Alt alone adds cursors in VS Code and rotates the screen with
    /// some graphics drivers); a combination taken by another program shows
    /// in the settings (`switch_hotkeys_blocked`).
    pub switch_modifiers: u32,
    /// While one of the other devices is in use, this PC's screen stays on
    /// and does not lock for being idle.
    pub keep_awake: bool,
    /// Locking this PC locks the other devices too, and theirs lock it.
    pub lock_sync: bool,
    /// Where to look for devices besides the local network's multicast:
    /// host names or IPv4 addresses (see the core's `manual_addresses`).
    pub manual_addresses: Vec<String>,
    /// How this PC's mouse feels on each peer it drives, by device ID.
    pub peer_input: BTreeMap<String, PeerInput>,
    /// The language of LinkCursor's own text: `system` (Windows' display
    /// language: Simplified Chinese, or English for every other one),
    /// `zh-CN` or `en-US`. The settings app reads it at start-up too.
    pub language: String,
}

/// The values of [`Settings::language`].
pub const LANGUAGES: [&str; 3] = ["system", "zh-CN", "en-US"];

/// Pointer and scrolling on one peer, applied by this PC as it drives it
/// (it tracks the pointer there, so the peer must not change the motion).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerInput {
    /// Multiplies pointer motion: 0.25 to 4.
    pub pointer_speed: f64,
    /// Multiplies scrolling: 0.25 to 5.
    pub scroll_speed: f64,
    /// Scrolls the other way (natural scrolling on one side only).
    pub invert_scroll: bool,
}

impl Default for PeerInput {
    fn default() -> Self {
        Self {
            pointer_speed: 1.0,
            scroll_speed: 1.0,
            invert_scroll: false,
        }
    }
}

impl PeerInput {
    fn is_valid(&self) -> bool {
        (0.25..=4.0).contains(&self.pointer_speed) && (0.25..=5.0).contains(&self.scroll_speed)
    }
}

/// The modifier combinations the switch shortcuts offer: Ctrl+Alt+Shift,
/// Ctrl+Alt.
pub const SWITCH_MODIFIER_CHOICES: [u32; 2] =
    [MOD_CONTROL | MOD_ALT | MOD_SHIFT, MOD_CONTROL | MOD_ALT];

/// Addresses the user may add, at most (the core's limit).
pub const MAX_MANUAL_ADDRESSES: usize = 32;

impl Default for Settings {
    fn default() -> Self {
        Self {
            clipboard: false,
            sharing: true,
            keep_in_tray: true,
            theme: "system".to_owned(),
            switch_delay_ms: 150,
            corner_guard: true,
            pause_in_fullscreen: false,
            lock_hotkey: Hotkey::default(),
            port: None,
            paired_only: true,
            modifier_map: linkcursor_core::ModifierMap::SWAP,
            hide_cursor_on_leave: true,
            switch_hotkeys: true,
            switch_modifiers: MOD_CONTROL | MOD_ALT | MOD_SHIFT,
            keep_awake: true,
            lock_sync: false,
            manual_addresses: Vec::new(),
            peer_input: BTreeMap::new(),
            language: "system".to_owned(),
        }
    }
}

/// A global shortcut: `MOD_*` bits and a virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

impl Default for Hotkey {
    /// Ctrl+Alt+L.
    fn default() -> Self {
        Self {
            modifiers: MOD_CONTROL | MOD_ALT,
            key: 0x4C,
        }
    }
}

impl Hotkey {
    /// A shortcut needs Ctrl, Alt or Win (Shift alone would eat typing) and
    /// a key that is not itself a modifier.
    fn is_valid(&self) -> bool {
        const MODIFIER_KEYS: [u32; 13] = [
            0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x14, 0x90,
        ];
        self.modifiers & !(MOD_ALT | MOD_CONTROL | MOD_SHIFT | MOD_WIN) == 0
            && self.modifiers & (MOD_ALT | MOD_CONTROL | MOD_WIN) != 0
            && (0x08..=0xFE).contains(&self.key)
            && !MODIFIER_KEYS.contains(&self.key)
    }
}

impl Settings {
    /// Applies the given keys to a copy; unknown keys and invalid values are
    /// refused with a message for the user.
    pub fn merged(&self, values: &Map<String, Value>) -> Result<Self, String> {
        let mut object = match serde_json::to_value(self) {
            Ok(Value::Object(object)) => object,
            _ => return Err(tr("设置无法读取", "Settings couldn't be read").to_owned()),
        };
        for (key, value) in values {
            if !object.contains_key(key) {
                return Err(if i18n::chinese() {
                    format!("未知的设置项：{key}")
                } else {
                    format!("Unknown setting: {key}")
                });
            }
            object.insert(key.clone(), value.clone());
        }
        let merged: Self = serde_json::from_value(Value::Object(object)).map_err(|error| {
            if i18n::chinese() {
                format!("设置的值无效：{error}")
            } else {
                format!("Invalid setting value: {error}")
            }
        })?;
        if !matches!(merged.theme.as_str(), "system" | "light" | "dark") {
            return Err(tr(
                "主题只能是跟随系统、浅色或深色",
                "Theme must be system, light, or dark",
            )
            .to_owned());
        }
        if !SWITCH_MODIFIER_CHOICES.contains(&merged.switch_modifiers) {
            return Err(tr(
                "切换设备的快捷键只能是 Ctrl+Alt+Shift 或 Ctrl+Alt 加方向键",
                "Switch shortcuts must be Ctrl+Alt+Shift or Ctrl+Alt with the arrow keys",
            )
            .to_owned());
        }
        if merged.lock_hotkey.modifiers == merged.switch_modifiers
            && (0x25..=0x28).contains(&merged.lock_hotkey.key)
        {
            return Err(tr(
                "锁定快捷键与切换设备的快捷键重复了",
                "The lock shortcut is the same as a switch shortcut",
            )
            .to_owned());
        }
        if !LANGUAGES.contains(&merged.language.as_str()) {
            return Err(tr(
                "语言只能是跟随系统、简体中文或英语",
                "Language must be system, Simplified Chinese, or English",
            )
            .to_owned());
        }
        if merged.switch_delay_ms > 1000 {
            return Err(tr(
                "切换延迟最长 1000 毫秒",
                "Switch delay can be at most 1000 ms",
            )
            .to_owned());
        }
        if !merged.lock_hotkey.is_valid() {
            return Err(tr(
                "快捷键需要包含 Ctrl、Alt 或 Win，再加一个普通按键",
                "Shortcuts must include Ctrl, Alt, or Win, plus a regular key",
            )
            .to_owned());
        }
        if let Some(port) = merged.port
            && (port < 1024 || port == DISCOVERY_PORT)
        {
            return Err(if i18n::chinese() {
                format!("端口需在 1024–65535 之间，且不能是发现端口 {DISCOVERY_PORT}")
            } else {
                format!(
                    "Ports must be between 1024 and 65535 and not the discovery port {DISCOVERY_PORT}"
                )
            });
        }
        if merged.manual_addresses.len() > MAX_MANUAL_ADDRESSES {
            return Err(if i18n::chinese() {
                format!("最多添加 {MAX_MANUAL_ADDRESSES} 个地址")
            } else {
                format!("You can add up to {MAX_MANUAL_ADDRESSES} addresses")
            });
        }
        if let Some(bad) = merged
            .manual_addresses
            .iter()
            .find(|address| linkcursor_core::parse_address(address, DISCOVERY_PORT).is_err())
        {
            return Err(if i18n::chinese() {
                format!("“{bad}”不是有效的地址：请输入 IP 地址或设备名称")
            } else {
                format!("“{bad}” isn't a valid address. Enter an IP address or a device name.")
            });
        }
        if merged.peer_input.values().any(|input| !input.is_valid()) {
            return Err(tr(
                "指针速度需在 0.25–4 倍之间，滚动速度在 0.25–5 倍之间",
                "Pointer speed must be between 0.25× and 4×, and scroll speed between 0.25× and 5×",
            )
            .to_owned());
        }
        Ok(merged)
    }

    /// How this PC's mouse feels on `peer_id`.
    pub fn input_for(&self, peer_id: &str) -> PeerInput {
        self.peer_input.get(peer_id).copied().unwrap_or_default()
    }
}

/// The settings and their file. Reading never waits for the disk: the input
/// hooks read settings, and Windows silently removes a hook that takes too
/// long. So the file is written outside the lock, and only ever with the
/// newest settings.
pub struct SettingsStore {
    /// The settings, and how many changes they have had.
    current: Mutex<(Settings, u64)>,
    /// Taken while saving: the change count of the settings last saved.
    saved: Mutex<u64>,
}

impl SettingsStore {
    pub fn load() -> Self {
        let settings: Settings = identity::read_json(&path()).unwrap_or_default();
        // A file edited by hand falls back to defaults for what is invalid.
        let settings = settings.merged(&Map::new()).unwrap_or_else(|_| Settings {
            clipboard: settings.clipboard,
            ..Settings::default()
        });
        Self {
            current: Mutex::new((settings, 0)),
            saved: Mutex::new(0),
        }
    }

    pub fn get(&self) -> Settings {
        self.current
            .lock()
            .map(|current| current.0.clone())
            .unwrap_or_default()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Settings {
        let (settings, changes) = {
            let Ok(mut current) = self.current.lock() else {
                return Settings::default();
            };
            change(&mut current.0);
            current.1 += 1;
            (current.0.clone(), current.1)
        };
        // Saves take turns. One that comes after a save of newer settings
        // has nothing to add, and must not put older ones back.
        if let Ok(mut saved) = self.saved.lock()
            && *saved < changes
        {
            if let Ok(text) = serde_json::to_string_pretty(&settings)
                && let Err(error) = identity::write_file(&path(), &text)
            {
                log!("cannot save settings: {error}");
            }
            *saved = changes;
        }
        settings
    }
}

fn path() -> std::path::PathBuf {
    identity::data_directory().join("settings.json")
}

/// Starting the agent when the user signs in: the `LinkCursor` value under
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub mod autostart {
    use crate::win::wide;
    use std::io;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "LinkCursor";

    fn open(access: u32) -> io::Result<HKEY> {
        let key = wide(RUN_KEY);
        let mut handle: HKEY = null_mut();
        // SAFETY: opens an existing per-user key; `handle` receives it.
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, access, &mut handle) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(handle)
    }

    /// Whether the agent is set to start at sign-in.
    pub fn enabled() -> bool {
        let Ok(handle) = open(KEY_QUERY_VALUE) else {
            return false;
        };
        let value = wide(VALUE);
        // SAFETY: queries only whether the value exists; closes the key.
        unsafe {
            let status = RegQueryValueExW(
                handle,
                value.as_ptr(),
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            RegCloseKey(handle);
            status == ERROR_SUCCESS
        }
    }

    /// Starts this agent executable at sign-in, or stops doing so.
    pub fn set(enabled: bool) -> io::Result<()> {
        let handle = open(KEY_SET_VALUE)?;
        let value = wide(VALUE);
        let status = if enabled {
            let exe = std::env::current_exe()?;
            let command = wide(&format!("\"{}\"", exe.display()));
            // SAFETY: writes a REG_SZ of `command`'s bytes, its terminator
            // included.
            unsafe {
                RegSetValueExW(
                    handle,
                    value.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32,
                )
            }
        } else {
            // SAFETY: deletes the value; a missing one is fine.
            match unsafe { RegDeleteValueW(handle, value.as_ptr()) } {
                2 => ERROR_SUCCESS, // ERROR_FILE_NOT_FOUND
                status => status,
            }
        };
        // SAFETY: closes the key opened above.
        unsafe {
            RegCloseKey(handle);
        }
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn old_files_keep_their_values_and_gain_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"clipboard":true}"#).unwrap();
        assert!(settings.clipboard);
        assert!(settings.sharing && settings.keep_in_tray && settings.corner_guard);
        assert_eq!(settings.switch_delay_ms, 150);
        assert_eq!(settings.lock_hotkey, Hotkey::default());
    }

    #[test]
    fn a_macs_modifier_keys_are_chosen_per_key() {
        use linkcursor_core::{Modifier, ModifierMap};
        let settings = Settings::default();
        let kept = settings
            .merged(&values(json!({"modifier_map": {
                "super": "super", "control": "control", "alt": "alt"
            }})))
            .unwrap();
        assert_eq!(kept.modifier_map, ModifierMap::KEEP);
        let custom = settings
            .merged(&values(json!({"modifier_map": {
                "super": "control", "control": "none", "alt": "shift"
            }})))
            .unwrap();
        assert_eq!(
            custom.modifier_map,
            ModifierMap {
                super_key: Modifier::Control,
                control: Modifier::None,
                alt: Modifier::Shift,
            }
        );
        assert!(
            settings
                .merged(&values(json!({"modifier_map": {"super": "hyper"}})))
                .is_err()
        );
        assert!(
            settings
                .merged(&values(json!({"modifier_map": true})))
                .is_err()
        );
    }

    #[test]
    fn merging_checks_every_value() {
        let settings = Settings::default();
        let merged = settings
            .merged(&values(
                json!({"sharing": false, "port": 24800, "theme": "dark"}),
            ))
            .unwrap();
        assert!(!merged.sharing);
        assert_eq!(merged.port, Some(24800));
        assert_eq!(merged.theme, "dark");

        for bad in [
            json!({"unknown": 1}),
            json!({"theme": "blue"}),
            json!({"port": 80}),
            json!({"port": 42420}),
            json!({"switch_delay_ms": 5000}),
            json!({"sharing": "yes"}),
            json!({"lock_hotkey": {"modifiers": 4, "key": 76}}),
            json!({"lock_hotkey": {"modifiers": 2, "key": 0x11}}),
        ] {
            assert!(settings.merged(&values(bad.clone())).is_err(), "{bad}");
        }
        // Clearing the port goes back to the core's default.
        assert_eq!(
            merged.merged(&values(json!({"port": null}))).unwrap().port,
            None
        );
    }

    #[test]
    fn addresses_and_per_device_input_are_checked() {
        let settings = Settings::default();
        assert_eq!(settings.modifier_map, linkcursor_core::ModifierMap::SWAP);
        assert!(settings.keep_awake && settings.hide_cursor_on_leave);
        assert!(settings.switch_hotkeys);
        assert_eq!(settings.switch_modifiers, MOD_CONTROL | MOD_ALT | MOD_SHIFT);
        assert!(
            settings
                .merged(&values(json!({"switch_modifiers": MOD_CONTROL | MOD_ALT})))
                .is_ok()
        );
        for bad in [
            json!({"switch_modifiers": MOD_WIN}),
            json!({"switch_modifiers": 3, "lock_hotkey": {"modifiers": 3, "key": 0x25}}),
        ] {
            assert!(settings.merged(&values(bad.clone())).is_err(), "{bad}");
        }
        assert!(!settings.lock_sync);
        let merged = settings
            .merged(&values(json!({
                "manual_addresses": ["192.168.1.20", "mac-mini.local:42420"],
                "peer_input": {"mac": {"pointer_speed": 1.5, "invert_scroll": true}}
            })))
            .unwrap();
        assert_eq!(merged.manual_addresses.len(), 2);
        let mac = merged.input_for("mac");
        assert_eq!(
            (mac.pointer_speed, mac.scroll_speed, mac.invert_scroll),
            (1.5, 1.0, true)
        );
        assert_eq!(merged.input_for("other"), PeerInput::default());
        for bad in [
            json!({"manual_addresses": ["not an address"]}),
            json!({"manual_addresses": vec!["10.0.0.1"; 33]}),
            json!({"peer_input": {"mac": {"pointer_speed": 9.0}}}),
            json!({"peer_input": {"mac": {"scroll_speed": 0.1}}}),
        ] {
            assert!(settings.merged(&values(bad.clone())).is_err(), "{bad}");
        }
    }
}

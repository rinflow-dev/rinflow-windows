//! Agent preferences other than the effect and the arrangement, saved to
//! `%LOCALAPPDATA%\LinkCursor\settings.json`. Starting at login is not kept
//! here: the `Run` registry value is the truth (see [`autostart`]).

use crate::identity;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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
}

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
            _ => return Err("设置无法读取".to_owned()),
        };
        for (key, value) in values {
            if !object.contains_key(key) {
                return Err(format!("未知的设置项：{key}"));
            }
            object.insert(key.clone(), value.clone());
        }
        let merged: Self = serde_json::from_value(Value::Object(object))
            .map_err(|error| format!("设置的值无效：{error}"))?;
        if !matches!(merged.theme.as_str(), "system" | "light" | "dark") {
            return Err("主题只能是跟随系统、浅色或深色".to_owned());
        }
        if merged.switch_delay_ms > 1000 {
            return Err("切换延迟最长 1000 毫秒".to_owned());
        }
        if !merged.lock_hotkey.is_valid() {
            return Err("快捷键需要包含 Ctrl、Alt 或 Win，再加一个普通按键".to_owned());
        }
        if let Some(port) = merged.port
            && (port < 1024 || port == DISCOVERY_PORT)
        {
            return Err(format!(
                "端口需在 1024–65535 之间，且不能是发现端口 {DISCOVERY_PORT}"
            ));
        }
        Ok(merged)
    }
}

pub struct SettingsStore(Mutex<Settings>);

impl SettingsStore {
    pub fn load() -> Self {
        let settings: Settings = identity::read_json(&path()).unwrap_or_default();
        // A file edited by hand falls back to defaults for what is invalid.
        let settings = settings.merged(&Map::new()).unwrap_or_else(|_| Settings {
            clipboard: settings.clipboard,
            ..Settings::default()
        });
        Self(Mutex::new(settings))
    }

    pub fn get(&self) -> Settings {
        self.0
            .lock()
            .map(|settings| settings.clone())
            .unwrap_or_default()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Settings {
        let Ok(mut settings) = self.0.lock() else {
            return Settings::default();
        };
        change(&mut settings);
        // Saved while holding the lock, so saves cannot interleave.
        if let Ok(text) = serde_json::to_string_pretty(&*settings)
            && let Err(error) = identity::write_file(&path(), &text)
        {
            eprintln!("linkcursor-agent: cannot save settings: {error}");
        }
        settings.clone()
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
}

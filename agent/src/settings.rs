//! Agent preferences other than the effect and the arrangement, saved to
//! `%LOCALAPPDATA%\LinkCursor\settings.json`.

use crate::identity;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::Mutex;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Share the clipboard with peers. Off by default: traffic is not
    /// encrypted yet, and clipboards can hold passwords.
    #[serde(default)]
    pub clipboard: bool,
}

pub struct SettingsStore(Mutex<Settings>);

impl SettingsStore {
    pub fn load() -> Self {
        let settings = fs::read_to_string(path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self(Mutex::new(settings))
    }

    pub fn get(&self) -> Settings {
        self.0
            .lock()
            .map(|settings| settings.clone())
            .unwrap_or_default()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) {
        let Ok(mut settings) = self.0.lock() else {
            return;
        };
        change(&mut settings);
        // Saved while holding the lock, so saves cannot interleave.
        if let Ok(text) = serde_json::to_string_pretty(&*settings)
            && let Err(error) = identity::write_file(&path(), &text)
        {
            eprintln!("linkcursor-agent: cannot save settings: {error}");
        }
    }
}

fn path() -> std::path::PathBuf {
    identity::data_directory().join("settings.json")
}

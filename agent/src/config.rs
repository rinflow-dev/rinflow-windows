// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Everything the agent keeps, in one file:
//! `%LOCALAPPDATA%\Rinflow\config.json`.
//!
//! ```json
//! {
//!   "version": 1,
//!   "device_id": "…",
//!   "settings": { … },   // settings.rs
//!   "layout": { … },     // arrangement.rs: placements, the user's devices
//!   "trusted": [ … ],    // trust.rs: paired devices
//!   "effect": { … }      // effect.rs: only once the tuning page changed it
//! }
//! ```
//!
//! Each module owns its section: it reads it once at start-up and replaces
//! it after every change. This module only keeps the file safe:
//!
//! - **Never half written.** A new version goes to `config.json.tmp`, is
//!   flushed to disk, and replaces the file; the version before stays as
//!   `config.json.bak`. A file that is not JSON (edited by hand) is kept as
//!   `config.json.corrupt` and the backup is used.
//! - **Never overwritten when it could not be read.** A file another program
//!   holds open (a virus scanner, a backup tool) is read again a few times;
//!   if it still cannot be, nothing is saved this session, so what is in it
//!   survives.
//! - **Never waited on by the input hooks.** [`stage`] takes the new section
//!   at once, under the owner's lock, so changes are saved in the order they
//!   were made; [`Pending::write`] writes the file after the owner's lock is
//!   let go, and skips a write a newer one already covered.
//!
//! Versions up to 0.0.1 kept `settings.json`, `layout.json`,
//! `trusted.json`, `effect.json` and `device-id`; the first start moves them
//! into `config.json`. The certificate (`identity.pem`) stays its own file:
//! it holds the private key, and the core reads and writes it.

use serde::Serialize;
use serde_json::{Map, Value};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const FILE: &str = "config.json";
const VERSION: u64 = 1;

/// Section names.
pub const DEVICE_ID: &str = "device_id";
pub const SETTINGS: &str = "settings";
pub const LAYOUT: &str = "layout";
pub const TRUSTED: &str = "trusted";
pub const EFFECT: &str = "effect";

/// The files of versions up to 0.0.1 and the section each became.
const LEGACY: [(&str, &str); 5] = [
    ("device-id", DEVICE_ID),
    ("settings.json", SETTINGS),
    ("layout.json", LAYOUT),
    ("trusted.json", TRUSTED),
    ("effect.json", EFFECT),
];

static CONFIG: OnceLock<Config> = OnceLock::new();

pub struct Config {
    path: PathBuf,
    current: Mutex<Current>,
    /// Taken while writing the file: the change count last written.
    written: Mutex<u64>,
}

struct Current {
    sections: Map<String, Value>,
    /// How many changes `sections` has had.
    changes: u64,
    /// False when the file exists but could not be read: saving would
    /// replace it with what little is known.
    writable: bool,
}

/// A staged change, written by [`Pending::write`].
#[must_use = "the change is only on disk once written"]
pub struct Pending {
    changes: Option<u64>,
}

/// Opens `config.json` in `directory` for the rest of the process. Called
/// once at start-up, before any section is read.
pub fn init(directory: &Path) -> &'static Config {
    CONFIG.get_or_init(|| Config::open(directory))
}

/// The raw value of a section; `None` when it is missing (or before
/// [`init`], as in unit tests).
pub fn section(name: &str) -> Option<Value> {
    CONFIG.get()?.section(name)
}

/// Replaces a section in memory; the file follows with [`Pending::write`].
/// Call it while still holding the lock that guards the owner's data, so
/// two changes are saved in the order they were made.
pub fn stage(name: &str, value: &impl Serialize) -> Pending {
    match CONFIG.get() {
        Some(config) => config.stage(name, value),
        None => Pending { changes: None },
    }
}

/// Whether sections can be saved (the file could be read, or there is none).
pub fn writable() -> bool {
    CONFIG.get().is_some_and(Config::writable)
}

impl Pending {
    /// Writes the file, unless a later write already did. Call it with no
    /// lock held: it waits for the disk.
    pub fn write(self) {
        if let (Some(changes), Some(config)) = (self.changes, CONFIG.get()) {
            config.write(changes);
        }
    }
}

impl Config {
    fn open(directory: &Path) -> Self {
        let path = directory.join(FILE);
        let (sections, writable) = load(&path);
        let config = Self {
            path,
            current: Mutex::new(Current {
                sections,
                changes: 0,
                writable,
            }),
            written: Mutex::new(0),
        };
        if writable && migrate_legacy(directory, &config) {
            log!("moved the settings files of an older version into {FILE}");
        }
        config
    }

    fn section(&self, name: &str) -> Option<Value> {
        let current = self.current.lock().ok()?;
        current.sections.get(name).cloned()
    }

    fn writable(&self) -> bool {
        self.current.lock().is_ok_and(|current| current.writable)
    }

    fn stage(&self, name: &str, value: &impl Serialize) -> Pending {
        let value = match serde_json::to_value(value) {
            Ok(value) => value,
            Err(error) => {
                log!("cannot encode {name}: {error}");
                return Pending { changes: None };
            }
        };
        let Ok(mut current) = self.current.lock() else {
            return Pending { changes: None };
        };
        current.sections.insert(name.to_owned(), value);
        if !current.writable {
            return Pending { changes: None };
        }
        current.changes += 1;
        Pending {
            changes: Some(current.changes),
        }
    }

    fn write(&self, changes: u64) {
        // Writes take turns. One that comes after a write of newer contents
        // has nothing to add, and must not put older ones back.
        let Ok(mut written) = self.written.lock() else {
            return;
        };
        if *written >= changes {
            return;
        }
        let (text, latest) = {
            let Ok(current) = self.current.lock() else {
                return;
            };
            let mut file = Map::new();
            file.insert("version".to_owned(), VERSION.into());
            file.extend(current.sections.clone());
            (
                serde_json::to_string_pretty(&Value::Object(file)),
                current.changes,
            )
        };
        match text {
            Ok(text) => {
                if let Err(error) = replace(&self.path, &text) {
                    log!("cannot save {FILE}: {error}");
                }
            }
            Err(error) => log!("cannot encode {FILE}: {error}"),
        }
        // What was just written includes every change staged so far.
        *written = latest;
    }
}

/// The sections in `path` (or its backup), and whether saving may replace
/// the file.
fn load(path: &Path) -> (Map<String, Value>, bool) {
    let backup = with_suffix(path, ".bak");
    match read_patiently(path) {
        Ok(bytes) => match parse(&bytes) {
            Some(sections) => (sections, true),
            None => {
                let aside = with_suffix(path, ".corrupt");
                log!(
                    "{} is not valid JSON; kept as {} and using the backup",
                    path.display(),
                    aside.display()
                );
                let _ = fs::rename(path, &aside);
                (read_backup(&backup).unwrap_or_default(), true)
            }
        },
        // No file yet (the first start, or one from before 0.0.2), or it was
        // being replaced when the last run ended: the backup, if any.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            (read_backup(&backup).unwrap_or_default(), true)
        }
        Err(error) => {
            log!(
                "cannot read {} ({error}); using the backup and saving nothing \
                 this session, so the file is kept as it is",
                path.display()
            );
            (read_backup(&backup).unwrap_or_default(), false)
        }
    }
}

fn read_backup(backup: &Path) -> Option<Map<String, Value>> {
    let sections = parse(&read_patiently(backup).ok()?)?;
    log!("using {}", backup.display());
    Some(sections)
}

/// The top-level object without `version`; `None` when it is not one.
fn parse(bytes: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice(bytes).ok()? {
        Value::Object(mut sections) => {
            sections.remove("version");
            Some(sections)
        }
        _ => None,
    }
}

/// Reads a file, trying again for about a second while another program
/// holds it (a sharing violation, say). A missing file is not retried.
fn read_patiently(path: &Path) -> io::Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        match fs::read(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            result => return result,
        }
    }
}

/// Moves the files of versions up to 0.0.1 into `config`, for sections it
/// does not have yet. A file that cannot be read now stays for the next
/// start; one that is not valid is kept as `<name>.corrupt`. True when
/// anything moved.
fn migrate_legacy(directory: &Path, config: &Config) -> bool {
    let mut moved = Vec::new();
    let mut pending = None;
    for (file, name) in LEGACY {
        let path = directory.join(file);
        let bytes = match read_patiently(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                log!("cannot read {} ({error}); moving it later", path.display());
                continue;
            }
        };
        if config.section(name).is_some() {
            // Already moved (a start that could not delete it).
            moved.push(path);
            continue;
        }
        let value = if name == DEVICE_ID {
            let id = String::from_utf8_lossy(&bytes).trim().to_owned();
            (!id.is_empty()).then_some(Value::String(id))
        } else {
            serde_json::from_slice::<Value>(&bytes).ok()
        };
        match value {
            Some(value) => {
                pending = Some(config.stage(name, &value));
                moved.push(path);
            }
            None => {
                let aside = with_suffix(&path, ".corrupt");
                log!(
                    "{} is not valid; kept as {}",
                    path.display(),
                    aside.display()
                );
                let _ = fs::rename(&path, aside);
            }
        }
    }
    if moved.is_empty() {
        return false;
    }
    if let Some(pending) = pending
        && let Some(changes) = pending.changes
    {
        config.write(changes);
    }
    // Only once config.json holds them.
    if !config.path.exists() {
        return false;
    }
    for path in moved {
        if let Err(error) = fs::remove_file(&path) {
            log!("cannot remove {} ({error})", path.display());
        }
    }
    true
}

/// Replaces `path` with `text` without ever leaving it half written: a
/// crash or power loss mid-write would otherwise make it unreadable. The
/// version it replaces stays as `<path>.bak`.
fn replace(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let temporary = with_suffix(path, ".tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // On disk before it replaces the old file, not only in the cache.
        file.sync_all()?;
    }
    if path.exists() {
        // Between these two renames there is only the backup; `load` takes
        // it then.
        fs::rename(path, with_suffix(path, ".bak"))?;
    }
    fs::rename(&temporary, path)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fresh, empty directory for one test.
    fn directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rinflow-config-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn stage_and_write(config: &Config, name: &str, value: Value) {
        let pending = config.stage(name, &value);
        config.write(pending.changes.unwrap());
    }

    fn on_disk(directory: &Path) -> Value {
        serde_json::from_slice(&fs::read(directory.join(FILE)).unwrap()).unwrap()
    }

    #[test]
    fn sections_are_saved_together_with_a_backup() {
        let directory = directory("together");
        let config = Config::open(&directory);
        stage_and_write(&config, SETTINGS, json!({"sharing": false}));
        stage_and_write(&config, TRUSTED, json!([]));
        assert_eq!(
            on_disk(&directory),
            json!({"version": 1, "settings": {"sharing": false}, "trusted": []})
        );
        let backup: Value =
            serde_json::from_slice(&fs::read(directory.join("config.json.bak")).unwrap()).unwrap();
        assert_eq!(
            backup,
            json!({"version": 1, "settings": {"sharing": false}})
        );

        let again = Config::open(&directory);
        assert_eq!(again.section(SETTINGS), Some(json!({"sharing": false})));
        assert_eq!(again.section("version"), None);
    }

    #[test]
    fn an_older_write_never_replaces_a_newer_one() {
        let directory = directory("order");
        let config = Config::open(&directory);
        let first = config.stage(SETTINGS, &json!({"theme": "light"}));
        let second = config.stage(SETTINGS, &json!({"theme": "dark"}));
        config.write(second.changes.unwrap());
        config.write(first.changes.unwrap());
        assert_eq!(on_disk(&directory)["settings"]["theme"], "dark");
    }

    #[test]
    fn a_broken_file_is_kept_and_the_backup_used() {
        let directory = directory("broken");
        fs::write(directory.join(FILE), b"{ not json").unwrap();
        fs::write(
            directory.join("config.json.bak"),
            br#"{"version":1,"device_id":"abc"}"#,
        )
        .unwrap();
        let config = Config::open(&directory);
        assert_eq!(config.section(DEVICE_ID), Some(json!("abc")));
        assert!(config.writable());
        assert_eq!(
            fs::read(directory.join("config.json.corrupt")).unwrap(),
            b"{ not json"
        );
    }

    #[test]
    fn a_file_between_renames_is_read_from_the_backup() {
        let directory = directory("between");
        fs::write(
            directory.join("config.json.bak"),
            br#"{"version":1,"trusted":[]}"#,
        )
        .unwrap();
        let config = Config::open(&directory);
        assert_eq!(config.section(TRUSTED), Some(json!([])));
    }

    #[test]
    fn older_files_move_into_config_json() {
        let directory = directory("legacy");
        fs::write(directory.join("device-id"), "f93f7f7f\r\n").unwrap();
        fs::write(directory.join("settings.json"), r#"{"sharing":false}"#).unwrap();
        fs::write(directory.join("trusted.json"), "[]").unwrap();
        fs::write(directory.join("layout.json"), "{ broken").unwrap();
        fs::write(directory.join("identity.pem"), "key").unwrap();
        let config = Config::open(&directory);
        assert_eq!(config.section(DEVICE_ID), Some(json!("f93f7f7f")));
        assert_eq!(
            on_disk(&directory),
            json!({"version": 1, "device_id": "f93f7f7f", "settings": {"sharing": false}, "trusted": []})
        );
        assert!(!directory.join("device-id").exists());
        assert!(!directory.join("settings.json").exists());
        assert!(!directory.join("trusted.json").exists());
        assert!(directory.join("layout.json.corrupt").exists());
        // The certificate is not touched.
        assert!(directory.join("identity.pem").exists());
    }
}

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{config, win};
use std::fs;
use std::io;
use std::path::PathBuf;

pub struct Identity {
    pub device_id: String,
    pub device_name: String,
    /// String SID of the signed-in user; scopes the IPC pipe to this user.
    pub user_sid: String,
}

/// Opens `config.json` and reads this PC's identity from it. Called once at
/// start-up, before anything else reads the configuration.
pub fn load() -> io::Result<Identity> {
    let directory = data_directory();
    fs::create_dir_all(&directory)?;
    config::init(&directory);
    Ok(Identity {
        device_id: load_or_create_device_id()?,
        device_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".to_owned()),
        user_sid: win::current_user_sid()?,
    })
}

/// `%LOCALAPPDATA%\Rinflow`, shared with the settings app.
pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rinflow")
}

/// The device ID, made on first use. Only a configuration without one gets
/// a new one: when `config.json` exists but cannot be read now (locked by a
/// scanner, a disk error), starting is an error. Pairings and placements on
/// every peer are keyed by this ID, so a new one would turn this PC into a
/// stranger to all of them.
fn load_or_create_device_id() -> io::Result<String> {
    if let Some(id) = config::section(config::DEVICE_ID)
        .as_ref()
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        return Ok(id.to_owned());
    }
    if !config::writable() {
        return Err(io::Error::other(
            "cannot read config.json; it is kept, not replaced",
        ));
    }
    let id = win::new_guid_string()?;
    config::stage(config::DEVICE_ID, &id).write();
    Ok(id)
}

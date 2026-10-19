use crate::win;
use std::fs;
use std::io;
use std::path::PathBuf;

pub struct Identity {
    pub device_id: String,
    pub device_name: String,
    /// String SID of the signed-in user; scopes the IPC pipe to this user.
    pub user_sid: String,
}

pub fn load() -> io::Result<Identity> {
    let directory = data_directory();
    fs::create_dir_all(&directory)?;
    Ok(Identity {
        device_id: load_or_create_device_id(&directory)?,
        device_name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".to_owned()),
        user_sid: win::current_user_sid()?,
    })
}

/// `%LOCALAPPDATA%\LinkCursor`, shared with the settings app.
pub fn data_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LinkCursor")
}

fn load_or_create_device_id(directory: &std::path::Path) -> io::Result<String> {
    let path = directory.join("device-id");
    if let Ok(stored) = fs::read_to_string(&path) {
        let stored = stored.trim();
        if !stored.is_empty() {
            return Ok(stored.to_owned());
        }
    }
    let id = win::new_guid_string()?;
    fs::write(&path, &id)?;
    Ok(id)
}

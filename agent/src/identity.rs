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

/// Replaces a settings file without ever leaving it half written: a crash or
/// power loss mid-write would otherwise make it unreadable and lose it.
pub fn write_file(path: &std::path::Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let temporary = path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // On disk before it replaces the old file, not only in the cache.
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// Reads a JSON settings file; `None` when there is none. One that cannot be
/// read is moved aside to `<name>.corrupt` and the defaults are used, so the
/// next save does not overwrite what may still be recovered by hand.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            eprintln!(
                "linkcursor-agent: {} is unreadable ({error}); kept as {} and using defaults",
                path.display(),
                std::path::Path::new(&aside).display()
            );
            let _ = fs::rename(path, &aside);
            None
        }
    }
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
    write_file(&path, &id)?;
    Ok(id)
}

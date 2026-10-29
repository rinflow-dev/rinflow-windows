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
            log!(
                "{} is unreadable ({error}); kept as {} and using defaults",
                path.display(),
                std::path::Path::new(&aside).display()
            );
            let _ = fs::rename(path, &aside);
            None
        }
    }
}

/// The device ID, made on first use. Only a missing (or empty) file gets a
/// new one: a file that cannot be read now (locked by a scanner, a disk
/// error) is an error. Pairings and placements on every peer are keyed by
/// this ID, so a new one would turn this PC into a stranger to all of them.
fn load_or_create_device_id(directory: &std::path::Path) -> io::Result<String> {
    let path = directory.join("device-id");
    match fs::read_to_string(&path) {
        Ok(stored) if !stored.trim().is_empty() => return Ok(stored.trim().to_owned()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("cannot read {} ({error}); it is kept, not replaced", path.display()),
            ));
        }
    }
    let id = win::new_guid_string()?;
    write_file(&path, &id)?;
    Ok(id)
}

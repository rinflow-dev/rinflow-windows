//! A small log for the programs that run as LocalSystem, where nobody sees
//! their console: `logs\<name>.log` in their own folder (Program Files when
//! installed), which only administrators and LocalSystem can change. Not
//! `%ProgramData%`: any user may create a folder there first, owning it, and
//! turn it into a junction that sends LocalSystem's writes somewhere else.
//! Started from a console (a development run), it writes to stderr instead.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);

/// Keeps one older file beside the log; a new one starts past this size.
const MAX_SIZE: u64 = 1024 * 1024;

pub fn logs_dir() -> Option<PathBuf> {
    crate::win::own_dir().map(|dir| dir.join("logs"))
}

/// Logs to the file `name` when running as LocalSystem; to stderr otherwise.
pub fn init(name: &str) {
    if !crate::win::is_local_system() {
        return;
    }
    let Some(dir) = logs_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("{name}.log"));
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_SIZE) {
        let _ = std::fs::rename(&path, dir.join(format!("{name}.old.log")));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path)
        && let Ok(mut slot) = FILE.lock()
    {
        *slot = Some(file);
    }
}

pub fn write(message: std::fmt::Arguments<'_>) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0);
    let line = format!("{seconds:.3} [{}] {message}\n", std::process::id());
    match FILE.lock().ok().as_deref_mut().and_then(Option::as_mut) {
        Some(file) => {
            let _ = file.write_all(line.as_bytes());
        }
        None => eprint!("{line}"),
    }
}

#[macro_export]
macro_rules! log {
    ($($argument:tt)*) => {
        $crate::log::write(format_args!($($argument)*))
    };
}

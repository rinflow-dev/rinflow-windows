//! The agent's log: `%LOCALAPPDATA%\LinkCursor\logs\agent.log`, for finding
//! out what happened when something went wrong ("打开日志文件夹" in the
//! settings app). Release builds have no console, so without it nothing
//! the agent noticed would be kept.
//!
//! Each line starts with the local time. The file is moved aside to
//! `agent.1.log` once it passes [`MAX_SIZE`], so two files at most are
//! kept. Clipboard contents, keys and anything else typed never go into it;
//! see `core_event` for which core events do.
//!
//! [`log!`](crate::log) writes a line; debug builds also print it.

use crate::identity;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

/// Moved aside past this size.
const MAX_SIZE: u64 = 2 * 1024 * 1024;

struct Log {
    file: Option<File>,
    written: u64,
}

static LOG: Mutex<Log> = Mutex::new(Log {
    file: None,
    written: 0,
});

/// Where the agent's log lives (the settings app opens this folder).
pub fn directory() -> PathBuf {
    identity::data_directory().join("logs")
}

fn path() -> PathBuf {
    directory().join("agent.log")
}

/// Opens the log, and logs panics too. Called once, first thing.
pub fn init() {
    let _ = fs::create_dir_all(directory());
    if let Ok(mut log) = LOG.lock() {
        open(&mut log);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_line(&format!("panic: {info}"));
        default_hook(info);
    }));
    write_line(&format!(
        "LinkCursor agent {} started (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
}

fn open(log: &mut Log) {
    let path = path();
    let size = fs::metadata(&path).map_or(0, |metadata| metadata.len());
    if size >= MAX_SIZE {
        let _ = fs::rename(&path, directory().join("agent.1.log"));
    }
    log.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    log.written = fs::metadata(&path).map_or(0, |metadata| metadata.len());
}

/// Writes one line (use [`log!`](crate::log)).
pub fn write_line(message: &str) {
    if cfg!(debug_assertions) {
        eprintln!("linkcursor-agent: {message}");
    }
    let Ok(mut log) = LOG.lock() else {
        return;
    };
    if log.written >= MAX_SIZE {
        log.file = None;
        open(&mut log);
    }
    let line = format!("{} {message}\n", timestamp());
    if let Some(file) = log.file.as_mut()
        && file.write_all(line.as_bytes()).is_ok()
    {
        log.written += line.len() as u64;
    }
}

/// `2026-10-23 01:22:48.123`, local time.
fn timestamp() -> String {
    // SAFETY: fills the structure it is given.
    let time = unsafe {
        let mut time = std::mem::zeroed();
        GetLocalTime(&mut time);
        time
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        time.wYear,
        time.wMonth,
        time.wDay,
        time.wHour,
        time.wMinute,
        time.wSecond,
        time.wMilliseconds
    )
}

/// Logs the core events that tell what happened to connections, pairing
/// and transfers. Not clipboards (their contents), progress or latency
/// (too many).
pub fn core_event(json: &str) {
    const LOGGED: [&str; 21] = [
        "started",
        "stopped",
        "error",
        "connecting",
        "connected",
        "disconnected",
        "peer_up",
        "peer_down",
        "control_revoked",
        "remote_control_started",
        "remote_control_ended",
        "input_sequence_gap",
        "pairing_requested",
        "paired",
        "pairing_ended",
        "unpaired",
        "manual_addresses",
        "files_receiving",
        "files_received",
        "files_failed",
        "files_send_ended",
    ];
    let Ok(mut event) = serde_json::from_str::<Value>(json) else {
        return;
    };
    let Some(kind) = event["event"].as_str().map(str::to_owned) else {
        return;
    };
    if LOGGED.contains(&kind.as_str()) || kind == "lock_requested" || kind == "peer_pointer_lock" {
        // The pairing code is for the user's eyes only.
        if let Some(object) = event.as_object_mut() {
            object.remove("code");
        }
        write_line(&format!("core: {event}"));
    }
}

/// Writes a line to the agent's log (and, in debug builds, to stderr).
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write_line(&format!($($arg)*))
    };
}

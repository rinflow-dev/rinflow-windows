// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Full-screen windows and games in front, for the edges (`edge.rs`: no
//! crossing out of a full-screen game, setting `lock_in_game`) and for a
//! peer's motion (`inject.rs`: a game that holds the pointer gets relative
//! moves).
//!
//! A full-screen game is a window that fills its monitor and one of:
//!
//! - Direct3D exclusive full screen (`SHQueryUserNotificationState`);
//! - the cursor is hidden while the mouse moves. A video player hides an
//!   idle cursor too, but shows it as soon as the mouse moves, and the
//!   pointer has to move to reach an edge;
//! - its program is in Windows' list of games, the one Game Bar and Game
//!   Mode go by (`HKCU\System\GameConfigStore\Children`, `MatchedExeFullPath`).
//!   That list is not documented and not complete (entries matched by a
//!   Store package carry no path), so it only adds the games that show a
//!   cursor full screen, such as strategy games that scroll at the edges.
//!
//! A full-screen video, browser or presentation is none of these.

use crate::win::wide;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};
use windows_sys::Win32::UI::Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GWL_STYLE, GetClassNameW, GetCursorInfo, GetForegroundWindow,
    GetShellWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsZoomed, WS_CAPTION,
};

/// The window in front covers its whole monitor: a full-screen game, video
/// or presentation. Not an ordinary maximized window (title bar; it leaves
/// the taskbar out unless that hides itself), and not the desktop.
pub fn foreground_full_screen() -> Option<HWND> {
    // SAFETY: plain window and monitor queries into properly sized
    // structures; a window that is gone meanwhile only makes them fail.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() || window == GetShellWindow() {
            return None;
        }
        let mut class = [0u16; 16];
        let length = GetClassNameW(window, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        if class == "Progman" || class == "WorkerW" {
            return None;
        }
        let style = GetWindowLongW(window, GWL_STYLE) as u32;
        if IsZoomed(window) != 0 && style & WS_CAPTION == WS_CAPTION {
            return None;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut rect) == 0 {
            return None;
        }
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(monitor, &mut info) != 0 && covers(rect, info.rcMonitor)).then_some(window)
    }
}

/// `window` covers all of `monitor` (it may reach beyond it).
fn covers(window: RECT, monitor: RECT) -> bool {
    monitor.right > monitor.left
        && monitor.bottom > monitor.top
        && window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// The cursor is hidden: its display count is below zero, or it has no
/// shape at all, as games that call `SetCursor(NULL)` leave it. False when
/// that cannot be read.
pub fn cursor_hidden() -> bool {
    // SAFETY: fills a properly sized structure.
    unsafe {
        let mut info: CURSORINFO = std::mem::zeroed();
        info.cbSize = size_of::<CURSORINFO>() as u32;
        GetCursorInfo(&mut info) != 0
            && (info.flags & CURSOR_SHOWING == 0 || info.hCursor.is_null())
    }
}

/// Whether the last [`game_running`] found a game, so a change is logged once.
static GAME_IN_FRONT: AtomicBool = AtomicBool::new(false);

/// A full-screen game is in front (see the module docs).
pub fn game_running() -> bool {
    let reason = foreground_full_screen().and_then(game_reason);
    let running = reason.is_some();
    if GAME_IN_FRONT.swap(running, Ordering::Relaxed) != running {
        match reason {
            Some(reason) => log!("a full-screen game is in front ({reason}): no switching"),
            None => log!("no full-screen game in front: switching again"),
        }
    }
    running
}

fn game_reason(window: HWND) -> Option<&'static str> {
    if direct3d_full_screen() {
        Some("Direct3D full screen")
    } else if cursor_hidden() {
        Some("cursor hidden")
    } else if listed_game(window) {
        Some("in Windows' list of games")
    } else {
        None
    }
}

fn direct3d_full_screen() -> bool {
    let mut state = 0;
    // SAFETY: writes the notification state into `state`.
    let ok = unsafe { SHQueryUserNotificationState(&mut state) } == 0;
    ok && state == QUNS_RUNNING_D3D_FULL_SCREEN
}

/// The program last looked up in the list of games, and whether it is there:
/// the list is read again only when another program comes to the front.
static LAST_LOOKUP: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// The program of `window` is in Windows' list of games.
fn listed_game(window: HWND) -> bool {
    let mut pid = 0;
    // SAFETY: writes the process ID into `pid`.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    let Some(image) = rinflow_input::win::process_image(pid) else {
        return false;
    };
    let image = image.to_string_lossy().into_owned();
    let mut last = LAST_LOOKUP
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some((path, listed)) = last.as_ref()
        && path.eq_ignore_ascii_case(&image)
    {
        return *listed;
    }
    let listed = listed_game_paths()
        .iter()
        .any(|path| path.eq_ignore_ascii_case(&image));
    *last = Some((image, listed));
    listed
}

/// The programs in Windows' list of games, by full path.
fn listed_game_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let subkey = wide(r"System\GameConfigStore\Children");
    let value = wide("MatchedExeFullPath");
    let mut key: HKEY = null_mut();
    // SAFETY: opens a key of this user's hive for reading; closed below.
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return paths;
    }
    for index in 0.. {
        // Each child is named by a GUID.
        let mut name = [0u16; 64];
        let mut length = name.len() as u32;
        // SAFETY: `name` holds `length` units, terminator included.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut length,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        let mut buffer = vec![0u16; 1024];
        let mut size = (buffer.len() * size_of::<u16>()) as u32;
        // SAFETY: `name` is null-terminated; `buffer` holds `size` bytes.
        let status = unsafe {
            RegGetValueW(
                key,
                name.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status == 0 {
            let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
            if length > 0 {
                paths.push(String::from_utf16_lossy(&buffer[..length]));
            }
        }
    }
    // SAFETY: opened above.
    unsafe { RegCloseKey(key) };
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_screen_window_covers_its_monitor() {
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        let monitor = rect(0, 0, 2880, 1920);
        assert!(covers(rect(0, 0, 2880, 1920), monitor));
        // Some games' windows reach a little beyond the monitor.
        assert!(covers(rect(-8, -8, 2888, 1928), monitor));
        // A maximized window leaves the taskbar out.
        assert!(!covers(rect(-8, -8, 2888, 1848), monitor));
        // A window on the next monitor.
        assert!(!covers(rect(2880, 0, 5760, 1920), monitor));
        assert!(!covers(rect(0, 0, 0, 0), rect(0, 0, 0, 0)));
    }

    #[test]
    fn reading_the_list_of_games_does_not_fail() {
        // Empty where Game Bar never ran; the paths are full paths.
        for path in listed_game_paths() {
            assert!(path.contains('\\'), "{path}");
        }
    }
}

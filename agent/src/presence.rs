// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Presence: this PC stays awake while another of the user's devices is in
//! use, and devices lock together (the core's "Presence" section).
//!
//! - Activity: every few seconds, if this PC's keyboard or mouse was used
//!   lately (`GetLastInputInfo`), peers are told (`report_user_activity`).
//!   The nudges this module plays to stay awake do not count, so two devices
//!   never keep each other awake for ever.
//! - Staying awake (setting `keep_awake`): while a peer reports activity
//!   (`peer_active`), the display stays on (`SetThreadExecutionState`) and
//!   a mouse move of zero pixels every half minute resets the idle timer
//!   that the screen saver and the idle lock go by, as Mouse Without
//!   Borders does. A minute after the last report it lets go.
//! - Locking together (setting `lock_sync`): when this PC is locked, peers
//!   are told (`lock_peers`); a peer's `lock_requested` locks this PC.

use crate::inject::INJECTED_MARKER;
use crate::settings::SettingsStore;
use crate::win::wide;
use rinflow_core::Core;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows_sys::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows_sys::Win32::System::Shutdown::LockWorkStation;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, INPUT, INPUT_0, INPUT_MOUSE, LASTINPUTINFO, MOUSEEVENTF_MOVE, MOUSEINPUT,
    SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, MSG,
    PostThreadMessageW, RegisterClassW, SetTimer, TranslateMessage, WM_APP, WM_QUIT, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Thread messages from the core's callback.
const WM_PEER_ACTIVE: u32 = WM_APP + 1;
const WM_LOCK_REQUESTED: u32 = WM_APP + 2;
/// From the window procedure: this session was locked.
const WM_SESSION_LOCKED: u32 = WM_APP + 3;
const WTS_SESSION_LOCK: usize = 7;
const TIMER: usize = 1;
const TICK_MS: u32 = 5_000;
/// Input this recent counts as someone using this PC.
const ACTIVE_WITHIN_MS: u32 = 10_000;
/// How long a peer's report keeps this PC awake.
const AWAKE_FOR: Duration = Duration::from_secs(60);
/// How often the idle timer is reset meanwhile.
const NUDGE_EVERY: Duration = Duration::from_secs(30);
/// A lock this soon after a peer's request was that request: not passed on.
const REQUESTED_LOCK: Duration = Duration::from_secs(5);

pub struct PresenceWatch {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

/// Hands the presence thread what it needs from the core's events.
#[derive(Clone, Copy)]
pub struct PresenceHandle {
    thread_id: u32,
}

impl PresenceHandle {
    pub fn core_event(&self, json: &str) {
        let message = if json.contains("\"peer_active\"") {
            WM_PEER_ACTIVE
        } else if json.contains("\"lock_requested\"") {
            WM_LOCK_REQUESTED
        } else {
            return;
        };
        // SAFETY: posts to the presence thread's queue.
        unsafe {
            PostThreadMessageW(self.thread_id, message, 0, 0);
        }
    }
}

impl PresenceWatch {
    pub fn start(core: Weak<Core>, settings: Arc<SettingsStore>) -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("rinflow-presence".to_owned())
            .spawn(move || {
                let window = match create_window() {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // SAFETY: plain thread id query; the queue exists (the window
                // was created on this thread).
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let mut presence = Presence {
                    core,
                    settings,
                    awake_until: None,
                    awake: false,
                    nudged: None,
                    nudge_tick: None,
                    lock_requested: None,
                };
                // SAFETY: standard message loop of this thread's window.
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        match (message.hwnd.is_null(), message.message) {
                            (true, WM_PEER_ACTIVE) => presence.peer_active(),
                            (true, WM_LOCK_REQUESTED) => presence.lock_requested(),
                            (true, WM_SESSION_LOCKED) => presence.locked(),
                            (false, WM_TIMER) => presence.tick(),
                            _ => {
                                TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                    }
                    presence.let_sleep();
                    KillTimer(window, TIMER);
                    WTSUnRegisterSessionNotification(window);
                    DestroyWindow(window);
                }
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                join: Some(join),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("presence thread exited during start-up")),
        }
    }

    pub fn handle(&self) -> PresenceHandle {
        PresenceHandle {
            thread_id: self.thread_id,
        }
    }
}

impl Drop for PresenceWatch {
    fn drop(&mut self) {
        // SAFETY: asks the presence thread's message loop to finish.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Presence {
    core: Weak<Core>,
    settings: Arc<SettingsStore>,
    /// A peer is in use: stay awake until then.
    awake_until: Option<Instant>,
    /// The execution state asks for the display to stay on.
    awake: bool,
    /// When the idle timer was last reset, and its tick count.
    nudged: Option<Instant>,
    nudge_tick: Option<u32>,
    /// When a peer last asked this PC to lock.
    lock_requested: Option<Instant>,
}

impl Presence {
    fn tick(&mut self) {
        self.report_activity();
        let wanted = self.settings.get().keep_awake
            && self.awake_until.is_some_and(|until| Instant::now() < until);
        if !wanted {
            self.let_sleep();
            return;
        }
        if !self.awake {
            self.awake = true;
            log!("another device is in use: keeping this PC awake");
            // SAFETY: changes this thread's execution state only.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED);
            }
        }
        if self.nudged.is_none_or(|at| at.elapsed() >= NUDGE_EVERY) {
            self.nudge();
        }
    }

    /// Tells peers when someone used this PC lately; the nudges played
    /// here are not someone.
    fn report_activity(&self) {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: fills the structure it is given; plain tick query.
        let (ok, now) = unsafe { (GetLastInputInfo(&mut info) != 0, GetTickCount()) };
        if !ok || !someone_active(now, info.dwTime, self.nudge_tick) {
            return;
        }
        if let Some(core) = self.core.upgrade() {
            let _ = core.report_user_activity();
        }
    }

    /// A zero-pixel mouse move: input as far as idle timers go, nothing
    /// anyone can see. Not on a locked or secure desktop.
    fn nudge(&mut self) {
        if !crate::win::input_desktop_is_default() {
            return;
        }
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: INJECTED_MARKER,
                },
            },
        };
        // SAFETY: one initialised INPUT with its size; plain tick query.
        unsafe {
            SendInput(1, &input, size_of::<INPUT>() as i32);
            self.nudge_tick = Some(GetTickCount());
        }
        self.nudged = Some(Instant::now());
    }

    fn let_sleep(&mut self) {
        if self.awake {
            self.awake = false;
            log!("no other device in use: this PC may sleep again");
            // SAFETY: changes this thread's execution state only.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }

    fn peer_active(&mut self) {
        self.awake_until = Some(Instant::now() + AWAKE_FOR);
        self.tick();
    }

    fn lock_requested(&mut self) {
        if !self.settings.get().lock_sync {
            return;
        }
        log!("a peer was locked: locking this PC too");
        self.lock_requested = Some(Instant::now());
        // SAFETY: plain call; fails harmlessly when already locked.
        unsafe {
            LockWorkStation();
        }
    }

    /// This PC was locked: peers lock too, unless a peer's lock caused it.
    fn locked(&mut self) {
        self.awake_until = None;
        self.let_sleep();
        if !self.settings.get().lock_sync
            || self
                .lock_requested
                .is_some_and(|at| at.elapsed() < REQUESTED_LOCK)
        {
            return;
        }
        if let Some(core) = self.core.upgrade() {
            log!("this PC was locked: locking the other devices");
            let _ = core.lock_peers();
        }
    }
}

/// Someone used this PC lately: its last input (`last_input`, a tick count)
/// is at most [`ACTIVE_WITHIN_MS`] old at `now`, and is not the nudge played
/// at `nudge_tick`. The system stamps the nudge a moment before or after
/// the tick taken right after playing it, so anything within half a second
/// either way is the nudge. Tick counts wrap after 49 days.
fn someone_active(now: u32, last_input: u32, nudge_tick: Option<u32>) -> bool {
    let recent = now.wrapping_sub(last_input) <= ACTIVE_WITHIN_MS;
    let nudge =
        nudge_tick.is_some_and(|tick| (last_input.wrapping_sub(tick) as i32).unsigned_abs() <= 500);
    recent && !nudge
}

/// A hidden window: session notifications need one.
fn create_window() -> io::Result<HWND> {
    let class_name = wide("RinflowPresence");
    // SAFETY: registers the class (again is harmless), creates a hidden
    // window on this thread and registers it for session notifications.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        if WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) == 0 {
            log!(
                "cannot watch for this PC being locked: {}",
                io::Error::last_os_error()
            );
        }
        SetTimer(window, TIMER, TICK_MS, None);
        Ok(window)
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Session changes may be sent rather than posted: handed to the message
    // loop, which owns the state.
    if message == WM_WTSSESSION_CHANGE && wparam == WTS_SESSION_LOCK {
        // SAFETY: posts to this thread's own queue.
        unsafe {
            PostThreadMessageW(GetCurrentThreadId(), WM_SESSION_LOCKED, 0, 0);
        }
        return 0;
    }
    // SAFETY: default handling (WM_TIMER is taken in the message loop).
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::someone_active;

    #[test]
    fn real_input_counts_and_the_nudges_do_not() {
        let now = 100_000;
        // Typed two seconds ago, no nudge yet.
        assert!(someone_active(now, now - 2_000, None));
        // Long ago.
        assert!(!someone_active(now, now - 60_000, None));
        // The nudge, stamped a little after or a little before its tick.
        assert!(!someone_active(now, now - 1_000, Some(now - 1_016)));
        assert!(!someone_active(now, now - 1_016, Some(now - 1_000)));
        // Real input a few seconds after the last nudge.
        assert!(someone_active(now, now - 1_000, Some(now - 9_000)));
        // Across the 49-day wrap of the tick count.
        let wrapped = 5_u32;
        assert!(someone_active(wrapped, u32::MAX - 100, None));
        assert!(!someone_active(
            wrapped,
            u32::MAX - 100,
            Some(u32::MAX - 90)
        ));
    }
}

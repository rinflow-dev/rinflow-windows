// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Hides this PC's cursor where a peer's pointer left it (setting
//! `hide_cursor_on_leave`), as Universal Control, Deskflow and Mouse Without
//! Borders do: the device the pointer is not on shows none.
//!
//! - When the last peer driving this PC gives control back
//!   (`remote_control_ended`, reason `remote reset`: its pointer moved on),
//!   a 1×1 window that shows no cursor goes under the cursor, topmost and
//!   without taking focus, and this thread's cursor display count drops
//!   below zero (`ShowCursor`), so another program's `SetCursor` does not
//!   bring it back. What lies under the cursor gets no hover effects or
//!   tooltips meanwhile.
//! - A low-level mouse hook, installed only while hidden, sees this PC's own
//!   mouse, touchpad, pen or touch screen. Motion takes the window away, and
//!   the cursor shows where it was. A button or the wheel would go to the
//!   window under the cursor, which Windows picked before the hook ran: the
//!   hook holds it back instead, takes the window away, and this thread
//!   plays it again (after a zero-pixel move, so Windows picks again) with
//!   whatever buttons followed meanwhile, in order. So the first click lands
//!   where it was aimed, even a touchpad tap whose release comes at once.
//! - Motion the agent played does not show it: the last moves of the peer
//!   that left often arrive a little after its reset (the input service
//!   plays them asynchronously), and the window follows them instead.
//! - A peer driving this PC again, this PC starting to drive a peer
//!   (`capture.rs`) and the setting turned off show it at once.
//!
//! Other ends of remote control (the connection lost, control taken over)
//! leave the cursor as it is.

use crate::inject::INJECTED_MARKER;
use crate::settings::SettingsStore;
use crate::win::wide;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HC_ACTION, HHOOK, HWND_TOPMOST, LWA_ALPHA, MSG, MSLLHOOKSTRUCT,
    PostThreadMessageW, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor,
    SetLayeredWindowAttributes, SetWindowPos, SetWindowsHookExW, ShowCursor, ShowWindow,
    TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Thread messages.
const WM_HIDE: u32 = WM_APP + 1;
const WM_REVEAL: u32 = WM_APP + 2;
/// From the hook: play the buttons it held back.
const WM_REPLAY: u32 = WM_APP + 3;

/// The hider thread, for [`reveal`] from anywhere; 0 before it runs.
static THREAD: AtomicU32 = AtomicU32::new(0);

/// Shows the cursor if it is hidden (from any thread).
pub fn reveal() {
    post(WM_REVEAL);
}

fn post(message: u32) {
    let thread_id = THREAD.load(Ordering::SeqCst);
    if thread_id != 0 {
        // SAFETY: posts to the hider thread's queue.
        unsafe {
            PostThreadMessageW(thread_id, message, 0, 0);
        }
    }
}

pub struct Hider {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
    controllers: Arc<Mutex<HashSet<String>>>,
}

/// Hands the hider the core's remote-control events.
#[derive(Clone)]
pub struct HiderHandle {
    controllers: Arc<Mutex<HashSet<String>>>,
}

impl HiderHandle {
    pub fn core_event(&self, json: &str) {
        if !json.contains("remote_control_") {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Ok(mut controllers) = self.controllers.lock() else {
            return;
        };
        match control_change(&mut controllers, &event) {
            Change::Hide => post(WM_HIDE),
            Change::Reveal => post(WM_REVEAL),
            Change::None => {}
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Hide,
    Reveal,
    None,
}

/// What a remote-control event means for the cursor, keeping track of the
/// peers that drive this PC: a peer arriving shows it; the last one moving
/// its pointer away hides it.
fn control_change(controllers: &mut HashSet<String>, event: &Value) -> Change {
    let peer_id = event["peer_id"].as_str().unwrap_or_default().to_owned();
    match event["event"].as_str() {
        Some("remote_control_started") => {
            controllers.insert(peer_id);
            Change::Reveal
        }
        Some("remote_control_ended") => {
            controllers.remove(&peer_id);
            if controllers.is_empty() && event["reason"] == "remote reset" {
                Change::Hide
            } else {
                Change::None
            }
        }
        _ => Change::None,
    }
}

/// What the hook does with one mouse event.
#[derive(Debug, PartialEq, Eq)]
enum HookAction {
    /// Let it through.
    Pass,
    /// Motion: show the cursor and let it through.
    Show,
    /// Motion the agent played (the last moves of a peer that just left,
    /// still on their way through the input service): the window follows,
    /// and the cursor stays hidden.
    Follow,
    /// The first button or wheel: show the cursor, hold the event back and
    /// play it again.
    Replay,
    /// A button or wheel while that replay is pending: hold it back too.
    Queue,
}

/// `hidden_at` is where the cursor is hidden; `replaying` that the window is
/// gone but held-back events wait; `ours` that the event carries the agent's
/// marker. A move of zero pixels (the presence nudge) changes nothing.
fn hook_action(
    hidden_at: Option<POINT>,
    replaying: bool,
    message: u32,
    point: POINT,
    ours: bool,
) -> HookAction {
    if replaying {
        // The replay itself, and motion meanwhile, go through.
        return if !ours && message != WM_MOUSEMOVE {
            HookAction::Queue
        } else {
            HookAction::Pass
        };
    }
    let Some(at) = hidden_at else {
        return HookAction::Pass;
    };
    if message == WM_MOUSEMOVE {
        if point.x == at.x && point.y == at.y {
            HookAction::Pass
        } else if ours {
            // A peer driving this PC again shows it through its
            // `remote_control_started`, not through its motion.
            HookAction::Follow
        } else {
            HookAction::Show
        }
    } else {
        HookAction::Replay
    }
}

/// The `SendInput` flags and data that play a low-level mouse message again;
/// `None` for motion.
fn replay_input(message: u32, mouse_data: u32) -> Option<(u32, u32)> {
    // The wheel distance and the X button are in the high word.
    let high = mouse_data >> 16;
    let wheel = high as u16 as i16 as i32 as u32;
    Some(match message {
        WM_LBUTTONDOWN => (MOUSEEVENTF_LEFTDOWN, 0),
        WM_LBUTTONUP => (MOUSEEVENTF_LEFTUP, 0),
        WM_RBUTTONDOWN => (MOUSEEVENTF_RIGHTDOWN, 0),
        WM_RBUTTONUP => (MOUSEEVENTF_RIGHTUP, 0),
        WM_MBUTTONDOWN => (MOUSEEVENTF_MIDDLEDOWN, 0),
        WM_MBUTTONUP => (MOUSEEVENTF_MIDDLEUP, 0),
        WM_XBUTTONDOWN => (MOUSEEVENTF_XDOWN, high),
        WM_XBUTTONUP => (MOUSEEVENTF_XUP, high),
        WM_MOUSEWHEEL => (MOUSEEVENTF_WHEEL, wheel),
        WM_MOUSEHWHEEL => (MOUSEEVENTF_HWHEEL, wheel),
        _ => return None,
    })
}

impl Hider {
    pub fn start(settings: Arc<SettingsStore>) -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("rinflow-hider".to_owned())
            .spawn(move || {
                let window = match create_window() {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                STATE.with_borrow_mut(|state| state.window = window);
                // SAFETY: plain thread id query; the queue exists (the window
                // was created on this thread).
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                // SAFETY: standard message loop of this thread's window.
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        match (message.hwnd.is_null(), message.message) {
                            (true, WM_HIDE) => {
                                if settings.get().hide_cursor_on_leave {
                                    hide();
                                }
                            }
                            (true, WM_REPLAY) => replay(),
                            (true, WM_REVEAL) => {
                                replay();
                                show();
                            }
                            _ => {
                                TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                    }
                    replay();
                    show();
                    DestroyWindow(window);
                }
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => {
                THREAD.store(thread_id, Ordering::SeqCst);
                Ok(Self {
                    thread_id,
                    join: Some(join),
                    controllers: Arc::default(),
                })
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("hider thread exited during start-up")),
        }
    }

    pub fn handle(&self) -> HiderHandle {
        HiderHandle {
            controllers: self.controllers.clone(),
        }
    }
}

impl Drop for Hider {
    fn drop(&mut self) {
        THREAD.store(0, Ordering::SeqCst);
        // SAFETY: asks the hider thread's message loop to finish.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// The hider thread's state; the hook procedure runs on this thread too.
struct State {
    window: HWND,
    hook: HHOOK,
    /// Where the cursor is hidden; `None` while it shows.
    hidden_at: Option<POINT>,
    /// Buttons and wheel held back by the hook, to play again in order
    /// (`SendInput` flags and data).
    replay: Vec<(u32, u32)>,
}

impl State {
    /// The window is gone, but held-back events still wait for the replay.
    fn replaying(&self) -> bool {
        self.hidden_at.is_none() && !self.hook.is_null()
    }

    /// Takes the window away and gives back the cursor display count.
    fn uncover(&mut self) {
        if self.hidden_at.take().is_some() {
            // SAFETY: this thread's window and display count; hiding the
            // window lets the cursor take the shape of what is under it.
            unsafe {
                ShowWindow(self.window, SW_HIDE);
                ShowCursor(1);
            }
        }
    }

    fn unhook(&mut self) {
        if !self.hook.is_null() {
            // SAFETY: the hook this thread installed.
            unsafe {
                UnhookWindowsHookEx(self.hook);
            }
            self.hook = null_mut();
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = const {
        RefCell::new(State {
            window: null_mut(),
            hook: null_mut(),
            hidden_at: None,
            replay: Vec::new(),
        })
    };
}

fn hide() {
    STATE.with_borrow_mut(|state| {
        // Not over a UAC prompt or the lock screen: no window of this
        // desktop is there to hide it, and the user needs the cursor.
        if state.hidden_at.is_some() || state.replaying() || !crate::win::input_desktop_is_default()
        {
            return;
        }
        let mut at = POINT { x: 0, y: 0 };
        // SAFETY: writes the cursor position into `at`; the window and the
        // hook belong to this thread, which runs a message loop.
        unsafe {
            if GetCursorPos(&mut at) == 0 {
                return;
            }
            SetWindowPos(
                state.window,
                HWND_TOPMOST,
                at.x,
                at.y,
                1,
                1,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            ShowCursor(0);
            state.hook =
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), GetModuleHandleW(null()), 0);
        }
        state.hidden_at = Some(at);
    });
}

fn show() {
    STATE.with_borrow_mut(|state| {
        state.uncover();
        state.unhook();
    });
}

/// Plays the events the hook held back, after a zero-pixel move that makes
/// Windows pick the window under the cursor again (this window is gone),
/// then removes the hook. The hook only runs while this thread waits for
/// messages, so nothing slips in between.
fn replay() {
    let events = STATE.with_borrow_mut(|state| std::mem::take(&mut state.replay));
    if !events.is_empty() {
        let inputs: Vec<INPUT> = std::iter::once((MOUSEEVENTF_MOVE, 0))
            .chain(events)
            .map(|(flags, data)| INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: data,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_MARKER,
                    },
                },
            })
            .collect();
        // SAFETY: fully initialised INPUTs with their exact size.
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
    }
    STATE.with_borrow_mut(|state| {
        if state.replaying() {
            state.unhook();
        }
    });
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_MOUSE_LL, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let message = wparam as u32;
        let action = STATE.with_borrow(|state| {
            hook_action(
                state.hidden_at,
                state.replaying(),
                message,
                info.pt,
                info.dwExtraInfo == INJECTED_MARKER,
            )
        });
        match (action, replay_input(message, info.mouseData)) {
            (HookAction::Pass, _) => {}
            (HookAction::Follow, _) => STATE.with_borrow_mut(|state| {
                // SAFETY: moves this thread's own window under the point the
                // move goes to, before Windows routes it.
                unsafe {
                    SetWindowPos(
                        state.window,
                        HWND_TOPMOST,
                        info.pt.x,
                        info.pt.y,
                        1,
                        1,
                        SWP_NOACTIVATE,
                    );
                }
                state.hidden_at = Some(info.pt);
            }),
            (HookAction::Replay, Some(input)) => {
                STATE.with_borrow_mut(|state| {
                    state.replay.push(input);
                    state.uncover();
                });
                // SAFETY: posts to this (the hider) thread's own queue.
                unsafe {
                    PostThreadMessageW(GetCurrentThreadId(), WM_REPLAY, 0, 0);
                }
                return 1;
            }
            (HookAction::Queue, Some(input)) => {
                STATE.with_borrow_mut(|state| state.replay.push(input));
                return 1;
            }
            // Motion, or something that cannot be played again: shown, and
            // the event goes on.
            _ => show(),
        }
    }
    // SAFETY: passes the event on to the next hook.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn create_window() -> io::Result<HWND> {
    let class_name = wide("RinflowHiddenPointer");
    // SAFETY: registers the class and creates a hidden window on this thread.
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
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Alpha 1 of 255: invisible, but still under the cursor for
        // hit-testing (alpha 0 would let the cursor see through it).
        SetLayeredWindowAttributes(window, 0, 1, LWA_ALPHA);
        Ok(window)
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_SETCURSOR {
        // SAFETY: no cursor while it is over this window.
        unsafe {
            SetCursor(null_mut());
        }
        return 1;
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_cursor_hides_when_the_last_driver_moves_away() {
        let mut controllers = HashSet::new();
        let started = |peer: &str| json!({"event": "remote_control_started", "peer_id": peer});
        let ended = |peer: &str, reason: &str| json!({"event": "remote_control_ended", "peer_id": peer, "reason": reason});
        assert_eq!(
            control_change(&mut controllers, &started("mac")),
            Change::Reveal
        );
        assert_eq!(
            control_change(&mut controllers, &ended("mac", "remote reset")),
            Change::Hide
        );
        // Lost or taken over: left as it is.
        control_change(&mut controllers, &started("mac"));
        assert_eq!(
            control_change(&mut controllers, &ended("mac", "disconnected")),
            Change::None
        );
        // Another peer still drives this PC.
        control_change(&mut controllers, &started("mac"));
        control_change(&mut controllers, &started("pc"));
        assert_eq!(
            control_change(&mut controllers, &ended("mac", "remote reset")),
            Change::None
        );
        assert_eq!(
            control_change(&mut controllers, &ended("pc", "remote reset")),
            Change::Hide
        );
        assert_eq!(
            control_change(&mut controllers, &json!({"event": "connected"})),
            Change::None
        );
    }

    #[test]
    fn motion_shows_the_cursor_and_buttons_are_played_again() {
        let at = POINT { x: 700, y: 0 };
        let off = POINT { x: 701, y: 0 };
        let hidden = Some(at);
        // The keep-awake nudge: a move of zero pixels.
        assert_eq!(
            hook_action(hidden, false, WM_MOUSEMOVE, at, true),
            HookAction::Pass
        );
        assert_eq!(
            hook_action(hidden, false, WM_MOUSEMOVE, off, false),
            HookAction::Show
        );
        // The last moves of the peer that left, played late: followed.
        assert_eq!(
            hook_action(hidden, false, WM_MOUSEMOVE, off, true),
            HookAction::Follow
        );
        assert_eq!(
            hook_action(hidden, false, WM_LBUTTONDOWN, at, false),
            HookAction::Replay
        );
        assert_eq!(
            hook_action(hidden, false, WM_MOUSEWHEEL, at, false),
            HookAction::Replay
        );
        // A tap's release right behind the press waits its turn; the replay
        // and motion go through.
        assert_eq!(
            hook_action(None, true, WM_LBUTTONUP, at, false),
            HookAction::Queue
        );
        assert_eq!(
            hook_action(None, true, WM_LBUTTONDOWN, at, true),
            HookAction::Pass
        );
        assert_eq!(
            hook_action(None, true, WM_MOUSEMOVE, off, false),
            HookAction::Pass
        );
        // Shown: nothing to do.
        assert_eq!(
            hook_action(None, false, WM_LBUTTONDOWN, at, false),
            HookAction::Pass
        );
    }

    #[test]
    fn held_back_events_play_as_they_came() {
        assert_eq!(
            replay_input(WM_LBUTTONDOWN, 0),
            Some((MOUSEEVENTF_LEFTDOWN, 0))
        );
        assert_eq!(
            replay_input(WM_RBUTTONUP, 0),
            Some((MOUSEEVENTF_RIGHTUP, 0))
        );
        // XBUTTON2 forward, in the high word.
        assert_eq!(
            replay_input(WM_XBUTTONDOWN, 2 << 16),
            Some((MOUSEEVENTF_XDOWN, 2))
        );
        // One notch towards the user: -120.
        assert_eq!(
            replay_input(WM_MOUSEWHEEL, (-120i16 as u16 as u32) << 16),
            Some((MOUSEEVENTF_WHEEL, -120i32 as u32))
        );
        assert_eq!(replay_input(WM_MOUSEMOVE, 0), None);
    }
}

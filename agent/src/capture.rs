//! Driving a peer: while the pointer is on a placed peer, this PC's keyboard
//! and mouse go to it.
//!
//! Runs on the edge thread ([`crate::edge`]). Low-level hooks are installed
//! only while a peer is driven. They swallow local input and forward it
//! through the core. The Windows cursor stays parked, hidden, in the middle
//! of the monitor it left from, so mouse motion is measured from there with
//! Windows' own pointer speed and acceleration, and a [`RemotePointer`]
//! follows the pointer on the peer. Moving it back out through a portal, the
//! release key (Ctrl+Alt+Esc) or losing the session ends the capture.

use crate::arrangement::Target;
use crate::displays::{self, PhysicalPortal};
use crate::inject::INJECTED_MARKER;
use crate::keymap;
use crate::win::wide;
use linkcursor_core::layout::RemotePointer;
use linkcursor_core::{
    Core, LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_FLAG_KEY_REPEAT, LC_INPUT_KEY_DOWN,
    LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE,
    LC_INPUT_SCROLL, LC_MODIFIER_COMMAND, LC_MODIFIER_CONTROL, LC_MODIFIER_LEFT_COMMAND,
    LC_MODIFIER_LEFT_CONTROL, LC_MODIFIER_LEFT_OPTION, LC_MODIFIER_LEFT_SHIFT, LC_MODIFIER_OPTION,
    LC_MODIFIER_RIGHT_COMMAND, LC_MODIFIER_RIGHT_CONTROL, LC_MODIFIER_RIGHT_OPTION,
    LC_MODIFIER_RIGHT_SHIFT, LC_MODIFIER_SHIFT, LcInputEvent,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Weak};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, LWA_ALPHA, MSLLHOOKSTRUCT, PostMessageW,
    RegisterClassW, SW_SHOWNOACTIVATE, SetCursor, SetCursorPos, SetLayeredWindowAttributes,
    SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_XBUTTONDOWN, WM_XBUTTONUP,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// Posted to the edge thread's window when a hook decided to end the
/// capture; hooks are not removed from inside their own callbacks.
pub const WM_END_CAPTURE: u32 = WM_APP + 2;

/// A notched wheel step (120 units) is one line, 10 points on the wire.
const POINTS_PER_WHEEL_UNIT: f64 = 10.0 / 120.0;
const USAGE_ESCAPE: u32 = 0x29;
const USAGE_CONTROL: [u32; 2] = [0xE0, 0xE4];
const USAGE_ALT: [u32; 2] = [0xE2, 0xE6];

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

struct Capture {
    core: Arc<Core>,
    peer_id: String,
    pointer: RemotePointer,
    /// Where the hidden cursor waits, in physical pixels.
    park: POINT,
    /// Physical pixels per point on the parking monitor.
    scale: f64,
    /// Where the pointer left this PC, in logical points.
    left_at: (f64, f64),
    /// Buttons and keys (HID usages) the peer has been told are down.
    buttons: u32,
    keys: HashSet<u32>,
    /// Keys (virtual keys) and buttons already held on this PC when the
    /// pointer left: their release belongs to this PC, not to the peer.
    local_keys: HashSet<u32>,
    local_buttons: u32,
    mouse_hook: HHOOK,
    keyboard_hook: HHOOK,
    cursor_window: HWND,
    /// The edge thread's window, for `WM_END_CAPTURE`.
    notify: HWND,
    /// Set once the capture is ending.
    ending: Option<Exit>,
}

/// Where the cursor goes when a capture ends.
#[derive(Clone, Copy)]
enum Exit {
    /// Back on this PC, at this local point (logical points).
    Return(f64, f64),
    /// Nowhere: a peer now drives this PC and places the cursor itself.
    Stay,
}

/// How a capture ended: the peer it drove, and where the cursor reappeared
/// if it came back to this PC.
pub struct Ended {
    pub peer_id: String,
    pub cursor: Option<POINT>,
}

pub fn active() -> bool {
    CAPTURE.with(|capture| capture.try_borrow().is_ok_and(|capture| capture.is_some()))
}

/// Crosses through `portal` at the physical cursor position and starts
/// driving its peer. Returns false (and changes nothing) when that is not
/// possible, for example because the core is shutting down.
pub fn begin(
    core: &Weak<Core>,
    target: Target,
    portal: &PhysicalPortal,
    cursor: POINT,
    notify: HWND,
) -> bool {
    if active() {
        return false;
    }
    let Some(core) = core.upgrade() else {
        return false;
    };
    let monitors = displays::monitors();
    let Some((x, y)) = displays::to_logical(&monitors, cursor.x, cursor.y) else {
        return false;
    };
    // Exactly on the portal's edge; the cursor sits on its last pixel row.
    let edge = portal.portal;
    let left_at = if edge.side.is_vertical() {
        (edge.line, y)
    } else {
        (x, edge.line)
    };
    let pointer = RemotePointer::enter(
        &target.peer,
        &target.portals,
        target.offset,
        edge,
        left_at.0,
        left_at.1,
    );
    let (arrival_x, arrival_y) = pointer.position();
    if core.set_input_target(Some(&portal.peer_id)).is_err() {
        return false;
    }
    // Where the pointer appears on the peer; the peer starts its cursor there.
    let _ = core.send_input(LcInputEvent {
        kind: LC_INPUT_MOUSE_MOVE,
        flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
        x: arrival_x,
        y: arrival_y,
        ..LcInputEvent::default()
    });

    let monitor = monitors
        .iter()
        .find(|monitor| {
            let p = &monitor.physical;
            cursor.x >= p.left && cursor.x < p.right && cursor.y >= p.top && cursor.y < p.bottom
        })
        .or(monitors.first());
    let (park, scale) = monitor.map_or((cursor, 1.0), |monitor| {
        let p = &monitor.physical;
        (
            POINT {
                x: (p.left + p.right) / 2,
                y: (p.top + p.bottom) / 2,
            },
            monitor.logical.scale,
        )
    });

    let (local_keys, local_buttons) = held_on_this_pc();
    let cursor_window = create_cursor_window(park);
    // SAFETY: moves the cursor onto the window that hides it, then installs
    // the hooks on this thread, which runs a message loop.
    let (mouse_hook, keyboard_hook) = unsafe {
        SetCursorPos(park.x, park.y);
        let module = GetModuleHandleW(null());
        (
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0),
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0),
        )
    };
    let capture = Capture {
        core,
        peer_id: portal.peer_id.clone(),
        pointer,
        park,
        scale,
        left_at,
        buttons: 0,
        keys: HashSet::new(),
        local_keys,
        local_buttons,
        mouse_hook,
        keyboard_hook,
        cursor_window,
        notify,
        ending: None,
    };
    let hooked = !mouse_hook.is_null() && !keyboard_hook.is_null();
    eprintln!(
        "linkcursor-agent: driving {} through its {:?} portal from ({:.1}, {:.1}), arriving at ({arrival_x:.1}, {arrival_y:.1})",
        portal.peer_id, edge.side, left_at.0, left_at.1
    );
    CAPTURE.with(|slot| *slot.borrow_mut() = Some(capture));
    if !hooked {
        eprintln!("linkcursor-agent: cannot install input hooks");
        // Undoes everything above and gives the pointer back.
        finish();
        return false;
    }
    true
}

/// Ends the capture when its peer is no longer connected. Called from the
/// edge thread's timer.
pub fn check(connected: impl Fn(&str) -> bool) {
    with_capture(|capture| {
        if !connected(&capture.peer_id) {
            capture.request_end(capture.home());
        }
    });
}

/// Gives control back without crossing (release, shutdown).
pub fn release() {
    with_capture(|capture| capture.request_end(capture.home()));
}

/// The core took this PC's control of `peer_id` away (`control_revoked`).
/// With `stay`, a peer now drives this PC and places the cursor itself;
/// otherwise the cursor comes back as after a release.
pub fn revoked(peer_id: &str, stay: bool) {
    with_capture(|capture| {
        if capture.peer_id == peer_id {
            let exit = if stay { Exit::Stay } else { capture.home() };
            capture.request_end(exit);
        }
    });
}

/// Removes the hooks, shows the cursor again where the pointer came back and
/// tells the peer to release everything. Runs on the edge thread, outside
/// the hooks.
pub fn finish() -> Option<Ended> {
    let capture = CAPTURE.with(|slot| slot.borrow_mut().take())?;
    // SAFETY: removes this thread's hooks and its cursor window.
    unsafe {
        if !capture.mouse_hook.is_null() {
            UnhookWindowsHookEx(capture.mouse_hook);
        }
        if !capture.keyboard_hook.is_null() {
            UnhookWindowsHookEx(capture.keyboard_hook);
        }
        DestroyWindow(capture.cursor_window);
    }
    let cursor = match capture.ending.unwrap_or_else(|| capture.home()) {
        Exit::Return(x, y) => {
            let cursor = displays::to_physical(&displays::monitors(), x, y)
                .map_or(capture.park, |(x, y)| POINT { x, y });
            // SAFETY: moves the cursor to a point on the desktop.
            unsafe {
                SetCursorPos(cursor.x, cursor.y);
            }
            eprintln!(
                "linkcursor-agent: back from {} at ({x:.1}, {y:.1})",
                capture.peer_id
            );
            Some(cursor)
        }
        Exit::Stay => {
            eprintln!(
                "linkcursor-agent: control of {} taken over; the new controller places the cursor",
                capture.peer_id
            );
            None
        }
    };
    // The peer releases whatever it still holds and ends its control. After
    // a revoke the core has done so already and this only acknowledges it.
    let _ = capture.core.reset_input();
    Some(Ended {
        peer_id: capture.peer_id,
        cursor,
    })
}

impl Capture {
    /// Back on this PC, just inside the edge the pointer left through.
    fn home(&self) -> Exit {
        let (x, y) = self.pointer.return_point(self.left_at.0, self.left_at.1);
        Exit::Return(x, y)
    }

    fn request_end(&mut self, exit: Exit) {
        if self.ending.is_some() {
            return;
        }
        self.ending = Some(exit);
        // SAFETY: posts to the edge thread's own window.
        unsafe {
            PostMessageW(self.notify, WM_END_CAPTURE, 0, 0);
        }
    }

    fn send(&self, event: LcInputEvent) {
        let _ = self.core.send_input(event);
    }

    /// Handles one mouse event; true swallows it.
    fn mouse(&mut self, message: u32, info: &MSLLHOOKSTRUCT) -> bool {
        if self.ending.is_some() {
            return true;
        }
        let wheel = f64::from((info.mouseData >> 16) as u16 as i16);
        match message {
            WM_MOUSEMOVE => {
                // `pt` is where this move would take the cursor. Measured
                // from where the cursor is (normally the parking spot), so a
                // program moving the cursor meanwhile does not count as motion.
                let mut current = POINT { x: 0, y: 0 };
                // SAFETY: reads the cursor position; puts it back on the
                // parking spot if something moved it.
                unsafe {
                    GetCursorPos(&mut current);
                    if current.x != self.park.x || current.y != self.park.y {
                        SetCursorPos(self.park.x, self.park.y);
                    }
                }
                let dx = f64::from(info.pt.x - current.x) / self.scale;
                let dy = f64::from(info.pt.y - current.y) / self.scale;
                if dx == 0.0 && dy == 0.0 {
                    return true;
                }
                if let Some((x, y)) = self.pointer.move_by(dx, dy) {
                    self.request_end(Exit::Return(x, y));
                    return true;
                }
                self.send(LcInputEvent {
                    kind: LC_INPUT_MOUSE_MOVE,
                    delta_x: dx,
                    delta_y: dy,
                    buttons: self.buttons,
                    ..LcInputEvent::default()
                });
            }
            WM_LBUTTONDOWN => return self.button(1, true),
            WM_LBUTTONUP => return self.button(1, false),
            WM_RBUTTONDOWN => return self.button(2, true),
            WM_RBUTTONUP => return self.button(2, false),
            WM_MBUTTONDOWN => return self.button(3, true),
            WM_MBUTTONUP => return self.button(3, false),
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                // XBUTTON1 is back (4), XBUTTON2 forward (5).
                let code = if (info.mouseData >> 16) & 0xFFFF == 2 {
                    5
                } else {
                    4
                };
                return self.button(code, message == WM_XBUTTONDOWN);
            }
            // Wire: positive y scrolls toward the top, positive x toward the
            // left. Windows: positive WHEEL is up, positive HWHEEL right.
            WM_MOUSEWHEEL => self.scroll(0.0, wheel * POINTS_PER_WHEEL_UNIT),
            WM_MOUSEHWHEEL => self.scroll(-wheel * POINTS_PER_WHEEL_UNIT, 0.0),
            _ => return false,
        }
        true
    }

    fn button(&mut self, code: u32, down: bool) -> bool {
        let bit = 1 << (code - 1);
        if !down && self.local_buttons & bit != 0 {
            // Pressed before the pointer left (a drag across the edge).
            self.local_buttons &= !bit;
            return false;
        }
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
        self.send(LcInputEvent {
            kind: if down {
                LC_INPUT_MOUSE_BUTTON_DOWN
            } else {
                LC_INPUT_MOUSE_BUTTON_UP
            },
            code,
            buttons: self.buttons,
            modifiers: self.modifiers(),
            ..LcInputEvent::default()
        });
        true
    }

    fn scroll(&self, x: f64, y: f64) {
        self.send(LcInputEvent {
            kind: LC_INPUT_SCROLL,
            scroll_x: x,
            scroll_y: y,
            buttons: self.buttons,
            modifiers: self.modifiers(),
            ..LcInputEvent::default()
        });
    }

    /// Handles one key event; true swallows it.
    fn key(&mut self, info: &KBDLLHOOKSTRUCT) -> bool {
        if self.ending.is_some() {
            return true;
        }
        let down = info.flags & LLKHF_UP == 0;
        if self.local_keys.contains(&info.vkCode) {
            // Held before the pointer left: its release goes to this PC, and
            // its auto-repeat goes nowhere.
            if !down {
                self.local_keys.remove(&info.vkCode);
                return false;
            }
            return true;
        }
        let extended = info.flags & LLKHF_EXTENDED != 0;
        let Some(usage) = keymap::hid_usage(info.scanCode, extended, info.vkCode) else {
            return true;
        };
        if down && usage == USAGE_ESCAPE && self.held(&USAGE_CONTROL) && self.held(&USAGE_ALT) {
            self.request_end(self.home());
            return true;
        }
        // Low-level hooks report auto-repeat as further key-downs.
        let repeat = if down {
            !self.keys.insert(usage)
        } else {
            self.keys.remove(&usage);
            false
        };
        self.send(LcInputEvent {
            kind: if down {
                LC_INPUT_KEY_DOWN
            } else {
                LC_INPUT_KEY_UP
            },
            code: usage,
            flags: if repeat { LC_INPUT_FLAG_KEY_REPEAT } else { 0 },
            repeat_count: u32::from(repeat),
            modifiers: self.modifiers(),
            buttons: self.buttons,
            ..LcInputEvent::default()
        });
        true
    }

    fn held(&self, usages: &[u32]) -> bool {
        usages.iter().any(|usage| self.keys.contains(usage))
    }

    /// The `LC_MODIFIER_*` bits for the keys the peer has been told are down.
    fn modifiers(&self) -> u32 {
        let mut bits = 0;
        for &(usage, side, general) in &[
            (0xE0, LC_MODIFIER_LEFT_CONTROL, LC_MODIFIER_CONTROL),
            (0xE4, LC_MODIFIER_RIGHT_CONTROL, LC_MODIFIER_CONTROL),
            (0xE1, LC_MODIFIER_LEFT_SHIFT, LC_MODIFIER_SHIFT),
            (0xE5, LC_MODIFIER_RIGHT_SHIFT, LC_MODIFIER_SHIFT),
            (0xE2, LC_MODIFIER_LEFT_OPTION, LC_MODIFIER_OPTION),
            (0xE6, LC_MODIFIER_RIGHT_OPTION, LC_MODIFIER_OPTION),
            (0xE3, LC_MODIFIER_LEFT_COMMAND, LC_MODIFIER_COMMAND),
            (0xE7, LC_MODIFIER_RIGHT_COMMAND, LC_MODIFIER_COMMAND),
        ] {
            if self.keys.contains(&usage) {
                bits |= side | general;
            }
        }
        bits
    }
}

/// Keys and mouse buttons physically held on this PC right now.
fn held_on_this_pc() -> (HashSet<u32>, u32) {
    let mut keys = HashSet::new();
    let mut buttons = 0;
    for key in 1..=0xFE {
        // SAFETY: plain key state query.
        let down = unsafe { GetAsyncKeyState(key) } as u16 & 0x8000 != 0;
        if !down {
            continue;
        }
        match key {
            0x01 => buttons |= 1,
            0x02 => buttons |= 2,
            0x04 => buttons |= 4,
            0x05 => buttons |= 8,
            0x06 => buttons |= 16,
            // The generic Shift/Control/Alt keys repeat their left/right ones.
            0x10..=0x12 => {}
            _ => {
                keys.insert(key as u32);
            }
        }
    }
    (keys, buttons)
}

fn with_capture<R>(action: impl FnOnce(&mut Capture) -> R) -> Option<R> {
    CAPTURE.with(|capture| {
        let mut capture = capture.try_borrow_mut().ok()?;
        capture.as_mut().map(action)
    })
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_MOUSE_LL, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if info.dwExtraInfo != INJECTED_MARKER
            && with_capture(|capture| capture.mouse(wparam as u32, info)) == Some(true)
        {
            return 1;
        }
    }
    // SAFETY: passes the event on to the next hook.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_KEYBOARD_LL, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if info.dwExtraInfo != INJECTED_MARKER
            && with_capture(|capture| capture.key(info)) == Some(true)
        {
            return 1;
        }
    }
    // SAFETY: passes the event on to the next hook.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// A tiny, nearly transparent topmost window under the parked cursor whose
/// cursor is none: Windows shows the cursor of the window under it, so this
/// hides it without touching the system cursors.
fn create_cursor_window(park: POINT) -> HWND {
    let class_name = wide("LinkCursorHiddenCursor");
    // SAFETY: registers the class (again is harmless) and creates the window
    // on this thread.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(cursor_window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        let size = 64;
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_POPUP,
            park.x - size / 2,
            park.y - size / 2,
            size,
            size,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if !window.is_null() {
            // Alpha 1 of 255: invisible, but still under the cursor for
            // hit-testing (alpha 0 would let the cursor see through it).
            SetLayeredWindowAttributes(window, 0, 1, LWA_ALPHA);
            ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        window
    }
}

unsafe extern "system" fn cursor_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_SETCURSOR {
        // SAFETY: hides the cursor while it is over this window.
        unsafe {
            SetCursor(null_mut());
        }
        return 1;
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

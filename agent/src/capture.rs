// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driving a peer: while the pointer is on a placed peer, this PC's keyboard
//! and mouse go to it.
//!
//! Runs on the edge thread ([`crate::edge`]). Low-level hooks are installed
//! only while a peer is driven. They swallow local input and forward it
//! through the core. The Windows cursor stays parked, hidden, in the middle
//! of the monitor it left from, so mouse motion is measured from there with
//! Windows' own pointer speed and acceleration, and a [`RemotePointer`]
//! follows the pointer on the peer. Moving it back out through a portal, the
//! release key (Ctrl+Alt+Esc) or losing the session ends the capture. While
//! the lock shortcut holds the pointer on the peer, portals lead nowhere.
//!
//! - Where the peer touches another placed peer, the pointer moves straight
//!   on into that one (three devices in a row), and the switch shortcuts
//!   (Ctrl+Alt+Shift+arrow keys by default) jump
//!   to the device on that side (see `arrangement::World`).
//! - Motion and scrolling are scaled per peer (the `peer_input` setting) here,
//!   where the pointer is tracked, so the two never disagree on where it is.
//! - While a game or a pointer-lock page on the peer holds its pointer (the
//!   core's `peer_pointer_lock`), all motion goes to it and edges lead
//!   nowhere; the shortcuts still work.
//!
//! The keyboard and mouse come back to this PC, with a notification, when
//! the hooks can no longer take them: Windows keeps an elevated window's
//! input from them (watched with a WinEvent hook and the timer), and
//! silently removes a hook that took too long to answer. Raw input, which
//! keeps arriving either way, shows the second: input a hook should have
//! seen but did not (see `silent_hook`).

use crate::arrangement::{Arrangement, Target};
use crate::displays::{self, PhysicalPortal};
use crate::edge;
use crate::inject::INJECTED_MARKER;
use crate::keymap;
use crate::settings::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, PeerInput, SettingsStore};
use crate::state::SharedState;
use crate::win::wide;
use rinflow_core::layout::{Motion, RemotePointer, Side};
use rinflow_core::{
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
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Input::{
    RAWINPUTDEVICE, RIDEV_INPUTSINK, RIDEV_REMOVE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, EVENT_SYSTEM_FOREGROUND,
    GetCursorPos, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_UP, LWA_ALPHA,
    MSLLHOOKSTRUCT, PostMessageW, RegisterClassW, SW_SHOWNOACTIVATE, SetCursor, SetCursorPos,
    SetLayeredWindowAttributes, SetWindowsHookExW, ShowCursor, ShowWindow, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
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
const USAGE_SHIFT: [u32; 2] = [0xE1, 0xE5];
const USAGE_ALT: [u32; 2] = [0xE2, 0xE6];
const USAGE_GUI: [u32; 2] = [0xE3, 0xE7];
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;

/// The side an arrow key points to.
pub fn arrow_side(vk: u32) -> Option<Side> {
    match vk {
        VK_LEFT => Some(Side::Left),
        VK_RIGHT => Some(Side::Right),
        VK_UP => Some(Side::Top),
        VK_DOWN => Some(Side::Bottom),
        _ => None,
    }
}

/// The arrow key for a side.
pub fn side_key(side: Side) -> u32 {
    match side {
        Side::Left => VK_LEFT,
        Side::Right => VK_RIGHT,
        Side::Top => VK_UP,
        Side::Bottom => VK_DOWN,
    }
}

/// What a capture needs to move on to other devices.
#[derive(Clone)]
pub struct Context {
    pub arrangement: Arc<Arrangement>,
    pub state: SharedState,
    pub settings: Arc<SettingsStore>,
}

/// How long keys still held when a capture ended are kept from this PC, at
/// most (their release may never come, e.g. after the secure desktop).
const DRAIN_LIMIT: Duration = Duration::from_secs(3);
/// Raw input a hook has not seen within this long means the hook is gone
/// (Windows waits at most a second for a hook before it removes it).
const HOOK_SILENCE: Duration = Duration::from_millis(500);
/// A hook that ran this shortly before raw input reported the same event
/// saw it already: the two arrive in either order.
const HOOK_ORDER_SLACK: Duration = Duration::from_millis(50);

/// Input devices, for telling whether the hooks still see what they get.
#[derive(Clone, Copy, Debug)]
pub enum Device {
    Mouse,
    Keyboard,
}

/// Whether a hook still sees the input of its device: raw input reports the
/// same input, before or after the hook runs, and keeps reporting it once
/// Windows removed the hook.
#[derive(Clone, Copy, Debug)]
struct HookWatch {
    /// When the hook last ran.
    ran_at: Instant,
    /// Since when raw input has reported input the hook has not seen.
    unseen: Option<Instant>,
}

impl HookWatch {
    fn new(now: Instant) -> Self {
        Self {
            ran_at: now,
            unseen: None,
        }
    }

    fn hook_ran(&mut self, now: Instant) {
        (self.ran_at, self.unseen) = (now, None);
    }

    fn raw_input(&mut self, now: Instant) {
        // Just after the hook ran, this is what it saw.
        if self.unseen.is_none() && now.saturating_duration_since(self.ran_at) > HOOK_ORDER_SLACK {
            self.unseen = Some(now);
        }
    }

    fn silent(&self, now: Instant) -> bool {
        self.unseen
            .is_some_and(|since| now.saturating_duration_since(since) > HOOK_SILENCE)
    }
}

/// Why the keyboard and mouse came back to this PC by themselves.
enum GiveBack {
    /// An elevated window came to the front: the hooks cannot take its input.
    Elevated,
    /// Windows removed a hook.
    HookLost(Device),
}

impl GiveBack {
    fn tell(&self) {
        let title = crate::i18n::tr(
            "键盘和鼠标已回到这台电脑",
            "Keyboard and mouse are back on this PC",
        );
        let text = match self {
            GiveBack::Elevated => {
                log!("an elevated window came to the front; capture ended");
                crate::i18n::tr(
                    "以管理员身份运行的窗口到了前台，Rinflow 无法接管它的键盘输入。先点一下其他窗口，再移到另一台设备。",
                    "A window running as administrator came to the front, so Rinflow can't take its keyboard input. Click another window first, then move to the other device.",
                )
            }
            GiveBack::HookLost(device) => {
                log!("the {device:?} hook stopped seeing input; capture ended");
                crate::i18n::tr(
                    "Windows 暂停了 Rinflow 对键盘和鼠标的接管（系统繁忙时会这样）。再移到另一台设备即可继续。",
                    "Windows stopped Rinflow from taking the keyboard and mouse, as it does when the system is busy. Move to the other device again to carry on.",
                )
            }
        };
        crate::tray::notify(title, text);
    }
}

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
    /// Keys (HID usages) the peer held when the capture ended: their
    /// auto-repeats and releases still come and must not reach this PC (a
    /// held Win key would open Start). The keyboard hook stays for them.
    static DRAIN: RefCell<Option<Drain>> = const { RefCell::new(None) };
}

struct Drain {
    hook: HHOOK,
    keys: HashSet<u32>,
    until: std::time::Instant,
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
    /// Tells the moment an elevated window comes to the front.
    foreground_hook: HWINEVENTHOOK,
    /// Whether the hooks still see the input (see `silent_hook`).
    mouse_watch: HookWatch,
    keyboard_watch: HookWatch,
    cursor_window: HWND,
    /// The edge thread's window, for `WM_END_CAPTURE`.
    notify: HWND,
    /// Set once the capture is ending.
    ending: Option<Exit>,
    /// The lock shortcut's key is down (its auto-repeat toggles nothing).
    hotkey_down: bool,
    /// The arrow key of a switch shortcut that is down (likewise).
    switch_key_down: Option<u32>,
    /// Keys (HID usages) the previous peer saw pressed when the pointer
    /// moved on: their repeats and releases belong to nobody.
    stale_keys: HashSet<u32>,
    /// How motion and scrolling feel on this peer.
    input: PeerInput,
    context: Context,
}

/// Where the cursor goes when a capture ends.
#[derive(Clone, Copy)]
enum Exit {
    /// Back on this PC, at this local point (logical points).
    Return(f64, f64),
    /// Back on this PC by shortcut, at this local point.
    Jump(f64, f64),
    /// Nowhere: a peer now drives this PC and places the cursor itself.
    Stay,
}

/// How a capture ended: the peer it drove, and where the cursor reappeared
/// if it came back to this PC (through a portal unless it jumped by
/// shortcut).
pub struct Ended {
    pub peer_id: String,
    pub cursor: Option<POINT>,
    pub jumped: bool,
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
    context: Context,
) -> bool {
    if active() || !can_take_keyboard() {
        return false;
    }
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
    )
    .with_neighbors(target.neighbors);
    log!(
        "driving {} through its {:?} portal from ({:.1}, {:.1})",
        portal.peer_id,
        edge.side,
        left_at.0,
        left_at.1
    );
    start(
        core,
        &portal.peer_id,
        pointer,
        left_at,
        cursor,
        notify,
        context,
    )
}

/// Jumps to `peer_id` by shortcut, the pointer arriving at the local point
/// `(x, y)` (inside one of its displays). Giving control back without
/// crossing returns the cursor to where it is now.
pub fn begin_at(
    core: &Weak<Core>,
    peer_id: &str,
    target: Target,
    (x, y): (f64, f64),
    notify: HWND,
    context: Context,
) -> bool {
    if active() || !can_take_keyboard() {
        return false;
    }
    let mut cursor = POINT { x: 0, y: 0 };
    // SAFETY: writes the cursor position into `cursor`.
    unsafe { GetCursorPos(&mut cursor) };
    let Some(left_at) = displays::to_logical(&displays::monitors(), cursor.x, cursor.y) else {
        return false;
    };
    let pointer = RemotePointer::enter_at(&target.peer, &target.portals, target.offset, x, y, None)
        .with_neighbors(target.neighbors);
    log!("driving {peer_id} by shortcut");
    start(core, peer_id, pointer, left_at, cursor, notify, context)
}

/// Windows keeps keyboard input meant for an elevated window from this
/// process's hook: typing "on the peer" would go into that window here (an
/// administrator's terminal, say). Not across, then, and say why.
fn can_take_keyboard() -> bool {
    if crate::win::foreground_is_elevated() {
        crate::tray::notify(
            crate::i18n::tr("键盘和鼠标没有切换", "Keyboard and mouse didn't switch"),
            crate::i18n::tr(
                "当前窗口以管理员身份运行，Rinflow 无法接管它的键盘输入。先点一下其他窗口，再移到另一台设备。",
                "The window in front runs as administrator, so Rinflow can't take its keyboard input. Click another window first, then move to the other device.",
            ),
        );
        return false;
    }
    true
}

/// Starts driving `peer_id` with `pointer`: the peer's cursor is placed,
/// this PC's is parked and hidden, and the hooks take the keyboard and mouse.
fn start(
    core: &Weak<Core>,
    peer_id: &str,
    pointer: RemotePointer,
    left_at: (f64, f64),
    cursor: POINT,
    notify: HWND,
    context: Context,
) -> bool {
    let Some(core) = core.upgrade() else {
        return false;
    };
    let monitors = displays::monitors();
    let (arrival_x, arrival_y) = pointer.position();
    if core.set_input_target(Some(peer_id)).is_err() {
        return false;
    }
    // Whether a game holds its pointer, the peer says afresh.
    edge::clear_pointer_held(peer_id);
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

    // Keys still draining from an earlier capture are held on this PC now,
    // which the snapshot below records.
    end_drain();
    // The cursor a peer left hidden here is this capture's to hide now.
    crate::hider::reveal();
    let cursor_window = create_cursor_window(park);
    // SAFETY: moves the cursor onto the window that hides it, then installs
    // the hooks on this thread, which runs a message loop.
    let (mouse_hook, keyboard_hook) = unsafe {
        // Below zero, this thread's display count keeps the cursor hidden
        // over its window even if another program sets a cursor meanwhile
        // (as Deskflow does); undone when the capture ends.
        ShowCursor(0);
        SetCursorPos(park.x, park.y);
        let module = GetModuleHandleW(null());
        (
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0),
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0),
        )
    };
    // An elevated window coming to the front ends the capture at once,
    // before more keys reach it; the timer checks too, in case this fails.
    // SAFETY: an out-of-context hook, called on this thread's message loop.
    let foreground_hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(foreground_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    // Raw keyboard input while capturing, to tell whether the keyboard hook
    // still sees the keys (the mouse's raw input is always on).
    if !watch_raw_keyboard(Some(notify)) {
        log!("cannot watch raw keyboard input; a removed keyboard hook goes unnoticed");
    }
    // After the hooks: whatever is pressed from now on reaches them, so
    // nothing falls between this snapshot and the capture.
    let (local_keys, local_buttons) = held_on_this_pc();
    let input = context.settings.get().input_for(peer_id);
    let now = Instant::now();
    let capture = Capture {
        core,
        peer_id: peer_id.to_owned(),
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
        foreground_hook,
        mouse_watch: HookWatch::new(now),
        keyboard_watch: HookWatch::new(now),
        cursor_window,
        notify,
        ending: None,
        hotkey_down: false,
        switch_key_down: None,
        stale_keys: HashSet::new(),
        input,
        context,
    };
    let hooked = !mouse_hook.is_null() && !keyboard_hook.is_null();
    log!("arriving at ({arrival_x:.1}, {arrival_y:.1}) on {peer_id}");
    CAPTURE.with(|slot| *slot.borrow_mut() = Some(capture));
    if !hooked {
        log!("cannot install input hooks");
        // Undoes everything above and gives the pointer back.
        finish();
        return false;
    }
    true
}

/// Ends the capture when its peer is no longer connected, or when another
/// desktop takes input (the lock screen after Win+L, a UAC prompt, the
/// Ctrl+Alt+Del screen): this PC's keyboard is needed here then, and the
/// hooks would never see the releases of keys held on the peer. Also when
/// the hooks cannot take the input any more: an elevated window is in front,
/// or Windows removed a hook. Called from the edge thread's timer.
pub fn check(connected: impl Fn(&str) -> bool) {
    if active() {
        let elsewhere = !crate::win::input_desktop_is_default();
        // On another desktop no window of this one is in front.
        let elevated = !elsewhere && crate::win::foreground_is_elevated();
        let given_back = with_capture(|capture| {
            if capture.ending.is_some() {
                return None;
            }
            let reason = if elsewhere || !connected(&capture.peer_id) {
                None
            } else if elevated {
                Some(GiveBack::Elevated)
            } else {
                // Nothing to end otherwise.
                Some(GiveBack::HookLost(capture.silent_hook()?))
            };
            capture.request_end(capture.home());
            reason
        })
        .flatten();
        if let Some(reason) = given_back {
            reason.tell();
        }
    }
    // Drained keys whose release never came are let go.
    let done = DRAIN.with(|drain| {
        drain
            .borrow()
            .as_ref()
            .is_some_and(|drain| drain.keys.is_empty() || std::time::Instant::now() >= drain.until)
    });
    if done {
        end_drain();
    }
}

/// Removes the hook kept for draining keys.
fn end_drain() {
    if let Some(drain) = DRAIN.with(|drain| drain.borrow_mut().take()) {
        // SAFETY: the hook this thread installed for the capture.
        unsafe { UnhookWindowsHookEx(drain.hook) };
    }
}

/// Gives control back without crossing (release, shutdown).
pub fn release() {
    with_capture(|capture| capture.request_end(capture.home()));
}

/// Raw input reported real input of `device` (the edge thread's window).
pub fn raw_input(device: Device) {
    with_capture(|capture| capture.raw_input(device));
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
    watch_raw_keyboard(None);
    // SAFETY: removes this thread's hooks and its cursor window.
    unsafe {
        if !capture.foreground_hook.is_null() {
            UnhookWinEvent(capture.foreground_hook);
        }
        if !capture.mouse_hook.is_null() {
            UnhookWindowsHookEx(capture.mouse_hook);
        }
        if !capture.keyboard_hook.is_null() {
            if capture.keys.is_empty() {
                UnhookWindowsHookEx(capture.keyboard_hook);
            } else {
                // Keys the peer holds are still down here too: their repeats
                // and releases are swallowed until they come up.
                let drain = Drain {
                    hook: capture.keyboard_hook,
                    keys: capture.keys.clone(),
                    until: std::time::Instant::now() + DRAIN_LIMIT,
                };
                DRAIN.with(|slot| *slot.borrow_mut() = Some(drain));
            }
        }
        // The display count lowered when the capture started.
        ShowCursor(1);
        DestroyWindow(capture.cursor_window);
    }
    let exit = capture.ending.unwrap_or_else(|| capture.home());
    let cursor = match exit {
        Exit::Return(x, y) | Exit::Jump(x, y) => {
            let cursor = displays::to_physical(&displays::monitors(), x, y)
                .map_or(capture.park, |(x, y)| POINT { x, y });
            // SAFETY: moves the cursor to a point on the desktop.
            unsafe {
                SetCursorPos(cursor.x, cursor.y);
            }
            log!("back from {} at ({x:.1}, {y:.1})", capture.peer_id);
            Some(cursor)
        }
        Exit::Stay => {
            log!(
                "control of {} taken over; the new controller places the cursor",
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
        jumped: matches!(exit, Exit::Jump(..)),
    })
}

/// A game or a pointer-lock page on `peer_id` let go of its pointer: the
/// peer's cursor is put back where this PC tracks it, so the edges are
/// where they seem again.
pub fn pointer_released(peer_id: &str) {
    with_capture(|capture| {
        if capture.peer_id == peer_id && capture.ending.is_none() {
            let (x, y) = capture.pointer.position();
            capture.send(LcInputEvent {
                kind: LC_INPUT_MOUSE_MOVE,
                flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
                x,
                y,
                ..LcInputEvent::default()
            });
        }
    });
}

/// The switch shortcut toward `side`, pressed while a peer is driven.
pub fn jump(side: Side) {
    with_capture(|capture| capture.jump(side));
}

impl Capture {
    /// Moves on to the placed peer `peer_id` (into a neighbour, or by
    /// shortcut), the pointer arriving at the local point `(x, y)`. False
    /// when that peer is not connected: then the pointer stays.
    fn move_on(&mut self, peer_id: &str, x: f64, y: f64) -> bool {
        if !self.context.state.is_connected(peer_id) {
            return false;
        }
        let Some(target) = self.context.arrangement.target(peer_id) else {
            return false;
        };
        let pointer = RemotePointer::enter_at(
            &target.peer,
            &target.portals,
            target.offset,
            x,
            y,
            self.pointer.entry(),
        )
        .with_neighbors(target.neighbors);
        // The old peer gets a reset: it releases what it holds.
        if self.core.set_input_target(Some(peer_id)).is_err() {
            return false;
        }
        let (arrival_x, arrival_y) = pointer.position();
        self.send(LcInputEvent {
            kind: LC_INPUT_MOUSE_MOVE,
            flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
            x: arrival_x,
            y: arrival_y,
            ..LcInputEvent::default()
        });
        // Modifiers held go on to the new peer (Shift through a drag across);
        // other keys were for the old one, so their releases go nowhere.
        let held: Vec<u32> = self.keys.drain().collect();
        self.buttons = 0;
        self.peer_id = peer_id.to_owned();
        self.pointer = pointer;
        self.input = self.context.settings.get().input_for(peer_id);
        edge::clear_pointer_held(peer_id);
        for usage in held {
            if (0xE0..=0xE7).contains(&usage) {
                self.keys.insert(usage);
                self.send(LcInputEvent {
                    kind: LC_INPUT_KEY_DOWN,
                    code: usage,
                    modifiers: self.modifiers(),
                    ..LcInputEvent::default()
                });
            } else {
                self.stale_keys.insert(usage);
            }
        }
        log!("moved on to {peer_id}, arriving at ({arrival_x:.1}, {arrival_y:.1})");
        true
    }

    /// A switch shortcut: on to the device on that side of this peer.
    fn jump(&mut self, side: Side) {
        if self.ending.is_some() || edge::locked() {
            return;
        }
        let Some(world) = self.context.arrangement.world() else {
            return;
        };
        let Some(to) = world.toward(Some(&self.peer_id), side) else {
            return;
        };
        let Some((x, y)) = world.landing(Some(&self.peer_id), to.as_deref()) else {
            return;
        };
        match to {
            None => self.request_end(Exit::Jump(x, y)),
            Some(peer_id) => {
                let _ = self.move_on(&peer_id, x, y);
            }
        }
    }
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

    fn watch(&mut self, device: Device) -> &mut HookWatch {
        match device {
            Device::Mouse => &mut self.mouse_watch,
            Device::Keyboard => &mut self.keyboard_watch,
        }
    }

    /// A hook ran: it sees what its device does.
    fn hook_ran(&mut self, device: Device) {
        self.watch(device).hook_ran(Instant::now());
    }

    /// Raw input reported real input of `device`.
    fn raw_input(&mut self, device: Device) {
        self.watch(device).raw_input(Instant::now());
    }

    /// A device whose input its hook has not seen for a while: Windows
    /// removed the hook, and that input now reaches this PC.
    fn silent_hook(&self) -> Option<Device> {
        let now = Instant::now();
        if self.mouse_watch.silent(now) {
            Some(Device::Mouse)
        } else if self.keyboard_watch.silent(now) {
            Some(Device::Keyboard)
        } else {
            None
        }
    }

    /// Handles one mouse event; true swallows it.
    fn mouse(&mut self, message: u32, info: &MSLLHOOKSTRUCT) -> bool {
        self.hook_ran(Device::Mouse);
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
                // The pointer speed set for this peer.
                let speed = self.input.pointer_speed / self.scale;
                let dx = f64::from(info.pt.x - current.x) * speed;
                let dy = f64::from(info.pt.y - current.y) * speed;
                if dx == 0.0 && dy == 0.0 {
                    return true;
                }
                // A game there holds its pointer: every bit of motion goes
                // to it (it turns the camera), and no edge leads anywhere.
                if edge::pointer_held(&self.peer_id) {
                    self.send(LcInputEvent {
                        kind: LC_INPUT_MOUSE_MOVE,
                        delta_x: dx,
                        delta_y: dy,
                        buttons: self.buttons,
                        ..LcInputEvent::default()
                    });
                    return true;
                }
                let before = self.pointer.position();
                let (mut dx, mut dy) = (dx, dy);
                match self.pointer.step(dx, dy) {
                    Motion::Stay => {}
                    Motion::Home(x, y) if !edge::locked() => {
                        self.request_end(Exit::Return(x, y));
                        return true;
                    }
                    Motion::Neighbor { id, x, y } if !edge::locked() && self.move_on(&id, x, y) => {
                        return true;
                    }
                    // A neighbour not connected (or locked): an edge.
                    Motion::Neighbor { .. } => {}
                    Motion::Home(..) => {
                        // Locked to the peer: slide along the edge instead.
                        let _ = self.pointer.move_by(dx, 0.0);
                        let _ = self.pointer.move_by(0.0, dy);
                        let after = self.pointer.position();
                        (dx, dy) = (after.0 - before.0, after.1 - before.1);
                        if dx == 0.0 && dy == 0.0 {
                            return true;
                        }
                    }
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
            // Pressed again: its earlier release went unseen, and this press
            // belongs to the peer.
            self.local_buttons &= !bit;
        } else if self.buttons & bit == 0 {
            // Pressed on a peer the pointer moved on from, which was reset:
            // this peer never saw it go down.
            return true;
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
        // The scrolling speed and direction set for this peer.
        let factor = self.input.scroll_speed * if self.input.invert_scroll { -1.0 } else { 1.0 };
        self.send(LcInputEvent {
            kind: LC_INPUT_SCROLL,
            scroll_x: x * factor,
            scroll_y: y * factor,
            buttons: self.buttons,
            modifiers: self.modifiers(),
            ..LcInputEvent::default()
        });
    }

    /// Handles one key event; true swallows it.
    fn key(&mut self, info: &KBDLLHOOKSTRUCT) -> bool {
        self.hook_ran(Device::Keyboard);
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
        if self.stale_keys.contains(&usage) {
            // Pressed on the peer the pointer moved on from.
            if !down {
                self.stale_keys.remove(&usage);
            }
            return true;
        }
        if down && usage == USAGE_ESCAPE && self.held(&USAGE_CONTROL) && self.held(&USAGE_ALT) {
            self.request_end(self.home());
            return true;
        }
        // The switch shortcuts (Ctrl+Alt+Shift+arrow keys by default): on to
        // the device on that side. The peer saw the modifiers, never the
        // arrow.
        if Some(info.vkCode) == self.switch_key_down {
            if !down {
                self.switch_key_down = None;
            }
            return true;
        }
        if down
            && let Some(side) = arrow_side(info.vkCode)
            && self.is_switch_shortcut()
        {
            self.switch_key_down = Some(info.vkCode);
            self.jump(side);
            return true;
        }
        // The lock shortcut toggles the lock here too; the peer never sees
        // the key (it did see the modifiers).
        let hotkey = edge::lock_hotkey();
        if info.vkCode == hotkey.key && (self.hotkey_down || down) {
            if !down {
                self.hotkey_down = false;
                return true;
            }
            if self.hotkey_down {
                // Its auto-repeat: toggled once already.
                return true;
            }
            if self.modifier_bits() == hotkey.modifiers {
                self.hotkey_down = true;
                // SAFETY: posts to the edge thread's own window.
                unsafe {
                    PostMessageW(self.notify, edge::WM_TOGGLE_LOCK, 0, 0);
                }
                return true;
            }
        }
        // Low-level hooks report auto-repeat as further key-downs.
        let repeat = if down {
            !self.keys.insert(usage)
        } else {
            // The peer never got this key down (the lock shortcut's key).
            if !self.keys.remove(&usage) {
                return true;
            }
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

    /// Exactly the switch shortcuts' modifiers are held (with an arrow key
    /// going down), and the shortcuts are on.
    fn is_switch_shortcut(&self) -> bool {
        let settings = self.context.settings.get();
        settings.switch_hotkeys
            && self.modifier_bits() | self.local_modifier_bits() == settings.switch_modifiers
    }

    /// The modifier bits of keys held since before the capture began (the
    /// modifiers of the switch shortcut that started it): held on, with
    /// another arrow key, they jump on.
    fn local_modifier_bits(&self) -> u32 {
        let mut bits = 0;
        for (keys, bit) in [
            ([0xA2, 0xA3], MOD_CONTROL),
            ([0xA0, 0xA1], MOD_SHIFT),
            ([0xA4, 0xA5], MOD_ALT),
            ([0x5B, 0x5C], MOD_WIN),
        ] {
            if keys.iter().any(|key| self.local_keys.contains(key)) {
                bits |= bit;
            }
        }
        bits
    }

    /// The `RegisterHotKey` modifier bits for the keys held down.
    fn modifier_bits(&self) -> u32 {
        let mut bits = 0;
        for (usages, bit) in [
            (USAGE_CONTROL, MOD_CONTROL),
            (USAGE_SHIFT, MOD_SHIFT),
            (USAGE_ALT, MOD_ALT),
            (USAGE_GUI, MOD_WIN),
        ] {
            if self.held(&usages) {
                bits |= bit;
            }
        }
        bits
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
        if info.dwExtraInfo != INJECTED_MARKER {
            match with_capture(|capture| capture.key(info)) {
                Some(true) => return 1,
                Some(false) => {}
                // No capture: this hook is only draining keys.
                None if drained(info) => return 1,
                None => {}
            }
        }
    }
    // SAFETY: passes the event on to the next hook.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// The foreground window changed. An elevated one ends the capture before
/// more keys go to it: Windows keeps them from the keyboard hook.
unsafe extern "system" fn foreground_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if !active() || !crate::win::foreground_is_elevated() {
        return;
    }
    let given_back = with_capture(|capture| {
        let first = capture.ending.is_none();
        capture.request_end(capture.home());
        first
    });
    if given_back == Some(true) {
        GiveBack::Elevated.tell();
    }
}

/// Registers raw keyboard input for `window` (in the background too), or
/// with `None` removes it. False if Windows refused.
fn watch_raw_keyboard(window: Option<HWND>) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop
        usUsage: 0x06,     // keyboard
        dwFlags: if window.is_some() {
            RIDEV_INPUTSINK
        } else {
            RIDEV_REMOVE
        },
        hwndTarget: window.unwrap_or(null_mut()),
    };
    // SAFETY: one valid RAWINPUTDEVICE of the size given.
    unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) != 0 }
}

/// Swallows a key event of a key still held from the capture; its release
/// ends that.
fn drained(info: &KBDLLHOOKSTRUCT) -> bool {
    let extended = info.flags & LLKHF_EXTENDED != 0;
    let Some(usage) = keymap::hid_usage(info.scanCode, extended, info.vkCode) else {
        return false;
    };
    DRAIN.with(|drain| {
        let mut drain = drain.borrow_mut();
        let Some(drain) = drain.as_mut() else {
            return false;
        };
        if !drain.keys.contains(&usage) {
            return false;
        }
        if info.flags & LLKHF_UP != 0 {
            drain.keys.remove(&usage);
        }
        true
    })
}

/// A tiny, nearly transparent topmost window under the parked cursor whose
/// cursor is none: Windows shows the cursor of the window under it, so this
/// hides it without touching the system cursors.
fn create_cursor_window(park: POINT) -> HWND {
    let class_name = wide("RinflowHiddenCursor");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn a_hook_that_sees_the_input_in_either_order_is_alive() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        // Raw input first, the hook a moment later.
        watch.raw_input(start + ms(1000));
        watch.hook_ran(start + ms(1003));
        assert!(!watch.silent(start + ms(2000)));
        // The hook first, raw input a moment later.
        watch.hook_ran(start + ms(3000));
        watch.raw_input(start + ms(3002));
        assert!(!watch.silent(start + ms(4000)));
        // A slow hook still counts, as long as it comes.
        watch.raw_input(start + ms(5000));
        assert!(!watch.silent(start + ms(5400)));
        watch.hook_ran(start + ms(5450));
        assert!(!watch.silent(start + ms(6000)));
    }

    #[test]
    fn input_the_hook_never_sees_means_it_is_gone() {
        let start = Instant::now();
        let mut watch = HookWatch::new(start);
        // Windows removed the hook; input keeps coming, the hook never runs.
        watch.raw_input(start + ms(2000));
        watch.raw_input(start + ms(2200));
        assert!(!watch.silent(start + ms(2400)));
        // Counted from the first input it missed, not the latest.
        watch.raw_input(start + ms(2450));
        assert!(watch.silent(start + ms(2600)));
    }
}

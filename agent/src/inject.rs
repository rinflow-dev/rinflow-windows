//! Plays input from a controlling peer with `SendInput`.
//!
//! Runs on its own thread so the core's network threads never wait on the
//! window manager. Received events follow the wire conventions documented on
//! `LcInputEvent` in linkcursor-core: deltas and scroll amounts in logical
//! points, keys as USB HID usages already translated for this platform's
//! shortcut convention.
//!
//! - Pointer motion is played as an absolute move on the virtual desktop
//!   (current position + delta × display scale). Relative injection would be
//!   accelerated a second time by "Enhance pointer precision".
//! - Keys are played by scan code (see `keymap.rs`).
//! - While a peer is in control the PC stays awake, and on a PC without a
//!   mouse the MouseKeys accessibility feature is enabled so Windows draws
//!   the cursor at all.
//! - When the LocalSystem input helper is in use (see `elevated.rs`), every
//!   event goes to it instead, so it also reaches elevated windows, UAC
//!   prompts and the lock screen. Motion then goes as pixels: on those
//!   desktops this process cannot even see where the pointer is.
//! - A game or a pointer-lock page that hides the cursor and confines it
//!   (`ClipCursor`) reads raw relative motion: motion is then played as
//!   relative moves, and the controlling peer is told (`set_pointer_locked`)
//!   so it stops crossing at the edges meanwhile.

use crate::displays;
use crate::elevated;
use crate::keymap::{WinKey, windows_key};
use linkcursor_core::Core;
use linkcursor_core::{
    LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_FLAG_PRECISE_SCROLL, LC_INPUT_KEY_DOWN,
    LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE,
    LC_INPUT_SCROLL, LcInputEvent,
};
use linkcursor_input::VirtualDesktop;
use std::collections::HashSet;
use std::io;
use std::sync::{Weak, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows_sys::Win32::UI::Accessibility::MOUSEKEYS;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC_EX, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetClipCursor, GetCursorInfo, GetCursorPos, GetSystemMetrics,
    MKF_AVAILABLE, MKF_MOUSEKEYSON, SM_MOUSEPRESENT, SPI_GETMOUSEKEYS, SPI_SETMOUSEKEYS,
    SPIF_SENDCHANGE, SystemParametersInfoW, XBUTTON1, XBUTTON2,
};

/// Stored in `dwExtraInfo` of every injected event ("LCIN"), so the agent's
/// own low-level hooks can tell played input from the user's hardware.
pub use linkcursor_input::INJECTED_MARKER;

/// Windows apps such as Chrome and Edge scroll 100 DIPs per wheel notch of
/// 120 units, so this keeps trackpad distances the same as on the sender.
const WHEEL_UNITS_PER_POINT: f64 = 1.2;
/// A notched wheel reports 10 points per line; one Windows line is 40 units
/// (a 120-unit notch scrolls three lines by default).
const WHEEL_UNITS_PER_LINE_POINT: f64 = 4.0;

enum Job {
    Input(LcInputEvent),
    /// A peer started (`true`) or stopped driving this PC.
    Control {
        peer_id: String,
        active: bool,
    },
    /// The core, once it runs: told when a game holds the pointer.
    Core(Weak<Core>),
    Quit,
}

/// How often the pointer lock is checked while a peer drives this PC and no
/// motion comes (a game may let go meanwhile).
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Cloneable sender used by the core callbacks.
#[derive(Clone)]
pub struct InjectorHandle(mpsc::Sender<Job>);

impl InjectorHandle {
    pub fn play(&self, event: LcInputEvent) {
        let _ = self.0.send(Job::Input(event));
    }

    /// The core, once it runs.
    pub fn set_core(&self, core: Weak<Core>) {
        let _ = self.0.send(Job::Core(core));
    }

    /// Picks the remote-control start and end out of the core's event stream.
    pub fn core_event(&self, json: &str) {
        if !json.contains("remote_control_") {
            return;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let active = match event["event"].as_str() {
            Some("remote_control_started") => true,
            Some("remote_control_ended") => false,
            _ => return,
        };
        let peer_id = event["peer_id"].as_str().unwrap_or_default().to_owned();
        let _ = self.0.send(Job::Control { peer_id, active });
    }
}

pub struct Injector {
    handle: InjectorHandle,
    join: Option<JoinHandle<()>>,
}

impl Injector {
    pub fn spawn() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("linkcursor-inject".to_owned())
            .spawn(move || {
                let mut player = Player::default();
                // Jobs arrive in order, so the key-ups the core sends when a
                // session ends are always played before Quit.
                loop {
                    let job = match receiver.recv_timeout(LOCK_POLL) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if player.controlled {
                                player.check_pointer_lock();
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match job {
                        Job::Input(event) => player.play(&event),
                        Job::Control { peer_id, active } => {
                            if active {
                                player.controllers.insert(peer_id);
                            } else {
                                player.controllers.remove(&peer_id);
                            }
                            player.set_controlled(!player.controllers.is_empty());
                        }
                        Job::Core(core) => player.core = Some(core),
                        Job::Quit => break,
                    }
                }
                player.set_controlled(false);
            })?;
        Ok(Self {
            handle: InjectorHandle(sender),
            join: Some(join),
        })
    }

    pub fn handle(&self) -> InjectorHandle {
        self.handle.clone()
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let _ = self.handle.0.send(Job::Quit);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Default)]
struct Player {
    /// Peers currently driving this PC; usually one, but sessions with
    /// several peers can overlap.
    controllers: HashSet<String>,
    controlled: bool,
    /// Sub-pixel motion carried to the next event, in pixels.
    motion_rest: (f64, f64),
    /// Sub-unit wheel movement carried to the next event, in wheel units.
    wheel_rest: (f64, f64),
    /// Where the pointer was last seen, for its monitor's scale while a UAC
    /// prompt or the lock screen hides it from this process.
    last_cursor: Option<POINT>,
    /// Where the last move put the pointer, and what `GetCursorPos` said
    /// before it. Windows applies injected moves a moment later; reading the
    /// old position again then means the move is still on its way.
    last_move: Option<(POINT, POINT)>,
    /// A game or a pointer-lock page holds the pointer (as last reported).
    pointer_locked: bool,
    core: Option<Weak<Core>>,
}

impl Player {
    fn play(&mut self, event: &LcInputEvent) {
        match event.kind {
            // Where the pointer enters this PC: a point in its logical space.
            LC_INPUT_MOUSE_MOVE if event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION != 0 => {
                self.move_to(event.x, event.y)
            }
            LC_INPUT_MOUSE_MOVE => self.move_by(event.delta_x, event.delta_y),
            LC_INPUT_MOUSE_BUTTON_DOWN => button(event.code, true),
            LC_INPUT_MOUSE_BUTTON_UP => button(event.code, false),
            LC_INPUT_SCROLL => self.scroll(event),
            LC_INPUT_KEY_DOWN => key(event, true),
            LC_INPUT_KEY_UP => key(event, false),
            _ => {}
        }
    }

    fn set_controlled(&mut self, controlled: bool) {
        if self.controlled == controlled {
            return;
        }
        self.controlled = controlled;
        self.motion_rest = (0.0, 0.0);
        self.wheel_rest = (0.0, 0.0);
        // The helper plays into UAC prompts and the lock screen only now.
        elevated::set_remote_control(controlled);
        let state = if controlled {
            // While a peer drives this PC, nobody touches its own mouse or
            // keyboard, so Windows would otherwise sleep or blank the screen.
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: changes this thread's execution state only.
        unsafe {
            SetThreadExecutionState(state);
        }
        if controlled {
            show_cursor_without_mouse();
            // A game may hold the pointer already: the new controller hears.
            self.pointer_locked = false;
            self.check_pointer_lock();
        }
    }

    /// Whether a game holds the pointer, told to the core when it changes.
    fn check_pointer_lock(&mut self) -> bool {
        let locked = pointer_captured();
        if locked != self.pointer_locked {
            self.pointer_locked = locked;
            log!(
                "the pointer is {} by the program in front",
                if locked { "held" } else { "no longer held" }
            );
            if let Some(core) = self.core.as_ref().and_then(Weak::upgrade) {
                let _ = core.set_pointer_locked(locked);
            }
        }
        locked
    }

    fn move_by(&mut self, dx: f64, dy: f64) {
        // A game reads raw relative motion: play it as such. The cursor is
        // hidden and confined, so where it is does not matter.
        if self.check_pointer_lock() {
            let scale = self.last_cursor.map_or(1.0, monitor_scale);
            let (step_x, rest_x) = scaled_step(dx, scale, self.motion_rest.0);
            let (step_y, rest_y) = scaled_step(dy, scale, self.motion_rest.1);
            self.motion_rest = (rest_x, rest_y);
            self.last_move = None;
            if step_x != 0 || step_y != 0 {
                send_mouse(MOUSEINPUT {
                    dx: step_x,
                    dy: step_y,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: INJECTED_MARKER,
                });
            }
            return;
        }
        let mut cursor = POINT { x: 0, y: 0 };
        // SAFETY: writes the cursor position into `cursor`. It fails while
        // another desktop (UAC prompt, lock screen) has input.
        let seen = unsafe { GetCursorPos(&mut cursor) } != 0;
        if seen {
            // Still where it was before the previous move: that move has not
            // landed yet, so go on from where it is going.
            if let Some((target, before)) = self.last_move
                && cursor.x == before.x
                && cursor.y == before.y
            {
                cursor = target;
            }
            self.last_cursor = Some(cursor);
        }
        let Some(at) = self.last_cursor else {
            return;
        };
        let scale = monitor_scale(at);
        let (step_x, rest_x) = scaled_step(dx, scale, self.motion_rest.0);
        let (step_y, rest_y) = scaled_step(dy, scale, self.motion_rest.1);
        self.motion_rest = (rest_x, rest_y);
        if step_x == 0 && step_y == 0 {
            return;
        }
        // The helper moves it from where the pointer really is.
        if elevated::move_by(step_x, step_y) || !seen {
            self.last_move = None;
            return;
        }
        let target = POINT {
            x: cursor.x + step_x,
            y: cursor.y + step_y,
        };
        let mut before = POINT { x: 0, y: 0 };
        // SAFETY: writes the cursor position into `before`.
        unsafe { GetCursorPos(&mut before) };
        place_cursor(target.x, target.y);
        // Clamped onto the desktop, as the move itself is.
        let desktop = VirtualDesktop::current();
        let landed = POINT {
            x: target.x.clamp(desktop.x, desktop.x + desktop.width - 1),
            y: target.y.clamp(desktop.y, desktop.y + desktop.height - 1),
        };
        self.last_move = (landed.x != before.x || landed.y != before.y).then_some((landed, before));
    }

    fn move_to(&mut self, x: f64, y: f64) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        self.motion_rest = (0.0, 0.0);
        self.last_move = None;
        if let Some((x, y)) = displays::to_physical(&displays::monitors(), x, y) {
            place_cursor(x, y);
        }
    }

    fn scroll(&mut self, event: &LcInputEvent) {
        let per_point = if event.flags & LC_INPUT_FLAG_PRECISE_SCROLL != 0 {
            WHEEL_UNITS_PER_POINT
        } else {
            WHEEL_UNITS_PER_LINE_POINT
        };
        // Wire: positive y scrolls up, positive x scrolls left. Windows:
        // positive WHEEL scrolls up, positive HWHEEL scrolls right.
        let (vertical, rest_y) = wheel_step(event.scroll_y, per_point, self.wheel_rest.1);
        let (horizontal, rest_x) = wheel_step(-event.scroll_x, per_point, self.wheel_rest.0);
        self.wheel_rest = (rest_x, rest_y);
        for (units, flag) in [
            (vertical, MOUSEEVENTF_WHEEL),
            (horizontal, MOUSEEVENTF_HWHEEL),
        ] {
            if units != 0 {
                send_mouse(MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: units as u32,
                    dwFlags: flag,
                    time: 0,
                    dwExtraInfo: INJECTED_MARKER,
                });
            }
        }
    }
}

/// A game or a pointer-lock page holds the pointer: the cursor is hidden and
/// confined to less than the desktop (`ClipCursor`). Either alone happens
/// in ordinary use (typing hides it; some apps confine it).
fn pointer_captured() -> bool {
    // SAFETY: plain queries into properly sized structures.
    unsafe {
        let mut info: CURSORINFO = std::mem::zeroed();
        info.cbSize = size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut info) == 0 || info.flags & CURSOR_SHOWING != 0 {
            return false;
        }
        let mut clip: RECT = std::mem::zeroed();
        if GetClipCursor(&mut clip) == 0 {
            return false;
        }
        confined(clip, &VirtualDesktop::current())
    }
}

/// The cursor's clip rectangle leaves out part of the virtual desktop.
fn confined(clip: RECT, desktop: &VirtualDesktop) -> bool {
    clip.left > desktop.x
        || clip.top > desktop.y
        || clip.right < desktop.x + desktop.width
        || clip.bottom < desktop.y + desktop.height
}

/// Whole pixels to move now and the fraction to carry over.
fn scaled_step(delta: f64, scale: f64, rest: f64) -> (i32, f64) {
    let total = delta * scale + rest;
    let whole = total.trunc();
    (whole as i32, total - whole)
}

/// Whole wheel units to send now and the fraction to carry over.
fn wheel_step(points: f64, units_per_point: f64, rest: f64) -> (i32, f64) {
    let total = points * units_per_point + rest;
    let whole = total.trunc();
    (whole as i32, total - whole)
}

fn monitor_scale(point: POINT) -> f64 {
    // SAFETY: plain monitor queries.
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) != 0 {
            return 1.0;
        }
        x as f64 / 96.0
    }
}

fn button(code: u32, down: bool) {
    let (flags, data) = match (code, down) {
        (1, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (1, false) => (MOUSEEVENTF_LEFTUP, 0),
        (2, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (2, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (3, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (3, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (4, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (5, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (5, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        _ => return,
    };
    send_mouse(MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: data as u32,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: INJECTED_MARKER,
    });
}

fn key(event: &LcInputEvent, down: bool) {
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    match windows_key(event.code) {
        Some(WinKey::Scan { code, extended }) => send_key(KEYBDINPUT {
            wVk: 0,
            wScan: code,
            dwFlags: KEYEVENTF_SCANCODE | up | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 },
            time: 0,
            dwExtraInfo: INJECTED_MARKER,
        }),
        Some(WinKey::Virtual(vk)) => {
            // SAFETY: plain keyboard-layout query.
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_EX) };
            let extended = scan >> 8 == 0xE0;
            send_key(KEYBDINPUT {
                wVk: vk,
                wScan: (scan & 0xFF) as u16,
                dwFlags: up | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 },
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            });
        }
        // A key this PC has no scan code for still types its character.
        None => {
            let Some(character) = char::from_u32(event.key_symbol).filter(|_| down) else {
                return;
            };
            let mut units = [0u16; 2];
            for &unit in character.encode_utf16(&mut units).iter() {
                for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                    send_key(KEYBDINPUT {
                        wVk: 0,
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_MARKER,
                    });
                }
            }
        }
    }
}

/// Puts the cursor on a physical pixel of the virtual desktop. Played as an
/// absolute move, so Windows applies no pointer acceleration on top.
fn place_cursor(x: i32, y: i32) {
    if let Some(input) = VirtualDesktop::current().move_to(x, y).to_input() {
        send(input);
    }
}

fn send_mouse(mi: MOUSEINPUT) {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    });
}

fn send_key(ki: KEYBDINPUT) {
    send(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    });
}

fn send(input: INPUT) {
    if elevated::forward(&input) {
        return;
    }
    // SAFETY: one fully initialised INPUT with its exact size.
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    // No retry: SendInput fails when another desktop (UAC, lock screen) has
    // input, and retrying would only spin. UIPI blocks silently anyway.
    if sent == 0 && cfg!(debug_assertions) {
        log!("SendInput failed: {}", io::Error::last_os_error());
    }
}

/// Windows hides the cursor on a PC with no mouse attached, even while input
/// moves it. Turning on MouseKeys makes it draw the cursor (the approach
/// Deskflow uses). It is left on afterwards: toggling it back can leave the
/// cursor stuck hidden. PCs with any pointing device are not touched.
fn show_cursor_without_mouse() {
    // SAFETY: plain system metric query and MOUSEKEYS get/set with its size.
    unsafe {
        if GetSystemMetrics(SM_MOUSEPRESENT) != 0 {
            return;
        }
        let mut keys = MOUSEKEYS {
            cbSize: size_of::<MOUSEKEYS>() as u32,
            ..std::mem::zeroed()
        };
        if SystemParametersInfoW(
            SPI_GETMOUSEKEYS,
            keys.cbSize,
            (&mut keys as *mut MOUSEKEYS).cast(),
            0,
        ) == 0
            || keys.dwFlags & MKF_MOUSEKEYSON != 0
        {
            return;
        }
        // Without MKF_REPLACENUMBERS: the numeric keypad keeps typing
        // digits instead of moving the pointer.
        keys.dwFlags |= MKF_AVAILABLE | MKF_MOUSEKEYSON;
        SystemParametersInfoW(
            SPI_SETMOUSEKEYS,
            keys.cbSize,
            (&mut keys as *mut MOUSEKEYS).cast(),
            SPIF_SENDCHANGE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WHEEL_UNITS_PER_LINE_POINT, WHEEL_UNITS_PER_POINT, confined, scaled_step, wheel_step,
    };
    use linkcursor_input::VirtualDesktop;
    use windows_sys::Win32::Foundation::RECT;

    #[test]
    fn a_clip_smaller_than_the_desktop_holds_the_pointer() {
        // Two monitors, the left one at negative coordinates.
        let desktop = VirtualDesktop {
            x: -1920,
            y: 0,
            width: 1920 + 2880,
            height: 1920,
        };
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        // Not confined: Windows reports the whole desktop.
        assert!(!confined(rect(-1920, 0, 2880, 1920), &desktop));
        // A game's window on the right monitor.
        assert!(confined(rect(0, 0, 2880, 1920), &desktop));
        // Chrome's pointer lock: a tiny rectangle.
        assert!(confined(rect(1440, 960, 1441, 961), &desktop));
    }

    #[test]
    fn motion_scales_and_keeps_fractions() {
        // 1.25 px per point at 125 %: four 1-point moves add up to 5 px.
        let mut rest = 0.0;
        let mut total = 0;
        for _ in 0..4 {
            let (step, carry) = scaled_step(1.0, 1.25, rest);
            total += step;
            rest = carry;
        }
        assert_eq!(total, 5);
        assert!(rest.abs() < 1e-9);
        assert_eq!(scaled_step(-3.0, 2.0, 0.0).0, -6);
    }

    #[test]
    fn wheel_units_follow_the_wire_conventions() {
        // One notched-wheel line (10 points) is one Windows line (40 units).
        assert_eq!(wheel_step(10.0, WHEEL_UNITS_PER_LINE_POINT, 0.0).0, 40);
        // Small trackpad deltas accumulate instead of being dropped.
        let mut rest = 0.0;
        let mut total = 0;
        for _ in 0..10 {
            let (units, carry) = wheel_step(0.5, WHEEL_UNITS_PER_POINT, rest);
            total += units;
            rest = carry;
        }
        // Whatever was not sent yet is carried, never lost.
        assert!((total as f64 + rest - 6.0).abs() < 1e-9, "{total} + {rest}");
    }
}

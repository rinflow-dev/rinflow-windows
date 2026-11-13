// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Plays input from a controlling peer with `SendInput`.
//!
//! Runs on its own thread so the core's network threads never wait on the
//! window manager. Received events follow the wire conventions documented on
//! `LcInputEvent` in rinflow-core: deltas and scroll amounts in logical
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
//!   desktops this process cannot even see where the pointer is. Without it
//!   (a build from the source tree, or its setting off) Windows refuses
//!   input meant for a UAC prompt or the lock screen; the agent says why
//!   once (log and notification) until input goes through again.
//! - A game or a pointer-lock page that hides the cursor and confines it
//!   (`ClipCursor`) reads raw relative motion: motion is then played as
//!   relative moves, and the controlling peer is told (`set_pointer_locked`)
//!   so it stops crossing at the edges meanwhile.
//! - A KVM placed right beside this PC is its attached screen (see
//!   rinflow-core's `docs/attached-screens.md`): the peer driving this PC
//!   sees it as one of its monitors. When the pointer goes onto it, this PC
//!   follows the pointer there itself (the real cursor stops at the edge),
//!   passes the input on to the KVM (`relay`) and shows its picture full
//!   screen; back across the edge, the real cursor carries on.

use crate::displays;
use crate::elevated;
use crate::i18n::{s, tr};
use crate::keymap::{WinKey, windows_key};
use rinflow_core::layout::Display;
use rinflow_core::{
    Core, LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_FLAG_PRECISE_SCROLL, LC_INPUT_KEY_DOWN,
    LC_INPUT_KEY_UP, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_BUTTON_UP, LC_INPUT_MOUSE_MOVE,
    LC_INPUT_SCROLL, LcInputEvent,
};
use rinflow_input::VirtualDesktop;
use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
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
pub use rinflow_input::INJECTED_MARKER;

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
    /// This PC's attached screens changed (`attached_screens`).
    Attached(Vec<AttachedScreen>),
    /// The core stopped relaying to an attached screen (`relay_ended`).
    RelayEnded {
        peer_id: String,
        reason: String,
    },
    Quit,
}

/// A KVM placed right beside this PC: its displays in this PC's logical
/// space, and where it is placed (subtracted for a point in its own space).
#[derive(Clone, Debug, PartialEq)]
struct AttachedScreen {
    peer_id: String,
    offset: (f64, f64),
    displays: Vec<Display>,
}

impl AttachedScreen {
    fn contains(&self, (x, y): (f64, f64)) -> bool {
        self.displays.iter().any(|display| contains(display, x, y))
    }

    fn from_json(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            peer_id: value["peer_id"].as_str()?.to_owned(),
            offset: (
                value["offset"]["x"].as_f64()?,
                value["offset"]["y"].as_f64()?,
            ),
            displays: serde_json::from_value(value["displays"].clone()).ok()?,
        })
    }
}

/// Following the pointer on an attached screen, for the peer driving this PC.
struct Relaying {
    peer_id: String,
    offset: (f64, f64),
    displays: Vec<Display>,
    /// The pointer, in this PC's logical space.
    pointer: (f64, f64),
    /// The core passes input on. Not after another device took the screen
    /// over: input is dropped until the pointer comes back.
    live: bool,
}

/// How often the pointer lock is checked while a peer drives this PC and no
/// motion comes (a game may let go meanwhile).
const LOCK_POLL: Duration = Duration::from_millis(250);

/// How long a played move may take to show in `GetCursorPos`. A pointer
/// still at its old place after that was stopped by something (an edge, a
/// clip rectangle), not late.
const LANDING_TIME: Duration = Duration::from_millis(100);

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

    /// Picks the remote-control start and end, the attached screens and the
    /// end of relaying out of the core's event stream.
    pub fn core_event(&self, json: &str) {
        if !json.contains("remote_control_")
            && !json.contains("\"attached_screens\"")
            && !json.contains("\"relay_ended\"")
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let peer_id = event["peer_id"].as_str().unwrap_or_default().to_owned();
        let job = match event["event"].as_str() {
            Some("remote_control_started") => Job::Control {
                peer_id,
                active: true,
            },
            Some("remote_control_ended") => Job::Control {
                peer_id,
                active: false,
            },
            Some("attached_screens") => Job::Attached(
                event["screens"]
                    .as_array()
                    .map(|screens| {
                        screens
                            .iter()
                            .filter_map(AttachedScreen::from_json)
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            Some("relay_ended") => Job::RelayEnded {
                peer_id,
                reason: event["reason"].as_str().unwrap_or_default().to_owned(),
            },
            _ => return,
        };
        let _ = self.0.send(job);
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
            .name("rinflow-inject".to_owned())
            .spawn(move || {
                rinflow_input::win::raise_input_thread();
                let mut player = Player::default();
                // Jobs arrive in order, so the key-ups the core sends when a
                // session ends are always played before Quit.
                // A job taken while looking for more motion to merge.
                let mut next: Option<Job> = None;
                loop {
                    let job = match next.take() {
                        Some(job) => job,
                        None => match receiver.recv_timeout(LOCK_POLL) {
                            Ok(job) => job,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if player.controlled {
                                    player.check_pointer_lock();
                                }
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        },
                    };
                    match job {
                        Job::Input(mut event) => {
                            // Motion that piled up (the network held it, or
                            // this PC was busy) is played as one move: one
                            // by one, each waits for the window manager,
                            // and the pointer falls further behind.
                            while next.is_none()
                                && let Ok(more) = receiver.try_recv()
                            {
                                match more {
                                    Job::Input(more) if merge_motion(&mut event, &more) => {}
                                    other => next = Some(other),
                                }
                            }
                            player.play(&event)
                        }
                        Job::Control { peer_id, active } => {
                            if active {
                                player.controllers.insert(peer_id);
                            } else {
                                player.controllers.remove(&peer_id);
                            }
                            player.set_controlled(!player.controllers.is_empty());
                        }
                        Job::Core(core) => player.core = Some(core),
                        Job::Attached(screens) => player.set_attached(screens),
                        Job::RelayEnded { peer_id, reason } => {
                            player.relay_ended(&peer_id, &reason)
                        }
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

/// Adds the relative motion `more` to the relative motion `event`; false
/// (and nothing changed) if either is anything else. An absolute move places
/// the pointer, and keys, buttons and scrolling keep their place in order.
fn merge_motion(event: &mut LcInputEvent, more: &LcInputEvent) -> bool {
    let relative = |event: &LcInputEvent| {
        event.kind == LC_INPUT_MOUSE_MOVE && event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION == 0
    };
    if !relative(event) || !relative(more) {
        return false;
    }
    event.delta_x += more.delta_x;
    event.delta_y += more.delta_y;
    event.buttons = more.buttons;
    event.modifiers = more.modifiers;
    true
}

/// How long the answer to "does a game hold the pointer" is good for while
/// motion is played: asking takes several calls into the window manager,
/// too many for every one of hundreds of moves a second.
const LOCK_CHECK_INTERVAL: Duration = Duration::from_millis(50);

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
    /// Where the last move put the pointer, what `GetCursorPos` said before
    /// it, and when. Windows applies injected moves a moment later; reading
    /// the old position again within [`LANDING_TIME`] means the move is
    /// still on its way.
    last_move: Option<(POINT, POINT, Instant)>,
    /// A game or a pointer-lock page holds the pointer (as last reported).
    pointer_locked: bool,
    /// When that was last looked up.
    lock_checked: Option<Instant>,
    core: Option<Weak<Core>>,
    /// KVMs placed right beside this PC (see the module comment).
    attached: Vec<AttachedScreen>,
    /// The pointer is on one of them.
    relaying: Option<Relaying>,
    /// This PC's monitors and when they were looked up: often enough to
    /// follow changes, not for every move.
    monitors: Option<(Vec<displays::Monitor>, Instant)>,
    /// Keys and buttons pressed here and not released yet. Released here
    /// even if the pointer has gone on to an attached screen meanwhile.
    held_keys: HashSet<u32>,
    held_buttons: HashSet<u32>,
}

/// How long a lookup of this PC's monitors is used for.
const MONITORS_FRESH: Duration = Duration::from_secs(1);

impl Player {
    fn play(&mut self, event: &LcInputEvent) {
        if self.relaying.is_some() {
            return self.play_relayed(event);
        }
        match event.kind {
            // Where the pointer enters this PC: a point in its logical space,
            // perhaps on an attached screen.
            LC_INPUT_MOUSE_MOVE if event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION != 0 => {
                let point = (event.x, event.y);
                match self.screen_at(point) {
                    Some(screen) if !self.on_this_pc(point) => self.start_relay(screen, point),
                    _ => self.move_to(event.x, event.y),
                }
            }
            LC_INPUT_MOUSE_MOVE => self.move_by(event.delta_x, event.delta_y),
            LC_INPUT_MOUSE_BUTTON_DOWN => {
                self.held_buttons.insert(event.code);
                button(event.code, true)
            }
            LC_INPUT_MOUSE_BUTTON_UP => {
                self.held_buttons.remove(&event.code);
                button(event.code, false)
            }
            LC_INPUT_SCROLL => self.scroll(event),
            LC_INPUT_KEY_DOWN => {
                self.held_keys.insert(event.code);
                key(event, true)
            }
            LC_INPUT_KEY_UP => {
                self.held_keys.remove(&event.code);
                key(event, false)
            }
            _ => {}
        }
    }

    fn core(&self) -> Option<std::sync::Arc<Core>> {
        self.core.as_ref().and_then(Weak::upgrade)
    }

    fn monitors(&mut self) -> Vec<displays::Monitor> {
        match &self.monitors {
            Some((monitors, at)) if at.elapsed() < MONITORS_FRESH => monitors.clone(),
            _ => {
                let monitors = displays::monitors();
                self.monitors = Some((monitors.clone(), Instant::now()));
                monitors
            }
        }
    }

    /// On one of this PC's own monitors (logical points).
    fn on_this_pc(&mut self, (x, y): (f64, f64)) -> bool {
        self.monitors()
            .iter()
            .any(|monitor| contains(&monitor.logical, x, y))
    }

    fn screen_at(&self, point: (f64, f64)) -> Option<AttachedScreen> {
        self.attached
            .iter()
            .find(|screen| screen.contains(point))
            .cloned()
    }

    fn set_attached(&mut self, screens: Vec<AttachedScreen>) {
        if let Some(relaying) = &mut self.relaying
            && let Some(screen) = screens.iter().find(|s| s.peer_id == relaying.peer_id)
        {
            // Moved or resized meanwhile. One that went away: the core says.
            relaying.offset = screen.offset;
            relaying.displays = screen.displays.clone();
        }
        self.attached = screens;
    }

    /// The pointer goes on to an attached screen at `pointer`. The real
    /// cursor waits at the nearest point of this PC, under the screen's
    /// picture, until the pointer comes back.
    fn start_relay(&mut self, screen: AttachedScreen, pointer: (f64, f64)) {
        let monitors = self.monitors();
        let Some((x, y)) = displays::to_physical(&monitors, pointer.0, pointer.1) else {
            return;
        };
        place_cursor(x, y);
        self.last_move = None;
        self.motion_rest = (0.0, 0.0);
        let live = self
            .core()
            .is_some_and(|core| core.set_relay_target(Some(&screen.peer_id)).is_ok());
        log!(
            "the pointer went on to the attached screen {}",
            screen.peer_id
        );
        let monitor = monitors.iter().find(|monitor| {
            let rect = monitor.physical;
            x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
        });
        if let (Some(viewers), Some(monitor)) = (crate::viewer::global(), monitor) {
            let rect = monitor.physical;
            viewers.driving(Some((
                &screen.peer_id,
                (rect.left, rect.top, rect.right, rect.bottom),
            )));
        }
        self.relaying = Some(Relaying {
            peer_id: screen.peer_id,
            offset: screen.offset,
            displays: screen.displays,
            pointer,
            live,
        });
        self.relay_pointer(0, 0);
    }

    /// Input while the pointer is on an attached screen: the pointer is
    /// followed here, everything else passed on, except the release of what
    /// was pressed on this PC.
    fn play_relayed(&mut self, event: &LcInputEvent) {
        let Some(relaying) = &self.relaying else {
            return;
        };
        match event.kind {
            LC_INPUT_MOUSE_MOVE => {
                let absolute = event.flags & LC_INPUT_FLAG_ABSOLUTE_POSITION != 0;
                let to = if absolute {
                    (event.x, event.y)
                } else {
                    (
                        relaying.pointer.0 + event.delta_x,
                        relaying.pointer.1 + event.delta_y,
                    )
                };
                let on_screen = relaying
                    .displays
                    .iter()
                    .any(|display| contains(display, to.0, to.1));
                // Against an outer edge of the screen, unless it is back
                // on this PC.
                let clamped = clamp_onto(&relaying.displays, relaying.pointer, to);
                let pointer = if on_screen {
                    to
                } else if absolute || self.on_this_pc(to) {
                    self.end_relay(true);
                    self.move_to(to.0, to.1);
                    return;
                } else {
                    clamped
                };
                if let Some(relaying) = &mut self.relaying {
                    relaying.pointer = pointer;
                }
                self.relay_pointer(event.buttons, event.modifiers);
            }
            LC_INPUT_KEY_UP if self.held_keys.remove(&event.code) => key(event, false),
            LC_INPUT_MOUSE_BUTTON_UP if self.held_buttons.remove(&event.code) => {
                button(event.code, false)
            }
            _ => self.relay(*event),
        }
    }

    /// Where the pointer is, as an absolute move in the screen's own space.
    fn relay_pointer(&self, buttons: u32, modifiers: u32) {
        let Some(relaying) = &self.relaying else {
            return;
        };
        self.relay(LcInputEvent {
            kind: LC_INPUT_MOUSE_MOVE,
            flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
            buttons,
            modifiers,
            x: relaying.pointer.0 - relaying.offset.0,
            y: relaying.pointer.1 - relaying.offset.1,
            ..LcInputEvent::default()
        });
    }

    fn relay(&self, event: LcInputEvent) {
        if self.relaying.as_ref().is_some_and(|relaying| relaying.live)
            && let Some(core) = self.core()
        {
            let _ = core.relay_input(event);
        }
    }

    /// The pointer left the attached screen, or relaying ended. `tell_core`:
    /// the core still relays and must stop (the screen then releases what
    /// it holds).
    fn end_relay(&mut self, tell_core: bool) {
        let Some(relaying) = self.relaying.take() else {
            return;
        };
        if tell_core
            && relaying.live
            && let Some(core) = self.core()
        {
            let _ = core.set_relay_target(None);
        }
        if let Some(viewers) = crate::viewer::global() {
            viewers.driving(None);
        }
        log!("the pointer left the attached screen {}", relaying.peer_id);
    }

    /// The core stopped relaying by itself (`relay_ended`).
    fn relay_ended(&mut self, peer_id: &str, reason: &str) {
        let Some(relaying) = &mut self.relaying else {
            return;
        };
        if relaying.peer_id != peer_id {
            return;
        }
        if reason == "revoked" {
            // Another device took the screen over: its picture shows that
            // device now. The pointer stays there, doing nothing, until it
            // comes back.
            log!("the attached screen {peer_id} was taken over by another device");
            relaying.live = false;
            if let Some(viewers) = crate::viewer::global() {
                viewers.driving(None);
            }
            return;
        }
        let pointer = relaying.pointer;
        self.end_relay(false);
        if reason == "not attached" {
            // Gone from under the pointer: back to the nearest point here.
            self.move_to(pointer.0, pointer.1);
        }
    }

    fn set_controlled(&mut self, controlled: bool) {
        if self.controlled == controlled {
            return;
        }
        self.controlled = controlled;
        self.motion_rest = (0.0, 0.0);
        self.wheel_rest = (0.0, 0.0);
        if !controlled {
            // The core already released everything and stopped relaying.
            self.end_relay(false);
            self.held_keys.clear();
            self.held_buttons.clear();
        }
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
        self.lock_checked = Some(Instant::now());
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
        let locked = match self.lock_checked {
            Some(at) if at.elapsed() < LOCK_CHECK_INTERVAL => self.pointer_locked,
            _ => self.check_pointer_lock(),
        };
        if locked {
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
            if let Some((target, before, at)) = self.last_move
                && cursor.x == before.x
                && cursor.y == before.y
                && at.elapsed() < LANDING_TIME
            {
                cursor = target;
            }
            self.last_cursor = Some(cursor);
        }
        let Some(at) = self.last_cursor else {
            return;
        };
        // Pushed past the edge onto an attached screen.
        if !self.attached.is_empty() {
            let monitors = self.monitors();
            if let Some((x, y)) = displays::to_logical(&monitors, at.x, at.y) {
                let to = (x + dx, y + dy);
                if let Some(screen) = self.screen_at(to)
                    && !monitors
                        .iter()
                        .any(|monitor| contains(&monitor.logical, to.0, to.1))
                {
                    return self.start_relay(screen, to);
                }
            }
        }
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
        // Where Windows will put it: on a monitor and within the cursor's
        // clip, not merely within the desktop's bounding box. A target in
        // the empty corner beside a smaller monitor would otherwise count as
        // still on its way for ever, and each move would go on from a point
        // the pointer never reaches.
        let landed = landing(target);
        self.last_move = (landed.x != before.x || landed.y != before.y).then_some((
            landed,
            before,
            Instant::now(),
        ));
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

/// Where an absolute move to `target` leaves the pointer: Windows keeps it
/// on a monitor (the nearest one) and within the cursor's clip rectangle.
fn landing(target: POINT) -> POINT {
    let mut landed = target;
    // SAFETY: plain monitor and clip queries into properly sized structures.
    unsafe {
        let monitor = MonitorFromPoint(target, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            landed = clamp_into(landed, info.rcMonitor);
        }
        let mut clip: RECT = std::mem::zeroed();
        if GetClipCursor(&mut clip) != 0 {
            landed = clamp_into(landed, clip);
        }
    }
    landed
}

/// The point of `rect` (right and bottom exclusive) nearest to `point`.
fn clamp_into(point: POINT, rect: RECT) -> POINT {
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return point;
    }
    POINT {
        x: point.x.clamp(rect.left, rect.right - 1),
        y: point.y.clamp(rect.top, rect.bottom - 1),
    }
}

/// The point is on the display (logical points; right and bottom exclusive).
fn contains(display: &Display, x: f64, y: f64) -> bool {
    x >= display.x && x < display.right() && y >= display.y && y < display.bottom()
}

/// `to`, kept on the display the pointer is on (`from`): it stops at an
/// edge that leads nowhere.
fn clamp_onto(displays: &[Display], from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let Some(display) = displays
        .iter()
        .find(|display| contains(display, from.0, from.1))
        .or(displays.first())
    else {
        return from;
    };
    // Just inside the right and bottom edges, which are exclusive.
    const INSIDE: f64 = 0.001;
    (
        to.0.clamp(display.x, display.right() - INSIDE),
        to.1.clamp(display.y, display.bottom() - INSIDE),
    )
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

/// Input was refused on a UAC prompt or the lock screen and the user was
/// told why; cleared when input goes through again.
static REFUSED_TOLD: AtomicBool = AtomicBool::new(false);

fn send(input: INPUT) {
    if elevated::forward(&input) {
        return;
    }
    // SAFETY: one fully initialised INPUT with its exact size.
    let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
    if sent != 0 {
        REFUSED_TOLD.store(false, Ordering::Relaxed);
        return;
    }
    let error = io::Error::last_os_error();
    // No retry: SendInput fails when another desktop (UAC, lock screen) has
    // input, and retrying would only spin. UIPI blocks silently anyway.
    if !crate::win::input_desktop_is_default() {
        if !REFUSED_TOLD.swap(true, Ordering::Relaxed) {
            tell_refused();
        }
    } else if cfg!(debug_assertions) {
        log!("SendInput failed: {error}");
    }
}

/// Says once why a peer's keys and clicks do nothing on a UAC prompt or
/// the lock screen, and what makes them work.
fn tell_refused() {
    // Installed with setup: the settings switch adds the service (setup
    // leaves it out while the setting is off).
    let installed = elevated::available() || rinflow_input::win::service_installable();
    log!(
        "a UAC prompt or the lock screen has input: Windows refuses this process's input there; {}",
        if installed {
            "the input service is off (the settings switch turns it on)"
        } else {
            "it needs the input service that the setup program installs"
        }
    );
    let text = if installed {
        tr(s::ElevatedTurnOnHint)
    } else {
        tr(s::ElevatedInstallHint)
    };
    crate::tray::notify(tr(s::ElevatedRefusedTitle), text);
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
    use rinflow_input::VirtualDesktop;
    use windows_sys::Win32::Foundation::RECT;

    #[test]
    fn only_relative_motion_is_merged() {
        use super::merge_motion;
        use rinflow_core::{
            LC_INPUT_FLAG_ABSOLUTE_POSITION, LC_INPUT_MOUSE_BUTTON_DOWN, LC_INPUT_MOUSE_MOVE,
            LcInputEvent,
        };
        let motion = |dx: f64, dy: f64| LcInputEvent {
            kind: LC_INPUT_MOUSE_MOVE,
            delta_x: dx,
            delta_y: dy,
            ..LcInputEvent::default()
        };
        let mut event = motion(1.5, -2.0);
        assert!(merge_motion(&mut event, &motion(0.25, 4.0)));
        assert!(merge_motion(
            &mut event,
            &LcInputEvent {
                buttons: 1,
                ..motion(1.0, 0.0)
            }
        ));
        assert_eq!(
            (event.delta_x, event.delta_y, event.buttons),
            (2.75, 2.0, 1)
        );

        // A click keeps its place between the moves around it, and the move
        // that places the pointer is not added to.
        let click = LcInputEvent {
            kind: LC_INPUT_MOUSE_BUTTON_DOWN,
            code: 1,
            ..LcInputEvent::default()
        };
        let placed = LcInputEvent {
            flags: LC_INPUT_FLAG_ABSOLUTE_POSITION,
            x: 10.0,
            ..motion(0.0, 0.0)
        };
        assert!(!merge_motion(&mut event, &click));
        assert!(!merge_motion(&mut event, &placed));
        assert!(!merge_motion(&mut placed.clone(), &motion(1.0, 1.0)));
        assert_eq!((event.delta_x, event.delta_y), (2.75, 2.0));
    }

    #[test]
    fn attached_screens_read_from_the_core_and_hold_the_pointer() {
        use super::{AttachedScreen, clamp_onto};
        let event: serde_json::Value = serde_json::from_str(
            r#"{"peer_id":"kvm","offset":{"x":1440,"y":-90},
                "displays":[{"x":1440,"y":-90,"width":1920,"height":1080,"scale":1}]}"#,
        )
        .unwrap();
        let screen = AttachedScreen::from_json(&event).unwrap();
        assert_eq!(screen.offset, (1440.0, -90.0));
        // Just past this PC's right edge (1440 wide) is on the screen.
        assert!(screen.contains((1440.5, 400.0)));
        assert!(!screen.contains((1439.5, 400.0)));
        assert!(!screen.contains((3360.0, 400.0)));
        // Pushed past its outer edges, the pointer stays on it.
        let held = clamp_onto(&screen.displays, (3000.0, 400.0), (3500.0, -200.0));
        assert!(screen.contains(held), "{held:?}");
        assert_eq!(held.1, -90.0);
    }

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
    fn a_move_lands_on_the_monitor_not_beside_it() {
        use super::clamp_into;
        use windows_sys::Win32::Foundation::POINT;
        // A 1920×1080 monitor next to a taller one: pushing down at its
        // bottom edge stays on its last row.
        let monitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let landed = clamp_into(POINT { x: 500, y: 1100 }, monitor);
        assert_eq!((landed.x, landed.y), (500, 1079));
        let inside = clamp_into(POINT { x: 500, y: 500 }, monitor);
        assert_eq!((inside.x, inside.y), (500, 500));
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

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The edge thread: watches the pointer at the arrangement's portals.
//!
//! Pushing against the portal of a connected peer feels the resistance of
//! the edge effect and then crosses into the peer, handing this PC's
//! keyboard and mouse to [`crate::capture`] until the pointer comes back.
//!
//! With the effect demo on, the portals (the whole right edge if nothing is
//! placed) instead behave as if the peer were there and the screen wrapped
//! around, for tuning the effect on one machine: after crossing, the cursor
//! comes out of the opposite edge, which then leads back without resistance.
//!
//! Push strength comes from raw input, which keeps reporting motion while the
//! cursor is pinned against the edge. A process can register raw mouse input
//! for one window only, so crossing and the demo share this thread.
//!
//! The user's settings decide whether a push crosses at all: sharing can be
//! paused, the lock shortcut (registered here) keeps the pointer on the
//! current screen, corners can be excluded, and full-screen games can pause
//! crossing. A push also has to last the switch delay.

use crate::arrangement::Arrangement;
use crate::capture::{self, WM_END_CAPTURE};
use crate::displays::PhysicalPortal;
use crate::effect::{EffectParams, EffectStore};
use crate::i18n::{s, trf};
use crate::overlay::{Command, ENTRY_INSET, Edge, EdgePoint, OverlayHandle};
use crate::settings::{Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, Settings, SettingsStore};
use crate::state::{DemoSide, SharedState};
use crate::win::wide;
use rinflow_core::Core;
use rinflow_core::layout::Side;
use std::cell::RefCell;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SetCursorPos, SetTimer, TranslateMessage, WM_APP, WM_HOTKEY, WM_INPUT, WM_QUIT, WM_TIMER,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Stands in for a peer in the demo when nothing is placed yet.
const FALLBACK_EDGE: Edge = Edge::Right;
/// Raw counts that return to the local side in the demo; no resistance.
const RETURN_PUSH: f32 = 3.0;
/// Ignore pushes right after a crossing so leftover motion cannot bounce back.
const CROSS_COOLDOWN: Duration = Duration::from_millis(250);
const RELEASE_TIMER: usize = 1;
/// Thread message: the demo was turned on (wparam 1) or off.
const WM_SET_DEMO: u32 = WM_APP + 1;
/// Thread message: entries were added to [`REVOKED`].
const WM_CONTROL_REVOKED: u32 = WM_APP + 3;
/// Thread message: the settings changed.
const WM_SETTINGS_CHANGED: u32 = WM_APP + 4;
/// Window message from the capture hooks: the lock shortcut was pressed
/// while a peer is driven (the hooks keep it from `RegisterHotKey`).
pub const WM_TOGGLE_LOCK: u32 = WM_APP + 5;
/// Thread message: another desktop took input (a UAC prompt, the lock
/// screen); a driven peer is given back so this PC's own keyboard and mouse
/// reach it.
const WM_RELEASE_CAPTURE: u32 = WM_APP + 6;
/// Thread message: a game on a peer let go of its pointer; the peer is in
/// [`POINTER_RELEASED`].
const WM_POINTER_RELEASED: u32 = WM_APP + 7;
/// Thread message: the arrangement changed (a placement, on either device,
/// or displays); a driven peer's pointer follows it (`capture::rearranged`).
const WM_ARRANGEMENT_CHANGED: u32 = WM_APP + 8;
const HOTKEY_ID: i32 = 1;
/// The switch shortcuts (arrow keys), registered while no peer is driven (the capture
/// hooks take them otherwise): ids 2 to 5, in [`SWITCH_SIDES`] order.
const SWITCH_HOTKEY_BASE: i32 = 2;
const SWITCH_SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

/// Peers whose pointer a game or a pointer-lock page holds (the core's
/// `peer_pointer_lock`): driving them, motion goes to them whole and their
/// edges lead nowhere.
static POINTER_HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Peers whose game let go, for the edge thread.
static POINTER_RELEASED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn pointer_held(peer_id: &str) -> bool {
    POINTER_HELD
        .lock()
        .is_ok_and(|held| held.iter().any(|id| id == peer_id))
}

/// Forgets what a peer said: it says it again when driven anew.
pub fn clear_pointer_held(peer_id: &str) {
    if let Ok(mut held) = POINTER_HELD.lock() {
        held.retain(|id| id != peer_id);
    }
}

/// Control this PC lost (`control_revoked`), queued by the core's callback
/// thread for the edge thread, which owns the capture: the peer, and whether
/// the cursor stays where the new controller puts it.
static REVOKED: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

/// Locked to the current screen: the pointer neither crosses into a peer
/// nor comes back from the one it drives.
static LOCKED: AtomicBool = AtomicBool::new(false);
/// The lock shortcut, `modifiers << 32 | key`, for the capture hooks.
static LOCK_HOTKEY: AtomicU64 = AtomicU64::new(0);

pub fn locked() -> bool {
    LOCKED.load(Ordering::Relaxed)
}

/// The lock shortcut while it is not registered (a peer is driven).
pub fn lock_hotkey() -> Hotkey {
    let packed = LOCK_HOTKEY.load(Ordering::Relaxed);
    Hotkey {
        modifiers: (packed >> 32) as u32,
        key: packed as u32,
    }
}

pub struct EdgeThread {
    handle: EdgeHandle,
    join: Option<JoinHandle<()>>,
}

/// Turns the demo on and off from other threads.
#[derive(Clone, Copy)]
pub struct EdgeHandle {
    thread_id: u32,
}

impl EdgeHandle {
    pub fn set_demo(&self, enabled: bool) {
        // SAFETY: posts to the edge thread's message queue.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_SET_DEMO, usize::from(enabled), 0);
        }
    }

    /// Re-reads the settings: sharing paused gives a driven peer back, and
    /// a new lock shortcut is registered.
    pub fn settings_changed(&self) {
        // SAFETY: posts to the edge thread's message queue.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_SETTINGS_CHANGED, 0, 0);
        }
    }

    /// Gives a driven peer back, as Ctrl+Alt+Esc does.
    pub fn release_capture(&self) {
        // SAFETY: posts to the edge thread's message queue.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_RELEASE_CAPTURE, 0, 0);
        }
    }

    /// Picks `control_revoked` out of the core's event stream: the core
    /// took this PC's control of a peer away, so the capture has to end.
    pub fn core_event(&self, json: &str) {
        if !(json.contains("control_revoked")
            || json.contains("peer_pointer_lock")
            || json.contains("\"disconnected\"")
            || json.contains("placement_changed")
            || json.contains("portals_changed"))
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        // The arrangement already took it in (main.rs hands it events
        // first); a peer driven meanwhile keeps its pointer where it is.
        if event["event"] == "placement_changed" || event["event"] == "portals_changed" {
            // SAFETY: posts to the edge thread's message queue.
            unsafe {
                PostThreadMessageW(self.thread_id, WM_ARRANGEMENT_CHANGED, 0, 0);
            }
            return;
        }
        let Some(peer_id) = event["peer_id"].as_str() else {
            return;
        };
        if event["event"] == "disconnected" {
            clear_pointer_held(peer_id);
            return;
        }
        if event["event"] == "peer_pointer_lock" {
            let locked = event["locked"].as_bool().unwrap_or(false);
            clear_pointer_held(peer_id);
            if locked {
                if let Ok(mut held) = POINTER_HELD.lock() {
                    held.push(peer_id.to_owned());
                }
            } else {
                if let Ok(mut released) = POINTER_RELEASED.lock() {
                    released.push(peer_id.to_owned());
                }
                // SAFETY: posts to the edge thread's message queue.
                unsafe {
                    PostThreadMessageW(self.thread_id, WM_POINTER_RELEASED, 0, 0);
                }
            }
            return;
        }
        if event["event"] != "control_revoked" {
            return;
        }
        // `taken_over`: a peer drives this PC now and places the cursor.
        let stay = event["reason"] == "taken_over";
        if let Ok(mut revoked) = REVOKED.lock() {
            revoked.push((peer_id.to_owned(), stay));
        }
        // SAFETY: posts to the edge thread's message queue.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_CONTROL_REVOKED, 0, 0);
        }
    }
}

impl EdgeThread {
    pub fn start(
        overlay: OverlayHandle,
        store: EffectStore,
        settings: Arc<SettingsStore>,
        state: SharedState,
        arrangement: Arc<Arrangement>,
        core: Weak<Core>,
    ) -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("rinflow-edge".to_owned())
            .spawn(move || {
                // SAFETY: creates the message queue before publishing the id.
                let thread_id = unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
                    GetCurrentThreadId()
                };
                let window = match create_input_window() {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(thread_id));
                TRACKER.with(|tracker| {
                    let mut created = Tracker::new(
                        overlay.clone(),
                        store,
                        settings,
                        state,
                        arrangement,
                        core,
                        window,
                    );
                    created.settings_changed();
                    *tracker.borrow_mut() = Some(created);
                });
                // SAFETY: standard message loop; the window lives on this thread.
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        if message.hwnd.is_null() && message.message == WM_SET_DEMO {
                            let enabled = message.wParam != 0;
                            with_tracker(|tracker| tracker.set_demo(enabled));
                            continue;
                        }
                        if message.hwnd.is_null() && message.message == WM_SETTINGS_CHANGED {
                            with_tracker(Tracker::settings_changed);
                            continue;
                        }
                        if message.hwnd.is_null() && message.message == WM_RELEASE_CAPTURE {
                            capture::release();
                            continue;
                        }
                        if message.hwnd.is_null() && message.message == WM_ARRANGEMENT_CHANGED {
                            capture::rearranged();
                            continue;
                        }
                        if message.hwnd.is_null() && message.message == WM_POINTER_RELEASED {
                            let released = POINTER_RELEASED
                                .lock()
                                .map(|mut queue| std::mem::take(&mut *queue))
                                .unwrap_or_default();
                            for peer_id in released {
                                capture::pointer_released(&peer_id);
                            }
                            continue;
                        }
                        if message.hwnd.is_null() && message.message == WM_CONTROL_REVOKED {
                            let revoked = REVOKED
                                .lock()
                                .map(|mut queue| std::mem::take(&mut *queue))
                                .unwrap_or_default();
                            for (peer_id, stay) in revoked {
                                capture::revoked(&peer_id, stay);
                            }
                            continue;
                        }
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    // Never leave the keyboard and mouse captured.
                    capture::release();
                    capture::finish();
                    UnregisterHotKey(window, HOTKEY_ID);
                    for id in 0..SWITCH_SIDES.len() as i32 {
                        UnregisterHotKey(window, SWITCH_HOTKEY_BASE + id);
                    }
                    remove_input_window(window);
                }
                TRACKER.with(|tracker| {
                    if let Some(mut tracker) = tracker.borrow_mut().take() {
                        tracker.cancel();
                    }
                });
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                handle: EdgeHandle { thread_id },
                join: Some(join),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("edge thread exited during start-up")),
        }
    }

    pub fn handle(&self) -> EdgeHandle {
        self.handle
    }
}

impl Drop for EdgeThread {
    fn drop(&mut self) {
        // SAFETY: asks the edge thread's message loop to finish; it gives
        // back the keyboard and mouse first if a peer is being driven.
        unsafe {
            PostThreadMessageW(self.handle.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

thread_local! {
    static TRACKER: RefCell<Option<Tracker>> = const { RefCell::new(None) };
}

struct Tracker {
    overlay: OverlayHandle,
    store: EffectStore,
    settings: Arc<SettingsStore>,
    state: SharedState,
    arrangement: Arc<Arrangement>,
    core: Weak<Core>,
    /// This thread's window, which hears when a capture ends.
    window: HWND,
    demo: bool,
    side: DemoSide,
    /// The demo edge last crossed outward; the way back is the opposite edge.
    crossed: Edge,
    /// Raw counts pushed against the peer edge since the push began.
    push: f32,
    pushing: bool,
    /// When the push began, for the switch delay.
    push_started: Instant,
    last_push: Instant,
    return_push: f32,
    warm_until: Option<Instant>,
    cooldown_until: Instant,
    /// The lock shortcut last registered (or tried).
    hotkey: Option<Hotkey>,
    /// The modifiers the switch shortcuts are registered with.
    switch_hotkeys: Option<u32>,
}

struct Hit {
    point: EdgePoint,
    monitor: RECT,
}

impl Tracker {
    fn new(
        overlay: OverlayHandle,
        store: EffectStore,
        settings: Arc<SettingsStore>,
        state: SharedState,
        arrangement: Arc<Arrangement>,
        core: Weak<Core>,
        window: HWND,
    ) -> Self {
        let now = Instant::now();
        Self {
            overlay,
            store,
            settings,
            state,
            arrangement,
            core,
            window,
            demo: false,
            side: DemoSide::Local,
            crossed: FALLBACK_EDGE,
            push: 0.0,
            pushing: false,
            push_started: now,
            last_push: now,
            return_push: 0.0,
            warm_until: None,
            cooldown_until: now,
            hotkey: None,
            switch_hotkeys: None,
        }
    }

    fn settings_changed(&mut self) {
        let settings = self.settings.get();
        if !settings.sharing {
            self.cancel();
            // A peer being driven gets its keyboard and mouse back.
            capture::release();
        }
        self.register_hotkey(settings.lock_hotkey);
        self.register_switch_hotkeys(settings.switch_hotkeys.then_some(settings.switch_modifiers));
    }

    /// What a capture needs to move on to other devices.
    fn context(&self) -> capture::Context {
        capture::Context {
            arrangement: self.arrangement.clone(),
            state: self.state.clone(),
            settings: self.settings.clone(),
        }
    }

    /// The switch shortcuts (the arrow keys with `modifiers`, `None` for
    /// off) while this PC has the pointer; a driven peer's capture hooks see
    /// them otherwise. A combination another program took first is reported
    /// to the settings page (`switch_hotkeys_blocked`), the others still work.
    fn register_switch_hotkeys(&mut self, modifiers: Option<u32>) {
        if self.switch_hotkeys == modifiers {
            return;
        }
        self.switch_hotkeys = modifiers;
        let mut blocked = false;
        for (index, side) in SWITCH_SIDES.iter().enumerate() {
            let id = SWITCH_HOTKEY_BASE + index as i32;
            // SAFETY: (un)registers this thread's window for the shortcut.
            unsafe {
                UnregisterHotKey(self.window, id);
                if let Some(modifiers) = modifiers
                    && RegisterHotKey(
                        self.window,
                        id,
                        (modifiers | MOD_NOREPEAT) as HOT_KEY_MODIFIERS,
                        capture::side_key(*side),
                    ) == 0
                {
                    blocked = true;
                    log!(
                        "switch shortcut {}+{side:?} is taken by another program: {}",
                        describe_modifiers(modifiers),
                        io::Error::last_os_error()
                    );
                }
            }
        }
        self.state.set_switch_hotkeys_blocked(blocked);
    }

    /// A switch shortcut: to the device on that side of this PC.
    fn jump(&mut self, side: Side) {
        if capture::active() {
            capture::jump(side);
            return;
        }
        let settings = self.settings.get();
        if !settings.sharing || !settings.switch_hotkeys || locked() || self.demo {
            return;
        }
        let Some(world) = self.arrangement.world() else {
            return;
        };
        let Some(Some(peer_id)) = world.toward(None, side) else {
            return;
        };
        if !self.state.is_connected(&peer_id) {
            return;
        }
        let (Some(point), Some(target)) = (
            world.landing(None, Some(&peer_id)),
            self.arrangement.target(&peer_id),
        ) else {
            return;
        };
        self.cancel();
        capture::begin_at(
            &self.core,
            &peer_id,
            target,
            point,
            self.window,
            self.context(),
        );
    }

    fn register_hotkey(&mut self, hotkey: Hotkey) {
        if self.hotkey == Some(hotkey) {
            return;
        }
        self.hotkey = Some(hotkey);
        LOCK_HOTKEY.store(
            u64::from(hotkey.modifiers) << 32 | u64::from(hotkey.key),
            Ordering::Relaxed,
        );
        // SAFETY: (re)registers this thread's window for the shortcut.
        let registered = unsafe {
            UnregisterHotKey(self.window, HOTKEY_ID);
            RegisterHotKey(
                self.window,
                HOTKEY_ID,
                (hotkey.modifiers | MOD_NOREPEAT) as HOT_KEY_MODIFIERS,
                hotkey.key,
            ) != 0
        };
        if !registered {
            let shortcut = describe_hotkey(hotkey);
            self.state
                .set_error(trf(s::LockShortcutTaken, &[&shortcut]));
        }
    }

    fn toggle_lock(&mut self) {
        let locked = !LOCKED.load(Ordering::Relaxed);
        LOCKED.store(locked, Ordering::Relaxed);
        self.state.set_locked(locked);
        if locked {
            self.cancel();
        }
    }

    /// Corners excluded from crossing, in points.
    fn dead_zone(settings: &Settings, params: &EffectParams) -> f32 {
        if settings.corner_guard {
            params.corner_dead_zone
        } else {
            0.0
        }
    }

    /// How far the push is, 1 meaning it crosses: it must be pushed far
    /// enough and have lasted the switch delay.
    fn progress(&self, settings: &Settings, params: &EffectParams, now: Instant) -> f32 {
        let distance = self.push / params.push_distance;
        let delay = settings.switch_delay_ms as f32 / 1000.0;
        let time = if delay <= 0.0 {
            1.0
        } else {
            now.duration_since(self.push_started).as_secs_f32() / delay
        };
        distance.min(time)
    }

    fn set_demo(&mut self, enabled: bool) {
        self.cancel();
        self.demo = enabled;
        self.side = DemoSide::Local;
        self.crossed = FALLBACK_EDGE;
        self.state.set_demo_side(DemoSide::Local);
    }

    fn on_motion(&mut self, dx: i32, dy: i32) {
        // While a peer is driven, the capture hooks handle all input.
        if capture::active() {
            return;
        }
        let now = Instant::now();
        if now < self.cooldown_until {
            return;
        }
        let settings = self.settings.get();
        // Sharing paused or locked: nothing crosses. The demo only simulates
        // a peer on this PC and keeps working.
        if !self.demo && (!settings.sharing || locked()) {
            self.cancel();
            return;
        }
        let params = self.store.get();
        let dead_zone = Self::dead_zone(&settings, &params);
        let cursor = cursor_position();
        let hit = match self.side {
            DemoSide::Local => self.portal_hit(cursor, dead_zone),
            DemoSide::Remote => {
                edge_hit(self.crossed.opposite(), cursor, dead_zone).map(|hit| (hit, None))
            }
        };
        let Some((hit, portal)) = hit else {
            self.cancel();
            return;
        };
        let edge = hit.point.edge;
        let outward = outward(edge, dx, dy) as f32;

        match self.side {
            DemoSide::Remote => {
                if outward > 0.0 {
                    self.return_push += outward;
                    if self.return_push >= RETURN_PUSH {
                        self.cross(&hit, None);
                    }
                }
            }
            DemoSide::Local => {
                let warm = self.warm_until.is_some_and(|until| now < until);
                if outward > 0.0 && warm {
                    self.cross(&hit, portal);
                    return;
                }
                if outward > 0.0 {
                    if self.push <= 0.0 {
                        // A new push; none into a full-screen game.
                        if !self.demo && settings.pause_in_fullscreen && full_screen_app_running() {
                            return;
                        }
                        self.push_started = now;
                    }
                    self.push += outward;
                    self.last_push = now;
                } else if outward < 0.0 {
                    self.push = (self.push + outward).max(0.0);
                }
                if self.push <= 0.0 {
                    self.cancel();
                    return;
                }
                let progress = self.progress(&settings, &params, now);
                if progress >= 1.0 {
                    self.cross(&hit, portal);
                    return;
                }
                // Sliding along the edge also lands here and moves the bump.
                self.pushing = true;
                self.overlay.send(Command::Approach {
                    at: hit.point,
                    progress,
                });
            }
        }
    }

    fn on_timer(&mut self) {
        if self.pushing {
            self.push_waiting();
        }
        let state = &self.state;
        capture::check(|peer_id| state.is_connected(peer_id));
    }

    /// A push that went far enough crosses when the switch delay is up, even
    /// if the pointer holds still against the edge meanwhile; otherwise the
    /// bump springs back once the pushing stops.
    fn push_waiting(&mut self) {
        let settings = self.settings.get();
        let params = self.store.get();
        if self.push >= params.push_distance {
            let dead_zone = Self::dead_zone(&settings, &params);
            if let Some((hit, portal)) = self.portal_hit(cursor_position(), dead_zone) {
                let progress = self.progress(&settings, &params, Instant::now());
                if progress >= 1.0 {
                    self.cross(&hit, portal);
                } else {
                    self.overlay.send(Command::Approach {
                        at: hit.point,
                        progress,
                    });
                }
                return;
            }
        }
        if self.last_push.elapsed().as_secs_f32() > params.release_delay {
            self.cancel();
        }
    }

    fn cancel(&mut self) {
        if self.pushing {
            self.overlay.send(Command::Release);
        }
        self.pushing = false;
        self.push = 0.0;
        self.return_push = 0.0;
    }

    /// Where the cursor pushes against a peer: inside a portal of a connected
    /// peer, or in the demo of any placed peer. The demo falls back to the
    /// whole right edge when nothing is placed.
    fn portal_hit(&self, cursor: POINT, dead_zone: f32) -> Option<(Hit, Option<PhysicalPortal>)> {
        let portals: Vec<PhysicalPortal> = self
            .arrangement
            .portals()
            .into_iter()
            .filter(|portal| self.demo || self.state.is_connected(&portal.peer_id))
            .collect();
        if portals.is_empty() {
            if !self.demo {
                return None;
            }
            return edge_hit(FALLBACK_EDGE, cursor, dead_zone).map(|hit| (hit, None));
        }
        let (_, _, scale) = monitor_at(cursor);
        portals.into_iter().find_map(|portal| {
            // A short portal in a corner would otherwise lie entirely in the
            // corner dead zone: let the dead zone take at most a quarter.
            let length = (portal.to - portal.from) as f32 / scale;
            let dead_zone = dead_zone.min(length / 4.0);
            let hit = edge_hit(edge_of(portal.side), cursor, dead_zone)?;
            let (line, along) = if portal.side.is_vertical() {
                (hit.point.x, hit.point.y)
            } else {
                (hit.point.y, hit.point.x)
            };
            (line == portal.line && (portal.from..portal.to).contains(&along))
                .then_some((hit, Some(portal)))
        })
    }

    fn cross(&mut self, hit: &Hit, portal: Option<PhysicalPortal>) {
        if self.pushing {
            self.overlay.send(Command::Pass);
        }
        self.pushing = false;
        self.push = 0.0;
        self.return_push = 0.0;

        if !self.demo {
            // Into the peer: from here the capture hooks drive.
            if let Some(portal) = portal
                && let Some(target) = self.arrangement.target(&portal.peer_id)
            {
                capture::begin(
                    &self.core,
                    target,
                    &portal,
                    cursor_position(),
                    self.window,
                    self.context(),
                );
            }
            return;
        }

        if self.side == DemoSide::Local {
            self.crossed = hit.point.edge;
        }
        let (entry, cursor) = wrap_around(hit);
        // SAFETY: moves the cursor to a point on the desktop.
        unsafe {
            SetCursorPos(cursor.x, cursor.y);
        }
        self.overlay.send(Command::Emerge { at: entry });
        self.start_cooldown();
        self.side = match self.side {
            DemoSide::Local => DemoSide::Remote,
            DemoSide::Remote => DemoSide::Local,
        };
        self.state.set_demo_side(self.side);
    }

    /// The pointer came back from a peer: show it coming in, and let it go
    /// straight back for a while without pushing again.
    fn returned(&mut self, peer_id: &str, cursor: POINT) {
        if let Some(at) = self.entry_point(peer_id, cursor) {
            self.overlay.send(Command::Emerge { at });
        }
        self.start_cooldown();
    }

    fn start_cooldown(&mut self) {
        let now = Instant::now();
        let warm = Duration::from_secs_f32(self.store.get().warm_seconds);
        self.cooldown_until = now + CROSS_COOLDOWN;
        self.warm_until = Some(now + warm);
    }

    /// The point on the portal edge nearest to where the cursor came back.
    fn entry_point(&self, peer_id: &str, cursor: POINT) -> Option<EdgePoint> {
        self.arrangement
            .portals()
            .into_iter()
            .filter(|portal| portal.peer_id == peer_id)
            .filter_map(|portal| {
                let (across, along) = if portal.side.is_vertical() {
                    (cursor.x, cursor.y)
                } else {
                    (cursor.y, cursor.x)
                };
                (portal.from..portal.to)
                    .contains(&along)
                    .then(|| ((across - portal.line).abs(), portal))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, portal)| {
                let (x, y) = if portal.side.is_vertical() {
                    (portal.line, cursor.y)
                } else {
                    (cursor.x, portal.line)
                };
                EdgePoint {
                    edge: edge_of(portal.side),
                    x,
                    y,
                }
            })
    }
}

/// A full-screen game (Direct3D exclusive mode), a full-screen app or
/// presentation mode is running.
fn full_screen_app_running() -> bool {
    let mut state = 0;
    // SAFETY: writes the notification state into `state`.
    let ok = unsafe { SHQueryUserNotificationState(&mut state) } == 0;
    ok && matches!(
        state,
        QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
    )
}

/// "Ctrl+Alt+L", for messages.
fn describe_hotkey(hotkey: Hotkey) -> String {
    let mut parts = vec![describe_modifiers(hotkey.modifiers)];
    parts.push(match char::from_u32(hotkey.key) {
        Some(key) if key.is_ascii_alphanumeric() => key.to_string(),
        _ => format!("0x{:02X}", hotkey.key),
    });
    parts.join("+")
}

/// "Ctrl+Alt+Shift", for messages.
fn describe_modifiers(modifiers: u32) -> String {
    [
        (MOD_CONTROL, "Ctrl"),
        (MOD_ALT, "Alt"),
        (MOD_SHIFT, "Shift"),
        (MOD_WIN, "Win"),
    ]
    .iter()
    .filter(|(bit, _)| modifiers & bit != 0)
    .map(|(_, name)| *name)
    .collect::<Vec<_>>()
    .join("+")
}

fn edge_of(side: Side) -> Edge {
    match side {
        Side::Left => Edge::Left,
        Side::Right => Edge::Right,
        Side::Top => Edge::Top,
        Side::Bottom => Edge::Bottom,
    }
}

/// Motion component pointing out of the screen through `edge`.
fn outward(edge: Edge, dx: i32, dy: i32) -> i32 {
    match edge {
        Edge::Right => dx,
        Edge::Left => -dx,
        Edge::Bottom => dy,
        Edge::Top => -dy,
    }
}

/// The cursor's spot on `edge` if it is pinned against an outer edge of the
/// desktop (no monitor continues past it) and outside the corner dead zones.
fn edge_hit(edge: Edge, cursor: POINT, dead_zone: f32) -> Option<Hit> {
    let (_, rect, scale) = monitor_at(cursor);
    let dead = (dead_zone * scale).round() as i32;
    let (pinned, beyond, point, along, (low, high)) = match edge {
        Edge::Right => (
            cursor.x >= rect.right - 1,
            POINT {
                x: rect.right,
                y: cursor.y,
            },
            (rect.right, cursor.y),
            cursor.y,
            (rect.top, rect.bottom),
        ),
        Edge::Left => (
            cursor.x <= rect.left,
            POINT {
                x: rect.left - 1,
                y: cursor.y,
            },
            (rect.left, cursor.y),
            cursor.y,
            (rect.top, rect.bottom),
        ),
        Edge::Bottom => (
            cursor.y >= rect.bottom - 1,
            POINT {
                x: cursor.x,
                y: rect.bottom,
            },
            (cursor.x, rect.bottom),
            cursor.x,
            (rect.left, rect.right),
        ),
        Edge::Top => (
            cursor.y <= rect.top,
            POINT {
                x: cursor.x,
                y: rect.top - 1,
            },
            (cursor.x, rect.top),
            cursor.x,
            (rect.left, rect.right),
        ),
    };
    if !pinned || along < low + dead || along >= high - dead {
        return None;
    }
    // SAFETY: plain monitor query.
    if !unsafe { MonitorFromPoint(beyond, MONITOR_DEFAULTTONULL) }.is_null() {
        return None;
    }
    Some(Hit {
        point: EdgePoint {
            edge,
            x: point.0,
            y: point.1,
        },
        monitor: rect,
    })
}

/// Where the cursor comes back in on the opposite side of the desktop, keeping
/// its relative position along the edge.
fn wrap_around(hit: &Hit) -> (EdgePoint, POINT) {
    let edge = hit.point.edge.opposite();
    let source = hit.monitor;
    // SAFETY: plain system metric queries.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let fraction = |value: i32, low: i32, high: i32| {
        ((value - low) as f32 / (high - low).max(1) as f32).clamp(0.0, 1.0)
    };
    let place =
        |low: i32, high: i32, f: f32| (low as f32 + f * (high - low - 1) as f32).round() as i32;
    match edge {
        Edge::Left | Edge::Right => {
            let f = fraction(hit.point.y, source.top, source.bottom);
            let probe_x = if edge == Edge::Left { vx } else { vx + vw - 1 };
            let (_, target, _) = monitor_at(POINT {
                x: probe_x,
                y: hit.point.y,
            });
            let y = place(target.top, target.bottom, f);
            if edge == Edge::Left {
                (
                    EdgePoint {
                        edge,
                        x: target.left,
                        y,
                    },
                    POINT {
                        x: target.left + ENTRY_INSET,
                        y,
                    },
                )
            } else {
                (
                    EdgePoint {
                        edge,
                        x: target.right,
                        y,
                    },
                    POINT {
                        x: target.right - 1 - ENTRY_INSET,
                        y,
                    },
                )
            }
        }
        Edge::Top | Edge::Bottom => {
            let f = fraction(hit.point.x, source.left, source.right);
            let probe_y = if edge == Edge::Top { vy } else { vy + vh - 1 };
            let (_, target, _) = monitor_at(POINT {
                x: hit.point.x,
                y: probe_y,
            });
            let x = place(target.left, target.right, f);
            if edge == Edge::Top {
                (
                    EdgePoint {
                        edge,
                        x,
                        y: target.top,
                    },
                    POINT {
                        x,
                        y: target.top + ENTRY_INSET,
                    },
                )
            } else {
                (
                    EdgePoint {
                        edge,
                        x,
                        y: target.bottom,
                    },
                    POINT {
                        x,
                        y: target.bottom - 1 - ENTRY_INSET,
                    },
                )
            }
        }
    }
}

fn monitor_at(point: POINT) -> (windows_sys::Win32::Graphics::Gdi::HMONITOR, RECT, f32) {
    // SAFETY: plain monitor queries with properly sized out-structs.
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let scale = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == 0 {
            dpi_x as f32 / 96.0
        } else {
            1.0
        };
        (monitor, info.rcMonitor, scale)
    }
}

fn cursor_position() -> POINT {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: writes the cursor position into `point`.
    unsafe {
        GetCursorPos(&mut point);
    }
    point
}

fn create_input_window() -> io::Result<HWND> {
    let class_name = wide("RinflowEdge");
    // SAFETY: hidden window on this thread, registered for raw mouse input
    // even while other applications have focus. A message-only window
    // (HWND_MESSAGE) does not receive background raw input.
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
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
        let device = RAWINPUTDEVICE {
            usUsagePage: 0x01, // generic desktop
            usUsage: 0x02,     // mouse
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        };
        if RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            let error = io::Error::last_os_error();
            DestroyWindow(window);
            return Err(error);
        }
        SetTimer(window, RELEASE_TIMER, 30, None);
        Ok(window)
    }
}

unsafe fn remove_input_window(window: HWND) {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: null_mut(),
    };
    // SAFETY: unregisters this process's raw mouse input and destroys the window.
    unsafe {
        RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32);
        KillTimer(window, RELEASE_TIMER);
        DestroyWindow(window);
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_INPUT => match read_raw_input(lparam) {
            Some(RawInput::Mouse { dx, dy, real }) => {
                if real {
                    capture::raw_input(capture::Device::Mouse);
                }
                with_tracker(|tracker| tracker.on_motion(dx, dy));
            }
            // Registered only while a peer is driven (see `capture`).
            Some(RawInput::Keyboard) => capture::raw_input(capture::Device::Keyboard),
            None => {}
        },
        WM_TIMER if wparam == RELEASE_TIMER => with_tracker(Tracker::on_timer),
        WM_HOTKEY if wparam == HOTKEY_ID as usize => {
            with_tracker(Tracker::toggle_lock);
            return 0;
        }
        WM_HOTKEY
            if (SWITCH_HOTKEY_BASE as usize..SWITCH_HOTKEY_BASE as usize + SWITCH_SIDES.len())
                .contains(&wparam) =>
        {
            let side = SWITCH_SIDES[wparam - SWITCH_HOTKEY_BASE as usize];
            with_tracker(|tracker| tracker.jump(side));
            return 0;
        }
        WM_TOGGLE_LOCK => {
            with_tracker(Tracker::toggle_lock);
            return 0;
        }
        WM_END_CAPTURE => {
            // Only a pointer that came back through a portal shows the
            // emerge animation.
            if let Some(ended) = capture::finish()
                && let Some(cursor) = ended.cursor
                && !ended.jumped
            {
                with_tracker(|tracker| tracker.returned(&ended.peer_id, cursor));
            }
            return 0;
        }
        _ => {}
    }
    // SAFETY: WM_INPUT must reach DefWindowProc so the system frees the data.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn with_tracker(action: impl FnOnce(&mut Tracker)) {
    TRACKER.with(|tracker| {
        if let Ok(mut tracker) = tracker.try_borrow_mut()
            && let Some(tracker) = tracker.as_mut()
        {
            action(tracker);
        }
    });
}

/// What raw input reported.
enum RawInput {
    /// Relative motion (or none, with buttons or a wheel). `real`: from a
    /// device, and something happened, so the mouse hook sees it too.
    Mouse { dx: i32, dy: i32, real: bool },
    /// A key from a keyboard device.
    Keyboard,
}

fn read_raw_input(lparam: LPARAM) -> Option<RawInput> {
    // SAFETY: RAWINPUT is large enough for mouse and keyboard data; size is
    // checked.
    unsafe {
        let mut input: RAWINPUT = std::mem::zeroed();
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = GetRawInputData(
            lparam as _,
            RID_INPUT,
            (&mut input as *mut RAWINPUT).cast(),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        );
        if read == u32::MAX {
            return None;
        }
        // Played input (SendInput, by this agent or anyone) has no device.
        let device = !input.header.hDevice.is_null();
        match input.header.dwType {
            RIM_TYPEMOUSE => {
                let mouse = input.data.mouse;
                // Absolute devices (pens, remote desktop, a peer's played
                // input) do not report pushes, nor does what this agent plays
                // itself (a peer's motion into a game, the nudges that keep
                // the PC awake).
                if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0
                    || mouse.ulExtraInformation as usize == crate::inject::INJECTED_MARKER
                {
                    return None;
                }
                let (dx, dy) = (mouse.lLastX, mouse.lLastY);
                let buttons = mouse.Anonymous.ulButtons & 0xFFFF != 0;
                Some(RawInput::Mouse {
                    dx,
                    dy,
                    real: device && (dx != 0 || dy != 0 || buttons),
                })
            }
            RIM_TYPEKEYBOARD if device => {
                let keyboard = input.data.keyboard;
                (keyboard.ExtraInformation as usize != crate::inject::INJECTED_MARKER)
                    .then_some(RawInput::Keyboard)
            }
            _ => None,
        }
    }
}

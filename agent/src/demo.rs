//! Single-machine demo of the push-through effect, for tuning it.
//!
//! While enabled, the portals of the arrangement (where placed peers touch
//! this PC; the whole right edge if nothing is placed) behave as if the peer
//! were there and the screen wrapped around. Push against one to feel the
//! resistance; after crossing, the cursor comes out of the opposite edge as
//! it would on the other device. That edge then leads back without
//! resistance, like returning to the local machine.
//!
//! Push strength comes from raw input, which keeps reporting motion while the
//! cursor is pinned against the edge.

use crate::arrangement::Arrangement;
use crate::effect::EffectStore;
use crate::overlay::{Command, ENTRY_INSET, Edge, EdgePoint, OverlayHandle};
use crate::state::{DemoSide, SharedState};
use crate::win::wide;
use linkcursor_core::layout::Side;
use std::cell::RefCell;
use std::io;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SetCursorPos, SetTimer, TranslateMessage, WM_INPUT, WM_QUIT, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Stands in for a peer when nothing is placed in the arrangement yet.
const FALLBACK_EDGE: Edge = Edge::Right;
/// Raw counts that return to the local side; returning has no resistance.
const RETURN_PUSH: f32 = 3.0;
/// Ignore pushes right after a crossing so leftover motion cannot bounce back.
const CROSS_COOLDOWN: Duration = Duration::from_millis(250);
const RELEASE_TIMER: usize = 1;

pub struct EdgeDemo {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl EdgeDemo {
    pub fn start(
        overlay: OverlayHandle,
        store: EffectStore,
        state: SharedState,
        arrangement: Arc<Arrangement>,
    ) -> io::Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("linkcursor-edge-demo".to_owned())
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
                state.set_demo_side(DemoSide::Local);
                TRACKER.with(|tracker| {
                    *tracker.borrow_mut() =
                        Some(Tracker::new(overlay.clone(), store, state, arrangement));
                });
                // SAFETY: standard message loop; the window lives on this thread.
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
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
                thread_id,
                join: Some(join),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("edge demo thread exited during start-up")),
        }
    }
}

impl Drop for EdgeDemo {
    fn drop(&mut self) {
        // SAFETY: asks the demo thread's message loop to finish.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
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
    state: SharedState,
    arrangement: Arc<Arrangement>,
    side: DemoSide,
    /// The edge last crossed outward; the way back is the opposite edge.
    crossed: Edge,
    /// Raw counts pushed against the peer edge since the push began.
    push: f32,
    pushing: bool,
    last_push: Instant,
    return_push: f32,
    warm_until: Option<Instant>,
    cooldown_until: Instant,
}

struct Hit {
    point: EdgePoint,
    monitor: RECT,
}

impl Tracker {
    fn new(
        overlay: OverlayHandle,
        store: EffectStore,
        state: SharedState,
        arrangement: Arc<Arrangement>,
    ) -> Self {
        let now = Instant::now();
        Self {
            overlay,
            store,
            state,
            arrangement,
            side: DemoSide::Local,
            crossed: FALLBACK_EDGE,
            push: 0.0,
            pushing: false,
            last_push: now,
            return_push: 0.0,
            warm_until: None,
            cooldown_until: now,
        }
    }

    fn on_motion(&mut self, dx: i32, dy: i32) {
        let now = Instant::now();
        if now < self.cooldown_until {
            return;
        }
        let params = self.store.get();
        let cursor = cursor_position();
        let hit = match self.side {
            DemoSide::Local => self.portal_hit(cursor, params.corner_dead_zone),
            DemoSide::Remote => edge_hit(self.crossed.opposite(), cursor, params.corner_dead_zone),
        };
        let Some(hit) = hit else {
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
                        self.cross(&hit);
                    }
                }
            }
            DemoSide::Local => {
                let warm = self.warm_until.is_some_and(|until| now < until);
                if outward > 0.0 && warm {
                    self.cross(&hit);
                    return;
                }
                if outward > 0.0 {
                    self.push += outward;
                    self.last_push = now;
                } else if outward < 0.0 {
                    self.push = (self.push + outward).max(0.0);
                }
                if self.push <= 0.0 {
                    self.cancel();
                    return;
                }
                let progress = self.push / params.push_distance;
                if progress >= 1.0 {
                    self.cross(&hit);
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
        if self.pushing && self.last_push.elapsed().as_secs_f32() > self.store.get().release_delay {
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

    /// Where the cursor pushes against a placed peer: inside one of the
    /// arrangement's portals. Without any placement, the whole right edge
    /// stands in for a peer so the effect can still be tried.
    fn portal_hit(&self, cursor: POINT, dead_zone: f32) -> Option<Hit> {
        let portals = self.arrangement.portals();
        if portals.is_empty() {
            return edge_hit(FALLBACK_EDGE, cursor, dead_zone);
        }
        portals.iter().find_map(|portal| {
            let edge = match portal.side {
                Side::Left => Edge::Left,
                Side::Right => Edge::Right,
                Side::Top => Edge::Top,
                Side::Bottom => Edge::Bottom,
            };
            let hit = edge_hit(edge, cursor, dead_zone)?;
            let (line, along) = if portal.side.is_vertical() {
                (hit.point.x, hit.point.y)
            } else {
                (hit.point.y, hit.point.x)
            };
            (line == portal.line && (portal.from..portal.to).contains(&along)).then_some(hit)
        })
    }

    fn cross(&mut self, hit: &Hit) {
        if self.pushing {
            self.overlay.send(Command::Pass);
        }
        if self.side == DemoSide::Local {
            self.crossed = hit.point.edge;
        }
        self.pushing = false;
        self.push = 0.0;
        self.return_push = 0.0;

        let (entry, cursor) = wrap_around(hit);
        // SAFETY: moves the cursor to a point on the desktop.
        unsafe {
            SetCursorPos(cursor.x, cursor.y);
        }
        self.overlay.send(Command::Emerge { at: entry });

        let now = Instant::now();
        let warm = Duration::from_secs_f32(self.store.get().warm_seconds);
        self.cooldown_until = now + CROSS_COOLDOWN;
        self.warm_until = Some(now + warm);
        self.side = match self.side {
            DemoSide::Local => DemoSide::Remote,
            DemoSide::Remote => DemoSide::Local,
        };
        self.state.set_demo_side(self.side);
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
    let (monitor, rect, scale) = monitor_at(cursor);
    let _ = monitor;
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
    let class_name = wide("LinkCursorEdgeDemo");
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
        WM_INPUT => {
            if let Some((dx, dy)) = read_relative_motion(lparam) {
                with_tracker(|tracker| tracker.on_motion(dx, dy));
            }
        }
        WM_TIMER if wparam == RELEASE_TIMER => with_tracker(Tracker::on_timer),
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

fn read_relative_motion(lparam: LPARAM) -> Option<(i32, i32)> {
    // SAFETY: RAWINPUT is large enough for mouse data; size is checked.
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
        if read == u32::MAX || input.header.dwType != RIM_TYPEMOUSE {
            return None;
        }
        let mouse = input.data.mouse;
        // Absolute devices (pens, remote desktop) do not report pushes.
        if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            return None;
        }
        Some((mouse.lLastX, mouse.lLastY))
    }
}

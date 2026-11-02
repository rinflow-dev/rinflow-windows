// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! This PC's monitors, in the two coordinate systems the agent needs.
//!
//! The arrangement (rinflow-core `layout`) works in logical points, which
//! makes sizes comparable across devices. Hooks, `SendInput` and the overlay
//! work in physical pixels. Each monitor keeps both, so a point on a logical
//! edge converts exactly through the monitor it belongs to.

use rinflow_core::layout::{Display, Portal, Side};
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, SetTimer,
    TranslateMessage, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::win::wide;

#[derive(Clone, Copy)]
pub struct Monitor {
    pub logical: Display,
    pub physical: RECT,
}

fn same_rect(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

/// The monitors, ordered left to right, then top to bottom.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> i32 {
        // SAFETY: `data` is the Vec passed to EnumDisplayMonitors below.
        let list = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
        list.push(monitor);
        1
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only appends to `handles`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            &mut handles as *mut Vec<HMONITOR> as LPARAM,
        );
    }
    let mut found: Vec<(RECT, f64)> = handles.into_iter().filter_map(describe).collect();
    found.sort_by_key(|(rect, _)| (rect.left, rect.top));
    lay_out(&found)
}

/// A monitor's rectangle in physical pixels and its scale.
fn describe(handle: HMONITOR) -> Option<(RECT, f64)> {
    // SAFETY: plain monitor queries with properly sized out-structs.
    unsafe {
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(handle, &mut info) == 0 {
            return None;
        }
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        let scale = if GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) == 0 {
            dpi_x as f64 / 96.0
        } else {
            1.0
        };
        Some((info.rcMonitor, scale))
    }
}

/// The monitors in logical points, keeping the ones that touch in physical
/// pixels touching. Dividing each monitor's pixel position by its own scale
/// would not: with mixed scaling (a 150 % laptop beside a 100 % monitor)
/// that leaves gaps or overlaps between them, which a peer's model of the
/// pointer could not cross while Windows moves the real cursor on.
///
/// The primary monitor (at the origin) keeps `position / scale`; each
/// neighbour is then put against the edge it shares, outwards, its offset
/// along that edge converted at the scale of the monitor already placed.
/// A monitor touching none (not something Windows allows) keeps
/// `position / scale`. Points convert through their own monitor.
fn lay_out(found: &[(RECT, f64)]) -> Vec<Monitor> {
    let natural = |(rect, scale): &(RECT, f64)| Display {
        x: rect.left as f64 / scale,
        y: rect.top as f64 / scale,
        width: (rect.right - rect.left) as f64 / scale,
        height: (rect.bottom - rect.top) as f64 / scale,
        scale: *scale,
    };
    let mut placed: Vec<Option<Display>> = vec![None; found.len()];
    let primary = found
        .iter()
        .position(|(rect, _)| rect.left <= 0 && rect.top <= 0 && rect.right > 0 && rect.bottom > 0)
        .unwrap_or(0);
    if let Some(first) = found.get(primary) {
        placed[primary] = Some(natural(first));
    }
    let mut queue = vec![primary];
    while let Some(index) = queue.pop() {
        let Some(anchor) = placed[index] else {
            continue;
        };
        let (a, _) = found[index];
        for (other, entry) in found.iter().enumerate() {
            if placed[other].is_some() {
                continue;
            }
            let (m, _) = *entry;
            let mut display = natural(entry);
            let along_y = anchor.y + f64::from(m.top - a.top) / anchor.scale;
            let along_x = anchor.x + f64::from(m.left - a.left) / anchor.scale;
            let spans_y = m.top < a.bottom && m.bottom > a.top;
            let spans_x = m.left < a.right && m.right > a.left;
            if m.left == a.right && spans_y {
                (display.x, display.y) = (anchor.right(), along_y);
            } else if m.right == a.left && spans_y {
                (display.x, display.y) = (anchor.x - display.width, along_y);
            } else if m.top == a.bottom && spans_x {
                (display.x, display.y) = (along_x, anchor.bottom());
            } else if m.bottom == a.top && spans_x {
                (display.x, display.y) = (along_x, anchor.y - display.height);
            } else {
                continue;
            }
            placed[other] = Some(display);
            queue.push(other);
        }
    }
    found
        .iter()
        .zip(placed)
        .map(|(entry, display)| Monitor {
            logical: display.unwrap_or_else(|| natural(entry)),
            physical: entry.0,
        })
        .collect()
}

pub fn logical(monitors: &[Monitor]) -> Vec<Display> {
    monitors.iter().map(|monitor| monitor.logical).collect()
}

/// The physical pixel for a point in logical points (the arrangement's
/// space), through the monitor that contains it. A point outside every
/// monitor is clamped into the nearest one.
pub fn to_physical(monitors: &[Monitor], x: f64, y: f64) -> Option<(i32, i32)> {
    let distance = |monitor: &&Monitor| {
        let l = &monitor.logical;
        let dx = (l.x - x).max(0.0).max(x - l.right());
        let dy = (l.y - y).max(0.0).max(y - l.bottom());
        dx * dx + dy * dy
    };
    let monitor = monitors
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))?;
    let l = &monitor.logical;
    let p = &monitor.physical;
    let px = p.left + ((x - l.x) * l.scale).round() as i32;
    let py = p.top + ((y - l.y) * l.scale).round() as i32;
    Some((px.clamp(p.left, p.right - 1), py.clamp(p.top, p.bottom - 1)))
}

pub fn same_layout(a: &[Monitor], b: &[Monitor]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.logical == b.logical && same_rect(&a.physical, &b.physical))
}

/// A physical pixel in logical points, through the monitor that contains it
/// (or the nearest one).
pub fn to_logical(monitors: &[Monitor], x: i32, y: i32) -> Option<(f64, f64)> {
    let distance = |monitor: &&Monitor| {
        let p = &monitor.physical;
        let dx = i64::from((p.left - x).max(0).max(x - (p.right - 1)));
        let dy = i64::from((p.top - y).max(0).max(y - (p.bottom - 1)));
        dx * dx + dy * dy
    };
    let monitor = monitors.iter().min_by_key(distance)?;
    let l = &monitor.logical;
    let p = &monitor.physical;
    Some((
        l.x + f64::from(x - p.left) / l.scale,
        l.y + f64::from(y - p.top) / l.scale,
    ))
}

/// A portal on this PC's side in physical pixels: the edge `line` (exclusive
/// for right and bottom, like `RECT`) and the range `from..to` along it,
/// with the peer it leads to and the same portal in logical points.
#[derive(Clone, Debug, PartialEq)]
pub struct PhysicalPortal {
    pub peer_id: String,
    pub portal: Portal,
    pub side: Side,
    pub line: i32,
    pub from: i32,
    pub to: i32,
}

pub fn physical_portal(
    monitors: &[Monitor],
    peer_id: &str,
    portal: &Portal,
) -> Option<PhysicalPortal> {
    let monitor = monitors.get(portal.local_display)?;
    let (logical_start, physical_start) = if portal.side.is_vertical() {
        (monitor.logical.y, monitor.physical.top)
    } else {
        (monitor.logical.x, monitor.physical.left)
    };
    let convert = |value: f64| {
        physical_start + ((value - logical_start) * monitor.logical.scale).round() as i32
    };
    let line = match portal.side {
        Side::Left => monitor.physical.left,
        Side::Right => monitor.physical.right,
        Side::Top => monitor.physical.top,
        Side::Bottom => monitor.physical.bottom,
    };
    Some(PhysicalPortal {
        peer_id: peer_id.to_owned(),
        portal: *portal,
        side: portal.side,
        line,
        from: convert(portal.from),
        to: convert(portal.to),
    })
}

/// Calls `changed` with the new monitors whenever they change: right away on
/// `WM_DISPLAYCHANGE` and DPI or setting changes, and on a two-second check
/// that also catches changes some drivers do not announce.
pub fn watch(changed: impl Fn(Vec<Monitor>) + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new()
        .name("rinflow-displays".to_owned())
        .spawn(move || {
            WATCH.with(|watch| {
                *watch.borrow_mut() = Some(Watch {
                    current: monitors(),
                    changed: Box::new(changed),
                })
            });
            let class_name = wide("RinflowDisplayWatch");
            // SAFETY: a hidden top-level window (message-only windows do not
            // receive WM_DISPLAYCHANGE broadcasts) with a timer on this thread.
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
                    return;
                }
                SetTimer(window, 1, 2000, None);
                let mut message: MSG = std::mem::zeroed();
                while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        })?;
    Ok(())
}

struct Watch {
    current: Vec<Monitor>,
    changed: Box<dyn Fn(Vec<Monitor>)>,
}

thread_local! {
    static WATCH: std::cell::RefCell<Option<Watch>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if matches!(
        message,
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE | WM_TIMER
    ) {
        WATCH.with(|watch| {
            if let Ok(mut watch) = watch.try_borrow_mut()
                && let Some(watch) = watch.as_mut()
            {
                let latest = monitors();
                if !latest.is_empty() && !same_layout(&latest, &watch.current) {
                    watch.current = latest.clone();
                    (watch.changed)(latest);
                }
            }
        });
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    #[test]
    fn logical_points_map_through_their_monitor() {
        let monitors = lay_out(&[
            (rect(0, 0, 2880, 1920), 2.0),
            (rect(2880, 0, 1920, 1080), 1.0),
        ]);
        assert_eq!(to_physical(&monitors, 720.0, 480.0), Some((1440, 960)));
        // The 100 % monitor starts where the 200 % one ends in points.
        assert_eq!(to_physical(&monitors, 1540.0, 50.0), Some((2980, 50)));
        // Outside every monitor: clamped into the nearest one.
        assert_eq!(to_physical(&monitors, -50.0, 100.0), Some((0, 200)));
        assert_eq!(to_physical(&[], 0.0, 0.0), None);
        // And back.
        assert_eq!(to_logical(&monitors, 1440, 960), Some((720.0, 480.0)));
        assert_eq!(to_logical(&monitors, 2980, 50), Some((1540.0, 50.0)));
    }

    #[test]
    fn monitors_that_touch_keep_touching_with_mixed_scaling() {
        // A 150 % laptop with a 100 % monitor to its right, one above it
        // (offset), and one to the left of the laptop at 125 %.
        let monitors = lay_out(&[
            (rect(-2000, 200, 2000, 1000), 1.25),
            (rect(0, 0, 1920, 1200), 1.5),
            (rect(1920, 0, 2560, 1440), 1.0),
            (rect(2200, -1080, 1920, 1080), 1.0),
        ]);
        let laptop = monitors[1].logical;
        assert_eq!((laptop.x, laptop.y, laptop.width), (0.0, 0.0, 1280.0));
        // Right of the laptop: no gap.
        let right = monitors[2].logical;
        assert_eq!((right.x, right.y), (laptop.right(), 0.0));
        // Above the right-hand monitor, at its offset along that edge.
        let top = monitors[3].logical;
        assert_eq!(top.bottom(), right.y);
        assert_eq!(top.x, right.x + 280.0);
        // Left of the laptop, 200 px down at the laptop's 150 %.
        let left = monitors[0].logical;
        assert_eq!(left.right(), laptop.x);
        assert!((left.y - 200.0 / 1.5).abs() < 1e-9);
        // A single monitor keeps its plain logical size.
        let single = lay_out(&[(rect(0, 0, 2880, 1920), 2.0)]);
        assert_eq!(single[0].logical.width, 1440.0);
    }
}

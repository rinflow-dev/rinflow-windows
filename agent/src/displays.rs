//! This PC's monitors, in the two coordinate systems the agent needs.
//!
//! The arrangement (linkcursor-core `layout`) works in logical points, which
//! makes sizes comparable across devices. Hooks, `SendInput` and the overlay
//! work in physical pixels. Each monitor keeps both, so a point on a logical
//! edge converts exactly through the monitor it belongs to.

use linkcursor_core::layout::{Display, Portal, Side};
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
    let mut monitors: Vec<Monitor> = handles.into_iter().filter_map(describe).collect();
    monitors.sort_by_key(|monitor| (monitor.physical.left, monitor.physical.top));
    monitors
}

fn describe(handle: HMONITOR) -> Option<Monitor> {
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
        let rect = info.rcMonitor;
        Some(Monitor {
            logical: Display {
                x: rect.left as f64 / scale,
                y: rect.top as f64 / scale,
                width: (rect.right - rect.left) as f64 / scale,
                height: (rect.bottom - rect.top) as f64 / scale,
                scale,
            },
            physical: rect,
        })
    }
}

pub fn logical(monitors: &[Monitor]) -> Vec<Display> {
    monitors.iter().map(|monitor| monitor.logical).collect()
}

pub fn same_layout(a: &[Monitor], b: &[Monitor]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.logical == b.logical && same_rect(&a.physical, &b.physical))
}

/// A portal on this PC's side in physical pixels: the edge `line` (exclusive
/// for right and bottom, like `RECT`) and the range `from..to` along it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalPortal {
    pub side: Side,
    pub line: i32,
    pub from: i32,
    pub to: i32,
}

pub fn physical_portal(monitors: &[Monitor], portal: &Portal) -> Option<PhysicalPortal> {
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
        .name("linkcursor-displays".to_owned())
        .spawn(move || {
            WATCH.with(|watch| {
                *watch.borrow_mut() = Some(Watch {
                    current: monitors(),
                    changed: Box::new(changed),
                })
            });
            let class_name = wide("LinkCursorDisplayWatch");
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

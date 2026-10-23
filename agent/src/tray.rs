//! Notification-area icon. Keeping the tray in the agent means the settings
//! app only runs while one of its windows is open: clicking the icon starts
//! it with `--tray`, which shows its tray panel (devices, the sharing switch,
//! the way to the main window).

use crate::win::wide;
use std::io;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::Console::{CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, AppendMenuW, CreateIconIndirect, CreatePopupMenu,
    CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, ICONINFO, IDI_APPLICATION, KillTimer, LoadIconW,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SW_SHOWNORMAL, SetForegroundWindow, SetTimer,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;
/// Retries adding the icon: Explorer may not take it yet (busy at sign-in,
/// or restarting). The agent works on meanwhile.
const ICON_RETRY_TIMER: usize = 1;
/// Notifications of one kind no more often than this.
const NOTIFY_INTERVAL: Duration = Duration::from_secs(15);

/// Explorer broadcasts this after it restarts; the icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The tray window once created, so [`quit`] works from any thread.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Quits the agent as the tray menu's 退出 does: the icon goes away and the
/// core stops, so peers see a clean disconnect. False before the tray exists.
pub fn quit() -> bool {
    let window = WINDOW.load(Ordering::Acquire);
    if window == 0 {
        return false;
    }
    // SAFETY: posts to this process's tray window; harmless if it is gone.
    unsafe { PostMessageW(window as HWND, WM_CLOSE, 0, 0) != 0 }
}

/// A folder that clicking the notification shown last opens.
static BALLOON_FOLDER: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Shows a notification from the tray icon, at most once per
/// [`NOTIFY_INTERVAL`] for the same `title`. Any thread.
pub fn notify(title: &str, text: &str) {
    if let Ok(mut folder) = BALLOON_FOLDER.lock() {
        *folder = None;
    }
    show_balloon(title, text);
}

/// Like [`notify`]; clicking the notification opens `folder`.
pub fn notify_with_folder(title: &str, text: &str, folder: PathBuf) {
    if let Ok(mut slot) = BALLOON_FOLDER.lock() {
        *slot = Some(folder);
    }
    show_balloon(title, text);
}

fn show_balloon(title: &str, text: &str) {
    static LAST: std::sync::Mutex<Vec<(String, std::time::Instant)>> =
        std::sync::Mutex::new(Vec::new());
    let window = WINDOW.load(Ordering::Acquire);
    if window == 0 {
        return;
    }
    if let Ok(mut last) = LAST.lock() {
        let now = std::time::Instant::now();
        last.retain(|(_, at)| now.duration_since(*at) < NOTIFY_INTERVAL);
        if last.iter().any(|(shown, _)| shown == title) {
            return;
        }
        last.push((title.to_owned(), now));
    }
    let mut data = icon_data(window as HWND);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_INFO;
    for (slot, unit) in data
        .szInfoTitle
        .iter_mut()
        .zip(title.encode_utf16().take(63))
    {
        *slot = unit;
    }
    for (slot, unit) in data.szInfo.iter_mut().zip(text.encode_utf16().take(255)) {
        *slot = unit;
    }
    // SAFETY: `data` names this process's icon; only the balloon changes.
    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// Debug builds run in a console (`make run`, `cargo run`): Ctrl+C and
/// closing the console quit cleanly instead of killing the process, which
/// would leave a dead tray icon and drop peers mid-session.
unsafe extern "system" fn console_control(event: u32) -> windows_sys::core::BOOL {
    if !quit() {
        // Not started yet: let Windows end the process as usual.
        return 0;
    }
    if event == CTRL_CLOSE_EVENT {
        // Windows ends the process once this returns; let shutdown finish.
        std::thread::sleep(Duration::from_secs(3));
    }
    1
}

/// Creates the tray icon and runs the message loop until the user quits.
pub fn run() -> io::Result<()> {
    let class_name = wide("LinkCursorAgentTray");
    let taskbar_created = wide("TaskbarCreated");
    // SAFETY: plain Win32 calls with valid, null-terminated strings. The
    // window is hidden; a top-level window is needed (rather than a
    // message-only one) to receive the TaskbarCreated broadcast.
    unsafe {
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(taskbar_created.as_ptr()),
            Ordering::Relaxed,
        );
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(io::Error::last_os_error());
        }
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPED,
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
        // Explorer may not take the icon yet (busy at sign-in, restarting):
        // keep trying instead of giving up on LinkCursor altogether.
        if !add_icon(window) {
            log!(
                "no tray icon yet ({}); trying again",
                io::Error::last_os_error()
            );
            SetTimer(window, ICON_RETRY_TIMER, 2000, None);
        }
        WINDOW.store(window as isize, Ordering::Release);
        SetConsoleCtrlHandler(Some(console_control), 1);

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => open_app(&["--tray"]),
                WM_RBUTTONUP => show_menu(window),
                NIN_BALLOONUSERCLICK => {
                    let folder = BALLOON_FOLDER.lock().ok().and_then(|mut slot| slot.take());
                    if let Some(folder) = folder {
                        open_folder(&folder);
                    }
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: removes the icon this window added; harmless if absent.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &icon_data(window));
                PostQuitMessage(0);
            }
            0
        }
        WM_TIMER if wparam == ICON_RETRY_TIMER => {
            if add_icon(window) {
                // SAFETY: this window's own timer.
                unsafe { KillTimer(window, ICON_RETRY_TIMER) };
            }
            0
        }
        _ if message == TASKBAR_CREATED.load(Ordering::Relaxed) && message != 0 => {
            if !add_icon(window) {
                // SAFETY: this window's own timer.
                unsafe { SetTimer(window, ICON_RETRY_TIMER, 2000, None) };
            }
            0
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is a plain C struct; all-zero is a valid start.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = TRAY_ID;
    data
}

fn add_icon(window: HWND) -> bool {
    let mut data = icon_data(window);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = tray_icon();
    for (slot, unit) in data.szTip.iter_mut().zip("LinkCursor".encode_utf16()) {
        *slot = unit;
    }
    // SAFETY: `data` is fully initialised for NIM_ADD.
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) != 0 }
}

fn show_menu(window: HWND) {
    let open = wide(crate::i18n::tr("打开主窗口", "Open LinkCursor"));
    let quit = wide(crate::i18n::tr("退出", "Quit"));
    // SAFETY: the menu is created, shown modally and destroyed here.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, MENU_OPEN, open.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());

        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        // Without this the menu does not close when the user clicks elsewhere.
        SetForegroundWindow(window);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            0,
            window,
            null(),
        );
        DestroyMenu(menu);

        match command as usize {
            MENU_OPEN => open_app(&[]),
            MENU_QUIT => {
                DestroyWindow(window);
            }
            _ => {}
        }
    }
}

/// Shows a folder in Explorer, in front (the click allowed that).
pub fn open_folder(folder: &std::path::Path) {
    let path = wide(&folder.to_string_lossy());
    let verb = wide("open");
    // SAFETY: valid NUL-terminated strings; the shell opens the folder.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            path.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Launches the settings app: `LINKCURSOR_APP_PATH`, or `LinkCursor.exe`
/// next to the agent (the layout produced by the app's build). A running app
/// takes the request over (it is single-instance) and shows the main window,
/// or the tray panel with `--tray`.
pub fn open_app(arguments: &[&str]) {
    let path = std::env::var_os("LINKCURSOR_APP_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("LinkCursor.exe")))
        });
    let Some(path) = path else { return };
    // The click gave this process the right to bring a window to the front;
    // pass it on, or the app's window would open behind the others.
    // SAFETY: plain Win32 call.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
    if let Err(error) = std::process::Command::new(&path).args(arguments).spawn() {
        log!("cannot open {}: {error}", path.display());
    }
}

/// The LinkCursor mark at the small-icon size: two overlapping rounded
/// squares, drawn once.
fn tray_icon() -> HICON {
    static ICON: OnceLock<isize> = OnceLock::new();
    let icon = *ICON.get_or_init(|| draw_icon().map_or(0, |icon| icon as isize));
    if icon != 0 {
        return icon as HICON;
    }
    // SAFETY: loads a shared system icon that must not be destroyed.
    unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
}

fn draw_icon() -> Option<HICON> {
    // SAFETY: plain system metric query.
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64) as usize;
    let s = size as f32;
    // Back square (light), front square (deep blue) with a clear gap.
    let back = (0.06 * s, 0.06 * s, 0.66 * s, 0.66 * s, 0.16 * s);
    let front = (0.34 * s, 0.34 * s, 0.94 * s, 0.94 * s, 0.16 * s);
    let gap = 0.07 * s;
    let inside = |(left, top, right, bottom, radius): (f32, f32, f32, f32, f32),
                  x: f32,
                  y: f32,
                  grow: f32| {
        let (left, top, right, bottom) = (left - grow, top - grow, right + grow, bottom + grow);
        let radius = radius + grow;
        let cx = x.clamp(left + radius, right - radius);
        let cy = y.clamp(top + radius, bottom - radius);
        x >= left
            && x <= right
            && y >= top
            && y <= bottom
            && (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    let mut pixels = vec![0u32; size * size];
    const SAMPLES: usize = 4;
    for py in 0..size {
        for px in 0..size {
            let (mut a, mut r, mut g, mut b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let color = if inside(front, x, y, 0.0) {
                        Some((0x0F, 0x6C, 0xBD))
                    } else if inside(back, x, y, 0.0) && !inside(front, x, y, gap) {
                        Some((0x6C, 0xB4, 0xF0))
                    } else {
                        None
                    };
                    if let Some((cr, cg, cb)) = color {
                        a += 1.0;
                        r += f32::from(cr as u8);
                        g += f32::from(cg as u8);
                        b += f32::from(cb as u8);
                    }
                }
            }
            if a > 0.0 {
                let coverage = a / (SAMPLES * SAMPLES) as f32;
                // Premultiplied BGRA.
                let alpha = (coverage * 255.0).round() as u32;
                let channel = |sum: f32| ((sum / a) * coverage).round() as u32;
                pixels[py * size + px] =
                    alpha << 24 | channel(r) << 16 | channel(g) << 8 | channel(b);
            }
        }
    }
    // SAFETY: creates a 32-bit color bitmap and an all-zero mask from
    // buffers of the right size, builds the icon and frees the bitmaps.
    unsafe {
        let color = CreateBitmap(size as i32, size as i32, 1, 32, pixels.as_ptr().cast());
        let mask_bits = vec![0u8; size.div_ceil(16) * 2 * size];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, mask_bits.as_ptr().cast());
        if color.is_null() || mask.is_null() {
            return None;
        }
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        (!icon.is_null()).then_some(icon)
    }
}

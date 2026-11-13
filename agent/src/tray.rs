// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Notification-area icon. Keeping the tray in the agent means the settings
//! app only runs while one of its windows is open: clicking the icon starts
//! it with `--tray`, which shows its tray panel (devices, the sharing switch,
//! the way to the main window).

use crate::i18n::{s, tr, trf};
use crate::win::wide;
use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::Console::{CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Controls::{LIM_SMALL, LoadIconMetric};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, AppendMenuW, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, HICON, KillTimer, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_ENDSESSION, WM_LBUTTONUP, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_THEMECHANGED,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;
const MENU_INSTALL_UPDATE: usize = 3;
/// The remote desktop of each peer with a picture, from this one on.
const MENU_VIEWER_FIRST: usize = 100;
/// Retries adding the icon: Explorer may not take it yet (busy at sign-in,
/// or restarting). The agent works on meanwhile.
const ICON_RETRY_TIMER: usize = 1;
/// Notifications of one kind no more often than this.
const NOTIFY_INTERVAL: Duration = Duration::from_secs(15);

/// Explorer broadcasts this after it restarts; the icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The tray window once created, so [`quit`] works from any thread.
static WINDOW: AtomicIsize = AtomicIsize::new(0);
// Owned by the tray thread. Retain until Explorer receives a replacement.
static ICON: AtomicIsize = AtomicIsize::new(0);

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

/// Shows a notification from the tray icon, at most once per
/// [`NOTIFY_INTERVAL`] for the same `title`. Any thread.
pub fn notify(title: &str, text: &str) {
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

thread_local! {
    /// What stops the agent, until it ran: after the message loop, or
    /// inside `WM_ENDSESSION`, where Windows waits for it.
    static SHUTDOWN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Runs what stops the agent, once.
fn shut_down() {
    if let Some(shutdown) = SHUTDOWN.with_borrow_mut(Option::take) {
        shutdown();
    }
}

/// Creates the tray icon and runs the message loop until the user quits,
/// then runs `shutdown`. When the user signs out or Windows shuts down,
/// `shutdown` runs while Windows still waits for this program, so peers see
/// a clean disconnect then too (a device driving this PC gets control back
/// at once, not after a timeout).
pub fn run(shutdown: impl FnOnce() + 'static) -> io::Result<()> {
    SHUTDOWN.set(Some(Box::new(shutdown)));
    let result = run_loop();
    shut_down();
    result
}

fn run_loop() -> io::Result<()> {
    let class_name = wide("RinflowAgentTray");
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
        // keep trying instead of giving up on Rinflow altogether.
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
                // The notification that an update is ready: the settings
                // page, where it is installed.
                NIN_BALLOONUSERCLICK if crate::update::ready_version().is_some() => {
                    open_app(&["--settings"])
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: removes the icon this window added; harmless if absent.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &icon_data(window));
                let icon = ICON.swap(0, Ordering::Relaxed) as HICON;
                if !icon.is_null() {
                    DestroyIcon(icon);
                }
                PostQuitMessage(0);
            }
            0
        }
        // Signing out, restarting or shutting down. Windows may end the
        // process as soon as this returns, so the agent stops here, not
        // after the message loop.
        WM_ENDSESSION if wparam != 0 => {
            log!("the Windows session is ending: stopping");
            shut_down();
            // SAFETY: this thread's own window; ends the message loop.
            unsafe { DestroyWindow(window) };
            0
        }
        WM_TIMER if wparam == ICON_RETRY_TIMER => {
            if add_icon(window) {
                // SAFETY: this window's own timer.
                unsafe { KillTimer(window, ICON_RETRY_TIMER) };
            }
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_DISPLAYCHANGE => {
            update_icon(window, NIM_MODIFY);
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
    update_icon(window, NIM_ADD)
}

fn update_icon(window: HWND, operation: u32) -> bool {
    let mut data = icon_data(window);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = load_tray_icon();
    if data.hIcon.is_null() {
        return false;
    }
    for (slot, unit) in data.szTip.iter_mut().zip("Rinflow".encode_utf16()) {
        *slot = unit;
    }
    // SAFETY: `data` is fully initialised; both icons are owned by this thread.
    unsafe {
        let updated = Shell_NotifyIconW(operation, &data) != 0;
        if updated {
            let old = ICON.swap(data.hIcon as isize, Ordering::Relaxed) as HICON;
            if !old.is_null() {
                DestroyIcon(old);
            }
        } else {
            DestroyIcon(data.hIcon);
        }
        updated
    }
}

fn load_tray_icon() -> HICON {
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut light = 1u32;
    let mut bytes = size_of::<u32>() as u32;
    let mut icon = null_mut();
    // SAFETY: the registry buffer is a DWORD, names are terminated, and
    // resources 101/102 are embedded by build.rs. LoadIconMetric selects the
    // small-icon DPI size from the multi-resolution group, rather than 32px.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut light as *mut u32).cast(),
            &mut bytes,
        );
        let resource = if light == 0 { 102 } else { 101 };
        LoadIconMetric(
            GetModuleHandleW(null()),
            resource as *const u16,
            LIM_SMALL,
            &mut icon,
        );
    }
    icon
}

fn show_menu(window: HWND) {
    let open = wide(tr(s::TrayOpen));
    let quit = wide(tr(s::TrayQuit));
    let install =
        crate::update::ready_version().map(|version| wide(&trf(s::TrayInstallUpdate, &[&version])));
    // Peers with a picture (KVMs): a remote desktop window each.
    let viewable = crate::viewer::global()
        .map(crate::viewer::Viewers::available)
        .unwrap_or_default();
    let viewer_items: Vec<Vec<u16>> = viewable
        .iter()
        .map(|(_, name)| wide(&trf(s::TrayRemoteDesktop, &[name])))
        .collect();
    // SAFETY: the menu is created, shown modally and destroyed here.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, MENU_OPEN, open.as_ptr());
        for (index, item) in viewer_items.iter().enumerate() {
            AppendMenuW(menu, MF_STRING, MENU_VIEWER_FIRST + index, item.as_ptr());
        }
        if let Some(install) = &install {
            AppendMenuW(menu, MF_STRING, MENU_INSTALL_UPDATE, install.as_ptr());
        }
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
            MENU_INSTALL_UPDATE => crate::update::install(),
            MENU_QUIT => {
                DestroyWindow(window);
            }
            command if command >= MENU_VIEWER_FIRST => {
                if let Some((peer_id, _)) = viewable.get(command - MENU_VIEWER_FIRST)
                    && let Some(viewers) = crate::viewer::global()
                    && let Err(error) = viewers.open(peer_id)
                {
                    log!("cannot open the remote desktop: {error}");
                }
            }
            _ => {}
        }
    }
}

/// Launches the settings app: `RINFLOW_APP_PATH`, or `Rinflow.exe`
/// next to the agent (the layout produced by the app's build). A running app
/// takes the request over (it is single-instance) and shows the main window,
/// or the tray panel with `--tray`.
pub fn open_app(arguments: &[&str]) {
    let path = std::env::var_os("RINFLOW_APP_PATH")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("Rinflow.exe")))
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

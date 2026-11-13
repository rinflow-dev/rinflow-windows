// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A peer's picture on this PC's screen, in one window per peer with two
//! ways of showing it:
//!
//! - **Full screen**, while this PC drives the peer through a screen edge
//!   (`capture.rs`): over the whole monitor the pointer left from, on top
//!   of everything, never taking the focus. The capture has the keyboard
//!   and mouse as for any peer; the picture is where the peer's own screen
//!   would be. The peer's cursor is in the picture.
//! - **A window** the user opened to watch the peer (the device's menu or
//!   the tray). It shows the picture only; driving is through the edge.
//!
//! A window that is in neither way hides, and closes after
//! [`LINGER`]: going back to the peer within that time shows its picture
//! at once, without waiting for the link and a key frame.
//!
//! Each window has a thread of its own. Frames arrive from the core's
//! threads in [`Shared`], with a message to wake the window; the window
//! decodes them and draws the newest. The title says how the picture is
//! doing.

use super::codec::{self, Codec};
use super::decoder::{self, Decoder, Picture};
use super::geometry::Screen;
use super::render::Renderer;
use crate::i18n::{s, trf};
use rinflow_core::{Core, MediaFrame};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW,
    GetWindowPlacement, HICON, HTCLIENT, HWND_NOTOPMOST, HWND_TOPMOST, ICON_BIG, ICON_SMALL,
    IDC_ARROW, IMAGE_ICON, IsIconic, KillTimer, LR_DEFAULTSIZE, LoadCursorW, LoadImageW,
    MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SPI_GETWORKAREA, SW_HIDE,
    SW_RESTORE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW, SetCursor, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, SetWindowTextW, ShowCursor, ShowWindow,
    SystemParametersInfoW, WINDOWPLACEMENT, WM_APP, WM_DESTROY, WM_ERASEBKGND, WM_MOUSEACTIVATE,
    WM_NCCREATE, WM_PAINT, WM_SETCURSOR, WM_SETICON, WM_SIZE, WM_TIMER, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_OVERLAPPEDWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, PCWSTR, w};

/// New frames are waiting in [`Shared`].
const WM_FRAMES: u32 = WM_APP + 1;
/// The link changed ([`Shared::set_link`]).
const WM_LINK: u32 = WM_APP + 2;
/// How the picture is shown changed ([`Shared::set_fullscreen`],
/// [`Shared::show_window`]).
const WM_MODE: u32 = WM_APP + 3;
/// Close, from another thread.
const WM_QUIT_VIEWER: u32 = WM_APP + 4;
const STATS_TIMER: usize = 1;
const LINGER_TIMER: usize = 2;
/// How long a hidden picture stays linked before its window closes.
pub const LINGER: Duration = Duration::from_secs(30);
/// Frames waiting beyond this mean the window has stalled (dragged by its
/// title, say): they are dropped and the picture starts again from a key
/// frame, so it does not lag behind for good.
const MAX_WAITING_FRAMES: usize = 90;
/// The window at first: a picture this large in logical points, or less to
/// fit the screen.
const INITIAL_SIZE: (i32, i32) = (1280, 720);
const WINDOW_STYLE: u32 = WS_OVERLAPPEDWINDOW.0;
const WINDOW_EX_STYLE: u32 = WS_EX_NOREDIRECTIONBITMAP.0;
const FULLSCREEN_STYLE: u32 = WS_POPUP.0;
const FULLSCREEN_EX_STYLE: u32 =
    WS_EX_NOREDIRECTIONBITMAP.0 | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;

/// Where the media link to the peer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Not watching yet: the peer has not offered its picture, or the link
    /// is being made.
    Connecting,
    /// Watching: frames come.
    Started,
    /// The link ended; the window waits for the peer to offer it again.
    Ended,
}

/// A monitor's rectangle in physical pixels: left, top, right, bottom.
pub type MonitorRect = (i32, i32, i32, i32);

/// What the window shares with the rest of the agent.
pub struct Shared {
    window: AtomicIsize,
    frames: Mutex<VecDeque<MediaFrame>>,
    link: Mutex<Link>,
    /// The peer's screen in its points, once it has described it.
    pub screen: Mutex<Option<Screen>>,
    /// Shown full screen on this monitor: this PC drives the peer.
    fullscreen: Mutex<Option<MonitorRect>>,
    /// The window is gone (or going): a new one is needed.
    closed: AtomicBool,
    /// The user opened the window to watch the peer.
    windowed: AtomicBool,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            window: AtomicIsize::new(0),
            frames: Mutex::new(VecDeque::new()),
            link: Mutex::new(Link::Connecting),
            screen: Mutex::new(None),
            fullscreen: Mutex::new(None),
            windowed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    fn post(&self, message: u32) {
        let window = self.window.load(Ordering::Acquire);
        if window != 0 {
            // SAFETY: posting to a window that lives until it clears `window`.
            unsafe {
                let _ = PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// A frame from the core. False: too many were waiting, all were
    /// dropped, and the picture needs a key frame.
    pub fn push(&self, frame: MediaFrame) -> bool {
        let Ok(mut frames) = self.frames.lock() else {
            return true;
        };
        if frames.len() >= MAX_WAITING_FRAMES {
            frames.clear();
            return false;
        }
        let wake = frames.is_empty();
        frames.push_back(frame);
        drop(frames);
        if wake {
            self.post(WM_FRAMES);
        }
        true
    }

    pub fn set_link(&self, link: Link) {
        if let Ok(mut current) = self.link.lock() {
            *current = link;
        }
        self.post(WM_LINK);
    }

    pub fn link(&self) -> Link {
        self.link.lock().map(|link| *link).unwrap_or(Link::Ended)
    }

    /// Full screen on `monitor` while this PC drives the peer, or `None`
    /// when it no longer does.
    pub fn set_fullscreen(&self, monitor: Option<MonitorRect>) {
        if let Ok(mut current) = self.fullscreen.lock() {
            *current = monitor;
        }
        self.post(WM_MODE);
    }

    fn fullscreen(&self) -> Option<MonitorRect> {
        self.fullscreen.lock().ok().and_then(|current| *current)
    }

    /// The user asks to watch the peer: the window shows (and comes to the
    /// front, unless the picture is full screen now).
    pub fn show_window(&self) {
        self.windowed.store(true, Ordering::Release);
        self.post(WM_MODE);
    }

    /// The window shows the picture in neither way: it is hidden.
    pub fn idle(&self) -> bool {
        !self.windowed.load(Ordering::Acquire) && self.fullscreen().is_none()
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn close(&self) {
        self.post(WM_QUIT_VIEWER);
    }
}

/// What the picture is doing, for the title.
enum Status {
    Link(Link),
    Playing,
    NoDecoder(Codec),
    Failed(String),
}

struct Stats {
    since: Instant,
    frames: u32,
    bytes: u64,
    /// Frames a second and Mbit/s over the last second.
    rate: (f64, f64),
}

/// How the window shows the picture now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Hidden,
    Window,
    Fullscreen(MonitorRect),
}

struct Viewer {
    window: HWND,
    peer_id: String,
    name: String,
    shared: Arc<Shared>,
    core: Weak<Core>,
    renderer: Option<Renderer>,
    decoder: Option<Decoder>,
    /// The newest picture, drawn again when the window changes size.
    picture: Option<Picture>,
    status: Status,
    stats: Stats,
    mode: Mode,
    /// Where the window was before it went full screen.
    placement: Option<WINDOWPLACEMENT>,
}

/// Opens the window on a thread of its own. `closed` runs on that thread
/// once the window is gone.
pub fn spawn(
    peer_id: String,
    name: String,
    shared: Arc<Shared>,
    core: Weak<Core>,
    closed: impl FnOnce() + Send + 'static,
) {
    let result = std::thread::Builder::new()
        .name("viewer".to_owned())
        .spawn(move || {
            // SAFETY: COM for Media Foundation and Direct3D on this thread,
            // undone when it ends.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            if let Err(error) = run(peer_id, name, shared.clone(), core) {
                log!("viewer: cannot open the window: {error}");
            }
            shared.closed.store(true, Ordering::Release);
            shared.window.store(0, Ordering::Release);
            closed();
            if com {
                // SAFETY: matches the initialization above.
                unsafe { CoUninitialize() };
            }
        });
    if let Err(error) = result {
        log!("viewer: cannot start its thread: {error}");
    }
}

/// The window's size as a window at first: [`INITIAL_SIZE`], or less to
/// leave some of the work area free, in the same shape.
fn initial_size() -> (i32, i32) {
    // SAFETY: plain system queries.
    unsafe {
        let dpi = GetDpiForSystem();
        let scale = f64::from(dpi) / 96.0;
        let mut work = RECT::default();
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let (mut width, mut height) = (
            f64::from(INITIAL_SIZE.0) * scale,
            f64::from(INITIAL_SIZE.1) * scale,
        );
        let fit = (f64::from(work.right - work.left) * 0.9 / width)
            .min(f64::from(work.bottom - work.top) * 0.85 / height)
            .min(1.0);
        if fit > 0.0 {
            width *= fit;
            height *= fit;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let _ = AdjustWindowRectExForDpi(
            &mut rect,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WINDOW_STYLE),
            false,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(WINDOW_EX_STYLE),
            dpi,
        );
        (rect.right - rect.left, rect.bottom - rect.top)
    }
}

fn run(
    peer_id: String,
    name: String,
    shared: Arc<Shared>,
    core: Weak<Core>,
) -> windows::core::Result<()> {
    // SAFETY: registers the class and creates the window on this thread;
    // the viewer lives in the window's user data until WM_DESTROY.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("RinflowViewer");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        // A second window finds the class registered already.
        RegisterClassW(&class);

        let viewer = Box::new(Viewer {
            window: HWND::default(),
            peer_id,
            name: name.clone(),
            shared: shared.clone(),
            core,
            renderer: None,
            decoder: None,
            picture: None,
            status: Status::Link(shared.link()),
            stats: Stats {
                since: Instant::now(),
                frames: 0,
                bytes: 0,
                rate: (0.0, 0.0),
            },
            mode: Mode::Hidden,
            placement: None,
        });
        let (width, height) = initial_size();
        let title = HSTRING::from(trf(s::ViewerConnecting, &[&name]));
        // Hidden until WM_MODE says how to show it.
        let window = CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(WINDOW_EX_STYLE),
            class_name,
            &title,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(WINDOW_STYLE),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            Some(Box::into_raw(viewer) as *const _),
        )?;
        set_icons(window, instance.into());
        shared.window.store(window.0 as isize, Ordering::Release);
        SetTimer(Some(window), STATS_TIMER, 1000, None);
        let _ = PostMessageW(Some(window), WM_MODE, WPARAM(0), LPARAM(0));
        // Frames that came before the window was there.
        let _ = PostMessageW(Some(window), WM_FRAMES, WPARAM(0), LPARAM(0));

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// Rinflow's icon in the title bar and the taskbar (resource 101, embedded
/// by build.rs).
fn set_icons(window: HWND, instance: HINSTANCE) {
    // SAFETY: loads a shared icon resource of this executable.
    unsafe {
        let dpi = GetDpiForSystem() as i32;
        for (kind, size) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
            if let Ok(icon) = LoadImageW(
                Some(instance),
                PCWSTR(101 as *const u16),
                IMAGE_ICON,
                size * dpi / 96,
                size * dpi / 96,
                LR_DEFAULTSIZE,
            ) {
                SendMessageW(
                    window,
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(HICON(icon.0).0 as isize)),
                );
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the viewer was put in the user data at WM_NCCREATE and is
    // taken out at WM_DESTROY; messages come on this thread only.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let viewer = create.lpCreateParams as *mut Viewer;
            (*viewer).window = window;
            SetWindowLongPtrW(window, GWLP_USERDATA, viewer as isize);
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let viewer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Viewer;
        if viewer.is_null() {
            return DefWindowProcW(window, message, wparam, lparam);
        }
        if message == WM_DESTROY {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            let viewer = Box::from_raw(viewer);
            // From now on a new window is made for the peer.
            viewer.shared.closed.store(true, Ordering::Release);
            if matches!(viewer.mode, Mode::Fullscreen(_)) {
                ShowCursor(true);
            }
            drop(viewer);
            PostQuitMessage(0);
            return LRESULT(0);
        }
        match (*viewer).handle(message, wparam, lparam) {
            Some(result) => result,
            None => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

impl Viewer {
    /// Handles a message; `None` leaves it to Windows.
    fn handle(&mut self, message: u32, wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
        match message {
            WM_FRAMES => self.frames(),
            WM_LINK => {
                let link = self.shared.link();
                if link != Link::Started || !matches!(self.status, Status::Playing) {
                    self.set_status(Status::Link(link));
                }
                if link == Link::Started
                    && let Some(decoder) = &mut self.decoder
                {
                    // A new link starts at a key frame.
                    decoder.restart();
                }
            }
            WM_MODE => self.apply_mode(),
            WM_QUIT_VIEWER => {
                // SAFETY: destroys this thread's window.
                unsafe {
                    let _ = DestroyWindow(self.window);
                }
            }
            WM_TIMER if wparam.0 == STATS_TIMER => self.tick(),
            WM_TIMER if wparam.0 == LINGER_TIMER => {
                // SAFETY: plain window calls on this window.
                unsafe {
                    let _ = KillTimer(Some(self.window), LINGER_TIMER);
                    if self.shared.idle() {
                        let _ = DestroyWindow(self.window);
                    }
                }
            }
            WM_SIZE => {
                let size = self.client_size();
                if size.0 > 0
                    && size.1 > 0
                    && let Some(renderer) = &mut self.renderer
                {
                    if let Err(error) = renderer.resize(size) {
                        log!("viewer: cannot resize: {error}");
                    }
                    self.redraw();
                }
                return Some(LRESULT(0));
            }
            WM_PAINT => {
                self.redraw();
                // SAFETY: the window is drawn.
                unsafe {
                    let _ = ValidateRect(Some(self.window), None);
                }
                return Some(LRESULT(0));
            }
            WM_ERASEBKGND => return Some(LRESULT(1)),
            // Full screen, the picture never takes the focus: the window in
            // front keeps it, and the capture has the input anyway.
            WM_MOUSEACTIVATE if matches!(self.mode, Mode::Fullscreen(_)) => {
                return Some(LRESULT(MA_NOACTIVATE as isize));
            }
            WM_SETCURSOR
                if matches!(self.mode, Mode::Fullscreen(_))
                    && (_lparam.0 & 0xFFFF) as u32 == HTCLIENT =>
            {
                // The peer's cursor is in the picture.
                // SAFETY: hides the cursor while it is over this window.
                unsafe { SetCursor(None) };
                return Some(LRESULT(1));
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Shows the window as [`Shared`] says now: full screen while this PC
    /// drives the peer, else as a window if the user opened one, else
    /// hidden (and closed after [`LINGER`]).
    fn apply_mode(&mut self) {
        let mode = match self.shared.fullscreen() {
            Some(monitor) => Mode::Fullscreen(monitor),
            None if self.shared.windowed.load(Ordering::Acquire) => Mode::Window,
            None => Mode::Hidden,
        };
        if mode == self.mode {
            return;
        }
        let window = self.window;
        // SAFETY: plain window calls on this thread's window.
        unsafe {
            if mode == Mode::Hidden {
                SetTimer(Some(window), LINGER_TIMER, LINGER.as_millis() as u32, None);
            } else {
                let _ = KillTimer(Some(window), LINGER_TIMER);
            }
            match mode {
                Mode::Fullscreen((left, top, right, bottom)) => {
                    if self.mode == Mode::Window {
                        let mut placement = WINDOWPLACEMENT {
                            length: size_of::<WINDOWPLACEMENT>() as u32,
                            ..Default::default()
                        };
                        if GetWindowPlacement(window, &mut placement).is_ok() {
                            self.placement = Some(placement);
                        }
                    }
                    if !matches!(self.mode, Mode::Fullscreen(_)) {
                        // This thread's count below zero: no cursor over it,
                        // whatever another program sets.
                        ShowCursor(false);
                        SetWindowLongPtrW(window, GWL_STYLE, FULLSCREEN_STYLE as isize);
                        SetWindowLongPtrW(window, GWL_EXSTYLE, FULLSCREEN_EX_STYLE as isize);
                    }
                    let _ = SetWindowPos(
                        window,
                        Some(HWND_TOPMOST),
                        left,
                        top,
                        right - left,
                        bottom - top,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
                    );
                }
                Mode::Window | Mode::Hidden => {
                    if matches!(self.mode, Mode::Fullscreen(_)) {
                        ShowCursor(true);
                        SetWindowLongPtrW(window, GWL_STYLE, WINDOW_STYLE as isize);
                        SetWindowLongPtrW(window, GWL_EXSTYLE, WINDOW_EX_STYLE as isize);
                        let _ = SetWindowPos(
                            window,
                            Some(HWND_NOTOPMOST),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                        );
                        match &self.placement {
                            Some(placement) => {
                                let _ = SetWindowPlacement(window, placement);
                            }
                            None if mode == Mode::Window => {
                                // Never a window before: the first size.
                                let (width, height) = initial_size();
                                let _ = SetWindowPos(
                                    window,
                                    None,
                                    left_of(width),
                                    top_of(height),
                                    width,
                                    height,
                                    SWP_NOACTIVATE,
                                );
                            }
                            None => {}
                        }
                    }
                    if mode == Mode::Window {
                        let _ = ShowWindow(window, SW_SHOW);
                        if IsIconic(window).as_bool() {
                            let _ = ShowWindow(window, SW_RESTORE);
                        }
                        // Opened by the user: to the front.
                        let _ = SetForegroundWindow(window);
                    } else {
                        let _ = ShowWindow(window, SW_HIDE);
                    }
                }
            }
        }
        self.mode = mode;
        self.update_title();
    }

    fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        // SAFETY: plain window query.
        unsafe {
            let _ = GetClientRect(self.window, &mut rect);
        }
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        )
    }

    fn renderer(&mut self) -> Option<&mut Renderer> {
        if self.renderer.is_none() {
            match Renderer::new(self.window, self.client_size()) {
                Ok(renderer) => self.renderer = Some(renderer),
                Err(error) => {
                    log!("viewer: cannot draw: {error}");
                    self.set_status(Status::Failed(error.message()));
                    return None;
                }
            }
        }
        self.renderer.as_mut()
    }

    fn redraw(&mut self) {
        let picture = self.picture.take();
        if let Some(renderer) = self.renderer() {
            let result = match &picture {
                Some(picture) => renderer.draw(picture),
                None => renderer.clear(),
            };
            if let Err(error) = result {
                log!("viewer: cannot draw: {error}");
            }
        }
        self.picture = picture;
    }

    /// Decodes the frames waiting and draws the newest picture.
    fn frames(&mut self) {
        let frames: Vec<MediaFrame> = match self.shared.frames.lock() {
            Ok(mut frames) => frames.drain(..).collect(),
            Err(_) => return,
        };
        if frames.is_empty() || self.renderer().is_none() {
            return;
        }
        let mut newest = None;
        for frame in frames {
            self.stats.frames += 1;
            self.stats.bytes += frame.data.len() as u64;
            if let Some(picture) = self.decode(&frame) {
                newest = Some(picture);
            }
        }
        if let Some(picture) = newest {
            if !matches!(self.status, Status::Playing) {
                self.set_status(Status::Playing);
            }
            // The one before goes back to the decoder first.
            self.picture = None;
            self.picture = Some(picture);
            self.redraw();
        }
    }

    fn decode(&mut self, frame: &MediaFrame) -> Option<Picture> {
        if self.decoder.is_none() {
            if !frame.key_frame {
                return None;
            }
            let codec = codec::detect(&frame.data)?;
            if let Status::NoDecoder(missing) = self.status
                && missing == codec
            {
                return None;
            }
            let device = self.renderer.as_ref()?.device.clone();
            let size = self
                .shared
                .screen
                .lock()
                .ok()
                .and_then(|screen| *screen)
                .map_or((1920, 1080), |screen| {
                    (screen.width as u32, screen.height as u32)
                });
            match Decoder::new(codec, &device, size) {
                Ok(decoder) => {
                    log!("viewer: {} with {}", codec.name(), decoder.name);
                    self.decoder = Some(decoder);
                }
                Err(error) => {
                    log!("viewer: no {} decoder: {error}", codec.name());
                    self.set_status(if decoder::available(codec) {
                        Status::Failed(error.message())
                    } else {
                        Status::NoDecoder(codec)
                    });
                    return None;
                }
            }
        }
        let decoder = self.decoder.as_mut()?;
        match decoder.decode(&frame.data, frame.key_frame, frame.timestamp_micros) {
            Ok(picture) => picture,
            Err(error) => {
                log!("viewer: a frame did not decode: {error}");
                decoder.restart();
                if let Some(core) = self.core.upgrade() {
                    let _ = core.request_media_key_frame(&self.peer_id);
                }
                None
            }
        }
    }

    /// Once a second: the title's numbers.
    fn tick(&mut self) {
        let seconds = self.stats.since.elapsed().as_secs_f64().max(0.001);
        self.stats.rate = (
            f64::from(self.stats.frames) / seconds,
            self.stats.bytes as f64 * 8.0 / seconds / 1_000_000.0,
        );
        self.stats.since = Instant::now();
        self.stats.frames = 0;
        self.stats.bytes = 0;
        if matches!(self.status, Status::Playing) {
            self.update_title();
        }
    }

    fn set_status(&mut self, status: Status) {
        self.status = status;
        self.update_title();
    }

    fn update_title(&self) {
        if self.mode != Mode::Window {
            return;
        }
        let name: &dyn std::fmt::Display = &self.name;
        let title = match &self.status {
            Status::Link(Link::Connecting) => trf(s::ViewerConnecting, &[name]),
            Status::Link(Link::Started) => trf(s::ViewerWaiting, &[name]),
            Status::Link(Link::Ended) => trf(s::ViewerEnded, &[name]),
            Status::Playing => {
                let (width, height) = self.decoder.as_ref().map(Decoder::size).unwrap_or_default();
                let codec = self
                    .decoder
                    .as_ref()
                    .map_or("", |decoder| decoder.codec.name());
                trf(
                    s::ViewerPlaying,
                    &[
                        name,
                        &format!("{width}×{height}"),
                        &codec,
                        &format!("{:.0}", self.stats.rate.0),
                        &format!("{:.1}", self.stats.rate.1),
                    ],
                )
            }
            Status::NoDecoder(codec) => trf(s::ViewerNoDecoder, &[name, &codec.name()]),
            Status::Failed(reason) => trf(s::ViewerFailed, &[name, reason]),
        };
        // SAFETY: sets this window's title.
        unsafe {
            let _ = SetWindowTextW(self.window, &HSTRING::from(title));
        }
    }
}

/// Where a window of this width goes to sit in the middle of the work area.
fn left_of(width: i32) -> i32 {
    work_area().map_or(CW_USEDEFAULT, |work| {
        work.left + (work.right - work.left - width).max(0) / 2
    })
}

fn top_of(height: i32) -> i32 {
    work_area().map_or(CW_USEDEFAULT, |work| {
        work.top + (work.bottom - work.top - height).max(0) / 2
    })
}

fn work_area() -> Option<RECT> {
    let mut work = RECT::default();
    // SAFETY: fills `work`.
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some((&mut work as *mut RECT).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    Some(work)
}

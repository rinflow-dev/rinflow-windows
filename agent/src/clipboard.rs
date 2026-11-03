// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The Windows clipboard, shared with peers through the core.
//!
//! The core moves clipboards between devices as control moves (see
//! rinflow-core/docs/clipboard-and-files.md). This module is the Windows
//! end: it reports every change of the system clipboard and writes what
//! peers send. Text, HTML and images travel as UTF-8 text, a UTF-8 HTML
//! fragment and PNG. Contents a password manager marks as private are never
//! shared.
//!
//! Files copied in Explorer (`CF_HDROP`) are reported as a `text/uri-list`;
//! the core turns them into a manifest and the peer pulls them. Files a peer
//! copied are published as OLE virtual files and read on demand. A peer
//! without file streams (a build older than them) offers files the old way:
//! that offer is declined, and the user is told to update that device.

mod virtual_files;

use crate::i18n::{s, tr, trf};
use crate::win::wide;
use rinflow_core::{Clipboard, ClipboardFormat, Core, MIME_HTML, MIME_PNG, MIME_TEXT};
use serde_json::Value;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{GlobalFree, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    KillTimer, MSG, PostMessageW, PostThreadMessageW, RegisterClassW, SetTimer, TranslateMessage,
    WM_APP, WM_CLIPBOARDUPDATE, WM_QUIT, WM_TIMER, WNDCLASSW,
};

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;
/// Copied files, as `file://` URLs one per line (the core's MIME type).
const MIME_URI_LIST: &str = "text/uri-list";
/// A peer's clipboard is waiting in `Shared::incoming`.
const WM_WRITE_CLIPBOARD: u32 = WM_APP + 1;
/// Sharing was turned on or off: report the clipboard (or nothing) again.
const WM_REPORT_CLIPBOARD: u32 = WM_APP + 2;
/// Largest image converted between PNG and DIB, in pixels.
const MAX_PIXELS: usize = 50_000_000;
/// What the core shares at most (`MAX_CLIPBOARD_SIZE`): larger contents
/// lose their image, then their HTML, so the text at least goes.
const SHARE_LIMIT: usize = 8 * 1024 * 1024 - 4096;
/// Timers on the clipboard window: try again when another program held the
/// clipboard too long, a few times.
const REPORT_RETRY_TIMER: usize = 1;
const WRITE_RETRY_TIMER: usize = 2;
const RETRY_DELAY_MS: u32 = 300;
const RETRY_ATTEMPTS: u32 = 5;

/// Shared by the clipboard thread, the core's event callback and IPC.
#[derive(Clone, Default)]
pub struct ClipboardHandle(Arc<Shared>);

#[derive(Default)]
struct Shared {
    enabled: AtomicBool,
    /// The clipboard thread's window; 0 until it runs.
    window: AtomicIsize,
    incoming: Mutex<Option<Incoming>>,
    /// The user was told a peer is too old to send files (once a run).
    outdated_told: AtomicBool,
    /// Set when the thread starts, for decisions on offered files.
    core: OnceLock<Weak<Core>>,
}

enum IncomingContent {
    Clipboard(Clipboard),
    Files {
        manifest: rinflow_core::FileManifest,
        peer: String,
    },
    Unavailable(String),
}
struct Incoming {
    sequence: u32,
    content: IncomingContent,
}

/// Why copied files are not shared (the core's `files_not_shared`: past
/// its limits), and what to do instead.
pub fn files_not_shared_text(event: &Value) -> String {
    let limit = event["limit"].as_u64().unwrap_or(0);
    let message = if event["reason"].as_str() == Some("too_deep") {
        s::FilesTooDeep
    } else {
        s::FilesTooMany
    };
    trf(message, &[&limit])
}

/// The core's download folder, Downloads\Rinflow. Files are not
/// downloaded any more (peers' files are pulled on demand), but the core
/// still plans an old-protocol offer there before it is declined.
pub fn download_directory() -> PathBuf {
    let mut path: windows_sys::core::PWSTR = null_mut();
    // SAFETY: asks for the user's Downloads folder; the string the shell
    // allocates is copied and freed.
    let downloads = unsafe {
        let found = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, null_mut(), &mut path) == 0
            && !path.is_null();
        let text = found.then(|| {
            let length = (0..).take_while(|&i| *path.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(path, length))
        });
        if !path.is_null() {
            CoTaskMemFree(path as *const _);
        }
        text
    };
    downloads
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Downloads"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Rinflow")
}

impl ClipboardHandle {
    pub fn new(enabled: bool) -> Self {
        let handle = Self::default();
        handle.0.enabled.store(enabled, Ordering::SeqCst);
        handle
    }

    pub fn enabled(&self) -> bool {
        self.0.enabled.load(Ordering::SeqCst)
    }

    /// The core, as soon as it runs: offered files are decided from its
    /// first events on, also if the clipboard thread could not start.
    pub fn set_core(&self, core: Weak<Core>) {
        let _ = self.0.core.set(core);
    }

    /// Turns sharing on or off. Turned on, the current clipboard is reported
    /// at once; turned off, the core forgets it and nothing is sent.
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::SeqCst);
        if !enabled && let Some(core) = self.0.core.get().and_then(Weak::upgrade) {
            let _ = core.revoke_file_streams();
        }
        self.post(WM_REPORT_CLIPBOARD);
    }

    /// Picks peers' clipboards and file offers out of the core's events.
    pub fn core_event(&self, json: &str) {
        if !(json.contains("\"clipboard\"")
            || json.contains("\"files_offered\"")
            || json.contains("\"files_available\"")
            || json.contains("\"files_unavailable\"")
            || json.contains("\"files_not_shared\""))
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        match event["event"].as_str() {
            Some("clipboard") => self.peer_clipboard(&event),
            Some("files_offered") => self.files_offered(&event),
            // Said right when the user copied them, not only on the page.
            Some("files_not_shared") => {
                crate::tray::notify(tr(s::FilesNotSharedTitle), &files_not_shared_text(&event))
            }
            Some("files_available") if self.enabled() => {
                if let (Ok(manifest), Some(peer)) = (
                    serde_json::from_value::<rinflow_core::FileManifest>(event["manifest"].clone()),
                    event["peer_id"].as_str(),
                ) {
                    self.queue_incoming(
                        IncomingContent::Files {
                            manifest,
                            peer: peer.to_owned(),
                        },
                        None,
                    );
                }
            }
            Some("files_unavailable") => {
                if let Some(peer) = event["peer_id"].as_str() {
                    self.queue_incoming(IncomingContent::Unavailable(peer.to_owned()), None);
                }
            }
            _ => {}
        }
    }

    /// A peer without file streams offered files the old way: declined at
    /// once (the core would otherwise hold the offer), and the user hears
    /// once why the files did not come.
    fn files_offered(&self, event: &Value) {
        let Some(transfer) = event["transfer"]
            .as_u64()
            .and_then(|transfer| u32::try_from(transfer).ok())
        else {
            return;
        };
        if let Some(core) = self.0.core.get().and_then(Weak::upgrade) {
            let _ = core.cancel_files(transfer);
        }
        log!(
            "declined files from {} in the old file protocol (no file streams)",
            event["peer_id"].as_str().unwrap_or("?")
        );
        if self.enabled() && !self.0.outdated_told.swap(true, Ordering::Relaxed) {
            crate::tray::notify(tr(s::FilesNotReceivedTitle), tr(s::PeerTooOldForFiles));
        }
    }

    fn peer_clipboard(&self, event: &Value) {
        if !self.enabled() {
            // The core keeps what it received as this PC's clipboard and
            // would pass it on to the next peer: clear that while sharing is
            // off (reporting while off reports nothing).
            self.post(WM_REPORT_CLIPBOARD);
            return;
        }
        match Clipboard::from_json(event) {
            Ok(clipboard) => self.queue_incoming(IncomingContent::Clipboard(clipboard), None),
            Err(error) => log!("peer clipboard not used: {error}"),
        }
    }

    fn queue_incoming(&self, content: IncomingContent, sequence: Option<u32>) {
        let mut slot = self.0.incoming.lock().unwrap();
        if let IncomingContent::Unavailable(peer) = &content {
            match slot.as_ref().map(|pending| &pending.content) {
                Some(IncomingContent::Files { peer: pending, .. }) if pending == peer => {}
                Some(_) => return,
                None => {}
            }
        }
        *slot = Some(Incoming {
            content,
            sequence: sequence.unwrap_or_else(|| unsafe { GetClipboardSequenceNumber() }),
        });
        drop(slot);
        self.post(WM_WRITE_CLIPBOARD);
    }

    fn post(&self, message: u32) {
        let window = self.0.window.load(Ordering::SeqCst);
        if window != 0 {
            // SAFETY: posts to the clipboard thread's window.
            unsafe {
                PostMessageW(window as HWND, message, 0, 0);
            }
        }
    }
}

/// The thread that owns the clipboard window.
pub struct ClipboardThread {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
    core: Weak<Core>,
}

impl ClipboardThread {
    pub fn start(handle: ClipboardHandle, core: Weak<Core>) -> io::Result<Self> {
        let shutdown_core = core.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name("rinflow-clipboard".to_owned())
            .spawn(move || {
                // OLE's clipboard owner must keep a pumping STA alive.
                if let Err(error) = unsafe { windows::Win32::System::Ole::OleInitialize(None) } {
                    let _ = ready_tx.send(Err(io::Error::other(error.to_string())));
                    return;
                }
                struct OleGuard;
                impl Drop for OleGuard {
                    fn drop(&mut self) {
                        unsafe {
                            windows::Win32::System::Ole::OleUninitialize();
                        }
                    }
                }
                let _ole = OleGuard;
                let window = match create_window() {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // SAFETY: plain thread id query.
                let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));
                let _ = handle.0.core.set(core.clone());
                handle.0.window.store(window as isize, Ordering::SeqCst);
                STATE.with(|state| {
                    *state.borrow_mut() = Some(State {
                        handle: handle.clone(),
                        core,
                        written: None,
                        native: None,
                        native_peer: None,
                        report_attempts: 0,
                        write_attempts: 0,
                    });
                });
                // Whatever is on the clipboard already counts as a change.
                with_state(|state| state.report(window));
                // SAFETY: standard message loop for the window on this thread.
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    handle.0.window.store(0, Ordering::SeqCst);
                    RemoveClipboardFormatListener(window);
                    DestroyWindow(window);
                }
                STATE.with(|state| {
                    state.borrow_mut().take();
                });
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                join: Some(join),
                core: shutdown_core,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("clipboard thread exited during start-up")),
        }
    }
}

impl Drop for ClipboardThread {
    fn drop(&mut self) {
        if let Some(core) = self.core.upgrade() {
            let _ = core.revoke_file_streams();
        }
        // SAFETY: asks the clipboard thread's message loop to finish.
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

struct State {
    handle: ClipboardHandle,
    core: Weak<Core>,
    /// The clipboard sequence number right after writing a peer's clipboard:
    /// that change is not reported back.
    written: Option<u32>,
    native: Option<windows::Win32::System::Com::IDataObject>,
    native_peer: Option<String>,
    /// Tries left for a report or write that found the clipboard busy.
    report_attempts: u32,
    write_attempts: u32,
}

impl State {
    fn changed(&mut self, window: HWND) {
        // SAFETY: plain clipboard query.
        let sequence = unsafe { GetClipboardSequenceNumber() };
        if self.written == Some(sequence) || self.owns_native() {
            return;
        }
        self.native = None;
        self.native_peer = None;
        self.report_attempts = RETRY_ATTEMPTS;
        self.report(window);
    }

    /// Hands the core the clipboard as it is (nothing while sharing is off).
    /// Another program holding the clipboard is not an empty clipboard: the
    /// report is tried again shortly.
    fn report(&mut self, window: HWND) {
        if !self.handle.enabled() {
            self.clear_native();
        } else if self.owns_native() {
            return;
        }
        let clipboard = if self.handle.enabled() {
            match read(window) {
                Some(clipboard) => clipboard,
                None => {
                    if self.report_attempts > 0 {
                        self.report_attempts -= 1;
                        // SAFETY: a timer on this thread's window.
                        unsafe { SetTimer(window, REPORT_RETRY_TIMER, RETRY_DELAY_MS, None) };
                    }
                    return;
                }
            }
        } else {
            Clipboard::default()
        };
        if let Some(core) = self.core.upgrade() {
            let _ = core.set_clipboard(clipboard);
        }
    }

    /// Writes a peer's clipboard. When another program holds the clipboard
    /// it waits for another try (unless a newer one arrived meanwhile).
    fn write_incoming(&mut self, window: HWND) {
        let incoming = self
            .handle
            .0
            .incoming
            .lock()
            .ok()
            .and_then(|mut incoming| incoming.take());
        let Some(clipboard) = incoming else {
            return;
        };
        if !self.handle.enabled() {
            return;
        }
        let sequence = unsafe { GetClipboardSequenceNumber() };
        if sequence != clipboard.sequence && self.written != Some(sequence) {
            return;
        }
        let success = match &clipboard.content {
            IncomingContent::Clipboard(value) => {
                let ok = write(window, value, sequence);
                if ok {
                    self.native = None;
                    self.native_peer = None;
                }
                ok
            }
            IncomingContent::Files { manifest, peer } => {
                match virtual_files::data_object(
                    manifest.clone(),
                    peer.clone(),
                    self.core.clone(),
                    self.handle.clone(),
                ) {
                    Ok(object) => {
                        if unsafe { GetClipboardSequenceNumber() } != sequence {
                            return;
                        }
                        if unsafe { windows::Win32::System::Ole::OleSetClipboard(&object) }.is_ok()
                        {
                            self.native = Some(object);
                            self.native_peer = Some(peer.clone());
                            true
                        } else {
                            false
                        }
                    }
                    Err(error) => {
                        log!("cannot publish virtual files: {error}");
                        crate::tray::notify(tr(s::FilesNotPublishedTitle), &error.to_string());
                        return;
                    }
                }
            }
            IncomingContent::Unavailable(peer) => {
                if self.native_peer.as_ref() == Some(peer) {
                    self.clear_native();
                }
                return;
            }
        };
        if success {
            // SAFETY: plain clipboard query.
            self.written = Some(unsafe { GetClipboardSequenceNumber() });
            self.write_attempts = RETRY_ATTEMPTS;
            return;
        }
        if self.write_attempts == 0 {
            log!("the clipboard stayed busy; a peer's clipboard was not written");
            self.write_attempts = RETRY_ATTEMPTS;
            return;
        }
        self.write_attempts -= 1;
        if let Ok(mut slot) = self.handle.0.incoming.lock()
            && slot.is_none()
        {
            *slot = Some(clipboard);
        }
        // SAFETY: a timer on this thread's window.
        unsafe { SetTimer(window, WRITE_RETRY_TIMER, RETRY_DELAY_MS, None) };
    }

    fn owns_native(&self) -> bool {
        self.native.as_ref().is_some_and(|object| {
            unsafe { windows::Win32::System::Ole::OleIsCurrentClipboard(object) }.is_ok()
        })
    }
    fn clear_native(&mut self) {
        if self.owns_native()
            && unsafe {
                windows::Win32::System::Ole::OleSetClipboard(
                    None::<&windows::Win32::System::Com::IDataObject>,
                )
            }
            .is_ok()
        {
            self.written = Some(unsafe { GetClipboardSequenceNumber() });
        }
        self.native = None;
        self.native_peer = None;
    }
}

fn with_state(action: impl FnOnce(&mut State)) {
    STATE.with(|state| {
        if let Ok(mut state) = state.try_borrow_mut()
            && let Some(state) = state.as_mut()
        {
            action(state);
        }
    });
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLIPBOARDUPDATE => with_state(|state| state.changed(window)),
        WM_WRITE_CLIPBOARD => with_state(|state| {
            state.write_attempts = RETRY_ATTEMPTS;
            state.write_incoming(window);
        }),
        WM_REPORT_CLIPBOARD => with_state(|state| {
            state.report_attempts = RETRY_ATTEMPTS;
            state.report(window);
        }),
        WM_TIMER => {
            // SAFETY: this window's own one-shot timer.
            unsafe { KillTimer(window, wparam) };
            match wparam {
                REPORT_RETRY_TIMER => with_state(|state| state.report(window)),
                WRITE_RETRY_TIMER => with_state(|state| state.write_incoming(window)),
                _ => {}
            }
        }
        _ => {
            // SAFETY: default handling for everything else.
            return unsafe { DefWindowProcW(window, message, wparam, lparam) };
        }
    }
    0
}

fn create_window() -> io::Result<HWND> {
    let class_name = wide("RinflowClipboard");
    // SAFETY: a message-only window on this thread, listening for clipboard
    // changes (message-only windows receive WM_CLIPBOARDUPDATE).
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
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        if AddClipboardFormatListener(window) == 0 {
            let error = io::Error::last_os_error();
            DestroyWindow(window);
            return Err(error);
        }
        Ok(window)
    }
}

/// The clipboard, opened for this window until dropped.
struct Opened;

impl Opened {
    /// Another program may hold the clipboard for a moment: retries briefly.
    fn new(window: HWND) -> Option<Self> {
        for _ in 0..10 {
            // SAFETY: opens the clipboard for this thread's window.
            if unsafe { OpenClipboard(window) } != 0 {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        None
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: closes the clipboard this thread opened.
        unsafe {
            CloseClipboard();
        }
    }
}

fn register(name: &str) -> u32 {
    let name = wide(name);
    // SAFETY: registers (or looks up) a named clipboard format.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

fn available(format: u32) -> bool {
    // SAFETY: plain clipboard query.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

/// A format's bytes. The clipboard must be open.
fn get_bytes(format: u32) -> Option<Vec<u8>> {
    if !available(format) {
        return None;
    }
    // SAFETY: reads a global memory block owned by the clipboard while it is
    // open, within the block's size.
    unsafe {
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle);
        let pointer = GlobalLock(handle) as *const u8;
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }
}

/// Puts bytes on the clipboard as `format`. The clipboard must be open and
/// emptied by this window.
fn set_bytes(format: u32, bytes: &[u8]) {
    // SAFETY: allocates a movable global block, fills it, and hands it to the
    // clipboard, which then owns it (freed here only if that fails).
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if handle.is_null() {
            return;
        }
        let pointer = GlobalLock(handle) as *mut u8;
        if pointer.is_null() {
            GlobalFree(handle);
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        GlobalUnlock(handle);
        if SetClipboardData(format, handle).is_null() {
            GlobalFree(handle);
        }
    }
}

/// Whether a password manager or similar marked the contents as private.
fn is_private() -> bool {
    if available(register("ExcludeClipboardContentFromMonitorProcessing"))
        || available(register("Clipboard Viewer Ignore"))
    {
        return true;
    }
    // A DWORD 0 here means "keep out of clipboard history".
    get_bytes(register("CanIncludeInClipboardHistory"))
        .is_some_and(|bytes| bytes.len() >= 4 && bytes[..4] == [0, 0, 0, 0])
}

/// What `read` copies while the clipboard is open; converted after closing
/// it, so other programs never wait on the conversions.
#[derive(Default)]
struct Raw {
    drop: Option<Vec<u8>>,
    html: Option<Vec<u8>>,
    png: Option<Vec<u8>>,
    dib: Option<Vec<u8>>,
    text: Option<Vec<u8>>,
}

/// The clipboard in the shared formats, most specific first; `None` when
/// another program held it too long to read.
fn read(window: HWND) -> Option<Clipboard> {
    let raw = {
        let _opened = Opened::new(window)?;
        if is_private() {
            return Some(Clipboard::default());
        }
        let drop = get_bytes(CF_HDROP);
        if drop.is_some() {
            Raw {
                drop,
                ..Raw::default()
            }
        } else {
            let png = get_bytes(register("PNG"));
            Raw {
                drop: None,
                html: get_bytes(register("HTML Format")),
                // A bitmap only when there is no PNG to take as it is.
                dib: if png.is_none() {
                    get_bytes(CF_DIB)
                } else {
                    None
                },
                png,
                text: get_bytes(CF_UNICODETEXT),
            }
        }
    };
    Some(convert(raw))
}

fn convert(raw: Raw) -> Clipboard {
    // Files copied in Explorer: the files (the core turns the URLs into a
    // manifest) and their names as text, never their icons.
    if let Some(paths) = raw.drop.and_then(|data| drop_files(&data))
        && !paths.is_empty()
    {
        let urls: Vec<String> = paths.iter().map(|path| file_url(path)).collect();
        let names: Vec<String> = paths
            .iter()
            .map(|path| {
                Path::new(path)
                    .file_name()
                    .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned())
            })
            .collect();
        return Clipboard {
            formats: vec![
                ClipboardFormat {
                    mime: MIME_URI_LIST.to_owned(),
                    data: urls.join("\r\n").into_bytes(),
                },
                ClipboardFormat {
                    mime: MIME_TEXT.to_owned(),
                    data: names.join("\r\n").into_bytes(),
                },
            ],
        };
    }
    let text = raw
        .text
        .and_then(|data| utf16_text(&data))
        // Line breaks travel as LF.
        .map(|text| text.replace("\r\n", "\n").into_bytes());
    let html = raw.html.and_then(|data| html_fragment(&data));
    let budget =
        |parts: &[Option<&Vec<u8>>]| parts.iter().flatten().map(|data| data.len()).sum::<usize>();
    // The image only when it fits with the rest; a bitmap is not even
    // converted when it could not.
    let png = match raw.png {
        Some(png) => Some(png),
        None => raw
            .dib
            .filter(|dib| dib.len() / 4 < SHARE_LIMIT)
            .and_then(|dib| dib_to_png(&dib)),
    }
    .filter(|png| budget(&[Some(png), html.as_ref(), text.as_ref()]) <= SHARE_LIMIT);
    let html =
        html.filter(|html| budget(&[png.as_ref(), Some(html), text.as_ref()]) <= SHARE_LIMIT);
    let text = text.filter(|text| text.len() <= SHARE_LIMIT);

    let mut formats = Vec::new();
    if let Some(html) = html {
        formats.push(ClipboardFormat {
            mime: MIME_HTML.to_owned(),
            data: html,
        });
    }
    if let Some(png) = png {
        formats.push(ClipboardFormat {
            mime: MIME_PNG.to_owned(),
            data: png,
        });
    }
    if let Some(text) = text {
        formats.push(ClipboardFormat {
            mime: MIME_TEXT.to_owned(),
            data: text,
        });
    }
    Clipboard { formats }
}

/// Replaces the clipboard with a peer's. False when it could not be opened.
fn write(window: HWND, clipboard: &Clipboard, expected_sequence: u32) -> bool {
    // Everything converted first: the clipboard is held only to copy it in.
    let entries = clipboard_entries(clipboard);
    let Some(_opened) = Opened::new(window) else {
        return false;
    };
    if unsafe { GetClipboardSequenceNumber() } != expected_sequence {
        return false;
    }
    // SAFETY: the clipboard is open for this window.
    unsafe {
        EmptyClipboard();
    }
    for (format, bytes) in entries {
        set_bytes(format, &bytes);
    }
    true
}

/// A peer's clipboard as Windows formats and their bytes.
fn clipboard_entries(clipboard: &Clipboard) -> Vec<(u32, Vec<u8>)> {
    // A peer's files never arrive here as paths: they come as virtual files
    // (`IncomingContent::Files`), read from the peer on demand.
    let mut entries = Vec::new();
    for format in &clipboard.formats {
        match format.mime.as_str() {
            MIME_TEXT => {
                // Line breaks travel as LF; Windows text uses CRLF (classic
                // edit controls show bare LF as one long line).
                let text = String::from_utf8_lossy(&format.data)
                    .replace("\r\n", "\n")
                    .replace('\n', "\r\n");
                let units: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                entries.push((CF_UNICODETEXT, bytes));
            }
            MIME_HTML => entries.push((register("HTML Format"), cf_html(&format.data))),
            MIME_PNG => {
                entries.push((register("PNG"), format.data.clone()));
                // For programs that only take bitmaps.
                if let Some(dib) = png_to_dib(&format.data) {
                    entries.push((CF_DIB, dib));
                }
            }
            _ => {}
        }
    }
    entries
}

/// The paths in a `CF_HDROP` block (`DROPFILES` and NUL-separated names).
fn drop_files(data: &[u8]) -> Option<Vec<String>> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
    let start = u32_at(0)? as usize;
    // Only wide names; ANSI drop lists are a relic no current program makes.
    if u32_at(16)? == 0 {
        return None;
    }
    let units: Vec<u16> = data
        .get(start..)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some(
        units
            .split(|&unit| unit == 0)
            .take_while(|name| !name.is_empty())
            .map(String::from_utf16_lossy)
            .collect(),
    )
}

/// `C:\Users\a b\x.txt` → `file:///C:/Users/a%20b/x.txt` (as the core reads
/// and writes them; UNC paths become `file:////server/share/…`).
fn file_url(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut url = String::from("file:///");
    for byte in path.trim_start_matches('/').bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    if path.starts_with("//") {
        url.insert("file:///".len(), '/');
    }
    url
}

/// `CF_UNICODETEXT` bytes (UTF-16, NUL-terminated) as text; None if empty.
fn utf16_text(data: &[u8]) -> Option<String> {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    let text = String::from_utf16_lossy(&units);
    (!text.is_empty()).then_some(text)
}

/// The fragment of a `CF_HTML` block, located by its header's byte offsets.
fn html_fragment(data: &[u8]) -> Option<Vec<u8>> {
    let header_end = data.len().min(1024);
    let header = String::from_utf8_lossy(&data[..header_end]);
    let offset = |key: &str| -> Option<usize> {
        let start = header.find(key)? + key.len();
        header[start..]
            .chars()
            .skip_while(|c| *c == ' ')
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok()
    };
    let (start, end) = match (offset("StartFragment:"), offset("EndFragment:")) {
        (Some(start), Some(end)) => (start, end),
        _ => (offset("StartHTML:")?, offset("EndHTML:")?),
    };
    let fragment = data.get(start..end.min(data.len()))?;
    (!fragment.is_empty()).then(|| fragment.to_vec())
}

/// A `CF_HTML` block around an HTML fragment.
fn cf_html(fragment: &[u8]) -> Vec<u8> {
    const PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";
    let header = |start_html: usize, end_html: usize, start: usize, end: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    let start_html = header(0, 0, 0, 0).len();
    let start = start_html + PREFIX.len();
    let end = start + fragment.len();
    let end_html = end + SUFFIX.len();
    let mut bytes = header(start_html, end_html, start, end).into_bytes();
    bytes.extend_from_slice(PREFIX.as_bytes());
    bytes.extend_from_slice(fragment);
    bytes.extend_from_slice(SUFFIX.as_bytes());
    bytes.push(0);
    bytes
}

/// A `CF_DIB` (24- or 32-bit, uncompressed) as PNG.
fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(dib.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?));
    let header = u32_at(0)? as usize;
    let width = u32_at(4)? as i32;
    let height = u32_at(8)? as i32;
    let bits = u16_at(14)?;
    let compression = u32_at(16)?;
    let colors = u32_at(32)? as usize;
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    if header < 40 || width <= 0 || height == 0 || !matches!(bits, 24 | 32) {
        return None;
    }
    if compression != BI_RGB && compression != BI_BITFIELDS {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if w.checked_mul(h)? > MAX_PIXELS {
        return None;
    }
    // A plain BITMAPINFOHEADER is followed by the three masks for bitfields.
    let masks = if compression == BI_BITFIELDS && header == 40 {
        12
    } else {
        0
    };
    let start = header + masks + colors * 4;
    let bytes_per_pixel = usize::from(bits / 8);
    let stride = (w * bytes_per_pixel + 3) & !3;
    let pixels = dib.get(start..start + stride * h)?;
    let mut rgba = vec![0u8; w * h * 4];
    let mut has_alpha = false;
    for y in 0..h {
        // Positive height: bottom-up rows.
        let source = if height > 0 { h - 1 - y } else { y };
        let row = &pixels[source * stride..source * stride + w * bytes_per_pixel];
        for x in 0..w {
            let pixel = &row[x * bytes_per_pixel..(x + 1) * bytes_per_pixel];
            let target = &mut rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
            target[0] = pixel[2];
            target[1] = pixel[1];
            target[2] = pixel[0];
            target[3] = if bytes_per_pixel == 4 { pixel[3] } else { 255 };
            has_alpha |= bytes_per_pixel == 4 && pixel[3] != 0;
        }
    }
    // Most programs leave the fourth byte of 32-bit bitmaps at zero.
    if !has_alpha {
        rgba.as_chunks_mut::<4>()
            .0
            .iter_mut()
            .for_each(|pixel| pixel[3] = 255);
    }
    encode_png(w as u32, h as u32, &rgba)
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// A PNG as a 32-bit bottom-up `CF_DIB`.
fn png_to_dib(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width == 0 || height == 0 || width.checked_mul(height)? > MAX_PIXELS {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let channels = match frame.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let line = frame.line_size;
    let mut dib = Vec::with_capacity(40 + width * height * 4);
    for value in [40u32, width as u32, height as u32] {
        dib.extend_from_slice(&value.to_le_bytes());
    }
    dib.extend_from_slice(&1u16.to_le_bytes()); // planes
    dib.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&((width * height * 4) as u32).to_le_bytes());
    dib.extend_from_slice(&[0; 16]); // resolution and palette counts
    for y in (0..height).rev() {
        let row = &buffer[y * line..y * line + width * channels];
        for pixel in row.chunks_exact(channels) {
            let (r, g, b, a) = match channels {
                1 => (pixel[0], pixel[0], pixel[0], 255),
                2 => (pixel[0], pixel[0], pixel[0], pixel[1]),
                3 => (pixel[0], pixel[1], pixel[2], 255),
                _ => (pixel[0], pixel[1], pixel[2], pixel[3]),
            };
            dib.extend_from_slice(&[b, g, r, a]);
        }
    }
    Some(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_survives_a_trip_through_cf_html() {
        let fragment = "<b>粗体</b> and <i>italic</i>".as_bytes();
        let block = cf_html(fragment);
        assert_eq!(html_fragment(&block).as_deref(), Some(fragment));
    }

    #[test]
    fn a_word_style_cf_html_block_yields_its_fragment() {
        let fragment = "<p>Hi</p>";
        let body =
            format!("<html><body><!--StartFragment-->{fragment}<!--EndFragment--></body></html>");
        let prefix_len = "<html><body><!--StartFragment-->".len();
        let header = "Version:1.0\r\nStartHTML:0000000097\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let start = header.len() + prefix_len;
        let header = format!(
            "Version:1.0\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n",
            header.len(),
            header.len() + body.len(),
            start,
            start + fragment.len()
        );
        let block = format!("{header}{body}");
        assert_eq!(
            html_fragment(block.as_bytes()).as_deref(),
            Some(fragment.as_bytes())
        );
    }

    #[test]
    fn images_survive_dib_to_png_and_back() {
        // 3 x 2, 24-bit, bottom-up, rows padded to 12 bytes.
        let mut dib = Vec::new();
        for value in [40u32, 3, 2] {
            dib.extend_from_slice(&value.to_le_bytes());
        }
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        // Bottom row: blue, green, red; top row: white, black, gray (BGR).
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
        dib.extend_from_slice(&[255, 255, 255, 0, 0, 0, 128, 128, 128, 0, 0, 0]);
        let png = dib_to_png(&dib).expect("png");
        assert_eq!(&png[1..4], b"PNG");

        let back = png_to_dib(&png).expect("dib");
        assert_eq!(u32::from_le_bytes(back[4..8].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(back[8..12].try_into().unwrap()), 2);
        let pixels = &back[40..];
        // First stored row is the bottom one: blue, green, red, opaque.
        assert_eq!(
            &pixels[..12],
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
        );
        // Then the top row: white, black, gray.
        assert_eq!(
            &pixels[12..24],
            &[255, 255, 255, 255, 0, 0, 0, 255, 128, 128, 128, 255]
        );
    }

    /// A `CF_HDROP` block for `paths`, as Explorer puts one on the clipboard.
    fn drop_files_block(paths: &[String]) -> Vec<u8> {
        let mut block = Vec::new();
        block.extend_from_slice(&20u32.to_le_bytes()); // pFiles: names follow
        block.extend_from_slice(&[0; 12]); // pt, fNC
        block.extend_from_slice(&1u32.to_le_bytes()); // fWide
        for path in paths {
            for unit in path.encode_utf16().chain(Some(0)) {
                block.extend_from_slice(&unit.to_le_bytes());
            }
        }
        block.extend_from_slice(&[0, 0]); // the list ends with an empty name
        block
    }

    #[test]
    fn copied_files_become_file_urls() {
        let paths = vec![
            r"C:\Users\a b\报告.pdf".to_owned(),
            r"D:\x%y#z.txt".to_owned(),
            r"\\server\share\dir".to_owned(),
        ];
        assert_eq!(drop_files(&drop_files_block(&paths)), Some(paths.clone()));

        let urls: Vec<String> = paths.iter().map(|path| file_url(path)).collect();
        assert_eq!(urls[0], "file:///C:/Users/a%20b/%E6%8A%A5%E5%91%8A.pdf");
        assert_eq!(urls[1], "file:///D:/x%25y%23z.txt");
        assert_eq!(urls[2], "file:////server/share/dir");
    }

    #[test]
    fn a_peers_file_paths_never_reach_the_clipboard() {
        // Only the old file protocol sent paths (of files it downloaded);
        // whatever a peer puts in a uri-list is not written as files here.
        let clipboard = Clipboard {
            formats: vec![ClipboardFormat {
                mime: MIME_URI_LIST.to_owned(),
                data: b"file:///C:/Windows/System32/cmd.exe".to_vec(),
            }],
        };
        assert!(clipboard_entries(&clipboard).is_empty());
    }

    #[test]
    fn text_stops_at_the_terminator() {
        let bytes: Vec<u8> = "你好\0junk"
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        assert_eq!(utf16_text(&bytes).as_deref(), Some("你好"));
        assert_eq!(utf16_text(&[0, 0]), None);
    }
}

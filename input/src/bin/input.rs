//! `linkcursor-input`: plays the agent's input on whichever desktop has
//! input, as LocalSystem in the user's session (see the crate docs).
//!
//! - A pipe server takes one client at a time: `linkcursor-agent` from this
//!   folder, in this session. The pipe's ACL lets in LocalSystem and the
//!   session's user, never remote clients.
//! - A watcher checks every 200 ms which desktop has input (`Default`, or
//!   `Winlogon` for UAC prompts and the lock screen) and every 2 s the
//!   machine-wide setting, and tells the agent when either changes.
//! - The injecting thread has no windows or hooks, so it can move to the
//!   input desktop (`SetThreadDesktop`) before playing.
//!
//! `--dev` (a development run as the user): ignores the setting. It can then
//! play only where the user's own processes can.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use linkcursor_input::win::{self, SecurityAttributes, is_sibling_process};
use linkcursor_input::{
    AGENT_EXE, ClientMessage, PROTOCOL_VERSION, RawInput, ServerMessage, VirtualDesktop, log,
    pipe_name, read_frame,
};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, watch};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, HDESK, OpenInputDesktop,
    SetThreadDesktop, UOI_NAME,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// Access to the input desktop for playing input on it.
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    enabled: bool,
    secure: bool,
}

/// What the watcher, the pipe and the injecting thread share.
struct Shared {
    enabled: AtomicBool,
    secure: AtomicBool,
    /// A peer drives this PC (the agent says so).
    remote_control: AtomicBool,
    /// Bumped whenever the input desktop changes.
    desktop_generation: AtomicU64,
}

impl Shared {
    /// Whether input may be played now.
    fn allowed(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
            && (!self.secure.load(Ordering::SeqCst) || self.remote_control.load(Ordering::SeqCst))
    }
}

enum Job {
    Input(RawInput),
    MoveBy(i32, i32),
    /// Log how many events were played so far.
    Report,
}

fn main() {
    let dev = std::env::args().any(|argument| argument == "--dev");
    log::init("input");
    // SAFETY: process-wide setting, before any window or DPI query.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let session = win::current_session();
    log!(
        "started in session {session} (LocalSystem: {}, dev: {dev})",
        win::is_local_system()
    );

    let shared = Arc::new(Shared {
        enabled: AtomicBool::new(false),
        secure: AtomicBool::new(false),
        remote_control: AtomicBool::new(false),
        desktop_generation: AtomicU64::new(0),
    });
    let (states, state) = watch::channel(State {
        enabled: false,
        secure: false,
    });
    spawn_watcher(shared.clone(), states, dev);
    let jobs = spawn_injector(shared.clone());

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            log!("no async runtime: {error}");
            return;
        }
    };
    loop {
        if let Err(error) = runtime.block_on(serve(session, &shared, &jobs, &state)) {
            log!("pipe server failed: {error}; trying again");
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

/// The name of the desktop that has input, or `None` when it cannot be
/// opened (taken as not the user's).
fn input_desktop_name() -> Option<String> {
    // SAFETY: opened for reading its name, closed below.
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desktop.is_null() {
        return None;
    }
    let name = desktop_name(desktop);
    // SAFETY: the handle opened above.
    unsafe { CloseDesktop(desktop) };
    name
}

fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut buffer = [0u16; 128];
    let mut needed = 0;
    // SAFETY: `buffer` is large enough for desktop names; its size is given.
    if unsafe {
        GetUserObjectInformationW(
            desktop,
            UOI_NAME,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut needed,
        )
    } == 0
    {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn is_secure(name: Option<&str>) -> bool {
    !name.is_some_and(|name| name.eq_ignore_ascii_case("Default"))
}

fn spawn_watcher(shared: Arc<Shared>, states: watch::Sender<State>, dev: bool) {
    std::thread::spawn(move || {
        let mut tick = 0u32;
        let mut last_name: Option<String> = None;
        loop {
            let name = input_desktop_name();
            if name != last_name {
                log!(
                    "input desktop: {:?}",
                    name.as_deref().unwrap_or("(not readable)")
                );
                shared.desktop_generation.fetch_add(1, Ordering::SeqCst);
                last_name = name;
            }
            let secure = is_secure(last_name.as_deref());
            shared.secure.store(secure, Ordering::SeqCst);
            if tick.is_multiple_of(10) {
                let enabled = dev || win::elevated_input_setting().unwrap_or(false);
                if shared.enabled.swap(enabled, Ordering::SeqCst) != enabled {
                    log!("elevated input {}", if enabled { "on" } else { "off" });
                }
            }
            let state = State {
                enabled: shared.enabled.load(Ordering::SeqCst),
                secure,
            };
            states.send_if_modified(|current| {
                let changed = *current != state;
                *current = state;
                changed
            });
            tick = tick.wrapping_add(1);
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

/// The thread that plays input. It owns no windows or hooks, so it may
/// switch to whichever desktop has input.
fn spawn_injector(shared: Arc<Shared>) -> std::sync::mpsc::Sender<Job> {
    let (jobs, receiver) = std::sync::mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut desktop = ThreadDesktop::default();
        let mut played = 0u64;
        let mut failing = false;
        while let Ok(job) = receiver.recv() {
            if matches!(job, Job::Report) {
                log!("played {played} events");
                continue;
            }
            if !shared.allowed() {
                continue;
            }
            desktop.follow(&shared, false);
            let mut ok = play(&job);
            if !ok {
                // The input desktop changed since the watcher looked.
                desktop.follow(&shared, true);
                ok = shared.allowed() && play(&job);
            }
            if ok {
                played += 1;
                failing = false;
            } else if !failing {
                // Once per run of failures.
                failing = true;
                log!("cannot play input: {}", std::io::Error::last_os_error());
            }
        }
    });
    jobs
}

/// Plays one job on this thread's desktop; false if that failed.
fn play(job: &Job) -> bool {
    let input = match *job {
        Job::Report => return true,
        Job::Input(raw) => raw,
        Job::MoveBy(dx, dy) => {
            let mut cursor = POINT { x: 0, y: 0 };
            // SAFETY: writes the cursor position into `cursor`.
            if unsafe { GetCursorPos(&mut cursor) } == 0 {
                return false;
            }
            VirtualDesktop::current()
                .move_to(cursor.x.saturating_add(dx), cursor.y.saturating_add(dy))
        }
    };
    let Some(input) = input.to_input() else {
        return true;
    };
    // SAFETY: one fully initialised INPUT with its exact size.
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) == 1 }
}

/// The desktop the injecting thread is on, moved to the input desktop
/// whenever that changes.
#[derive(Default)]
struct ThreadDesktop {
    /// Opened by this thread; closed when it moves on.
    opened: Option<HDESK>,
    generation: Option<u64>,
}

impl ThreadDesktop {
    fn follow(&mut self, shared: &Shared, force: bool) {
        let generation = shared.desktop_generation.load(Ordering::SeqCst);
        if !force && self.generation == Some(generation) {
            return;
        }
        self.generation = Some(generation);
        // SAFETY: opens the input desktop; kept while the thread is on it.
        let desktop = unsafe { OpenInputDesktop(0, 0, MAXIMUM_ALLOWED) };
        if desktop.is_null() {
            log!(
                "cannot open the input desktop: {}",
                std::io::Error::last_os_error()
            );
            return;
        }
        let secure = is_secure(desktop_name(desktop).as_deref());
        shared.secure.store(secure, Ordering::SeqCst);
        // SAFETY: this thread has no windows or hooks.
        if unsafe { SetThreadDesktop(desktop) } == 0 {
            log!(
                "cannot switch to the input desktop: {}",
                std::io::Error::last_os_error()
            );
            // SAFETY: opened above, not in use.
            unsafe { CloseDesktop(desktop) };
            return;
        }
        if let Some(previous) = self.opened.replace(desktop) {
            // SAFETY: opened by this thread earlier, no longer its desktop.
            unsafe { CloseDesktop(previous) };
        }
    }
}

/// Serves the agent of this session, one connection at a time: a newer one
/// replaces the older.
async fn serve(
    session: u32,
    shared: &Arc<Shared>,
    jobs: &std::sync::mpsc::Sender<Job>,
    state: &watch::Receiver<State>,
) -> std::io::Result<()> {
    let name = pipe_name(session);
    let user = win::session_user_sid(session)?;
    // LocalSystem and the session's user, nobody else; not across the
    // network either (`reject_remote_clients`).
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{user})");
    let mut security = SecurityAttributes::from_sddl(&sddl)?;
    // The first instance must be ours: if something else took the name, a
    // client could talk to it instead.
    // SAFETY: the security attributes outlive the call.
    let mut server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(&name, security.as_mut_ptr())?
    };
    log!("serving {name} for {user}");
    let mut current: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        server.connect().await?;
        let client = server;
        // A new instance before this one is handed on, so the name never
        // goes free.
        // SAFETY: as above.
        server = unsafe {
            ServerOptions::new()
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(&name, security.as_mut_ptr())?
        };
        let mut pid = 0;
        // SAFETY: a connected pipe handle of this process.
        let known = unsafe { GetNamedPipeClientProcessId(client.as_raw_handle(), &mut pid) } != 0;
        if !known || !is_sibling_process(pid, AGENT_EXE) {
            log!(
                "refused client {pid}: {}",
                win::process_image(pid)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            );
            continue;
        }
        log!("client {pid} connected");
        if let Some(previous) = current.take() {
            previous.abort();
        }
        current = Some(tokio::spawn(handle_client(
            client,
            shared.clone(),
            jobs.clone(),
            state.clone(),
        )));
    }
}

async fn handle_client(
    client: NamedPipeServer,
    shared: Arc<Shared>,
    jobs: std::sync::mpsc::Sender<Job>,
    mut state: watch::Receiver<State>,
) {
    let (mut reader, mut writer) = tokio::io::split(client);
    // Hello first, or nothing.
    let hello = read_frame(&mut reader)
        .await
        .ok()
        .flatten()
        .and_then(|payload| ClientMessage::decode(&payload));
    if hello
        != Some(ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
    {
        log!("client spoke another protocol: {hello:?}");
        return;
    }
    // Writes happen on their own task, so a slow reader never blocks input.
    let (outgoing, mut pending) = mpsc::unbounded_channel::<ServerMessage>();
    let writing = tokio::spawn(async move {
        while let Some(message) = pending.recv().await {
            if writer.write_all(&message.encode()).await.is_err() {
                break;
            }
        }
    });
    let send_state = |state: State| {
        let _ = outgoing.send(ServerMessage::State {
            enabled: state.enabled,
            secure: state.secure,
        });
    };
    send_state(*state.borrow_and_update());
    loop {
        tokio::select! {
            frame = read_frame(&mut reader) => {
                let Ok(Some(payload)) = frame else { break };
                match ClientMessage::decode(&payload) {
                    Some(ClientMessage::Input(raw)) => {
                        let _ = jobs.send(Job::Input(raw));
                    }
                    Some(ClientMessage::MoveBy { dx, dy }) => {
                        let _ = jobs.send(Job::MoveBy(dx, dy));
                    }
                    Some(ClientMessage::RemoteControl { active }) => {
                        shared.remote_control.store(active, Ordering::SeqCst);
                    }
                    Some(ClientMessage::Hello { .. }) => {}
                    None => {
                        log!("malformed message; closing");
                        break;
                    }
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                send_state(*state.borrow_and_update());
            }
        }
    }
    // Without its agent nobody drives this PC.
    shared.remote_control.store(false, Ordering::SeqCst);
    writing.abort();
    let _ = jobs.send(Job::Report);
    log!("client gone");
}

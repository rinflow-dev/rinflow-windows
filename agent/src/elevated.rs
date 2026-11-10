// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The agent's end of `rinflow-input` (see the `rinflow-input` crate):
//! the LocalSystem helper that plays input where this process cannot, in
//! windows of elevated programs, UAC prompts and on the lock screen.
//!
//! A thread keeps a connection to the helper of this session when there is
//! one (the service is installed), checking that the pipe belongs to
//! `rinflow-input.exe` from this folder. While connected and the
//! machine-wide setting is on, `inject.rs` hands every event to the helper
//! instead of calling `SendInput` itself; otherwise it plays them as before.
//! The helper says when a UAC prompt or the lock screen takes input; a peer
//! this PC drives is then given back, so the user can answer it here.

use rinflow_input::win::is_sibling_process;
use rinflow_input::{
    ClientMessage, INPUT_EXE, PROTOCOL_VERSION, RawInput, ServerMessage, pipe_name, read_frame,
};
use std::os::windows::io::AsRawHandle;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::mpsc;
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT;

/// What the rest of the agent sees of the helper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// Connected to the helper (installed, service running).
    pub available: bool,
    /// The machine-wide setting is on: input goes through the helper.
    pub enabled: bool,
    /// A UAC prompt or the lock screen has input.
    pub secure: bool,
}

struct Channel {
    messages: mpsc::UnboundedSender<ClientMessage>,
    connected: AtomicBool,
    enabled: AtomicBool,
    /// A peer drives this PC; sent again on every new connection.
    remote_control: AtomicBool,
}

static CHANNEL: OnceLock<Channel> = OnceLock::new();

/// Starts connecting; `on_status` runs on the connection's thread whenever
/// the status changes.
pub fn start(on_status: impl Fn(Status) + Send + 'static) {
    let (messages, receiver) = mpsc::unbounded_channel();
    if CHANNEL
        .set(Channel {
            messages,
            connected: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            remote_control: AtomicBool::new(false),
        })
        .is_err()
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("rinflow-elevated".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(keep_connected(receiver, on_status));
        });
    if let Err(error) = spawned {
        log!("cannot start the input helper link: {error}");
    }
}

/// Connected to the helper (installed, service running), whether or not the
/// machine-wide setting lets it play.
pub fn available() -> bool {
    CHANNEL
        .get()
        .is_some_and(|channel| channel.connected.load(Ordering::SeqCst))
}

/// Input goes through the helper now.
pub fn active() -> bool {
    CHANNEL.get().is_some_and(|channel| {
        channel.connected.load(Ordering::SeqCst) && channel.enabled.load(Ordering::SeqCst)
    })
}

/// Hands one event to the helper; false when it is not in use, so the
/// caller plays it itself.
pub fn forward(input: &INPUT) -> bool {
    let Some(raw) = RawInput::from_input(input) else {
        return false;
    };
    send(ClientMessage::Input(raw))
}

/// Pointer motion in physical pixels, from wherever the pointer is on the
/// input desktop; false when the helper is not in use.
pub fn move_by(dx: i32, dy: i32) -> bool {
    send(ClientMessage::MoveBy { dx, dy })
}

/// Whether a peer drives this PC: the helper plays into UAC prompts and the
/// lock screen only then.
pub fn set_remote_control(active: bool) {
    if let Some(channel) = CHANNEL.get() {
        channel.remote_control.store(active, Ordering::SeqCst);
        if channel.connected.load(Ordering::SeqCst) {
            let _ = channel
                .messages
                .send(ClientMessage::RemoteControl { active });
        }
    }
}

fn send(message: ClientMessage) -> bool {
    let Some(channel) = CHANNEL.get() else {
        return false;
    };
    active() && channel.messages.send(message).is_ok()
}

async fn keep_connected(
    mut outgoing: mpsc::UnboundedReceiver<ClientMessage>,
    on_status: impl Fn(Status),
) {
    let name = pipe_name(rinflow_input::win::current_session());
    let mut reported: Option<Status> = None;
    let mut report = |status: Status| {
        if reported != Some(status) {
            reported = Some(status);
            on_status(status);
        }
    };
    report(Status::default());
    loop {
        if let Ok(pipe) = ClientOptions::new().open(&name) {
            if is_helper(&pipe) {
                serve(pipe, &mut outgoing, &mut report).await;
            } else {
                log!("{name} is not Rinflow's input helper");
            }
        }
        let channel = CHANNEL.get().expect("started");
        channel.connected.store(false, Ordering::SeqCst);
        report(Status::default());
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The pipe belongs to Rinflow's helper: it was created by LocalSystem
/// (its owner says so; only a system or elevated process can make that the
/// owner), as the installed helper does. A development run of the helper,
/// as this user, must be `rinflow-input.exe` from this folder in this
/// session. (This process cannot look into a LocalSystem process, so the
/// first check is the one that works when installed.)
fn is_helper(pipe: &NamedPipeClient) -> bool {
    if rinflow_input::win::object_owner_sid(pipe)
        .is_ok_and(|owner| rinflow_input::win::is_privileged_owner(&owner))
    {
        return true;
    }
    let mut pid = 0;
    // SAFETY: a connected pipe handle of this process.
    let known = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } != 0;
    known && is_sibling_process(pid, INPUT_EXE)
}

async fn serve(
    pipe: NamedPipeClient,
    outgoing: &mut mpsc::UnboundedReceiver<ClientMessage>,
    report: &mut impl FnMut(Status),
) {
    let channel = CHANNEL.get().expect("started");
    // Input queued while there was no connection is stale by now.
    while outgoing.try_recv().is_ok() {}
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
    };
    if writer.write_all(&hello.encode()).await.is_err() {
        return;
    }
    // Connected before the value is read: a change from here on is queued
    // (`set_remote_control`) and sent by the loop below, so none is lost
    // between reading it and saying so.
    channel.connected.store(true, Ordering::SeqCst);
    let remote_control = ClientMessage::RemoteControl {
        active: channel.remote_control.load(Ordering::SeqCst),
    };
    if writer.write_all(&remote_control.encode()).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            frame = read_frame(&mut reader) => {
                let Ok(Some(payload)) = frame else { return };
                let Some(ServerMessage::State { enabled, secure }) = ServerMessage::decode(&payload) else {
                    return;
                };
                channel.enabled.store(enabled, Ordering::SeqCst);
                report(Status { available: true, enabled, secure });
            }
            message = outgoing.recv() => {
                let Some(message) = message else { return };
                if writer.write_all(&message.encode()).await.is_err() {
                    return;
                }
            }
        }
    }
}

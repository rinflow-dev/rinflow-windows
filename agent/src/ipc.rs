// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Named-pipe endpoint for the settings app.
//!
//! Each message is one UTF-8 JSON object per line. The app sends requests
//! (`hello`, `connect`, `set_effect`, …); the agent answers `hello` and then
//! pushes a full `state` snapshot whenever anything changes. See docs/design.md
//! for the message shapes.

use crate::effect::EffectParams;
use crate::services::Services;
use crate::state::Snapshot;
use crate::win::PipeSecurity;
use rinflow_core::layout::{Offset, Side};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// 6: `update_settings`, `forget_device`, `quit`; the snapshot gains
/// `settings`, `launch_at_login`, `control_port`, `locked`,
/// `agent_version`, session `latency_ms` and device `remembered` /
/// `last_seen_ms`.
/// 7: `preview_placement`, answered with `placement_preview`.
/// 8: `accept_pairing`, `reject_pairing`; the snapshot gains `fingerprint`,
/// `pairing`, `paired_peers`, session `paired`, and the `paired_only`
/// setting.
/// 9: sessions lose `encrypted`: every connection is encrypted now.
/// 10: the snapshot gains `elevated_input`.
/// 11: `clear_error`; the snapshot gains `quitting`.
/// 12: `cancel_transfer`; the snapshot gains `manual_addresses` and
/// `transfers`; settings `shortcut_remap`, `switch_hotkeys`, `keep_awake`,
/// `lock_sync`, `manual_addresses` and `peer_input`.
/// 13: setting `hide_cursor_on_leave`.
/// 14: `accept_transfer` and transfer `awaiting_acceptance`.
/// 15: `elevated_input` gains `installable` (setup installs the input service
/// only while its setting is on; the settings switch adds it).
/// 16: `accept_transfer` and transfer `awaiting_acceptance` are gone: files
/// come only through file streams; offers in the old file protocol are
/// declined.
/// 17: setting `modifier_map` (a Mac's modifier keys, chosen per key)
/// replaces `shortcut_remap`.
/// 18: `check_update`, `install_update`; the snapshot gains `update`, and
/// the setting `auto_update`.
pub const PROTOCOL_VERSION: u32 = 18;

/// The longest request line the agent reads, `\n` excluded. The app's
/// largest requests (`update_settings` with every manual address and
/// per-device speed, `set_effect`) are a few KB; a client sending more is
/// dropped instead of growing the agent's memory without bound.
const MAX_REQUEST_LINE: usize = 1024 * 1024;

pub fn pipe_name(user_sid: &str) -> String {
    format!(r"\\.\pipe\Rinflow.Agent.{user_sid}")
}

/// `rinflow-agent --quit`: asks the agent running for this user to quit,
/// as the tray's 退出 does, and waits until it has (its end of the pipe
/// closes as the process exits), at most `wait`. Setup runs this before it
/// replaces the programs, so peers see a clean disconnect instead of the
/// process being ended. True when no agent runs any more.
pub fn quit_running_agent(user_sid: &str, wait: std::time::Duration) -> bool {
    use std::io::{Read, Write};
    let pipe = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name(user_sid))
    {
        Ok(pipe) => pipe,
        // No agent (or not answering): nothing to wait for.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(error) => {
            eprintln!("rinflow-agent: cannot reach the running agent: {error}");
            return false;
        }
    };
    // An agent of another version answers `hello` with an error, but still
    // quits: `quit` has not changed since protocol 6.
    let request =
        format!("{{\"type\":\"hello\",\"protocol\":{PROTOCOL_VERSION}}}\n{{\"type\":\"quit\"}}\n");
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut pipe = pipe;
        if pipe.write_all(request.as_bytes()).is_ok() {
            // Snapshots until the agent's end closes.
            let mut buffer = [0u8; 64 * 1024];
            while matches!(pipe.read(&mut buffer), Ok(read) if read > 0) {}
        }
        let _ = done.send(());
    });
    finished.recv_timeout(wait).is_ok()
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello {
        protocol: u32,
    },
    Connect {
        peer_id: String,
    },
    Disconnect {
        peer_id: String,
    },
    /// Places a peer near this offset (logical points, this PC's space);
    /// the core snaps it to a valid spot and syncs it.
    SetPlacement {
        peer_id: String,
        x: f64,
        y: f64,
    },
    RemovePlacement {
        peer_id: String,
    },
    /// Where `set_placement` with this offset would put the peer, without
    /// placing it: answered with `placement_preview`.
    PreviewPlacement {
        peer_id: String,
        x: f64,
        y: f64,
    },
    PlaceBeside {
        peer_id: String,
        side: Side,
    },
    SetEffect {
        params: EffectParams,
    },
    ResetEffect,
    /// The demo belongs to the client that enabled it and stops when that
    /// client disconnects, so closing the settings window ends it.
    SetEffectDemo {
        enabled: bool,
    },
    EffectPreview {
        #[serde(default)]
        hold: Option<f32>,
    },
    SetClipboardSync {
        enabled: bool,
    },
    /// Some keys of `settings` (see `settings.rs`), and `launch_at_login`.
    UpdateSettings {
        values: Map<String, Value>,
    },
    ForgetDevice {
        peer_id: String,
    },
    /// The user compared the pairing code (snapshot `pairing`) and accepts.
    AcceptPairing {
        peer_id: String,
    },
    RejectPairing {
        peer_id: String,
    },
    /// The user dismissed `last_error`.
    ClearError,
    /// Stops files being received or sent (`transfers`).
    CancelTransfer {
        transfer: u32,
    },
    /// Quits the agent as the tray's 退出 does.
    Quit,
    /// Looks for a newer version now (snapshot `update`).
    CheckUpdate,
    /// Runs the setup program of the update that is ready.
    InstallUpdate,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message<'a> {
    Hello {
        protocol: u32,
        agent_version: &'static str,
    },
    State(&'a Snapshot),
    Error {
        message: String,
    },
    /// Where a dragged peer would land (`null` when nowhere).
    PlacementPreview {
        peer_id: String,
        offset: Option<Offset>,
    },
}

pub fn spawn(user_sid: &str, services: Arc<Services>) {
    let user_sid = user_sid.to_owned();
    let state = services.state.clone();
    let result = std::thread::Builder::new()
        .name("rinflow-ipc".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    state.set_error(format!("IPC runtime failed: {error}"));
                    return;
                }
            };
            if let Err(error) = runtime.block_on(serve(&user_sid, services)) {
                log!("IPC stopped: {error}");
                state.set_error(format!("IPC stopped: {error}"));
            }
        });
    if let Err(error) = result {
        log!("cannot start IPC thread: {error}");
    }
}

async fn serve(user_sid: &str, services: Arc<Services>) -> io::Result<()> {
    let name = pipe_name(user_sid);
    let mut security = PipeSecurity::for_user(user_sid)?;

    // The first instance flag makes creation fail if another process already
    // owns the name, so nothing can squat on the pipe before the agent.
    let mut server = create_instance(&name, &mut security, true)?;
    loop {
        // A client that went away before being accepted fails only its own
        // connection; the next instance exists before this one is let go, so
        // the name stays this process's.
        let connected = server.connect().await;
        let next = loop {
            match create_instance(&name, &mut security, false) {
                Ok(next) => break next,
                Err(error) => {
                    log!("cannot open another IPC pipe: {error}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        };
        let current = std::mem::replace(&mut server, next);
        match connected {
            Ok(()) => {
                tokio::spawn(handle_client(current, services.clone()));
            }
            Err(error) => log!("IPC client failed to connect: {error}"),
        }
    }
}

fn create_instance(
    name: &str,
    security: &mut PipeSecurity,
    first: bool,
) -> io::Result<NamedPipeServer> {
    // SAFETY: the security attributes outlive the call and stay valid for the
    // lifetime of `security`, which the caller keeps for every instance.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, security.as_mut_ptr())
    }
}

async fn handle_client(pipe: NamedPipeServer, services: Arc<Services>) {
    let mut owns_demo = false;
    serve_client(pipe, &services, &mut owns_demo).await;
    if owns_demo {
        let _ = services.set_effect_demo(false);
    }
}

async fn serve_client(pipe: NamedPipeServer, services: &Services, owns_demo: &mut bool) {
    let (reader, mut writer) = tokio::io::split(pipe);
    let mut reader = BufReader::new(reader);
    // Kept across loop turns: what `read_line` read before `select!` took
    // the other branch stays here.
    let mut pending = Vec::new();
    let mut snapshots = services.state.subscribe();

    let hello = Message::Hello {
        protocol: PROTOCOL_VERSION,
        agent_version: env!("CARGO_PKG_VERSION"),
    };
    if write(&mut writer, &hello).await.is_err() {
        return;
    }
    let snapshot = snapshots.borrow_and_update().clone();
    if write(&mut writer, &Message::State(&snapshot))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            line = read_line(&mut reader, &mut pending, MAX_REQUEST_LINE) => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        log!("settings app connection dropped: {error}");
                        break;
                    }
                };
                let reply = match handle_request(&line, services, owns_demo) {
                    Ok(reply) => reply,
                    Err(message) => Some(Message::Error { message }),
                };
                if let Some(reply) = reply
                    && write(&mut writer, &reply).await.is_err()
                {
                    break;
                }
            }
            changed = snapshots.changed() => {
                if changed.is_err() {
                    break;
                }
                let snapshot = snapshots.borrow_and_update().clone();
                if write(&mut writer, &Message::State(&snapshot)).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// Acts on one request; some are answered with a message of their own.
fn handle_request(
    line: &str,
    services: &Services,
    owns_demo: &mut bool,
) -> Result<Option<Message<'static>>, String> {
    let request = serde_json::from_str::<Request>(line)
        .map_err(|error| format!("invalid request: {error}"))?;
    let core = || {
        services
            .core
            .upgrade()
            .ok_or("Rinflow core is shutting down")
    };
    let done = match request {
        Request::PreviewPlacement { peer_id, x, y } => {
            let offset = services.preview_placement(&peer_id, Offset { x, y });
            return Ok(Some(Message::PlacementPreview { peer_id, offset }));
        }
        Request::Hello { protocol } if protocol == PROTOCOL_VERSION => Ok(()),
        Request::Hello { protocol } => Err(format!(
            "settings app speaks protocol {protocol}, agent speaks {PROTOCOL_VERSION}"
        )),
        Request::Connect { peer_id } => core()?.connect(&peer_id),
        Request::Disconnect { peer_id } => core()?.disconnect_peer(&peer_id),
        Request::SetPlacement { peer_id, x, y } => {
            core()?.set_placement(&peer_id, Some(Offset { x, y }))
        }
        Request::RemovePlacement { peer_id } => core()?.set_placement(&peer_id, None),
        Request::PlaceBeside { peer_id, side } => core()?.place_beside(&peer_id, side),
        Request::SetEffect { params } => {
            services.set_effect(params);
            Ok(())
        }
        Request::ResetEffect => {
            services.reset_effect();
            Ok(())
        }
        Request::SetEffectDemo { enabled } => {
            services.set_effect_demo(enabled)?;
            *owns_demo = enabled;
            Ok(())
        }
        Request::EffectPreview { hold } => {
            services.preview_effect(hold);
            Ok(())
        }
        Request::SetClipboardSync { enabled } => {
            services.set_clipboard_sync(enabled);
            Ok(())
        }
        Request::UpdateSettings { values } => services.update_settings(values),
        Request::ForgetDevice { peer_id } => services.forget_device(&peer_id),
        Request::AcceptPairing { peer_id } => services.accept_pairing(&peer_id),
        Request::RejectPairing { peer_id } => services.reject_pairing(&peer_id),
        Request::CancelTransfer { transfer } => core()?.cancel_files(transfer),
        Request::ClearError => {
            services.state.clear_error();
            Ok(())
        }
        Request::Quit => {
            crate::tray::quit();
            Ok(())
        }
        Request::CheckUpdate => {
            services.update.check();
            Ok(())
        }
        Request::InstallUpdate => {
            services.update.install();
            Ok(())
        }
    };
    done.map(|()| None)
}

/// Reads the next line, without its `\n` (or `\r\n`); `None` at the end of
/// the stream. A line longer than `limit` bytes, or one that is not UTF-8,
/// is an error: the client is not following the protocol.
///
/// Cancel safe, so it can sit in `select!`: the only await is `fill_buf`,
/// and bytes taken from the buffer are moved into `pending` at once, so a
/// line split across turns is picked up where it was left.
async fn read_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    pending: &mut Vec<u8>,
    limit: usize,
) -> io::Result<Option<String>> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // A last line without `\n` is incomplete: dropped, as the
            // connection is gone anyway.
            return Ok(None);
        }
        let (taken, complete) = match available.iter().position(|&byte| byte == b'\n') {
            Some(end) => (end, true),
            None => (available.len(), false),
        };
        if pending.len() + taken > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("request line longer than {limit} bytes"),
            ));
        }
        pending.extend_from_slice(&available[..taken]);
        reader.consume(taken + usize::from(complete));
        if complete {
            if pending.last() == Some(&b'\r') {
                pending.pop();
            }
            let line = String::from_utf8(std::mem::take(pending))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            return Ok(Some(line));
        }
    }
}

async fn write(writer: &mut (impl AsyncWrite + Unpin), message: &Message<'_>) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::read_line;
    use std::io;
    use tokio::io::BufReader;

    /// Reads every line of `input` through a buffer of `capacity` bytes.
    fn lines(input: &[u8], capacity: usize, limit: usize) -> Vec<io::Result<Option<String>>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut reader = BufReader::with_capacity(capacity, input);
            let mut pending = Vec::new();
            let mut results = Vec::new();
            loop {
                let result = read_line(&mut reader, &mut pending, limit).await;
                let last = !matches!(result, Ok(Some(_)));
                results.push(result);
                if last {
                    return results;
                }
            }
        })
    }

    fn texts(results: &[io::Result<Option<String>>]) -> Vec<Option<&str>> {
        results
            .iter()
            .map(|result| result.as_ref().unwrap().as_deref())
            .collect()
    }

    #[test]
    fn splits_lines_across_small_buffers() {
        let results = lines(
            b"{\"type\":\"quit\"}\r\n\n\xe4\xb8\xad\xe6\x96\x87\nrest",
            3,
            64,
        );
        assert_eq!(
            texts(&results),
            [Some("{\"type\":\"quit\"}"), Some(""), Some("中文"), None]
        );
    }

    #[test]
    fn refuses_a_line_over_the_limit() {
        let results = lines(b"12345678\n", 4, 8);
        assert_eq!(texts(&results), [Some("12345678"), None]);

        let results = lines(b"ok\n123456789\n", 4, 8);
        assert_eq!(results[0].as_ref().unwrap().as_deref(), Some("ok"));
        let error = results[1].as_ref().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn refuses_a_long_line_without_a_newline() {
        let input = vec![b'x'; 10_000];
        let results = lines(&input, 1024, 4096);
        let error = results[0].as_ref().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn refuses_invalid_utf8() {
        let results = lines(b"\xff\xfe\n", 16, 64);
        let error = results[0].as_ref().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

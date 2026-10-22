//! Named-pipe endpoint for the settings app.
//!
//! Each message is one UTF-8 JSON object per line. The app sends requests
//! (`hello`, `connect`, `set_effect`, …); the agent answers `hello` and then
//! pushes a full `state` snapshot whenever anything changes. See the README
//! for the message shapes.

use crate::effect::EffectParams;
use crate::services::Services;
use crate::state::Snapshot;
use crate::win::PipeSecurity;
use linkcursor_core::layout::{Offset, Side};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
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
pub const PROTOCOL_VERSION: u32 = 10;

pub fn pipe_name(user_sid: &str) -> String {
    format!(r"\\.\pipe\LinkCursor.Agent.{user_sid}")
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
    /// Quits the agent as the tray's 退出 does.
    Quit,
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
        .name("linkcursor-ipc".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    state.set_error(format!("IPC runtime failed: {error}"));
                    return;
                }
            };
            if let Err(error) = runtime.block_on(serve(&user_sid, services)) {
                eprintln!("linkcursor-agent: IPC stopped: {error}");
                state.set_error(format!("IPC stopped: {error}"));
            }
        });
    if let Err(error) = result {
        eprintln!("linkcursor-agent: cannot start IPC thread: {error}");
    }
}

async fn serve(user_sid: &str, services: Arc<Services>) -> io::Result<()> {
    let name = pipe_name(user_sid);
    let mut security = PipeSecurity::for_user(user_sid)?;

    // The first instance flag makes creation fail if another process already
    // owns the name, so nothing can squat on the pipe before the agent.
    let mut server = create_instance(&name, &mut security, true)?;
    loop {
        server.connect().await?;
        let connected = server;
        server = create_instance(&name, &mut security, false)?;
        tokio::spawn(handle_client(connected, services.clone()));
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
    let mut lines = BufReader::new(reader).lines();
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
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
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
            .ok_or("LinkCursor core is shutting down")
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
        Request::Quit => {
            crate::tray::quit();
            Ok(())
        }
    };
    done.map(|()| None)
}

async fn write(writer: &mut (impl AsyncWrite + Unpin), message: &Message<'_>) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

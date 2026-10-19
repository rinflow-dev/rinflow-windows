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
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

pub const PROTOCOL_VERSION: u32 = 2;

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
    Disconnect,
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
                if let Err(message) = handle_request(&line, services, owns_demo)
                    && write(&mut writer, &Message::Error { message }).await.is_err()
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

fn handle_request(line: &str, services: &Services, owns_demo: &mut bool) -> Result<(), String> {
    let request = serde_json::from_str::<Request>(line)
        .map_err(|error| format!("invalid request: {error}"))?;
    let core = || {
        services
            .core
            .upgrade()
            .ok_or("LinkCursor core is shutting down")
    };
    match request {
        Request::Hello { protocol } if protocol == PROTOCOL_VERSION => Ok(()),
        Request::Hello { protocol } => Err(format!(
            "settings app speaks protocol {protocol}, agent speaks {PROTOCOL_VERSION}"
        )),
        Request::Connect { peer_id } => core()?.connect(&peer_id),
        Request::Disconnect => core()?.disconnect(),
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
    }
}

async fn write(writer: &mut (impl AsyncWrite + Unpin), message: &Message<'_>) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

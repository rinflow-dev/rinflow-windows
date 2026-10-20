use crate::effect::EffectParams;
use linkcursor_core::layout::{Display, Offset, Side};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub platform: String,
    pub address: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreStatus {
    Starting,
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Connecting,
    Connected,
}

/// One peer this PC is connecting or connected to. The core keeps one
/// session per peer and several at once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Session {
    pub peer_id: String,
    /// The peer's name, or its ID until it is known.
    pub name: String,
    pub status: SessionStatus,
    /// This peer is currently driving this PC's mouse and keyboard.
    pub remote_control: bool,
}

/// The arrangement page: this PC's displays and every known peer, all in
/// logical points; placed peers carry their offset in this PC's space.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LayoutView {
    pub displays: Vec<Display>,
    pub devices: Vec<DeviceView>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceView {
    pub peer_id: String,
    pub name: String,
    pub platform: String,
    /// The peer's displays in its own space (a default size until known).
    pub displays: Vec<Display>,
    pub offset: Option<Offset>,
    /// The side of this PC the peer is attached to, if it touches one.
    pub side: Option<Side>,
}

/// Which side of the simulated edge the cursor is on in the effect demo.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoSide {
    Local,
    Remote,
}

/// Everything the settings app renders. The agent pushes the whole snapshot
/// on every change, so the app never has to replay individual core events.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    pub core: CoreStatus,
    pub peers: Vec<Peer>,
    pub sessions: Vec<Session>,
    pub layout: LayoutView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub effect: EffectParams,
    /// Changes when the parameters change for a reason other than the app's
    /// own `set_effect` (start-up, reset), telling the app to reload them.
    pub effect_revision: u64,
    pub effect_demo: bool,
    pub demo_side: DemoSide,
}

#[derive(Clone)]
pub struct SharedState(Arc<watch::Sender<Snapshot>>);

impl SharedState {
    pub fn new(device_id: &str, device_name: &str, effect: EffectParams) -> Self {
        Self(Arc::new(watch::Sender::new(Snapshot {
            device_id: device_id.to_owned(),
            device_name: device_name.to_owned(),
            core: CoreStatus::Starting,
            peers: Vec::new(),
            sessions: Vec::new(),
            layout: LayoutView::default(),
            last_error: None,
            effect,
            effect_revision: 1,
            effect_demo: false,
            demo_side: DemoSide::Local,
        })))
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.0.subscribe()
    }

    /// `reload` bumps the revision so the settings app re-reads the values.
    pub fn set_effect(&self, effect: EffectParams, reload: bool) {
        self.0.send_if_modified(|snapshot| {
            if snapshot.effect == effect && !reload {
                return false;
            }
            snapshot.effect = effect;
            if reload {
                snapshot.effect_revision += 1;
            }
            true
        });
    }

    pub fn set_layout(&self, layout: LayoutView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.layout, layout));
    }

    pub fn set_effect_demo(&self, enabled: bool) {
        self.0.send_if_modified(|snapshot| {
            let changed = replace(&mut snapshot.effect_demo, enabled);
            if !enabled {
                snapshot.demo_side = DemoSide::Local;
            }
            changed
        });
    }

    pub fn set_demo_side(&self, side: DemoSide) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.demo_side, side));
    }

    pub fn set_error(&self, message: impl Into<String>) {
        let message = message.into();
        self.0
            .send_modify(|snapshot| snapshot.last_error = Some(message));
    }

    /// Folds one JSON event from `linkcursor_core::Core` into the snapshot.
    /// Runs on the core's threads.
    pub fn apply_core_event(&self, json: &str) {
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Some(kind) = event["event"].as_str() else {
            return;
        };
        self.0.send_if_modified(|snapshot| match kind {
            "started" => replace(&mut snapshot.core, CoreStatus::Running),
            "stopped" => replace(&mut snapshot.core, CoreStatus::Stopped),
            "peer_up" => {
                let Some(peer) = parse_peer(&event["peer"]) else {
                    return false;
                };
                if snapshot.peers.iter().any(|known| known.id == peer.id) {
                    return false;
                }
                snapshot.peers.push(peer);
                snapshot.peers.sort_by_key(|peer| peer.name.to_lowercase());
                true
            }
            "peer_down" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let before = snapshot.peers.len();
                snapshot.peers.retain(|peer| peer.id != peer_id);
                snapshot.peers.len() != before
            }
            "connecting" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let known = snapshot.sessions.iter().any(|s| s.peer_id == peer_id);
                let session = snapshot.session_mut(peer_id);
                replace(&mut session.status, SessionStatus::Connecting) || !known
            }
            "connected" => {
                let Some(peer) = parse_peer(&event["peer"]) else {
                    return false;
                };
                let session = snapshot.session_mut(&peer.id);
                session.name = peer.name;
                session.status = SessionStatus::Connected;
                snapshot.last_error = None;
                true
            }
            "remote_control_started" | "remote_control_ended" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let active = kind == "remote_control_started";
                match snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    Some(session) => replace(&mut session.remote_control, active),
                    None => false,
                }
            }
            "disconnected" => {
                // Sessions are per peer; a disconnect without a peer ID does
                // not concern any session shown here.
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let name = snapshot
                    .sessions
                    .iter()
                    .find(|session| session.peer_id == peer_id)
                    .map(|session| session.name.clone())
                    .unwrap_or_else(|| peer_id.to_owned());
                snapshot
                    .sessions
                    .retain(|session| session.peer_id != peer_id);
                // Ending the session here or on the peer is not an error.
                let reason = event["reason"].as_str().unwrap_or("disconnected");
                if reason != "local request" && reason != "peer closed the connection" {
                    snapshot.last_error = Some(format!("{name}: {reason}"));
                }
                true
            }
            "error" => {
                snapshot.last_error = event["message"].as_str().map(str::to_owned);
                true
            }
            _ => false,
        });
    }
}

impl Snapshot {
    /// The session with `peer_id`, added as `connecting` if there is none.
    fn session_mut(&mut self, peer_id: &str) -> &mut Session {
        let index = match self.sessions.iter().position(|s| s.peer_id == peer_id) {
            Some(index) => index,
            None => {
                let name = self
                    .peers
                    .iter()
                    .find(|peer| peer.id == peer_id)
                    .map(|peer| peer.name.clone())
                    .unwrap_or_else(|| peer_id.to_owned());
                self.sessions.push(Session {
                    peer_id: peer_id.to_owned(),
                    name,
                    status: SessionStatus::Connecting,
                    remote_control: false,
                });
                self.sessions.len() - 1
            }
        };
        &mut self.sessions[index]
    }
}

fn replace<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        return false;
    }
    *slot = value;
    true
}

fn parse_peer(value: &Value) -> Option<Peer> {
    serde_json::from_value(value.clone()).ok()
}

use crate::effect::EffectParams;
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

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Connection {
    Disconnected,
    Connecting { peer_id: String },
    Connected { peer: Peer },
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
    pub connection: Connection,
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
            connection: Connection::Disconnected,
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
                snapshot.connection = Connection::Connecting {
                    peer_id: peer_id.to_owned(),
                };
                true
            }
            "connected" => {
                let Some(peer) = parse_peer(&event["peer"]) else {
                    return false;
                };
                snapshot.connection = Connection::Connected { peer };
                snapshot.last_error = None;
                true
            }
            "disconnected" => {
                snapshot.connection = Connection::Disconnected;
                let reason = event["reason"].as_str().unwrap_or("disconnected");
                if reason != "local request" {
                    snapshot.last_error = Some(reason.to_owned());
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

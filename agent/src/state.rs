use crate::effect::EffectParams;
use crate::settings::Settings;
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
    /// Round trip to the peer, in whole milliseconds, once measured.
    pub latency_ms: Option<u32>,
    /// The peer's certificate is the one paired with.
    pub paired: bool,
}

/// Pairing in progress: the code both devices show.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PairingView {
    pub peer_id: String,
    pub name: String,
    pub platform: String,
    pub code: String,
    /// This side's user accepted; waiting for the other device.
    pub accepted: bool,
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
    /// One of the user's devices: connected at least once, or placed.
    /// Others are only nearby (the app offers them under 添加设备).
    pub remembered: bool,
    /// When the peer was last seen online (Unix milliseconds), if ever.
    pub last_seen_ms: Option<u64>,
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
    /// The clipboard is shared with peers (see `clipboard.rs`).
    pub clipboard_sync: bool,
    /// Preferences (see `settings.rs`).
    pub settings: Settings,
    /// The agent starts when the user signs in.
    pub launch_at_login: bool,
    /// The port the core listens on now; `settings.port` applies after a
    /// restart.
    pub control_port: u16,
    /// Locked to the current screen with the lock shortcut: no crossing.
    pub locked: bool,
    pub agent_version: &'static str,
    /// SHA-256 of this PC's certificate (hex), once the core runs with QUIC.
    pub fingerprint: Option<String>,
    /// A pairing waiting for the user.
    pub pairing: Option<PairingView>,
    /// Device IDs of paired peers.
    pub paired_peers: Vec<String>,
    /// The LocalSystem input helper (see `elevated.rs`).
    pub elevated_input: ElevatedInputView,
    /// The user quit LinkCursor (tray 退出): the app quits too instead of
    /// starting the agent again.
    pub quitting: bool,
}

/// Whether input can reach elevated windows, UAC prompts and the lock screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ElevatedInputView {
    /// The helper runs (the service is installed and started).
    pub available: bool,
    /// The machine-wide setting is on.
    pub enabled: bool,
}

#[derive(Clone)]
pub struct SharedState(Arc<watch::Sender<Snapshot>>);

impl SharedState {
    pub fn new(
        device_id: &str,
        device_name: &str,
        effect: EffectParams,
        settings: Settings,
        launch_at_login: bool,
        control_port: u16,
    ) -> Self {
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
            clipboard_sync: settings.clipboard,
            settings,
            launch_at_login,
            control_port,
            locked: false,
            agent_version: env!("CARGO_PKG_VERSION"),
            fingerprint: None,
            pairing: None,
            paired_peers: Vec::new(),
            elevated_input: ElevatedInputView::default(),
            quitting: false,
        })))
    }

    pub fn set_quitting(&self) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.quitting, true));
    }

    pub fn set_elevated_input(&self, view: ElevatedInputView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.elevated_input, view));
    }

    pub fn set_paired_peers(&self, peers: Vec<String>) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.paired_peers, peers));
    }

    /// This side's user accepted the pairing with `peer_id`.
    pub fn set_pairing_accepted(&self, peer_id: &str) {
        self.0
            .send_if_modified(|snapshot| match &mut snapshot.pairing {
                Some(pairing) if pairing.peer_id == peer_id => replace(&mut pairing.accepted, true),
                _ => false,
            });
    }

    pub fn set_settings(&self, settings: Settings) {
        self.0.send_if_modified(|snapshot| {
            let clipboard = replace(&mut snapshot.clipboard_sync, settings.clipboard);
            replace(&mut snapshot.settings, settings) || clipboard
        });
    }

    pub fn set_launch_at_login(&self, enabled: bool) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.launch_at_login, enabled));
    }

    pub fn set_locked(&self, locked: bool) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.locked, locked));
    }

    pub fn clear_error(&self) {
        self.0
            .send_if_modified(|snapshot| snapshot.last_error.take().is_some());
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.0.subscribe()
    }

    /// Whether a session with the peer is established.
    pub fn is_connected(&self, peer_id: &str) -> bool {
        self.0
            .borrow()
            .sessions
            .iter()
            .any(|session| session.peer_id == peer_id && session.status == SessionStatus::Connected)
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
            "started" => {
                let fingerprint = event["fingerprint"].as_str().map(str::to_owned);
                replace(&mut snapshot.core, CoreStatus::Running)
                    | replace(&mut snapshot.fingerprint, fingerprint)
            }
            "pairing_requested" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let text = |key: &str| event[key].as_str().unwrap_or_default().to_owned();
                snapshot.pairing = Some(PairingView {
                    peer_id: peer_id.to_owned(),
                    name: event["name"].as_str().unwrap_or(peer_id).to_owned(),
                    platform: text("platform"),
                    code: text("code"),
                    accepted: false,
                });
                true
            }
            "paired" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                if !snapshot.paired_peers.iter().any(|id| id == peer_id) {
                    snapshot.paired_peers.push(peer_id.to_owned());
                }
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| p.peer_id == peer_id)
                {
                    snapshot.pairing = None;
                }
                if let Some(session) = snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    session.paired = true;
                }
                true
            }
            "unpaired" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                let before = snapshot.paired_peers.len();
                snapshot.paired_peers.retain(|id| id != peer_id);
                snapshot.paired_peers.len() != before
            }
            "pairing_ended" => {
                let peer_id = event["peer_id"].as_str();
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| Some(p.peer_id.as_str()) == peer_id)
                {
                    snapshot.pairing = None;
                    return true;
                }
                false
            }
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
                session.paired = event["paired"].as_bool().unwrap_or(false);
                snapshot.last_error = None;
                true
            }
            "latency" => {
                let (Some(peer_id), Some(ms)) = (event["peer_id"].as_str(), event["ms"].as_f64())
                else {
                    return false;
                };
                let ms = ms.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                match snapshot.sessions.iter_mut().find(|s| s.peer_id == peer_id) {
                    Some(session) => replace(&mut session.latency_ms, Some(ms)),
                    None => false,
                }
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
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| p.peer_id == peer_id)
                {
                    snapshot.pairing = None;
                }
                let reason = event["reason"].as_str().unwrap_or("disconnected");
                if let Some(message) = disconnect_message(reason) {
                    snapshot.last_error = Some(format!("{name}：{message}"));
                }
                true
            }
            "error" => {
                snapshot.last_error = event["message"].as_str().map(str::to_owned);
                true
            }
            "files_failed" => {
                let reason = event["reason"].as_str().unwrap_or("unknown error");
                let reason = match reason {
                    // Declined here (or sharing is off), or the peer copied
                    // something else meanwhile: nothing went wrong.
                    "cancelled"
                    | "replaced by a newer copy"
                    | "files are no longer on the clipboard" => {
                        return false;
                    }
                    "connection lost" => "连接断开了",
                    other => other,
                };
                snapshot.last_error = Some(format!("接收文件失败：{reason}"));
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
                    latency_ms: None,
                    paired: false,
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

/// What to tell the user about a session that ended, if anything: ending it
/// here or on the peer, or declining a pairing here, is not an error.
fn disconnect_message(reason: &str) -> Option<String> {
    Some(match reason {
        "local request"
        | "peer closed the connection"
        | "disconnected on the other device"
        | "going to sleep"
        | "pairing declined" => return None,
        // The peer vanished without closing (slept, lost the network); the
        // core reconnects once it is back.
        "heartbeat timeout" => {
            "对方没有响应（可能睡眠了或断网），回来后会自动重新连接。".to_owned()
        }
        "refused by the other device" => {
            "对方拒绝了连接：对方可能移除了本机，或只允许已配对的设备。在设备菜单里点“连接”可以重新配对。"
                .to_owned()
        }
        "the device's identity changed" => {
            "设备的身份证书变了，已拒绝连接。如果对方重装过，请在设备菜单里移除它再重新配对。"
                .to_owned()
        }
        "pairing declined on the other device" => "对方拒绝了配对。".to_owned(),
        "pairing timed out" => "配对超时，没有在两分钟内确认。".to_owned(),
        "pairing required" => "未配对的设备已断开（已开启“仅允许已配对设备”）。".to_owned(),
        "unpaired" => "已取消配对。".to_owned(),
        other => other.to_owned(),
    })
}

fn parse_peer(value: &Value) -> Option<Peer> {
    serde_json::from_value(value.clone()).ok()
}

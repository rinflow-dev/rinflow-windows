// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::effect::EffectParams;
use crate::i18n::{s, tr, trf};
use crate::settings::Settings;
use crate::update::UpdateView;
use rinflow_core::layout::{Display, Offset, Side};
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

/// Files on their way to or from a peer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransferView {
    pub transfer: u32,
    pub peer_id: String,
    /// The peer's name, or its ID.
    pub peer_name: String,
    /// `receiving` or `sending`.
    pub direction: &'static str,
    /// The first top-level item, how many top-level items, how many files.
    pub name: String,
    pub items: u64,
    pub count: u64,
    /// Bytes moved so far, of `total`.
    pub done: u64,
    pub total: u64,
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
    /// The user quit Rinflow (tray 退出): the app quits too instead of
    /// starting the agent again.
    pub quitting: bool,
    /// The addresses the user added and how each fares: the core's
    /// `manual_addresses` entries (`address`, `status`, `peer_id`, `name`,
    /// `error`).
    pub manual_addresses: Vec<Value>,
    /// Files on their way, either way.
    pub transfers: Vec<TransferView>,
    /// Another program took one or more of the switch shortcuts first: the
    /// settings page suggests the other combination.
    pub switch_hotkeys_blocked: bool,
    /// A newer version: found, downloading, ready to install (see `update.rs`).
    pub update: UpdateView,
    /// Peers that offer a picture to watch (a KVM: the screen of the
    /// computer behind it), in the order they offered it (see `viewer`).
    pub media_peers: Vec<String>,
}

/// Whether input can reach elevated windows, UAC prompts and the lock screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ElevatedInputView {
    /// The helper runs (the service is installed and started).
    pub available: bool,
    /// The machine-wide setting is on.
    pub enabled: bool,
    /// The service can be installed when the user turns the setting on: this
    /// copy was installed into Program Files, with the service beside it.
    /// Setup installs the service only while the setting is on.
    pub installable: bool,
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
            manual_addresses: Vec::new(),
            transfers: Vec::new(),
            switch_hotkeys_blocked: false,
            update: UpdateView::default(),
            media_peers: Vec::new(),
        })))
    }

    pub fn set_update(&self, view: UpdateView) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.update, view));
    }

    /// Another program took one or more of the switch shortcuts first.
    pub fn set_switch_hotkeys_blocked(&self, blocked: bool) {
        self.0
            .send_if_modified(|snapshot| replace(&mut snapshot.switch_hotkeys_blocked, blocked));
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

    /// Folds one JSON event from `rinflow_core::Core` into the snapshot.
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
                snapshot
                    .transfers
                    .retain(|transfer| transfer.peer_id != peer_id);
                if snapshot
                    .pairing
                    .as_ref()
                    .is_some_and(|p| p.peer_id == peer_id)
                {
                    snapshot.pairing = None;
                }
                let reason = event["reason"].as_str().unwrap_or("disconnected");
                if let Some(message) = disconnect_message(reason) {
                    snapshot.last_error = Some(trf(s::PeerProblem, &[&name, &message]));
                }
                true
            }
            "error" => {
                snapshot.last_error = event["message"].as_str().map(str::to_owned);
                true
            }
            // Copied files past the core's limits: the page says to zip them.
            "files_not_shared" => {
                snapshot.last_error = Some(crate::clipboard::files_not_shared_text(&event));
                true
            }
            "files_failed" | "files_stream_ended" | "files_send_ended" => {
                let direction = if kind != "files_send_ended" {
                    "receiving"
                } else {
                    "sending"
                };
                let removed = snapshot.remove_transfer(&event, direction);
                let reason = event["reason"].as_str().unwrap_or("unknown error");
                let reason = match reason {
                    // Done, declined here (or sharing is off), or the peer
                    // copied something else meanwhile: nothing went wrong.
                    "done"
                    | "delivered"
                    | "released"
                    | "cancelled"
                    | "replaced by a newer copy"
                    | "files are no longer on the clipboard" => {
                        return removed;
                    }
                    "connection lost" => tr(s::TransferConnectionLost),
                    other => other,
                };
                let failed = if direction == "receiving" {
                    s::ReceiveFilesFailed
                } else {
                    s::SendFilesFailed
                };
                snapshot.last_error = Some(trf(failed, &[&reason]));
                true
            }
            // Files come only through file streams (`files_offered` and
            // `files_received` belong to the old file protocol, declined).
            "files_receiving" | "files_sending" => {
                let direction = if kind != "files_sending" {
                    "receiving"
                } else {
                    "sending"
                };
                let (Some(peer_id), Some(transfer)) = (
                    event["peer_id"].as_str(),
                    event["transfer"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok()),
                ) else {
                    return false;
                };
                let number = |key: &str| event[key].as_u64().unwrap_or(0);
                let view = TransferView {
                    transfer,
                    peer_id: peer_id.to_owned(),
                    peer_name: snapshot.peer_name(peer_id),
                    direction,
                    name: event["name"].as_str().unwrap_or_default().to_owned(),
                    items: number("items"),
                    count: number("count"),
                    done: number("sent"),
                    total: if direction == "receiving" {
                        number("total_size")
                    } else {
                        number("total")
                    },
                };
                match snapshot
                    .transfers
                    .iter_mut()
                    .find(|t| t.transfer == transfer && t.direction == direction)
                {
                    Some(existing) => replace(existing, view),
                    None => {
                        snapshot.transfers.push(view);
                        true
                    }
                }
            }
            "files_progress" => {
                let (Some(transfer), Some(received)) = (
                    event["transfer"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok()),
                    event["received"].as_u64(),
                ) else {
                    return false;
                };
                match snapshot
                    .transfers
                    .iter_mut()
                    .find(|t| t.transfer == transfer && t.direction == "receiving")
                {
                    Some(existing) => replace(&mut existing.done, received),
                    None => false,
                }
            }
            "manual_addresses" => {
                let addresses = event["addresses"].as_array().cloned().unwrap_or_default();
                replace(&mut snapshot.manual_addresses, addresses)
            }
            "media_available" => {
                let Some(peer_id) = event["peer_id"].as_str() else {
                    return false;
                };
                if snapshot.media_peers.iter().any(|id| id == peer_id) {
                    return false;
                }
                snapshot.media_peers.push(peer_id.to_owned());
                true
            }
            "media_unavailable" => {
                let before = snapshot.media_peers.len();
                snapshot
                    .media_peers
                    .retain(|id| event["peer_id"].as_str() != Some(id));
                snapshot.media_peers.len() != before
            }
            _ => false,
        });
    }
}

impl Snapshot {
    /// The peer's name as known from its session or discovery.
    fn peer_name(&self, peer_id: &str) -> String {
        self.sessions
            .iter()
            .find(|session| session.peer_id == peer_id)
            .map(|session| session.name.clone())
            .or_else(|| {
                self.peers
                    .iter()
                    .find(|peer| peer.id == peer_id)
                    .map(|peer| peer.name.clone())
            })
            .unwrap_or_else(|| peer_id.to_owned())
    }

    /// Drops the transfer an event names; true if there was one.
    fn remove_transfer(&mut self, event: &Value, direction: &str) -> bool {
        let Some(transfer) = event["transfer"].as_u64() else {
            return false;
        };
        let before = self.transfers.len();
        self.transfers
            .retain(|t| !(u64::from(t.transfer) == transfer && t.direction == direction));
        self.transfers.len() != before
    }

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
        "heartbeat timeout" => tr(s::PeerNotResponding).to_owned(),
        "refused by the other device" => tr(s::PeerRefused).to_owned(),
        "the device's identity changed" => tr(s::PeerIdentityChanged).to_owned(),
        "pairing declined on the other device" => tr(s::PairingDeclined).to_owned(),
        "pairing timed out" => tr(s::PairingTimedOut).to_owned(),
        "pairing required" => tr(s::UnpairedDisconnected).to_owned(),
        "unpaired" => tr(s::Unpaired).to_owned(),
        other => other.to_owned(),
    })
}

fn parse_peer(value: &Value) -> Option<Peer> {
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectParams;

    fn state() -> SharedState {
        SharedState::new(
            "this-pc",
            "This PC",
            EffectParams::default(),
            Settings::default(),
            false,
            42421,
        )
    }

    fn apply(state: &SharedState, event: serde_json::Value) {
        state.apply_core_event(&event.to_string());
    }

    #[test]
    fn files_past_the_limits_ask_to_zip_them() {
        let state = state();
        apply(
            &state,
            serde_json::json!({"event": "files_not_shared", "reason": "too_many_files", "limit": 10000}),
        );
        let error = state.0.borrow().last_error.clone().unwrap();
        assert!(
            error.contains("10000") && error.to_lowercase().contains("zip"),
            "{error}"
        );
        apply(
            &state,
            serde_json::json!({"event": "files_not_shared", "reason": "too_deep", "limit": 32}),
        );
        let error = state.0.borrow().last_error.clone().unwrap();
        assert!(
            error.contains("32") && error.to_lowercase().contains("zip"),
            "{error}"
        );
    }

    #[test]
    fn transfers_show_while_files_move_either_way() {
        let state = state();
        apply(
            &state,
            serde_json::json!({"event": "connected", "peer": {"id": "mac", "name": "Mac mini", "address": "10.0.0.2", "port": 42421}, "paired": true}),
        );
        // An offer in the old file protocol is declined: no card.
        apply(
            &state,
            serde_json::json!({"event": "files_offered", "peer_id": "mac", "transfer": 6, "name": "旧", "items": 1, "count": 1, "total_size": 10}),
        );
        assert!(state.0.borrow().transfers.is_empty());
        apply(
            &state,
            serde_json::json!({"event": "files_receiving", "peer_id": "mac", "transfer": 7, "name": "报告", "items": 2, "count": 4, "total_size": 400}),
        );
        apply(
            &state,
            serde_json::json!({"event": "files_progress", "peer_id": "mac", "transfer": 7, "received": 100, "total": 400}),
        );
        apply(
            &state,
            serde_json::json!({"event": "files_sending", "peer_id": "mac", "transfer": 9, "name": "photo.jpg", "items": 1, "count": 1, "sent": 50, "total": 80}),
        );
        {
            let snapshot = state.0.borrow();
            assert_eq!(snapshot.transfers.len(), 2);
            let receiving = &snapshot.transfers[0];
            assert_eq!(
                (
                    receiving.direction,
                    receiving.peer_name.as_str(),
                    receiving.name.as_str()
                ),
                ("receiving", "Mac mini", "报告")
            );
            assert_eq!(
                (
                    receiving.items,
                    receiving.count,
                    receiving.done,
                    receiving.total
                ),
                (2, 4, 100, 400)
            );
            let sending = &snapshot.transfers[1];
            assert_eq!(
                (sending.direction, sending.done, sending.total),
                ("sending", 50, 80)
            );
        }
        // Done sending, quietly; received, quietly.
        apply(
            &state,
            serde_json::json!({"event": "files_send_ended", "peer_id": "mac", "transfer": 9, "reason": "done"}),
        );
        apply(
            &state,
            serde_json::json!({"event": "files_stream_ended", "peer_id": "mac", "transfer": 7, "reason": "done"}),
        );
        assert!(state.0.borrow().transfers.is_empty());
        assert_eq!(state.0.borrow().last_error, None);

        // A transfer that breaks off says so; one stopped on purpose does not.
        apply(
            &state,
            serde_json::json!({"event": "files_receiving", "peer_id": "mac", "transfer": 8, "name": "a", "items": 1, "count": 1, "total_size": 10}),
        );
        apply(
            &state,
            serde_json::json!({"event": "files_failed", "peer_id": "mac", "transfer": 8, "reason": "cancelled"}),
        );
        assert_eq!(state.0.borrow().last_error, None);
        apply(
            &state,
            serde_json::json!({"event": "files_sending", "peer_id": "mac", "transfer": 10, "name": "b", "items": 1, "count": 1, "sent": 0, "total": 10}),
        );
        apply(
            &state,
            serde_json::json!({"event": "files_send_ended", "peer_id": "mac", "transfer": 10, "reason": "disk full"}),
        );
        assert!(
            state
                .0
                .borrow()
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("disk full"))
        );

        // A peer that goes away takes its transfers with it.
        apply(
            &state,
            serde_json::json!({"event": "files_receiving", "peer_id": "mac", "transfer": 11, "name": "c", "items": 1, "count": 1, "total_size": 10}),
        );
        apply(
            &state,
            serde_json::json!({"event": "disconnected", "peer_id": "mac", "reason": "local request"}),
        );
        assert!(state.0.borrow().transfers.is_empty());
    }

    #[test]
    fn manual_address_states_pass_through() {
        let state = state();
        apply(
            &state,
            serde_json::json!({"event": "manual_addresses", "addresses": [{"address": "192.168.1.20", "status": "found", "peer_id": "mac", "name": "Mac mini"}]}),
        );
        let snapshot = state.0.borrow();
        assert_eq!(snapshot.manual_addresses.len(), 1);
        assert_eq!(snapshot.manual_addresses[0]["status"], "found");
    }
}

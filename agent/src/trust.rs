//! Paired peers, saved to `%LOCALAPPDATA%\LinkCursor\trusted.json`.
//!
//! The core decides who is trusted (see linkcursor-core's
//! `runtime/trust.rs`); this keeps what it reports across restarts: each
//! `paired` event adds the peer's certificate fingerprint, `unpaired`
//! removes it, and the list goes back to the core as
//! `CoreConfig::trusted_peers`.

use crate::identity;
use linkcursor_core::TrustedPeer;
use serde_json::Value;
use std::fs;
use std::sync::Mutex;

pub struct TrustStore(Mutex<Vec<TrustedPeer>>);

impl TrustStore {
    pub fn load() -> Self {
        let peers: Vec<TrustedPeer> = fs::read_to_string(path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // A hand-edited file must not stop the core from starting.
        let peers = peers
            .into_iter()
            .filter(|peer| {
                peer.fingerprint.len() == 64
                    && peer.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .collect();
        Self(Mutex::new(peers))
    }

    pub fn peers(&self) -> Vec<TrustedPeer> {
        self.0.lock().map(|peers| peers.clone()).unwrap_or_default()
    }

    /// Picks `paired` and `unpaired` out of the core's events.
    pub fn core_event(&self, json: &str) -> bool {
        if !json.contains("paired") {
            return false;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return false;
        };
        let Some(peer_id) = event["peer_id"].as_str() else {
            return false;
        };
        let Ok(mut peers) = self.0.lock() else {
            return false;
        };
        match event["event"].as_str() {
            Some("paired") => {
                let Some(fingerprint) = event["fingerprint"].as_str() else {
                    return false;
                };
                peers.retain(|peer| peer.peer_id != peer_id);
                peers.push(TrustedPeer {
                    peer_id: peer_id.to_owned(),
                    fingerprint: fingerprint.to_owned(),
                });
            }
            Some("unpaired") => {
                let before = peers.len();
                peers.retain(|peer| peer.peer_id != peer_id);
                if peers.len() == before {
                    return false;
                }
            }
            _ => return false,
        }
        save(&peers);
        true
    }
}

fn path() -> std::path::PathBuf {
    identity::data_directory().join("trusted.json")
}

fn save(peers: &[TrustedPeer]) {
    match serde_json::to_string_pretty(peers) {
        Ok(text) => {
            if let Err(error) = identity::write_file(&path(), &text) {
                eprintln!("linkcursor-agent: cannot save paired devices: {error}");
            }
        }
        Err(error) => eprintln!("linkcursor-agent: cannot encode paired devices: {error}"),
    }
}

/// Where this device's key pair and certificate live (see the core's
/// `secure.rs`): readable by this user only, like the rest of the folder.
pub fn identity_path() -> std::path::PathBuf {
    identity::data_directory().join("identity.pem")
}

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Paired peers: the `trusted` section of `config.json` (see `config.rs`).
//!
//! The core decides who is trusted (see rinflow-core's
//! `runtime/trust.rs`); this keeps what it reports across restarts: each
//! `paired` event adds the peer's certificate fingerprint, `unpaired`
//! removes it, and the list goes back to the core as
//! `CoreConfig::trusted_peers`.

use crate::{config, identity};
use rinflow_core::TrustedPeer;
use serde_json::Value;
use std::sync::Mutex;

pub struct TrustStore(Mutex<Vec<TrustedPeer>>);

impl TrustStore {
    pub fn load() -> Self {
        Self(Mutex::new(from_saved(
            config::section(config::TRUSTED).as_ref(),
        )))
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
        let pending = {
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
            config::stage(config::TRUSTED, &*peers)
        };
        pending.write();
        true
    }
}

/// The paired devices saved in `config.json`. Each entry is read on its
/// own: one edited by hand into something invalid is dropped alone, and
/// must not stop the core from starting.
fn from_saved(saved: Option<&Value>) -> Vec<TrustedPeer> {
    let Some(entries) = saved.and_then(Value::as_array) else {
        if saved.is_some() {
            log!("the saved paired devices are not a list; none are paired");
        }
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let peer = serde_json::from_value::<TrustedPeer>(entry.clone()).ok();
            let valid = peer.filter(|peer| {
                peer.fingerprint.len() == 64
                    && peer.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            });
            if valid.is_none() {
                log!("a saved paired device is not valid and was ignored");
            }
            valid
        })
        .collect()
}

/// Where this device's key pair and certificate live (see the core's
/// `secure.rs`): readable by this user only, like the rest of the folder.
/// Its own file, not part of `config.json`: it holds the private key, and
/// the core reads and writes it.
pub fn identity_path() -> std::path::PathBuf {
    identity::data_directory().join("identity.pem")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_entries_are_dropped_alone() {
        let good = "ab".repeat(32);
        let saved = json!([
            {"peer_id": "mac", "fingerprint": good},
            {"peer_id": "short", "fingerprint": "abc"},
            {"peer_id": 5},
            "text"
        ]);
        let peers = from_saved(Some(&saved));
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id, "mac");
        assert!(from_saved(Some(&json!({"mac": good}))).is_empty());
        assert!(from_saved(None).is_empty());
    }
}

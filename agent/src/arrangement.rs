//! The arrangement of this PC and its peers, as the settings app shows it.
//!
//! linkcursor-core owns placements: it snaps them, syncs them with peers and
//! reports every change as `placement_changed`. This module saves those
//! reports to `%LOCALAPPDATA%\LinkCursor\layout.json` (handed back to the core
//! at start-up), remembers peers' names and displays so placed devices still
//! show while offline, and derives the view and the physical portals.

use crate::displays::{self, Monitor, PhysicalPortal};
use crate::identity;
use crate::state::{DeviceView, LayoutView, SharedState};
use linkcursor_core::PlacementRecord;
use linkcursor_core::layout::{self, Display, Offset, Portal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    placements: Vec<PlacementRecord>,
    /// Every peer seen so far, by device ID.
    #[serde(default)]
    devices: BTreeMap<String, KnownDevice>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct KnownDevice {
    name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    displays: Vec<Display>,
}

pub struct Arrangement {
    inner: Mutex<Inner>,
    state: SharedState,
}

/// A placed peer as the pointer crosses into it.
pub struct Target {
    pub peer: Vec<Display>,
    pub offset: Offset,
    pub portals: Vec<Portal>,
}

struct Inner {
    saved: Saved,
    monitors: Vec<Monitor>,
    portals: Vec<PhysicalPortal>,
}

impl Arrangement {
    pub fn load(state: SharedState, monitors: Vec<Monitor>) -> Arc<Self> {
        let saved = fs::read_to_string(path())
            .ok()
            .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
            .unwrap_or_default();
        let arrangement = Arc::new(Self {
            inner: Mutex::new(Inner {
                saved,
                monitors,
                portals: Vec::new(),
            }),
            state,
        });
        if let Ok(mut inner) = arrangement.inner.lock() {
            arrangement.publish(&mut inner);
        }
        arrangement
    }

    /// Saved placements, for `CoreConfig::placements`.
    pub fn placements(&self) -> Vec<PlacementRecord> {
        self.inner
            .lock()
            .map(|inner| inner.saved.placements.clone())
            .unwrap_or_default()
    }

    pub fn set_monitors(&self, monitors: Vec<Monitor>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.monitors = monitors;
            self.publish(&mut inner);
        }
    }

    /// Portals on this PC's side, in physical pixels.
    pub fn portals(&self) -> Vec<PhysicalPortal> {
        self.inner
            .lock()
            .map(|inner| inner.portals.clone())
            .unwrap_or_default()
    }

    /// What controlling a placed peer needs: its displays (in its space), its
    /// offset in this PC's space, and every portal leading to it.
    pub fn target(&self, peer_id: &str) -> Option<Target> {
        let inner = self.inner.lock().ok()?;
        let offset = inner
            .saved
            .placements
            .iter()
            .find(|record| record.peer_id == peer_id)?
            .offset?;
        let known = inner
            .saved
            .devices
            .get(peer_id)
            .cloned()
            .unwrap_or_default();
        let peer = layout::peer_or_default(&known.displays);
        let portals = layout::portals(&displays::logical(&inner.monitors), &peer, offset);
        Some(Target {
            peer,
            offset,
            portals,
        })
    }

    /// Picks peer descriptions and placement changes out of the core's events.
    pub fn core_event(&self, json: &str) {
        if !(json.contains("peer_up")
            || json.contains("peer_displays")
            || json.contains("placement_changed"))
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        match event["event"].as_str() {
            Some("peer_up") => {
                let peer = &event["peer"];
                let Some(id) = peer["id"].as_str() else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                device.name = peer["name"].as_str().unwrap_or(id).to_owned();
                device.platform = peer["platform"].as_str().unwrap_or_default().to_owned();
                if let Ok(displays) =
                    serde_json::from_value::<Vec<Display>>(peer["displays"].clone())
                    && !displays.is_empty()
                {
                    device.displays = displays;
                }
            }
            Some("peer_displays") => {
                let Some(id) = event["peer_id"].as_str() else {
                    return;
                };
                let Ok(displays) =
                    serde_json::from_value::<Vec<Display>>(event["displays"].clone())
                else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                if device.name.is_empty() {
                    device.name = id.to_owned();
                }
                device.displays = displays;
            }
            Some("placement_changed") => {
                let Some(id) = event["peer_id"].as_str() else {
                    return;
                };
                let offset = serde_json::from_value::<Option<Offset>>(event["offset"].clone())
                    .unwrap_or(None);
                let updated_ms = event["updated_ms"].as_u64().unwrap_or_default();
                let record = PlacementRecord {
                    peer_id: id.to_owned(),
                    offset,
                    updated_ms,
                };
                match inner
                    .saved
                    .placements
                    .iter_mut()
                    .find(|existing| existing.peer_id == id)
                {
                    Some(existing) => *existing = record,
                    None => inner.saved.placements.push(record),
                }
            }
            _ => return,
        }
        save(&inner.saved);
        self.publish(&mut inner);
    }

    fn publish(&self, inner: &mut Inner) {
        let local = displays::logical(&inner.monitors);
        let mut devices = Vec::new();
        let mut portals = Vec::new();
        let mut ids: Vec<&String> = inner.saved.devices.keys().collect();
        for record in &inner.saved.placements {
            if !inner.saved.devices.contains_key(&record.peer_id) {
                ids.push(&record.peer_id);
            }
        }
        for id in ids {
            let known = inner.saved.devices.get(id).cloned().unwrap_or_default();
            let peer = layout::peer_or_default(&known.displays);
            let offset = inner
                .saved
                .placements
                .iter()
                .find(|record| &record.peer_id == id)
                .and_then(|record| record.offset);
            let touching = offset
                .map(|offset| layout::portals(&local, &peer, offset))
                .unwrap_or_default();
            portals.extend(
                touching
                    .iter()
                    .filter_map(|portal| displays::physical_portal(&inner.monitors, id, portal)),
            );
            devices.push(DeviceView {
                peer_id: id.clone(),
                name: if known.name.is_empty() {
                    id.clone()
                } else {
                    known.name
                },
                platform: known.platform,
                displays: peer,
                offset,
                side: touching.first().map(|portal| portal.side),
            });
        }
        devices.sort_by_key(|device| device.name.to_lowercase());
        inner.portals = portals;
        self.state.set_layout(LayoutView {
            displays: local,
            devices,
        });
    }
}

fn path() -> std::path::PathBuf {
    identity::data_directory().join("layout.json")
}

fn save(saved: &Saved) {
    match serde_json::to_string_pretty(saved) {
        Ok(text) => {
            if let Err(error) = identity::write_file(&path(), &text) {
                eprintln!("linkcursor-agent: cannot save layout: {error}");
            }
        }
        Err(error) => eprintln!("linkcursor-agent: cannot encode layout: {error}"),
    }
}

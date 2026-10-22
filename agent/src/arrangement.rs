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
    /// Connected at least once: one of the user's devices.
    #[serde(default)]
    remembered: bool,
    /// Last seen online, Unix milliseconds (0: not recorded).
    #[serde(default)]
    last_seen_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
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
        let saved = identity::read_json::<Saved>(&path()).unwrap_or_default();
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

    /// Where a peer dropped at `offset` would land, for showing it while the
    /// user drags: the core's own snapping, with the same displays and the
    /// other placed peers as obstacles. `None` when there is no room.
    pub fn preview(&self, peer_id: &str, offset: Offset) -> Option<Offset> {
        let inner = self.inner.lock().ok()?;
        let displays_of = |id: &str| {
            layout::peer_or_default(
                inner
                    .saved
                    .devices
                    .get(id)
                    .map(|device| device.displays.as_slice())
                    .unwrap_or_default(),
            )
        };
        let obstacles: Vec<Display> = inner
            .saved
            .placements
            .iter()
            .filter(|record| record.peer_id != peer_id)
            .filter_map(|record| {
                record
                    .offset
                    .map(|offset| layout::placed(&displays_of(&record.peer_id), offset))
            })
            .flatten()
            .collect();
        layout::snap(
            &displays::logical(&inner.monitors),
            &displays_of(peer_id),
            offset,
            &obstacles,
        )
    }

    /// Forgets a device: its name, displays and placement. It shows up again
    /// as a nearby device while it is online.
    pub fn forget(&self, peer_id: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.saved.devices.remove(peer_id);
        inner
            .saved
            .placements
            .retain(|record| record.peer_id != peer_id);
        save(&inner.saved);
        self.publish(&mut inner);
    }

    /// Picks peer descriptions, connections and placement changes out of the
    /// core's events.
    pub fn core_event(&self, json: &str) {
        if !(json.contains("peer_up")
            || json.contains("peer_down")
            || json.contains("peer_displays")
            || json.contains("placement_changed")
            || json.contains("\"connected\"")
            || json.contains("disconnected"))
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
                describe(device, id, peer);
                device.last_seen_ms = now_ms();
            }
            Some("connected") => {
                let peer = &event["peer"];
                let Some(id) = peer["id"].as_str() else {
                    return;
                };
                let device = inner.saved.devices.entry(id.to_owned()).or_default();
                describe(device, id, peer);
                device.remembered = true;
                device.last_seen_ms = now_ms();
            }
            Some("peer_down" | "disconnected") => {
                let Some(device) = event["peer_id"]
                    .as_str()
                    .and_then(|id| inner.saved.devices.get_mut(id))
                else {
                    return;
                };
                device.last_seen_ms = now_ms();
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
                if offset.is_none() && !inner.saved.devices.contains_key(id) {
                    // Removed with a forgotten device: nothing to keep.
                    inner.saved.placements.retain(|record| record.peer_id != id);
                    save(&inner.saved);
                    self.publish(&mut inner);
                    return;
                }
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
            let remembered = known.remembered || offset.is_some();
            let last_seen_ms = (known.last_seen_ms > 0).then_some(known.last_seen_ms);
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
                remembered,
                last_seen_ms,
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

/// Takes a peer's name, platform and displays from a `peer` object of the
/// core's events.
fn describe(device: &mut KnownDevice, id: &str, peer: &Value) {
    device.name = peer["name"].as_str().unwrap_or(id).to_owned();
    if let Some(platform) = peer["platform"].as_str()
        && !platform.is_empty()
    {
        device.platform = platform.to_owned();
    }
    if let Ok(displays) = serde_json::from_value::<Vec<Display>>(peer["displays"].clone())
        && !displays.is_empty()
    {
        device.displays = displays;
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

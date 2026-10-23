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
use linkcursor_core::layout::{self, Display, Neighbor, Offset, Portal, Side};
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
    /// Its portals with this PC (none when it sits against another peer only).
    pub portals: Vec<Portal>,
    /// The other placed peers, which the pointer may move on into where
    /// they touch this one.
    pub neighbors: Vec<Neighbor>,
}

/// This PC and every placed peer, in this PC's space: for moving to the
/// device on one side by shortcut.
pub struct World {
    pub local: Vec<Display>,
    pub peers: Vec<Neighbor>,
}

impl World {
    /// The device next to `from` (`None`: this PC) toward `side`: the
    /// nearest whose middle lies that way. `Some(None)` is this PC.
    pub fn toward(&self, from: Option<&str>, side: Side) -> Option<Option<String>> {
        let bounds_of = |displays: &[Display]| layout::bounds(displays);
        let current = match from {
            None => bounds_of(&self.local)?,
            Some(id) => bounds_of(&self.peers.iter().find(|peer| peer.id == id)?.displays)?,
        };
        let middle = |(left, top, right, bottom): (f64, f64, f64, f64)| {
            ((left + right) / 2.0, (top + bottom) / 2.0)
        };
        let (cx, cy) = middle(current);
        let candidates = std::iter::once((None, bounds_of(&self.local)))
            .chain(
                self.peers
                    .iter()
                    .map(|peer| (Some(peer.id.clone()), bounds_of(&peer.displays))),
            )
            .filter(|(id, _)| id.as_deref() != from);
        candidates
            .filter_map(|(id, bounds)| {
                let (x, y) = middle(bounds?);
                let (along, across) = match side {
                    Side::Right => (x - cx, y - cy),
                    Side::Left => (cx - x, y - cy),
                    Side::Bottom => (y - cy, x - cx),
                    Side::Top => (cy - y, x - cx),
                };
                // That way, and more that way than sideways.
                (along > 1.0 && across.abs() <= along * 2.0)
                    .then_some((along + 2.0 * across.abs(), id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }

    /// Where the pointer lands when it jumps to `to` (`None`: this PC) from
    /// `from`: the middle of the display of `to` nearest to `from`.
    pub fn landing(&self, from: Option<&str>, to: Option<&str>) -> Option<(f64, f64)> {
        let displays_of = |id: Option<&str>| match id {
            None => Some(self.local.as_slice()),
            Some(id) => self
                .peers
                .iter()
                .find(|peer| peer.id == id)
                .map(|peer| peer.displays.as_slice()),
        };
        let (left, top, right, bottom) = layout::bounds(displays_of(from)?)?;
        let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
        displays_of(to)?
            .iter()
            .map(|display| {
                let (x, y) = (
                    display.x + display.width / 2.0,
                    display.y + display.height / 2.0,
                );
                ((x - cx).hypot(y - cy), (x, y))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, point)| point)
    }
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
    /// offset in this PC's space, every portal leading to it, and the other
    /// placed peers it may lead on to.
    pub fn target(&self, peer_id: &str) -> Option<Target> {
        let inner = self.inner.lock().ok()?;
        let offset = inner.offset_of(peer_id)?;
        let peer = inner.displays_of(peer_id);
        let portals = layout::portals(&displays::logical(&inner.monitors), &peer, offset);
        let neighbors = inner
            .placed()
            .into_iter()
            .filter(|neighbor| neighbor.id != peer_id)
            .collect();
        Some(Target {
            peer,
            offset,
            portals,
            neighbors,
        })
    }

    /// This PC and every placed peer, in this PC's space.
    pub fn world(&self) -> Option<World> {
        let inner = self.inner.lock().ok()?;
        Some(World {
            local: displays::logical(&inner.monitors),
            peers: inner.placed(),
        })
    }

    /// Where a peer dropped at `offset` would land, for showing it while the
    /// user drags: the core's own snapping, with the same displays and the
    /// other placed peers (which it may also sit against). `None` when there
    /// is no room.
    pub fn preview(&self, peer_id: &str, offset: Offset) -> Option<Offset> {
        let inner = self.inner.lock().ok()?;
        let others: Vec<Vec<Display>> = inner
            .placed()
            .into_iter()
            .filter(|other| other.id != peer_id)
            .map(|other| other.displays)
            .collect();
        layout::snap_in_arrangement(
            &displays::logical(&inner.monitors),
            &inner.displays_of(peer_id),
            offset,
            &others,
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

impl Inner {
    fn offset_of(&self, peer_id: &str) -> Option<Offset> {
        self.saved
            .placements
            .iter()
            .find(|record| record.peer_id == peer_id)?
            .offset
    }

    /// The peer's displays in its own space (a default until described).
    fn displays_of(&self, peer_id: &str) -> Vec<Display> {
        layout::peer_or_default(
            self.saved
                .devices
                .get(peer_id)
                .map(|device| device.displays.as_slice())
                .unwrap_or_default(),
        )
    }

    /// Every placed peer, as its displays in this PC's space, by ID.
    fn placed(&self) -> Vec<Neighbor> {
        let mut placed: Vec<Neighbor> = self
            .saved
            .placements
            .iter()
            .filter_map(|record| {
                record.offset.map(|offset| Neighbor {
                    id: record.peer_id.clone(),
                    displays: layout::placed(&self.displays_of(&record.peer_id), offset),
                })
            })
            .collect();
        // The core's order, so both snap alike.
        placed.sort_by(|a, b| a.id.cmp(&b.id));
        placed
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
                log!("cannot save layout: {error}");
            }
        }
        Err(error) => log!("cannot encode layout: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(x: f64, y: f64, width: f64, height: f64) -> Display {
        Display {
            x,
            y,
            width,
            height,
            scale: 1.0,
        }
    }

    /// This PC in the middle, a Mac to its right, a PC right of the Mac,
    /// and a tablet above this PC.
    fn world() -> World {
        World {
            local: vec![screen(0.0, 0.0, 1920.0, 1080.0)],
            peers: vec![
                Neighbor {
                    id: "mac".into(),
                    displays: vec![screen(1920.0, 0.0, 1512.0, 982.0)],
                },
                Neighbor {
                    id: "pc".into(),
                    displays: vec![screen(3432.0, 0.0, 1920.0, 1080.0)],
                },
                Neighbor {
                    id: "tablet".into(),
                    displays: vec![screen(400.0, -800.0, 1280.0, 800.0)],
                },
            ],
        }
    }

    #[test]
    fn shortcuts_move_to_the_nearest_device_on_that_side() {
        let world = world();
        assert_eq!(world.toward(None, Side::Right), Some(Some("mac".into())));
        assert_eq!(
            world.toward(Some("mac"), Side::Right),
            Some(Some("pc".into()))
        );
        assert_eq!(world.toward(Some("mac"), Side::Left), Some(None));
        assert_eq!(world.toward(None, Side::Top), Some(Some("tablet".into())));
        assert_eq!(world.toward(Some("tablet"), Side::Bottom), Some(None));
        // Nothing that way.
        assert_eq!(world.toward(None, Side::Left), None);
        assert_eq!(world.toward(Some("pc"), Side::Right), None);
    }

    #[test]
    fn a_jump_lands_in_the_middle_of_the_nearest_display() {
        let world = World {
            local: vec![
                screen(0.0, 0.0, 1920.0, 1080.0),
                screen(-1600.0, 0.0, 1600.0, 900.0),
            ],
            peers: world().peers,
        };
        assert_eq!(world.landing(None, Some("mac")), Some((2676.0, 491.0)));
        // Back from the Mac: the right-hand monitor of this PC, not the left.
        assert_eq!(world.landing(Some("mac"), None), Some((960.0, 540.0)));
    }
}

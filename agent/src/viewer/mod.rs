// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A peer's picture on this PC's screen (a KVM's: the screen of the
//! computer behind it).
//!
//! **Driving.** A KVM is a device like any other: placed beside this PC's
//! screens, and driven by pushing the pointer past that edge
//! (`capture.rs`). Only, the computer behind it may have no screen anyone
//! can see. So while this PC drives a peer that has a picture, the picture
//! fills the monitor the pointer left from ([`Viewers::driving`]), and goes
//! when the pointer comes back. The keyboard and mouse are the capture's,
//! as for any peer; the peer's cursor is in the picture.
//!
//! **Watching.** The user can also open a window on the picture, from the
//! device's menu or the tray ([`Viewers::open`]). It only shows; driving is
//! through the edge, and the window goes full screen meanwhile.
//!
//! The core carries the picture (`media` in its config, see
//! `rinflow-core/docs/media.md`): a peer that has one offers it
//! (`media_available`), and a window watches it (`window.rs`). The frames
//! are H.265 or H.264; `decoder.rs` decodes them on the graphics processor
//! and `render.rs` draws them. A window stays when the link drops, and
//! watches again when the peer offers its picture again. A picture shown
//! only while driving stays linked, hidden, for `window::LINGER` after:
//! back and forth, it is there at once.

mod audio;
mod codec;
mod decoder;
mod geometry;
mod render;
mod window;

use geometry::Screen;
use rinflow_core::{Core, MediaAudio, MediaFrame};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
pub use window::MonitorRect;
use window::{Link, Shared};

/// What is known of a peer that may have a picture.
#[derive(Default)]
struct Peer {
    name: String,
    screen: Option<Screen>,
    /// It offers its picture now.
    offered: bool,
}

#[derive(Default)]
struct Inner {
    core: Weak<Core>,
    peers: HashMap<String, Peer>,
    /// Windows, shown or lingering, by peer.
    windows: HashMap<String, Arc<Shared>>,
    /// The peer this PC drives through an edge now.
    driving: Option<String>,
}

#[derive(Clone, Default)]
pub struct Viewers(Arc<Mutex<Inner>>);

/// The agent's viewers, for the capture, the tray menu and the settings
/// app's requests.
static VIEWERS: std::sync::OnceLock<Viewers> = std::sync::OnceLock::new();

/// The agent's viewers, once the core runs.
pub fn global() -> Option<&'static Viewers> {
    VIEWERS.get()
}

impl Viewers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes these the viewers [`global`] returns.
    pub fn install(&self) {
        let _ = VIEWERS.set(self.clone());
    }

    /// The core is running: frames of watched pictures come here.
    pub fn set_core(&self, core: &Arc<Core>) {
        if let Ok(mut inner) = self.0.lock() {
            inner.core = Arc::downgrade(core);
        }
        let viewers = Arc::downgrade(&self.0);
        core.set_media_sink(move |peer_id, frame| {
            if let Some(viewers) = viewers.upgrade() {
                Self(viewers).frame(peer_id, frame);
            }
        });
        let viewers = Arc::downgrade(&self.0);
        core.set_media_audio_sink(move |peer_id, sound| {
            if let Some(viewers) = viewers.upgrade() {
                Self(viewers).sound(peer_id, &sound);
            }
        });
    }

    fn frame(&self, peer_id: &str, frame: MediaFrame) {
        let Some((shared, core)) = self
            .0
            .lock()
            .ok()
            .and_then(|inner| Some((inner.windows.get(peer_id)?.clone(), inner.core.clone())))
        else {
            return;
        };
        if !shared.push(frame)
            && let Some(core) = core.upgrade()
        {
            let _ = core.request_media_key_frame(peer_id);
        }
    }

    /// Sound from a peer, for its window.
    fn sound(&self, peer_id: &str, sound: &MediaAudio) {
        let shared = self
            .0
            .lock()
            .ok()
            .and_then(|inner| inner.windows.get(peer_id).cloned());
        if let Some(shared) = shared {
            shared.sound(sound);
        }
    }

    /// Whether this PC's microphone is shared with the peers whose picture
    /// shows.
    pub fn microphone(&self) -> bool {
        window::MICROPHONE.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_microphone(&self, shared: bool) {
        window::MICROPHONE.store(shared, std::sync::atomic::Ordering::Release);
        log!(
            "viewer: the microphone is {}",
            if shared { "shared" } else { "not shared" }
        );
        if let Ok(inner) = self.0.lock() {
            for window in inner.windows.values() {
                window.microphone_changed();
            }
        }
    }

    /// The peer's window, made (hidden, watching) if there is none.
    fn window(&self, inner: &mut Inner, peer_id: &str) -> Result<Arc<Shared>, String> {
        if let Some(shared) = inner.windows.get(peer_id)
            && !shared.closed()
        {
            return Ok(shared.clone());
        }
        let core = inner.core.clone();
        let Some(running) = core.upgrade() else {
            return Err("the core is not running".to_owned());
        };
        let peer = inner.peers.entry(peer_id.to_owned()).or_default();
        let name = if peer.name.is_empty() {
            peer_id.to_owned()
        } else {
            peer.name.clone()
        };
        let shared = Shared::new();
        if let Ok(mut screen) = shared.screen.lock() {
            *screen = peer.screen;
        }
        if peer.offered
            && let Err(error) = running.watch(peer_id)
        {
            log!("viewer: cannot watch {peer_id}: {error}");
        }
        inner.windows.insert(peer_id.to_owned(), shared.clone());

        let viewers = Arc::downgrade(&self.0);
        let closed_peer = peer_id.to_owned();
        let closed_window = shared.clone();
        window::spawn(peer_id.to_owned(), name, shared.clone(), core, move || {
            let Some(viewers) = viewers.upgrade() else {
                return;
            };
            let Ok(mut inner) = viewers.lock() else {
                return;
            };
            // Only this window: another may have opened since.
            if inner
                .windows
                .get(&closed_peer)
                .is_some_and(|open| Arc::ptr_eq(open, &closed_window))
            {
                inner.windows.remove(&closed_peer);
                if let Some(core) = inner.core.upgrade() {
                    core.unwatch(&closed_peer);
                }
            }
        });
        Ok(shared)
    }

    /// Opens a window on a peer's picture, or brings its window to the
    /// front.
    pub fn open(&self, peer_id: &str) -> Result<(), String> {
        let Ok(mut inner) = self.0.lock() else {
            return Err("viewer state unavailable".to_owned());
        };
        self.window(&mut inner, peer_id)?.show_window();
        Ok(())
    }

    /// This PC now drives `peer_id` through an edge, the pointer having
    /// left from the monitor at `monitor` (physical pixels), or drives
    /// nobody. Called by the capture, on the edge thread: it only posts.
    pub fn driving(&self, target: Option<(&str, MonitorRect)>) {
        let Ok(mut inner) = self.0.lock() else {
            return;
        };
        let peer = target.map(|(peer_id, _)| peer_id.to_owned());
        if inner.driving != peer
            && let Some(previous) = inner.driving.take()
            && let Some(shared) = inner.windows.get(&previous)
        {
            shared.set_fullscreen(None);
        }
        inner.driving = peer;
        let Some((peer_id, monitor)) = target else {
            return;
        };
        if !inner.peers.get(peer_id).is_some_and(|peer| peer.offered) {
            return;
        }
        match self.window(&mut inner, peer_id) {
            Ok(shared) => shared.set_fullscreen(Some(monitor)),
            Err(error) => log!("viewer: cannot show {peer_id}: {error}"),
        }
    }

    /// Peers offering a picture now, with their names.
    pub fn available(&self) -> Vec<(String, String)> {
        let Ok(inner) = self.0.lock() else {
            return Vec::new();
        };
        let mut peers: Vec<(String, String)> = inner
            .peers
            .iter()
            .filter(|(_, peer)| peer.offered)
            .map(|(id, peer)| {
                let name = if peer.name.is_empty() { id } else { &peer.name };
                (id.clone(), name.clone())
            })
            .collect();
        peers.sort_by(|a, b| a.1.cmp(&b.1));
        peers
    }

    /// Closes every window, when the agent quits.
    pub fn close_all(&self) {
        if let Ok(inner) = self.0.lock() {
            for shared in inner.windows.values() {
                shared.close();
            }
        }
    }

    pub fn core_event(&self, json: &str) {
        // Most events are input and latency: only these concern pictures.
        if !(json.contains("\"media_")
            || json.contains("\"peer_up\"")
            || json.contains("\"peer_displays\""))
        {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let Ok(mut inner) = self.0.lock() else {
            return;
        };
        let kind = event["event"].as_str().unwrap_or_default();
        let peer_id = event["peer_id"]
            .as_str()
            .or_else(|| event["peer"]["id"].as_str())
            .unwrap_or_default()
            .to_owned();
        if peer_id.is_empty() {
            return;
        }
        match kind {
            "peer_up" => {
                let peer = inner.peers.entry(peer_id.clone()).or_default();
                if let Some(name) = event["peer"]["name"].as_str() {
                    peer.name = name.to_owned();
                }
                let screen = screen(&event["peer"]["displays"]);
                if screen.is_some() {
                    peer.screen = screen;
                }
            }
            "peer_displays" => {
                let screen = screen(&event["displays"]);
                inner.peers.entry(peer_id.clone()).or_default().screen = screen;
            }
            "media_available" => {
                inner.peers.entry(peer_id.clone()).or_default().offered = true;
                // A window waiting for the picture watches again.
                if inner.windows.contains_key(&peer_id)
                    && let Some(core) = inner.core.upgrade()
                    && let Err(error) = core.watch(&peer_id)
                {
                    log!("viewer: cannot watch {peer_id}: {error}");
                }
            }
            "media_unavailable" => {
                inner.peers.entry(peer_id.clone()).or_default().offered = false;
            }
            _ => {}
        }
        let Some(shared) = inner.windows.get(&peer_id) else {
            return;
        };
        match kind {
            "peer_up" | "peer_displays" => {
                if let Ok(mut screen) = shared.screen.lock() {
                    *screen = inner.peers.get(&peer_id).and_then(|peer| peer.screen);
                }
            }
            "media_started" => shared.set_link(Link::Started),
            "media_ended" | "media_unavailable" => shared.set_link(Link::Ended),
            "media_available" if shared.link() == Link::Ended => {
                shared.set_link(Link::Connecting);
            }
            _ => {}
        }
    }
}

/// Development: shows an H.265 file (an Annex B stream, as `probe` in
/// rinflow-nanokvm records one) in a viewer window at 30 frames a second,
/// over and over, without a core or a peer. Returns when the window closes.
/// With `fullscreen`, the picture fills the primary monitor from the third
/// second to the seventh, as while driving a peer.
#[cfg(debug_assertions)]
pub fn play_file(path: &str, fullscreen: bool) -> Result<(), String> {
    let stream = std::fs::read(path).map_err(|error| format!("{path}: {error}"))?;
    let units = access_units(&stream);
    if units.is_empty() {
        return Err(format!("{path}: no H.265 pictures"));
    }
    let shared = Shared::new();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    window::spawn(
        "file".to_owned(),
        path.to_owned(),
        shared.clone(),
        Weak::new(),
        move || {
            let _ = closed_tx.send(());
        },
    );
    shared.set_link(Link::Started);
    shared.show_window();
    let mut sequence = 0u64;
    loop {
        for (key_frame, data) in &units {
            sequence += 1;
            if fullscreen && sequence == 90 {
                shared.set_fullscreen(Some(primary_monitor()));
            }
            if fullscreen && sequence == 210 {
                shared.set_fullscreen(None);
            }
            shared.push(MediaFrame {
                key_frame: *key_frame,
                sequence,
                timestamp_micros: sequence * 33_333,
                data: data.clone().into(),
            });
            if closed_rx
                .recv_timeout(std::time::Duration::from_micros(33_333))
                .is_ok()
            {
                return Ok(());
            }
        }
    }
}

/// The primary monitor, physical pixels.
#[cfg(debug_assertions)]
fn primary_monitor() -> MonitorRect {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // SAFETY: plain system metrics.
    unsafe {
        (
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

/// An H.265 stream's access units, and whether each is a key frame (holds
/// an IRAP picture, NAL types 16 to 23). A unit starts at a delimiter, at
/// parameter sets or SEI after a picture's slices, or at a picture's first
/// slice after another picture's.
#[cfg(debug_assertions)]
fn access_units(stream: &[u8]) -> Vec<(bool, Vec<u8>)> {
    // Where each NAL unit's start code begins, its type, and for slices
    // whether it is the picture's first.
    let mut nals: Vec<(usize, u8, bool)> = Vec::new();
    let mut at = 0;
    while at + 4 < stream.len() {
        if stream[at..at + 3] == [0, 0, 1] {
            let start = if at > 0 && stream[at - 1] == 0 {
                at - 1
            } else {
                at
            };
            let kind = (stream[at + 3] >> 1) & 0x3F;
            let first_slice = stream.get(at + 5).is_some_and(|byte| byte & 0x80 != 0);
            nals.push((start, kind, first_slice));
            at += 3;
        } else {
            at += 1;
        }
    }
    let mut units = Vec::new();
    let mut unit_start = None::<usize>;
    let mut has_picture = false;
    let mut key_frame = false;
    for (index, &(start, kind, first_slice)) in nals.iter().enumerate() {
        let vcl = kind < 32;
        let begins = kind == 35
            || (matches!(kind, 32..=34 | 39) && has_picture)
            || (vcl && first_slice && has_picture);
        if begins && let Some(from) = unit_start {
            units.push((key_frame, stream[from..start].to_vec()));
            unit_start = None;
        }
        if unit_start.is_none() {
            unit_start = Some(start);
            has_picture = false;
            key_frame = false;
        }
        if vcl {
            has_picture = true;
            key_frame |= (16..=23).contains(&kind);
        }
        if index == nals.len() - 1
            && let Some(from) = unit_start
        {
            units.push((key_frame, stream[from..].to_vec()));
        }
    }
    units
}

/// The peer's screen: the bounding box of its displays, in its points. A
/// KVM has one, the computer's picture.
fn screen(displays: &Value) -> Option<Screen> {
    let displays = displays.as_array()?;
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for display in displays {
        let x = display["x"].as_f64()?;
        let y = display["y"].as_f64()?;
        let right = x + display["width"].as_f64()?;
        let bottom = y + display["height"].as_f64()?;
        bounds = Some(match bounds {
            None => (x, y, right, bottom),
            Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(right), b.max(bottom)),
        });
    }
    let (left, top, right, bottom) = bounds?;
    (right > left && bottom > top).then_some(Screen {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// Development: plays a quiet tone on this PC's speakers for two seconds
/// through [`audio::Player`] and listens to its microphone meanwhile
/// through [`audio::Microphone`], without a core or a peer, and says what
/// came of each.
#[cfg(debug_assertions)]
pub fn sound_test() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    let (heard, loudest) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let (thread_heard, thread_loudest) = (heard.clone(), loudest.clone());
    let microphone = audio::Microphone::start(move |samples| {
        thread_heard.fetch_add(samples.len() as u64, Ordering::Relaxed);
        let peak = samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| u64::from(i16::from_le_bytes(*sample).unsigned_abs()))
            .max()
            .unwrap_or(0);
        thread_loudest.fetch_max(peak, Ordering::Relaxed);
    });
    let player = audio::Player::start();
    let began = Instant::now();
    let mut sample = 0u64;
    for packet in 1..=200u32 {
        let samples: Vec<u8> = (0..480)
            .flat_map(|_| {
                let angle = sample as f64 * 440.0 * std::f64::consts::TAU / 48_000.0;
                sample += 1;
                ((angle.sin() * 3000.0) as i16).to_le_bytes()
            })
            .collect();
        player.push(&MediaAudio {
            channels: 1,
            sequence: packet,
            samples: samples.into(),
        });
        let due = began + Duration::from_millis(10) * packet;
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    let left = player.waiting();
    std::thread::sleep(Duration::from_millis(300));
    println!(
        "speakers: 2.0 s pushed, {:.0} ms still waiting at the end, {:.0} ms after 0.3 s more",
        left as f64 / 48.0,
        player.waiting() as f64 / 48.0
    );
    println!(
        "microphone: {:.2} s heard in {:.2} s, loudest sample {}",
        heard.load(Ordering::Relaxed) as f64 / 2.0 / 48_000.0,
        began.elapsed().as_secs_f64(),
        loudest.load(Ordering::Relaxed)
    );
    drop((player, microphone));
    std::thread::sleep(Duration::from_millis(300));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_peers_screen_is_the_box_around_its_displays() {
        assert_eq!(
            screen(&json!([{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0, "scale": 1.0}])),
            Some(Screen {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0
            })
        );
        assert_eq!(
            screen(&json!([
                {"x": 0.0, "y": 0.0, "width": 2560.0, "height": 1440.0},
                {"x": -1080.0, "y": 0.0, "width": 1080.0, "height": 1920.0}
            ])),
            Some(Screen {
                x: -1080.0,
                y: 0.0,
                width: 3640.0,
                height: 1920.0
            })
        );
        assert_eq!(screen(&json!([])), None);
        assert_eq!(screen(&Value::Null), None);
    }

    #[test]
    fn offers_and_names_come_from_core_events() {
        let viewers = Viewers::new();
        viewers.core_event(
            &json!({"event": "peer_up", "peer": {"id": "kvm", "name": "kvm-8f30",
                "displays": [{"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0}]}})
            .to_string(),
        );
        assert!(viewers.available().is_empty());
        viewers.core_event(&json!({"event": "media_available", "peer_id": "kvm"}).to_string());
        assert_eq!(
            viewers.available(),
            vec![("kvm".to_owned(), "kvm-8f30".to_owned())]
        );
        viewers.core_event(&json!({"event": "media_unavailable", "peer_id": "kvm"}).to_string());
        assert!(viewers.available().is_empty());
        // Without a running core there is nothing to watch with.
        assert!(viewers.open("kvm").is_err());
    }

    #[test]
    fn a_stream_splits_into_pictures() {
        // VPS, SPS, PPS, IDR slice | a trailing picture's slice | AUD, slice.
        let stream = [
            0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0, 0, 0, 1, 0x44, 0x01,
            0xC1, 0, 0, 0, 1, 0x26, 0x01, 0xAF, 0, 0, 0, 1, 0x02, 0x01, 0xD0, 0x11, 0, 0, 0, 1,
            0x46, 0x01, 0x50, 0, 0, 0, 1, 0x02, 0x01, 0x80, 0x22,
        ];
        let units = access_units(&stream);
        assert_eq!(units.len(), 3);
        assert!(units[0].0);
        assert_eq!(units[0].1.len(), 28);
        assert!(!units[1].0);
        assert_eq!(units[1].1, [0, 0, 0, 1, 0x02, 0x01, 0xD0, 0x11]);
        assert!(!units[2].0);
        assert_eq!(units[2].1[4], 0x46);
    }
}

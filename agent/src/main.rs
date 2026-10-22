// Release builds run without a console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod arrangement;
mod capture;
mod clipboard;
mod displays;
mod edge;
mod effect;
mod elevated;
mod identity;
mod inject;
mod ipc;
mod keymap;
mod overlay;
mod power;
mod services;
mod settings;
mod spring;
mod state;
mod tray;
mod trust;
mod win;

use linkcursor_core::{Core, CoreConfig};
use services::Services;
use std::sync::{Arc, OnceLock};

const INSTANCE_MUTEX: &str = "Local\\LinkCursor.Agent";
/// Pairing requests open the main window at most this often.
const PAIRING_WINDOW_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

fn main() {
    // `--launch-at-login on|off`: the installer turns starting at sign-in on
    // (or off) for the user who runs it, the same value as the 开机时启动
    // switch, and nothing else happens.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, value] = arguments.as_slice()
        && flag == "--launch-at-login"
    {
        let result = settings::autostart::set(value == "on");
        std::process::exit(i32::from(result.is_err()));
    }

    let Some(_instance) = win::SingleInstance::acquire(INSTANCE_MUTEX) else {
        eprintln!("linkcursor-agent: another instance is already running in this session");
        return;
    };

    let identity = match identity::load() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("linkcursor-agent: cannot load device identity: {error}");
            return;
        }
    };

    let effects = effect::EffectStore::load();
    let settings = Arc::new(settings::SettingsStore::load());
    let preferences = settings.get();
    let clipboard_sync = preferences.clipboard;
    let control_port = preferences
        .port
        .unwrap_or_else(|| CoreConfig::default().control_port);
    let state = state::SharedState::new(
        &identity.device_id,
        &identity.device_name,
        effects.get(),
        preferences.clone(),
        settings::autostart::enabled(),
        control_port,
    );
    let clipboard = clipboard::ClipboardHandle::new(clipboard_sync);
    let overlay = match overlay::Overlay::spawn(effects.clone()) {
        Ok(overlay) => overlay,
        Err(error) => {
            eprintln!("linkcursor-agent: cannot start the edge overlay: {error}");
            return;
        }
    };
    let injector = match inject::Injector::spawn() {
        Ok(injector) => injector,
        Err(error) => {
            eprintln!("linkcursor-agent: cannot start input injection: {error}");
            return;
        }
    };
    let monitors = displays::monitors();
    let arrangement = arrangement::Arrangement::load(state.clone(), monitors.clone());
    let trust = Arc::new(trust::TrustStore::load());
    let paired_ids = |trust: &trust::TrustStore| {
        trust
            .peers()
            .into_iter()
            .map(|peer| peer.peer_id)
            .collect::<Vec<_>>()
    };
    state.set_paired_peers(paired_ids(&trust));
    let config = CoreConfig {
        device_id: identity.device_id.clone(),
        device_name: identity.device_name.clone(),
        platform: "windows".to_owned(),
        control_port,
        displays: displays::logical(&monitors),
        placements: arrangement.placements(),
        // Always offered to peers; whether this PC's clipboard is actually
        // shared is the user's setting, applied by `clipboard.rs`.
        clipboard: true,
        // Copied files travel with the clipboard. Every incoming transfer
        // waits for `clipboard.rs` (limit 0), which takes it or not
        // according to the same setting.
        files: true,
        download_directory: Some(
            clipboard::download_directory()
                .to_string_lossy()
                .into_owned(),
        ),
        max_auto_receive_bytes: 0,
        // Encryption and pairing (QUIC with TLS): this PC's certificate,
        // the devices the user paired, and whether only those may connect.
        identity_path: Some(trust::identity_path().to_string_lossy().into_owned()),
        trusted_peers: trust.peers(),
        require_pairing: preferences.paired_only,
        ..CoreConfig::default()
    };
    let events = state.clone();
    let trust_events = trust.clone();
    let layout_events = arrangement.clone();
    let control = injector.handle();
    let input = injector.handle();
    let clipboard_events = clipboard.clone();
    // The edge thread starts after the core; until then nothing is captured,
    // so nothing can be revoked either.
    let edge_slot = Arc::new(OnceLock::<edge::EdgeHandle>::new());
    let edge_events = edge_slot.clone();
    let window_opened = std::sync::Mutex::new(None::<std::time::Instant>);
    let core = match Core::start(
        config,
        move |json| {
            events.apply_core_event(&json);
            if trust_events.core_event(&json) {
                events.set_paired_peers(paired_ids(&trust_events));
            }
            // A device asks to pair: bring the main window up, where the
            // code shows. Off the core's event thread, and at most every few
            // seconds: a device asking over and over must not start a stream
            // of processes.
            if json.contains("\"pairing_requested\"")
                && let Ok(mut opened) = window_opened.lock()
                && opened.is_none_or(|at| at.elapsed() >= PAIRING_WINDOW_INTERVAL)
            {
                *opened = Some(std::time::Instant::now());
                std::thread::spawn(|| tray::open_app(&[]));
            }
            layout_events.core_event(&json);
            control.core_event(&json);
            clipboard_events.core_event(&json);
            if let Some(edge) = edge_events.get() {
                edge.core_event(&json);
            }
        },
        move |event| input.play(event),
    ) {
        Ok(core) => Arc::new(core),
        Err(error) => {
            eprintln!("linkcursor-agent: cannot start core: {error}");
            return;
        }
    };
    // Offered files are decided from the core's first events on.
    clipboard.set_core(Arc::downgrade(&core));
    // Sharing paused in the settings: peers may not drive this PC either.
    if !preferences.sharing {
        let _ = core.set_remote_control_allowed(false);
    }

    // Sleep, wake and network changes: sessions end in order before sleep
    // and come back right after waking or a network change.
    let power = power::PowerWatch::start(Arc::downgrade(&core));

    // Resolution, scaling or monitor changes reach the core (which re-fits
    // placements and tells peers) and the arrangement view.
    let watched_core = Arc::downgrade(&core);
    let watched_arrangement = arrangement.clone();
    if let Err(error) = displays::watch(move |monitors| {
        if let Some(core) = watched_core.upgrade() {
            let _ = core.set_local_displays(displays::logical(&monitors));
        }
        watched_arrangement.set_monitors(monitors);
    }) {
        eprintln!("linkcursor-agent: cannot watch display changes: {error}");
    }

    // Watches the pointer at the portals: crossing into peers, and the demo.
    let edge = match edge::EdgeThread::start(
        overlay.handle(),
        effects.clone(),
        settings.clone(),
        state.clone(),
        arrangement.clone(),
        Arc::downgrade(&core),
    ) {
        Ok(edge) => edge,
        Err(error) => {
            eprintln!("linkcursor-agent: cannot watch the screen edges: {error}");
            return;
        }
    };
    let _ = edge_slot.set(edge.handle());

    // The LocalSystem input helper, when installed: input then reaches
    // elevated windows, UAC prompts and the lock screen. When one of those
    // takes input, a peer this PC drives is given back so it can be
    // answered here.
    let elevated_state = state.clone();
    let elevated_edge = edge.handle();
    elevated::start(move |status| {
        elevated_state.set_elevated_input(state::ElevatedInputView {
            available: status.available,
            enabled: status.enabled,
        });
        if status.secure {
            elevated_edge.release_capture();
        }
    });

    // Reports clipboard changes and writes peers' clipboards.
    let clipboard_thread =
        match clipboard::ClipboardThread::start(clipboard.clone(), Arc::downgrade(&core)) {
            Ok(thread) => Some(thread),
            Err(error) => {
                eprintln!("linkcursor-agent: cannot watch the clipboard: {error}");
                None
            }
        };

    let services = Arc::new(Services::new(
        Arc::downgrade(&core),
        state,
        effects,
        overlay.handle(),
        edge.handle(),
        clipboard,
        settings,
        arrangement,
    ));
    ipc::spawn(&identity.user_sid, services.clone());

    if let Err(error) = tray::run() {
        eprintln!("linkcursor-agent: tray failed: {error}");
    }
    // An open settings app learns that the user quit, and quits too rather
    // than starting the agent again.
    services.state.set_quitting();
    std::thread::sleep(std::time::Duration::from_millis(150));

    let _ = services.set_effect_demo(false);
    // Gives back the keyboard and mouse if a peer is being driven.
    drop(edge);
    drop(clipboard_thread);
    drop(power);
    drop(overlay);
    // The IPC thread only holds a weak reference, so this stops the core and
    // lets connected peers see a clean disconnect before the process exits.
    // Stopping the core queues key-ups for anything the peer still held; the
    // injector plays them before it exits.
    drop(core);
    drop(injector);
}

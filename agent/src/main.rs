// Release builds run without a console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod arrangement;
mod capture;
mod displays;
mod edge;
mod effect;
mod identity;
mod inject;
mod ipc;
mod keymap;
mod overlay;
mod services;
mod spring;
mod state;
mod tray;
mod win;

use linkcursor_core::{Core, CoreConfig};
use services::Services;
use std::sync::Arc;

const INSTANCE_MUTEX: &str = "Local\\LinkCursor.Agent";

fn main() {
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
    let state = state::SharedState::new(&identity.device_id, &identity.device_name, effects.get());
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
    let config = CoreConfig {
        device_id: identity.device_id.clone(),
        device_name: identity.device_name.clone(),
        platform: "windows".to_owned(),
        displays: displays::logical(&monitors),
        placements: arrangement.placements(),
        ..CoreConfig::default()
    };
    let events = state.clone();
    let layout_events = arrangement.clone();
    let control = injector.handle();
    let input = injector.handle();
    let core = match Core::start(
        config,
        move |json| {
            events.apply_core_event(&json);
            layout_events.core_event(&json);
            control.core_event(&json);
        },
        move |event| input.play(event),
    ) {
        Ok(core) => Arc::new(core),
        Err(error) => {
            eprintln!("linkcursor-agent: cannot start core: {error}");
            return;
        }
    };

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

    let services = Arc::new(Services::new(
        Arc::downgrade(&core),
        state,
        effects,
        overlay.handle(),
        edge.handle(),
    ));
    ipc::spawn(&identity.user_sid, services.clone());

    if let Err(error) = tray::run() {
        eprintln!("linkcursor-agent: tray failed: {error}");
    }

    let _ = services.set_effect_demo(false);
    // Gives back the keyboard and mouse if a peer is being driven.
    drop(edge);
    drop(overlay);
    // The IPC thread only holds a weak reference, so this stops the core and
    // lets connected peers see a clean disconnect before the process exits.
    // Stopping the core queues key-ups for anything the peer still held; the
    // injector plays them before it exits.
    drop(core);
    drop(injector);
}

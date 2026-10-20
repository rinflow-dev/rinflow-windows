// Release builds run without a console window; debug builds keep one for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod demo;
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
    let config = CoreConfig {
        device_id: identity.device_id.clone(),
        device_name: identity.device_name.clone(),
        platform: "windows".to_owned(),
        ..CoreConfig::default()
    };
    let events = state.clone();
    let control = injector.handle();
    let input = injector.handle();
    let core = match Core::start(
        config,
        move |json| {
            events.apply_core_event(&json);
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

    let services = Arc::new(Services::new(
        Arc::downgrade(&core),
        state,
        effects,
        overlay.handle(),
    ));
    ipc::spawn(&identity.user_sid, services.clone());

    if let Err(error) = tray::run() {
        eprintln!("linkcursor-agent: tray failed: {error}");
    }

    let _ = services.set_effect_demo(false);
    drop(overlay);
    // The IPC thread only holds a weak reference, so this stops the core and
    // lets connected peers see a clean disconnect before the process exits.
    // Stopping the core queues key-ups for anything the peer still held; the
    // injector plays them before it exits.
    drop(core);
    drop(injector);
}

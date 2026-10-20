//! What the settings app can drive through IPC.

use crate::arrangement::Arrangement;
use crate::demo::EdgeDemo;
use crate::effect::{EffectParams, EffectStore};
use crate::overlay::{Command, OverlayHandle};
use crate::state::SharedState;
use linkcursor_core::Core;
use std::sync::{Arc, Mutex, Weak};

pub struct Services {
    pub core: Weak<Core>,
    pub state: SharedState,
    pub effects: EffectStore,
    pub overlay: OverlayHandle,
    pub arrangement: Arc<Arrangement>,
    demo: Mutex<Option<EdgeDemo>>,
}

impl Services {
    pub fn new(
        core: Weak<Core>,
        state: SharedState,
        effects: EffectStore,
        overlay: OverlayHandle,
        arrangement: Arc<Arrangement>,
    ) -> Self {
        Self {
            core,
            state,
            effects,
            overlay,
            arrangement,
            demo: Mutex::new(None),
        }
    }

    pub fn set_effect(&self, params: EffectParams) {
        let params = self.effects.set(params);
        self.state.set_effect(params, false);
    }

    pub fn reset_effect(&self) {
        let params = self.effects.set(EffectParams::default());
        self.state.set_effect(params, true);
    }

    pub fn preview_effect(&self, hold: Option<f32>) {
        self.overlay.send(Command::Preview { hold });
    }

    pub fn set_effect_demo(&self, enabled: bool) -> Result<(), String> {
        let mut demo = self.demo.lock().map_err(|_| "demo state poisoned")?;
        if enabled && demo.is_none() {
            let started = EdgeDemo::start(
                self.overlay.clone(),
                self.effects.clone(),
                self.state.clone(),
                self.arrangement.clone(),
            )
            .map_err(|error| format!("cannot start the effect demo: {error}"))?;
            *demo = Some(started);
        } else if !enabled {
            // Dropping stops the demo thread and retracts any bump on screen.
            demo.take();
        }
        self.state.set_effect_demo(enabled);
        Ok(())
    }
}

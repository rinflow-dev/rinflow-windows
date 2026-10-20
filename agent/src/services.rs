//! What the settings app can drive through IPC.

use crate::clipboard::ClipboardHandle;
use crate::edge::EdgeHandle;
use crate::effect::{EffectParams, EffectStore};
use crate::overlay::{Command, OverlayHandle};
use crate::settings::SettingsStore;
use crate::state::SharedState;
use linkcursor_core::Core;
use std::sync::{Arc, Weak};

pub struct Services {
    pub core: Weak<Core>,
    pub state: SharedState,
    pub effects: EffectStore,
    pub overlay: OverlayHandle,
    edge: EdgeHandle,
    clipboard: ClipboardHandle,
    settings: Arc<SettingsStore>,
}

impl Services {
    pub fn new(
        core: Weak<Core>,
        state: SharedState,
        effects: EffectStore,
        overlay: OverlayHandle,
        edge: EdgeHandle,
        clipboard: ClipboardHandle,
        settings: Arc<SettingsStore>,
    ) -> Self {
        Self {
            core,
            state,
            effects,
            overlay,
            edge,
            clipboard,
            settings,
        }
    }

    /// Shares the clipboard with peers, or stops sharing it; remembered.
    pub fn set_clipboard_sync(&self, enabled: bool) {
        self.clipboard.set_enabled(enabled);
        self.settings
            .update(|settings| settings.clipboard = enabled);
        self.state.set_clipboard_sync(enabled);
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

    /// The demo runs on the edge thread, which also drives real crossings;
    /// turning it off retracts any bump on screen.
    pub fn set_effect_demo(&self, enabled: bool) -> Result<(), String> {
        self.edge.set_demo(enabled);
        self.state.set_effect_demo(enabled);
        Ok(())
    }
}

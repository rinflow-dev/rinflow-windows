//! What the settings app can drive through IPC.

use crate::arrangement::Arrangement;
use crate::clipboard::ClipboardHandle;
use crate::edge::EdgeHandle;
use crate::effect::{EffectParams, EffectStore};
use crate::overlay::{Command, OverlayHandle};
use crate::settings::{SettingsStore, autostart};
use crate::state::SharedState;
use linkcursor_core::Core;
use serde_json::{Map, Value};
use std::sync::{Arc, Weak};

pub struct Services {
    pub core: Weak<Core>,
    pub state: SharedState,
    pub effects: EffectStore,
    pub overlay: OverlayHandle,
    edge: EdgeHandle,
    clipboard: ClipboardHandle,
    settings: Arc<SettingsStore>,
    arrangement: Arc<Arrangement>,
}

impl Services {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: Weak<Core>,
        state: SharedState,
        effects: EffectStore,
        overlay: OverlayHandle,
        edge: EdgeHandle,
        clipboard: ClipboardHandle,
        settings: Arc<SettingsStore>,
        arrangement: Arc<Arrangement>,
    ) -> Self {
        Self {
            core,
            state,
            effects,
            overlay,
            edge,
            clipboard,
            settings,
            arrangement,
        }
    }

    /// Shares the clipboard with peers, or stops sharing it; remembered.
    pub fn set_clipboard_sync(&self, enabled: bool) {
        let mut values = Map::new();
        values.insert("clipboard".to_owned(), Value::Bool(enabled));
        let _ = self.update_settings(values);
    }

    /// Changes some settings (keys of [`crate::settings::Settings`], plus
    /// `launch_at_login`), applies them at once and remembers them.
    pub fn update_settings(&self, mut values: Map<String, Value>) -> Result<(), String> {
        if let Some(value) = values.remove("launch_at_login") {
            let enabled = value
                .as_bool()
                .ok_or("launch_at_login 需要 true 或 false")?;
            autostart::set(enabled).map_err(|error| format!("无法设置开机启动：{error}"))?;
            self.state.set_launch_at_login(autostart::enabled());
        }
        if values.is_empty() {
            return Ok(());
        }
        let merged = self.settings.get().merged(&values)?;
        let settings = self.settings.update(|settings| *settings = merged);

        if values.contains_key("clipboard") {
            self.clipboard.set_enabled(settings.clipboard);
        }
        if values.contains_key("sharing")
            && let Some(core) = self.core.upgrade()
        {
            let _ = core.set_remote_control_allowed(settings.sharing);
        }
        self.edge.settings_changed();
        // A shortcut that failed to register may work now; the edge thread
        // reports it again if not.
        if values.contains_key("lock_hotkey") {
            self.state.clear_error();
        }
        self.state.set_settings(settings);
        Ok(())
    }

    /// Forgets a device: disconnected, unplaced, and no longer listed as one
    /// of the user's devices.
    pub fn forget_device(&self, peer_id: &str) -> Result<(), String> {
        if let Some(core) = self.core.upgrade() {
            let _ = core.disconnect_peer(peer_id);
            let _ = core.set_placement(peer_id, None);
        }
        self.arrangement.forget(peer_id);
        Ok(())
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

// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What the settings app can drive through IPC.

use crate::arrangement::Arrangement;
use crate::clipboard::ClipboardHandle;
use crate::edge::EdgeHandle;
use crate::effect::{EffectParams, EffectStore};
use crate::overlay::{Command, OverlayHandle};
use crate::settings::{SettingsStore, autostart};
use crate::state::SharedState;
use linkcursor_core::Core;
use linkcursor_core::layout::Offset;
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
            let enabled = value.as_bool().ok_or(crate::i18n::tr(
                "launch_at_login 需要 true 或 false",
                "launch_at_login must be true or false",
            ))?;
            autostart::set(enabled).map_err(|error| {
                if crate::i18n::chinese() {
                    format!("无法设置开机启动：{error}")
                } else {
                    format!("Couldn't change \"Start when I sign in\": {error}")
                }
            })?;
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
        if values.contains_key("paired_only")
            && let Some(core) = self.core.upgrade()
        {
            let _ = core.set_require_pairing(settings.paired_only);
        }
        if values.contains_key("language") {
            crate::i18n::set_language(&settings.language);
        }
        if values.contains_key("modifier_map")
            && let Some(core) = self.core.upgrade()
        {
            let _ = core.set_modifier_map(settings.modifier_map);
        }
        if values.contains_key("manual_addresses")
            && let Some(core) = self.core.upgrade()
        {
            let _ = core.set_manual_addresses(settings.manual_addresses.clone());
        }
        if values.contains_key("hide_cursor_on_leave") && !settings.hide_cursor_on_leave {
            crate::hider::reveal();
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

    /// Where a peer dropped at `offset` would land (see `Arrangement::preview`).
    pub fn preview_placement(&self, peer_id: &str, offset: Offset) -> Option<Offset> {
        self.arrangement.preview(peer_id, offset)
    }

    /// Forgets a device: disconnected, unplaced, and no longer listed as one
    /// of the user's devices.
    pub fn forget_device(&self, peer_id: &str) -> Result<(), String> {
        if let Some(core) = self.core.upgrade() {
            let _ = core.disconnect_peer(peer_id);
            let _ = core.set_placement(peer_id, None);
            // Pairing again will be needed.
            let _ = core.unpair(peer_id);
        }
        self.arrangement.forget(peer_id);
        Ok(())
    }

    /// The user checked the code shown for `peer_id` and accepts.
    pub fn accept_pairing(&self, peer_id: &str) -> Result<(), String> {
        let core = self
            .core
            .upgrade()
            .ok_or("LinkCursor core is shutting down")?;
        core.accept_pairing(peer_id)?;
        self.state.set_pairing_accepted(peer_id);
        Ok(())
    }

    pub fn reject_pairing(&self, peer_id: &str) -> Result<(), String> {
        let core = self
            .core
            .upgrade()
            .ok_or("LinkCursor core is shutting down")?;
        core.reject_pairing(peer_id)
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

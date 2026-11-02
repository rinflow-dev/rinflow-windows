// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tunable parameters for the edge push-through effect.
//!
//! The same JSON shape is meant to be shared with the macOS and Linux
//! renderers, so every value is in platform-neutral units: points at 100 %
//! scale, seconds, and raw pointer counts for the push distance.

use crate::identity;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectParams {
    /// Raw pointer counts the user has to push against the edge before the
    /// cursor passes through (first crossing only, see `warm_seconds`).
    pub push_distance: f32,
    /// Seconds without pushing after which the bump springs back.
    pub release_delay: f32,
    /// After a crossing, crossings within this many seconds skip the
    /// resistance. Returning to the local device never has resistance.
    pub warm_seconds: f32,
    /// Points from a screen corner in which the edge does not react, so the
    /// Start button, menus and hot corners stay usable.
    pub corner_dead_zone: f32,

    /// Spring response in seconds (time to settle, like SwiftUI's `response`).
    pub spring_response: f32,
    /// Spring damping ratio: 1 is critically damped, lower values overshoot.
    pub spring_damping: f32,

    /// How far the bump bulges into the screen at full push, in points.
    pub bump_depth: f32,
    /// Half the length of the bump along the edge, in points.
    pub bump_width: f32,
    /// Diameter of the pointer dot that rides at the tip of the bump.
    pub dot_size: f32,
    /// Overall opacity of the material.
    pub opacity: f32,
    /// Expanding ring where the cursor comes through.
    pub ripple: bool,
    /// `auto` follows the Windows app theme; `light` or `dark` force one.
    pub theme: String,
}

impl Default for EffectParams {
    fn default() -> Self {
        Self {
            push_distance: 260.0,
            release_delay: 0.25,
            warm_seconds: 20.0,
            corner_dead_zone: 48.0,
            spring_response: 0.32,
            spring_damping: 0.78,
            bump_depth: 26.0,
            bump_width: 72.0,
            dot_size: 18.0,
            opacity: 0.92,
            ripple: true,
            theme: "auto".to_owned(),
        }
    }
}

impl EffectParams {
    /// Clamps every value to a range the renderer and the tracker handle.
    pub fn sanitized(mut self) -> Self {
        fn clamp(value: f32, min: f32, max: f32, default: f32) -> f32 {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                default
            }
        }
        let defaults = Self::default();
        self.push_distance = clamp(self.push_distance, 10.0, 2000.0, defaults.push_distance);
        self.release_delay = clamp(self.release_delay, 0.0, 2.0, defaults.release_delay);
        self.warm_seconds = clamp(self.warm_seconds, 0.0, 600.0, defaults.warm_seconds);
        self.corner_dead_zone = clamp(self.corner_dead_zone, 0.0, 400.0, defaults.corner_dead_zone);
        self.spring_response = clamp(self.spring_response, 0.05, 2.0, defaults.spring_response);
        self.spring_damping = clamp(self.spring_damping, 0.1, 2.0, defaults.spring_damping);
        self.bump_depth = clamp(self.bump_depth, 4.0, 120.0, defaults.bump_depth);
        self.bump_width = clamp(self.bump_width, 16.0, 300.0, defaults.bump_width);
        self.dot_size = clamp(self.dot_size, 0.0, 60.0, defaults.dot_size);
        self.opacity = clamp(self.opacity, 0.1, 1.0, defaults.opacity);
        if !matches!(self.theme.as_str(), "auto" | "light" | "dark") {
            self.theme = defaults.theme;
        }
        self
    }
}

/// Current parameters, shared by the IPC thread, the edge tracker and the
/// overlay renderer. Saved to `%LOCALAPPDATA%\LinkCursor\effect.json`.
#[derive(Clone)]
pub struct EffectStore(Arc<RwLock<EffectParams>>);

impl EffectStore {
    pub fn load() -> Self {
        let params = crate::identity::read_json::<EffectParams>(&path())
            .unwrap_or_default()
            .sanitized();
        Self(Arc::new(RwLock::new(params)))
    }

    pub fn get(&self) -> EffectParams {
        self.0
            .read()
            .map(|params| params.clone())
            .unwrap_or_default()
    }

    pub fn set(&self, params: EffectParams) -> EffectParams {
        let params = params.sanitized();
        // Saved while holding the lock, so concurrent saves cannot interleave
        // and the file always ends up with the values in memory.
        let Ok(mut current) = self.0.write() else {
            return params;
        };
        *current = params.clone();
        if let Ok(text) = serde_json::to_string_pretty(&params)
            && let Err(error) = identity::write_file(&path(), &text)
        {
            log!("cannot save effect settings: {error}");
        }
        params
    }
}

fn path() -> std::path::PathBuf {
    identity::data_directory().join("effect.json")
}

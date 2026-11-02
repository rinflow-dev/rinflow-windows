// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The language of the text the agent shows itself: the tray menu,
//! notifications, message boxes and the errors the settings app displays.
//! The user's choice (the `language` setting: `zh-CN`, `en-US`), or else
//! Simplified Chinese when Windows' display language is Simplified Chinese
//! and English for every other language: the same choice the settings app
//! makes, so the two never mix languages.
//!
//! Both versions of a string sit side by side where it is used ([`tr`])
//! rather than in resource files: there are few strings and two languages.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;

/// Chinese (Simplified, China) and Chinese (Simplified, Singapore).
const SIMPLIFIED_CHINESE: [u16; 2] = [0x0804, 0x1004];

const FOLLOW_SYSTEM: u8 = 0;
const CHINESE: u8 = 1;
const ENGLISH: u8 = 2;

/// The `language` setting, as one of the values above.
static CHOSEN: AtomicU8 = AtomicU8::new(FOLLOW_SYSTEM);

/// Applies the `language` setting: `zh-CN`, `en-US`, or anything else
/// (`system`) to follow Windows. Text shown from now on uses it.
pub fn set_language(language: &str) {
    let chosen = match language {
        "zh-CN" => CHINESE,
        "en-US" => ENGLISH,
        _ => FOLLOW_SYSTEM,
    };
    CHOSEN.store(chosen, Ordering::Relaxed);
}

/// Whether to show Simplified Chinese: chosen in the settings, or, when
/// following Windows, its display language is zh-CN or zh-SG. Traditional
/// Chinese (Taiwan, Hong Kong) gets English, like every other language,
/// rather than characters its readers do not use.
pub fn chinese() -> bool {
    match CHOSEN.load(Ordering::Relaxed) {
        CHINESE => true,
        ENGLISH => false,
        _ => system_chinese(),
    }
}

/// Asked once: Windows applies a new display language only after the user
/// signs out again, which ends the agent too.
fn system_chinese() -> bool {
    static SYSTEM: OnceLock<bool> = OnceLock::new();
    *SYSTEM.get_or_init(|| {
        // SAFETY: no arguments; returns the user's UI language ID.
        let language = unsafe { GetUserDefaultUILanguage() };
        SIMPLIFIED_CHINESE.contains(&language)
    })
}

/// `zh` or `en`, whichever [`chinese`] picks.
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    if chinese() { zh } else { en }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_wins_over_the_system_language() {
        set_language("en-US");
        assert_eq!(tr("退出", "Quit"), "Quit");
        set_language("zh-CN");
        assert_eq!(tr("退出", "Quit"), "退出");
        set_language("system");
        let expected = if system_chinese() { "退出" } else { "Quit" };
        assert_eq!(tr("退出", "Quit"), expected);
    }
}

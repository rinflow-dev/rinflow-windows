// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The text the agent shows itself: the tray menu, notifications and the
//! errors the settings app displays.
//!
//! Strings live in `Strings\<language>\Resources.resw`, the format the
//! settings app uses, so translators have one kind of file. The app reads its
//! own through MRT Core; this process has no package resources, so build.rs
//! compiles these into [`s`] and fails if a language lacks a string or uses
//! different placeholders.
//!
//! The language is the user's choice (the `language` setting), or else the
//! first of the user's Windows languages that Rinflow has, then English: what
//! MRT Core picks for the app, so the two never mix languages.

use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

/// A string in every language, in the order of [`Language`].
#[derive(Clone, Copy)]
pub struct Message([&'static str; 3]);

include!(concat!(env!("OUT_DIR"), "/strings.rs"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    English = 0,
    Chinese = 1,
    Japanese = 2,
}

const FOLLOW_SYSTEM: u8 = u8::MAX;

/// The `language` setting: a [`Language`], or [`FOLLOW_SYSTEM`].
static CHOSEN: AtomicU8 = AtomicU8::new(FOLLOW_SYSTEM);

/// Applies the `language` setting: `en-US`, `zh-CN`, `ja-JP`, or anything
/// else (`system`) to follow Windows. Text shown from now on uses it.
pub fn set_language(language: &str) {
    let chosen = match language {
        "en-US" => Language::English as u8,
        "zh-CN" => Language::Chinese as u8,
        "ja-JP" => Language::Japanese as u8,
        _ => FOLLOW_SYSTEM,
    };
    CHOSEN.store(chosen, Ordering::Relaxed);
}

/// The index of the current language in a [`Message`].
fn current() -> usize {
    match CHOSEN.load(Ordering::Relaxed) {
        FOLLOW_SYSTEM => system_language() as usize,
        chosen => chosen as usize,
    }
}

/// Asked once: Windows applies a new display language only after the user
/// signs out again, which ends the agent too.
fn system_language() -> Language {
    static SYSTEM: OnceLock<Language> = OnceLock::new();
    *SYSTEM.get_or_init(|| pick(&preferred_languages()))
}

/// The first language Rinflow has, or English. Traditional Chinese (Taiwan,
/// Hong Kong) has no match, rather than characters its readers do not use.
fn pick<S: AsRef<str>>(languages: &[S]) -> Language {
    languages
        .iter()
        .find_map(|tag| {
            let tag = tag.as_ref().to_ascii_lowercase();
            let base = tag.split('-').next().unwrap_or_default();
            match base {
                "en" => Some(Language::English),
                "ja" => Some(Language::Japanese),
                "zh" if tag == "zh-cn" || tag == "zh-sg" || tag.starts_with("zh-hans") => {
                    Some(Language::Chinese)
                }
                _ => None,
            }
        })
        .unwrap_or(Language::English)
}

/// The user's Windows languages, most preferred first.
fn preferred_languages() -> Vec<String> {
    let (mut count, mut length) = (0u32, 0u32);
    // SAFETY: a null buffer asks for the length needed.
    let sized = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut length,
        )
    };
    if sized == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: the buffer holds the `length` characters just asked for.
    let filled = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            buffer.as_mut_ptr(),
            &mut length,
        )
    };
    if filled == 0 {
        return Vec::new();
    }
    // Names separated by NUL characters, ending with two.
    buffer
        .split(|&unit| unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// `message` in the current language.
pub fn tr(message: Message) -> &'static str {
    message.0[current()]
}

/// `message` in the current language with `{0}`, `{1}`, … replaced by the
/// arguments. Each language puts them where its grammar needs them.
pub fn trf(message: Message, arguments: &[&dyn Display]) -> String {
    fill(tr(message), arguments)
}

fn fill(text: &str, arguments: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let argument = rest
            .find('}')
            .and_then(|close| Some((rest[1..close].parse::<usize>().ok()?, close)))
            .and_then(|(number, close)| Some((arguments.get(number)?, close)));
        if let Some((argument, close)) = argument {
            out.push_str(&argument.to_string());
            rest = &rest[close + 1..];
        } else {
            out.push('{');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resw;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn the_setting_wins_over_the_system_language() {
        for (setting, expected) in [("en-US", "Quit"), ("zh-CN", "退出"), ("ja-JP", "終了")] {
            set_language(setting);
            assert_eq!(tr(s::TrayQuit), expected);
        }
        set_language("system");
        assert_eq!(tr(s::TrayQuit), s::TrayQuit.0[system_language() as usize]);
    }

    #[test]
    fn windows_languages_pick_the_first_one_rinflow_has() {
        assert_eq!(pick(&["ja-JP", "en-US"]), Language::Japanese);
        assert_eq!(pick(&["en-GB", "ja-JP"]), Language::English);
        assert_eq!(pick(&["fr-FR", "ja-JP"]), Language::Japanese);
        assert_eq!(pick(&["zh-CN"]), Language::Chinese);
        assert_eq!(pick(&["zh-Hans-SG"]), Language::Chinese);
        assert_eq!(pick(&["zh-SG"]), Language::Chinese);
        // Traditional Chinese is not offered Simplified.
        assert_eq!(pick(&["zh-TW", "ja-JP"]), Language::Japanese);
        assert_eq!(pick(&["zh-Hant-HK"]), Language::English);
        assert_eq!(pick(&["fr-FR"]), Language::English);
        assert_eq!(pick::<&str>(&[]), Language::English);
    }

    #[test]
    fn arguments_go_where_the_language_puts_them() {
        assert_eq!(fill("{1} から {0}", &[&"a", &2]), "2 から a");
        // An argument's own braces are left alone.
        assert_eq!(fill("{0} {1}", &[&"{1}", &"x"]), "{1} x");
        assert_eq!(fill("{5} {x} {", &[&"a"]), "{5} {x} {");
    }

    /// The settings app gets its strings from MRT Core at run time, where a
    /// missing one falls back to English without a word.
    #[test]
    fn every_language_of_the_app_has_every_string() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/Strings");
        let load = |language: &str| -> BTreeMap<String, String> {
            let path = folder.join(language).join("Resources.resw");
            let text = std::fs::read_to_string(&path).expect("read the resources");
            resw::parse(&text).into_iter().collect()
        };
        let english = load("en-US");
        assert!(english.len() > 100, "no strings found");
        for language in ["zh-CN", "ja-JP"] {
            let translated = load(language);
            let names = |map: &BTreeMap<String, String>| map.keys().cloned().collect::<Vec<_>>();
            assert_eq!(
                names(&english),
                names(&translated),
                "{language} differs in names"
            );
            for (name, value) in &english {
                assert_eq!(
                    resw::placeholders(value),
                    resw::placeholders(&translated[name]),
                    "{language}: {name} differs in placeholders"
                );
            }
        }
        assert_eq!(load("ja-JP")["LanguageTag"], "ja-JP");
    }
}

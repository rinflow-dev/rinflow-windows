// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Reads `.resw` files, for build.rs (which compiles the agent's strings in)
//! and for the test that checks the settings app's strings.

/// The strings of a `.resw` file, as (name, value), in file order.
pub fn parse(text: &str) -> Vec<(String, String)> {
    const NAME: &str = "<data name=\"";
    const VALUE: &str = "<value>";
    const VALUE_END: &str = "</value>";
    let mut strings = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(NAME) {
        rest = &rest[at + NAME.len()..];
        let name_end = rest.find('"').expect("a closing quote after the name");
        let name = rest[..name_end].to_owned();
        let start = rest.find(VALUE).expect("a value after the name") + VALUE.len();
        let end = start + rest[start..].find(VALUE_END).expect("a closing value tag");
        strings.push((name, unescape(&rest[start..end])));
        rest = &rest[end..];
    }
    strings
}

/// The argument numbers a string uses (`{0}`, `{1:02}`), sorted, without
/// repeats. Every language of a string has to use the same ones.
pub fn placeholders(value: &str) -> Vec<usize> {
    let mut numbers: Vec<usize> = value
        .split('{')
        .skip(1)
        .filter_map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            let next = part[digits.len()..].chars().next();
            matches!(next, Some('}' | ':')).then(|| digits.parse().ok())?
        })
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    numbers
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

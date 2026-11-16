// SPDX-License-Identifier: AGPL-3.0-only

//! UI language: German, or English when the first preferred macOS language is not
//! German. Applies to menus and notifications; the log stays German.

use std::sync::OnceLock;

static ENGLISH: OnceLock<bool> = OnceLock::new();

pub fn english() -> bool {
    *ENGLISH.get_or_init(|| !system_prefers_german())
}

/// Picks the text for the UI language.
pub fn tr(de: &'static str, en: &'static str) -> &'static str {
    if english() {
        en
    } else {
        de
    }
}

#[cfg(all(target_os = "macos", not(test)))]
fn system_prefers_german() -> bool {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|lang| lang.to_string().to_ascii_lowercase().starts_with("de"))
        .unwrap_or(true)
}

/// Tests (and other platforms) always use German, independent of the machine.
#[cfg(any(not(target_os = "macos"), test))]
fn system_prefers_german() -> bool {
    true
}

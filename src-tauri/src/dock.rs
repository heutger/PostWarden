// SPDX-License-Identifier: AGPL-3.0-only

//! Dock badge and bringing the window to the front.

use crate::state::Badge;
use tauri::{Runtime, WebviewWindow};

pub fn badge_label(badge: Badge) -> Option<String> {
    match badge {
        Badge::None => None,
        Badge::Alert => Some("!".into()),
        Badge::Count(n) if n > 9999 => Some("9999+".into()),
        Badge::Count(n) => Some(n.to_string()),
    }
}

pub fn set_badge<R: Runtime>(window: &WebviewWindow<R>, badge: Badge) {
    // The Dock badge is app-wide on macOS; Tauri exposes it on the window.
    if let Err(e) = window.set_badge_label(badge_label(badge)) {
        crate::log::write(&format!("Badge fehlgeschlagen: {e}"));
    }
}

pub fn bring_to_front<R: Runtime>(window: &WebviewWindow<R>) {
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(badge_label(Badge::None), None);
        assert_eq!(badge_label(Badge::Alert).as_deref(), Some("!"));
        assert_eq!(badge_label(Badge::Count(7)).as_deref(), Some("7"));
        assert_eq!(badge_label(Badge::Count(123_456)).as_deref(), Some("9999+"));
    }
}

// SPDX-License-Identifier: AGPL-3.0-only

//! macOS notifications: texts and de-duplication.
//!
//! Counter increase and Outlook's own web notification for the same mail arrive within
//! seconds of each other. Only such a *pair* is merged: a counter alert consumes one web
//! notification within [`DEBOUNCE`] and vice versa. Two counter alerts (or two web
//! notifications) are never merged with each other.

use crate::i18n::{english, tr};
use crate::state::Alert;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

pub const DEBOUNCE: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Counter increase detected by PostWarden.
    Mail,
    /// Outlook's own web notification.
    OutlookWeb,
    Auth,
    Connection,
    Unknown,
    Reconnected,
    /// Test and diagnostics messages – never suppressed.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub category: Category,
    pub title: String,
    pub body: Option<String>,
}

impl Message {
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            category: Category::Info,
            title: title.into(),
            body: Some(body.into()),
        }
    }

    pub fn from_alert(alert: Alert) -> Self {
        let (category, title, body): (Category, String, Option<&str>) = match alert {
            Alert::NewMail(1) => (Category::Mail, tr("1 neue Mail", "1 new mail").into(), None),
            Alert::NewMail(n) => (
                Category::Mail,
                if english() {
                    format!("{n} new mails")
                } else {
                    format!("{n} neue Mails")
                },
                None,
            ),
            Alert::AuthNeeded => (
                Category::Auth,
                tr("Bitte erneut anmelden", "Please sign in again").into(),
                Some(tr(
                    "Outlook verlangt eine erneute Anmeldung oder Bestätigung.",
                    "Outlook requires you to sign in again or confirm.",
                )),
            ),
            Alert::ConnectionProblem => (
                Category::Connection,
                tr("Verbindungsproblem", "Connection problem").into(),
                Some(tr(
                    "Outlook ist gerade nicht erreichbar. PostWarden versucht es automatisch erneut.",
                    "Outlook cannot be reached right now. PostWarden will retry automatically.",
                )),
            ),
            Alert::Unknown => (
                Category::Unknown,
                tr("Status unklar", "Status unclear").into(),
                Some(tr(
                    "Der Posteingang ist nicht sichtbar (andere Ansicht offen oder Ordnerliste eingeklappt?). Neue Mails werden gerade nicht erkannt.",
                    "The inbox is not visible (another view open or folder pane collapsed?). New mail is not being detected right now.",
                )),
            ),
            Alert::Reconnected => (
                Category::Reconnected,
                tr("Wieder verbunden", "Reconnected").into(),
                None,
            ),
        };
        Self {
            category,
            title,
            body: body.map(str::to_string),
        }
    }

    /// Outlook's own web notification. Content only if the user enabled it.
    pub fn from_outlook(title: Option<&str>, body: Option<&str>, show_content: bool) -> Self {
        let clean = |s: Option<&str>| s.map(sanitize).filter(|s| !s.is_empty());
        if show_content {
            if let Some(title) = clean(title) {
                return Self {
                    category: Category::OutlookWeb,
                    title,
                    body: clean(body),
                };
            }
        }
        Self {
            category: Category::OutlookWeb,
            // Outlook also notifies about reminders etc. – without content, stay neutral.
            title: tr("Neue Outlook-Mitteilung", "New Outlook notification").into(),
            body: None,
        }
    }
}

/// Removes control characters and limits the length (input comes from the web page).
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.chars().count() > MAX_TEXT {
        let mut cut: String = cleaned.chars().take(MAX_TEXT - 1).collect();
        cut.push('…');
        cut
    } else {
        cleaned
    }
}

#[derive(Debug)]
pub struct Debouncer {
    last: HashMap<Category, Instant>,
    /// Unpaired counter alert / web notification still waiting for its partner.
    open_counter: Option<Instant>,
    open_web: Option<Instant>,
    /// Counter alerts overall: a flapping (or hostile) counter cannot spam; the badge
    /// still shows the number.
    mail: crate::ratelimit::RateLimit,
}

impl Default for Debouncer {
    fn default() -> Self {
        Self {
            last: HashMap::new(),
            open_counter: None,
            open_web: None,
            mail: crate::ratelimit::RateLimit::new(6, Duration::from_secs(60)),
        }
    }
}

fn fresh(t: Option<Instant>, now: Instant) -> bool {
    t.is_some_and(|t| now.saturating_duration_since(t) < DEBOUNCE)
}

impl Debouncer {
    pub fn allow(&mut self, category: Category, now: Instant) -> bool {
        match category {
            Category::Info => true,
            Category::Mail => {
                if fresh(self.open_web, now) {
                    self.open_web = None; // partner of the web notification just shown
                    false
                } else if self.mail.allow(now) {
                    self.open_counter = Some(now);
                    true
                } else {
                    false
                }
            }
            Category::OutlookWeb => {
                if fresh(self.open_counter, now) {
                    self.open_counter = None;
                    false
                } else {
                    self.open_web = Some(now);
                    true
                }
            }
            _ => match self.last.get(&category) {
                Some(t) if now.saturating_duration_since(*t) < DEBOUNCE => false,
                _ => {
                    self.last.insert(category, now);
                    true
                }
            },
        }
    }
}

pub fn show<R: Runtime>(app: &AppHandle<R>, msg: &Message) {
    let mut builder = app.notification().builder().title(&msg.title);
    if let Some(body) = &msg.body {
        builder = builder.body(body);
    }
    if let Err(e) = builder.show() {
        crate::log::write(&format!("Mitteilung fehlgeschlagen: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(Message::from_alert(Alert::NewMail(1)).title, "1 neue Mail");
        assert_eq!(Message::from_alert(Alert::NewMail(3)).title, "3 neue Mails");
        assert_eq!(
            Message::from_alert(Alert::AuthNeeded).title,
            "Bitte erneut anmelden"
        );
        assert_eq!(
            Message::from_alert(Alert::Reconnected).category,
            Category::Reconnected
        );
    }

    #[test]
    fn outlook_content_only_when_enabled() {
        let m = Message::from_outlook(Some("Alice"), Some("Secret subject"), false);
        assert_eq!(m.title, "Neue Outlook-Mitteilung");
        assert_eq!(m.body, None);
        let m = Message::from_outlook(Some("Alice"), Some("Subject\n\tline"), true);
        assert_eq!(m.title, "Alice");
        assert_eq!(m.body.as_deref(), Some("Subject line"));
        let m = Message::from_outlook(Some("  "), None, true);
        assert_eq!(m.title, "Neue Outlook-Mitteilung");
    }

    #[test]
    fn sanitize_limits_length() {
        let long = "x".repeat(1000);
        let s = sanitize(&long);
        assert_eq!(s.chars().count(), MAX_TEXT);
        assert!(s.ends_with('…'));
        assert_eq!(sanitize("a\u{0}b"), "a b");
    }

    #[test]
    fn debounce_per_category() {
        let t0 = Instant::now();
        let mut d = Debouncer::default();
        assert!(d.allow(Category::Auth, t0));
        assert!(!d.allow(Category::Auth, t0 + Duration::from_secs(3)));
        assert!(d.allow(Category::Connection, t0 + Duration::from_secs(3)));
        assert!(d.allow(Category::Auth, t0 + Duration::from_secs(10)));
        assert!(d.allow(Category::Info, t0 + Duration::from_secs(10)));
        assert!(d.allow(Category::Info, t0 + Duration::from_secs(10)));
    }

    #[test]
    fn mail_pairs_are_merged_once() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut d = Debouncer::default();
        // counter, then Outlook's notification for the same mail → merged
        assert!(d.allow(Category::Mail, t0));
        assert!(!d.allow(Category::OutlookWeb, t0 + s(2)));
        // a second, different mail within 10 s is still announced
        assert!(d.allow(Category::Mail, t0 + s(4)));
        // … and its web notification merged with it
        assert!(!d.allow(Category::OutlookWeb, t0 + s(5)));
        // web first, then counter
        assert!(d.allow(Category::OutlookWeb, t0 + s(30)));
        assert!(!d.allow(Category::Mail, t0 + s(31)));
        // two web notifications are never merged with each other
        assert!(d.allow(Category::OutlookWeb, t0 + s(60)));
        assert!(d.allow(Category::OutlookWeb, t0 + s(61)));
        // two counter alerts neither
        assert!(d.allow(Category::Mail, t0 + s(90)));
        assert!(d.allow(Category::Mail, t0 + s(91)));
        // stale partner (> 10 s) does not merge
        assert!(d.allow(Category::OutlookWeb, t0 + s(120)));
        assert!(d.allow(Category::Mail, t0 + s(131)));
    }

    #[test]
    fn counter_alerts_are_capped() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut d = Debouncer::default();
        let shown = (0..20)
            .filter(|i| d.allow(Category::Mail, t0 + s(*i)))
            .count();
        assert_eq!(shown, 6);
        assert!(d.allow(Category::Mail, t0 + s(61)));
    }
}

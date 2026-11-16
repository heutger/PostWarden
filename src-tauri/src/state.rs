// SPDX-License-Identifier: AGPL-3.0-only

//! State machine: Ok / AuthNeeded / Error / Unknown plus the unread counter.
//!
//! Pure logic without Tauri types: inputs are page events, reports from the injected
//! script and timer ticks (each with an explicit `now`), outputs are [`Effect`]s that
//! `lib.rs` applies (badge, notification, bring window to front, reload).
//!
//! ```text
//!           report ok + inbox_found
//!    ┌───────────────────────────────────────┐
//!    ▼                                       │
//!   Ok ──auth_banner / login host──▶ AuthNeeded
//!    │ ──error_page / offline──────▶ Error (auto reload with backoff, not when offline)
//!    │ ──no inbox / no report ≥ N──▶ Unknown (reload with backoff if no report)
//!    └◀──────── back as soon as the page is healthy again
//! ```

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing known yet after launch (no badge, no alert).
    Starting,
    Ok,
    AuthNeeded,
    Error,
    Unknown,
}

impl Status {
    pub fn is_problem(self) -> bool {
        matches!(self, Status::AuthNeeded | Status::Error | Status::Unknown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A: re-authentication banner inside Outlook.
    Banner,
    /// B: main frame on the sign-in page (or any non-Outlook host).
    LoginPage,
    /// C: Outlook error screen.
    ErrorPage,
    /// C: browser reports no network.
    Offline,
    /// D: no visible inbox for `heartbeat` – folder pane collapsed, or another module
    /// (calendar, contacts, settings) is open. PostWarden only sees new mail in the
    /// mail view, so this must not pass silently as "Ok".
    NoInbox,
    /// D: no report from the page for `heartbeat` (script dead / page hangs / load failed).
    NoReport,
}

impl Reason {
    pub fn status(self) -> Status {
        match self {
            Reason::Banner | Reason::LoginPage => Status::AuthNeeded,
            Reason::ErrorPage | Reason::Offline => Status::Error,
            Reason::NoInbox | Reason::NoReport => Status::Unknown,
        }
    }

    /// Automatic reload only where it cannot hurt: never for A/B (the user must confirm
    /// themselves), not while offline (Outlook recovers on its own once online; a reload
    /// would replace the page with an empty error view), not for a missing inbox (the
    /// page is alive, the user may be writing a mail).
    fn needs_reload(self) -> bool {
        matches!(self, Reason::ErrorPage | Reason::NoReport)
    }

    /// Already qualified by the heartbeat timeout – no extra grace period.
    fn is_timeout(self) -> bool {
        matches!(self, Reason::NoInbox | Reason::NoReport)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Banner => "banner",
            Reason::LoginPage => "login_page",
            Reason::ErrorPage => "error_page",
            Reason::Offline => "offline",
            Reason::NoInbox => "no_inbox",
            Reason::NoReport => "no_report",
        }
    }
}

/// What the injected script saw (already validated by `commands.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub unread: Option<u32>,
    pub inbox_found: bool,
    pub auth_banner: bool,
    pub error_page: bool,
    pub offline: bool,
    /// The mail module is open (logged for context; a missing inbox counts in any view).
    pub mail_view: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    NewMail(u32),
    AuthNeeded,
    ConnectionProblem,
    Unknown,
    Reconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    None,
    Count(u32),
    Alert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Badge(Badge),
    Notify(Alert),
    BringToFront,
    Reload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub heartbeat: Duration,
    pub reminder: Option<Duration>,
    pub grace: Duration,
    pub bring_to_front_on_auth: bool,
    pub notify_on_reconnect: bool,
}

/// Reload backoff for error states: 1, 2, 5, 10, 10, … minutes.
const BACKOFF_SECS: [u64; 4] = [60, 120, 300, 600];

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(BACKOFF_SECS[(attempt as usize).min(BACKOFF_SECS.len() - 1)])
}

#[derive(Debug)]
pub struct Machine {
    settings: Settings,
    status: Status,
    reason: Option<Reason>,
    unread: Option<u32>,
    /// Latest report of the current document (cleared on every page load).
    report: Option<Report>,
    /// Last report or page start – basis for the "no report" watchdog.
    signal_at: Instant,
    /// Last time a visible inbox was reported.
    inbox_seen_at: Instant,
    /// Main frame is not on an Outlook host (sign-in page, IdP, …).
    off_app: bool,
    pending: Option<(Reason, Instant)>,
    last_alert: Option<Instant>,
    reload_attempts: u32,
    reload_at: Option<Instant>,
    /// The main frame was allowed to navigate to a sign-in page that has not committed
    /// yet: no automatic reload until it does (at most [`SIGN_IN_HOLD`]).
    sign_in_started: Option<Instant>,
    badge: Option<Badge>,
}

/// How long a started sign-in navigation blocks automatic reloads without committing.
const SIGN_IN_HOLD: Duration = Duration::from_secs(60);

impl Machine {
    pub fn new(settings: Settings, now: Instant) -> Self {
        Self {
            settings,
            status: Status::Starting,
            reason: None,
            unread: None,
            report: None,
            signal_at: now,
            inbox_seen_at: now,
            off_app: false,
            pending: None,
            last_alert: None,
            reload_attempts: 0,
            reload_at: None,
            sign_in_started: None,
            badge: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }
    pub fn reason(&self) -> Option<Reason> {
        self.reason
    }
    pub fn unread(&self) -> Option<u32> {
        self.unread
    }
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// The main frame started loading a document on `host`.
    pub fn page_started(&mut self, on_app_host: bool, now: Instant) -> Vec<Effect> {
        self.sign_in_started = None;
        self.off_app = !on_app_host;
        self.report = None;
        self.signal_at = now;
        self.inbox_seen_at = now;
        self.step(now)
    }

    pub fn report(&mut self, report: Report, now: Instant) -> Vec<Effect> {
        self.signal_at = now;
        if report.inbox_found {
            self.inbox_seen_at = now;
        }
        // The page is alive again: a reload scheduled for a dead page or an error screen
        // must not hit it later.
        if !report.error_page {
            self.reload_at = None;
        }
        self.report = Some(report);
        self.step(now)
    }

    /// Timer: watchdog, grace periods, reminders, reload backoff.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        self.step(now)
    }

    fn observed(&self, now: Instant) -> Option<Reason> {
        if self.off_app {
            return Some(Reason::LoginPage);
        }
        if let Some(r) = &self.report {
            if r.auth_banner {
                return Some(Reason::Banner);
            }
            // Offline wins over an error screen: a reload while offline would only
            // replace the page with an empty error view.
            if r.offline {
                return Some(Reason::Offline);
            }
            if r.error_page {
                return Some(Reason::ErrorPage);
            }
        }
        let hb = self.settings.heartbeat;
        if now.saturating_duration_since(self.signal_at) >= hb {
            return Some(Reason::NoReport);
        }
        if now.saturating_duration_since(self.inbox_seen_at) >= hb {
            return Some(Reason::NoInbox);
        }
        None
    }

    fn healthy(&self) -> bool {
        !self.off_app && self.report.as_ref().is_some_and(|r| r.inbox_found)
    }

    fn step(&mut self, now: Instant) -> Vec<Effect> {
        let mut fx = Vec::new();

        match self.observed(now) {
            Some(reason) => {
                if self.status.is_problem() && self.reason == Some(reason) {
                    self.pending = None;
                    // A report in between (e.g. the splash screen right after a reload)
                    // cancels the scheduled reload; re-arm it while the problem lasts.
                    if reason.needs_reload() && self.reload_at.is_none() {
                        self.reload_at = Some(now + backoff(self.reload_attempts));
                    }
                } else if reason.is_timeout() || self.settings.grace.is_zero() {
                    self.commit(reason, now, &mut fx);
                } else {
                    match self.pending {
                        Some((p, since)) if p == reason => {
                            if now.saturating_duration_since(since) >= self.settings.grace {
                                self.commit(reason, now, &mut fx);
                            }
                        }
                        _ => self.pending = Some((reason, now)),
                    }
                }
            }
            None => {
                self.pending = None;
                if self.healthy() && self.status != Status::Ok {
                    // No "reconnected" after a merely hidden inbox: nothing was disconnected.
                    let was_problem =
                        self.status.is_problem() && self.reason != Some(Reason::NoInbox);
                    self.status = Status::Ok;
                    self.reason = None;
                    self.last_alert = None;
                    self.reload_at = None;
                    self.reload_attempts = 0;
                    if was_problem && self.settings.notify_on_reconnect {
                        fx.push(Effect::Notify(Alert::Reconnected));
                    }
                }
            }
        }

        // Unread counter: only counted while Ok, so mails arriving during a problem
        // are announced once the page is healthy again. The count may come from the page
        // title (folder pane collapsed); the life sign itself needs a visible inbox
        // (`inbox_found`), so a stale title cannot keep the status Ok.
        if self.status == Status::Ok {
            if let Some(n) = self.report.as_ref().and_then(|r| r.unread) {
                if let Some(old) = self.unread {
                    if n > old {
                        fx.push(Effect::Notify(Alert::NewMail(n - old)));
                    }
                }
                self.unread = Some(n);
            }
        }

        if self.status.is_problem() {
            if let (Some(every), Some(last)) = (self.settings.reminder, self.last_alert) {
                if now.saturating_duration_since(last) >= every {
                    if let Some(alert) = status_alert(self.status) {
                        fx.push(Effect::Notify(alert));
                    }
                    self.last_alert = Some(now);
                }
            }
            if let Some(at) = self.reload_at {
                if now >= at {
                    if self.reload_allowed(now) {
                        fx.push(Effect::Reload);
                        self.reload_attempts = self.reload_attempts.saturating_add(1);
                        self.reload_at = Some(now + backoff(self.reload_attempts));
                    } else {
                        // The page moved on (sign-in, banner, offline, other problem
                        // pending): never interrupt it with a stale reload.
                        self.reload_at = None;
                    }
                }
            }
        }

        let badge = self.badge();
        if self.badge != Some(badge) {
            self.badge = Some(badge);
            fx.push(Effect::Badge(badge));
        }
        fx
    }

    fn commit(&mut self, reason: Reason, now: Instant, fx: &mut Vec<Effect>) {
        let prev = self.status;
        self.status = reason.status();
        self.reason = Some(reason);
        self.pending = None;
        if prev != self.status {
            if let Some(alert) = status_alert(self.status) {
                fx.push(Effect::Notify(alert));
            }
            self.last_alert = Some(now);
            if self.status == Status::AuthNeeded && self.settings.bring_to_front_on_auth {
                fx.push(Effect::BringToFront);
            }
        }
        if reason.needs_reload() {
            if self.reload_at.is_none() {
                self.reload_at = Some(now + backoff(self.reload_attempts));
            }
        } else {
            self.reload_at = None;
            self.reload_attempts = 0;
        }
    }

    /// The main frame starts navigating to a sign-in page (decided before WebKit loads
    /// it): cancel a scheduled reload and block new ones until the page commits.
    pub fn sign_in_started(&mut self, now: Instant) {
        self.sign_in_started = Some(now);
        self.reload_at = None;
    }

    /// A reload is only justified while the *current* observation is the problem that
    /// scheduled it – also checked again right before the effect is applied, because
    /// timer and IPC apply their effects with a delay.
    pub fn reload_allowed(&self, now: Instant) -> bool {
        self.sign_in_started
            .is_none_or(|t| now.saturating_duration_since(t) >= SIGN_IN_HOLD)
            && self.status.is_problem()
            && self.reason.is_some_and(Reason::needs_reload)
            && self.pending.is_none()
            && self.observed(now) == self.reason
    }

    pub fn badge(&self) -> Badge {
        if self.status.is_problem() {
            Badge::Alert
        } else {
            match self.unread {
                Some(n) if n > 0 => Badge::Count(n),
                _ => Badge::None,
            }
        }
    }
}

fn status_alert(status: Status) -> Option<Alert> {
    match status {
        Status::AuthNeeded => Some(Alert::AuthNeeded),
        Status::Error => Some(Alert::ConnectionProblem),
        Status::Unknown => Some(Alert::Unknown),
        Status::Starting | Status::Ok => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn settings() -> Settings {
        Settings {
            heartbeat: 300 * S,
            reminder: Some(1800 * S),
            grace: 10 * S,
            bring_to_front_on_auth: true,
            notify_on_reconnect: true,
        }
    }

    fn ok(unread: u32) -> Report {
        Report {
            unread: Some(unread),
            inbox_found: true,
            mail_view: true,
            ..Default::default()
        }
    }

    fn notifies(fx: &[Effect]) -> Vec<Alert> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Notify(a) => Some(*a),
                _ => None,
            })
            .collect()
    }

    struct T {
        m: Machine,
        t0: Instant,
    }
    impl T {
        fn new() -> Self {
            let t0 = Instant::now();
            Self {
                m: Machine::new(settings(), t0),
                t0,
            }
        }
        fn at(&self, secs: u64) -> Instant {
            self.t0 + S * secs as u32
        }
        fn report(&mut self, secs: u64, r: Report) -> Vec<Effect> {
            let now = self.at(secs);
            self.m.report(r, now)
        }
        fn tick(&mut self, secs: u64) -> Vec<Effect> {
            let now = self.at(secs);
            self.m.tick(now)
        }
        fn page(&mut self, secs: u64, app: bool) -> Vec<Effect> {
            let now = self.at(secs);
            self.m.page_started(app, now)
        }
    }

    #[test]
    fn startup_count_without_notification() {
        let mut t = T::new();
        t.page(0, true);
        let fx = t.report(2, ok(3));
        assert_eq!(t.m.status(), Status::Ok);
        assert!(notifies(&fx).is_empty());
        assert!(fx.contains(&Effect::Badge(Badge::Count(3))));
    }

    #[test]
    fn new_mail_notifies_delta_and_badge_follows() {
        let mut t = T::new();
        t.report(1, ok(0));
        let fx = t.report(5, ok(2));
        assert_eq!(notifies(&fx), vec![Alert::NewMail(2)]);
        assert!(fx.contains(&Effect::Badge(Badge::Count(2))));
        let fx = t.report(9, ok(1));
        assert!(notifies(&fx).is_empty());
        assert!(fx.contains(&Effect::Badge(Badge::Count(1))));
        let fx = t.report(12, ok(0));
        assert!(fx.contains(&Effect::Badge(Badge::None)));
        // unchanged report → no effects at all
        assert!(t.report(15, ok(0)).is_empty());
    }

    #[test]
    fn banner_needs_grace_then_alerts() {
        let mut t = T::new();
        t.report(1, ok(4));
        let banner = Report {
            auth_banner: true,
            ..ok(4)
        };
        assert!(notifies(&t.report(10, banner.clone())).is_empty());
        assert_eq!(t.m.status(), Status::Ok);
        let fx = t.tick(21);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        assert_eq!(t.m.reason(), Some(Reason::Banner));
        assert_eq!(notifies(&fx), vec![Alert::AuthNeeded]);
        assert!(fx.contains(&Effect::BringToFront));
        assert!(fx.contains(&Effect::Badge(Badge::Alert)));
        // no automatic reload for auth problems
        assert!(!t.tick(4000).contains(&Effect::Reload));
        // banner gone → back to Ok, count visible, reconnect message
        let fx = t.report(4001, ok(4));
        assert_eq!(t.m.status(), Status::Ok);
        assert_eq!(notifies(&fx), vec![Alert::Reconnected]);
        assert!(fx.contains(&Effect::Badge(Badge::Count(4))));
    }

    #[test]
    fn short_banner_flicker_is_ignored() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(
            10,
            Report {
                auth_banner: true,
                ..ok(0)
            },
        );
        t.report(13, ok(0));
        assert!(t.tick(30).is_empty());
        assert_eq!(t.m.status(), Status::Ok);
    }

    #[test]
    fn login_redirect_within_grace_is_ignored() {
        let mut t = T::new();
        t.page(0, true);
        t.page(1, false); // SSO redirect via login.microsoftonline.com
        t.tick(5);
        t.page(6, true);
        let fx = t.report(8, ok(1));
        assert_eq!(t.m.status(), Status::Ok);
        assert!(notifies(&fx).is_empty());
    }

    #[test]
    fn login_page_alerts_and_reminds() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.page(100, false);
        let fx = t.tick(111);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        assert_eq!(t.m.reason(), Some(Reason::LoginPage));
        assert_eq!(notifies(&fx), vec![Alert::AuthNeeded]);
        // no NoReport/Unknown while on the login page, reminder after 30 min
        assert!(notifies(&t.tick(1000)).is_empty());
        assert_eq!(notifies(&t.tick(111 + 1800)), vec![Alert::AuthNeeded]);
        assert!(notifies(&t.tick(111 + 1801)).is_empty());
        assert_eq!(notifies(&t.tick(111 + 3600)), vec![Alert::AuthNeeded]);
    }

    #[test]
    fn reminder_can_be_disabled() {
        let mut t = T::new();
        t.m.set_settings(Settings {
            reminder: None,
            ..settings()
        });
        t.page(0, false);
        t.tick(10);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        assert!(notifies(&t.tick(100_000)).is_empty());
    }

    #[test]
    fn switching_auth_reason_does_not_renotify() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(
            2,
            Report {
                auth_banner: true,
                ..ok(0)
            },
        );
        t.tick(12);
        assert_eq!(t.m.reason(), Some(Reason::Banner));
        t.page(20, false);
        let fx = t.tick(31);
        assert_eq!(t.m.reason(), Some(Reason::LoginPage));
        assert!(notifies(&fx).is_empty());
        assert!(!fx.contains(&Effect::BringToFront));
    }

    #[test]
    fn error_page_reloads_with_backoff_until_recovered() {
        let mut t = T::new();
        t.report(1, ok(0));
        let err = Report {
            error_page: true,
            inbox_found: false,
            unread: None,
            ..ok(0)
        };
        t.report(10, err.clone());
        let fx = t.tick(20);
        assert_eq!(t.m.status(), Status::Error);
        assert_eq!(notifies(&fx), vec![Alert::ConnectionProblem]);
        assert!(!fx.contains(&Effect::BringToFront));
        assert!(!t.tick(79).contains(&Effect::Reload));
        assert!(t.tick(80).contains(&Effect::Reload)); // +1 min
        t.page(81, true);
        t.report(85, err.clone());
        assert!(!t.tick(199).contains(&Effect::Reload));
        assert!(t.tick(200).contains(&Effect::Reload)); // +2 min
        assert!(t.tick(500).contains(&Effect::Reload)); // +5 min
        assert!(t.tick(1100).contains(&Effect::Reload)); // +10 min
        assert!(t.tick(1700).contains(&Effect::Reload)); // +10 min
        t.page(1701, true);
        let fx = t.report(1710, ok(2));
        assert_eq!(t.m.status(), Status::Ok);
        // Mails that arrived during the outage are announced after recovery.
        assert_eq!(notifies(&fx), vec![Alert::Reconnected, Alert::NewMail(2)]);
        assert!(!t.tick(5000).contains(&Effect::Reload));
    }

    #[test]
    fn backoff_continues_after_intermediate_report() {
        let mut t = T::new();
        t.report(1, ok(0));
        let err = Report {
            error_page: true,
            inbox_found: false,
            unread: None,
            ..ok(0)
        };
        let splash = Report {
            inbox_found: false,
            unread: None,
            ..ok(0)
        };
        t.report(10, err.clone());
        t.tick(20);
        assert!(t.tick(80).contains(&Effect::Reload)); // +1 min
        t.page(81, true);
        t.report(82, splash); // page loads, error screen not rendered yet
        t.report(85, err.clone()); // error screen again
        let reloads: Vec<u64> = (86..=1200)
            .step_by(5)
            .filter(|s| t.tick(*s).contains(&Effect::Reload))
            .collect();
        assert!(!reloads.is_empty(), "backoff must continue");
        assert!(reloads[0] >= 85 + 120, "{reloads:?}"); // next step: +2 min
    }

    #[test]
    fn offline_does_not_reload() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(
            10,
            Report {
                offline: true,
                ..ok(0)
            },
        );
        let fx = t.tick(20);
        assert_eq!(t.m.status(), Status::Error);
        assert_eq!(t.m.reason(), Some(Reason::Offline));
        assert_eq!(notifies(&fx), vec![Alert::ConnectionProblem]);
        for s in (30..2000).step_by(30) {
            // keep reporting (script alive), never reload
            t.report(
                s,
                Report {
                    offline: true,
                    ..ok(0)
                },
            );
            assert!(!t.tick(s).contains(&Effect::Reload));
        }
        let fx = t.report(2000, ok(0));
        assert_eq!(notifies(&fx), vec![Alert::Reconnected]);
    }

    #[test]
    fn watchdog_no_report_unknown_and_reload() {
        let mut t = T::new();
        t.report(1, ok(1));
        assert!(notifies(&t.tick(300)).is_empty());
        let fx = t.tick(301);
        assert_eq!(t.m.status(), Status::Unknown);
        assert_eq!(t.m.reason(), Some(Reason::NoReport));
        assert_eq!(notifies(&fx), vec![Alert::Unknown]);
        assert!(fx.contains(&Effect::Badge(Badge::Alert)));
        assert!(t.tick(361).contains(&Effect::Reload));
        t.page(362, true);
        // still dead → stays Unknown without re-notifying, keeps reloading
        let fx = t.tick(700);
        assert!(notifies(&fx).is_empty());
        assert_eq!(t.m.status(), Status::Unknown);
    }

    #[test]
    fn watchdog_from_start() {
        let mut t = T::new();
        t.page(0, true);
        t.tick(299);
        assert_eq!(t.m.status(), Status::Starting);
        let fx = t.tick(300);
        assert_eq!(t.m.status(), Status::Unknown);
        assert_eq!(notifies(&fx), vec![Alert::Unknown]);
    }

    #[test]
    fn missing_inbox_in_mail_view_is_unknown_without_reload() {
        let mut t = T::new();
        t.report(1, ok(0));
        let no_inbox = Report {
            inbox_found: false,
            unread: None,
            mail_view: true,
            ..Default::default()
        };
        for s in (31..=301).step_by(30) {
            t.report(s, no_inbox.clone());
        }
        assert_eq!(t.m.status(), Status::Unknown);
        assert_eq!(t.m.reason(), Some(Reason::NoInbox));
        for s in (331..3000).step_by(30) {
            t.report(s, no_inbox.clone());
            assert!(!t.tick(s).contains(&Effect::Reload));
        }
        let fx = t.report(3000, ok(0));
        assert_eq!(t.m.status(), Status::Ok);
        assert!(notifies(&fx).is_empty());
    }

    #[test]
    fn other_module_is_unknown_after_heartbeat() {
        let mut t = T::new();
        t.report(1, ok(2));
        let calendar = Report {
            inbox_found: false,
            unread: None,
            mail_view: false,
            ..Default::default()
        };
        for s in (30..=270).step_by(30) {
            t.report(s, calendar.clone());
        }
        assert_eq!(t.m.status(), Status::Ok);
        assert_eq!(t.m.badge(), Badge::Count(2));
        // monitoring is blind outside the mail view: say so, but never reload
        t.report(301, calendar.clone());
        assert_eq!(t.m.status(), Status::Unknown);
        assert_eq!(t.m.reason(), Some(Reason::NoInbox));
        for s in (330..2000).step_by(30) {
            t.report(s, calendar.clone());
            assert!(!t.tick(s).contains(&Effect::Reload));
        }
        t.report(2000, ok(2));
        assert_eq!(t.m.status(), Status::Ok);
    }

    #[test]
    fn report_cancels_pending_reload() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.tick(301);
        assert_eq!(t.m.reason(), Some(Reason::NoReport));
        // page answers again, but the inbox is not (yet) visible
        t.report(
            320,
            Report {
                inbox_found: false,
                unread: None,
                mail_view: true,
                ..Default::default()
            },
        );
        for s in (330..600).step_by(10) {
            assert!(!t.tick(s).contains(&Effect::Reload));
        }
    }

    #[test]
    fn mails_during_problem_are_announced_after_recovery() {
        let mut t = T::new();
        t.report(1, ok(1));
        t.page(10, false);
        t.tick(21);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        assert_eq!(t.m.badge(), Badge::Alert);
        t.page(100, true);
        let fx = t.report(105, ok(4));
        assert_eq!(notifies(&fx), vec![Alert::Reconnected, Alert::NewMail(3)]);
        assert!(fx.contains(&Effect::Badge(Badge::Count(4))));
    }

    #[test]
    fn reconnect_notification_can_be_disabled() {
        let mut t = T::new();
        t.m.set_settings(Settings {
            notify_on_reconnect: false,
            bring_to_front_on_auth: false,
            ..settings()
        });
        t.report(1, ok(0));
        t.page(2, false);
        let fx = t.tick(12);
        assert!(!fx.contains(&Effect::BringToFront));
        t.page(20, true);
        assert!(notifies(&t.report(21, ok(0))).is_empty());
    }

    #[test]
    fn first_launch_on_login_page() {
        let mut t = T::new();
        t.page(0, false);
        let fx = t.tick(10);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        assert_eq!(notifies(&fx), vec![Alert::AuthNeeded]);
    }

    #[test]
    fn title_count_is_no_life_sign() {
        // Folder pane hidden: the script reports the title count with inbox_found=false.
        let title = |n| Report {
            unread: Some(n),
            inbox_found: false,
            ..ok(0)
        };
        let mut t = T::new();
        t.report(1, ok(2));
        assert_eq!(t.m.status(), Status::Ok);
        // counter still follows the title while Ok …
        assert_eq!(notifies(&t.report(2, title(3))), vec![Alert::NewMail(1)]);
        // … but a stale title (e.g. after a session timeout) does not keep it Ok
        t.report(200, title(3));
        let fx = t.tick(301);
        assert_eq!(t.m.status(), Status::Unknown);
        assert_eq!(t.m.reason(), Some(Reason::NoInbox));
        assert_eq!(notifies(&fx), vec![Alert::Unknown]);
    }

    #[test]
    fn title_alone_does_not_recover() {
        let mut t = T::new();
        t.page(0, false);
        t.tick(10);
        assert_eq!(t.m.status(), Status::AuthNeeded);
        t.page(20, true);
        t.report(
            21,
            Report {
                unread: Some(4),
                inbox_found: false,
                ..ok(0)
            },
        );
        t.tick(40);
        assert_ne!(t.m.status(), Status::Ok);
    }

    fn error_page() -> Report {
        Report {
            error_page: true,
            inbox_found: false,
            unread: None,
            ..ok(0)
        }
    }

    #[test]
    fn sign_in_cancels_scheduled_reload() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(10, error_page());
        t.tick(20); // Error, first reload planned for +60 s
        assert_eq!(t.m.reason(), Some(Reason::ErrorPage));
        t.page(75, false); // user reaches the sign-in page before the reload is due
        for s in (76..200).step_by(1) {
            assert!(!t.tick(s).contains(&Effect::Reload), "reload at {s}");
        }
        assert_eq!(t.m.status(), Status::AuthNeeded);
    }

    #[test]
    fn offline_error_page_never_reloads() {
        let mut t = T::new();
        t.report(1, ok(0));
        let both = Report {
            offline: true,
            ..error_page()
        };
        t.report(10, both.clone());
        t.tick(20);
        assert_eq!(t.m.reason(), Some(Reason::Offline));
        for s in (30..2000).step_by(30) {
            t.report(s, both.clone());
            assert!(!t.tick(s).contains(&Effect::Reload));
        }
    }

    #[test]
    fn reload_needs_current_problem() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(10, error_page());
        t.tick(20);
        assert!(t.m.reload_allowed(t.at(21)));
        t.report(30, ok(0));
        assert!(!t.m.reload_allowed(t.at(31)));
    }

    #[test]
    fn sign_in_before_commit_blocks_reload() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(10, error_page());
        t.tick(20);
        assert!(t.m.reload_allowed(t.at(79)));
        // the guard lets the main frame go to the sign-in page; it has not committed yet
        t.m.sign_in_started(t.at(79));
        assert!(!t.m.reload_allowed(t.at(80)));
        assert!(!t.tick(80).contains(&Effect::Reload));
        // commit of the sign-in page releases the hold (state then follows the page)
        t.page(90, false);
        assert!(!t.m.reload_allowed(t.at(91)));
        assert_eq!(t.m.sign_in_started, None);
    }

    #[test]
    fn stuck_sign_in_navigation_does_not_block_forever() {
        let mut t = T::new();
        t.report(1, ok(0));
        t.report(10, error_page());
        t.tick(20);
        t.m.sign_in_started(t.at(30));
        t.report(40, error_page()); // navigation never committed, error screen remains
        assert!(!t.m.reload_allowed(t.at(80)));
        assert!(t.m.reload_allowed(t.at(91)));
    }
}

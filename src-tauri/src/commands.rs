// SPDX-License-Identifier: AGPL-3.0-only

//! IPC commands for the injected script. Everything coming from the page is untrusted:
//! the caller must be the main webview on an Outlook host, inputs are bounded and validated.

use crate::notify::Message;
use crate::state::Report;
use crate::{nav, Shared};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager, Runtime, State, Webview};

const MAX_UNREAD: u32 = 1_000_000;
const MAX_PATTERN: usize = 200;

/// Reported page state (see `inject/postwarden.js`).
#[derive(Debug, Clone, Deserialize)]
pub struct PageState {
    pub unread: Option<u32>,
    #[serde(default)]
    pub inbox_found: bool,
    /// Only the matched *pattern*, never the banner text.
    pub auth_banner: Option<String>,
    #[serde(default)]
    pub error_page: bool,
    #[serde(default)]
    pub offline: bool,
    #[serde(default = "yes")]
    pub mail_view: bool,
}

fn yes() -> bool {
    true
}

impl PageState {
    /// Bounds-checks the input and converts it into a state-machine report. A banner
    /// must name one of the configured `patterns` exactly; the combination of fields
    /// must be one the injected script can produce.
    pub fn validate(self, patterns: &[String]) -> Result<(Report, Option<String>), String> {
        if self.unread.is_some_and(|n| n > MAX_UNREAD) {
            return Err("unread out of range".into());
        }
        if self.inbox_found && (self.unread.is_none() || self.error_page) {
            return Err("inconsistent report".into());
        }
        let pattern = self
            .auth_banner
            .filter(|p| !p.trim().is_empty())
            .map(|p| {
                if p.len() <= MAX_PATTERN && patterns.contains(&p) {
                    Ok(p)
                } else {
                    Err("unknown banner pattern".to_string())
                }
            })
            .transpose()?;
        let report = Report {
            unread: self.unread,
            inbox_found: self.inbox_found,
            auth_banner: pattern.is_some(),
            error_page: self.error_page,
            offline: self.offline,
            mail_view: self.mail_view,
        };
        Ok((report, pattern))
    }
}

fn limited(
    shared: &Shared,
    pick: impl FnOnce(&mut crate::ratelimit::Limits) -> &mut crate::ratelimit::RateLimit,
) -> Result<(), String> {
    let mut limits = shared.limits.lock().unwrap_or_else(|e| e.into_inner());
    if pick(&mut limits).allow(Instant::now()) {
        Ok(())
    } else {
        Err("rate limited".into())
    }
}

/// The caller must be the main webview showing an Outlook host.
fn check_caller<R: Runtime>(webview: &Webview<R>, shared: &Shared) -> Result<(), String> {
    if webview.label() != crate::MAIN {
        return Err("not allowed".into());
    }
    let url = webview.url().map_err(|e| e.to_string())?;
    let cfg = shared.cfg.read().unwrap_or_else(|e| e.into_inner());
    if nav::https_host_in(&cfg.app_hosts, &url) {
        Ok(())
    } else {
        Err("not allowed".into())
    }
}

#[tauri::command]
pub fn pw_get_config<R: Runtime>(
    webview: Webview<R>,
    shared: State<'_, Arc<Shared>>,
) -> Result<crate::config::ScriptConfig, String> {
    check_caller(&webview, &shared)?;
    limited(&shared, |l| &mut l.config)?;
    Ok(shared
        .cfg
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .script_config())
}

#[tauri::command]
pub fn pw_report<R: Runtime>(
    app: AppHandle<R>,
    webview: Webview<R>,
    shared: State<'_, Arc<Shared>>,
    state: PageState,
) -> Result<(), String> {
    check_caller(&webview, &shared)?;
    limited(&shared, |l| &mut l.report)?;
    let patterns = shared
        .cfg
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .auth_banner
        .patterns
        .clone();
    let (report, pattern) = state.validate(&patterns)?;
    if let Some(p) = &pattern {
        shared.note_banner_pattern(p);
    }
    let fx = shared.machine().report(report, Instant::now());
    crate::apply(&app, &shared, fx);
    Ok(())
}

#[tauri::command]
pub fn pw_notify<R: Runtime>(
    app: AppHandle<R>,
    webview: Webview<R>,
    shared: State<'_, Arc<Shared>>,
    kind: String,
    title: Option<String>,
    body: Option<String>,
) -> Result<(), String> {
    check_caller(&webview, &shared)?;
    if kind != "outlook" {
        return Err("unknown kind".into());
    }
    limited(&shared, |l| &mut l.notify)?;
    let show_content = shared
        .cfg
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .notification_show_content;
    let msg = Message::from_outlook(title.as_deref(), body.as_deref(), show_content);
    crate::log::write("Outlook-Webmitteilung empfangen");
    shared.notify(&app, msg);
    Ok(())
}

#[tauri::command]
pub fn pw_zoom<R: Runtime>(
    app: AppHandle<R>,
    webview: Webview<R>,
    shared: State<'_, Arc<Shared>>,
    direction: String,
) -> Result<(), String> {
    check_caller(&webview, &shared)?;
    limited(&shared, |l| &mut l.zoom)?;
    let step = match direction.as_str() {
        "in" => crate::Zoom::In,
        "out" => crate::Zoom::Out,
        "reset" => crate::Zoom::Reset,
        _ => return Err("unknown direction".into()),
    };
    if let Some(w) = app.get_webview_window(crate::MAIN) {
        crate::zoom(&w, &shared, step);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<PageState, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn pats() -> Vec<String> {
        vec!["erneut anmelden".into(), "sitzung".into()]
    }

    #[test]
    fn valid_report() {
        let s = parse(r#"{"unread":3,"inbox_found":true,"auth_banner":null,"error_page":false,"url_host":"outlook.office.com"}"#)
            .unwrap();
        let (r, p) = s.validate(&pats()).unwrap();
        assert_eq!(r.unread, Some(3));
        assert!(r.inbox_found && r.mail_view && !r.auth_banner);
        assert!(p.is_none());
    }

    #[test]
    fn banner_pattern_must_be_configured() {
        let (r, p) = parse(r#"{"auth_banner":"sitzung"}"#)
            .unwrap()
            .validate(&pats())
            .unwrap();
        assert!(r.auth_banner);
        assert_eq!(p.as_deref(), Some("sitzung"));
        let long = "a".repeat(5000);
        assert!(parse(&format!(r#"{{"auth_banner":"{long}"}}"#))
            .unwrap()
            .validate(&pats())
            .is_err());
        assert!(parse(r#"{"auth_banner":"made up"}"#)
            .unwrap()
            .validate(&pats())
            .is_err());
        let (r, _) = parse(r#"{"auth_banner":"  "}"#)
            .unwrap()
            .validate(&pats())
            .unwrap();
        assert!(!r.auth_banner);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(r#"{"unread":-1}"#).is_err());
        assert!(parse(r#"{"unread":"3"}"#).is_err());
        assert!(parse(r#"{"inbox_found":"yes"}"#).is_err());
        assert!(parse(r#"{"unread":99999999}"#)
            .unwrap()
            .validate(&pats())
            .is_err());
        // states the script never produces
        assert!(parse(r#"{"inbox_found":true}"#)
            .unwrap()
            .validate(&pats())
            .is_err());
        assert!(
            parse(r#"{"unread":1,"inbox_found":true,"error_page":true}"#)
                .unwrap()
                .validate(&pats())
                .is_err()
        );
    }
}

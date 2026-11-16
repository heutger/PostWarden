// SPDX-License-Identifier: AGPL-3.0-only

//! PostWarden: Outlook on the web in its own window, plus Dock badge, notifications
//! and re-authentication alerts. See `docs/ARCHITECTURE.md`.

mod commands;
mod config;
mod dock;
mod i18n;
mod log;
#[cfg(target_os = "macos")]
mod macos;
mod nav;
mod notify;
mod ratelimit;
mod state;

use config::Config;
use i18n::tr;
use nav::Decision;
use notify::{Debouncer, Message};
use state::{Effect, Machine, Reason, Settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};
use tauri::menu::{AboutMetadataBuilder, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::webview::{DownloadEvent, NewWindowFeatures, NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Manager, RunEvent, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_opener::OpenerExt;
use url::Url;

pub const MAIN: &str = "main";
const INJECT: &str = include_str!("../inject/postwarden.js");
const TICK: Duration = Duration::from_secs(5);
const ZOOM_LEVELS: [f64; 13] = [
    0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

pub struct Paths {
    pub config: PathBuf,
    pub logs: PathBuf,
}

/// Downloads of one window, for closing a popup that only existed for one download.
/// Tauri reports download events only by window label, and WebKit asks for the
/// download target (`Requested`) only some time after the response became a download.
/// So the popup is closed automatically only in the unambiguous case: one download in
/// an empty popup. A second download response, or a page shown, keeps it open for good.
#[derive(Debug, Default)]
struct PopupDownloads {
    close_when_done: bool,
    /// Auto-close ruled out for this window (ambiguous or used).
    keep_open: bool,
    /// Download responses not yet `Requested`, and downloads started and not finished.
    responses: u32,
    running: u32,
}

impl PopupDownloads {
    /// A main-frame response became a download; `shown`: the popup has shown a page.
    fn response(&mut self, shown: bool) {
        if shown || self.close_when_done || self.responses > 0 || self.running > 0 {
            self.keep_open = true;
        } else {
            self.close_when_done = true;
        }
        self.responses += 1;
    }
    /// WebKit asked for the target and PostWarden accepted (`accepted`) or refused it.
    fn requested(&mut self, accepted: bool) {
        self.responses = self.responses.saturating_sub(1);
        if accepted {
            self.running += 1;
        }
    }
    fn finished(&mut self) {
        self.running = self.running.saturating_sub(1);
    }
    fn page_shown(&mut self) {
        self.keep_open = true;
    }
    fn should_close(&self) -> bool {
        self.close_when_done && !self.keep_open && self.responses == 0 && self.running == 0
    }
}

struct NavState {
    /// Host of the last main-frame document that was allowed.
    main_host: Option<String>,
    /// Last URL handed to the browser (suppresses duplicates).
    last_external: Option<(String, Instant)>,
    /// Hand-offs to the browser/other apps overall (a page or iframe cannot flood them).
    external: ratelimit::RateLimit,
    /// Last time the main frame was sent back to `start_url` (loop guard).
    last_recover: Option<Instant>,
    /// Hosts already reported as unknown sign-in target (one notification each).
    idp_warned: Vec<String>,
    /// Popups that have committed an allowed (non-blank) document.
    popups_shown: std::collections::HashSet<String>,
    /// Empty popups whose only content is a running download (closed when it ends).
    popup_downloads: std::collections::HashMap<String, PopupDownloads>,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            main_host: None,
            last_external: None,
            external: ratelimit::RateLimit::new(5, Duration::from_secs(30)),
            last_recover: None,
            idp_warned: Vec::new(),
            popups_shown: std::collections::HashSet::new(),
            popup_downloads: std::collections::HashMap::new(),
        }
    }
}

/// State shared between commands, menu handlers, page hooks and the timer thread.
/// Lock order: never hold two of these locks at once.
pub struct Shared {
    pub paths: Paths,
    pub cfg: RwLock<Config>,
    machine: Mutex<Machine>,
    debouncer: Mutex<Debouncer>,
    nav: Mutex<NavState>,
    zoom: Mutex<f64>,
    last_logged: Mutex<String>,
    last_banner_pattern: Mutex<Option<String>>,
    pub limits: Mutex<ratelimit::Limits>,
    /// The main window's pre-commit navigation guard is in place. Without it nothing
    /// ever loads Outlook (fail closed) – also not a reload.
    guarded: std::sync::atomic::AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn settings_from(cfg: &Config) -> Settings {
    Settings {
        heartbeat: cfg.heartbeat(),
        reminder: cfg.reminder(),
        grace: Duration::from_secs(cfg.grace_seconds),
        bring_to_front_on_auth: cfg.bring_to_front_on_auth,
        notify_on_reconnect: cfg.notify_on_reconnect,
    }
}

impl Shared {
    fn new(paths: Paths, cfg: Config) -> Self {
        let machine = Machine::new(settings_from(&cfg), Instant::now());
        Self {
            paths,
            cfg: RwLock::new(cfg),
            machine: Mutex::new(machine),
            debouncer: Mutex::new(Debouncer::default()),
            nav: Mutex::new(NavState::default()),
            zoom: Mutex::new(1.0),
            last_logged: Mutex::new(String::new()),
            last_banner_pattern: Mutex::new(None),
            limits: Mutex::new(ratelimit::Limits::default()),
            guarded: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn cfg(&self) -> Config {
        self.cfg.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn machine(&self) -> MutexGuard<'_, Machine> {
        lock(&self.machine)
    }

    pub fn notify<R: Runtime>(&self, app: &AppHandle<R>, msg: Message) {
        let allowed = lock(&self.debouncer).allow(msg.category, Instant::now());
        if allowed {
            notify::show(app, &msg);
        }
    }

    pub fn note_banner_pattern(&self, pattern: &str) {
        *lock(&self.last_banner_pattern) = Some(pattern.to_string());
    }

    /// Logs status/reason/counter changes (never content).
    fn log_state(&self) {
        let pattern = lock(&self.last_banner_pattern).clone();
        let line = {
            let m = self.machine();
            format!(
                "Status {:?}{} · ungelesen {}",
                m.status(),
                m.reason()
                    .map(|r| match (r, pattern.as_deref()) {
                        // The configured pattern, never the banner text.
                        (Reason::Banner, Some(p)) => format!(" (banner: {p})"),
                        _ => format!(" ({})", r.as_str()),
                    })
                    .unwrap_or_default(),
                m.unread()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into())
            )
        };
        let mut last = lock(&self.last_logged);
        if *last != line {
            log::write(&line);
            *last = line;
        }
    }

    /// Opens `url` in the default browser/app; the same URL at most once per 3 s and at
    /// most 5 hand-offs per 30 s overall (a page or iframe cannot flood the browser).
    fn open_external<R: Runtime>(&self, app: &AppHandle<R>, url: &Url) {
        {
            let mut nav = lock(&self.nav);
            let now = Instant::now();
            if let Some((last, at)) = &nav.last_external {
                if last == url.as_str() && now.duration_since(*at) < Duration::from_secs(3) {
                    return;
                }
            }
            if !nav.external.allow(now) {
                drop(nav);
                log::write(&format!("Extern öffnen gedrosselt: {}", nav::redact(url)));
                return;
            }
            nav.last_external = Some((url.to_string(), now));
        }
        // Scheme and host only: the path of a link in a mail is mail content.
        log::write(&format!("Extern geöffnet: {}", nav::redact(url)));
        if app.opener().open_url(url.as_str(), None::<&str>).is_err() {
            // Never the error text: it can contain the full URL (launcher command line).
            log::write(&format!("Öffnen fehlgeschlagen: {}", nav::redact(url)));
        }
    }
}

/// Applies state-machine effects.
pub fn apply<R: Runtime>(app: &AppHandle<R>, shared: &Shared, effects: Vec<Effect>) {
    shared.log_state();
    let window = app.get_webview_window(MAIN);
    for effect in effects {
        match effect {
            Effect::Badge(_) => {
                // Timer thread and IPC may apply effects out of order. The badge is
                // therefore read from the current state *on the main thread*, in the
                // order the updates are queued there: the last one always shows the
                // newest state.
                if let Some(w) = window.clone() {
                    let state = app.state::<Arc<Shared>>().inner().clone();
                    let _ = app.run_on_main_thread(move || {
                        let badge = state.machine().badge();
                        dock::set_badge(&w, badge);
                    });
                }
            }
            Effect::Notify(alert) => shared.notify(app, Message::from_alert(alert)),
            Effect::BringToFront => {
                if let Some(w) = &window {
                    dock::bring_to_front(w);
                }
            }
            Effect::Reload => {
                // Final check and reload in one main-thread operation – the same thread
                // on which WebKit starts navigations (sign-in) and the guard records them.
                if let Some(w) = window.clone() {
                    let state = app.state::<Arc<Shared>>().inner().clone();
                    let _ = app.run_on_main_thread(move || {
                        if state.machine().reload_allowed(Instant::now()) {
                            log::write("Automatisches Neuladen");
                            reload(&w, &state);
                        }
                    });
                }
            }
        }
    }
}

/// Reloads the main window: a real reload on an Outlook page; otherwise – sign-in page,
/// or no document committed at all (first load failed because the network was not up
/// at login; `reload()` would do nothing) – `start_url` is loaded.
fn reload<R: Runtime>(window: &WebviewWindow<R>, shared: &Shared) {
    if !shared.guarded.load(Ordering::SeqCst) {
        log::write("Neu laden abgelehnt: Navigationsschutz nicht verfügbar");
        return;
    }
    let cfg = shared.cfg();
    let on_app = window
        .url()
        .ok()
        .is_some_and(|u| nav::https_host_in(&cfg.app_hosts, &u));
    if on_app {
        let _ = window.reload();
    } else if let Ok(start) = Url::parse(&cfg.start_url) {
        let _ = window.navigate(start);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Zoom {
    In,
    Out,
    Reset,
}

pub fn zoom<R: Runtime>(window: &WebviewWindow<R>, shared: &Shared, step: Zoom) {
    let mut z = lock(&shared.zoom);
    let next = match step {
        Zoom::Reset => 1.0,
        Zoom::In => ZOOM_LEVELS
            .iter()
            .copied()
            .find(|l| *l > *z + 0.001)
            .unwrap_or(*z),
        Zoom::Out => ZOOM_LEVELS
            .iter()
            .rev()
            .copied()
            .find(|l| *l < *z - 0.001)
            .unwrap_or(*z),
    };
    if window.set_zoom(next).is_ok() {
        *z = next;
    }
}

/// Main frame committed a new document (called for the main frame only).
fn on_main_frame<R: Runtime>(
    app: &AppHandle<R>,
    shared: &Shared,
    window: &WebviewWindow<R>,
    url: &Url,
) {
    let cfg = shared.cfg();
    let previous = lock(&shared.nav).main_host.clone();
    match nav::main_frame_decision(&cfg.main_frame_hosts(), &cfg.app_hosts, url) {
        Decision::Allow => {
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            let on_app = nav::https_host_in(&cfg.app_hosts, url);
            if previous.as_deref() != Some(host.as_str()) {
                let kind = if on_app {
                    "Outlook"
                } else if nav::host_in(&cfg.login_hosts, &host) {
                    "Anmeldeseite"
                } else {
                    "andere Seite"
                };
                log::write(&format!("Seite: {host} ({kind})"));
            }
            if !host.is_empty() {
                lock(&shared.nav).main_host = Some(host);
            }
            if url.scheme() != "about" {
                let fx = shared.machine().page_started(on_app, Instant::now());
                apply(app, shared, fx);
            }
        }
        decision => {
            // Fallback only: `NavigationGuard` (macos.rs) normally cancels such a
            // navigation before WebKit commits it. If a document still got through
            // (guard not installed yet, other WebKit path), leave it at once.
            log::write(&format!(
                "Fremdes Dokument im Hauptfenster: {} – wird verlassen",
                nav::redact(url)
            ));
            if previous.as_deref().is_some_and(|p| {
                nav::host_in(&cfg.login_hosts, p) || nav::host_in(&cfg.idp_hosts, p)
            }) {
                warn_unknown_idp(app, shared, url);
            }
            if decision == Decision::External {
                shared.open_external(app, url);
            }
            let now = Instant::now();
            let recover = {
                let mut nav = lock(&shared.nav);
                let ok = nav
                    .last_recover
                    .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10));
                if ok {
                    nav.last_recover = Some(now);
                }
                ok
            };
            let target = if recover {
                Url::parse(&cfg.start_url).ok()
            } else {
                // Repeating redirect: show an empty page instead of the foreign one.
                // No report follows, so the watchdog reports "Status unklar" and
                // reloads `start_url` with backoff – no false "sign in again".
                log::write("Weiterleitung wiederholt sich – leere Seite statt fremder Seite.");
                Url::parse("about:blank").ok()
            };
            if let Some(target) = target {
                let _ = window.navigate(target);
            }
            let fx = shared.machine().page_started(true, now);
            apply(app, shared, fx);
        }
    }
}

/// One notification per host when a sign-in flow leaves for an unconfigured host.
fn warn_unknown_idp<R: Runtime>(app: &AppHandle<R>, shared: &Shared, url: &Url) {
    let host = url.host_str().unwrap_or(url.scheme()).to_ascii_lowercase();
    log::write(&format!(
        "Anmeldung leitet auf {host} weiter – nicht freigegeben. Föderierte Anmeldung? Dann den Host in idp_hosts eintragen."
    ));
    let first = {
        let mut nav = lock(&shared.nav);
        let first = !nav.idp_warned.contains(&host);
        if first {
            nav.idp_warned.push(host.clone());
        }
        first
    };
    if first {
        let body = if i18n::english() {
            format!("Sign-in redirects to {host}, which is not allowed. If your organisation signs in there, add it to idp_hosts in the configuration.")
        } else {
            format!("Die Anmeldung leitet auf {host} weiter, das ist nicht freigegeben. Meldet sich deine Organisation dort an, den Host in der Konfiguration unter idp_hosts eintragen.")
        };
        shared.notify(
            app,
            Message::info(
                tr("Anmeldeseite nicht freigegeben", "Sign-in page not allowed"),
                body,
            ),
        );
    }
}

/// Pre-commit main-frame policy for the main window and popups (called by
/// `macos::NavigationGuard` before WebKit loads anything). `popup` is the popup window,
/// `None` for the main window.
fn guard_main_frame<R: Runtime>(
    app: &AppHandle<R>,
    shared: &Shared,
    target: &str,
    current: Option<&str>,
    popup: Option<&WebviewWindow<R>>,
) -> bool {
    let Ok(url) = Url::parse(target) else {
        log::write("Hauptframe-Navigation ohne gültige URL abgelehnt");
        return false;
    };
    let cfg = shared.cfg();
    let mut hosts = cfg.main_frame_hosts();
    if popup.is_some() {
        hosts.extend(cfg.popup_hosts.iter().cloned());
    }
    // Attachment hosts: WebKit only learns from the response that a navigation is a
    // download. Should one show a document instead, the post-commit check removes it.
    hosts.extend(cfg.download_hosts.iter().cloned());
    let decision = nav::main_frame_decision(&hosts, &cfg.app_hosts, &url);
    if decision == Decision::Allow {
        // Main window heads for a sign-in page: no automatic reload may interrupt it,
        // already before the page commits.
        let host = url.host_str().unwrap_or_default();
        if popup.is_none()
            && url.scheme() == "https"
            && (nav::host_in(&cfg.login_hosts, host) || nav::host_in(&cfg.idp_hosts, host))
        {
            shared.machine().sign_in_started(Instant::now());
        }
        return true;
    }
    let current = current.and_then(|c| Url::parse(c).ok());
    let current_host = current
        .as_ref()
        .and_then(|c| c.host_str())
        .map(str::to_ascii_lowercase);
    if current_host
        .as_deref()
        .is_some_and(|h| nav::host_in(&cfg.login_hosts, h) || nav::host_in(&cfg.idp_hosts, h))
    {
        warn_unknown_idp(app, shared, &url);
    }
    log::write(&format!(
        "{} nicht freigegeben: {}",
        if popup.is_some() {
            "Popup-Ziel"
        } else {
            "Ziel"
        },
        nav::redact(&url)
    ));
    if decision == Decision::External {
        shared.open_external(app, &url);
    }
    // A popup that never showed an allowed page has nothing left to show. (Its URL is
    // not reliable here: during a redirect it is the provisional, allowed URL.)
    if let Some(w) = popup {
        if !lock(&shared.nav).popups_shown.contains(w.label()) {
            let _ = w.close();
        }
    }
    false
}

/// Installs the pre-commit main-frame policy on `window`.
#[cfg(target_os = "macos")]
/// Returns whether the guard is in place. Called on the main thread, where
/// `with_webview` runs its closure inline; anything else counts as failure.
fn install_guard<R: Runtime>(
    window: &WebviewWindow<R>,
    app: &AppHandle<R>,
    shared: Arc<Shared>,
    is_popup: bool,
) -> bool {
    use std::sync::atomic::AtomicBool;
    let app = app.clone();
    let app_for_response = app.clone();
    let popup = is_popup.then(|| window.clone());
    let label = window.label().to_string();
    let installed = Arc::new(AtomicBool::new(false));
    let flag = installed.clone();
    let dispatched = window.with_webview(move |wv| {
        let shared_dl = shared.clone();
        let popup_dl = popup.clone();
        let policy: macos::MainFramePolicy = Box::new(move |target, current| {
            guard_main_frame(&app, &shared, target, current, popup.as_ref())
        });
        let app_dl = app_for_response.clone();
        let response_policy: macos::ResponsePolicy = Box::new(move |url, attachment, can_show| {
            response_policy(
                &app_dl,
                &shared_dl,
                popup_dl.as_ref(),
                url,
                attachment,
                can_show,
            )
        });
        // SAFETY: handle comes straight from Tauri, closure runs on the main thread.
        let ok =
            unsafe { macos::install_navigation_guard(wv.inner(), &label, policy, response_policy) };
        flag.store(ok, Ordering::SeqCst);
    });
    let ok = dispatched.is_ok() && installed.load(Ordering::SeqCst);
    if !ok {
        log::write("Navigationsprüfung vor dem Laden nicht verfügbar");
    } else if !is_popup {
        log::write("Navigationsprüfung vor dem Laden aktiv");
    }
    ok
}

/// Main-frame responses from hosts that are only allowed as download sources: an
/// attachment (header) or a non-displayable type is saved as a download; a displayable
/// document (an error or status page, …) is not shown in PostWarden and not saved – it
/// opens in the default browser. Responses from document hosts are not touched.
#[cfg(target_os = "macos")]
fn response_policy<R: Runtime>(
    app: &AppHandle<R>,
    shared: &Shared,
    popup: Option<&WebviewWindow<R>>,
    url: &str,
    attachment: bool,
    can_show: bool,
) -> macos::ResponseDecision {
    use macos::ResponseDecision;
    let Ok(url) = Url::parse(url) else {
        return ResponseDecision::Forward;
    };
    let cfg = shared.cfg();
    let mut documents = cfg.main_frame_hosts();
    if popup.is_some() {
        documents.extend(cfg.popup_hosts.iter().cloned());
    }
    if !nav::https_host_in(&cfg.download_hosts, &url) || nav::https_host_in(&documents, &url) {
        return ResponseDecision::Forward;
    }
    let shown = popup.is_some_and(|w| lock(&shared.nav).popups_shown.contains(w.label()));
    if attachment || !can_show {
        // An empty popup is closed only once its download has ended (download_handler).
        if let Some(w) = popup {
            lock(&shared.nav)
                .popup_downloads
                .entry(w.label().to_string())
                .or_default()
                .response(shown);
        }
        return ResponseDecision::Download;
    }
    log::write(&format!(
        "Seite eines Download-Hosts nicht im Fenster: {}",
        nav::redact(&url)
    ));
    shared.open_external(app, &url);
    if let (Some(w), false) = (popup, shown) {
        let _ = w.close();
    }
    ResponseDecision::Cancel
}

/// Downloads (main window and popups): WebKit saves into ~/Downloads. WebKit decides
/// a download before any navigation policy runs, so the source is checked here: only
/// Outlook's own data (`blob:`/`data:` of the page, `app_hosts`, `download_hosts`).
/// Anything else goes to the default browser like other external targets.
fn download_handler<R: Runtime>(
    app: AppHandle<R>,
    shared: Arc<Shared>,
) -> impl Fn(tauri::Webview<R>, DownloadEvent<'_>) -> bool + Send + Sync + 'static {
    move |webview, event| match event {
        DownloadEvent::Requested { url, .. } => {
            let cfg = shared.cfg();
            let mut hosts = cfg.app_hosts.clone();
            hosts.extend(cfg.download_hosts.iter().cloned());
            match nav::download_decision(&hosts, &cfg.app_hosts, &url) {
                Decision::Allow => {
                    log::write("Download gestartet");
                    // Counted for every window, so a later "close when done" never
                    // closes a popup while an earlier download still runs.
                    lock(&shared.nav)
                        .popup_downloads
                        .entry(webview.label().to_string())
                        .or_default()
                        .requested(true);
                    true
                }
                decision => {
                    log::write(&format!(
                        "Download aus nicht freigegebener Quelle: {}",
                        nav::redact(&url)
                    ));
                    if decision == Decision::External {
                        shared.open_external(&app, &url);
                    }
                    if let Some(d) = lock(&shared.nav).popup_downloads.get_mut(webview.label()) {
                        d.requested(false);
                    }
                    close_if_download_popup(&app, &shared, webview.label());
                    false
                }
            }
        }
        DownloadEvent::Finished { success, .. } => {
            log::write(if success {
                "Download abgeschlossen"
            } else {
                "Download fehlgeschlagen"
            });
            if let Some(d) = lock(&shared.nav).popup_downloads.get_mut(webview.label()) {
                d.finished();
            }
            close_if_download_popup(&app, &shared, webview.label());
            true
        }
        _ => true,
    }
}

/// A popup that only existed for downloads is closed once none is running any more
/// (or the only one was refused) – not earlier, so WebKit and Tauri still have it while
/// a download runs. A popup that has shown a page meanwhile stays open.
fn close_if_download_popup<R: Runtime>(app: &AppHandle<R>, shared: &Shared, label: &str) {
    let close = {
        let mut nav = lock(&shared.nav);
        let shown = nav.popups_shown.contains(label);
        let done = !shown
            && nav
                .popup_downloads
                .get(label)
                .is_some_and(PopupDownloads::should_close);
        if done {
            nav.popup_downloads.remove(label);
        }
        done
    };
    if close {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.close();
        }
    }
}

/// Popup for an allowed `window.open`/`target=_blank` (sign-in, attachment viewer, …).
/// WebKit requires the opener's configuration (shared scripts and IPC channel; the
/// script never monitors there, see `arm()` in postwarden.js). Unlike a bare WebKit
/// popup it gets PostWarden's navigation policy: its main frame may only show
/// Outlook, Microsoft sign-in, configured IdPs and `popup_hosts`; anything else is
/// opened in the default browser and the popup is closed.
fn build_popup<R: Runtime>(
    app: &AppHandle<R>,
    shared: Arc<Shared>,
    url: &Url,
    features: NewWindowFeatures,
) -> tauri::Result<Option<WebviewWindow<R>>> {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let label = format!("popup-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let app_nav = app.clone();
    let shared_nav = shared.clone();
    let app_new = app.clone();
    let shared_new = shared.clone();
    let app_load = app.clone();
    let shared_load = shared.clone();
    let blank = Url::parse("about:blank").expect("static URL");
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::External(blank))
        .window_features(features)
        // Tauri's own file-drop handling would swallow all drag events: HTML5 drag and
        // drop in Outlook (moving mails to folders, attaching files) needs WebKit's.
        .disable_drag_drop_handler()
        .title(url.host_str().unwrap_or("PostWarden"))
        .on_document_title_changed(|w, title| {
            let _ = w.set_title(&title);
        })
        .on_navigation(move |u| frame_navigation(&app_nav, &shared_nav, u))
        .on_new_window(move |u, _features| {
            // No popups from popups: hand external targets to the browser.
            if nav::new_window_decision(&[], &u) == Decision::External {
                shared_new.open_external(&app_new, &u);
            }
            NewWindowResponse::Deny
        })
        .on_download(download_handler(app.clone(), shared.clone()))
        .on_page_load(move |w, payload| {
            if payload.event() != PageLoadEvent::Started {
                return;
            }
            // Fallback behind the pre-commit `NavigationGuard`.
            let u = payload.url();
            let shared = &shared_load;
            let cfg = shared.cfg();
            let mut hosts = cfg.main_frame_hosts();
            hosts.extend(cfg.popup_hosts.iter().cloned());
            match nav::main_frame_decision(&hosts, &cfg.app_hosts, u) {
                Decision::Allow => {
                    if u.scheme() != "about" {
                        // A page is shown now: never close this popup after a download.
                        let mut nav = lock(&shared.nav);
                        nav.popups_shown.insert(w.label().to_string());
                        if let Some(d) = nav.popup_downloads.get_mut(w.label()) {
                            d.page_shown();
                        }
                    }
                }
                decision => {
                    log::write(&format!(
                        "Popup verlässt freigegebene Hosts: {}",
                        nav::redact(u)
                    ));
                    if decision == Decision::External {
                        shared.open_external(&app_load, u);
                    }
                    let _ = w.close();
                }
            }
        })
        .build()?;
    // Without the pre-commit guard the popup is not handed to WebKit.
    #[cfg(target_os = "macos")]
    if !install_guard(&window, app, shared, true) {
        let _ = window.close();
        return Ok(None);
    }
    Ok(Some(window))
}

/// Navigation of any frame in the main window or a popup.
fn frame_navigation<R: Runtime>(app: &AppHandle<R>, shared: &Shared, url: &Url) -> bool {
    match nav::navigation_decision(url) {
        Decision::Allow => true,
        Decision::External => {
            shared.open_external(app, url);
            false
        }
        Decision::Block => {
            log::write(&format!("Navigation blockiert: {}", url.scheme()));
            false
        }
    }
}

fn build_window<R: Runtime>(
    app: &AppHandle<R>,
    shared: Arc<Shared>,
) -> tauri::Result<WebviewWindow<R>> {
    let cfg = shared.cfg();
    let start = Url::parse(&cfg.start_url).expect("start_url validated in config");

    let app_nav = app.clone();
    let shared_nav = shared.clone();
    let app_new = app.clone();
    let shared_new = shared.clone();
    let app_load = app.clone();
    let shared_load = shared.clone();
    let downloads = download_handler(app.clone(), shared.clone());

    // Start empty: Outlook is loaded only once the pre-commit guard is in place.
    let blank = Url::parse("about:blank").expect("static URL");
    let mut builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::External(blank))
        .title("PostWarden")
        // Tauri's own file-drop handling would swallow all drag events: HTML5 drag and
        // drop in Outlook (moving mails to folders, attaching files) needs WebKit's.
        .disable_drag_drop_handler()
        .inner_size(1280.0, 860.0)
        .min_inner_size(640.0, 480.0)
        .initialization_script(INJECT)
        .on_navigation(move |url| frame_navigation(&app_nav, &shared_nav, url))
        .on_new_window(move |url, features| {
            let popup_hosts = shared_new.cfg().popup_hosts;
            match nav::new_window_decision(&popup_hosts, &url) {
                Decision::Allow => {
                    match build_popup(&app_new, shared_new.clone(), &url, features) {
                        Ok(Some(window)) => NewWindowResponse::Create { window },
                        Ok(None) | Err(_) => {
                            // No error text: it may contain the target URL.
                            log::write(&format!(
                                "Popup konnte nicht geöffnet werden: {}",
                                nav::redact(&url)
                            ));
                            NewWindowResponse::Deny
                        }
                    }
                }
                Decision::External => {
                    shared_new.open_external(&app_new, &url);
                    NewWindowResponse::Deny
                }
                Decision::Block => NewWindowResponse::Deny,
            }
        })
        .on_download(downloads)
        .on_page_load(move |window, payload| match payload.event() {
            PageLoadEvent::Started => {
                on_main_frame(&app_load, &shared_load, &window, payload.url());
            }
            PageLoadEvent::Finished => {
                // Only the main window starts the monitoring in the shared script.
                let _ = window.eval("window.__pw && window.__pw.arm()");
            }
        });
    if let Some(ua) = cfg.user_agent.as_deref().filter(|s| !s.trim().is_empty()) {
        builder = builder.user_agent(ua);
    }
    let window = builder.build()?;

    #[cfg(target_os = "macos")]
    let guarded = install_guard(&window, app, shared.clone(), false);
    #[cfg(not(target_os = "macos"))]
    let guarded = true;
    shared.guarded.store(guarded, Ordering::SeqCst);
    if guarded {
        let _ = window.navigate(start);
    } else {
        // Fail closed: e.g. a Tauri/wry update changed the web view internals.
        shared.notify(
            app,
            Message::info(
                tr("Navigationsschutz nicht verfügbar", "Navigation protection unavailable"),
                tr(
                    "PostWarden lädt Outlook nicht ohne Navigationsschutz. Bitte auf eine neuere PostWarden-Version aktualisieren.",
                    "PostWarden does not load Outlook without navigation protection. Please update PostWarden.",
                ),
            ),
        );
    }

    #[cfg(target_os = "macos")]
    {
        let _ = window.with_webview(|wv| {
            // SAFETY: handle comes straight from Tauri, closure runs on the main thread.
            let ok = unsafe { macos::disable_webview_throttling(wv.inner()) };
            log::write(if ok {
                "WebView-Drosselung im Hintergrund deaktiviert"
            } else {
                "WebView-Drosselung nicht abschaltbar (macOS < 14)"
            });
        });
    }
    Ok(window)
}

fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let sep = || PredefinedMenuItem::separator(app);
    let about = AboutMetadataBuilder::new()
        .name(Some("PostWarden"))
        .version(Some(env!("CARGO_PKG_VERSION")))
        // macOS shows `credits` and `copyright` (not `comments`/`license`).
        .credits(Some(tr("Ein Wrapper für Outlook im Web.\nMicrosoft und Outlook sind Marken der Microsoft-Unternehmensgruppe; PostWarden ist ein unabhängiges Projekt.", "A wrapper for Outlook on the web.\nMicrosoft and Outlook are trademarks of the Microsoft group of companies; PostWarden is an independent project.")))
        .copyright(Some(tr(
            "Copyright © 2026 Christian Heutger. Lizenz: AGPL-3.0-only.",
            "Copyright © 2026 Christian Heutger. Licensed under AGPL-3.0-only.",
        )))
        .build();

    let app_menu = Submenu::with_items(
        app,
        "PostWarden",
        true,
        &[
            &PredefinedMenuItem::about(
                app,
                Some(tr("Über PostWarden", "About PostWarden")),
                Some(about),
            )?,
            &sep()?,
            &MenuItem::with_id(
                app,
                "settings",
                tr("Einstellungen…", "Settings…"),
                true,
                Some("CmdOrCtrl+,"),
            )?,
            &sep()?,
            &PredefinedMenuItem::services(app, Some(tr("Dienste", "Services")))?,
            &sep()?,
            &PredefinedMenuItem::hide(app, Some(tr("PostWarden ausblenden", "Hide PostWarden")))?,
            &PredefinedMenuItem::hide_others(app, Some(tr("Andere ausblenden", "Hide Others")))?,
            &PredefinedMenuItem::show_all(app, Some(tr("Alle einblenden", "Show All")))?,
            &sep()?,
            &PredefinedMenuItem::quit(app, Some(tr("PostWarden beenden", "Quit PostWarden")))?,
        ],
    )?;
    let edit = Submenu::with_items(
        app,
        tr("Bearbeiten", "Edit"),
        true,
        &[
            &PredefinedMenuItem::undo(app, Some(tr("Widerrufen", "Undo")))?,
            &PredefinedMenuItem::redo(app, Some(tr("Wiederholen", "Redo")))?,
            &sep()?,
            &PredefinedMenuItem::cut(app, Some(tr("Ausschneiden", "Cut")))?,
            &PredefinedMenuItem::copy(app, Some(tr("Kopieren", "Copy")))?,
            &PredefinedMenuItem::paste(app, Some(tr("Einsetzen", "Paste")))?,
            &PredefinedMenuItem::select_all(app, Some(tr("Alles auswählen", "Select All")))?,
        ],
    )?;
    let view = Submenu::with_items(
        app,
        tr("Darstellung", "View"),
        true,
        &[
            &MenuItem::with_id(
                app,
                "reload",
                tr("Neu laden", "Reload"),
                true,
                Some("CmdOrCtrl+R"),
            )?,
            &sep()?,
            &MenuItem::with_id(
                app,
                "zoom_in",
                tr("Vergrößern", "Zoom In"),
                true,
                Some("CmdOrCtrl+="),
            )?,
            &MenuItem::with_id(
                app,
                "zoom_out",
                tr("Verkleinern", "Zoom Out"),
                true,
                Some("CmdOrCtrl+-"),
            )?,
            &MenuItem::with_id(
                app,
                "zoom_reset",
                tr("Originalgröße", "Actual Size"),
                true,
                Some("CmdOrCtrl+0"),
            )?,
            &sep()?,
            &PredefinedMenuItem::fullscreen(app, Some(tr("Vollbild", "Enter Full Screen")))?,
        ],
    )?;
    let diagnose = Submenu::with_items(
        app,
        tr("Diagnose", "Diagnostics"),
        true,
        &[
            &MenuItem::with_id(
                app,
                "config_open",
                tr("Konfigurationsdatei öffnen", "Open Configuration File"),
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(
                app,
                "config_reload",
                tr("Konfiguration neu laden", "Reload Configuration"),
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(
                app,
                "log_open",
                tr("Protokoll anzeigen", "Show Log"),
                true,
                None::<&str>,
            )?,
            &sep()?,
            &MenuItem::with_id(
                app,
                "test_notification",
                tr("Testmitteilung senden", "Send Test Notification"),
                true,
                None::<&str>,
            )?,
        ],
    )?;
    let window = Submenu::with_items(
        app,
        tr("Fenster", "Window"),
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some(tr("Im Dock ablegen", "Minimize")))?,
            &PredefinedMenuItem::close_window(app, Some(tr("Fenster schließen", "Close Window")))?,
            &sep()?,
            &MenuItem::with_id(
                app,
                "show",
                tr("PostWarden-Fenster", "PostWarden Window"),
                true,
                None::<&str>,
            )?,
        ],
    )?;
    Menu::with_items(app, &[&app_menu, &edit, &view, &diagnose, &window])
}

fn open_path<R: Runtime>(app: &AppHandle<R>, path: &std::path::Path, with: Option<&str>) {
    if let Err(e) = app.opener().open_path(path.display().to_string(), with) {
        log::write(&format!("Öffnen fehlgeschlagen: {e}"));
    }
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let shared = app.state::<Arc<Shared>>().inner().clone();
    let window = app.get_webview_window(MAIN);
    match event.id().as_ref() {
        "reload" => {
            if let Some(w) = &window {
                reload(w, &shared);
            }
        }
        "zoom_in" | "zoom_out" | "zoom_reset" => {
            let step = match event.id().as_ref() {
                "zoom_in" => Zoom::In,
                "zoom_out" => Zoom::Out,
                _ => Zoom::Reset,
            };
            if let Some(w) = &window {
                zoom(w, &shared, step);
            }
        }
        "settings" | "config_open" => open_path(app, &shared.paths.config, Some("TextEdit")),
        "config_reload" => reload_config(app, &shared),
        "log_open" => open_path(
            app,
            &shared.paths.logs.join("postwarden.log"),
            Some("Console"),
        ),
        "test_notification" => shared.notify(
            app,
            Message::info(
                tr("Testmitteilung", "Test notification"),
                tr(
                    "Mitteilungen von PostWarden funktionieren.",
                    "Notifications from PostWarden are working.",
                ),
            ),
        ),
        "show" => {
            if let Some(w) = &window {
                dock::bring_to_front(w);
            }
        }
        _ => {}
    }
}

fn reload_config<R: Runtime>(app: &AppHandle<R>, shared: &Shared) {
    let (cfg, warning) = config::load(&shared.paths.config);
    if let Some(w) = &warning {
        log::write(w);
        shared.notify(
            app,
            Message::info(
                tr("Konfiguration fehlerhaft", "Configuration invalid"),
                tr(
                    "config.json ist fehlerhaft – die bisherige Konfiguration bleibt aktiv. Details im Protokoll.",
                    "config.json is invalid – the previous configuration stays active. Details in the log.",
                ),
            ),
        );
        return;
    }
    warn_hosts_without_ipc(&cfg);
    shared.machine().set_settings(settings_from(&cfg));
    sync_autostart(app, &cfg);
    *shared.cfg.write().unwrap_or_else(|e| e.into_inner()) = cfg;
    log::write("Konfiguration neu geladen");
    // The page fetches the new selectors/patterns on load.
    if let Some(w) = app.get_webview_window(MAIN) {
        reload(&w, shared);
    }
    shared.notify(
        app,
        Message::info(
            tr("Konfiguration neu geladen", "Configuration reloaded"),
            tr("Die Seite wird neu geladen.", "The page is being reloaded."),
        ),
    );
}

/// Config problem at start (defaults are active): logged in German, the notification
/// gets a short hint in the UI language.
fn config_warning(_w: &str) -> String {
    tr(
        "config.json ist fehlerhaft oder nicht lesbar – Standardwerte sind aktiv. Details im Protokoll.",
        "config.json is invalid or unreadable – defaults are active. Details in the log.",
    )
    .into()
}

fn warn_hosts_without_ipc(cfg: &Config) {
    let hosts = cfg.hosts_without_ipc();
    if !hosts.is_empty() {
        log::write(&format!(
            "app_hosts ohne IPC-Freigabe (capabilities/remote.json): {} – dort keine Überwachung",
            hosts.join(", ")
        ));
    }
}

/// Launch at login via LaunchAgent – only for the installed .app, never for dev builds.
fn sync_autostart<R: Runtime>(app: &AppHandle<R>, cfg: &Config) {
    use tauri_plugin_autostart::ManagerExt;
    let in_bundle = std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false);
    if !in_bundle {
        return;
    }
    let launcher = app.autolaunch();
    let enabled = launcher.is_enabled().unwrap_or(false);
    let result = match (cfg.launch_at_login, enabled) {
        (true, false) => launcher.enable(),
        (false, true) => launcher.disable(),
        _ => Ok(()),
    };
    if let Err(e) = result {
        log::write(&format!("Autostart konnte nicht gesetzt werden: {e}"));
    }
}

fn start_timer<R: Runtime>(app: AppHandle<R>, shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("postwarden-timer".into())
        .spawn(move || {
            let mut last_poll = Instant::now();
            loop {
                std::thread::sleep(TICK);
                let fx = shared.machine().tick(Instant::now());
                apply(&app, &shared, fx);
                let poll = Duration::from_secs(shared.cfg().poll_seconds);
                if last_poll.elapsed() >= poll {
                    last_poll = Instant::now();
                    // Rust-side trigger in case page timers are throttled in the background.
                    if let Some(w) = app.get_webview_window(MAIN) {
                        let _ = w.eval("window.__pw && window.__pw.tick()");
                    }
                }
            }
        })
        .expect("spawn timer thread");
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn run() {
    let support = home().join("Library/Application Support/PostWarden");
    let paths = Paths {
        config: support.join("config.json"),
        logs: home().join("Library/Logs/PostWarden"),
    };
    log::init(paths.logs.clone());
    log::write(&format!("PostWarden {} startet", env!("CARGO_PKG_VERSION")));
    let (cfg, warning) = config::load(&paths.config);
    if let Some(w) = &warning {
        log::write(w);
    }
    warn_hosts_without_ipc(&cfg);
    let shared = Arc::new(Shared::new(paths, cfg));

    let app = tauri::Builder::default()
        // No JS link interception: its script would run in the Microsoft pages and
        // try IPC the remote page is not granted. Links are handled in Rust.
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                // Popups keep the size Outlook asks for; only the main window is restored.
                .with_filter(|label| label == MAIN)
                .build(),
        )
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::pw_get_config,
            commands::pw_report,
            commands::pw_notify,
            commands::pw_zoom,
        ])
        .menu(build_menu)
        .on_menu_event(on_menu)
        .on_window_event(|window, event| match event {
            // Closing (red button / ⌘W) only hides – monitoring continues.
            WindowEvent::CloseRequested { api, .. } if window.label() == MAIN => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Destroyed => {
                #[cfg(target_os = "macos")]
                macos::release_navigation_guard(window.label());
                if let Some(shared) = window.try_state::<Arc<Shared>>() {
                    let mut nav = lock(&shared.nav);
                    nav.popups_shown.remove(window.label());
                    nav.popup_downloads.remove(window.label());
                }
            }
            _ => {}
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            #[cfg(target_os = "macos")]
            {
                macos::disable_app_nap();
                let h = handle.clone();
                macos::on_did_become_active(move || {
                    if let Some(w) = h.get_webview_window(MAIN) {
                        if !w.is_visible().unwrap_or(true) {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                });
            }
            {
                use tauri_plugin_notification::NotificationExt;
                let _ = handle.notification().request_permission();
            }
            let cfg = shared.cfg();
            sync_autostart(&handle, &cfg);
            if let Some(w) = &warning {
                shared.notify(
                    &handle,
                    Message::info(
                        tr("Konfiguration fehlerhaft", "Configuration invalid"),
                        config_warning(w),
                    ),
                );
            }
            build_window(&handle, shared.clone())?;
            start_timer(handle, shared.clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building PostWarden");

    app.run(|app, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => {
            if let Some(w) = app.get_webview_window(MAIN) {
                dock::bring_to_front(&w);
            }
        }
        RunEvent::Exit => log::write("PostWarden beendet"),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::PopupDownloads;

    #[test]
    fn single_download_closes_empty_popup_after_it_ends() {
        let mut d = PopupDownloads::default();
        d.response(false);
        assert!(!d.should_close()); // target not requested yet
        d.requested(true);
        assert!(!d.should_close()); // running
        d.finished();
        assert!(d.should_close());
    }

    #[test]
    fn refused_download_closes_empty_popup() {
        let mut d = PopupDownloads::default();
        d.response(false);
        d.requested(false);
        assert!(d.should_close());
    }

    #[test]
    fn overlapping_downloads_keep_popup_open() {
        // Response A → Requested A → Response B → Finished A → Requested B → Finished B
        let mut d = PopupDownloads::default();
        d.response(false);
        d.requested(true);
        d.response(false);
        d.finished();
        assert!(!d.should_close(), "B is not even requested yet");
        d.requested(true);
        d.finished();
        assert!(!d.should_close(), "two downloads: ambiguous, stays open");
    }

    #[test]
    fn popup_that_showed_a_page_stays_open() {
        let mut d = PopupDownloads::default();
        d.response(false);
        d.requested(true);
        d.page_shown();
        d.finished();
        assert!(!d.should_close());
        let mut d = PopupDownloads::default();
        d.response(true); // download from a popup that already shows a page
        d.requested(true);
        d.finished();
        assert!(!d.should_close());
    }
}

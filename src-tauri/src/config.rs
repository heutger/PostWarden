// SPDX-License-Identifier: AGPL-3.0-only

//! `config.json` in `~/Library/Application Support/PostWarden/`.
//!
//! The file is created with defaults on first start. Every field is optional: missing
//! fields fall back to their default, so older config files keep working after updates.
//! Selectors and text patterns for the DOM detection live here, not in the code,
//! because Outlook's DOM changes without notice.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Outlook hosts with IPC access – must match `capabilities/remote.json` and the host
/// check at the top of `inject/postwarden.js`.
pub const IPC_HOSTS: [&str; 4] = [
    "outlook.office.com",
    "outlook.office365.com",
    "outlook.cloud.microsoft",
    "outlook.live.com",
];

/// Banner pattern lists that earlier versions wrote as defaults (November 2026),
/// oldest first. Only a list exactly equal to one of these (same order) is migrated.
const V1_PATTERNS: &[&str] = &[
    "erneut anmelden",
    "neu anmelden",
    "melden sie sich (erneut|neu) an",
    "anmeldung erforderlich",
    "bestätigen sie",
    "identität",
    "sitzung",
    "aktion erforderlich",
    "sign in again",
    "sign in to continue",
    "verify",
    "reauthenticate",
    "session (has )?expired",
    "action required",
];
const V2_NEW: &[&str] = &[
    "sie müssen sich anmelden",
    "sitzung ist abgelaufen",
    "you need to sign in",
    "your session has expired",
];
const V3_NEW: &[&str] = &[
    "sie müssen sich anmelden",
    "sitzung (ist )?abgelaufen",
    "you need to sign in",
    "your session has expired",
];
/// Error-page pattern list of the first versions (with generic connection texts).
const V1_ERROR_PATTERNS: &[&str] = &[
    "etwas ist schiefgelaufen",
    "something went wrong",
    "keine verbindung",
    "no (internet )?connection",
    "sie sind offline",
    "you('| a)re offline",
];

fn old_banner_pattern_lists() -> Vec<Vec<&'static str>> {
    let v2 = V2_NEW.iter().chain(V1_PATTERNS).copied().collect();
    let v3 = V3_NEW.iter().chain(V1_PATTERNS).copied().collect();
    vec![V1_PATTERNS.to_vec(), v2, v3]
}

/// Exactly the former default: same entries in the same order. A reordered list may
/// be a deliberate prioritisation (the first banner pattern names the log line), so
/// it counts as edited and is kept.
fn same_list(list: &[String], known: &[&str]) -> bool {
    list.len() == known.len() && list.iter().zip(known).all(|(l, k)| l == k)
}

/// Banner selectors of the first default config (without dialogs).
const OLD_BANNER_SELECTORS: &[&str] = &[
    "[role='alert']",
    "[role='status']",
    "[class*='MessageBar']",
    "[class*='messageBar']",
    "[class*='banner']",
];

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// URL loaded at start.
    pub start_url: String,
    /// Hosts that run Outlook itself. The injected script reports only from these
    /// (they must match `capabilities/remote.json`). A main frame on any other host
    /// counts as "sign-in required".
    pub app_hosts: Vec<String>,
    /// Federated identity providers of the tenant (e.g. ADFS) that the sign-in flow
    /// redirects to. Together with `app_hosts` and `login_hosts` these are the only
    /// hosts the main frame may show; everything else opens in the default browser.
    /// Exact hosts or `*.example.com` (matches `example.com` and all subdomains).
    /// The former `allowed_hosts` field is ignored (it was too broad).
    pub idp_hosts: Vec<String>,
    /// Targets of `window.open`/`target=_blank` that open in a PostWarden popup.
    /// All other new-window targets (e.g. links in mails) open in the default browser.
    pub popup_hosts: Vec<String>,
    /// Download sources besides `app_hosts` (attachments). Downloads from anywhere else
    /// are handed to the default browser.
    pub download_hosts: Vec<String>,
    /// Microsoft sign-in hosts. Shown inside the app (main frame), page counts as
    /// "sign-in required".
    pub login_hosts: Vec<String>,
    pub unread: UnreadConfig,
    pub auth_banner: AuthBannerConfig,
    pub error_page: ErrorPageConfig,
    /// No inbox / no report from the page for this long → "Status unklar".
    pub heartbeat_minutes: u64,
    /// Repeat the alert while a problem persists (0 = off).
    pub auth_reminder_minutes: u64,
    /// A problem must persist this long before it is reported (filters redirects
    /// through the sign-in page and short flickers).
    pub grace_seconds: u64,
    /// Safety polling of the page detection.
    pub poll_seconds: u64,
    pub bring_to_front_on_auth: bool,
    pub notify_on_reconnect: bool,
    /// Pass sender/subject of Outlook's own web notifications on to macOS.
    pub notification_show_content: bool,
    pub launch_at_login: bool,
    /// Only set if Outlook rejects the WebView (e.g. the UA string of the installed Safari).
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UnreadConfig {
    pub inbox_names: Vec<String>,
    pub selectors: Vec<String>,
    pub count_regex: String,
    pub title_regex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AuthBannerConfig {
    pub selectors: Vec<String>,
    /// Case-insensitive JavaScript regular expressions.
    pub patterns: Vec<String>,
    /// Additional areas whose elements are never treated as banner (message list,
    /// reading pane and compose areas are always excluded by the injected script).
    pub exclude_within: Vec<String>,
    /// Longer texts are containers, not banners.
    pub max_text_length: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ErrorPageConfig {
    /// Elements whose visible text is checked (error screens put their message into a
    /// heading or alert; plain UI text such as calendar items is not checked).
    pub selectors: Vec<String>,
    /// Case-insensitive JavaScript regular expressions, checked against the visible text
    /// of `selectors` outside mail areas whenever no visible inbox is found.
    pub patterns: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            start_url: "https://outlook.office.com/mail/".into(),
            app_hosts: strings(&[
                "outlook.office.com",
                "outlook.office365.com",
                "outlook.cloud.microsoft",
                "outlook.live.com",
            ]),
            idp_hosts: Vec::new(),
            popup_hosts: strings(&[
                "outlook.office.com",
                "outlook.office365.com",
                "outlook.cloud.microsoft",
                "outlook.live.com",
                "login.microsoftonline.com",
                "login.microsoft.com",
                "login.live.com",
                "*.officeapps.live.com",
            ]),
            download_hosts: strings(&["attachments.office.net"]),
            login_hosts: strings(&[
                "login.microsoftonline.com",
                "login.microsoft.com",
                "login.live.com",
                "login.windows.net",
                "device.login.microsoftonline.com",
                "autologon.microsoftazuread-sso.com",
                "mysignins.microsoft.com",
                "account.activedirectory.windowsazure.com",
                "passwordreset.microsoftonline.com",
            ]),
            unread: UnreadConfig::default(),
            auth_banner: AuthBannerConfig::default(),
            error_page: ErrorPageConfig::default(),
            heartbeat_minutes: 5,
            auth_reminder_minutes: 30,
            grace_seconds: 10,
            poll_seconds: 30,
            bring_to_front_on_auth: true,
            notify_on_reconnect: true,
            notification_show_content: false,
            launch_at_login: true,
            user_agent: None,
        }
    }
}

impl Default for UnreadConfig {
    fn default() -> Self {
        Self {
            inbox_names: strings(&["Posteingang", "Inbox"]),
            selectors: strings(&["[role='treeitem']", "[role='tree'] [title]"]),
            count_regex: r"(\d+)\s*(ungelesen|unread)".into(),
            title_regex: r"^\((\d+)\)".into(),
        }
    }
}

impl Default for AuthBannerConfig {
    fn default() -> Self {
        Self {
            selectors: strings(&[
                "[role='alert']",
                "[role='status']",
                "[role='alertdialog']",
                "[role='dialog'][aria-modal='true']",
                "[class*='MessageBar']",
                "[class*='messageBar']",
                "[class*='banner']",
            ]),
            // Most specific first: the first match is logged. The first two are the
            // exact texts of Outlook's session-expired MessageBar (captured 2026-11).
            patterns: strings(&[
                "sie müssen sich anmelden",
                "sitzung (ist )?abgelaufen",
                "you need to sign in",
                "your session has expired",
                "erneut anmelden",
                "neu anmelden",
                "melden sie sich (erneut|neu) an",
                "anmeldung erforderlich",
                "bestätigen sie ihre identität",
                "identität (erneut )?bestätigen",
                "sitzung.{0,40}(abgelaufen|beendet)",
                "aktion erforderlich",
                "sign in again",
                "sign in to continue",
                "verify your (identity|account)",
                "reauthenticate",
                "session (has )?expired",
                "action required",
            ]),
            exclude_within: strings(&[
                "[role='listbox']",
                "[role='option']",
                "[role='document']",
                "[aria-label*='Nachrichtentext']",
                "[aria-label*='Message body']",
            ]),
            max_text_length: 500,
        }
    }
}

impl Default for ErrorPageConfig {
    fn default() -> Self {
        Self {
            selectors: strings(&[
                "h1",
                "h2",
                "h3",
                "[role='heading']",
                "[role='alert']",
                "[role='alertdialog']",
            ]),
            patterns: strings(&[
                // No generic "keine Verbindung"/"no connection": such words also appear in
                // ordinary titles; a lost connection is reported by `navigator.onLine`.
                "etwas ist schiefgelaufen",
                "something went wrong",
                "sie sind offline",
                "you('| a)re offline",
            ]),
        }
    }
}

/// Subset of the config the injected script needs (served by `pw_get_config`).
#[derive(Debug, Clone, Serialize)]
pub struct ScriptConfig {
    pub app_hosts: Vec<String>,
    pub unread: UnreadConfig,
    pub auth_banner: AuthBannerConfig,
    pub error_page: ErrorPageConfig,
    pub poll_seconds: u64,
    pub notification_show_content: bool,
}

impl Config {
    /// Clamps values to sane ranges so a typo cannot make the app spin or go silent forever.
    pub fn sanitized(mut self) -> Self {
        self.heartbeat_minutes = self.heartbeat_minutes.clamp(2, 120);
        self.auth_reminder_minutes = self.auth_reminder_minutes.min(24 * 60);
        self.grace_seconds = self.grace_seconds.min(300);
        // The page reports at least every 60 s plus one poll interval; keep that well
        // inside the watchdog (heartbeat ≥ 2 min, poll ≤ heartbeat / 4 → gap < heartbeat).
        let max_poll = self.heartbeat_minutes * 60 / 4;
        self.poll_seconds = self.poll_seconds.clamp(5, max_poll.max(5));
        self.auth_banner.max_text_length = self.auth_banner.max_text_length.clamp(50, 5000);
        if url::Url::parse(&self.start_url)
            .map(|u| u.scheme() != "https")
            .unwrap_or(true)
        {
            self.start_url = Config::default().start_url;
        }
        self
    }

    /// `app_hosts` the injected script cannot report from: IPC is granted statically in
    /// `capabilities/remote.json` (and the script's host check), not by this config.
    pub fn hosts_without_ipc(&self) -> Vec<String> {
        self.app_hosts
            .iter()
            .filter(|h| !IPC_HOSTS.contains(&h.to_ascii_lowercase().as_str()))
            .cloned()
            .collect()
    }

    /// Hosts the main frame may show: Outlook, Microsoft sign-in, configured IdPs.
    pub fn main_frame_hosts(&self) -> Vec<String> {
        self.app_hosts
            .iter()
            .chain(&self.login_hosts)
            .chain(&self.idp_hosts)
            .cloned()
            .collect()
    }

    pub fn heartbeat(&self) -> Duration {
        Duration::from_secs(self.heartbeat_minutes * 60)
    }

    pub fn reminder(&self) -> Option<Duration> {
        (self.auth_reminder_minutes > 0)
            .then(|| Duration::from_secs(self.auth_reminder_minutes * 60))
    }

    pub fn script_config(&self) -> ScriptConfig {
        ScriptConfig {
            app_hosts: self.app_hosts.clone(),
            unread: self.unread.clone(),
            auth_banner: self.auth_banner.clone(),
            error_page: self.error_page.clone(),
            poll_seconds: self.poll_seconds,
            notification_show_content: self.notification_show_content,
        }
    }

    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str::<Config>(text).map(Config::sanitized)
    }

    /// Replaces unmodified *former* default lists (written into config.json by older
    /// versions) with the current defaults. Only lists exactly equal to a former
    /// default (same entries, same order) count as unmodified; shortened, extended,
    /// reordered or otherwise edited lists are kept. Returns the JSON paths of the migrated fields.
    pub fn migrate_old_defaults(&mut self) -> Vec<&'static str> {
        let mut migrated = Vec::new();
        let banner = AuthBannerConfig::default();
        if old_banner_pattern_lists()
            .iter()
            .any(|old| same_list(&self.auth_banner.patterns, old))
        {
            self.auth_banner.patterns = banner.patterns;
            migrated.push("auth_banner.patterns");
        }
        if same_list(&self.auth_banner.selectors, OLD_BANNER_SELECTORS) {
            self.auth_banner.selectors = banner.selectors;
            migrated.push("auth_banner.selectors");
        }
        if same_list(&self.error_page.patterns, V1_ERROR_PATTERNS) {
            self.error_page.patterns = ErrorPageConfig::default().patterns;
            migrated.push("error_page.patterns");
        }
        migrated
    }
}

/// Writes the migrated fields back into the user's file: only those fields change,
/// everything else (also fields unknown to this version) stays as it is. Written to a
/// temporary file and renamed, so an interruption never leaves a partial config.
fn write_migrated(path: &Path, text: &str, cfg: &Config, fields: &[&str]) -> std::io::Result<()> {
    let mut json: serde_json::Value = serde_json::from_str(text).map_err(std::io::Error::other)?;
    let current = serde_json::to_value(cfg).map_err(std::io::Error::other)?;
    for field in fields {
        let pointer = format!("/{}", field.replace('.', "/"));
        if let (Some(target), Some(value)) = (json.pointer_mut(&pointer), current.pointer(&pointer))
        {
            *target = value.clone();
        }
    }
    let out = serde_json::to_string_pretty(&json).map_err(std::io::Error::other)? + "\n";
    // A temporary file of our own (unique per process and call, created exclusively),
    // so two instances never write into the same file.
    use std::io::Write;
    let tmp = unique_sibling(path, "tmp");
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(out.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    replace_if_unchanged(path, &tmp, text, &out)
}

/// `config.json.<pid>-<n>.<ext>` next to `path`.
fn unique_sibling(path: &Path, ext: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    path.with_extension(format!(
        "json.{}-{}.{ext}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Puts `tmp` (content `ours`) in place of `path`, but never loses a version of
/// `path` other than the one we read (`read`): the files are exchanged atomically
/// (`renamex_np(RENAME_SWAP)`), and the version taken out is checked afterwards. If
/// someone saved in between, their version is swapped back; anything that would still
/// be displaced is kept as `config.json.<pid>-<n>.kept` (logged) instead of deleted.
#[cfg(target_os = "macos")]
fn replace_if_unchanged(path: &Path, tmp: &Path, read: &str, ours: &str) -> std::io::Result<()> {
    // Cheap early exit; the swap below is what makes it safe.
    if !same_bytes(path, read) {
        let _ = std::fs::remove_file(tmp);
        return Err(std::io::Error::other("config.json changed meanwhile"));
    }
    swap_checked(path, tmp, read, ours)
}

/// Byte comparison; a file that cannot be read (also: another encoding, vanished)
/// counts as different, i.e. as a conflict.
fn same_bytes(p: &Path, expected: &str) -> bool {
    std::fs::read(p).is_ok_and(|b| b == expected.as_bytes())
}

/// The part after the early check (separate so the race can be tested).
#[cfg(target_os = "macos")]
fn swap_checked(path: &Path, tmp: &Path, read: &str, ours: &str) -> std::io::Result<()> {
    if let Err(e) = swap_files(tmp, path) {
        // e.g. a file system without RENAME_SWAP: config.json stays as it is.
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    // `tmp` now holds what was in `path` at the moment of the swap.
    if same_bytes(tmp, read) {
        let _ = std::fs::remove_file(tmp);
        return Ok(());
    }
    // Something else was saved just before the swap (or it cannot be read – treated
    // the same): restore it.
    if let Err(e) = swap_files(tmp, path) {
        crate::log::write(&format!(
            "Konfiguration konnte nicht zurückgetauscht werden; gespeicherte Fassung liegt in {}",
            tmp.display()
        ));
        return Err(e);
    }
    if same_bytes(tmp, ours) {
        let _ = std::fs::remove_file(tmp);
    } else {
        // Yet another save in this instant: keep it next to the config.
        let kept = unique_sibling(path, "kept");
        let place = match std::fs::rename(tmp, &kept) {
            Ok(()) => kept,
            Err(_) => tmp.to_path_buf(),
        };
        crate::log::write(&format!(
            "Gleichzeitig gespeicherte Konfiguration aufbewahrt: {}",
            place.display()
        ));
    }
    Err(std::io::Error::other("config.json changed meanwhile"))
}

#[cfg(not(target_os = "macos"))]
fn replace_if_unchanged(path: &Path, tmp: &Path, read: &str, _ours: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path)? != read {
        let _ = std::fs::remove_file(tmp);
        return Err(std::io::Error::other("config.json changed meanwhile"));
    }
    std::fs::rename(tmp, path)
}

/// Exchanges two files atomically (both names stay, contents swap).
#[cfg(target_os = "macos")]
fn swap_files(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    let b = CString::new(b.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: both arguments are valid NUL-terminated C strings for the duration of
    // the call; `renamex_np` does not keep them.
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Loads the config, creating the file with defaults if it does not exist.
/// A broken file is left untouched (so the user can fix it) and defaults are used.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match Config::parse(&text) {
            Ok(mut cfg) => {
                let migrated = cfg.migrate_old_defaults();
                if !migrated.is_empty() {
                    // Write back so the file shows what is in effect.
                    let saved = write_migrated(path, &text, &cfg, &migrated).is_ok();
                    crate::log::write(&format!(
                        "Alte Standardwerte aktualisiert: {}{}",
                        migrated.join(", "),
                        if saved {
                            ""
                        } else {
                            " (Datei nicht gespeichert)"
                        }
                    ));
                }
                (cfg, None)
            }
            Err(e) => (
                Config::default(),
                Some(format!("config.json ungültig, Standardwerte aktiv: {e}")),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cfg = Config::default();
            let warning = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                .and_then(|_| {
                    let json = serde_json::to_string_pretty(&cfg).expect("config serializes");
                    std::fs::write(path, json + "\n")
                })
                .err()
                .map(|e| format!("config.json konnte nicht angelegt werden: {e}"));
            (cfg, warning)
        }
        Err(e) => (
            Config::default(),
            Some(format!(
                "config.json nicht lesbar, Standardwerte aktiv: {e}"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(Config::parse(&json).unwrap(), cfg);
    }

    #[test]
    fn partial_file_uses_defaults() {
        let cfg = Config::parse(
            r#"{ "heartbeat_minutes": 7, "unread": { "inbox_names": ["Eingang"] } }"#,
        )
        .unwrap();
        assert_eq!(cfg.heartbeat_minutes, 7);
        assert_eq!(cfg.unread.inbox_names, vec!["Eingang"]);
        assert_eq!(cfg.unread.selectors, UnreadConfig::default().selectors);
        assert_eq!(cfg.start_url, Config::default().start_url);
    }

    #[test]
    fn sanitizes_values() {
        let cfg = Config::parse(
            r#"{ "heartbeat_minutes": 0, "poll_seconds": 1, "start_url": "http://evil.example" }"#,
        )
        .unwrap();
        assert_eq!(cfg.heartbeat_minutes, 2);
        assert_eq!(cfg.poll_seconds, 5);
        assert_eq!(cfg.start_url, Config::default().start_url);
        assert_eq!(cfg.reminder(), Some(Duration::from_secs(30 * 60)));
        let off = Config::parse(r#"{ "auth_reminder_minutes": 0 }"#).unwrap();
        assert_eq!(off.reminder(), None);
    }

    #[test]
    fn main_frame_hosts_are_narrow() {
        let cfg = Config::parse(
            r#"{ "allowed_hosts": ["*.microsoft.com"], "idp_hosts": ["adfs.example.org"] }"#,
        )
        .unwrap();
        let hosts = cfg.main_frame_hosts();
        assert!(crate::nav::host_in(&hosts, "outlook.office.com"));
        assert!(crate::nav::host_in(&hosts, "login.microsoftonline.com"));
        assert!(crate::nav::host_in(&hosts, "adfs.example.org"));
        // legacy wide list is ignored; no broad wildcards by default
        assert!(!crate::nav::host_in(&hosts, "www.microsoft.com"));
        assert!(!crate::nav::host_in(&hosts, "evil.sharepoint.com"));
        assert!(!hosts.iter().any(|h| h.starts_with("*.")));
    }

    #[test]
    fn invalid_json_is_error() {
        assert!(Config::parse("{ nope").is_err());
    }

    #[test]
    fn load_creates_file() {
        let dir = std::env::temp_dir().join(format!("pw-cfg-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let _ = std::fs::remove_dir_all(&dir);
        let (cfg, warn) = load(&path);
        assert!(warn.is_none());
        assert_eq!(cfg, Config::default());
        assert!(path.exists());
        std::fs::write(&path, "{ broken").unwrap();
        let (cfg, warn) = load(&path);
        assert!(warn.is_some());
        assert_eq!(cfg, Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod dump {
    use super::*;

    /// `cargo test dump_default_config -- --ignored --nocapture` prints the defaults
    /// (used for the JS fixture tests and documentation).
    #[test]
    #[ignore]
    fn dump_default_config() {
        let cfg = super::Config::default();
        println!(
            "PWCONFIG{}",
            serde_json::to_string(&cfg.script_config()).unwrap()
        );
    }

    #[test]
    fn default_app_hosts_have_ipc() {
        assert!(Config::default().hosts_without_ipc().is_empty());
        let cfg = Config {
            app_hosts: strings(&["outlook.office.de"]),
            ..Config::default()
        };
        assert_eq!(
            cfg.hosts_without_ipc(),
            vec!["outlook.office.de".to_string()]
        );
    }

    #[test]
    fn capability_matches_ipc_hosts() {
        let cap = include_str!("../capabilities/remote.json");
        for host in IPC_HOSTS {
            assert!(cap.contains(&format!("https://{host}/*")), "{host}");
        }
        let js = include_str!("../inject/postwarden.js");
        for host in IPC_HOSTS {
            assert!(js.contains(&host.replace('.', "\\.")), "{host}");
        }
    }

    #[test]
    fn old_default_lists_are_migrated() {
        for old in old_banner_pattern_lists() {
            let mut cfg = Config::default();
            cfg.auth_banner.patterns = strings(&old);
            assert_eq!(cfg.migrate_old_defaults(), vec!["auth_banner.patterns"]);
            assert_eq!(
                cfg.auth_banner.patterns,
                AuthBannerConfig::default().patterns
            );
        }
        let mut cfg = Config::default();
        cfg.auth_banner.selectors = strings(OLD_BANNER_SELECTORS);
        cfg.error_page.patterns = strings(V1_ERROR_PATTERNS);
        assert_eq!(
            cfg.migrate_old_defaults(),
            vec!["auth_banner.selectors", "error_page.patterns"]
        );
        // edited lists are kept: shortened, extended, own entries, reordered
        for edited in [
            vec!["sitzung"],
            V1_PATTERNS[1..].to_vec(),
            V1_PATTERNS
                .iter()
                .chain(["meine meldung"].iter())
                .copied()
                .collect(),
            V1_PATTERNS.iter().rev().copied().collect(),
            V1_PATTERNS[1..]
                .iter()
                .chain(&V1_PATTERNS[..1])
                .copied()
                .collect(),
        ] {
            let mut cfg = Config::default();
            cfg.auth_banner.patterns = strings(&edited);
            assert!(cfg.migrate_old_defaults().is_empty(), "{edited:?}");
            assert_eq!(cfg.auth_banner.patterns, strings(&edited));
        }
        // current defaults stay untouched
        assert!(Config::default().migrate_old_defaults().is_empty());
    }

    #[test]
    fn migration_rewrites_only_migrated_fields() {
        let dir = std::env::temp_dir().join(format!("pw-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let old = serde_json::json!({
            "heartbeat_minutes": 9,
            "future_field": {"keep": true},
            "auth_banner": {"patterns": V1_PATTERNS, "max_text_length": 321}
        });
        std::fs::write(&path, old.to_string()).unwrap();
        let (cfg, warn) = load(&path);
        assert!(warn.is_none());
        assert_eq!(
            cfg.auth_banner.patterns,
            AuthBannerConfig::default().patterns
        );
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["heartbeat_minutes"], 9);
        assert_eq!(saved["future_field"]["keep"], true);
        assert_eq!(saved["auth_banner"]["max_text_length"], 321);
        assert_eq!(
            saved["auth_banner"]["patterns"],
            serde_json::to_value(AuthBannerConfig::default().patterns).unwrap()
        );
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_stays_inside_watchdog() {
        let cfg = Config::parse(r#"{"heartbeat_minutes":1,"poll_seconds":180}"#).unwrap();
        assert_eq!(cfg.heartbeat_minutes, 2);
        assert_eq!(cfg.poll_seconds, 30);
        // worst-case report gap: 60 s heartbeat report + one poll
        assert!(60 + cfg.poll_seconds < cfg.heartbeat_minutes * 60);
        let cfg = Config::parse(r#"{"heartbeat_minutes":120,"poll_seconds":600}"#).unwrap();
        assert!(60 + cfg.poll_seconds < cfg.heartbeat_minutes * 60);
    }

    #[test]
    fn migration_keeps_a_file_saved_after_reading() {
        let dir = std::env::temp_dir().join(format!("pw-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let old = serde_json::json!({"auth_banner": {"patterns": V1_PATTERNS}}).to_string();
        std::fs::write(&path, &old).unwrap();
        let mut cfg = Config::parse(&old).unwrap();
        let fields = cfg.migrate_old_defaults();
        // the user saves a change after we read the file
        std::fs::write(&path, r#"{"heartbeat_minutes": 9}"#).unwrap();
        assert!(write_migrated(&path, &old, &cfg, &fields).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"heartbeat_minutes": 9}"#
        );
        // two writers one after the other: the second sees a newer file and keeps it
        std::fs::write(&path, &old).unwrap();
        assert!(write_migrated(&path, &old, &cfg, &fields).is_ok());
        assert!(write_migrated(&path, &old, &cfg, &fields).is_err());
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_exchanges_contents() {
        let dir = std::env::temp_dir().join(format!("pw-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::write(&a, "A").unwrap();
        std::fs::write(&b, "B").unwrap();
        swap_files(&a, &b).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "A");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn save_just_before_swap_is_restored() {
        // The early check saw the version we read; then the user saved: the swap takes
        // the user's version out, so it must be swapped back and ours discarded.
        let dir = std::env::temp_dir().join(format!("pw-swapback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let tmp = dir.join("ours.tmp");
        std::fs::write(&path, "USER-NEWER").unwrap();
        std::fs::write(&tmp, "OURS").unwrap();
        assert!(swap_checked(&path, &tmp, "OLD", "OURS").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "USER-NEWER");
        assert!(!tmp.exists());
        // unchanged file: ours is put in place
        std::fs::write(&path, "OLD").unwrap();
        std::fs::write(&tmp, "OURS").unwrap();
        assert!(swap_checked(&path, &tmp, "OLD", "OURS").is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "OURS");
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_utf8_save_before_swap_is_restored() {
        let dir = std::env::temp_dir().join(format!("pw-utf16-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let tmp = dir.join("ours.tmp");
        // UTF-16LE with BOM, as some editors save it
        let user: Vec<u8> = [0xFF, 0xFE, b'{', 0, b'}', 0].to_vec();
        std::fs::write(&path, &user).unwrap();
        std::fs::write(&tmp, "OURS").unwrap();
        assert!(swap_checked(&path, &tmp, "OLD", "OURS").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), user);
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

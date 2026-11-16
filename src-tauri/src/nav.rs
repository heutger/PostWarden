// SPDX-License-Identifier: AGPL-3.0-only

//! Navigation whitelist: which URLs stay in the app, which open in the default browser.

use url::Url;

/// `pattern` is either an exact host or `*.domain`, which matches `domain` itself and
/// every subdomain. Comparison is case-insensitive; a trailing dot is ignored.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    match pattern.strip_prefix("*.") {
        Some(domain) => {
            !domain.is_empty()
                && (host == domain
                    || (host.len() > domain.len()
                        && host.ends_with(domain)
                        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'))
        }
        None => host == pattern,
    }
}

pub fn host_in(list: &[String], host: &str) -> bool {
    list.iter().any(|p| host_matches(p, host))
}

/// `https` URL whose host is in `list`.
pub fn https_host_in(list: &[String], url: &Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(|h| host_in(list, h))
}

/// What to do with a URL the page wants to navigate to or open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Load inside PostWarden.
    Allow,
    /// Cancel and hand to macOS (default browser, mail app, …).
    External,
    /// Cancel silently.
    Block,
}

/// Schemes the WebView handles internally (frames, generated content).
fn is_internal_scheme(scheme: &str) -> bool {
    matches!(scheme, "about" | "data" | "blob" | "javascript")
}

/// Schemes worth handing to macOS; everything else (file:, custom app schemes) is blocked.
fn is_external_scheme(scheme: &str) -> bool {
    matches!(scheme, "https" | "http" | "mailto" | "tel" | "msteams")
}

/// Navigation of any frame. WKWebView asks for main frame *and* iframes alike, so any
/// https URL is allowed here and the main frame is checked separately on page load
/// (see [`main_frame_decision`]); otherwise add-in or embedded iframes would pop up in
/// the browser. Plain http never loads inside the app: it is handed to the browser.
pub fn navigation_decision(url: &Url) -> Decision {
    let scheme = url.scheme();
    if scheme == "https" || is_internal_scheme(scheme) {
        Decision::Allow
    } else if is_external_scheme(scheme) {
        Decision::External
    } else {
        Decision::Block
    }
}

/// Main frame (main window or popup) navigates to `url`: decided before WebKit loads
/// it (`NavigationGuard`) and checked again on commit. Only https pages on `main_hosts`
/// (Outlook, Microsoft sign-in, explicitly configured identity providers; for popups
/// also `popup_hosts`) stay in the window; everything else (links with `target=_self`,
/// redirect chains to foreign hosts) opens in the default browser. No wildcard trust is
/// inherited from previous pages. `about:` and `javascript:` are document-internal;
/// `blob:` only if it belongs to an Outlook page; `data:` never as a document.
pub fn main_frame_decision(main_hosts: &[String], app_hosts: &[String], url: &Url) -> Decision {
    match url.scheme() {
        "about" | "javascript" => Decision::Allow,
        "blob" if blob_of(app_hosts, url) => Decision::Allow,
        "blob" | "data" => Decision::Block,
        _ if https_host_in(main_hosts, url) => Decision::Allow,
        s if is_external_scheme(s) => Decision::External,
        _ => Decision::Block,
    }
}

/// `blob:https://host/uuid` created by a page on one of `hosts`.
fn blob_of(hosts: &[String], url: &Url) -> bool {
    url.scheme() == "blob" && Url::parse(url.path()).is_ok_and(|inner| https_host_in(hosts, &inner))
}

/// Source of a download (attachments). Outlook's own data (`blob:` of an Outlook page,
/// `data:` – the page that creates it is Outlook, WebKit only offers it from there) and
/// https on `hosts` stay in PostWarden; other https/http sources go to the browser.
pub fn download_decision(hosts: &[String], app_hosts: &[String], url: &Url) -> Decision {
    match url.scheme() {
        "blob" if blob_of(app_hosts, url) => Decision::Allow,
        "data" => Decision::Allow,
        "https" if https_host_in(hosts, url) => Decision::Allow,
        "https" | "http" => Decision::External,
        _ => Decision::Block,
    }
}

/// Target of `window.open` / `target=_blank`.
pub fn new_window_decision(popup_hosts: &[String], url: &Url) -> Decision {
    if url.scheme() == "about" || https_host_in(popup_hosts, url) {
        Decision::Allow
    } else if is_external_scheme(url.scheme()) {
        Decision::External
    } else {
        Decision::Block
    }
}

/// Scheme and host only – paths of links in mails can carry content or tokens.
pub fn redact(url: &Url) -> String {
    match url.host_str() {
        Some(host) => format!("{}://{}", url.scheme(), host),
        None => url.scheme().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn exact_and_wildcard() {
        assert!(host_matches("outlook.office.com", "outlook.office.com"));
        assert!(host_matches("outlook.office.com", "OUTLOOK.office.com."));
        assert!(!host_matches(
            "outlook.office.com",
            "evil-outlook.office.com"
        ));
        assert!(host_matches("*.office.com", "outlook.office.com"));
        assert!(host_matches("*.office.com", "a.b.office.com"));
        assert!(host_matches("*.office.com", "office.com"));
        assert!(!host_matches("*.office.com", "evil-office.com"));
        assert!(!host_matches("*.office.com", "office.com.evil.net"));
        assert!(!host_matches("*.", "anything"));
        assert!(!host_matches("", "x"));
    }

    #[test]
    fn frame_navigation() {
        assert_eq!(
            navigation_decision(&u("https://example.org/")),
            Decision::Allow
        );
        assert_eq!(
            navigation_decision(&u("http://example.org/")),
            Decision::External
        );
        assert_eq!(navigation_decision(&u("about:blank")), Decision::Allow);
        assert_eq!(navigation_decision(&u("mailto:a@b.de")), Decision::External);
        assert_eq!(
            navigation_decision(&u("file:///etc/passwd")),
            Decision::Block
        );
        assert_eq!(navigation_decision(&u("x-custom://foo")), Decision::Block);
    }

    #[test]
    fn main_frame() {
        let hosts = list(&[
            "outlook.office.com",
            "login.microsoftonline.com",
            "adfs.example.org",
        ]);
        let app = list(&["outlook.office.com"]);
        let d = |s: &str| main_frame_decision(&hosts, &app, &u(s));
        assert_eq!(d("https://outlook.office.com/mail/"), Decision::Allow);
        assert_eq!(
            d("https://login.microsoftonline.com/common"),
            Decision::Allow
        );
        // configured federated IdP
        assert_eq!(d("https://adfs.example.org/adfs/ls"), Decision::Allow);
        // `_self` link in a mail to SharePoint / Office / anything else
        assert_eq!(
            d("https://contoso.sharepoint.com/sites/x"),
            Decision::External
        );
        assert_eq!(d("https://www.office.com/"), Decision::External);
        assert_eq!(d("https://example.org/x"), Decision::External);
        // look-alike hosts
        assert_eq!(
            d("https://login.microsoftonline.com.evil.net/"),
            Decision::External
        );
        assert_eq!(d("https://evil-outlook.office.com/"), Decision::External);
        // http is never shown inside the app, not even for whitelisted hosts
        assert_eq!(d("http://outlook.office.com/"), Decision::External);
        assert_eq!(d("http://login.microsoftonline.com/"), Decision::External);
        assert_eq!(d("about:blank"), Decision::Allow);
        assert_eq!(d("file:///tmp/x"), Decision::Block);
        // generated documents: never data:, blob: only from an Outlook page
        assert_eq!(d("data:text/html,<h1>Login</h1>"), Decision::Block);
        assert_eq!(
            d("blob:https://outlook.office.com/0b5c-11ee"),
            Decision::Allow
        );
        assert_eq!(d("blob:https://evil.example/0b5c-11ee"), Decision::Block);
        assert_eq!(d("blob:null/0b5c"), Decision::Block);
    }

    #[test]
    fn downloads() {
        let app = list(&["outlook.office.com"]);
        let hosts = list(&["outlook.office.com", "attachments.office.net"]);
        let d = |s: &str| download_decision(&hosts, &app, &u(s));
        assert_eq!(d("blob:https://outlook.office.com/123"), Decision::Allow);
        assert_eq!(d("data:application/pdf;base64,AAAA"), Decision::Allow);
        assert_eq!(
            d("https://attachments.office.net/owa/x/file.pdf"),
            Decision::Allow
        );
        assert_eq!(d("https://evil.example/invoice.zip"), Decision::External);
        assert_eq!(d("http://outlook.office.com/x"), Decision::External);
        assert_eq!(d("blob:https://evil.example/123"), Decision::Block);
        assert_eq!(d("file:///etc/passwd"), Decision::Block);
    }

    #[test]
    fn redirect_chain_from_login_does_not_widen_trust() {
        // Each hop is judged on its own: a foreign host after the login page is not
        // allowed just because the previous page was a sign-in page.
        let hosts = list(&["outlook.office.com", "login.microsoftonline.com"]);
        let chain = [
            (
                "https://login.microsoftonline.com/common/oauth2",
                Decision::Allow,
            ),
            ("https://phish.example.net/login", Decision::External),
            ("https://adfs.unconfigured.example/", Decision::External),
            ("https://outlook.office.com/mail/", Decision::Allow),
        ];
        for (url, expected) in chain {
            assert_eq!(
                main_frame_decision(&hosts, &hosts, &u(url)),
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn new_windows() {
        let popup = list(&["outlook.office.com", "*.officeapps.live.com"]);
        assert_eq!(
            new_window_decision(&popup, &u("about:blank")),
            Decision::Allow
        );
        assert_eq!(
            new_window_decision(
                &popup,
                &u("https://outlook.office.com/mail/deeplink/compose")
            ),
            Decision::Allow
        );
        assert_eq!(
            new_window_decision(
                &popup,
                &u("https://eur01.safelinks.protection.outlook.com/?url=x")
            ),
            Decision::External
        );
        assert_eq!(
            new_window_decision(&popup, &u("https://www.microsoft.com/")),
            Decision::External
        );
        assert_eq!(
            new_window_decision(&popup, &u("file:///tmp/x")),
            Decision::Block
        );
    }

    #[test]
    fn redacts_query() {
        assert_eq!(
            redact(&u(
                "https://login.microsoftonline.com/common/oauth2?code=secret#x"
            )),
            "https://login.microsoftonline.com"
        );
        assert_eq!(redact(&u("mailto:a@example.org")), "mailto");
    }
}

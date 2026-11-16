# PostWarden – Architektur & Umsetzungshinweise (Tauri 2)

> API-Namen unten sind nach bestem Wissen angegeben. Vor Verwendung in der aktuellen Tauri-2-Doku / docs.rs verifizieren. Wo Tauri etwas nicht direkt anbietet, über `with_webview` bzw. `objc2` auf AppKit/WebKit zugreifen.

## Überblick

```
┌──────────────────────── PostWarden.app ────────────────────────┐
│  Rust (Tauri core)                                             │
│   ├─ state.rs      Zustandsmaschine (Ok / AuthNeeded / Error / │
│   │                Unknown) + Ungelesen-Zähler                 │
│   ├─ notify.rs     macOS-Mitteilungen, Entprellung, Reminder   │
│   ├─ dock.rs       Badge (Zahl oder "!"), Fenster nach vorne    │
│   ├─ nav.rs        Navigation-Whitelist, externe Links         │
│   ├─ config.rs     config.json laden/anlegen                   │
│   ├─ ratelimit.rs  Rate Limits je IPC-Command                  │
│   └─ commands.rs   IPC-Commands für das Injektionsskript       │
│                          ▲  invoke("pw_report", …)              │
│  WKWebView ──────────────┘                                     │
│   └─ inject/postwarden.js  (initialization script)             │
│        • liest Ungelesen-Zahl, Hinweisbalken, Fehlerbilder     │
│        • ersetzt window.Notification → leitet an Rust weiter   │
│        • Heartbeat                                             │
└────────────────────────────────────────────────────────────────┘
```

Kein Frontend-Bundler. Das Fenster lädt direkt `WebviewUrl::External(start_url)`.

## Wichtige Bausteine

### Fenster & WebView
- `WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))`
  - `.initialization_script(include_str!("../inject/postwarden.js"))` – läuft in jedem Dokument vor Seitenskripten. Konfiguration (Selektoren/Muster) als JSON vorne einsetzen: `format!("window.__PW_CONFIG__ = {};\n{}", cfg_json, script)`.
  - `.on_navigation(|url| …)` – erlaubte Hosts → `true`; sonst `false` und `tauri-plugin-opener` (`open_url`) im Standardbrowser öffnen.
  - Neue Fenster (`window.open`, `target=_blank`): falls Tauri-Version einen New-Window-Handler bietet (`on_new_window` o. ä.), externe Ziele im Browser öffnen, Microsoft-Ziele im selben Fenster oder einem zweiten WebviewWindow. Sonst Fallback im Injektionsskript: `window.open` überschreiben und Klicks auf `a[target=_blank]` abfangen.
  - Downloads: `.on_download(…)` → Ziel `~/Downloads`.
- **Persistenz**: Standard-Datenspeicher der WKWebView verwenden (nicht inkognito). Keinen eigenen Cookie-Code schreiben.
- **User-Agent**: Standard belassen. Nur wenn `config.user_agent` gesetzt ist, `.user_agent(…)` verwenden (Fallback falls Outlook „Browser nicht unterstützt“ meldet; dann typischerweise der UA der installierten Safari-Version).

### Hintergrund nicht einfrieren
Zwei Ebenen, beide nötig:
1. **App Nap aus**: beim Start `NSProcessInfo.processInfo.beginActivityWithOptions(NSActivityUserInitiatedAllowingIdleSystemSleep, reason)` (via `objc2-foundation`) und das zurückgegebene Token für die Laufzeit halten.
2. **WebView-Drosselung aus**: per `webview.with_webview(|wv| …)` an die `WKWebView` kommen und `configuration.preferences.inactiveSchedulingPolicy = .none` setzen (`WKInactiveSchedulingPolicyNone`, macOS 14+). Auf älteren macOS-Versionen überspringen.
3. Fenster beim Schließen nicht zerstören: `on_window_event` → `CloseRequested` → `api.prevent_close(); window.hide()`.
4. Dock-Klick bei verstecktem Fenster: `RunEvent::Reopen` → `window.show(); window.set_focus()`.

Prüfen (TESTING): Läuft der MutationObserver/Heartbeat auch bei verstecktem Fenster über Stunden weiter? Falls nicht: Rust-seitiger Timer, der alle 30 s per `webview.eval("window.__pw && window.__pw.tick()")` einen Check anstößt.

### Dock-Badge
- Tauri 2 bietet auf macOS `Window::set_badge_count(Option<i64>)` und `set_badge_label(Option<String>)` – verifizieren. Fallback: `NSApplication.sharedApplication.dockTile.setBadgeLabel(...)` via `objc2-app-kit`.
- Logik: `AuthNeeded|Error|Unknown` → `"!"`; sonst Zahl > 0 → Zahl; sonst `None`.

### Mitteilungen
- `tauri-plugin-notification`. Berechtigung beim ersten Start anfragen.
- Hinweis: Mitteilungen funktionieren zuverlässig nur aus dem gebündelten `.app` (nicht aus `cargo tauri dev`). Für Tests immer `cargo tauri build` und die App aus `target/release/bundle/macos/` starten.
- Klick auf Mitteilung → Fenster zeigen/fokussieren (falls das Plugin Klick-Events auf macOS nicht liefert: dokumentieren, kein Muss für v1).
- Entprellung in `notify.rs`: gleiche Kategorie nicht öfter als 1× pro 10 s; Auth-Reminder nach `auth_reminder_minutes`.

### IPC (Sicherheit!)
- Die Remote-Seite bekommt per Capability nur Zugriff auf die eigenen Commands:
  ```json
  // src-tauri/capabilities/remote.json
  {
    "identifier": "outlook-remote",
    "windows": ["main"],
    "remote": { "urls": ["https://outlook.office.com/*", "https://outlook.office365.com/*", "https://outlook.cloud.microsoft/*", "https://outlook.live.com/*"] },
    "permissions": ["allow-pw-get-config", "allow-pw-report", "allow-pw-notify", "allow-pw-zoom"]
  }
  ```
  (Eigene Permissions für die App-Commands über `build.rs` / `tauri_build::Attributes` bzw. `permissions/`-Ordner definieren.) **Keine** Core- oder Plugin-Permissions (fs, shell, opener, http …) für die Remote-Seite.
- Commands:
  - `pw_report(state: PageState)` – `{ unread: Option<u32>, inbox_found: bool, auth_banner: Option<String /*nur gematchtes Muster, kein Volltext*/>, error_page: bool, url_host: String }`
  - `pw_notify(kind: "outlook", title: Option<String>, body: Option<String>)` – Inhalt nur verwenden, wenn `notification_show_content`.
- Alle Eingaben aus der Seite als nicht vertrauenswürdig behandeln: Längen begrenzen, validieren.

### Injektionsskript `inject/postwarden.js`
- Läuft nur im Top-Frame (`if (window.top !== window) return;`) und nur auf Outlook-Hosts (auf Login-Hosts nur `url_host` melden).
- `window.Notification` ersetzen: Konstruktor ruft `pw_notify` auf; `Notification.permission = "granted"`; `requestPermission()` → `Promise.resolve("granted")`.
- `detect()`:
  1. Hinweisbalken: alle `auth_banner.selectors` sammeln, sichtbare Elemente (`offsetParent !== null`), `innerText.toLowerCase()` gegen `patterns` (als RegExp) prüfen.
  2. Posteingang: in `unread.selectors` Element finden, dessen `title`/`aria-label`/Text mit einem `inbox_names` beginnt; Zahl per `count_regex` oder aus Kind-Element mit nur Ziffern.
  3. Fallback-Zähler: `document.title` gegen `title_regex`.
  4. Fehlerbild: bekannte Fehlertexte („Etwas ist schiefgelaufen“, „Something went wrong“, „Keine Verbindung“) im Body ohne Ordnerbaum.
- Auslöser: `MutationObserver` (debounced 1 s) auf `document.body` + `setInterval(detect, 30000)` + `visibilitychange`/`online`/`offline`.
- Nur bei Änderung `pw_report` senden (plus alle 60 s als Lebenszeichen).
- `window.__pw = { tick: detect }` für den Rust-Timer.

### Zustandsmaschine (`state.rs`)
```
          report ok + inbox_found
   ┌───────────────────────────────────────┐
   ▼                                       │
  Ok ──auth_banner / login host──▶ AuthNeeded
   │ ──error_page / load error───▶ Error (Auto-Reload mit Backoff)
   │ ──kein inbox_found ≥ N min──▶ Unknown
   └◀──────── jeweils zurück, sobald inbox_found && !auth_banner
```
- Zusätzlich Rust-Watchdog: kommt ≥ `heartbeat_minutes` gar kein `pw_report`, → `Unknown` (Skript tot / Seite hängt) und einmal `reload`.
- Zustandswechsel → Badge + ggf. Mitteilung; Zähleranstieg im Zustand `Ok` → „Neue Mail“-Mitteilung.

### Menüs
Tauri-2-Menü-API (`tauri::menu::{Menu, Submenu, MenuItem, PredefinedMenuItem}`); Edit-Menü mit Predefined-Items, damit ⌘C/⌘V in der WebView funktionieren.

### Autostart
`tauri-plugin-autostart` (LaunchAgent), gesteuert durch `launch_at_login`.

### Bundle
- `tauri.conf.json`: `productName: "PostWarden"`, `identifier: "net.heutger.postwarden"`, `bundle.targets: ["app", "dmg"]`, `macOS.minimumSystemVersion: "13.0"`.
- Eigenes, neutrales Icon (z. B. Briefkasten/Wachturm-Motiv, keine Outlook-Optik). Platzhalter-Icon ist für den Anfang ok.
- Signierung: zunächst ad-hoc (`signingIdentity: "-"`). Notarisierung erst bei öffentlicher Verteilung.

## Projektstruktur (Ziel)

```
PostWarden/
├─ CLAUDE.md
├─ README.md
├─ LICENSE                  (AGPL-3.0)
├─ docs/ SPEC.md ARCHITECTURE.md TESTING.md
└─ src-tauri/
   ├─ Cargo.toml
   ├─ build.rs
   ├─ tauri.conf.json
   ├─ capabilities/ remote.json
   ├─ Info.plist    (LSFileQuarantineEnabled)
   ├─ permissions/ (App-Command-Permissions)
   ├─ icons/
   ├─ inject/postwarden.js
   └─ src/ main.rs lib.rs state.rs notify.rs dock.rs nav.rs config.rs ratelimit.rs i18n.rs macos.rs commands.rs
```

## Meilensteine

1. **Gerüst**: Tauri-2-Projekt, Fenster lädt Outlook, Navigation-Whitelist, externe Links im Browser, Edit-Menü, LICENSE. → Anmeldung testen.
2. **Hintergrund**: Fenster verstecken statt schließen, Reopen, App Nap aus, `inactiveSchedulingPolicy`.
3. **Zähler + Badge**: Injektionsskript (Zähler), `pw_report`, Badge.
4. **Mitteilungen**: neue Mail per Zähler, Notification-Bridge, Entprellung.
5. **Auth-/Fehlererkennung**: Balken, Login-Host, Fehlerseite, Heartbeat, Watchdog, `!`, Reminder, Auto-Reload bei Fehler.
6. **Diagnose & Konfiguration**: config.json, Diagnose-Menü (Snapshot später entfernt, siehe SPEC F6).
7. **Feinschliff**: Autostart, Fensterzustand merken, Icon, README, `cargo tauri build` → `.app`/`.dmg`.

## Umsetzungsstand v0.1 – Abweichungen und Präzisierungen

- **Konfiguration per IPC statt eingebettet**: Das Skript holt Selektoren/Muster über den Command `pw_get_config`. So greift „Konfiguration neu laden“ ohne Neustart (Seite wird neu geladen). Zusätzliche Commands: `pw_zoom` (⌘+ auf Tastaturen mit eigener „+“-Taste; ⌘=, ⌘−, ⌘0 laufen über das Menü). Alle Commands prüfen, dass der Aufrufer das Fenster `main` auf einem `app_hosts`-Host ist.
- **Login-Seiten ohne IPC**: Die Anmeldeseite (Signal B) wird in Rust über `on_page_load` erkannt (Hauptframe-Host ∉ `app_hosts`), nicht vom Skript. Microsoft-Anmeldeseiten bekommen so keinerlei IPC-Zugriff.
- **Navigation**: Tauri/wry geben beim Navigations-Callback nicht weiter, ob Haupt- oder Unterframe navigiert. Deshalb sitzt vor wrys `WKNavigationDelegate` ein eigener Delegate (`macos::NavigationGuard`, Hauptfenster und Popups): Er entscheidet **Hauptframe**-Navigationen (Links, Weiterleitungen, `location`-Wechsel) **vor dem Laden** und reicht alles andere – Unterframes, Downloads, alle übrigen Delegate-Methoden – unverändert an wry weiter (`respondsToSelector:`/`forwardingTargetForSelector:`). Der Hauptframe darf nur https auf `app_hosts`, `login_hosts` und `idp_hosts` zeigen (Popups zusätzlich `popup_hosts`), dazu `about:`/`javascript:` und `blob:` einer Outlook-Seite; `data:` nie als Dokument. Alles andere öffnet im Standardbrowser; ein Popup ohne bisher erlaubte Seite schließt sich. Kommt der Wechsel von einer Anmelde- oder IdP-Seite, gibt es einmal pro Host eine Mitteilung mit dem Hinweis auf `idp_hosts` (Hauptfenster und Popup). Unterframes (Add-ins, Einbettungen) dürfen jedes https laden; http, `mailto:`, `tel:` gehen an macOS. Der Delegate entscheidet außerdem `decidePolicyForNavigationResponse`: Eine Hauptframe-Antwort von einem reinen Download-Host (`download_hosts`, nicht zugleich Dokument-Host) wird als Download gespeichert, wenn sie einer ist (`Content-Disposition: attachment` oder nicht darstellbarer Typ). Eine darstellbare Seite von dort (Fehler-/Statusseite) wird weder angezeigt noch gespeichert, sondern im Standardbrowser geöffnet. Ein leeres Popup schließt sich automatisch nur im eindeutigen Fall: genau ein Download, nach dessen Ende oder Ablehnung (`PopupDownloads`, mit Tests). Ein zweiter Download oder eine angezeigte Seite lässt es dauerhaft offen, weil Tauri Download-Ereignisse nur dem Fenster, nicht dem einzelnen Download zuordnet. Der Delegate gehört dem Fenster (Tabelle auf dem Main-Thread) und wird bei `WindowEvent::Destroyed` freigegeben. **Fail closed:** Das Hauptfenster startet leer und lädt Outlook erst, wenn der Delegate installiert ist; sonst Mitteilung „Navigationsschutz nicht verfügbar“. Ein Popup ohne Delegate wird nicht geöffnet. Als Rückfallebene prüft `on_page_load` den Hauptframe nach dem Commit noch einmal: Dann wird `start_url` geladen, bei wiederholter Weiterleitung (10-s-Sperre) eine leere Seite – nie bleibt ein fremdes Dokument stehen, und es entsteht kein falscher Anmeldealarm (der Watchdog meldet „Status unklar“). Übergaben an Browser/andere Apps: gleiche URL höchstens alle 3 s, insgesamt höchstens 5 pro 30 s. Im Protokoll stehen nur Schema und Host, nie Pfad, Query oder Fehlertexte des Openers.
- **Neue Fenster**: `on_new_window` – Ziele in `popup_hosts` (Outlook, Login, Office-Viewer) und `about:blank` öffnen in einem eigenen Tauri-Fenster (`popup-N`, `NewWindowResponse::Create`), alles andere (Links in Mails, auch zu microsoft.com) im Standardbrowser. WebKit verlangt für Popups die Konfiguration des Öffners, Skripte und IPC-Kanal sind also geteilt; anders als ein nacktes WebKit-Popup (`Allow`) bekommt das Popup aber PostWardens Navigationsregeln: Sein Hauptframe darf nur `app_hosts`, `login_hosts`, `idp_hosts` und `popup_hosts` zeigen, sonst öffnet das Ziel im Browser und das Popup schließt sich. Popups aus Popups werden nicht geöffnet. Bekannte Grenze: wry behandelt `window.close()` nicht, Popups schließt der Nutzer selbst.
- **Überwachung nur im Hauptfenster**: Das Skript macht vor `__pw.arm()` (Rust ruft es im Hauptfenster nach `PageLoadEvent::Finished` und mit jedem `__pw.tick()` auf) **keinen** IPC-Aufruf: keine Konfiguration, keine Meldung, keine Mitteilungs- oder Zoom-Weiterleitung. Pop-outs (auch ohne `window.opener`) bleiben so stumm. Jeder Konfigurationsabruf hat eine Frist von 10 s; späte Antworten überholter Versuche werden ignoriert. Scheitert das Laden (6 Versuche), startet ein späterer Tick höchstens einmal pro Minute einen neuen Versuch. Grenze: Eine IPC-Isolation gegenüber Outlook-Skripten gleicher Herkunft ist das nicht (PW-004). Auf Anmelde- und IdP-Seiten greift das Skript gar nicht ein (Host-Prüfung vor dem Notification-Patch).
- **Neu laden** (Menü, Konfiguration, automatisch): auf einer Outlook-Seite ein echtes `reload()`, sonst – Anmeldeseite oder gar kein geladenes Dokument (z. B. kein Netz beim Anmelden am Mac; `reload()` täte dann nichts) – wird `start_url` geladen. Ein Report zwischen zwei Reloads (Ladebildschirm) hält den Backoff nicht an: Solange derselbe Fehler besteht, wird der nächste Reload neu geplant.
- **Unbekannter Anmelde-Host** (föderierte Anmeldung ohne `idp_hosts`-Eintrag): Protokolleintrag und einmal pro Host eine Mitteilung mit dem Hinweis auf `idp_hosts`.
- **Fensterzustand** (Größe/Position) wird nur für das Hauptfenster gespeichert, nicht für Popups.
- **Opener-Plugin** ohne JS-Linkabfang (`open_js_links_on_click(false)`): Sein Skript liefe sonst in den Microsoft-Seiten und versuchte nicht freigegebene IPC. Das Notification-Plugin injiziert weiterhin ein kleines Skript (Berechtigungsabfrage, wird abgelehnt); PostWardens eigene `Notification`-Brücke ersetzt es auf Outlook-Seiten.
- **Zusätzliche Konfigurationsfelder**: `app_hosts`, `idp_hosts`, `popup_hosts`, `auth_banner.exclude_within`, `auth_banner.max_text_length`, `error_page.patterns`, `grace_seconds` (Standard 10), `poll_seconds` (Standard 30).
- **Zustandsmaschine** (`state.rs`, Unit-getestet):
  - Neuer Anfangszustand `Starting` (kein Badge, keine Mitteilung beim Start; der erste Zählerstand löst keine „Neue Mail“ aus).
  - Problemzustände A/B/C müssen `grace_seconds` anhalten (filtert die SSO-Weiterleitung über die Anmeldeseite beim Start und kurzes Flackern). Rückkehr zu Ok ist sofort.
  - `offline` (navigator.onLine) → Error **ohne** Auto-Reload (Outlook erholt sich selbst; ein Reload offline würde die Seite durch eine leere Fehlerseite ersetzen). Fehlerseite und Watchdog („kein Report ≥ heartbeat“) → Reload mit Backoff 1/2/5/10/10… min statt nur einmal.
  - „Posteingang nicht sichtbar“ zählt in jeder Ansicht: auch im Kalender oder in den Einstellungen sieht PostWarden keine neuen Mails, daher nach `heartbeat_minutes` „Status unklar“ (geändert 2026-11-15; vorher galt jede Nicht-Mail-Ansicht dauerhaft als gesund). Kein Reload und keine „Wieder verbunden“-Meldung danach.
  - **Anmeldung vor dem Commit**: Lässt der Delegate den Hauptframe zu einer Anmelde- oder IdP-Seite navigieren, wird ein geplanter Reload sofort gestrichen und bis zum Commit (höchstens 60 s) keiner ausgelöst. Der automatische Reload selbst prüft und lädt in **einer** Operation auf dem Main-Thread, auf dem auch WebKit Navigationen startet.
  - **Reload-Berechtigung**: Ein automatischer Reload läuft nur, wenn die *aktuelle* Beobachtung noch genau der Grund ist, der ihn geplant hat, und kein anderer Grund in der Karenz steht (`Machine::reload_allowed`); `apply()` prüft das unmittelbar vor der Ausführung erneut. Anmeldeseite, Hinweisbalken oder Offline sperren einen geplanten Reload sofort; Offline hat Vorrang vor einer Fehlerseite. Ein Report ohne Fehlerseite hebt den Termin auf; besteht derselbe Fehler danach weiter, wird mit beibehaltenem Versuchszähler neu geplant.
  - **Watchdog vs. Polling**: `heartbeat_minutes` ≥ 2 und `poll_seconds` ≤ heartbeat / 4, sodass die längste Meldelücke (60 s Lebenszeichen + ein Poll) immer unter der Watchdog-Schwelle bleibt.
  - Während eines Problemzustands wird der Zähler nicht fortgeschrieben; nach der Erholung werden inzwischen eingetroffene Mails als „n neue Mails“ gemeldet.
- **Lebenszeichen** ist ausschließlich ein *sichtbares* Posteingangselement. Der Seitentitel (`(n) …`) und versteckte Ordnerbäume liefern höchstens den Zähler, nie ein Lebenszeichen. Folge: Ist die Ordnerliste eingeklappt, folgt die Zahl weiter dem Titel, nach `heartbeat_minutes` meldet PostWarden aber „Status unklar“.
- **Fehlerseite** wird geprüft, sobald kein sichtbarer Posteingang da ist – unabhängig vom Titel-Zähler und auch bei generischem `<main>`. Sie gilt nur, wenn kein Ordnerbaum sichtbar ist, der sichtbare Text höchstens 1500 Zeichen lang ist und der Fehlertext in einem Meldungselement steht (`error_page.selectors`: Überschriften, `alert`). Kalender- oder Aufgabentexte mit „Keine Verbindung“ lösen so nichts aus; eine funktionierende Nicht-Mail-Ansicht läuft in die passive „Status unklar“-Frist. Gelesen wird nur sichtbarer Text außerhalb fester Mailbereiche (Nachrichtenliste, Lesebereich, Verfassen); Fehlertexte in Mails oder versteckten Vorlagen lösen nichts aus.
- **Feste Mailbereiche** (nicht abschaltbar, im Skript): `[role=listbox]`, `[role=option]`, `[role=document]`, `contenteditable`, `textarea` sowie aria-labels für Nachrichtentext/-liste, Lesebereich und Verfassen. Sie gelten für Hinweisbalken und Fehlertexte; `auth_banner.exclude_within` ergänzt sie nur.
- **Hinweisbalken**: Elemente unter 4×4 px (Screenreader-Live-Regionen) und Elemente in oder mit Mailbereichen zählen nicht. Auch modale Dialoge (`alertdialog`, `dialog[aria-modal]`) werden geprüft. Ein Balken oder Dialog mit unbekanntem Wortlaut bleibt unerkannt, bis sein Text als Muster ergänzt ist. Der echte Outlook-Balken (2026-11) ist eine Fluent-UI-`MessageBar` (`role=group`, Klasse `fui-MessageBar`) mit „Sie müssen sich anmelden. Ihre Sitzung ist abgelaufen. …“ und wird über `[class*='MessageBar']` gefunden. Muster stehen spezifisch vor allgemein, weil das erste Treffer-Muster protokolliert wird.
- **Diagnose-Snapshot (entfernt 2026-11-15)**: war festes Schema ohne HTML (Rust: `diag::Snapshot`, `deny_unknown_fields`), Text nur von sichtbaren Kandidaten (≤ 300 Zeichen), E-Mail-Adressen maskiert, Anzahl/Länge aller Felder in Rust begrenzt. Das Skript sendet den Snapshot als Roh-Bytes; Rust prüft die Größe (≤ 256 KB) vor dem Parsen. Parsen, Schreiben und Finder-Aufruf laufen asynchron in `spawn_blocking`. Restrisiko: Ein gerade sichtbarer Outlook-Toast außerhalb der Mailbereiche kann Absender/Betreff enthalten – Datei vor Weitergabe prüfen.
- **Vertrauensgrenze**: Das Überwachungsskript läuft in derselben JavaScript-Welt wie Outlook. Ein kompromittiertes Outlook-Skript könnte die PostWarden-Commands also ebenfalls aufrufen. Eine isolierte `WKContentWorld` ist mit Tauri-IPC nicht direkt verfügbar und wurde deshalb nicht umgesetzt. Begrenzt wird stattdessen: Rate Limits je Command (`ratelimit.rs`), Hinweisbalken-Muster müssen exakt einem konfigurierten Muster entsprechen, unmögliche Feldkombinationen werden abgelehnt, Reloads nur mit Backoff, keine Commands mit Datei-, Netz- oder Shell-Zugriff.
- **Mitteilungen**: Entprellt wird nur das Paar aus Zähleranstieg und Outlook-Webmitteilung (je eine verbraucht die andere binnen 10 s). Zwei Zähleranstiege oder zwei Webmitteilungen werden nie zusammengefasst. Zähler-Mitteilungen insgesamt höchstens 6 pro Minute (das Badge zeigt die Zahl trotzdem). Bekannte Näherung: Gepaart wird allein nach Zeitnähe, weil die Webmitteilung ohne Inhalt nicht zuzuordnen ist. Eine Outlook-Erinnerung kann deshalb die Zähler-Mitteilung einer gleichzeitig eingetroffenen Mail „verbrauchen“ (und umgekehrt); Badge und Zahl stimmen trotzdem. Webmitteilungen ohne Inhalt heißen neutral „Neue Outlook-Mitteilung“ (Outlook meldet auch Erinnerungen). Das Dock-Badge wird auf dem Main-Thread aus dem aktuellen Zustand gelesen, nicht aus dem Effekt: Timer und IPC können sich überholen, die zuletzt ausgeführte Aktualisierung zeigt aber immer den neuesten Stand.
- **CSP**: Tauris CSP gilt nur für lokal ausgelieferte Seiten, nicht für Outlook. Die einzige lokale Seite ist ein nie angezeigter Platzhalter; sie bekommt die restriktive CSP `default-src 'none'`.
- **Klick auf Mitteilung**: `tauri-plugin-notification` liefert auf macOS keine Klick-Events. Ein Klick aktiviert aber die App; PostWarden beobachtet `NSApplicationDidBecomeActiveNotification` und zeigt dann das Fenster.
- **Downloads**: WebKit/wry legt Downloads in `~/Downloads` ab (mit „ (1)“-Suffix bei Namenskonflikten). WebKit entscheidet einen Download vor jeder Navigationsprüfung, daher prüft `download_handler` die Quelle: `blob:` einer Outlook-Seite, `data:`, https auf `app_hosts` und `download_hosts` (Standard `attachments.office.net`) bleiben in PostWarden; andere http(s)-Quellen gehen an den Standardbrowser, alles andere wird abgelehnt. Grenzen (wie im Browser, zur Entscheidung vorgelegt): wry prüft bei einem bereits gestarteten Download nur die ursprüngliche URL, nicht spätere Weiterleitungen; `data:`-Downloads haben keine nachweisbare Herkunft. Jede heruntergeladene Datei bekommt das Quarantäne-Attribut, Gatekeeper prüft sie also wie bei Safari. `Info.plist` setzt `LSFileQuarantineEnabled`, damit Anhänge wie in Safari das Quarantäne-Attribut bekommen (noch mit `xattr -l` an einem echten Download zu prüfen).
- **Sprache**: Menüs und Mitteilungen deutsch; englisch, wenn die erste bevorzugte Sprache in macOS nicht Deutsch ist (`i18n.rs`, `NSLocale.preferredLanguages`). Protokoll immer deutsch. `Info.plist` meldet `de`/`en` als Lokalisierungen.
- **Alte Konfigurationen**: Standardlisten früherer Versionen (Banner-Muster, Banner-Selektoren ohne Dialoge, Fehlermuster mit „keine Verbindung“) werden nur ersetzt, wenn sie **genau** einer früheren Standardliste entsprechen (gleiche Einträge in gleicher Reihenfolge); gekürzte, ergänzte, umsortierte oder sonst geänderte Listen bleiben. Zurückgeschrieben werden nur diese Felder, alle anderen (auch unbekannte) bleiben erhalten. Geschrieben wird in eine eigene, exklusiv angelegte temporäre Datei (Prozess-ID + Zähler). Ersetzt wird per atomarem Tausch (`renamex_np` mit `RENAME_SWAP`); danach wird die herausgetauschte Fassung byteweise geprüft. Weicht sie vom gelesenen Stand ab oder ist sie nicht lesbar (Nutzer oder zweite Instanz hat gespeichert), wird zurückgetauscht. Eine im selben Augenblick erneut gespeicherte Fassung bleibt als `config.json.<pid>-<n>.kept` erhalten (Protokolleintrag). Eine andere als die gelesene Fassung geht so nie verloren. Fehlerhafte Datei beim Start: Standardwerte; beim „Konfiguration neu laden“: bisherige Konfiguration bleibt aktiv.
- **Eingabegrenzen der IPC**: Tauri parst JSON-Argumente vollständig, bevor die Commands ihre Längen prüfen; die Grenzen (Muster ≤ 200 Zeichen und exakt konfiguriert, Mitteilungstexte ≤ 200 Zeichen, Zähler ≤ 1 000 000) wirken danach. Das unbenutzte Feld `url_host` wurde entfernt.
- **Drag & Drop**: Tauris eigene Datei-Drop-Behandlung ist abgeschaltet (`disable_drag_drop_handler`, Hauptfenster und Popups). Sie meldete macOS jeden Drag als angenommen, ohne WebKit zu fragen – Mails in Ordner ziehen und Dateien in eine Mail ziehen funktionierten dann nicht.
- **Autostart** wird nur gesetzt, wenn die App aus einem `.app`-Bundle läuft (nie für `cargo run`/Dev-Builds).
- **Favicon-Fallback** für den Zähler ist nicht umgesetzt (Format unbekannt; bei Bedarf über den Web-Inspector eines Debug-Builds nachrüsten).
- `capabilities/default.json` entfällt – es gibt keine lokale Oberfläche; die einzige Capability ist `outlook-remote`. Ihre Hosts sind fest (`config::IPC_HOSTS`, gleiche Liste im Skript, per Test abgeglichen). Steht in `app_hosts` ein anderer Host (z. B. eine Sovereign Cloud), protokolliert PostWarden eine Warnung: dort gibt es keine Überwachung.

## Wartung: Updates von Tauri und wry

`Cargo.lock` legt alle Versionen fest; ein Build ändert nichts von selbst. Updates nur bewusst (`cargo update`), etwa alle paar Monate oder bei Sicherheitsmeldungen (`cargo audit`). Nach jedem Update von **wry** oder **tauri-runtime-wry** den `NavigationGuard` (`macos.rs`) gegen die neue wry-Quelle prüfen: Welche Navigations-Delegate-Methoden implementiert wry, setzt es den Delegate später neu, läuft `with_webview` auf dem Main-Thread weiterhin inline? Fällt die Installation aus, lädt PostWarden Outlook nicht (fail closed) und meldet „Navigationsschutz nicht verfügbar“ – ein solcher Fehler wird also sichtbar, nicht still.

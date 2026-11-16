# PostWarden – Spezifikation

## 1. Ziel

Ein Postfach, das nur über Outlook im Web erreichbar ist, soll sich wie eine normale Mail-App verhalten: Man sieht am Dock-Icon und in der Mitteilungszentrale, dass Post da ist – und man merkt sofort, wenn die Sitzung eine erneute Anmeldung/2FA braucht.

Nicht-Ziel: ein eigener Mail-Client, Offline-Zugriff, Mehrkonten-Verwaltung (v1), Mailinhalte außerhalb der Webseite.

## 2. Funktionen

### F1 – Outlook-Fenster
- Ein Fenster lädt die Start-URL (Standard `https://outlook.office.com/mail/`, konfigurierbar, z. B. `outlook.office365.com` oder `outlook.live.com`).
- Persistente Cookies/Speicher (wie Safari), damit Anmeldungen so lange halten, wie Microsoft es zulässt.
- Navigation innerhalb erlaubter Microsoft-Domains bleibt in der App; alle anderen Links (z. B. Links in Mails) öffnen im Standardbrowser.
- Fenstergröße/-position wird gemerkt.
- Standard-Tastenkürzel: ⌘R neu laden, ⌘W Fenster ausblenden, ⌘Q beenden, ⌘C/⌘V/⌘A/⌘Z funktionieren, ⌘+/⌘− Zoom.
- Dateidownloads (Anhänge) landen im Downloads-Ordner; Uploads über Dateiauswahl funktionieren.

### F2 – Hintergrundbetrieb
- Fenster schließen (rotes X / ⌘W) **versteckt** nur; die App läuft weiter. Klick aufs Dock-Icon zeigt das Fenster wieder.
- Die Seite darf im Hintergrund nicht eingefroren werden (App Nap / WebView-Drosselung abschalten, siehe ARCHITECTURE).
- Optional: Autostart bei Anmeldung (Einstellung, Standard an).

### F3 – Ungelesen-Zähler am Dock-Icon
- Zahl ungelesener Mails **im Posteingang** als Badge am Dock-Icon. 0 → kein Badge.
- Erkennung über das DOM (Ordnerliste: Posteingang mit Ungelesen-Zahl), mit Rückfallebene Seitentitel (Favicon-Änderung nicht umgesetzt). Selektoren/Muster konfigurierbar.
- Aktualisierung bei DOM-Änderung (MutationObserver) plus Sicherheits-Polling alle 30 s.

### F4 – Mitteilung bei neuer Mail
- Steigt der Ungelesen-Zähler, kommt eine macOS-Mitteilung: „PostWarden – 1 neue Mail“ bzw. „3 neue Mails“.
- Zusätzlich werden Outlooks eigene Browser-Benachrichtigungen (Web Notification API) abgefangen und als macOS-Mitteilung weitergereicht. **Standard: ohne Inhalt** (nur „Neue Mail“). Einstellung „Absender/Betreff in Mitteilungen anzeigen“ (Standard aus).
- Doppelte Mitteilungen vermeiden: Kommt innerhalb von 10 s sowohl Zähler-Anstieg als auch Outlook-Notification, nur eine Mitteilung.
- Klick auf die Mitteilung bringt das Fenster nach vorne.

### F5 – Erkennung „Neu-Authentifizierung nötig“ (wichtigste Fehlerfunktion)
Zustände, die als **Handlungsbedarf** gelten:

| # | Signal | Erkennung |
|---|--------|-----------|
| A | **Hinweisbalken in Outlook** (Hauptfall: Outlook blendet oben einen Balken ein, man muss sich erneut bestätigen, z. B. per Microsoft Authenticator) | MutationObserver auf Balken/Banner-Elemente (`[role=alert]`, `[role=status]`, MessageBar-Komponenten u. ä.); Text gegen konfigurierbare Muster (DE+EN), z. B. „erneut anmelden“, „Anmeldung erforderlich“, „bestätigen“, „Identität“, „Sitzung“, „sign in again“, „verify“, „session expired“, „action required“, „reauthenticate“. **Nicht** an Farbe festmachen. |
| B | Weiterleitung zur Anmeldeseite | Hauptframe-URL auf `login.microsoftonline.com`, `login.live.com`, `login.microsoft.com`, `device.login.microsoftonline.com` o. ä. |
| C | Fehler-/Offline-Seite | Ladefehler der WebView (über den Watchdog „kein Lebenszeichen“; Neuladen lädt dann die Start-URL), HTTP-Fehlerseite, Outlook-Fehlerbildschirm („Etwas ist schiefgelaufen“ / „Something went wrong“). |
| D | Kein Lebenszeichen | Posteingang-Element seit **N Minuten** (Standard 5) nicht auffindbar, obwohl kein anderer Zustand greift. Gilt als „Status unklar“. |

Reaktion:
- Dock-Badge `!` (hat Vorrang vor der Zahl).
- macOS-Mitteilung „PostWarden – Bitte erneut anmelden“ (bzw. „Verbindungsproblem“ / „Status unklar“).
- Erinnerung alle 30 min, solange der Zustand anhält (konfigurierbar, 0 = aus).
- Option „Fenster bei Handlungsbedarf nach vorne holen“ (Standard an).
- Sobald der Posteingang wieder normal erkannt wird: `!` weg, Zähler wieder sichtbar, einmalige Mitteilung „Wieder verbunden“ (abschaltbar).
- Bei C: automatischer Neuladeversuch mit Backoff (1, 2, 5, 10 min). Bei A und B **kein** automatisches Handeln – der Nutzer muss selbst bestätigen.

### F6 – Diagnose (geändert 2026-11-15)
Ursprünglich war ein Menüpunkt „Seitenzustand erfassen“ vorgesehen, der Balken- und Ordnerkandidaten als JSON-Datei speichert. Er wurde in v0.1 umgesetzt, hat den echten Sitzungs-abgelaufen-Balken einmal erfasst (Muster daraufhin ergänzt) und wurde danach auf Nutzerwunsch **entfernt**: Die Datei konnte trotz Ausschlüssen Seiteninhalte enthalten, und der zusätzliche IPC-Command vergrößerte die Angriffsfläche.
Stattdessen:
- Das Protokoll nennt bei einem erkannten Hinweisbalken das **konfigurierte Muster**, das angeschlagen hat (`Status AuthNeeded (banner: sie müssen sich anmelden)`), nie den Balkentext.
- Wer andere Wortlaute (andere Sprache, anderer Mandant) erkennen muss, ergänzt `auth_banner.patterns` in der Konfiguration; zum Nachsehen der Seite dient der Web-Inspector eines Debug-Builds (siehe TESTING 5a).

### F7 – Menüleiste/Menüs
- App-Menü: Über PostWarden, Einstellungen…, Beenden.
- Ansicht: Neu laden, Zoom.
- Diagnose: Konfigurationsdatei öffnen, Konfiguration neu laden, Protokoll anzeigen, Testmitteilung senden.
- Kein Menüleisten-Symbol (Nutzerwunsch). Optional später.

### F8 – Konfiguration
Datei `~/Library/Application Support/PostWarden/config.json`, beim ersten Start mit Standardwerten angelegt. Änderungen werden beim Neustart (oder per „Konfiguration neu laden“) übernommen. Die beim ersten Start geschriebene Datei enthält alle Felder mit ihren Standardwerten (Quelle: `src-tauri/src/config.rs`). Wichtigste Felder (Stand 2026-11-15):

- `start_url`, `app_hosts` (Outlook-Hosts; nur die vier Hosts aus `capabilities/remote.json` bekommen IPC), `login_hosts`, `idp_hosts` (föderierte Anmeldung, z. B. ADFS), `popup_hosts`.
- `unread` (Posteingangsnamen, Selektoren, Regex für Zähler und Titel).
- `auth_banner` (Selektoren, Muster – spezifisch vor allgemein, z. B. „sie müssen sich anmelden“, „sitzung (ist )?abgelaufen“ –, zusätzliche Ausschlussbereiche, Maximallänge).
- `error_page.patterns`.
- `heartbeat_minutes`, `auth_reminder_minutes`, `grace_seconds`, `poll_seconds`, `bring_to_front_on_auth`, `notify_on_reconnect`, `notification_show_content`, `launch_at_login`, `user_agent`.

Das frühere Feld `allowed_hosts` wird ignoriert.

Die Werte für Selektoren/Muster sind **Startwerte** und müssen gegen die echte Seite verifiziert werden (siehe TESTING).

## 3. Datenschutz & Compliance
- Siehe „Harte Regeln“ in `CLAUDE.md`.
- Logs (`~/Library/Logs/PostWarden/`) enthalten nur Zustandswechsel und Zähler, keine Inhalte.
- Keine Netzwerkaufrufe außer denen der WebView.

## 4. Akzeptanzkriterien
1. App startet, zeigt Outlook im Web, Anmeldung inkl. 2FA/Passkey funktioniert.
2. Nach Neustart der App ist man (sofern Microsoft es zulässt) noch angemeldet.
3. Fenster geschlossen, App läuft: eine Testmail an das Postfach erzeugt binnen ≤ 60 s Badge-Zahl und Mitteilung.
4. Mail gelesen → Badge sinkt/verschwindet binnen ≤ 60 s.
5. Hinweisbalken zur Neu-Authentifizierung → `!` + Mitteilung binnen ≤ 60 s (verifiziert, sobald der Balken real auftritt; vorher per Simulation, siehe TESTING).
6. Abmelden in Outlook → Anmeldeseite → `!` + Mitteilung.
7. WLAN aus → nach spätestens Heartbeat-Zeit „Verbindungsproblem/Status unklar“; WLAN an → automatische Erholung, `!` verschwindet.
8. Links in Mails öffnen im Standardbrowser.
9. Über 24 h Laufzeit im Hintergrund kein Einfrieren (Badge reagiert weiterhin).
10. Keine Netzwerkverbindungen außer zu Microsoft-Domains (prüfbar z. B. mit Little Snitch/LuLu).

# PostWarden – Testcheckliste

Immer mit der gebündelten App testen (`cargo tauri build`, dann `src-tauri/target/release/bundle/macos/PostWarden.app` starten), weil Mitteilungen und Badge aus `cargo tauri dev` nicht zuverlässig funktionieren.

## Automatisiert
- `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
- Unit-Tests für: Zustandsmaschine (alle Übergänge), Entprellung/Reminder-Logik, Host-Whitelist (inkl. Wildcards), Validierung der IPC-Eingaben, Config-Defaults und -Parsing.
- Injektionsskript: `python3 inject/test/make_fixture.py` (in `src-tauri/`) erzeugt `inject/test/fixture.html` aus `fixture.template.html` mit den aktuellen Standardwerten und gemockter Tauri-IPC. In Safari öffnen (Fenster mit normaler Größe – die Sichtbarkeitsprüfung braucht Layout) → Titel „ALL PASS“. Geprüft: Zähler (aria-label, Kind-Element, Unterordner ignoriert, 0), Hinweisbalken inkl. Simulationstext aus 5a, Screenreader-Regionen und Nachrichtenliste ignoriert, Re-Auth als modaler Dialog, Verfassen-Dialog ist kein Hinweisbalken, Fehlerseite (auch mit `<main>`) vs. Fehlertext im Lesebereich und versteckte Fehlervorlage, Titel nur Zähler und kein Lebenszeichen, versteckter Ordnerbaum kein Lebenszeichen, Fehlerseite trotz Titel-Zähler, kein IPC vor `arm()` (keine Konfiguration, keine Mitteilungsweiterleitung), Konfiguration nach `arm()`, Posteingang unter transparentem Elternelement kein Lebenszeichen, Fehlerwörter auf langer Seite, neben Ordnerbaum, außerhalb von Überschriften und als Termintitel in einer Überschrift ignoriert, Notification-Bridge ohne Inhalt, echter Sitzungs-abgelaufen-Balken (Fluent-`MessageBar`, Aufbau wie 2026-11 erfasst) mit spezifischem Muster, keine Snapshot-Funktion mehr, Report nur bei Änderung, MutationObserver. Neue Fixtures **ohne** persönliche Daten anlegen.

## Manuell (Akzeptanzkriterien aus SPEC §4)

| # | Test | Erwartung |
|---|------|-----------|
| 1 | Erststart, Anmeldung mit 2FA/Passkey | Outlook lädt, Posteingang sichtbar |
| 2 | App beenden, neu starten | Weiterhin angemeldet (sofern Mandant es erlaubt) |
| 3 | Fenster schließen, Testmail von anderer Adresse senden | ≤ 60 s: Badge-Zahl + Mitteilung |
| 4 | Mail lesen | Badge sinkt / verschwindet |
| 5a | **Simulation Hinweisbalken**: in Web-Inspector (Rechtsklick → Element untersuchen; dafür mit `cargo tauri build --debug` bauen) `document.body.insertAdjacentHTML('afterbegin','<div role="alert">Bitte melden Sie sich erneut an</div>')` | ≤ 60 s: `!` + Mitteilung „Bitte erneut anmelden“; nach Entfernen des Elements wieder Zahl |
| 5b | **Echter Balken** (Sitzung abgelaufen) | `!` + Mitteilung; Protokoll zeigt `Status AuthNeeded (banner: sie müssen sich anmelden)`. Erfolgreich beobachtet am 2026-11-15 |
| 6 | In Outlook abmelden | Anmeldeseite → `!` + Mitteilung |
| 7 | WLAN aus / an | `!` „Verbindungsproblem/Status unklar“ → automatische Erholung, `!` weg, „Wieder verbunden“ |
| 8 | Link in Mail anklicken | Öffnet im Standardbrowser |
| 9 | 24 h im Hintergrund laufen lassen, dann Testmail | Badge + Mitteilung kommen weiterhin |
| 10 | Netzwerkmonitor (LuLu/Little Snitch) | Nur Microsoft-Domains |
| 11 | Anhang herunterladen / hochladen | Download in ~/Downloads, Upload-Dialog funktioniert |
| 12 | ⌘C/⌘V/⌘Z/⌘R/⌘Q | Funktionieren |
| 13 | Protokoll öffnen | Keine Betreffzeilen/Absender/Mailtexte enthalten |
| 14 | „Anmelden“ im Sitzungs-Balken (öffnet Popup) | Anmeldung im PostWarden-Popup klappt; danach `!` weg. Popup ggf. selbst schließen |
| 15 | Link zu fremder Seite im Hauptfenster bzw. in einem Popup | Fremde Seite erscheint **nie** im PostWarden-Fenster, öffnet im Standardbrowser; Protokoll „Ziel nicht freigegeben: https://host“ bzw. „Popup-Ziel …“ |
| 16 | Anhang herunterladen, dann `xattr -l ~/Downloads/<Datei>` | `com.apple.quarantine` vorhanden; Protokoll „Download gestartet“ (nicht „nicht freigegebene Quelle“ – sonst Host in `download_hosts`) |
| 17 | 6 min im Kalender bleiben | „Status unklar“ + `!`; zurück in der Mailansicht verschwindet `!` ohne „Wieder verbunden“ |
| 19 | Mail per Drag & Drop in einen Ordner ziehen; Datei aus dem Finder in eine neue Mail ziehen | Mail wird verschoben; Datei wird angehängt |
| 18 | Ohne Netz starten (WLAN aus), nach 5 min WLAN an | „Status unklar“; spätestens beim nächsten automatischen Neuladen erscheint Outlook |

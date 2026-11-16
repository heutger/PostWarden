# PostWarden – Projektkontext für Claude Code

## Worum es geht

PostWarden ist eine schlanke macOS-App (Tauri 2 + Rust), die **Outlook im Web** (`outlook.office.com`) in einem eigenen Fenster anzeigt – im Kern ein Ein-Seiten-Browser. Der einzige Mehrwert gegenüber Safari/Chrome:

1. **Neue Mails** → Zahl am Dock-Icon + macOS-Mitteilung (Mitteilungszentrale).
2. **Neu-Authentifizierung nötig** (Hinweisbalken in Outlook, Weiterleitung zur Anmeldeseite, Fehlerseite, kein Lebenszeichen) → rotes `!` am Dock-Icon + Mitteilung.

Hintergrund: Für Postfächer, bei denen der Betreiber nur Outlook im Web zulässt – kein Outlook-Desktop, kein Apple Mail/IMAP, keine Weiterleitungen, keine Benachrichtigungen. Mails werden dort leicht tagelang übersehen.

Vollständige Anforderungen: `docs/SPEC.md`. Technische Umsetzung: `docs/ARCHITECTURE.md`. Tests: `docs/TESTING.md`.

## Harte Regeln (nicht verhandelbar)

PostWarden muss sich gegenüber Microsoft und dem Mandanten exakt wie ein normaler Browser verhalten. Deshalb:

- **Keine Microsoft-APIs**: kein Microsoft Graph, kein EWS, kein IMAP/POP/SMTP, keine App-Registrierung in Entra ID, keine eigene OAuth-Client-ID.
- **Keine Tokens/Cookies auslesen** oder außerhalb der WebView speichern/weiterverwenden.
- **Keine automatisierten Aktionen** in Outlook: nichts anklicken, nichts abrufen, nichts verschieben, kein Auto-Login, keine Passwortspeicherung durch PostWarden.
- **Nur lesen, was sichtbar ist**: Das injizierte Skript liest ausschließlich Zähler, Titel und Hinweisbalken aus dem DOM. Keine Mailinhalte, Betreffzeilen oder Absender in Logs, Mitteilungen oder Diagnosedateien – außer der Nutzer aktiviert das ausdrücklich (siehe SPEC, Datenschutz).
- **Keine Telemetrie, keine eigenen Netzwerkaufrufe.** Die einzigen Netzwerkverbindungen macht die WebView zu Microsoft.
- **Kein User-Agent-Spoofing als Standard.** Nur falls Outlook die WebView ablehnt, darf der UA konfigurierbar an Safari angeglichen werden (siehe ARCHITECTURE).
- **Marken**: „Outlook“/„Microsoft“ nur beschreibend verwenden („ein Wrapper für Outlook im Web“). Kein Microsoft-Logo, kein Outlook-ähnliches Icon.
- **Tauri-IPC minimal**: Die Remote-Seite bekommt ausschließlich die eigenen PostWarden-Commands freigegeben (keine fs-, shell-, http- oder sonstigen Plugin-Rechte).

## Lizenz

AGPL-3.0-only. Erste Aufgabe: offiziellen Lizenztext von https://www.gnu.org/licenses/agpl-3.0.txt als `LICENSE` ablegen. In `Cargo.toml` `license = "AGPL-3.0-only"`, SPDX-Header `// SPDX-License-Identifier: AGPL-3.0-only` in jede Quelldatei.

## Schreibweise

Projektname: **PostWarden** (Binärname/Bundle: `PostWarden`, Crate: `postwarden`, Bundle-ID: `net.heutger.postwarden`).

## Umgebung

- macOS (Apple Silicon), Xcode Command Line Tools und Rust-Toolchain sind installiert.
- Tauri CLI 2.x (`cargo install tauri-cli --version "^2"` falls nicht vorhanden).
- Kein Node/Frontend-Build nötig: Die App lädt eine externe URL; Einstellungs-/Diagnose-UI (falls überhaupt) als statische HTML-Datei ohne Bundler.

## Arbeitsweise

- Zuerst `docs/SPEC.md` und `docs/ARCHITECTURE.md` vollständig lesen.
- In kleinen, testbaren Schritten arbeiten (Reihenfolge: ARCHITECTURE → „Meilensteine“).
- Bei unsicheren Tauri-API-Namen in der aktuellen Tauri-2-Doku bzw. docs.rs prüfen statt raten.
- Die DOM-Erkennung (Ungelesen-Zähler, Hinweisbalken) ist der fragilste Teil: Selektoren und Textmuster gehören in die Konfigurationsdatei, nicht hart in den Code.
- Nach jedem Meilenstein: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo tauri build` und manuellen Test laut `docs/TESTING.md`.
- Commits auf Englisch, Doku/Kommentare dürfen Deutsch oder Englisch sein; README öffentlich auf Englisch.

# PostWarden

A tiny macOS app (Tauri 2 / Rust) that wraps **Outlook on the web** in its own window and adds what the browser tab is missing:

- **Unread badge** on the Dock icon and **macOS notifications** for new mail.
- **Re-authentication alerts**: a red `!` badge and a notification when Outlook asks you to sign in again or confirm your identity (MFA), or when the session/connection breaks.

Built for mailboxes where the provider only allows web access – no Outlook desktop, no IMAP, no forwarding, no notifications.

## What it does *not* do

PostWarden behaves like a normal browser. It does **not** use Microsoft Graph, EWS or IMAP, does not register an app with Entra ID, does not read tokens or cookies, and does not automate anything inside Outlook. It only reads what is already visible on the page (unread count, banners) and passes that on to macOS. No telemetry, no network calls of its own.

## Status

Version 0.1 – all milestones implemented. Unread counter, new-mail notifications, offline detection and the real "session expired" banner have been verified against Outlook on the web (German UI, November 2026). See `docs/SPEC.md`, `docs/ARCHITECTURE.md` and `docs/TESTING.md`.

**Apple Silicon only** (M1 or newer). The released build is `aarch64`; Intel Macs are not supported.

**Two-stage code review.** The code was written with an AI coding assistant (Claude Code) and then reviewed in two independent stages before release: an internal review by a second Claude model and an external review by OpenAI Codex. Every finding was checked against the code before it was fixed or rejected, and the reviews were repeated until neither stage reported a high-severity (P0/P1) issue. Remaining lower-priority findings are tracked as GitHub issues.

## Install

Download the DMG from the [Releases](../../releases) page, open it and drag PostWarden to Applications. The release build is signed with a Developer ID and notarized by Apple, so macOS only asks once whether to open an app downloaded from the internet.

## Build

Requires macOS 13+, Xcode Command Line Tools and Rust 1.90 or newer (the locked Tauri version needs it).

```sh
cargo install tauri-cli --version "^2"
cd src-tauri
cargo tauri build
open target/release/bundle/macos/PostWarden.app
```

Your own build is signed ad hoc. macOS may ask you to confirm opening it (right-click → Open). Signed and notarized releases are built as described in `docs/RELEASING.md`.

## Usage

- Closing the window (red button / ⌘W) only hides it; PostWarden keeps watching. Click the Dock icon to bring it back, ⌘Q quits.
- Dock badge: number of unread mails in the inbox, or a red `!` when you need to sign in again, the connection is broken or the inbox is no longer recognized.
- When a re-authentication banner is detected, the log names the configured pattern that matched (never the banner text). If your tenant or language uses a different wording, add it to `auth_banner.patterns` in the config.
- The interface is German; menus and notifications switch to English when the first preferred language in macOS is not German. The log is always German.
- **Diagnose → Testmitteilung senden** (*Diagnostics → Send Test Notification*) checks that macOS lets PostWarden show notifications (System Settings → Notifications → PostWarden: allow notifications and badges).
- Settings live in `~/Library/Application Support/PostWarden/config.json` (created on first launch; unchanged default lists from older versions are updated automatically, and deleting the file restores all defaults; **Diagnose → Konfiguration neu laden** applies changes). Log: `~/Library/Logs/PostWarden/postwarden.log` (state changes and counters only).

## Development

```sh
cd src-tauri
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
python3 inject/test/make_fixture.py   # then open inject/test/fixture.html in Safari → "ALL PASS"
```

## Trademarks

Microsoft and Outlook are trademarks of the Microsoft group of companies. PostWarden is an independent project and is not affiliated with or endorsed by Microsoft.

## License

Copyright © 2026 Christian Heutger. Licensed under [AGPL-3.0-only](LICENSE).

# Releasing (signed and notarized)

Without signing, `cargo tauri build` uses an ad-hoc signature (`signingIdentity: "-"` in `tauri.conf.json`), so anyone can build the project without an Apple certificate. A release build for distribution is signed with a Developer ID and notarized by Apple.

## One-time setup

1. Get a **Developer ID Application** certificate. Apple Developer Program → Certificates → "+" → Developer ID Application, using a CSR from Keychain Access. Then install the downloaded `.cer` file. Check it with:
   ```bash
   security find-identity -v -p codesigning
   ```
2. Store the credentials for the Apple notary service in the keychain. Use an app-specific password from account.apple.com; the command asks for it:
   ```bash
   xcrun notarytool store-credentials postwarden-notary --apple-id "APPLE_ID" --team-id "TEAM_ID"
   ```

## Build, notarize and staple

```bash
cd src-tauri
APPLE_SIGNING_IDENTITY="Developer ID Application: NAME (TEAM_ID)" cargo tauri build
cd target/release/bundle
xcrun notarytool submit dmg/PostWarden_*_aarch64.dmg --keychain-profile postwarden-notary --wait
xcrun stapler staple dmg/PostWarden_*_aarch64.dmg
xcrun stapler staple macos/PostWarden.app
```

Tauri skips its own notarization step because no `APPLE_ID`/`APPLE_API_KEY` variables are set. That is intended: the keychain profile keeps the credentials out of the environment.

## Checks

```bash
codesign --verify --deep --strict --verbose=2 macos/PostWarden.app
codesign -dv --verbose=4 macos/PostWarden.app   # flags=0x10000(runtime), Timestamp, TeamIdentifier
spctl --assess --type execute --verbose macos/PostWarden.app   # source=Notarized Developer ID
spctl --assess --type open --context context:primary-signature --verbose dmg/PostWarden_*_aarch64.dmg
```

The app needs no entitlements. Hardened runtime is enabled by Tauri by default.

## When credentials expire

- The `postwarden-notary` keychain profile never changes on its own. Re-run `store-credentials` with a new app-specific password if notarization fails with HTTP 401. That happens after the Apple ID password changed (which revokes all app-specific passwords), after the app-specific password was revoked, or after the membership lapsed.
- The Developer ID Application certificate is valid for 5 years. Apps that are already signed and notarized keep working after it expires, because the signature carries a secure timestamp.
- New builds can only be notarized while the Apple Developer Program membership (yearly) is active.

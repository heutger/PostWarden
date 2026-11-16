// SPDX-License-Identifier: AGPL-3.0-only

fn main() {
    // Generates the permissions `allow-pw-report`, `allow-pw-notify`, … for our own
    // commands. Only these are granted to the remote Outlook page (capabilities/remote.json).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "pw_get_config",
            "pw_report",
            "pw_notify",
            "pw_zoom",
        ]),
    ))
    .expect("failed to run tauri-build");
}

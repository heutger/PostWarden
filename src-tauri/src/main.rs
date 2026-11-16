// SPDX-License-Identifier: AGPL-3.0-only

// Prevents an additional console window on Windows in release builds (irrelevant on macOS).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    postwarden_lib::run();
}

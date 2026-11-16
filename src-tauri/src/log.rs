// SPDX-License-Identifier: AGPL-3.0-only

//! Minimal log in `~/Library/Logs/PostWarden/postwarden.log`.
//! Only state changes, counters and hosts – never mail content, subjects or senders.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_BYTES: u64 = 1024 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

pub fn init(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let _ = PATH.set(dir.join("postwarden.log"));
}

pub fn write(line: &str) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    #[cfg(debug_assertions)]
    eprintln!("[{stamp}] {line}");
    let Some(path) = PATH.get() else { return };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{stamp}] {line}");
    }
}

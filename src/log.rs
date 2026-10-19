//! Tiny append-only log in the config directory, so a user can send something when it fails.
//! ponytail: no log crate, no levels; one line per event, file capped at 1 MB by truncation.
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("OpenNC")
}

pub fn log_path() -> PathBuf {
    config_dir().join("opennc.log")
}

/// "2026-10-20 14:03:07" from the system clock, UTC (no timezone crate).
fn timestamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil-from-days (Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn log(line: impl AsRef<str>) {
    let mut guard = FILE.lock().unwrap();
    if guard.is_none() {
        let p = log_path();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if std::fs::metadata(&p).map(|m| m.len() > 1_000_000).unwrap_or(false) {
            let _ = std::fs::remove_file(&p);
        }
        *guard = std::fs::OpenOptions::new().create(true).append(true).open(&p).ok();
    }
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "{}  {}", timestamp(), line.as_ref());
    }
    eprintln!("{}", line.as_ref());
}

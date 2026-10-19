//! "Start with Windows": a value in HKCU\...\Run pointing at this executable with --minimized.
//! Other platforms: not implemented yet (returns false / does nothing).

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "OpenNC";

fn command() -> String {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    format!("\"{exe}\" --minimized")
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(VALUE))
        .map(|v| v == command())
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn set_enabled(on: bool) -> std::io::Result<()> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)?;
    if on {
        key.set_value(VALUE, &command())
    } else {
        match key.delete_value(VALUE) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_enabled(_on: bool) -> std::io::Result<()> {
    Ok(())
}

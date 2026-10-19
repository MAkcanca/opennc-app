//! Automatic updates from GitHub releases of MAkcanca/opennc-app. Checks the latest release, downloads the Windows
//! installer, verifies it against the release's SHA256SUMS, and launches it. The installer closes the running app.
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub const REPO: &str = "MAkcanca/opennc-app";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    pub installer_url: String,
    pub sums_url: String,
    pub installer_name: String,
}

fn agent() -> ureq::Agent {
    use ureq::tls::{TlsConfig, TlsProvider};
    ureq::Agent::config_builder()
        .user_agent(format!("OpenNC/{CURRENT}"))
        .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).build()) // the OS TLS stack, no bundled crypto
        .build()
        .into()
}

/// The newest release if it is newer than this build; `None` when up to date. Network errors are returned as Err.
pub fn check() -> Result<Option<Update>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body: serde_json::Value = agent().get(&url).call().context("GitHub releases request")?.body_mut().read_json()?;
    let tag = body["tag_name"].as_str().ok_or_else(|| anyhow!("no tag in release"))?;
    let latest = semver::Version::parse(tag.trim_start_matches('v')).context("release tag is not a version")?;
    let current = semver::Version::parse(CURRENT)?;
    if latest <= current {
        return Ok(None);
    }
    let assets = body["assets"].as_array().ok_or_else(|| anyhow!("no assets"))?;
    let find = |pred: &dyn Fn(&str) -> bool| {
        assets.iter().find(|a| a["name"].as_str().map(pred).unwrap_or(false)).and_then(|a| {
            Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str()?.to_string()))
        })
    };
    let Some((installer_name, installer_url)) = find(&|n| n.ends_with("-Setup.exe")) else { return Ok(None) };
    let Some((_, sums_url)) = find(&|n| n == "SHA256SUMS") else { return Ok(None) };
    Ok(Some(Update { version: latest.to_string(), installer_url, sums_url, installer_name }))
}

/// Download, verify and launch the installer. Returns the installer path once it has been started.
pub fn install(u: &Update) -> Result<PathBuf> {
    let agent = agent();
    let sums = agent.get(&u.sums_url).call()?.body_mut().read_to_string()?;
    let expected = sums
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.trim_start_matches('*').to_string()))
        })
        .find(|(_, name)| *name == u.installer_name)
        .map(|(h, _)| h)
        .ok_or_else(|| anyhow!("SHA256SUMS has no entry for {}", u.installer_name))?;
    let bytes = agent.get(&u.installer_url).call()?.body_mut().with_config().limit(200 * 1024 * 1024).read_to_vec()?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected.to_lowercase() {
        bail!("installer checksum mismatch (expected {expected}, got {actual})");
    }
    let path = std::env::temp_dir().join(&u.installer_name);
    std::fs::write(&path, &bytes)?;
    #[cfg(windows)]
    std::process::Command::new(&path).args(["/SILENT", "/CLOSEAPPLICATIONS", "/RESTARTAPPLICATIONS"]).spawn().context("launching the installer")?;
    #[cfg(not(windows))]
    bail!("automatic install is Windows-only for now; open {}", path.display());
    Ok(path)
}

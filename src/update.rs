//! Looks for a newer Pollen on GitHub once a day (unless turned off in
//! Settings), and installs it when asked: the new Pollen.exe is downloaded to
//! the temp folder and run with `--install`, which replaces this one.
//!
//! The only thing sent is the request for the latest release's description
//! (api.github.com), with "Pollen" as the user agent.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::install::VERSION;

const LATEST: &str = "https://api.github.com/repos/bekirevrimsumer/pollen/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub url: String,
}

static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);

/// A newer version found by the last check.
pub fn available() -> Option<Release> {
    AVAILABLE.lock().ok().and_then(|a| a.clone())
}

fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)))
}

/// The latest release, if it is newer than this program.
pub fn check() -> Option<Release> {
    let j = crate::net::get_json(LATEST)?;
    let tag = j["tag_name"].as_str()?;
    if parse(tag)? <= parse(VERSION)? {
        return None;
    }
    let url = j["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("Pollen.exe")))?["browser_download_url"]
        .as_str()?
        .to_string();
    Some(Release { version: tag.trim_start_matches('v').to_string(), url })
}

/// Checks a minute after start, then every day; `notify` gets `message` when
/// a newer version turns up. Only the installed copy looks.
pub fn start(notify: HWND, message: u32) {
    if !crate::install::is_installed_copy() {
        return;
    }
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-update".into())
        .spawn(move || {
            // Downloads of earlier updates, now installed.
            if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with("Pollen-") && name.ends_with(".exe") {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(60));
            loop {
                if crate::settings::get().check_updates {
                    if let Some(release) = check() {
                        let new = available().map_or(true, |a| a.version != release.version);
                        if let Ok(mut a) = AVAILABLE.lock() {
                            *a = Some(release);
                        }
                        if new {
                            unsafe {
                                let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(24 * 60 * 60));
            }
        })
        .ok();
}

/// Downloads the newer version and lets it install itself over this one
/// (it closes this Pollen first). Runs on a worker thread.
pub fn install_available() {
    let Some(release) = available() else { return };
    std::thread::spawn(move || {
        let Some(body) = crate::net::get(&release.url, 64 * 1024 * 1024) else { return };
        // A Windows program starts with "MZ".
        if body.len() < 100_000 || !body.starts_with(b"MZ") {
            return;
        }
        let path: PathBuf = std::env::temp_dir().join(format!("Pollen-{}.exe", release.version));
        if std::fs::write(&path, body).is_err() {
            return;
        }
        // Only a program that calls itself that version of Pollen is run.
        let path_s = path.display().to_string();
        let name = crate::apps::version_string(&path_s, "ProductName");
        let version = crate::apps::version_string(&path_s, "ProductVersion");
        if name.as_deref() != Some("Pollen") || version.as_deref() != Some(release.version.as_str()) {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let _ = std::process::Command::new(&path).arg("--install").spawn();
    });
}

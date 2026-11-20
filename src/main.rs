#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod appearance;
#[cfg(windows)]
mod background;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod cleanup;
#[cfg(windows)]
mod preferences;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod storage;

const APP_NAME: &str = "TermShot";
const LEGACY_APP_NAME: &str = "CliScreenshot";
mod files;
#[cfg(windows)]
include!("win_app.rs");
#[cfg(windows)]
use storage::is_private_temp_dir;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac::is_private_temp_dir;
#[cfg(target_os = "macos")]
fn main() {
    mac::run();
}

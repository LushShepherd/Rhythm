#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
mod automatic_sync;
#[cfg(target_os = "windows")]
mod break_reminder;
#[cfg(target_os = "windows")]
mod cli_quit;
#[cfg(target_os = "windows")]
mod encrypted_file;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod files;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod lifecycle;
#[cfg(target_os = "windows")]
mod notification;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod protected_sync;
#[cfg(target_os = "windows")]
mod sync_engine;
#[cfg(target_os = "windows")]
mod windows;

fn main() {
    #[cfg(target_os = "windows")]
    windows::run();

    #[cfg(not(target_os = "windows"))]
    eprintln!("This desktop prototype targets Windows. The Rhythm core is portable.");
}

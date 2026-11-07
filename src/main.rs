#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod apps;
mod dock;
mod icons;
mod pacer;
mod render;
mod settings;
mod shell;
mod taskbar;
mod tray;
mod util;

use windows::core::{w, Result};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

fn main() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--diag") {
            apps::print_diagnostics();
            return Ok(());
        }
        if let Some(i) = args.iter().position(|a| a == "--snapshot") {
            let dir = args.get(i + 1).cloned().unwrap_or_else(|| ".".into());
            return dock::Dock::snapshot(&dir);
        }
        if args.iter().any(|a| a == "--restore-taskbar") {
            taskbar::restore_after_crash();
            return Ok(());
        }

        let _instance = CreateMutexW(None, true, w!("Local\\Pollen.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }

        // A previous run that was killed may have left the taskbar hidden.
        taskbar::restore_after_crash();
        // Never leave the user without a taskbar, even if Pollen crashes.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            taskbar::restore();
            default_hook(info);
        }));

        let dock = dock::Dock::create();
        if dock.is_err() {
            taskbar::restore();
        }
        let _dock = dock?;

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

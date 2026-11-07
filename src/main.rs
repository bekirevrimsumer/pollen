#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod appicon;
mod autostart;
mod apps;
mod controls;
mod dock;
mod i18n;
mod icons;
mod install;
mod launcher;
mod net;
mod notch;
mod pacer;
mod preview;
mod render;
mod settings;
mod shell;
mod taskbar;
mod tray;
mod update;
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
        if args.iter().any(|a| a == "--controls") {
            println!("sound: {:?}", controls::audio::read());
            println!("internet: {}", controls::network::internet());
            for d in controls::displays::list_now() {
                println!("display: {d:?}");
            }
            let (tx, rx) = std::sync::mpsc::channel();
            controls::network::refresh_radios(move || {
                let _ = tx.send(());
            });
            let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
            println!("radios: {:?}", controls::network::radios());
            // WinRT async calls need a multithreaded apartment; this thread is STA.
            let media = std::thread::spawn(controls::media::read_now).join().unwrap_or_default();
            println!("media current: {:?}", media.current);
            for t in &media.tracks {
                println!(
                    "track: [{}] '{}' - '{}' playing={} {}/{} ms art={:?}",
                    t.app_name,
                    t.title,
                    t.artist,
                    t.playing,
                    t.position_now(),
                    t.duration_ms,
                    t.art.as_ref().map(|a| (a.width, a.height))
                );
                println!("  app id {:?}, mixer volume {:?}", t.app, controls::audio::app_volume(&t.app, &t.app_name));
            }
            let mut sampler = controls::system::Sampler::new();
            std::thread::sleep(std::time::Duration::from_millis(500));
            println!("system: {:?}", sampler.sample());
            let place = std::thread::spawn(|| {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                controls::weather::check()
            })
            .join()
            .ok()
            .flatten();
            println!("weather: {place:?}");
            if let Some(i) = args.iter().position(|a| a == "--city") {
                let name = args.get(i + 1).cloned().unwrap_or_default();
                for p in controls::weather::search(&name) {
                    println!("place: {} ({}) {:.3}, {:.3}", p.name, p.region, p.latitude, p.longitude);
                }
            }
            println!("privacy: {:?}", controls::privacy::read());
            let mut meter = controls::audio::Meter::new();
            println!("peaks: {:?}", meter.peaks());
            return Ok(());
        }
        if args.iter().any(|a| a == "--check-update") {
            println!("this: {}  newer: {:?}", install::VERSION, update::check());
            if let Some(path) = args.iter().position(|a| a == "--check-update").and_then(|i| args.get(i + 1)) {
                println!(
                    "{path}: ProductName {:?}, ProductVersion {:?}",
                    apps::version_string(path, "ProductName"),
                    apps::version_string(path, "ProductVersion")
                );
            }
            return Ok(());
        }
        if let Some(i) = args.iter().position(|a| a == "--recent") {
            // pollen --recent <program path or AppUserModelID>
            let target = args.get(i + 1).cloned().unwrap_or_default();
            apps::prefetch_start_ids();
            std::thread::sleep(std::time::Duration::from_millis(1500));
            let ids = if target.contains('\\') { apps::app_ids(None, Some(&target)) } else { vec![target.clone()] };
            println!("ids: {ids:?}");
            let exe = target.contains('\\').then_some(target.as_str());
            for group in apps::jump_list(&ids, exe, 10) {
                println!("[{}]", group.title);
                for item in group.items {
                    println!("  {} | {} {}", item.name, item.target, item.args);
                }
            }
            return Ok(());
        }
        if let Some(i) = args.iter().position(|a| a == "--search") {
            launcher::print_search(args.get(i + 1).map(String::as_str).unwrap_or(""));
            return Ok(());
        }
        if let Some(i) = args.iter().position(|a| a == "--demo-shots") {
            let dir = args.get(i + 1).cloned().unwrap_or_else(|| ".".into());
            util::set_demo();
            settings::use_defaults();
            if args.iter().any(|a| a == "--english") {
                let mut s = settings::get();
                s.language = settings::Language::English;
                settings::set(s, false);
            }
            dock::Dock::demo_shots(&dir)?;
            launcher::demo_shots(&dir)?;
            return notch::Notch::demo_shots(&dir);
        }
        if let Some(i) = args.iter().position(|a| a == "--snapshot") {
            let dir = args.get(i + 1).cloned().unwrap_or_else(|| ".".into());
            settings::use_defaults();
            return dock::Dock::snapshot(&dir);
        }
        if args.iter().any(|a| a == "--restore-taskbar") {
            taskbar::restore_after_crash();
            return Ok(());
        }

        // Run from a download: install, update or uninstall first.
        if install::handle(&args) {
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

        // The notch first, so the dock can tell it about fullscreen windows.
        let _notch = notch::Notch::create().ok();
        let dock = dock::Dock::create();
        if dock.is_err() {
            taskbar::restore();
        }
        let _dock = dock?;
        // Alt+Space; false when another program already has it.
        let _ = launcher::start();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

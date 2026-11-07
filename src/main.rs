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
            println!("batteries: {:?}", controls::battery::read());
            let mut meter = controls::audio::Meter::new();
            println!("peaks: {:?}", meter.peaks());
            return Ok(());
        }
        if args.iter().any(|a| a == "--toast-events") {
            // Which window events does a notification popup cause?
            unsafe extern "system" fn seen(
                _: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
                event: u32,
                hwnd: windows::Win32::Foundation::HWND,
                object: i32,
                _: i32,
                _: u32,
                _: u32,
            ) {
                if object != 0 || hwnd.0.is_null() {
                    return;
                }
                let mut class = [0u16; 128];
                let n = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class);
                let mut title = [0u16; 128];
                let m = windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title);
                let class = String::from_utf16_lossy(&class[..n as usize]);
                if class.contains("CoreWindow") || class.contains("Xaml") || class.contains("Toast") || class.contains("Notification") {
                    println!("event {event:#06x} class {class} title '{}'", String::from_utf16_lossy(&title[..m as usize]));
                }
            }
            use windows::Win32::UI::Accessibility::SetWinEventHook;
            use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE, WINEVENT_OUTOFCONTEXT};
            let _hooks: Vec<_> = [(0x0002u32, 0x0002u32), (0x8002, 0x8002), (0x8017, 0x8018), (0x800C, 0x800C)]
                .into_iter()
                .map(|(a, b)| SetWinEventHook(a, b, None, Some(seen), 0, 0, WINEVENT_OUTOFCONTEXT))
                .collect();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < until {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            return Ok(());
        }
        if args.iter().any(|a| a == "--notify-watch") {
            // Starts the watcher and prints what it catches for a while.
            controls::notifications::start(windows::Win32::Foundation::HWND::default(), 0);
            for _ in 0..12 {
                std::thread::sleep(std::time::Duration::from_secs(1));
                for toast in controls::notifications::take() {
                    if toast.title.starts_with("Pollen test") {
                        println!("caught: [{}] {} | {}", toast.app, toast.title, toast.body);
                    } else {
                        println!("caught one from {}", toast.app);
                    }
                }
            }
            return Ok(());
        }
        if args.iter().any(|a| a == "--notify-test") {
            // Can an unpackaged program read Windows' notifications?
            let result = std::thread::spawn(|| {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
                use windows::UI::Notifications::Management::UserNotificationListener;
                let listener = match UserNotificationListener::Current() {
                    Ok(l) => l,
                    Err(e) => return format!("Current() failed: {e}"),
                };
                let status = listener.GetAccessStatus().map(|s| s.0);
                let request = listener.RequestAccessAsync().and_then(|op| op.get()).map(|s| s.0);
                use windows::UI::Notifications::NotificationKinds;
                let count = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()).and_then(|list| list.Size());
                let started = std::time::Instant::now();
                for _ in 0..20 {
                    let _ = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()).and_then(|list| list.Size());
                }
                println!("one read: {:.1} ms", started.elapsed().as_secs_f64() * 1000.0 / 20.0);
                let handler = windows::Foundation::TypedEventHandler::new(|_, _| Ok(()));
                let subscribe = listener.NotificationChanged(&handler);
                format!(
                    "access status {status:?}, request {request:?} (0 unspecified, 1 allowed, 2 denied); notifications now: {count:?}; change event: {:?}",
                    subscribe.map(|_| "subscribed")
                )
            })
            .join()
            .unwrap_or_default();
            println!("{result}");
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
            if let Some(theme) = args.iter().position(|a| a == "--theme").and_then(|i| args.get(i + 1)).and_then(|t| settings::Theme::parse(t)) {
                let mut s = settings::get();
                s.theme = theme;
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

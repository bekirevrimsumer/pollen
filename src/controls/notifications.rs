//! New Windows notifications, for the notch to show for a moment.
//!
//! Windows lets a program read the notifications in the Action Center
//! (UserNotificationListener), but only packaged apps may subscribe to new
//! ones, and Windows' notification store (a database under
//! `%LOCALAPPDATA%\Microsoft\Windows\Notifications`) does not trigger folder
//! change notifications. So a worker thread looks at the store's write time
//! every 1.5 s — a few microseconds — and reads the list (some 75 ms) only
//! when it changed.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use windows::Foundation::DateTime;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;


#[derive(Clone, Debug)]
pub struct Toast {
    pub app: String,
    pub title: String,
    pub body: String,
}

static NEW: Mutex<Vec<Toast>> = Mutex::new(Vec::new());

/// The notifications that came in since the last call, oldest first.
pub fn take() -> Vec<Toast> {
    NEW.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
}

fn read(n: &UserNotification) -> Option<Toast> {
    let app = n.AppInfo().ok()?.DisplayInfo().ok()?.DisplayName().ok()?.to_string();
    let binding = n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    let texts: Vec<String> = binding
        .GetTextElements()
        .ok()?
        .into_iter()
        .filter_map(|t| t.Text().ok().map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
        .collect();
    let mut texts = texts.into_iter();
    let title = texts.next().unwrap_or_else(|| app.clone());
    let body = texts.collect::<Vec<_>>().join(" ");
    Some(Toast { app, title, body })
}

/// Windows' clock in its own units (100 ns since 1601), for comparing with
/// a notification's creation time.
fn now() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn age_seconds(created: DateTime) -> i64 {
    (now() - created.UniversalTime) / 10_000_000
}

/// Starts watching; `notify` gets `message` when new notifications are in
/// `take()`. Does nothing if Windows does not allow reading notifications.
pub fn start(notify: HWND, message: u32) {
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-notifications".into())
        .spawn(move || unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let Ok(listener) = UserNotificationListener::Current() else { return };
            let allowed = match listener.GetAccessStatus() {
                Ok(UserNotificationListenerAccessStatus::Allowed) => true,
                _ => listener.RequestAccessAsync().and_then(|op| op.get()) == Ok(UserNotificationListenerAccessStatus::Allowed),
            };
            if !allowed {
                return;
            }
            let list = |listener: &UserNotificationListener| -> Vec<UserNotification> {
                listener
                    .GetNotificationsAsync(NotificationKinds::Toast)
                    .and_then(|op| op.get())
                    .map(|v| v.into_iter().collect())
                    .unwrap_or_default()
            };
            // What is already there is not new.
            let mut seen: HashSet<u32> = list(&listener).iter().filter_map(|n| n.Id().ok()).collect();
            let Some(local) = std::env::var_os("LOCALAPPDATA") else { return };
            let store = std::path::PathBuf::from(local).join(r"Microsoft\Windows\Notifications");
            // The database's write-ahead log changes with every notification.
            let stamp = || {
                ["wpndatabase.db-wal", "wpndatabase.db"]
                    .map(|name| std::fs::metadata(store.join(name)).ok().map(|m| (m.modified().ok(), m.len())))
            };
            let mut last = stamp();
            loop {
                std::thread::sleep(Duration::from_millis(1500));
                let now = stamp();
                if now == last {
                    continue;
                }
                last = now;
                // Windows writes in a few steps; let it finish.
                std::thread::sleep(Duration::from_millis(300));
                let mut fresh = Vec::new();
                for n in list(&listener) {
                    let Ok(id) = n.Id() else { continue };
                    if !seen.insert(id) {
                        continue;
                    }
                    // Only what just arrived, not older ones synced in later.
                    if n.CreationTime().map(age_seconds).unwrap_or(i64::MAX) > 30 {
                        continue;
                    }
                    if let Some(toast) = read(&n) {
                        if toast.app != "Pollen" {
                            fresh.push(toast);
                        }
                    }
                }
                if !fresh.is_empty() {
                    if let Ok(mut q) = NEW.lock() {
                        q.extend(fresh);
                    }
                    let _ = PostMessageW(Some(HWND(notify as *mut _)), message, WPARAM(0), LPARAM(0));
                }
            }
        })
        .ok();
}

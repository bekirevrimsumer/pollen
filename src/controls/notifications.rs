//! New Windows notifications, for the notch to show for a moment, and the
//! ones waiting in the Action Center, for the dock's badges.
//!
//! Windows lets a program read the notifications in the Action Center
//! (UserNotificationListener), but only packaged apps may subscribe to new
//! ones, and Windows' notification store (a database under
//! `%LOCALAPPDATA%\Microsoft\Windows\Notifications`) does not trigger folder
//! change notifications. So a worker thread looks at the store's write time
//! every 1.5 s — a few microseconds — and reads the list (some 75 ms) only
//! when it changed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
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

/// A notification waiting in the Action Center, for the dock's badges.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub id: u32,
    pub aumid: String,
    pub app: String,
    /// When it came, in Windows' units (see `now`).
    pub created: i64,
    /// A direct message or one that mentions the user, which counts on the
    /// badge; the rest of a chat app's channel messages only get a dot, as the
    /// taskbar's orange flash.
    pub loud: bool,
}

/// The user's name, folded (see `fold`) and split into words, for spotting
/// mentions of them: Windows' account name and full name.
fn my_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<String> = Vec::new();
        let mut add = |text: &str| {
            for word in text.split(|c: char| !c.is_alphanumeric()) {
                let word = fold(word);
                if word.chars().count() >= 3 && !names.contains(&word) {
                    names.push(word);
                }
            }
        };
        if let Ok(user) = std::env::var("USERNAME") {
            add(&user);
        }
        if let Some(full) = full_name() {
            add(&full);
        }
        names
    })
}

/// The account's full name (e.g. "Bekir Evrim Sümer"), if it has one.
fn full_name() -> Option<String> {
    use windows::Win32::NetworkManagement::NetManagement::{NetApiBufferFree, NetUserGetInfo, USER_INFO_2};
    let user = std::env::var("USERNAME").ok()?;
    let wide: Vec<u16> = user.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut buffer: *mut u8 = std::ptr::null_mut();
        if NetUserGetInfo(windows::core::PCWSTR::null(), windows::core::PCWSTR(wide.as_ptr()), 2, &mut buffer) != 0
            || buffer.is_null()
        {
            return None;
        }
        let info = &*(buffer as *const USER_INFO_2);
        let name = (!info.usri2_full_name.is_null()).then(|| info.usri2_full_name.to_string().unwrap_or_default());
        let _ = NetApiBufferFree(Some(buffer as *const _));
        name.filter(|n| !n.trim().is_empty())
    }
}

/// Lowercase without Turkish and other accents, so "@SUMER" finds "Sümer".
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'ü' => 'u',
            'ö' => 'o',
            'ç' => 'c',
            'ş' => 's',
            'ğ' => 'g',
            'ı' => 'i',
            'â' | 'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ê' => 'e',
            'î' | 'í' => 'i',
            'û' | 'ú' => 'u',
            c => c,
        })
        .filter(|&c| c != '\u{307}')
        .collect()
}

/// Whether a chat notification is a direct message or mentions the user.
/// Channel messages are told apart by their title (Slack "#channel", Discord
/// "Name (#channel, Server)") or, for Slack, the "Sender: " in front of the
/// text (private channels have no "#"); mentions by an "@" followed by the
/// user's name, or @everyone, @here and @channel.
fn is_loud(aumid: &str, title: &str, body: &str) -> bool {
    let body_folded = fold(body);
    let mentioned = body_folded.match_indices('@').any(|(at, _)| {
        let after: String = body_folded[at + 1..].chars().take(40).collect();
        ["everyone", "here", "channel"].iter().any(|w| after.starts_with(w))
            || my_names().iter().any(|name| after.contains(name.as_str()))
    });
    if mentioned {
        return true;
    }
    let slack = aumid.to_lowercase().contains("slack");
    let sender_first = body
        .split_once(": ")
        .is_some_and(|(sender, _)| !sender.is_empty() && sender.chars().count() <= 40 && !sender.contains(['@', '\n']));
    let channel = title.contains('#') || (slack && sender_first);
    !channel
}

impl Waiting {
    /// Which app it is for `seen`: its app ID, or its name if it has none.
    fn sender(&self) -> String {
        if self.aumid.is_empty() { format!("name:{}", self.app.to_lowercase()) } else { self.aumid.to_lowercase() }
    }
}

static WAITING: Mutex<Vec<Waiting>> = Mutex::new(Vec::new());
/// The window (and its message) told when `waiting()` changes: the dock.
static WAITING_HWND: AtomicIsize = AtomicIsize::new(0);
static WAITING_MESSAGE: AtomicU32 = AtomicU32::new(0);

/// When each app (by `Waiting::sender`) was last in front while it had
/// notifications waiting. Apps rarely take their notifications out of the
/// Action Center once they are read, so the dock counts only newer ones; kept
/// in `%APPDATA%\Pollen\notifications-seen.txt` so they stay read over a restart.
static SEEN: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

fn seen_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen").join("notifications-seen.txt"))
}

fn with_seen<R>(f: impl FnOnce(&mut HashMap<String, i64>) -> R) -> Option<R> {
    let mut guard = SEEN.lock().ok()?;
    let seen = guard.get_or_insert_with(|| {
        let text = seen_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        text.lines()
            .filter_map(|line| {
                let (time, sender) = line.split_once('\t')?;
                Some((sender.to_string(), time.parse().ok()?))
            })
            .collect()
    });
    Some(f(seen))
}

/// Whether `n` came after its app was last in front.
pub fn unseen(n: &Waiting) -> bool {
    let sender = n.sender();
    with_seen(|seen| seen.get(&sender).is_none_or(|&at| n.created > at)).unwrap_or(true)
}

/// Its app is in front: these notifications have been seen.
pub fn mark_seen<'a>(list: impl IntoIterator<Item = &'a Waiting>) {
    let at = now();
    with_seen(|seen| {
        let mut changed = false;
        for n in list {
            let entry = seen.entry(n.sender()).or_insert(i64::MIN);
            if n.created > *entry {
                *entry = at;
                changed = true;
            }
        }
        if changed {
            if let Some(path) = seen_path() {
                let text: String = seen.iter().map(|(sender, at)| format!("{at}\t{sender}\n")).collect();
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, text);
            }
        }
    });
}

/// The notifications that came in since the last call, oldest first.
pub fn take() -> Vec<Toast> {
    NEW.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
}

/// Everything in the Action Center right now.
pub fn waiting() -> Vec<Waiting> {
    WAITING.lock().map(|w| w.clone()).unwrap_or_default()
}

/// `hwnd` gets `message` whenever `waiting()` changes.
pub fn watch_waiting(hwnd: HWND, message: u32) {
    WAITING_MESSAGE.store(message, Ordering::Relaxed);
    WAITING_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
}

fn set_waiting(list: &[UserNotification]) {
    let now: Vec<Waiting> = list
        .iter()
        .filter_map(|n| {
            let id = n.Id().ok()?;
            let info = n.AppInfo().ok()?;
            let aumid = info.AppUserModelId().map(|s| s.to_string()).unwrap_or_default();
            let app = info.DisplayInfo().and_then(|d| d.DisplayName()).map(|s| s.to_string()).unwrap_or_default();
            let created = n.CreationTime().map(|t| t.UniversalTime).unwrap_or(0);
            let texts = texts(n).unwrap_or_default();
            let title = texts.first().map(String::as_str).unwrap_or("");
            let body = texts.get(1..).map(|rest| rest.join(" ")).unwrap_or_default();
            let loud = is_loud(&aumid, title, &body);
            Some(Waiting { id, aumid, app, created, loud })
        })
        .filter(|w| w.app != "Pollen")
        .collect();
    let changed = match WAITING.lock() {
        Ok(mut w) => {
            let same = w.len() == now.len() && w.iter().zip(&now).all(|(a, b)| a.id == b.id);
            *w = now;
            !same
        }
        Err(_) => false,
    };
    let hwnd = WAITING_HWND.load(Ordering::Relaxed);
    if changed && hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WAITING_MESSAGE.load(Ordering::Relaxed), WPARAM(0), LPARAM(0));
        }
    }
}

/// A notification's lines of text: its title first.
fn texts(n: &UserNotification) -> Option<Vec<String>> {
    let binding = n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    Some(
        binding
            .GetTextElements()
            .ok()?
            .into_iter()
            .filter_map(|t| t.Text().ok().map(|s| s.to_string()))
            .filter(|s| !s.trim().is_empty())
            .collect(),
    )
}

fn read(n: &UserNotification) -> Option<Toast> {
    let app = n.AppInfo().ok()?.DisplayInfo().ok()?.DisplayName().ok()?.to_string();
    let mut texts = texts(n)?.into_iter();
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
            // What is already there is not new, but it is waiting.
            let first = list(&listener);
            set_waiting(&first);
            let mut seen: HashSet<u32> = first.iter().filter_map(|n| n.Id().ok()).collect();
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
                let current = list(&listener);
                set_waiting(&current);
                for n in current {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SLACK: &str = "com.tinyspeck.slackdesktop_8yrtsj140pw4g!Slack";
    const DISCORD: &str = "com.squirrel.Discord.Discord";

    #[test]
    fn folds_turkish() {
        assert_eq!(fold("SÜMER"), "sumer");
        assert_eq!(fold("İkram Halıcı"), "ikram halici");
    }

    #[test]
    fn slack_channels_are_quiet() {
        assert!(!is_loud(SLACK, "#builds", "Bitbucket Cloud: Pipeline passed"));
        // A private channel: no "#", but the sender in front.
        assert!(!is_loud(SLACK, "team-room", "Ayşe Yılmaz: günaydın"));
    }

    #[test]
    fn slack_direct_messages_are_loud() {
        assert!(is_loud(SLACK, "Ayşe Yılmaz", "bakıyorum"));
    }

    #[test]
    fn mentions_are_loud() {
        let me = my_names().first().cloned().unwrap_or_default();
        if !me.is_empty() {
            assert!(is_loud(SLACK, "#builds", &format!("Ayşe Yılmaz: @{me} bakar mısın")));
            assert!(is_loud(DISCORD, "Ali (#genel, Sunucu)", &format!("selam @{}", me.to_uppercase())));
        }
        assert!(is_loud(SLACK, "#builds", "Ayşe Yılmaz: @here toplantı"));
        assert!(is_loud(DISCORD, "Bot (#duyurular, Sunucu)", "@everyone yeni video"));
    }

    #[test]
    fn discord() {
        assert!(!is_loud(DISCORD, "\u{2068}dabu\u{2069} (\u{2068}#self-promotion\u{2069}, \u{2068}World\u{2069})", "link"));
        assert!(is_loud(DISCORD, "mcactee", "hey: naber"));
    }
}

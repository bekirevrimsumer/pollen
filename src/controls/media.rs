//! What is playing, through Windows' media sessions (the same source as the
//! media flyout: Spotify, browsers, players...).
//!
//! Event driven: a worker thread subscribes to the session manager's and every
//! session's change events, and only then reads the sessions again and tells
//! the notch. Nothing runs while nothing changes. Album art is decoded once per
//! track, scaled down to a small premultiplied bitmap.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Album art, premultiplied BGRA.
pub struct Art {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone)]
pub struct Track {
    /// The player's AppUserModelID; identifies the session.
    pub app: String,
    pub app_name: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Position when read, and when that was; the notch moves it forward.
    pub position_ms: i64,
    pub read_at: Instant,
    pub duration_ms: i64,
    pub art: Option<Arc<Art>>,
    /// Changes when the track changes, so the art bitmap can be cached by it.
    pub key: String,
}

impl Track {
    pub fn position_now(&self) -> i64 {
        let moved = if self.playing { self.read_at.elapsed().as_millis() as i64 } else { 0 };
        (self.position_ms + moved).min(self.duration_ms.max(0)).max(0)
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub tracks: Vec<Track>,
    /// The session Windows considers current.
    pub current: Option<String>,
}

#[derive(Clone, Copy)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
    /// Seek to milliseconds.
    Seek(i64),
}

enum Message {
    /// Sessions, a track, or play/pause changed: read everything again.
    Changed,
    /// Only a position moved. Players send these often (a live stream about
    /// every second), so they get a light read.
    Timeline,
    Do(String, Command),
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);
static WORKER: OnceLock<Mutex<Sender<Message>>> = OnceLock::new();

pub fn snapshot() -> Snapshot {
    SNAPSHOT.lock().ok().and_then(|s| s.clone()).unwrap_or_default()
}

/// Starts the worker; `notify` gets `message` whenever something changed.
pub fn start(notify: HWND, message: u32) {
    let (tx, rx) = channel();
    if WORKER.set(Mutex::new(tx.clone())).is_err() {
        return;
    }
    let notify = notify.0 as isize;
    std::thread::Builder::new()
        .name("pollen-media".into())
        .spawn(move || run(tx, rx, HWND(notify as *mut _), message))
        .ok();
}

/// Reads the sessions right now on this thread (for `pollen --controls`).
pub fn read_now() -> Snapshot {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return Snapshot::default() };
    let mut cache = HashMap::new();
    let tracks = manager
        .GetSessions()
        .map(|v| v.into_iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .iter()
        .filter_map(|s| {
            let id = s.SourceAppUserModelId().ok()?.to_string();
            read_track(s, &id, &mut cache)
        })
        .collect();
    let current = manager.GetCurrentSession().ok().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string());
    Snapshot { tracks, current }
}

/// Sends a command to the player with this AppUserModelID.
pub fn command(app: &str, command: Command) {
    if let Some(tx) = WORKER.get().and_then(|m| m.lock().ok()) {
        let _ = tx.send(Message::Do(app.to_string(), command));
    }
}

fn run(tx: Sender<Message>, rx: Receiver<Message>, notify: HWND, message: u32) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return };

    let wake = |tx: &Sender<Message>, timeline: bool| {
        let tx = tx.clone();
        move || {
            let _ = tx.send(if timeline { Message::Timeline } else { Message::Changed });
        }
    };
    {
        let w = wake(&tx, false);
        let _ = manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
            w();
            Ok(())
        }));
        let w = wake(&tx, false);
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            w();
            Ok(())
        }));
    }

    // Sessions we listen to, by AppUserModelID.
    let mut watched: HashMap<String, Session> = HashMap::new();
    let mut art_cache: HashMap<String, Option<Arc<Art>>> = HashMap::new();
    let mut last_timeline_read = Instant::now() - std::time::Duration::from_secs(10);
    let post = || unsafe {
        let _ = PostMessageW(Some(notify), message, WPARAM(0), LPARAM(0));
    };
    let _ = tx.send(Message::Changed);

    while let Ok(first) = rx.recv() {
        let (mut changed, mut timeline) = (false, false);
        let mut commands = Vec::new();
        let mut take = |m: Message| match m {
            Message::Changed => changed = true,
            Message::Timeline => timeline = true,
            Message::Do(app, c) => commands.push((app, c)),
        };
        take(first);
        // Events come in bursts (a track change fires several); read once.
        std::thread::sleep(std::time::Duration::from_millis(60));
        while let Ok(m) = rx.try_recv() {
            take(m);
        }

        for (app, c) in commands {
            if let Some(session) = find(&manager, &app) {
                let _ = match c {
                    Command::PlayPause => session.TryTogglePlayPauseAsync().and_then(|op| op.get()),
                    Command::Next => session.TrySkipNextAsync().and_then(|op| op.get()),
                    Command::Previous => session.TrySkipPreviousAsync().and_then(|op| op.get()),
                    Command::Seek(ms) => session.TryChangePlaybackPositionAsync(ms * 10_000).and_then(|op| op.get()),
                };
            }
            changed = true;
        }

        if !changed {
            // Positions only: at most once a second, and the notch is told only
            // when one jumped (a seek); it moves playing positions forward itself.
            if timeline && last_timeline_read.elapsed() >= std::time::Duration::from_secs(1) {
                last_timeline_read = Instant::now();
                if update_positions(&watched) {
                    post();
                }
            }
            continue;
        }

        let sessions: Vec<(Session, String)> = manager
            .GetSessions()
            .map(|v| v.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| {
                let id = s.SourceAppUserModelId().ok()?.to_string();
                Some((s, id))
            })
            .collect();
        // Listen to new sessions; forget closed ones.
        watched.retain(|id, _| sessions.iter().any(|(_, i)| i == id));
        for (session, id) in &sessions {
            if watched.contains_key(id) {
                continue;
            }
            let w = wake(&tx, false);
            let _ = session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                w();
                Ok(())
            }));
            let w = wake(&tx, false);
            let _ = session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                w();
                Ok(())
            }));
            let w = wake(&tx, true);
            let _ = session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                w();
                Ok(())
            }));
            watched.insert(id.clone(), session.clone());
        }

        let tracks: Vec<Track> = sessions.iter().filter_map(|(s, id)| read_track(s, id, &mut art_cache)).collect();
        let current = manager.GetCurrentSession().ok().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string());
        // Keep the art cache to the tracks still around.
        art_cache.retain(|k, _| tracks.iter().any(|t| &t.key == k));
        if let Ok(mut snap) = SNAPSHOT.lock() {
            *snap = Some(Snapshot { tracks, current });
        }
        post();
    }
}

/// Re-reads only the positions. True if one is off from where the notch would
/// have it by more than 1.5 s, or a duration changed.
fn update_positions(watched: &HashMap<String, Session>) -> bool {
    let Ok(mut guard) = SNAPSHOT.lock() else { return false };
    let Some(snap) = guard.as_mut() else { return false };
    let mut jumped = false;
    for track in &mut snap.tracks {
        let Some(session) = watched.get(&track.app) else { continue };
        let Some((position, duration)) = read_timeline(session, track.playing) else { continue };
        if (position - track.position_now()).abs() > 1500 || duration != track.duration_ms {
            track.position_ms = position;
            track.duration_ms = duration;
            track.read_at = Instant::now();
            jumped = true;
        }
    }
    jumped
}

fn find(manager: &Manager, app: &str) -> Option<Session> {
    let sessions = manager.GetSessions().ok()?;
    sessions.into_iter().find(|s| s.SourceAppUserModelId().map(|h| h.to_string() == app).unwrap_or(false))
}

/// FILETIME units (100 ns since 1601), the same clock WinRT's DateTime uses.
fn now_ticks() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

/// Position and length in ms. Players report the position at their last
/// update, so a playing one is moved forward to now.
fn read_timeline(session: &Session, playing: bool) -> Option<(i64, i64)> {
    let timeline = session.GetTimelineProperties().ok()?;
    let start = timeline.StartTime().map(|t| t.Duration).unwrap_or(0);
    let end = timeline.EndTime().map(|t| t.Duration).unwrap_or(0);
    let position = timeline.Position().map(|t| t.Duration).unwrap_or(0);
    let mut position_ms = (position - start) / 10_000;
    if playing {
        if let Ok(updated) = timeline.LastUpdatedTime() {
            let since = (now_ticks() - updated.UniversalTime) / 10_000;
            if (0..6 * 3600 * 1000).contains(&since) {
                position_ms += since;
            }
        }
    }
    Some((position_ms, (end - start) / 10_000))
}

fn read_track(session: &Session, app: &str, art_cache: &mut HashMap<String, Option<Arc<Art>>>) -> Option<Track> {
    let props = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
    if title.trim().is_empty() {
        return None;
    }
    let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
    let playing = session.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).ok() == Some(Status::Playing);

    let (position_ms, duration_ms) = read_timeline(session, playing).unwrap_or((0, 0));

    let key = format!("{app}\u{1}{title}\u{1}{artist}");
    let art = match art_cache.get(&key) {
        Some(art) => art.clone(),
        None => {
            let art = props.Thumbnail().ok().and_then(|t| decode_art(&t)).map(Arc::new);
            art_cache.insert(key.clone(), art.clone());
            art
        }
    };

    Some(Track {
        app: app.to_string(),
        app_name: app_name(app),
        title,
        artist,
        playing,
        position_ms,
        read_at: Instant::now(),
        duration_ms,
        art,
        key,
    })
}

/// "Spotify", "Zen"... from the AppUserModelID, through the shell.
fn app_name(app: &str) -> String {
    static NAMES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    if let Some(name) = NAMES.lock().ok().and_then(|n| n.as_ref().and_then(|m| m.get(app).cloned())) {
        return name;
    }
    let name = crate::apps::shell_display_name(&format!("shell:AppsFolder\\{app}"))
        .or_else(|| {
            // Desktop players often use their exe name.
            app.strip_suffix(".exe").map(|s| s.to_string())
        })
        .unwrap_or_else(|| app.to_string());
    if let Ok(mut names) = NAMES.lock() {
        names.get_or_insert_with(HashMap::new).insert(app.to_string(), name.clone());
    }
    name
}

/// Reads the thumbnail stream and decodes it into a small square-ish bitmap.
fn decode_art(reference: &IRandomAccessStreamReference) -> Option<Art> {
    const SIZE: u32 = 160;
    let stream = reference.OpenReadAsync().ok()?.get().ok()?;
    let length = stream.Size().ok()? as u32;
    if length == 0 || length > 20_000_000 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    reader.LoadAsync(length).ok()?.get().ok()?;
    let mut bytes = vec![0u8; length as usize];
    reader.ReadBytes(&mut bytes).ok()?;

    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let wic_stream = wic.CreateStream().ok()?;
        wic_stream.InitializeFromMemory(&bytes).ok()?;
        let decoder =
            wic.CreateDecoderFromStream(&wic_stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let k = SIZE as f32 / w.max(h) as f32;
        let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let converter = wic.CreateFormatConverter().ok()?;
        converter
            .Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .ok()?;
        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        converter.CopyPixels(std::ptr::null(), tw * 4, &mut pixels).ok()?;
        Some(Art { width: tw, height: th, pixels })
    }
}

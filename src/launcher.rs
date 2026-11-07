//! Alt+Space: a search box in the middle of the screen, in place of the
//! taskbar's search. It finds the apps the Start menu lists (desktop and Store
//! apps alike), recently opened files, a few system commands, and works out
//! sums. Apps opened often rank higher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, SetFocus, MOD_ALT, MOD_NOREPEAT, VK_DOWN, VK_SPACE, VK_UP};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, SHGetKnownFolderItem, KF_FLAG_DEFAULT,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, GetWindowLongPtrW, LoadCursorW, RegisterClassExW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, SWP_NOACTIVATE, SW_HIDE,
    SW_SHOW, WM_ACTIVATE, WM_CHAR, WM_HOTKEY, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_numerics::Vector2;

use crate::i18n::t;
use crate::render::Surface;
use crate::{icons, shell};

const WIDTH: f32 = 620.0;
const SEARCH_HEIGHT: f32 = 58.0;
const ROW: f32 = 48.0;
const ROWS: usize = 7;
const PAD: f32 = 8.0;
const RADIUS: f32 = 18.0;
/// Room around the card for its shadow.
const MARGIN: f32 = 24.0;
const HOTKEY: i32 = 1;

const ICON_SEARCH: char = '\u{E721}';
const ICON_APP: char = '\u{ECAA}';
const ICON_FILE: char = '\u{E8A5}';
const ICON_CALC: char = '\u{E8EF}';
const ICON_LOCK: char = '\u{E72E}';
const ICON_SLEEP: char = '\u{E708}';
const ICON_POWER: char = '\u{E7E8}';
const ICON_RESTART: char = '\u{E777}';
const ICON_SIGN_OUT: char = '\u{F3B1}';
const ICON_SETTINGS: char = '\u{E713}';
const ICON_RECYCLE: char = '\u{E74D}';

static LAUNCHER_HWND: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone)]
struct App {
    name: String,
    /// `shell:AppsFolder\<id>`: how the Start menu itself starts it.
    target: String,
    folded: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Command {
    Lock,
    Sleep,
    Restart,
    ShutDown,
    SignOut,
    EmptyBin,
    PollenSettings,
}

impl Command {
    const ALL: [Command; 7] = [
        Command::Lock,
        Command::Sleep,
        Command::Restart,
        Command::ShutDown,
        Command::SignOut,
        Command::EmptyBin,
        Command::PollenSettings,
    ];

    fn title(self) -> &'static str {
        match self {
            Command::Lock => t("Kilitle", "Lock"),
            Command::Sleep => t("Uyku", "Sleep"),
            Command::Restart => t("Yeniden başlat", "Restart"),
            Command::ShutDown => t("Bilgisayarı kapat", "Shut down"),
            Command::SignOut => t("Oturumu kapat", "Sign out"),
            Command::EmptyBin => t("Geri Dönüşüm Kutusu'nu boşalt", "Empty the Recycle Bin"),
            Command::PollenSettings => t("Pollen ayarları", "Pollen settings"),
        }
    }

    /// Words it answers to, in both languages.
    fn words(self) -> &'static [&'static str] {
        match self {
            Command::Lock => &["kilitle", "lock"],
            Command::Sleep => &["uyku", "uyut", "sleep"],
            Command::Restart => &["yeniden başlat", "restart", "reboot"],
            Command::ShutDown => &["kapat", "bilgisayarı kapat", "shut down", "shutdown", "power off"],
            Command::SignOut => &["oturumu kapat", "çıkış yap", "sign out", "log out", "logoff"],
            Command::EmptyBin => &["çöp", "geri dönüşüm kutusu", "recycle bin", "empty trash"],
            Command::PollenSettings => &["pollen", "pollen ayarları", "pollen settings"],
        }
    }

    fn icon(self) -> char {
        match self {
            Command::Lock => ICON_LOCK,
            Command::Sleep => ICON_SLEEP,
            Command::Restart => ICON_RESTART,
            Command::ShutDown => ICON_POWER,
            Command::SignOut => ICON_SIGN_OUT,
            Command::EmptyBin => ICON_RECYCLE,
            Command::PollenSettings => ICON_SETTINGS,
        }
    }

    /// Asks for a second Enter first.
    fn needs_confirm(self) -> bool {
        matches!(self, Command::Restart | Command::ShutDown | Command::SignOut | Command::EmptyBin)
    }

    fn run(self) {
        let hidden = |args: &str| {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("shutdown.exe")
                .raw_arg(args)
                .creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0)
                .spawn();
        };
        unsafe {
            match self {
                Command::Lock => {
                    let _ = windows::Win32::System::Shutdown::LockWorkStation();
                }
                Command::Sleep => {
                    let _ = windows::Win32::System::Power::SetSuspendState(false, false, false);
                }
                Command::Restart => hidden("/r /t 0"),
                Command::ShutDown => hidden("/s /t 0"),
                Command::SignOut => hidden("/l"),
                Command::EmptyBin => {
                    use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
                    let _ = SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND);
                }
                Command::PollenSettings => crate::notch::show_settings(),
            }
        }
    }
}

#[derive(Clone)]
enum Action {
    Open(String),
    Copy(String),
    Run(Command),
}

#[derive(Clone)]
struct Entry {
    title: String,
    subtitle: String,
    /// What to take the icon from (an app or a file), or a glyph.
    icon: Option<String>,
    glyph: char,
    action: Action,
}

struct Launcher {
    hwnd: HWND,
    surface: Surface,
    brush: ID2D1SolidColorBrush,
    dwrite: IDWriteFactory,
    input_format: IDWriteTextFormat,
    title_format: IDWriteTextFormat,
    sub_format: IDWriteTextFormat,
    glyph_format: IDWriteTextFormat,
    scale: f32,
    origin: (i32, i32),
    query: String,
    results: Vec<Entry>,
    selected: usize,
    /// A command waiting for its second Enter.
    confirming: Option<usize>,
    apps: Arc<Mutex<(Vec<App>, Option<Instant>)>>,
    icons: HashMap<String, Option<ID2D1Bitmap>>,
    usage: HashMap<String, u32>,
    shown: bool,
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn format(dwrite: &IDWriteFactory, family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT) -> windows::core::Result<IDWriteTextFormat> {
    unsafe { dwrite.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("tr-tr")) }
}

/// Lower case, with Turkish letters folded to their plain Latin ones, so
/// "sarki" finds "Şarkı" and "word" finds "Word".
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'İ' | 'I' | 'ı' | 'i' => vec!['i'],
            'Ş' | 'ş' => vec!['s'],
            'Ç' | 'ç' => vec!['c'],
            'Ğ' | 'ğ' => vec!['g'],
            'Ö' | 'ö' => vec!['o'],
            'Ü' | 'ü' => vec!['u'],
            c => c.to_lowercase().collect(),
        })
        .collect()
}

/// How well `name` (folded) matches `q` (folded); 0 for not at all.
fn score(name: &str, q: &str) -> i32 {
    if q.is_empty() {
        return 0;
    }
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let base = if name == q {
        1000
    } else if name.starts_with(q) {
        800 - (name.len() as i32 - q.len() as i32).min(100)
    } else if words.iter().any(|w| w.starts_with(q)) {
        600
    } else if q.len() >= 2 && is_subsequence(q, &words.iter().filter_map(|w| w.chars().next()).collect::<String>()) {
        // Initials, as "vsc" for "Visual Studio Code".
        500
    } else if q.len() >= 3 && name.contains(q) {
        400
    } else {
        return 0;
    };
    // Uninstallers are seldom what is wanted.
    if name.contains("uninstall") || name.contains("kaldir") {
        base - 300
    } else {
        base
    }
}

fn is_subsequence(q: &str, s: &str) -> bool {
    let mut chars = s.chars();
    q.chars().all(|c| chars.by_ref().any(|n| n == c))
}

/// A sum like "12*7", "(3+4)/2", "2^10", "15%"; None for anything else.
fn calculate(text: &str) -> Option<f64> {
    let s: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).map(|c| match c {
        ',' => '.',
        'x' | '×' => '*',
        '÷' => '/',
        c => c,
    }).collect();
    if s.is_empty() || !s.iter().any(|c| c.is_ascii_digit()) || !s.iter().any(|c| "+-*/^%".contains(*c)) {
        return None;
    }
    // The parser recurses per bracket and sign; keep it shallow.
    if s.len() > 120 || s.iter().filter(|c| matches!(c, '(' | '-' | '^')).count() > 30 {
        return None;
    }
    if !s.iter().all(|c| c.is_ascii_digit() || "+-*/^%().".contains(*c)) {
        return None;
    }
    struct P {
        s: Vec<char>,
        i: usize,
    }
    impl P {
        fn peek(&self) -> Option<char> {
            self.s.get(self.i).copied()
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            while let Some(c) = self.peek() {
                match c {
                    '+' => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    '-' => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => break,
                }
            }
            Some(v)
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.power()?;
            while let Some(c) = self.peek() {
                match c {
                    '*' => {
                        self.i += 1;
                        v *= self.power()?;
                    }
                    '/' => {
                        self.i += 1;
                        v /= self.power()?;
                    }
                    _ => break,
                }
            }
            Some(v)
        }
        fn power(&mut self) -> Option<f64> {
            let base = self.unary()?;
            if self.peek() == Some('^') {
                self.i += 1;
                return Some(base.powf(self.power()?));
            }
            Some(base)
        }
        fn unary(&mut self) -> Option<f64> {
            if self.peek() == Some('-') {
                self.i += 1;
                return Some(-self.unary()?);
            }
            let mut v = self.atom()?;
            if self.peek() == Some('%') {
                self.i += 1;
                v /= 100.0;
            }
            Some(v)
        }
        fn atom(&mut self) -> Option<f64> {
            if self.peek() == Some('(') {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(')') {
                    return None;
                }
                self.i += 1;
                return Some(v);
            }
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '.') {
                self.i += 1;
            }
            self.s[start..self.i].iter().collect::<String>().parse().ok()
        }
    }
    let mut p = P { s, i: 0 };
    let v = p.expr()?;
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

fn format_number(v: f64) -> String {
    let text = if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{:.10}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    crate::i18n::decimal(text)
}

fn usage_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Pollen").join("launcher-usage.txt"))
}

fn read_usage() -> HashMap<String, u32> {
    let Some(text) = usage_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return HashMap::new() };
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(n, target)| Some((target.to_string(), n.parse().ok()?)))
        .collect()
}

fn write_usage(usage: &HashMap<String, u32>) {
    if let Some(p) = usage_path() {
        let mut lines: Vec<(&String, &u32)> = usage.iter().collect();
        lines.sort_by(|a, b| b.1.cmp(a.1));
        let text: String = lines.iter().take(200).map(|(t, n)| format!("{n}\t{t}\n")).collect();
        let _ = std::fs::write(p, text);
    }
}

/// Every app the Start menu lists: the shell's Applications folder.
fn read_apps() -> Vec<App> {
    let mut apps = Vec::new();
    unsafe {
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else { return apps };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return apps };
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut fetched = 0u32;
            let _ = items.Next(&mut batch, Some(&mut fetched));
            if fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                let read = |kind| -> Option<String> {
                    let p = item.GetDisplayName(kind).ok()?;
                    let s = String::from_utf16_lossy(p.as_wide());
                    CoTaskMemFree(Some(p.0 as *const _));
                    Some(s)
                };
                let (Some(name), Some(id)) = (read(SIGDN_NORMALDISPLAY), read(SIGDN_PARENTRELATIVEPARSING)) else { continue };
                if name.is_empty() {
                    continue;
                }
                apps.push(App { folded: fold(&name), name, target: format!("shell:AppsFolder\\{id}") });
            }
        }
    }
    apps
}

/// Files opened lately: the shortcuts Windows keeps in the Recent folder.
fn recent_files() -> Vec<(String, PathBuf)> {
    let Some(dir) = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Recent")) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            let name = e.path().file_stem()?.to_string_lossy().to_string();
            // Folders have no extension in their name.
            name.contains('.').then_some((modified, name, e.path()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().take(300).map(|(_, n, p)| (n, p)).collect()
}

/// The best matches for a query, at most ROWS.
fn find(query: &str, apps: &[App], usage: &HashMap<String, u32>) -> Vec<Entry> {
    let q = fold(query.trim());
    let mut scored: Vec<(i32, Entry)> = Vec::new();
    if let Some(v) = calculate(query) {
        let text = format_number(v);
        scored.push((
            10_000,
            Entry {
                title: format!("= {text}"),
                subtitle: t("Kopyalamak için Enter", "Enter to copy").into(),
                icon: None,
                glyph: ICON_CALC,
                action: Action::Copy(text),
            },
        ));
    }
    if q.is_empty() {
        // Nothing typed: the apps opened most.
        let mut used: Vec<&App> = apps.iter().filter(|a| usage.contains_key(&a.target)).collect();
        used.sort_by_key(|a| std::cmp::Reverse(usage[&a.target]));
        for app in used.into_iter().take(ROWS) {
            scored.push((0, app_entry(app)));
        }
    } else {
        for app in apps {
            let s = score(&app.folded, &q);
            if s > 0 {
                let boost = usage.get(&app.target).copied().unwrap_or(0).min(20) as i32 * 15;
                scored.push((s + boost, app_entry(app)));
            }
        }
        for command in Command::ALL {
            let s = command.words().iter().map(|w| score(&fold(w), &q)).max().unwrap_or(0).max(score(&fold(command.title()), &q));
            if s > 0 {
                scored.push((
                    s - 30,
                    Entry {
                        title: command.title().into(),
                        subtitle: t("Komut", "Command").into(),
                        icon: None,
                        glyph: command.icon(),
                        action: Action::Run(command),
                    },
                ));
            }
        }
        if q.chars().count() >= 2 {
            for (name, lnk) in recent_files() {
                let s = score(&fold(&name), &q);
                if s > 0 {
                    let path = lnk.display().to_string();
                    scored.push((
                        s - 120,
                        Entry {
                            title: name,
                            subtitle: t("Son açılan dosya", "Recent file").into(),
                            icon: Some(path.clone()),
                            glyph: ICON_FILE,
                            action: Action::Open(path),
                        },
                    ));
                }
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    scored.into_iter().map(|(_, e)| e).filter(|e| seen.insert(e.title.clone())).take(ROWS).collect()
}


fn app_entry(app: &App) -> Entry {
    Entry {
        title: app.name.clone(),
        subtitle: t("Uygulama", "App").into(),
        icon: Some(app.target.clone()),
        glyph: ICON_APP,
        action: Action::Open(app.target.clone()),
    }
}


/// `pollen --search <query>`: what the launcher would show.
pub fn print_search(query: &str) {
    let apps = read_apps();
    println!("{} apps", apps.len());
    for entry in find(query, &apps, &read_usage()) {
        println!("{} | {}", entry.title, entry.subtitle);
    }
}

fn instance() -> Option<&'static mut Launcher> {
    let hwnd = LAUNCHER_HWND.load(Ordering::Relaxed);
    if hwnd == 0 {
        return None;
    }
    unsafe { (GetWindowLongPtrW(HWND(hwnd as *mut _), GWLP_USERDATA) as *mut Launcher).as_mut() }
}

/// Makes the (hidden) launcher window and claims Alt+Space. Returns false if
/// another program (PowerToys Run, for one) already has the shortcut.
pub fn start() -> bool {
    let Some(hwnd) = create() else { return false };
    if let Some(l) = instance_of(hwnd) {
        l.refresh_apps();
    }
    unsafe { RegisterHotKey(Some(hwnd), HOTKEY, MOD_ALT | MOD_NOREPEAT, VK_SPACE.0 as u32).is_ok() }
}

/// `pollen --demo-shots`: the launcher with sample results (Windows' own
/// apps and the built-in commands, never the user's), for the README.
pub fn demo_shots(dir: &str) -> windows::core::Result<()> {
    let Some(hwnd) = create() else { return Ok(()) };
    let Some(l) = instance_of(hwnd) else { return Ok(()) };
    let app = |name: &str, id: &str| App { name: name.into(), target: format!("shell:AppsFolder\\{id}"), folded: fold(name) };
    let apps = vec![
        app(t("Not Defteri", "Notepad"), "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"),
        app(t("Yapışkan Notlar", "Sticky Notes"), "Microsoft.MicrosoftStickyNotes_8wekyb3d8bbwe!App"),
        app(t("Hesap Makinesi", "Calculator"), "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"),
        app("Terminal", "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"),
    ];
    l.scale = 0.0;
    for (query, name) in [(t("not", "note"), "launcher"), ("(12+30)*4", "launcher-calc"), (t("kilit", "lock"), "launcher-command")] {
        l.query = query.into();
        l.results = find(query, &apps, &HashMap::new());
        l.selected = 0;
        l.layout_for_demo();
        l.render();
        let path = format!("{dir}\\{name}.png");
        l.surface.save_png(&path)?;
        println!("saved {path}");
    }
    Ok(())
}

/// The launcher's window (hidden), not yet listening for its shortcut.
fn create() -> Option<HWND> {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return None };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("PollenLauncher"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("PollenLauncher"),
            w!("Pollen"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            return None;
        };
        let Ok(factory) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return None };
        let Ok(surface) = Surface::new(&factory, 1, 1) else { return None };
        surface.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        surface.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let Ok(brush) = surface.rt.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None) else { return None };
        let Ok(dwrite) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return None };
        let make = |family, size, weight| format(&dwrite, family, size, weight);
        let (Ok(input), Ok(title), Ok(sub), Ok(glyph)) = (
            make(w!("Segoe UI Variable Text"), 18.0, DWRITE_FONT_WEIGHT_NORMAL),
            make(w!("Segoe UI Variable Text"), 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            make(w!("Segoe UI Variable Text"), 11.0, DWRITE_FONT_WEIGHT_NORMAL),
            make(w!("Segoe Fluent Icons"), 18.0, DWRITE_FONT_WEIGHT_NORMAL),
        ) else {
            return None;
        };
        let launcher = Box::new(Launcher {
            hwnd,
            surface,
            brush,
            dwrite,
            input_format: input,
            title_format: title,
            sub_format: sub,
            glyph_format: glyph,
            scale: 1.0,
            origin: (0, 0),
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            confirming: None,
            apps: Arc::new(Mutex::new((Vec::new(), None))),
            icons: HashMap::new(),
            usage: read_usage(),
            shown: false,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(launcher) as isize);
        LAUNCHER_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        Some(hwnd)
    }
}

fn instance_of(hwnd: HWND) -> Option<&'static mut Launcher> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Launcher).as_mut() }
}

/// Opens it (the tray menu, or anything else that wants to).
pub fn show() {
    if let Some(l) = instance() {
        l.show();
    }
}

impl Launcher {
    /// Reads the app list on a worker thread, at most every few minutes.
    fn refresh_apps(&self) {
        let apps = self.apps.clone();
        if apps.lock().is_ok_and(|a| a.1.is_some_and(|at| at.elapsed() < Duration::from_secs(300))) {
            return;
        }
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let list = read_apps();
            if let Ok(mut a) = apps.lock() {
                *a = (list, Some(Instant::now()));
            }
        });
    }

    fn show(&mut self) {
        if self.shown {
            self.hide();
            return;
        }
        self.refresh_apps();
        self.query.clear();
        self.confirming = None;
        self.search();
        unsafe {
            // On the monitor with the mouse, a quarter of the way down.
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx as f32 / 96.0;
            if (scale - self.scale).abs() > f32::EPSILON {
                self.scale = scale;
                self.icons.clear();
                let make = |family, size, weight| format(&self.dwrite, family, size * scale, weight);
                if let (Ok(input), Ok(title), Ok(sub), Ok(glyph)) = (
                    make(w!("Segoe UI Variable Text"), 18.0, DWRITE_FONT_WEIGHT_NORMAL),
                    make(w!("Segoe UI Variable Text"), 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD),
                    make(w!("Segoe UI Variable Text"), 11.0, DWRITE_FONT_WEIGHT_NORMAL),
                    make(w!("Segoe Fluent Icons"), 18.0, DWRITE_FONT_WEIGHT_NORMAL),
                ) {
                    self.input_format = input;
                    self.title_format = title;
                    self.sub_format = sub;
                    self.glyph_format = glyph;
                }
            }
            let (w, h) = self.size();
            let m = info.rcWork;
            self.origin = ((m.left + m.right - w) / 2, m.top + (m.bottom - m.top) / 5);
            let _ = self.surface.resize(w, h);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), self.origin.0, self.origin.1, w, h, SWP_NOACTIVATE);
            self.shown = true;
            self.render();
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    fn size(&self) -> (i32, i32) {
        let s = self.scale;
        let w = ((WIDTH + MARGIN * 2.0) * s).ceil() as i32;
        let h = ((SEARCH_HEIGHT + PAD * 2.0 + ROW * ROWS as f32 + MARGIN * 2.0) * s).ceil() as i32;
        (w, h)
    }

    /// The demo renders at 100%, sized like the real thing.
    fn layout_for_demo(&mut self) {
        if self.scale != 1.0 {
            self.scale = 1.0;
        }
        let (w, h) = self.size();
        let _ = self.surface.resize(w, h);
        self.shown = true;
    }

    fn hide(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        // The pixels are not needed while hidden.
        let _ = self.surface.resize(1, 1);
        self.icons.clear();
    }

    fn search(&mut self) {
        let apps = self.apps.lock().map(|a| a.0.clone()).unwrap_or_default();
        self.results = find(&self.query, &apps, &self.usage);
        self.selected = 0;
        self.confirming = None;
    }

    fn activate(&mut self, index: usize) {
        let Some(entry) = self.results.get(index).cloned() else { return };
        if let Action::Run(command) = entry.action {
            if command.needs_confirm() && self.confirming != Some(index) {
                self.confirming = Some(index);
                self.render();
                return;
            }
        }
        self.hide();
        match entry.action {
            Action::Open(target) => {
                *self.usage.entry(target.clone()).or_insert(0) += 1;
                write_usage(&self.usage);
                shell::launch(&target);
            }
            Action::Copy(text) => set_clipboard(self.hwnd, &text),
            Action::Run(command) => command.run(),
        }
    }

    fn on_char(&mut self, c: u32) {
        match c {
            0x1B => {
                self.hide();
                return;
            }
            0x0D => {
                self.activate(self.selected);
                return;
            }
            0x08 => {
                self.query.pop();
            }
            // Ctrl+Backspace: the last word.
            0x7F => {
                let trimmed = self.query.trim_end().to_string();
                let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
                self.query.truncate(cut);
            }
            // Ctrl+V
            0x16 => {
                if let Some(text) = get_clipboard(self.hwnd) {
                    let room = 100usize.saturating_sub(self.query.chars().count());
                    self.query.extend(text.lines().next().unwrap_or("").trim().chars().take(room));
                }
            }
            c if c >= 0x20 => match char::from_u32(c) {
                Some(ch) if self.query.chars().count() < 100 => self.query.push(ch),
                _ => return,
            },
            _ => return,
        }
        self.search();
        self.render();
    }

    fn on_key(&mut self, key: u32) {
        let n = self.results.len();
        if n == 0 {
            return;
        }
        if key == VK_DOWN.0 as u32 {
            self.selected = (self.selected + 1) % n;
        } else if key == VK_UP.0 as u32 {
            self.selected = (self.selected + n - 1) % n;
        } else {
            return;
        }
        self.confirming = None;
        self.render();
    }

    // ---- drawing ---------------------------------------------------------------------

    fn row_rect(&self, i: usize) -> D2D_RECT_F {
        let s = self.scale;
        let left = (MARGIN + PAD) * s;
        let top = (MARGIN + SEARCH_HEIGHT + PAD) * s + i as f32 * ROW * s;
        rect(left, top, (MARGIN + WIDTH - PAD) * s, top + ROW * s)
    }

    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        (0..self.results.len()).find(|&i| {
            let r = self.row_rect(i);
            x >= r.left && x < r.right && y >= r.top && y < r.bottom
        })
    }

    fn fill_rounded(&self, r: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.surface.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
        }
    }

    fn text_size(&self, text: &str, format: &IDWriteTextFormat) -> (f32, f32) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            match self.dwrite.CreateTextLayout(&utf16, format, 4000.0, 200.0) {
                Ok(layout) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = layout.GetMetrics(&mut m);
                    (m.widthIncludingTrailingWhitespace, m.height)
                }
                Err(_) => (0.0, 0.0),
            }
        }
    }

    fn text(&self, text: &str, format: &IDWriteTextFormat, x: f32, y: f32, color: D2D1_COLOR_F, max_width: f32) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&utf16, format, max_width.max(1.0), 200.0) else { return };
            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(format) {
                let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = layout.SetTrimming(&trimming, &sign);
            }
            self.brush.SetColor(&color);
            self.surface.rt.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }

    fn icon_for(&mut self, source: &str) -> Option<ID2D1Bitmap> {
        if let Some(cached) = self.icons.get(source) {
            return cached.clone();
        }
        let bitmap = icons::load(source, (32.0 * self.scale).ceil() as i32).and_then(|icon| unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            self.surface
                .rt
                .CreateBitmap(D2D_SIZE_U { width: icon.width, height: icon.height }, Some(icon.pixels.as_ptr() as *const _), icon.width * 4, &props)
                .ok()
        });
        self.icons.insert(source.to_string(), bitmap.clone());
        bitmap
    }

    fn render(&mut self) {
        if !self.shown {
            return;
        }
        let s = self.scale;
        let (sr, sg, sb) = crate::settings::palette().surface;
        let (ar, ag, ab) = crate::settings::palette().accent;
        let white = |a: f32| rgba(1.0, 1.0, 1.0, a);
        let rows = self.results.len();
        let height = SEARCH_HEIGHT + if rows > 0 { PAD * 2.0 + ROW * rows as f32 } else { 0.0 };
        let card = rect(MARGIN * s, MARGIN * s, (MARGIN + WIDTH) * s, (MARGIN + height) * s);
        // Icons first: loading one may take a moment, outside the drawing.
        let sources: Vec<Option<String>> = self.results.iter().map(|e| e.icon.clone()).collect();
        let bitmaps: Vec<Option<ID2D1Bitmap>> = sources.iter().map(|src| src.as_ref().and_then(|p| self.icon_for(p))).collect();
        let rt = self.surface.rt.clone();
        unsafe {
            rt.BeginDraw();
            rt.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
            // A soft shadow: a few widening, fading rounded rectangles.
            for k in 1..=6 {
                let g = k as f32 * 3.0 * s;
                let shadow = rect(card.left - g, card.top - g + 6.0 * s, card.right + g, card.bottom + g + 6.0 * s);
                self.fill_rounded(shadow, RADIUS * s + g, rgba(0.0, 0.0, 0.0, 0.05));
            }
            self.fill_rounded(card, RADIUS * s, rgba(sr, sg, sb, 0.97));
            self.brush.SetColor(&white(0.12));
            let edge = rect(card.left + 0.5, card.top + 0.5, card.right - 0.5, card.bottom - 0.5);
            rt.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: edge, radiusX: RADIUS * s, radiusY: RADIUS * s }, &self.brush, 1.0, None);

            // The search row.
            let cy = card.top + SEARCH_HEIGHT * s / 2.0;
            let glyph = ICON_SEARCH.to_string();
            let (gw, gh) = self.text_size(&glyph, &self.glyph_format);
            self.text(&glyph, &self.glyph_format, card.left + 30.0 * s - gw / 2.0, cy - gh / 2.0, white(0.6), gw + 4.0);
            let x = card.left + 52.0 * s;
            if self.query.is_empty() {
                let hint = t("Uygulama, dosya ara ya da hesapla…", "Search apps and files, or calculate…");
                let (_, h) = self.text_size(hint, &self.input_format);
                self.text(hint, &self.input_format, x + 2.0 * s, cy - h / 2.0, white(0.4), card.right - x - 20.0 * s);
            }
            let (qw, qh) = self.text_size(&self.query, &self.input_format);
            let qh = if self.query.is_empty() { self.text_size("A", &self.input_format).1 } else { qh };
            self.text(&self.query, &self.input_format, x, cy - qh / 2.0, white(0.95), card.right - x - 20.0 * s);
            let caret = (x + qw + 1.0 * s).min(card.right - 22.0 * s);
            self.fill_rounded(rect(caret, cy - 10.0 * s, caret + 2.0 * s, cy + 10.0 * s), 1.0 * s, rgba(ar, ag, ab, 1.0));
            if rows > 0 {
                self.brush.SetColor(&white(0.08));
                let y = card.top + SEARCH_HEIGHT * s;
                rt.DrawLine(Vector2 { X: card.left, Y: y }, Vector2 { X: card.right, Y: y }, &self.brush, 1.0, None);
            }

            // The results.
            for (i, entry) in self.results.iter().enumerate() {
                let r = self.row_rect(i);
                if i == self.selected {
                    self.fill_rounded(r, 10.0 * s, rgba(ar, ag, ab, 0.22));
                }
                let icon = rect(r.left + 10.0 * s, r.top + 8.0 * s, r.left + 42.0 * s, r.top + 40.0 * s);
                match &bitmaps[i] {
                    Some(bitmap) => rt.DrawBitmap(bitmap, Some(&icon), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                    None => {
                        self.fill_rounded(icon, 8.0 * s, white(0.08));
                        let g = entry.glyph.to_string();
                        let (gw, gh) = self.text_size(&g, &self.glyph_format);
                        self.text(&g, &self.glyph_format, (icon.left + icon.right - gw) / 2.0, (icon.top + icon.bottom - gh) / 2.0, white(0.85), gw + 4.0);
                    }
                }
                let tx = icon.right + 12.0 * s;
                let (_, th) = self.text_size(&entry.title, &self.title_format);
                let (_, sh) = self.text_size(&entry.subtitle, &self.sub_format);
                let top = r.top + (ROW * s - th - sh) / 2.0;
                self.text(&entry.title, &self.title_format, tx, top, white(0.95), r.right - tx - 130.0 * s);
                let confirm = self.confirming == Some(i);
                let subtitle = if confirm { t("Emin misin? Onaylamak için yine Enter", "Sure? Press Enter again") } else { entry.subtitle.as_str() };
                self.text(subtitle, &self.sub_format, tx, top + th, if confirm { rgba(1.0, 0.62, 0.4, 1.0) } else { white(0.5) }, r.right - tx - 20.0 * s);
                if i == self.selected {
                    let hint = "Enter ↵";
                    let (hw, hh) = self.text_size(hint, &self.sub_format);
                    self.text(hint, &self.sub_format, r.right - 14.0 * s - hw, (r.top + r.bottom - hh) / 2.0, white(0.4), hw + 4.0);
                }
            }
            let _ = rt.EndDraw(None, None);
        }
        let _ = self.surface.present(self.hwnd, self.origin.0, self.origin.1, None);
    }
}

fn set_clipboard(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let target = GlobalLock(memory) as *mut u16;
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                let _ = GlobalUnlock(memory);
                // CF_UNICODETEXT
                let _ = SetClipboardData(13, Some(HANDLE(memory.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn get_clipboard(hwnd: HWND) -> Option<String> {
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = GetClipboardData(13).ok().and_then(|handle| {
            let memory = windows::Win32::Foundation::HGLOBAL(handle.0);
            let p = GlobalLock(memory) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 && len < 10_000 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(memory);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

fn mouse_point(lparam: LPARAM) -> (f32, f32) {
    ((lparam.0 & 0xFFFF) as u16 as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(l) = instance_of(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
    match msg {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY => {
            l.show();
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Clicking anywhere else closes it.
            if wparam.0 & 0xFFFF == 0 {
                l.hide();
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CHAR => {
            l.on_char(wparam.0 as u32);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            l.on_key(wparam.0 as u32);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_point(lparam);
            if let Some(i) = l.hit(x, y) {
                if i != l.selected {
                    l.selected = i;
                    l.confirming = None;
                    l.render();
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = mouse_point(lparam);
            if let Some(i) = l.hit(x, y) {
                l.activate(i);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}


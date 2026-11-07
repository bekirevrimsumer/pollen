//! Pollen.exe is its own installer. Started from anywhere else (a download),
//! it offers to install or update itself for the current user:
//!
//! - the program goes to `%LOCALAPPDATA%\Programs\Pollen\Pollen.exe`,
//! - a Start menu shortcut is made,
//! - it is listed in Settings › Apps (uninstall runs `Pollen.exe --uninstall`),
//! - starting with Windows is set if asked.
//!
//! No admin rights are needed and nothing outside the user's profile changes.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_ALL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Controls::{
    TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_USE_COMMAND_LINKS, TDF_USE_HICON_MAIN,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, LoadIconW, PostMessageW, SW_SHOWNORMAL, WM_CLOSE};

use crate::i18n::t;
use crate::util::{pcwstr, wide};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Pollen";

const INSTALL: i32 = 100;
const RUN_HERE: i32 = 101;
const OPEN_INSTALLED: i32 = 102;
const UNINSTALL: i32 = 103;

fn install_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Programs").join("Pollen"))
}

fn installed_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join("Pollen.exe"))
}

fn start_menu_shortcut() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join("Pollen.lnk"),
    )
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// This process is the installed copy.
pub fn is_installed_copy() -> bool {
    match (std::env::current_exe(), installed_exe()) {
        (Ok(me), Some(installed)) => same_file(&me, &installed),
        _ => false,
    }
}

/// The installed version, from the Apps list entry.
fn installed_version() -> Option<String> {
    if !installed_exe()?.exists() {
        return None;
    }
    let key = wide(UNINSTALL_KEY);
    let mut buffer = [0u16; 64];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            pcwstr(&key),
            w!("DisplayVersion"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Decides, before anything else starts, whether this run installs, updates or
/// uninstalls. Returns true when the process should exit afterwards.
///
/// - `--uninstall`: asks, then removes Pollen (Settings › Apps runs this).
/// - `--install`: installs or updates without asking and starts the installed
///   copy (for scripts and development).
/// - `--here`: runs this copy as it is.
/// - otherwise, a copy that is not the installed one asks what to do.
pub fn handle(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--uninstall") {
        uninstall_flow();
        return true;
    }
    if args.iter().any(|a| a == "--install") {
        let autostart = crate::autostart::is_enabled_anywhere();
        if install(autostart).is_ok() {
            launch_installed();
        }
        return true;
    }
    if args.iter().any(|a| a == "--here") || cfg!(debug_assertions) || is_installed_copy() {
        return false;
    }
    install_flow()
}

fn app_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        LoadIconW(Some(module.into()), PCWSTR(1 as *const u16)).unwrap_or_default()
    }
}

/// A task dialog with command-link buttons; returns the pressed button's id
/// and whether the check box was ticked.
fn ask(
    title: &str,
    content: &str,
    buttons: &[(i32, &str)],
    check: Option<(&str, bool)>,
) -> (i32, bool) {
    let title_w = wide(title);
    let content_w = wide(content);
    let texts: Vec<Vec<u16>> = buttons.iter().map(|(_, t)| wide(t)).collect();
    let items: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&texts)
        .map(|((id, _), t)| TASKDIALOG_BUTTON { nButtonID: *id, pszButtonText: pcwstr(t) })
        .collect();
    let check_w = check.map(|(t, _)| wide(t));
    let mut config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_USE_HICON_MAIN | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: w!("Pollen"),
        pszMainInstruction: pcwstr(&title_w),
        pszContent: pcwstr(&content_w),
        cButtons: items.len() as u32,
        pButtons: items.as_ptr(),
        nDefaultButton: buttons.first().map_or(0, |b| b.0),
        ..Default::default()
    };
    config.Anonymous1.hMainIcon = app_icon();
    if let Some(text) = &check_w {
        config.pszVerificationText = pcwstr(text);
        if check.is_some_and(|(_, on)| on) {
            // TASKDIALOGCONFIG is packed: copy the field out and back.
            let flags = config.dwFlags;
            config.dwFlags = flags | windows::Win32::UI::Controls::TDF_VERIFICATION_FLAG_CHECKED;
        }
    }
    let mut pressed = 0i32;
    let mut checked = BOOL(0);
    unsafe {
        if TaskDialogIndirect(&config, Some(&mut pressed), None, Some(&mut checked)).is_err() {
            return (0, false);
        }
    }
    (pressed, checked.as_bool())
}

fn install_flow() -> bool {
    let Some(dir) = install_dir() else { return false };
    let tr = crate::i18n::is_turkish();
    let (title, content, first) = match installed_version() {
        None => (
            t("Pollen'ı kur", "Install Pollen").to_string(),
            if tr {
                format!(
                    "Pollen görev çubuğunun yerine bir dock ve ekranın üstüne bir ada koyar. \
                     Pollen'dan çıkınca görev çubuğu geri gelir.\n\nKurulum yeri: {}",
                    dir.display()
                )
            } else {
                format!(
                    "Pollen puts a dock in place of the taskbar and an island at the top of the screen. \
                     Quitting Pollen brings the taskbar back.\n\nInstalls to: {}",
                    dir.display()
                )
            },
            (
                INSTALL,
                t(
                    "Kur\nBaşlat menüsüne eklenir; Ayarlar › Uygulamalar'dan kaldırılabilir",
                    "Install\nAdded to the Start menu; remove it from Settings › Apps",
                )
                .to_string(),
            ),
        ),
        Some(v) if v == VERSION => (
            t("Pollen zaten kurulu", "Pollen is already installed").to_string(),
            format!("{} {v}", t("Kurulu sürüm:", "Installed version:")),
            (OPEN_INSTALLED, t("Kurulu Pollen'ı aç", "Open the installed Pollen").to_string()),
        ),
        Some(v) => (
            if tr { format!("Pollen {VERSION} sürümüne güncellensin mi?") } else { format!("Update Pollen to {VERSION}?") },
            if tr {
                format!("Kurulu sürüm: {v}. Ayarların ve sabitlediklerin korunur.")
            } else {
                format!("Installed version: {v}. Your settings and pins stay.")
            },
            (INSTALL, format!("{}\n{v} → {VERSION}", t("Güncelle", "Update"))),
        ),
    };
    let mut buttons = vec![(first.0, first.1.as_str())];
    let reinstall = t("Yeniden kur", "Reinstall");
    if first.0 == OPEN_INSTALLED {
        buttons.push((INSTALL, reinstall));
    }
    buttons.push((
        RUN_HERE,
        t("Kurmadan çalıştır\nBu dosyayı olduğu yerden çalıştırır", "Run without installing\nRuns this file where it is"),
    ));
    let autostart_now = crate::autostart::is_enabled_anywhere();
    let (pressed, autostart) =
        ask(&title, &content, &buttons, Some((t("Windows ile başlat", "Start with Windows"), autostart_now)));
    match pressed {
        INSTALL => {
            match install(autostart) {
                Ok(()) => launch_installed(),
                Err(e) => {
                    ask(t("Pollen kurulamadı", "Pollen could not be installed"), &e, &[], None);
                }
            }
            true
        }
        OPEN_INSTALLED => {
            launch_installed();
            true
        }
        RUN_HERE => false,
        _ => true,
    }
}

/// Asks the running Pollen (if any) to quit, and waits for it.
fn stop_running() {
    unsafe {
        let deadline = Instant::now() + Duration::from_secs(6);
        while let Ok(hwnd) = FindWindowW(w!("PollenDock"), PCWSTR::null()) {
            if hwnd.0.is_null() || Instant::now() > deadline {
                break;
            }
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
            std::thread::sleep(Duration::from_millis(150));
        }
        // The process may still be closing its files.
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Copies this program into place and registers it.
fn install(autostart: bool) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = install_dir().ok_or("LOCALAPPDATA yok")?;
    let target = dir.join("Pollen.exe");
    stop_running();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !same_file(&me, &target) {
        // A closing Pollen may hold the file for a moment.
        let mut result = std::fs::copy(&me, &target).map(|_| ());
        for _ in 0..20 {
            if result.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            result = std::fs::copy(&me, &target).map(|_| ());
        }
        result.map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let _ = make_shortcut(&target);
    register(&target, &dir);
    if autostart {
        crate::autostart::enable_for(&target);
    } else {
        crate::autostart::disable();
    }
    Ok(())
}

fn launch_installed() {
    if let Some(exe) = installed_exe() {
        unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(exe.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

fn make_shortcut(target: &Path) -> windows::core::Result<()> {
    let Some(lnk) = start_menu_shortcut() else { return Ok(()) };
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL)?;
        let target_w = HSTRING::from(target.as_os_str());
        link.SetPath(&target_w)?;
        link.SetDescription(&HSTRING::from(t("Pollen: dock ve ada", "Pollen: dock and island")))?;
        link.SetIconLocation(&target_w, 0)?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
    }
    Ok(())
}

fn set_string(key: &[u16], name: &str, value: &str) {
    let name = wide(name);
    let value = wide(value);
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            pcwstr(key),
            pcwstr(&name),
            REG_SZ.0,
            Some(value.as_ptr() as *const _),
            (value.len() * 2) as u32,
        );
    }
}

fn set_dword(key: &[u16], name: &str, value: u32) {
    let name = wide(name);
    unsafe {
        let _ = RegSetKeyValueW(HKEY_CURRENT_USER, pcwstr(key), pcwstr(&name), REG_DWORD.0, Some(&value as *const u32 as *const _), 4);
    }
}

/// The entry in Settings › Apps.
fn register(exe: &Path, dir: &Path) {
    let key = wide(UNINSTALL_KEY);
    let exe_s = exe.display().to_string();
    set_string(&key, "DisplayName", "Pollen");
    set_string(&key, "DisplayVersion", VERSION);
    set_string(&key, "Publisher", "Pollen");
    set_string(&key, "DisplayIcon", &format!("{exe_s},0"));
    set_string(&key, "InstallLocation", &dir.display().to_string());
    set_string(&key, "UninstallString", &format!("\"{exe_s}\" --uninstall"));
    set_dword(&key, "NoModify", 1);
    set_dword(&key, "NoRepair", 1);
    let size_kb = std::fs::metadata(exe).map(|m| m.len() / 1024).unwrap_or(0) as u32;
    set_dword(&key, "EstimatedSize", size_kb);
}

fn uninstall_flow() {
    let (pressed, wipe) = ask(
        t("Pollen kaldırılsın mı?", "Uninstall Pollen?"),
        t(
            "Görev çubuğu geri gelir; Başlat menüsündeki kısayol ve Uygulamalar listesindeki kayıt silinir.",
            "The taskbar comes back; the Start menu shortcut and the entry in Apps are removed.",
        ),
        &[(UNINSTALL, t("Kaldır", "Uninstall"))],
        Some((t("Ayarlarımı ve sabitlediklerimi de sil", "Also delete my settings and pins"), false)),
    );
    if pressed != UNINSTALL {
        return;
    }
    stop_running();
    // In case Pollen was not running but left the taskbar hidden.
    crate::taskbar::restore_after_crash();
    crate::autostart::disable();
    if let Some(lnk) = start_menu_shortcut() {
        let _ = std::fs::remove_file(lnk);
    }
    let key = wide(UNINSTALL_KEY);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, pcwstr(&key));
    }
    if wipe {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let _ = std::fs::remove_dir_all(PathBuf::from(appdata).join("Pollen"));
        }
    }
    // This program is running from the folder; a short-lived cmd removes it
    // once this process has exited.
    if let Some(dir) = install_dir() {
        if dir.exists() {
            let command = format!("/c ping 127.0.0.1 -n 3 >nul & rmdir /s /q \"{}\"", dir.display());
            let _ = std::process::Command::new("cmd.exe")
                .raw_arg(command)
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn();
        }
    }
}

//! Puts the icon, the version information and the manifest into Pollen.exe,
//! with the Windows SDK's resource compiler (rc.exe). Without the SDK the exe
//! still builds, just without them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/pollen.ico");
    println!("cargo:rerun-if-changed=assets/pollen.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found (Windows SDK); Pollen.exe will have no icon");
        return;
    };

    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let numbers = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let path = |p: &Path| p.display().to_string().replace('\\', "/");

    let script = format!(
        r#"#pragma code_page(65001)
1 ICON "{icon}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Pollen"
      VALUE "FileDescription", "Pollen"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "Pollen"
      VALUE "LegalCopyright", "GPL-3.0-only"
      VALUE "OriginalFilename", "Pollen.exe"
      VALUE "ProductName", "Pollen"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = path(&root.join("assets").join("pollen.ico")),
        manifest = path(&root.join("assets").join("pollen.manifest")),
    );
    let rc_file = out.join("pollen.rc");
    let res_file = out.join("pollen.res");
    std::fs::write(&rc_file, script).unwrap();
    let status = Command::new(&rc).arg("/nologo").arg("/fo").arg(&res_file).arg(&rc_file).status();
    match status {
        Ok(s) if s.success() => {
            // The linker takes the compiled resources as an input; the manifest
            // is ours, so the linker should not make one.
            println!("cargo:rustc-link-arg-bins={}", res_file.display());
            println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
        }
        _ => println!("cargo:warning=rc.exe failed; Pollen.exe will have no icon"),
    }
}

/// The newest x64 rc.exe in the Windows 10/11 SDK.
fn find_rc() -> Option<PathBuf> {
    if let Ok(rc) = std::env::var("RC") {
        return Some(PathBuf::from(rc));
    }
    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("x64").join("rc.exe").exists())
        .collect();
    versions.sort();
    versions.pop().map(|v| v.join("x64").join("rc.exe"))
}

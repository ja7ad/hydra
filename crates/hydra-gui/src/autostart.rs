// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Launch Hydra on startup": registers/unregisters the app as a login item.
//!
//! macOS: a LaunchAgent plist (appears under System Settings > General >
//! Login Items as an allowed background item; no extra permission dialogs
//! required). Linux: an XDG autostart entry. Windows: a value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` — which launches
//! the exe directly (no console flash, unlike the Startup-folder .cmd
//! shipped before 0.2.x) and lets Task Manager's
//! "Startup apps" page show the icon and "Hydra Download Manager" name
//! embedded in the exe by build.rs. `minimized` starts the app with
//! `--minimized`, so it comes up in the tray instead of opening the window.

#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;

fn exe() -> Option<String> {
    // From an AppImage, `current_exe()` is a path inside the runtime's mount
    // — it exists only for this run, so a login entry pointing at it would
    // be dead by the next boot. The image file is the launcher.
    if let Some(img) = hya_updater::appimage_path() {
        return Some(img.to_string_lossy().into_owned());
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
fn entry_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/LaunchAgents/io.github.ja7ad.hydra.plist"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn entry_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".config/autostart/hydra.desktop"))
}

/// Sync the login item with the settings. Safe to call every save.
#[cfg(target_os = "windows")]
pub fn apply(enabled: bool, minimized: bool) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Hydra Download Manager";

    // Versions before 0.2.x wrote a hydra.cmd into the Startup folder, which
    // flashed a console window at logon; clear it whichever way the toggle
    // goes so the two mechanisms never both fire.
    if let Some(legacy) = dirs::config_dir()
        .map(|d| d.join("Microsoft/Windows/Start Menu/Programs/Startup/hydra.cmd"))
    {
        if legacy.exists() && std::fs::remove_file(&legacy).is_ok() {
            crate::log::info("legacy Startup-folder hydra.cmd removed");
        }
    }

    let run = match RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY) {
        Ok((key, _)) => key,
        Err(e) => {
            crate::log::warn(&format!("login item registry open failed: {e}"));
            return;
        }
    };
    if !enabled {
        match run.delete_value(RUN_VALUE) {
            Ok(()) => crate::log::info("login item removed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => crate::log::warn(&format!("login item remove failed: {e}")),
        }
        return;
    }
    let Some(exe) = exe() else { return };
    let cmd = if minimized {
        format!("\"{exe}\" --minimized")
    } else {
        format!("\"{exe}\"")
    };
    match run.set_value(RUN_VALUE, &cmd) {
        Ok(()) => crate::log::info(&format!("login item written: HKCU\\{RUN_KEY}\\{RUN_VALUE}")),
        Err(e) => crate::log::warn(&format!("login item write failed: {e}")),
    }
}

/// Sync the login item with the settings. Safe to call every save.
#[cfg(not(target_os = "windows"))]
pub fn apply(enabled: bool, minimized: bool) {
    let Some(path) = entry_path() else { return };
    if !enabled {
        // On Linux the deb/rpm packages ship /etc/xdg/autostart/hydra.desktop,
        // and deleting the per-user entry would let that system entry apply
        // again. A Hidden=true entry with the same basename masks it instead.
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let masked =
                "[Desktop Entry]\nType=Application\nName=Hydra Download Manager\nHidden=true\n";
            match std::fs::write(&path, masked) {
                Ok(()) => crate::log::info("login item disabled (Hidden=true entry written)"),
                Err(e) => crate::log::warn(&format!("login item disable failed: {e}")),
            }
        }
        #[cfg(target_os = "macos")]
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            crate::log::info("login item removed");
        }
        return;
    }
    let Some(exe) = exe() else { return };
    #[cfg(not(target_os = "macos"))]
    let arg = if minimized { "--minimized" } else { "" };
    let content: String;
    #[cfg(target_os = "macos")]
    {
        let arg_xml = if minimized {
            "        <string>--minimized</string>\n"
        } else {
            ""
        };
        content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.github.ja7ad.hydra</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
{arg_xml}    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        content = format!(
            "[Desktop Entry]\nType=Application\nName=Hydra Download Manager\nExec=\"{exe}\" {arg}\nX-GNOME-Autostart-enabled=true\n"
        );
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(&path, content) {
        Ok(()) => crate::log::info(&format!("login item written: {}", path.display())),
        Err(e) => crate::log::warn(&format!("login item write failed: {e}")),
    }
}

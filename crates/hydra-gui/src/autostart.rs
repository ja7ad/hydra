// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Launch Hydra on startup": registers/unregisters the app as a login item.
//!
//! macOS 13+: `SMAppService.mainApp`, listed under System Settings > General >
//! Login Items > Open at Login. The launch Apple event identifies login
//! launches, which honor the saved tray preference. macOS 11–12: a LaunchAgent
//! plist, which can carry `--minimized`. Linux: an XDG autostart entry.
//! Windows: a value under
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
    let cur = std::env::current_exe().ok()?;
    #[cfg(target_os = "macos")]
    let cur = stable_bundle_exe(cur)?;
    Some(cur.to_string_lossy().into_owned())
}

/// A quarantined app's first launch runs from a Gatekeeper App Translocation
/// mount, and launching straight out of the DMG runs from /Volumes — both
/// paths are gone by the next login, so a login item recorded at that first
/// launch never fires. Map such a path onto the same bundle under
/// /Applications or ~/Applications; if no installed copy exists, report
/// nothing rather than write a dead entry.
#[cfg(target_os = "macos")]
fn stable_bundle_exe(cur: PathBuf) -> Option<PathBuf> {
    if !is_transient(&cur) {
        return Some(cur);
    }
    let comps: Vec<_> = cur.components().collect();
    let app_idx = comps
        .iter()
        .rposition(|c| c.as_os_str().to_string_lossy().ends_with(".app"))?;
    let bundle_name = comps[app_idx].as_os_str().to_owned();
    let inner: PathBuf = comps[app_idx + 1..].iter().collect();
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    for root in roots {
        let candidate = root.join(&bundle_name).join(&inner);
        if candidate.is_file() {
            crate::log::info(&format!(
                "login item: running from transient {} — using installed {}",
                cur.display(),
                candidate.display()
            ));
            return Some(candidate);
        }
    }
    crate::log::warn(&format!(
        "login item: running from transient {} and no installed copy found; entry left as is",
        cur.display()
    ));
    None
}

#[cfg(target_os = "macos")]
fn is_transient(exe: &std::path::Path) -> bool {
    let text = exe.to_string_lossy();
    text.contains("/AppTranslocation/")
        || text.starts_with("/Volumes/")
        || text.starts_with("/private/var/folders/")
        || text.starts_with("/var/folders/")
}

#[cfg(target_os = "macos")]
fn entry_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/LaunchAgents/io.github.ja7ad.hydra.plist"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn entry_path() -> Option<PathBuf> {
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        Some(dirs::home_dir()?.join(format!(".config/autostart/{id}.desktop")))
    } else {
        Some(dirs::home_dir()?.join(".config/autostart/hydra.desktop"))
    }
}

/// Sync the login item with the settings. Safe to call every save.
///
/// A `--config DIR` instance leaves the login item alone. There is exactly
/// one per user and the ordinary install owns it, while a fresh profile's
/// settings have "launch on startup" ON — so a portable copy would seize it
/// on its very first run, and every logon after that would start the
/// portable copy, on the portable download list, instead of the installed
/// one. Same reasoning as the browser registration in `nmhost`.
pub fn apply(enabled: bool, minimized: bool) {
    if let Some(dir) = crate::model::app_dir_override() {
        crate::log::info(&format!(
            "login item: --config {} — left to the default profile",
            dir.display()
        ));
        return;
    }
    apply_platform(enabled, minimized);
}

#[cfg(target_os = "windows")]
fn apply_platform(enabled: bool, minimized: bool) {
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
    if run.get_value::<String, _>(RUN_VALUE).ok().as_deref() == Some(cmd.as_str()) {
        return;
    }
    match run.set_value(RUN_VALUE, &cmd) {
        Ok(()) => crate::log::info(&format!(
            "login item written: HKCU\\{RUN_KEY}\\{RUN_VALUE} = {cmd}"
        )),
        Err(e) => crate::log::warn(&format!("login item write failed: {e}")),
    }
}

#[cfg(target_os = "macos")]
fn apply_platform(enabled: bool, minimized: bool) {
    if objc2::available!(macos = 13.0) {
        remove_launch_agent();
        main_app::apply(enabled);
    } else {
        apply_entry(enabled, minimized);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn apply_platform(enabled: bool, minimized: bool) {
    apply_entry(enabled, minimized);
}

/// The LaunchAgent older versions wrote would start a second copy next to
/// the Open at Login item.
#[cfg(target_os = "macos")]
fn remove_launch_agent() {
    let Some(path) = entry_path() else { return };
    if path.exists() {
        match std::fs::remove_file(&path) {
            Ok(()) => crate::log::info("legacy LaunchAgent login item removed"),
            Err(e) => crate::log::warn(&format!("legacy LaunchAgent remove failed: {e}")),
        }
    }
}

#[cfg(target_os = "macos")]
mod main_app {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Change {
        Register,
        Unregister,
    }

    /// A disabled-by-user item reports `RequiresApproval`; registering it
    /// again only fails, and the switch in System Settings is theirs.
    pub(super) fn change(enabled: bool, status: SMAppServiceStatus) -> Option<Change> {
        match (enabled, status) {
            (true, SMAppServiceStatus::NotRegistered | SMAppServiceStatus::NotFound) => {
                Some(Change::Register)
            }
            (false, SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval) => {
                Some(Change::Unregister)
            }
            _ => None,
        }
    }

    pub(super) fn apply(enabled: bool) {
        // SAFETY: class method with no arguments, available on macOS 13+,
        // which the caller has checked.
        let service = unsafe { SMAppService::mainAppService() };
        // SAFETY: plain property read on a live service object.
        let status = unsafe { service.status() };
        match change(enabled, status) {
            Some(Change::Register) => {
                let exe = std::env::current_exe().unwrap_or_default();
                if super::is_transient(&exe) {
                    crate::log::warn(&format!(
                        "login item: running from transient {}; not registered",
                        exe.display()
                    ));
                    return;
                }
                // SAFETY: registers the running bundle; no arguments.
                match unsafe { service.registerAndReturnError() } {
                    Ok(()) => crate::log::info("login item registered (Open at Login)"),
                    Err(e) => crate::log::warn(&format!("login item register failed: {e}")),
                }
            }
            Some(Change::Unregister) => {
                // SAFETY: as above.
                match unsafe { service.unregisterAndReturnError() } {
                    Ok(()) => crate::log::info("login item unregistered"),
                    Err(e) => crate::log::warn(&format!("login item unregister failed: {e}")),
                }
            }
            None if enabled && status == SMAppServiceStatus::RequiresApproval => {
                crate::log::info("login item switched off in System Settings; left as is");
            }
            None => {}
        }
    }

    pub(super) fn is_enabled() -> bool {
        // SAFETY: as in `apply`.
        unsafe { SMAppService::mainAppService().status() == SMAppServiceStatus::Enabled }
    }
}

/// A login entry that is a file: the pre-13 macOS LaunchAgent, or the XDG
/// autostart entry.
#[cfg(not(target_os = "windows"))]
fn apply_entry(enabled: bool, minimized: bool) {
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
        content = unix_desktop_entry_content(&exe, minimized);
    }
    // Already correct: leave it alone (a rewrite would only bump the
    // Background Task Management generation on macOS for no change).
    if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::write(&path, content) {
        Ok(()) => crate::log::info(&format!("login item written: {} -> {exe}", path.display())),
        Err(e) => crate::log::warn(&format!("login item write failed: {e}")),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn unix_desktop_entry_content(exe: &str, minimized: bool) -> String {
    let space_arg = if minimized { " --minimized" } else { "" };
    if let Ok(id) = std::env::var("FLATPAK_ID") {
        format!(
            "[Desktop Entry]\nType=Application\nName=Hydra Download Manager\nExec=flatpak run {id}{space_arg}\nIcon={id}\nX-GNOME-Autostart-enabled=true\n"
        )
    } else {
        format!(
            "[Desktop Entry]\nType=Application\nName=Hydra Download Manager\nExec=\"{exe}\"{space_arg}\nIcon=hydra\nX-GNOME-Autostart-enabled=true\n"
        )
    }
}

/// Whether the login item is registered and points at an executable that
/// still exists. Used by the permissions guide.
#[cfg(target_os = "macos")]
pub fn is_registered() -> bool {
    if objc2::available!(macos = 13.0) {
        return main_app::is_enabled();
    }
    let Some(path) = entry_path() else {
        return false;
    };
    let Ok(plist) = std::fs::read_to_string(&path) else {
        return false;
    };
    // First <string> inside ProgramArguments is the executable.
    let Some(start) = plist.find("<array>") else {
        return false;
    };
    let rest = &plist[start..];
    let Some(s) = rest.find("<string>") else {
        return false;
    };
    let rest = &rest[s + "<string>".len()..];
    let Some(e) = rest.find("</string>") else {
        return false;
    };
    std::path::Path::new(&rest[..e]).is_file()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::stable_bundle_exe;
    #[cfg(target_os = "macos")]
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_paths_pass_through() {
        let p = PathBuf::from(
            "/Applications/Hydra Download Manager.app/Contents/MacOS/Hydra Download Manager",
        );
        assert_eq!(stable_bundle_exe(p.clone()), Some(p));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn transient_paths_without_an_installed_copy_are_rejected() {
        for p in [
            "/private/var/folders/ab/xyz/T/AppTranslocation/1234/d/NoSuchHydra.app/Contents/MacOS/NoSuchHydra",
            "/Volumes/Hydra/NoSuchHydra.app/Contents/MacOS/NoSuchHydra",
        ] {
            assert_eq!(stable_bundle_exe(PathBuf::from(p)), None, "{p}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_at_login_follows_the_setting_but_not_over_a_user_switch_off() {
        use super::main_app::{change, Change};
        use objc2_service_management::SMAppServiceStatus as S;

        assert_eq!(change(true, S::NotRegistered), Some(Change::Register));
        assert_eq!(change(true, S::NotFound), Some(Change::Register));
        assert_eq!(change(true, S::Enabled), None);
        assert_eq!(change(true, S::RequiresApproval), None);

        assert_eq!(change(false, S::Enabled), Some(Change::Unregister));
        assert_eq!(change(false, S::RequiresApproval), Some(Change::Unregister));
        assert_eq!(change(false, S::NotRegistered), None);
        assert_eq!(change(false, S::NotFound), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dmg_and_translocated_launches_are_transient() {
        use std::path::Path;
        assert!(super::is_transient(Path::new(
            "/private/var/folders/ab/T/AppTranslocation/1/d/Hydra.app/Contents/MacOS/Hydra"
        )));
        assert!(super::is_transient(Path::new(
            "/Volumes/Hydra/Hydra.app/Contents/MacOS/Hydra"
        )));
        assert!(!super::is_transient(Path::new(
            "/Applications/Hydra.app/Contents/MacOS/Hydra"
        )));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn test_unix_entry_path_and_content() {
        std::env::remove_var("FLATPAK_ID");
        let native_entry = super::entry_path();
        assert!(native_entry.unwrap().ends_with("hydra.desktop"));
        let native_content = super::unix_desktop_entry_content("/usr/bin/hydra-gui", true);
        assert!(native_content.contains("Exec=\"/usr/bin/hydra-gui\" --minimized"));
        assert!(native_content.contains("Icon=hydra"));

        let native_content_no_min = super::unix_desktop_entry_content("/usr/bin/hydra-gui", false);
        assert!(native_content_no_min.contains("Exec=\"/usr/bin/hydra-gui\"\n"));

        std::env::set_var("FLATPAK_ID", "io.github.ja7ad.hydra");
        let flatpak_entry = super::entry_path();
        assert!(flatpak_entry
            .unwrap()
            .ends_with("io.github.ja7ad.hydra.desktop"));
        let flatpak_content = super::unix_desktop_entry_content("", true);
        assert!(flatpak_content.contains("Exec=flatpak run io.github.ja7ad.hydra --minimized"));
        assert!(flatpak_content.contains("Icon=io.github.ja7ad.hydra"));

        let flatpak_content_no_min = super::unix_desktop_entry_content("", false);
        assert!(flatpak_content_no_min.contains("Exec=flatpak run io.github.ja7ad.hydra\n"));
        std::env::remove_var("FLATPAK_ID");
    }
}

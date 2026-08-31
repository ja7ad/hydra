// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native-messaging host self-registration.
//!
//! A browser only talks to `hydra-host` if it finds a manifest naming that
//! binary, and every browser looks for it in its own per-user directory (or,
//! on Windows, under its own `HKCU` key). `scripts/install-native-host.sh`
//! writes those by hand, which is fine for a checkout and wrong for an
//! installed application: nobody should have to run a shell script to make
//! the extension work.
//!
//! So the app does it itself, at every start. All of these locations are
//! per-user, so nothing here needs administrator rights, and every write is
//! idempotent — an unchanged manifest is left alone, so this costs a few
//! `stat`s on a normal boot.
//!
//! Only browsers already present on the machine are touched: the profile
//! directory has to exist before a manifest is written into it, so this never
//! creates configuration for a browser the user does not have.
//!
//! Safari is absent on purpose. Its extension talks to the containing app
//! through `SFSafariWebExtensionHandler`, so there is no manifest to write.

use std::path::{Path, PathBuf};

const HOST_NAME: &str = "com.hydra.host";

/// Chromium extension id, derived from the `key` pinned in
/// `extensions/chrome/manifest.json` (first 16 bytes of SHA-256 over the DER
/// public key, hex digits mapped to a–p). Pinning the key is what keeps this
/// id stable across rebuilds; `scripts/build-extensions.sh` documents the
/// consequences of signing with a different one.
const CHROME_EXT_ID: &str = "jpnonmbbkjdpeebdhkjoliklfhkdcomj";

/// Firefox allow-lists by add-on id, not by an extension origin. Mirrors
/// `browser_specific_settings.gecko.id` in `extensions/firefox/manifest.json`.
const FIREFOX_EXT_ID: &str = "hydra@ja7ad.github.io";

/// Where `hydra-host` lives: next to the running executable. Packaging puts
/// both binaries in the same directory on every platform, and resolving it
/// relative to `current_exe` means a moved or relocated install still points
/// at its own host rather than at whatever was there at install time.
fn host_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(target_os = "windows") {
        "hydra-host.exe"
    } else {
        "hydra-host"
    };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

/// The two manifest dialects. Chromium allow-lists an extension origin,
/// Firefox an add-on id; everything else about the file is identical.
fn manifest(host_path: &Path, gecko: bool) -> String {
    let allow = if gecko {
        format!("\"allowed_extensions\": [{}]", json_str(FIREFOX_EXT_ID))
    } else {
        format!(
            "\"allowed_origins\": [{}]",
            json_str(&format!("chrome-extension://{CHROME_EXT_ID}/"))
        )
    };
    format!(
        "{{\n  \"name\": {},\n  \"description\": \"Hydra Download Manager native host\",\n  \"path\": {},\n  \"type\": \"stdio\",\n  {allow}\n}}\n",
        json_str(HOST_NAME),
        json_str(&host_path.to_string_lossy()),
    )
}

/// Minimal JSON string escaping. The only characters that realistically turn
/// up here are the backslashes of a Windows path.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write `body` to `dir/com.hydra.host.json`, but only when `root` exists —
/// that is the test for "this browser is installed for this user". Returns
/// the path when something was actually written.
#[cfg(any(not(target_os = "windows"), test))]
fn write_manifest(root: &Path, dir: &Path, body: &str) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let file = dir.join(format!("{HOST_NAME}.json"));
    // Rewriting an identical file would still bump its mtime for no reason.
    if std::fs::read_to_string(&file).is_ok_and(|cur| cur == body) {
        return None;
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        crate::log::warn(&format!("nmhost: cannot create {}: {e}", dir.display()));
        return None;
    }
    match std::fs::write(&file, body) {
        Ok(()) => Some(file),
        Err(e) => {
            crate::log::warn(&format!("nmhost: cannot write {}: {e}", file.display()));
            None
        }
    }
}

/// `(profile root, manifest directory, is_firefox)` for every browser this
/// platform knows about. The root is what decides whether the browser is
/// installed; the directory is where its manifest goes.
#[cfg(not(target_os = "windows"))]
fn targets(home: &Path) -> Vec<(PathBuf, PathBuf, bool)> {
    // Chromium browsers keep NativeMessagingHosts inside the profile root;
    // Firefox uses one shared directory per Mozilla-family application.
    let chromium = |root: PathBuf| {
        let dir = root.join("NativeMessagingHosts");
        (root, dir, false)
    };

    #[cfg(target_os = "macos")]
    {
        let sup = home.join("Library/Application Support");
        let gecko = |root: PathBuf, dir: PathBuf| (root, dir, true);
        vec![
            chromium(sup.join("Google/Chrome")),
            chromium(sup.join("Google/Chrome Beta")),
            chromium(sup.join("Chromium")),
            chromium(sup.join("Microsoft Edge")),
            chromium(sup.join("BraveSoftware/Brave-Browser")),
            chromium(sup.join("Vivaldi")),
            chromium(sup.join("com.operasoftware.Opera")),
            chromium(sup.join("Arc/User Data")),
            gecko(
                home.join("Library/Application Support/Firefox"),
                sup.join("Mozilla/NativeMessagingHosts"),
            ),
            gecko(
                sup.join("LibreWolf"),
                sup.join("LibreWolf/NativeMessagingHosts"),
            ),
        ]
    }

    #[cfg(target_os = "linux")]
    {
        let cfg = home.join(".config");
        let gecko = |root: PathBuf, dir: PathBuf| (root, dir, true);
        // Snap and Flatpak do not use ~/.config or ~/.mozilla at all: each
        // browser gets its own private tree. Ubuntu has shipped Firefox as a
        // SNAP by default since 22.04, so on a stock Ubuntu the classic
        // paths below match nothing and the extension is left reporting
        // "Hydra is not reachable" with no indication why.
        let snap = home.join("snap");
        let flat = home.join(".var/app");
        vec![
            chromium(cfg.join("google-chrome")),
            chromium(cfg.join("google-chrome-beta")),
            chromium(cfg.join("chromium")),
            chromium(cfg.join("microsoft-edge")),
            chromium(cfg.join("BraveSoftware/Brave-Browser")),
            chromium(cfg.join("vivaldi")),
            chromium(cfg.join("opera")),
            // Snap Chromium keeps its profile under the snap's own tree.
            chromium(snap.join("chromium/common/chromium")),
            // Flatpak browsers keep theirs under the app id.
            chromium(flat.join("com.google.Chrome/config/google-chrome")),
            chromium(flat.join("org.chromium.Chromium/config/chromium")),
            chromium(flat.join("com.brave.Browser/config/BraveSoftware/Brave-Browser")),
            chromium(flat.join("com.microsoft.Edge/config/microsoft-edge")),
            gecko(
                home.join(".mozilla"),
                home.join(".mozilla/native-messaging-hosts"),
            ),
            gecko(
                home.join(".librewolf"),
                home.join(".librewolf/native-messaging-hosts"),
            ),
            // The Ubuntu default.
            gecko(
                snap.join("firefox/common/.mozilla"),
                snap.join("firefox/common/.mozilla/native-messaging-hosts"),
            ),
            gecko(
                flat.join("org.mozilla.firefox/.mozilla"),
                flat.join("org.mozilla.firefox/.mozilla/native-messaging-hosts"),
            ),
            gecko(
                flat.join("io.gitlab.librewolf-community/.librewolf"),
                flat.join("io.gitlab.librewolf-community/.librewolf/native-messaging-hosts"),
            ),
        ]
    }
}

/// The `HKCU` keys each Windows browser reads. Chromium and Gecko share the
/// shape; only the vendor path differs.
#[cfg(target_os = "windows")]
const WIN_KEYS: &[(&str, bool)] = &[
    (r"Software\Google\Chrome\NativeMessagingHosts", false),
    (r"Software\Chromium\NativeMessagingHosts", false),
    (r"Software\Microsoft\Edge\NativeMessagingHosts", false),
    (
        r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
        false,
    ),
    (r"Software\Vivaldi\NativeMessagingHosts", false),
    (r"Software\Opera Software\NativeMessagingHosts", false),
    (r"Software\Mozilla\NativeMessagingHosts", true),
];

/// Point every browser's `HKCU` key at its manifest. Unlike the Unix side
/// there is nothing to probe for first: writing a key for a browser that is
/// not installed is inert, and creating it in advance means a browser
/// installed later works without Hydra being restarted.
#[cfg(target_os = "windows")]
fn register_windows(host: &Path) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    // One manifest per dialect, in Hydra's own directory: on Windows the
    // browser is told where to look, so there is nothing to place inside a
    // browser-owned folder.
    let dir = crate::model::app_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::log::warn(&format!("nmhost: cannot create {}: {e}", dir.display()));
        return;
    }
    let chromium = dir.join(format!("{HOST_NAME}.json"));
    let gecko = dir.join(format!("{HOST_NAME}.firefox.json"));
    for (path, body) in [
        (&chromium, manifest(host, false)),
        (&gecko, manifest(host, true)),
    ] {
        if std::fs::read_to_string(path).is_ok_and(|cur| cur == body) {
            continue;
        }
        if let Err(e) = std::fs::write(path, &body) {
            crate::log::warn(&format!("nmhost: cannot write {}: {e}", path.display()));
            return;
        }
    }

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (key, is_gecko) in WIN_KEYS {
        let want = if *is_gecko { &gecko } else { &chromium }
            .to_string_lossy()
            .to_string();
        let sub = format!(r"{key}\{HOST_NAME}");
        // create_subkey opens an existing key or makes a new one, which is
        // what "register, idempotently" means here.
        match hkcu.create_subkey(&sub) {
            Ok((k, _)) => {
                let cur: std::io::Result<String> = k.get_value("");
                if cur.ok().as_deref() != Some(want.as_str()) {
                    if let Err(e) = k.set_value("", &want) {
                        crate::log::warn(&format!("nmhost: {sub}: {e}"));
                    }
                }
            }
            Err(e) => crate::log::warn(&format!("nmhost: {sub}: {e}")),
        }
    }
    crate::log::info(&format!("nmhost: registry keys point at {}", dir.display()));
}

/// Register the host with every browser on this machine. Idempotent, and
/// safe to call on every start — which is the point: an OS or browser
/// upgrade that wipes a profile directory repairs itself on the next launch.
///
/// Runs off the UI thread; failures are logged and otherwise ignored, since
/// the WebSocket transport still works whenever the app is already running.
pub fn ensure_registered() {
    std::thread::Builder::new()
        .name("nmhost-register".into())
        .spawn(|| {
            let Some(host) = host_binary() else {
                crate::log::warn(
                    "nmhost: hydra-host is not next to the app; browser capture cannot launch Hydra",
                );
                return;
            };

            #[cfg(target_os = "windows")]
            {
                register_windows(&host);
                return;
            }

            #[cfg(not(target_os = "windows"))]
            {
                let Some(home) = dirs::home_dir() else {
                    crate::log::warn("nmhost: no home directory");
                    return;
                };
                let chromium = manifest(&host, false);
                let gecko = manifest(&host, true);
                let mut written = 0;
                for (root, dir, is_gecko) in targets(&home) {
                    let body = if is_gecko { &gecko } else { &chromium };
                    if let Some(p) = write_manifest(&root, &dir, body) {
                        crate::log::info(&format!("nmhost: registered {}", p.display()));
                        written += 1;
                    }
                }
                crate::log::info(&format!(
                    "nmhost: {} manifest(s) written, host = {}",
                    written,
                    host.display()
                ));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_manifest_allow_lists_the_extension_origin() {
        let m = manifest(Path::new("/opt/hydra/hydra-host"), false);
        assert!(m.contains(&format!("chrome-extension://{CHROME_EXT_ID}/")));
        assert!(m.contains("\"allowed_origins\""));
        assert!(m.contains("\"path\": \"/opt/hydra/hydra-host\""));
        assert!(!m.contains("allowed_extensions"));
        // Must parse: a browser silently ignores a malformed manifest.
        serde_json::from_str::<serde_json::Value>(&m).unwrap();
    }

    #[test]
    fn firefox_manifest_allow_lists_the_addon_id() {
        let m = manifest(Path::new("/opt/hydra/hydra-host"), true);
        assert!(m.contains(FIREFOX_EXT_ID));
        assert!(m.contains("\"allowed_extensions\""));
        assert!(!m.contains("allowed_origins"));
        serde_json::from_str::<serde_json::Value>(&m).unwrap();
    }

    #[test]
    fn windows_paths_survive_json_escaping() {
        let m = manifest(Path::new(r"C:\Program Files\Hydra\hydra-host.exe"), false);
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(
            v["path"].as_str().unwrap(),
            r"C:\Program Files\Hydra\hydra-host.exe"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_covers_snap_and_flatpak_browsers() {
        // Ubuntu ships Firefox as a snap, whose profile is nowhere near
        // ~/.mozilla. Missing it means a stock Ubuntu registers nothing and
        // the extension reports "Hydra is not reachable" with no clue why.
        let home = std::path::Path::new("/home/tester");
        let dirs: Vec<String> = targets(home)
            .into_iter()
            .map(|(_, d, _)| d.to_string_lossy().into_owned())
            .collect();
        let has = |p: &str| dirs.iter().any(|d| d == p);

        assert!(
            has("/home/tester/snap/firefox/common/.mozilla/native-messaging-hosts"),
            "snap Firefox — the Ubuntu default — is not covered"
        );
        assert!(has(
            "/home/tester/.var/app/org.mozilla.firefox/.mozilla/native-messaging-hosts"
        ));
        assert!(has(
            "/home/tester/snap/chromium/common/chromium/NativeMessagingHosts"
        ));
        assert!(has(
            "/home/tester/.var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts"
        ));
        // ...without losing the classic ones.
        assert!(has("/home/tester/.mozilla/native-messaging-hosts"));
        assert!(has(
            "/home/tester/.config/google-chrome/NativeMessagingHosts"
        ));
    }

    #[test]
    fn nothing_is_written_where_the_browser_is_absent() {
        let tmp = std::env::temp_dir().join(format!("hydra-nmhost-{}", std::process::id()));
        let root = tmp.join("not-installed");
        let dir = root.join("NativeMessagingHosts");
        assert!(write_manifest(&root, &dir, "{}").is_none());
        assert!(!dir.exists());
    }

    #[test]
    fn an_unchanged_manifest_is_not_rewritten() {
        let tmp = std::env::temp_dir().join(format!("hydra-nmhost-same-{}", std::process::id()));
        let dir = tmp.join("NativeMessagingHosts");
        std::fs::create_dir_all(&tmp).unwrap();
        let body = manifest(Path::new("/opt/hydra/hydra-host"), false);
        assert!(write_manifest(&tmp, &dir, &body).is_some());
        assert!(write_manifest(&tmp, &dir, &body).is_none());
        // A changed host path does get written through.
        let moved = manifest(Path::new("/usr/local/bin/hydra-host"), false);
        assert!(write_manifest(&tmp, &dir, &moved).is_some());
        std::fs::remove_dir_all(&tmp).ok();
    }
}

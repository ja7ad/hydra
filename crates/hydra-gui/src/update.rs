// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application self-update: the startup version check and the "Update Now"
//! pipeline (download → checksum → extract → launch the finisher → exit).
//!
//! The heavy lifting lives in the `hya-updater` crate; this module adapts it
//! to iced — the check is a one-shot `Task::perform` future, the update run
//! is a `Task::run` stream so the dialog can draw live progress. Endpoints
//! come from `hya_updater::api_base()`, so `HYDRA_UPDATE_API` pointed at the
//! mock server (`cargo run -p hya-updater --example mock_server`) exercises
//! this whole path without touching real releases.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What the dialog needs to know about the newer release.
#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    /// Release notes, GitHub-flavoured markdown.
    pub notes: String,
    /// Release web page, for "open in browser".
    pub html_url: String,
    pub asset_name: String,
    pub asset_url: String,
    pub size: u64,
    /// `SHA256SUMS.txt` asset, when the release publishes one.
    pub sums_url: Option<String>,
    /// Whether Hydra can install this update itself. False for a packaged
    /// install (`/usr/bin` from a deb or rpm, a `.pkg` in `/Applications`):
    /// the dialog then offers the installer instead.
    pub in_place: bool,
    /// Whether finishing the update will ask for an administrator password
    /// — a root-owned install (a tarball unpacked into `/usr/local` with
    /// sudo) that Hydra may still replace, once the user authorises it.
    pub needs_auth: bool,
    /// The `.deb`/`.rpm` for this machine, when the install is packaged and
    /// the release ships one: (file name, download URL, size).
    pub package: Option<(String, String, u64)>,
}

/// Progress of a running update, streamed into the dialog.
#[derive(Clone, Debug)]
pub enum UpdateEvent {
    Progress(u64, Option<u64>),
    Verifying,
    Preparing,
    /// The finisher is running; the app must now exit so it can swap files.
    ReadyToRestart,
    Cancelled,
    Failed(String),
}

fn user_agent() -> String {
    format!("hydra-gui/{}", env!("CARGO_PKG_VERSION"))
}

/// Ask the release API whether a newer GUI bundle exists for this machine.
/// `beta` (Options > General > "Download Beta channel") also considers `-rc`
/// pre-releases when one is ahead of the stable release.
///
/// `Ok(None)` covers both "up to date" and "newer release exists but has no
/// asset for this OS/arch" — the dialog can only offer what it can install.
pub async fn check(beta: bool) -> Result<Option<UpdateInfo>, String> {
    let rel = hya_updater::check_channel(&user_agent(), beta)
        .await
        .map_err(|e| e.to_string())?;
    if !hya_updater::is_newer(rel.version(), env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    // Running from an AppImage, the update IS the new AppImage: the release
    // tarball would only be unpackable over a read-only mount that stops
    // existing when this process does.
    let appimage = hya_updater::appimage_path();
    let asset = match &appimage {
        Some(_) => rel.appimage_asset(),
        None => rel.gui_asset(),
    };
    let Some(asset) = asset else {
        crate::log::warn(&format!(
            "update {} available but has no {} bundle",
            rel.version(),
            match &appimage {
                Some(_) => format!("{}.AppImage", hya_updater::appimage_arch()),
                None => format!("{}-{}", hya_updater::os_tag(), hya_updater::arch_tag()),
            }
        ));
        return Ok(None);
    };
    // Can the finisher actually rewrite this install? Everything Hydra put
    // there itself — an unpacked archive, a macOS `.app`, a per-user
    // Windows install — it can replace; a root-owned copy takes an
    // authorisation prompt; only a package manager's files (`/usr/bin` from
    // a deb or rpm, a `.pkg` receipt in `/Applications`) are off limits,
    // because dpkg's database has to keep describing what is on disk.
    // Better to say so now than to download 11 MB first.
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    let method = install_dir
        .as_deref()
        .map(hya_updater::update_method)
        .unwrap_or(hya_updater::UpdateMethod::Package);
    let in_place = method.is_self_update();
    // For an AppImage the install is the image file, not the mount
    // `current_exe()` reports — say so in the log, that is the path the
    // finisher will rewrite.
    let where_ = appimage
        .as_deref()
        .or(install_dir.as_deref())
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "the install directory".into());
    if !in_place {
        crate::log::info(&format!(
            "update {} available but {where_} is owned by a package manager; \
             offering the installer instead",
            rel.version()
        ));
    } else {
        crate::log::info(&format!(
            "update {} available; {where_} updates {}",
            rel.version(),
            match method {
                hya_updater::UpdateMethod::Elevated => "in place, after authorisation",
                hya_updater::UpdateMethod::AppImage => "by replacing the image file",
                _ => "in place",
            }
        ));
    }
    let package = (!in_place)
        .then(|| rel.package_asset())
        .flatten()
        .map(|a| (a.name.clone(), a.browser_download_url.clone(), a.size));
    Ok(Some(UpdateInfo {
        version: rel.version().to_string(),
        notes: hya_updater::clean_notes(&rel.body),
        html_url: rel.html_url.clone(),
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        size: asset.size,
        sums_url: rel
            .asset("SHA256SUMS.txt")
            .map(|a| a.browser_download_url.clone()),
        in_place,
        needs_auth: match method {
            hya_updater::UpdateMethod::Elevated => true,
            // The image may sit in /opt or /usr/local/bin; the finisher
            // elevates on its own, but the dialog should warn first.
            hya_updater::UpdateMethod::AppImage => hya_updater::appimage_needs_auth(),
            _ => false,
        },
        package,
    }))
}

/// Run the full update as an event stream: download the archive into the OS
/// temp staging dir, verify it, extract it, put the finisher binary in a
/// path that will not be overwritten, and start it detached.
pub fn run(
    info: UpdateInfo,
    cancel: Arc<AtomicBool>,
) -> impl iced::futures::Stream<Item = UpdateEvent> {
    iced::stream::channel(64, async move |mut tx| {
        use iced::futures::SinkExt;
        match drive(info, cancel, &mut tx).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                let _ = tx.send(UpdateEvent::Cancelled).await;
            }
            Err(e) => {
                crate::log::error(&format!("update failed: {e}"));
                let _ = tx.send(UpdateEvent::Failed(e.to_string())).await;
            }
        }
    })
}

async fn drive(
    info: UpdateInfo,
    cancel: Arc<AtomicBool>,
    tx: &mut iced::futures::channel::mpsc::Sender<UpdateEvent>,
) -> std::io::Result<()> {
    use iced::futures::SinkExt;
    let ua = user_agent();
    let stage = hya_updater::staging_dir();
    let archive = stage.join(&info.asset_name);

    // Download. Progress goes through try_send: a full channel drops a
    // repaint, never blocks the transfer; the terminal events use send().
    {
        let mut progress_tx = tx.clone();
        let cancel = cancel.clone();
        hya_updater::http::download_to_file(&info.asset_url, &ua, &archive, move |got, total| {
            let _ = progress_tx.try_send(UpdateEvent::Progress(got, total));
            !cancel.load(Ordering::Relaxed)
        })
        .await?;
    }

    // Verify against the release's published checksums when it has any.
    if let Some(sums_url) = &info.sums_url {
        let _ = tx.send(UpdateEvent::Verifying).await;
        let sums = hya_updater::http::get_bytes(sums_url, &ua, 1024 * 1024).await?;
        let sums = String::from_utf8_lossy(&sums).into_owned();
        if let Some(want) = hya_updater::sum_for(&sums, &info.asset_name) {
            let got = hya_updater::file_sha256(&archive)?;
            if got != want {
                let _ = std::fs::remove_file(&archive);
                return Err(std::io::Error::other(
                    "checksum mismatch — the downloaded archive was discarded",
                ));
            }
        }
    }

    let _ = tx.send(UpdateEvent::Preparing).await;
    // An AppImage download is the finished article: one executable file that
    // replaces the installed one. Everything else arrives as an archive of
    // binaries to unpack and copy over.
    let appimage = hya_updater::appimage_path();
    let bundle = match &appimage {
        Some(_) => None,
        None => {
            let unpack = stage.join("unpacked");
            let _ = std::fs::remove_dir_all(&unpack);
            Some(hya_updater::extract(&archive, &unpack)?)
        }
    };

    // The finisher: prefer the NEW release's copy (version-matched to what it
    // installs), fall back to the one shipped next to the running app. Either
    // way it runs from the staging dir so the swap never overwrites it. An
    // AppImage has only the second option — the download is a squashfs image,
    // not a directory, and mounting it to fish one binary out would buy
    // nothing the shipped finisher cannot already do.
    let updater_name = if cfg!(target_os = "windows") {
        "hydra-updater.exe"
    } else {
        "hydra-updater"
    };
    let exe = std::env::current_exe()?;
    let install_dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("executable has no parent directory"))?
        .to_path_buf();
    let updater_src: PathBuf = bundle
        .iter()
        .map(|b| b.join(updater_name))
        .chain(std::iter::once(install_dir.join(updater_name)))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "no {updater_name} in the release bundle or next to the app; \
                 the downloaded update is at {}",
                archive.display()
            ))
        })?;
    let updater = stage.join(updater_name);
    std::fs::copy(&updater_src, &updater)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&updater, std::fs::Permissions::from_mode(0o755));
    }

    let mut cmd = std::process::Command::new(&updater);
    match (&appimage, &bundle) {
        // Replace the image file the user launched, and relaunch that same
        // path — not `current_exe()`, which points into a mount that will
        // not exist a moment from now.
        (Some(img), _) => {
            cmd.arg("--src-file")
                .arg(&archive)
                .arg("--appimage")
                .arg(img)
                .arg("--relaunch")
                .arg(img);
        }
        (None, Some(bundle)) => {
            cmd.arg("--src-dir")
                .arg(bundle)
                .arg("--install-dir")
                .arg(&install_dir)
                // What the new files are: a macOS `.app` carries its version
                // in Info.plist, which nothing in the archive itself can tell
                // the finisher.
                .arg("--app-version")
                .arg(&info.version)
                .arg("--relaunch")
                .arg(&exe);
        }
        (None, None) => return Err(std::io::Error::other("nothing was extracted to install")),
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | DETACHED_PROCESS: no console flash, and the
        // finisher outlives this process cleanly.
        cmd.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    cmd.spawn()?;
    crate::log::info(&format!(
        "update {} downloaded; finisher started, exiting to let it swap files",
        info.version
    ));
    let _ = tx.send(UpdateEvent::ReadyToRestart).await;
    Ok(())
}

/// Startup housekeeping: remove `.old` files a previous update left behind
/// (Windows cannot delete the old exe while it is still tearing down).
pub fn sweep_leftovers() {
    // The AppImage case first: the previous image was renamed aside next to
    // itself, and `current_exe()` points at a mount that never holds one.
    if let Some(img) = hya_updater::appimage_path() {
        hya_updater::sweep_appimage_leftover(&img);
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            hya_updater::sweep_old_files(dir);
        }
    }
}

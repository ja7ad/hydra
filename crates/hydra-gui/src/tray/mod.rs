// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! System-tray icon (docs/logo.png) with a control menu: the app keeps
//! downloading from the tray after the main window closes.
//!
//! One menu, described here as [`Entry`] values, rendered by whichever
//! backend the platform has:
//!
//! * macOS + Windows -> [`muda`]: the `tray-icon`/muda native menus.
//! * Linux -> [`sni`]: the freedesktop StatusNotifierItem protocol spoken
//!   over D-Bus by `ksni`. No GTK, so no GTK main loop to reconcile with
//!   winit's and none of the discontinued gtk3/glib stack `tray-icon`'s own
//!   Linux backend would drag in (RUSTSEC-2024-0429).
//!
//! Activations arrive as menu-item id strings on [`crate::menubus`], the same
//! channel the macOS menu bar uses, so `app.rs` handles tray and menu-bar
//! clicks through one code path.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod muda;
#[cfg(target_os = "linux")]
mod sni;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use muda as backend;
#[cfg(target_os = "linux")]
use sni as backend;

use crate::app::MenuAction;
use crate::dl_indicator::IconFrame;
use crate::i18n::tr;

/// Reserved [`crate::menubus`] id: the desktop's theme changed and the tray
/// glyph has to be re-tinted for it. Not a [`MenuAction`] — nothing in the
/// menu produces it — so `app.rs` answers it the way it answers `show_main`.
/// It travels on the menu channel because that is the route a native surface
/// already has to the UI thread, and only the UI thread may touch the icon.
pub const THEME_CHANGED: &str = "tray_theme_changed";

/// Reserved [`crate::menubus`] id: the download tray icon was clicked.
/// `app.rs` answers with the progress window of the newest active download.
pub const ICON_CLICKED: &str = "icon_clicked";

/// Re-tint the tray glyph for the theme in force now. Must run on the UI
/// thread. A no-op where the platform recolors the glyph itself (macOS
/// template images) or watches it already (the Linux backend re-tints from
/// its own thread).
pub fn refresh_icon() {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    backend::refresh_icon();
}

/// Show a second tray icon — the download arrow at `frame` — beside the
/// Hydra one, or remove it with [`IconFrame::Idle`]. UI thread only on macOS
/// and Windows; before the main icon exists the frame is only remembered.
/// `animate` runs a sheen down the arrow from a native timer (no iced
/// message, so no window repaint).
#[allow(unused_variables)]
pub fn set_progress(frame: IconFrame, animate: bool) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    backend::set_progress(frame, animate);
}

/// Backend-neutral menu description, built once per rebuild and rendered by
/// muda or by ksni. Labels are already translated; ids are
/// [`MenuAction::id`] strings (plus the `show_main` special case `app.rs`
/// handles directly).
pub enum Entry {
    Item {
        id: String,
        label: String,
    },
    Check {
        id: String,
        label: String,
        checked: bool,
    },
    Sub {
        label: String,
        items: Vec<Entry>,
    },
    Separator,
}

fn item(label: &str, action: MenuAction) -> Entry {
    Entry::Item {
        id: action.id(),
        label: tr(label),
    }
}

/// The tray menu for the current locale, queue set and power-save state.
fn model(queues: &[String], power_save: bool) -> Vec<Entry> {
    vec![
        Entry::Item {
            id: "show_main".into(),
            label: tr("Show Hydra"),
        },
        Entry::Separator,
        Entry::Sub {
            label: tr("Downloads"),
            items: vec![
                item("Add new download", MenuAction::AddNewDownload),
                item("Pause All", MenuAction::PauseAll),
                item("Stop All", MenuAction::StopAll),
                Entry::Separator,
                item("Scheduler", MenuAction::Scheduler),
            ],
        },
        Entry::Sub {
            label: tr("Start queue"),
            items: queues
                .iter()
                .map(|q| Entry::Item {
                    id: MenuAction::StartQueue(q.clone()).id(),
                    label: q.clone(),
                })
                .collect(),
        },
        Entry::Sub {
            label: tr("Stop queue"),
            items: queues
                .iter()
                .map(|q| Entry::Item {
                    id: MenuAction::StopQueue(q.clone()).id(),
                    label: q.clone(),
                })
                .collect(),
        },
        Entry::Separator,
        // Checkable: the tick reflects the setting; the menu is rebuilt on
        // toggle so it stays truthful.
        Entry::Check {
            id: MenuAction::PowerSaveToggle.id(),
            label: tr("Power save mode"),
            checked: power_save,
        },
        item("Options", MenuAction::Options),
        item("About Hydra", MenuAction::About),
        Entry::Separator,
        item("Exit", MenuAction::Exit),
    ]
}

/// Create the tray icon; later calls are no-ops. On macOS/Windows this must
/// run on the main thread once the event loop is up (first `WindowOpened`);
/// the Linux backend is a D-Bus client and has no such constraint.
#[allow(unused_variables)]
pub fn install(queues: &[String], power_save: bool) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    backend::install(model(queues, power_save));
}

/// Rebuild the menu (language switch, power-save toggle, queue changes).
#[allow(unused_variables)]
pub fn reinstall(queues: &[String], power_save: bool) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    backend::reinstall(model(queues, power_save));
}

/// Whether the app is reachable from a tray icon right now. Closing the last
/// window is only allowed to leave Hydra running when this is true —
/// otherwise the process would live on with no way to reach it (a GNOME
/// session without the AppIndicator extension, a failed Windows shell
/// notification area registration).
pub fn is_active() -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        backend::is_active()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// Wait (briefly) for the tray to become usable. Only the `--minimized`
/// launch needs this: it opens no window, so a tray that never appears would
/// leave an invisible process. Linux registers over D-Bus asynchronously and
/// at login can start before the shell's status-notifier watcher exists;
/// elsewhere the answer is already final and this returns immediately.
#[allow(unused_variables)]
pub fn wait_ready(timeout: std::time::Duration) -> bool {
    #[cfg(target_os = "linux")]
    {
        let deadline = std::time::Instant::now() + timeout;
        while !backend::is_active() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        true
    }
    #[cfg(not(target_os = "linux"))]
    {
        is_active()
    }
}

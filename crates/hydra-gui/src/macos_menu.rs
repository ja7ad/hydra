// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native macOS menu bar via `muda`.
//!
//! On Windows and Linux the menu bar is drawn inside the main window;
//! on macOS that would break platform convention, so the same menu model
//! (`ui::menu::entries`) is installed as an `NSMenu` and its activations come
//! back as [`crate::app::Message::NativeMenu`] ids through a channel the
//! subscription drains.

#![cfg(target_os = "macos")]

use crate::app::{MenuAction, SortKey};
use crate::i18n::tr;
use std::cell::RefCell;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

/// The toggle/selection state the menu bar must display.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub theme_mode: crate::model::ThemeMode,
    pub show_categories: bool,
    pub font_size: u16,
    pub language: String,
    pub speed_limiter: bool,
}

/// The installed menu, plus the check items whose tick has to track the
/// settings behind them.
struct Installed {
    /// Kept alive: AppKit retains the NSMenu, muda routes callbacks through
    /// these wrappers, and a language switch rebuilds from here.
    _menu: Menu,
    hide_categories: CheckMenuItem,
    speed_limiter: CheckMenuItem,
    themes: Vec<(crate::model::ThemeMode, CheckMenuItem)>,
    fonts: Vec<(u16, CheckMenuItem)>,
    languages: Vec<(String, CheckMenuItem)>,
}

thread_local! {
    /// The installed NSMenu's Rust wrappers (main thread only).
    static CURRENT: RefCell<Option<Installed>> = const { RefCell::new(None) };
}

fn item(label: &str, action: MenuAction) -> MenuItem {
    MenuItem::with_id(action.id(), tr(label), true, None)
}

fn check(label: &str, action: MenuAction, on: bool) -> CheckMenuItem {
    CheckMenuItem::with_id(action.id(), tr(label), true, on, None)
}

/// Install the menu bar. Must run on the main thread with NSApp up (called
/// from the first `WindowOpened`); later calls are no-ops.
pub fn install(state: &MenuState, queues: &[String], languages: &[String]) {
    let installed = CURRENT.with(|c| c.borrow().is_some());
    if installed {
        return;
    }
    reinstall(state, queues, languages);
}

/// Rebuild with the current locale/state and swap it in — called after any
/// toggle the menu displays (language, dark mode, categories, font, limiter).
pub fn reinstall(state: &MenuState, queues: &[String], languages: &[String]) {
    crate::menubus::ensure_menu_handler();

    let menu = Menu::new();

    let app_m = Submenu::new("Hydra", true);
    let _ = app_m.append_items(&[
        &item("About Hydra", MenuAction::About),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ]);
    let _ = menu.append(&app_m);

    let tasks = Submenu::new(tr("Tasks"), true);
    let _ = tasks.append_items(&[
        &item("Add new download", MenuAction::AddNewDownload),
        &item("Add batch download", MenuAction::AddBatch),
        &item(
            "Add batch download from clipboard",
            MenuAction::AddBatchClipboard,
        ),
        &item(
            "Add batch download from text file",
            MenuAction::AddBatchFile,
        ),
        &PredefinedMenuItem::separator(),
        &item("Export", MenuAction::ExportList),
        &item("Import", MenuAction::ImportList),
    ]);
    let _ = menu.append(&tasks);

    let file = Submenu::new(tr("File"), true);
    let _ = file.append_items(&[
        &item("Stop Download", MenuAction::StopDownload),
        &item("Remove", MenuAction::Remove),
        &item("Download Now", MenuAction::DownloadNow),
        &item("Redownload", MenuAction::Redownload),
    ]);
    let _ = menu.append(&file);

    let downloads = Submenu::new(tr("Downloads"), true);
    let _ = downloads.append_items(&[
        &item("Pause All", MenuAction::PauseAll),
        &item("Stop All", MenuAction::StopAll),
        &PredefinedMenuItem::separator(),
        &item("Delete All Completed", MenuAction::DeleteAllCompleted),
        &PredefinedMenuItem::separator(),
        &item("Scheduler", MenuAction::Scheduler),
    ]);
    let start_q = Submenu::new(tr("Start queue"), true);
    let stop_q = Submenu::new(tr("Stop queue"), true);
    for q in queues {
        let _ = start_q.append(&item(q, MenuAction::StartQueue(q.clone())));
        let _ = stop_q.append(&item(q, MenuAction::StopQueue(q.clone())));
    }
    let _ = downloads.append(&start_q);
    let _ = downloads.append(&stop_q);
    let speed_limiter = check(
        "Speed Limiter",
        MenuAction::SpeedLimiterToggle,
        state.speed_limiter,
    );
    let _ = downloads.append_items(&[
        &speed_limiter,
        &PredefinedMenuItem::separator(),
        &item("Options", MenuAction::Options),
    ]);
    let _ = menu.append(&downloads);

    let view = Submenu::new(tr("View"), true);
    let hide_categories = check(
        "Hide categories",
        MenuAction::HideCategories,
        !state.show_categories,
    );
    let _ = view.append(&hide_categories);
    let arrange = Submenu::new(tr("Arrange files"), true);
    for (label, key) in [
        ("File Name", SortKey::Name),
        ("Size", SortKey::Size),
        ("Status", SortKey::Status),
        ("Time left", SortKey::TimeLeft),
        ("Transfer rate", SortKey::Rate),
        ("Last Try Date", SortKey::LastTry),
        ("Description", SortKey::Description),
    ] {
        let _ = arrange.append(&item(label, MenuAction::ArrangeBy(key)));
    }
    let _ = view.append(&arrange);
    let theme_m = Submenu::new(tr("Theme"), true);
    let mut themes = Vec::new();
    for (label, mode) in crate::theme::THEME_CHOICES {
        let it = check(label, MenuAction::SetTheme(mode), state.theme_mode == mode);
        let _ = theme_m.append(&it);
        themes.push((mode, it));
    }
    let _ = view.append(&theme_m);
    let font = Submenu::new(tr("Font"), true);
    let mut fonts = Vec::new();
    for (label, size) in crate::theme::FONT_CHOICES {
        let it = check(label, MenuAction::FontSize(size), state.font_size == size);
        let _ = font.append(&it);
        fonts.push((size, it));
    }
    let _ = view.append(&font);
    let lang = Submenu::new(tr("Language"), true);
    let mut langs = Vec::new();
    for l in languages {
        let it = CheckMenuItem::with_id(
            MenuAction::Language(l.clone()).id(),
            crate::i18n::display_name(l),
            true,
            state.language == *l,
            None,
        );
        let _ = lang.append(&it);
        langs.push((l.clone(), it));
    }
    let _ = view.append(&lang);
    let _ = menu.append(&view);

    let help = Submenu::new(tr("Help"), true);
    let _ = help.append_items(&[
        &item("Hydra Home Page", MenuAction::HomePage),
        &item("Contribute on GitHub", MenuAction::Contribute),
        &item("Keyboard Shortcuts", MenuAction::Shortcuts),
        &item("Permissions", MenuAction::Permissions),
        &item("Logs", MenuAction::Logs),
        &item("Report an Issue", MenuAction::ReportIssue),
        &item("Check for updates", MenuAction::CheckUpdates),
        &item("About Hydra", MenuAction::About),
    ]);
    let _ = menu.append(&help);

    menu.init_for_nsapp();
    // Keep the Rust wrappers alive and drop the previous generation.
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(Installed {
            _menu: menu,
            hide_categories,
            speed_limiter,
            themes,
            fonts,
            languages: langs,
        });
    });
}

/// Re-tick the installed menu from `state`, in place. `false` when no menu is
/// installed yet (nothing to sync; the caller reinstalls).
///
/// AppKit flips a check item the moment it is clicked — muda does it in its
/// own item handler, before the app sees the activation — so the tick a menu
/// is left with is "whatever was clicked last", not what the setting says.
/// Left alone, View > Font showed both the old and the new size ticked, and
/// clicking the size already in use unticked it. The groups here are radio
/// sets and toggles over settings the app owns, so every one of them is set
/// from the settings rather than trusted to have toggled itself.
pub fn sync(state: &MenuState) -> bool {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let Some(installed) = borrow.as_ref() else {
            return false;
        };
        installed
            .hide_categories
            .set_checked(!state.show_categories);
        installed.speed_limiter.set_checked(state.speed_limiter);
        for (mode, item) in &installed.themes {
            item.set_checked(*mode == state.theme_mode);
        }
        for (size, item) in &installed.fonts {
            item.set_checked(*size == state.font_size);
        }
        for (lang, item) in &installed.languages {
            item.set_checked(*lang == state.language);
        }
        true
    })
}

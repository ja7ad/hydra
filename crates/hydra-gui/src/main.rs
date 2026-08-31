// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! hydra-gui: desktop front end for the hydra download engine.
//!
//! Runs as an iced daemon: the main window plus every dialog (Add URL, File
//! Info, progress, Options, Scheduler, ...) is its own OS window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod engine;
mod ext_info;
mod extbus;
mod fmt;
mod i18n;
mod icons;
#[cfg(target_os = "linux")]
mod linux_taskbar;
mod log;
#[cfg(target_os = "macos")]
mod macos_dock;
#[cfg(target_os = "macos")]
mod macos_menu;
#[cfg(target_os = "macos")]
mod macos_surface;
mod menubus;
mod model;
mod nmhost;
mod scan;
mod sounds;
mod theme;
mod tray;
mod ui;
mod update;
mod windows;

use app::{App, Message, WinKind};
use iced::{window, Subscription, Task, Theme};

fn main() -> iced::Result {
    // Single instance: if a running instance answers on the extbus
    // socket, hand it the spotlight (it opens its main window) and leave.
    // Two instances would fight over state.redb, the tray, and ipc.json —
    // the browser extension then talks to whichever wrote ipc.json last.
    let minimized = std::env::args().any(|a| a == "--minimized");
    if extbus::signal_existing(minimized) {
        return Ok(());
    }

    // Software rendering by default: this is a widget UI, not a shader
    // workload, and the wgpu/Metal path costs a 100-300 MB baseline for
    // swapchains and driver heaps where tiny-skia sits in the tens.
    // `renderer = "gpu"` in config.toml opts back in.
    let pre = model::load_config();
    let want_gpu = pre.settings.gpu_render || pre.renderer.as_deref() == Some("gpu");
    if !want_gpu && std::env::var_os("ICED_BACKEND").is_none() {
        std::env::set_var("ICED_BACKEND", "tiny-skia");
    }

    // Vazirmatn ships in the binary and is ALWAYS the default face: system
    // per-glyph fallback shaped some Arabic-script runs to nothing (blank
    // button labels), and a single bundled family renders identically on
    // every OS in every language — its Latin set is clean too.
    iced::daemon(boot, App::update, view)
        .title(title)
        .theme(theme_of)
        .style(style_of)
        // View > Font is a scale factor, not a text size: iced grows the
        // whole interface by the ratio, so the rows, buttons and dialog
        // chrome keep the proportions the layout was drawn with.
        .scale_factor(scale_of)
        .subscription(subscription)
        .font(include_bytes!("../assets/fonts/Vazirmatn-Regular.ttf").as_slice())
        .default_font(iced::Font::with_name("Vazirmatn"))
        .run()
}

fn boot() -> (App, Task<Message>) {
    let cfg = model::load_config();
    if let Some(lang) = &cfg.language {
        i18n::set_locale(lang);
    }
    log::init(cfg.log_level.as_deref());
    let state = model::load_state();
    log::banner();

    // The engine thread must exist before the first subscription poll takes
    // its event receiver.
    engine::ensure_started();
    engine::set_power_save(cfg.settings.power_save);

    // Browser-extension bridge: publish capture settings, then listen for
    // the native-messaging host on a loopback socket (port in ipc.json).
    extbus::publish_config(&cfg);
    extbus::start();
    // Register the native-messaging host with every installed browser, so a
    // fresh install works without anyone running the shell script.
    nmhost::ensure_registered();

    let quota_saved = (state.dl_quota.used, state.dl_quota.window_start);
    let mut app = App {
        cfg,
        state,
        windows: std::collections::HashMap::new(),
        main_id: None,
        selected: vec![],
        sel_anchor: None,
        mods: iced::keyboard::Modifiers::default(),
        col_widths: vec![],
        resizing: None,
        tree_sel: app::TreeSel::All,
        tree_open: [true, false, false, false],
        renaming_queue: None,
        queue_rename_draft: String::new(),
        last_queue_click: None,
        open_menu: None,
        open_submenu: None,
        cursor: iced::Point::ORIGIN,
        ctx_at: None,
        last_click: None,
        sort: (app::SortKey::LastTry, false),
        add_url: app::AddUrlState::default(),
        file_info: app::FileInfoState::default(),
        prog: std::collections::HashMap::new(),
        scans: std::collections::HashMap::new(),
        options: app::OptionsState::default(),
        updater: app::UpdateUiState::default(),
        sch: app::SchState::default(),
        batch: app::BatchState::default(),
        confirm: None,
        confirm_remove_file: false,
        pending_delete: vec![],
        state_dirty: false,
        cfg_dirty: false,
        quota_saved,
        last_clipboard: String::new(),
        pending_add: None,
        capture_raise: false,
        queue_menu: None,
        list_press: None,
        list_drag: false,
        band: None,
        list_drag_from_empty: false,
        hover_row: None,
        hover_col: None,
        drag_order: Vec::new(),
        table_scroll: 0.0,
        table_vh: 0.0,
        cursor_cell: std::sync::Arc::new(ui::probe::CursorCell::default()),
        perm_status: windows::permissions::PermStatus::default(),
        main_pos: None,
        main_size: iced::Size::new(0.0, 0.0),
        system_dark: theme::system_is_dark(),
    };

    // Column widths from config, defaults otherwise.
    app.col_widths = if app.cfg.settings.column_widths.is_empty() {
        ui::table::default_widths()
    } else {
        app.cfg.settings.column_widths.clone()
    };
    // The Q column shrank from a 110px queue-name text column to an
    // icon-only strip; configs saved before the change still carry the old
    // default, which would leave a wide empty band next to File Name.
    if app.col_widths.len() > 1 && app.col_widths[1] == 110.0 {
        app.col_widths[1] = ui::table::default_widths()[1];
    }

    // Queues marked "start on startup" begin running; the first Tick starts
    // their files. Through set_queue_running so paused members are promoted
    // back to Queued — flipping `running` directly left nothing to start.
    let startup_queues: Vec<String> = app
        .cfg
        .queues
        .iter()
        .filter(|q| q.schedule.start_on_startup)
        .map(|q| q.name.clone())
        .collect();
    for name in startup_queues {
        app.set_queue_running(&name, true);
    }

    // Keep the login item in sync with the settings (binary may have moved).
    autostart::apply(
        app.cfg.settings.launch_on_startup,
        app.cfg.settings.start_in_tray,
    );

    // `.old` files a previous update's finisher could not delete (Windows
    // keeps the outgoing exe locked through teardown) get swept now.
    update::sweep_leftovers();

    // Autostart hands us --minimized: live in the tray, no window. Whether
    // the tray really materialises is only knowable on Linux after the D-Bus
    // registration below, so this is the intent, not yet the outcome.
    let mut start_hidden = std::env::args().any(|a| a == "--minimized")
        && cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        ));

    if start_hidden {
        // There is no window to install from, so the tray goes up here. On
        // Linux registration is a D-Bus round trip that can also find no
        // watcher at all (a session without status-notifier support, or a
        // login that outran the shell): wait briefly, and open the window
        // after all rather than leave an invisible process behind.
        let queues: Vec<String> = app.cfg.queues.iter().map(|q| q.name.clone()).collect();
        tray::install(&queues, app.cfg.settings.power_save);
        if tray::wait_ready(std::time::Duration::from_secs(5)) {
            log::info("started minimized to tray");
        } else {
            log::warn("no system tray available; opening the main window instead");
            start_hidden = false;
        }
    }
    // Dock visibility before any window opens, so a tray-only launch never
    // flashes a Dock tile. A window about to open forces Regular: Accessory
    // apps get no menu bar, and on macOS every Hydra menu lives there.
    #[cfg(target_os = "macos")]
    macos_dock::sync(app.cfg.settings.hide_from_taskbar, !start_hidden);
    if start_hidden {
        (app, Task::none())
    } else {
        let open_main = app.open_window(WinKind::Main);
        let perm = app.check_folder_access();
        // Startup update check (Options > General). A modal only ever opens
        // on a positive answer; failures are silent — offline is normal.
        let check = if app.cfg.settings.check_updates_on_startup {
            let beta = app.cfg.settings.beta_channel;
            Task::perform(update::check(beta), Message::UpdateChecked)
        } else {
            Task::none()
        };
        (app, Task::batch([open_main, perm, check]))
    }
}

fn view(app: &App, id: window::Id) -> app::El<'_> {
    match app.windows.get(&id) {
        Some(WinKind::Main) => windows::main_win::view(app),
        Some(WinKind::AddUrl) => windows::add_url::view(app),
        Some(WinKind::FileInfo(_)) => windows::file_info::view(app),
        Some(WinKind::Progress(dl)) => windows::progress::view(app, *dl),
        Some(WinKind::Complete(dl)) => windows::complete::view(app, *dl),
        Some(WinKind::Options) => windows::options::view(app),
        Some(WinKind::Scheduler) => windows::scheduler::view(app),
        Some(WinKind::Batch) => windows::batch::view(app),
        Some(WinKind::About) => windows::about::view(app),
        Some(WinKind::Shortcuts) => windows::shortcuts::view(app),
        Some(WinKind::Confirm) => windows::confirm::view(app),
        Some(WinKind::Permissions) => windows::permissions::view(app),
        Some(WinKind::Update) => windows::update::view(app),
        None => iced::widget::container(iced::widget::text(""))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(theme::window)
            .into(),
    }
}

fn title(app: &App, id: window::Id) -> String {
    use crate::i18n::tr;
    match app.windows.get(&id) {
        // Version lives in the About dialog only.
        Some(WinKind::Main) => tr("Hydra Download Manager"),
        Some(WinKind::AddUrl) => tr("Enter new address to download"),
        Some(WinKind::FileInfo(_)) => tr("Download File Info"),
        Some(WinKind::Progress(dl)) => windows::progress::title(app, *dl),
        Some(WinKind::Complete(_)) => tr("Download complete"),
        Some(WinKind::Options) => tr("Hydra Configuration"),
        Some(WinKind::Scheduler) => tr("Scheduler"),
        Some(WinKind::Batch) => tr("Add batch download"),
        Some(WinKind::About) => tr("About Hydra"),
        Some(WinKind::Shortcuts) => tr("Keyboard Shortcuts"),
        Some(WinKind::Confirm) => tr("Hydra"),
        Some(WinKind::Permissions) => tr("Permissions"),
        Some(WinKind::Update) => tr("Update Hydra"),
        None => "Hydra".into(),
    }
}

fn theme_of(app: &App, _id: window::Id) -> Theme {
    let dark = match app.cfg.settings.theme() {
        model::ThemeMode::System => app.system_dark,
        model::ThemeMode::Light => false,
        model::ThemeMode::Dark => true,
    };
    if dark {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// Paint every window surface explicitly: unpainted regions otherwise show
/// through as black bands during resize/tab switches.
/// The View > Font ratio, applied to every window (see `theme::ui_scale`).
fn scale_of(app: &App, _id: window::Id) -> f32 {
    theme::ui_scale(app.cfg.settings.font_size)
}

fn style_of(_app: &App, t: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: theme::window_bg(t),
        text_color: theme::text_color(t),
    }
}

fn subscription(app: &App) -> Subscription<Message> {
    let power_save = app.cfg.settings.power_save;
    let mut subs = vec![
        Subscription::run(engine_events).map(Message::Engine),
        Subscription::run(native_menu_events).map(Message::NativeMenu),
        Subscription::run(ext_events).map(Message::Ext),
        Subscription::run(scan_events).map(Message::Scan),
        iced::time::every(std::time::Duration::from_secs(if power_save {
            3
        } else {
            1
        }))
        .map(|_| Message::Tick),
        // OS light/dark flips (View > Theme > System Default); the palette
        // in force at startup is read directly, see `theme::system_is_dark`.
        // Subscribed whatever the setting is, so switching to System Default
        // never has to wait for the next flip to catch up with the desktop.
        iced::system::theme_changes().map(Message::SystemTheme),
        window::close_events().map(Message::WindowClosed),
        window::close_requests().map(Message::WindowCloseRequested),
        // Modifier state for Cmd/Ctrl/Shift multi-selection, and global
        // mouse-up to end a column-resize drag wherever it is released.
        iced::event::listen_with(|event, status, window| match event {
            iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => {
                Some(Message::Mods(m))
            }
            // Shortcuts fire only on events no widget consumed, so typing
            // Cmd+A inside a text field still selects text, not downloads.
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. })
                if status == iced::event::Status::Ignored && modifiers.command() =>
            {
                Some(Message::RawKey(key, modifiers))
            }
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::MouseUp)
            }
            iced::Event::Window(iced::window::Event::Moved(p)) => {
                Some(Message::WinMoved(window, p))
            }
            iced::Event::Window(iced::window::Event::Resized(s)) => {
                Some(Message::WinResized(window, s))
            }
            _ => None,
        }),
    ];
    // A drag in flight samples the pointer on a fixed cadence instead of on
    // every motion event. Each message repaints the window — on a Retina
    // display macOS colour-converts that whole surface per present — so a
    // 120 Hz pointer meant 120 full repaints a second for a rectangle that
    // reads the same at 60. Power save halves it again.
    if app.list_drag || app.resizing.is_some() {
        subs.push(
            iced::time::every(std::time::Duration::from_millis(if power_save {
                33
            } else {
                16
            }))
            .map(|_| Message::DragTick),
        );
    }
    // The glide tick exists only while something is moving; an idle list
    // costs zero redraws. Power save drops the animation entirely — the bar
    // then steps at the engine's event rate.
    // The marquee of a running virus scan needs the same tick even when no
    // transfer is left moving, so it is part of the condition.
    if !power_save && (app.state.downloads.iter().any(|d| d.state.is_active()) || app.scanning()) {
        subs.push(
            iced::time::every(std::time::Duration::from_millis(80)).map(|_| Message::AnimTick),
        );
    }
    Subscription::batch(subs)
}

/// Engine events as a stream. The receiver can be taken only once; a second
/// subscription instance (which iced never creates for the same id) would
/// simply pend forever.
fn engine_events() -> impl iced::futures::Stream<Item = engine::Event> {
    iced::futures::stream::unfold(engine::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Browser-extension captures arriving over the extbus loopback socket.
fn ext_events() -> impl iced::futures::Stream<Item = extbus::ExtEvent> {
    iced::futures::stream::unfold(extbus::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Console output of the virus scanner running over a finished file.
fn scan_events() -> impl iced::futures::Stream<Item = scan::ScanEvent> {
    iced::futures::stream::unfold(scan::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let ev = r.recv().await?;
                Some((ev, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

/// Menu-bar and tray activations, one stream on every platform (the channel
/// just stays quiet where there are no native menus).
fn native_menu_events() -> impl iced::futures::Stream<Item = String> {
    iced::futures::stream::unfold(menubus::take_events(), |rx| async move {
        match rx {
            Some(mut r) => {
                let id = r.recv().await?;
                Some((id, Some(r)))
            }
            None => iced::futures::future::pending().await,
        }
    })
}

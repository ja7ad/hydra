// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Linux tray backend: the freedesktop StatusNotifierItem protocol, spoken
//! over D-Bus by `ksni`.
//!
//! Why not `tray-icon`'s own Linux backend: it is libappindicator, which
//! needs a GTK main loop running on the thread that owns the icon. winit
//! already owns the main thread here, and the gtk3 bindings are
//! discontinued and pin an unsound glib (RUSTSEC-2024-0429). ksni is plain
//! D-Bus: no toolkit, no second event loop.
//!
//! D-Bus also means the icon does not care which display server is running
//! — one code path for X11 and Wayland — and StatusNotifierItem is what the
//! desktops consume today: KDE Plasma, Cinnamon, Budgie, LXQt, Unity and
//! xfce4-panel 4.18+ host it natively, GNOME through the AppIndicator
//! extension (shipped and enabled by default on Ubuntu), Wayland bars such
//! as waybar through their tray module. The gap is the pre-SNI XEmbed
//! system tray (xfce4-panel < 4.18, bare i3bar): an X11-only protocol that
//! would need exactly the GTK stack above.
//!
//! No shell speaks it -> no `org.kde.StatusNotifierWatcher` on the bus ->
//! [`is_active`] stays false and Hydra keeps behaving like a plain windowed
//! app (closing the window quits) instead of vanishing into a tray that
//! isn't there. The service stays subscribed either way, so a watcher that
//! shows up later (GNOME Shell restart on Xorg, extension enabled after
//! launch, login racing the shell) is picked up without a restart.

#![cfg(target_os = "linux")]

use super::Entry;
use crate::dl_indicator::IconFrame;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, OfflineReason, ToolTip};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

/// A watcher is registered and the icon is (or will be) on a panel.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// `watcher_offline` fired — including the soft failure `assume_sni_available`
/// turns the initial "no watcher on the bus" into, which happens before
/// `spawn()` returns and so must not be overwritten by it.
static OFFLINE: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
/// Current menu. The tray thread reads it whenever a host asks for the
/// layout, so a rebuild is "swap this, then nudge the service".
static MODEL: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// Menu-rebuild requests for the tray thread, which owns the ksni handle.
static REFRESH: OnceLock<mpsc::Sender<()>> = OnceLock::new();
/// Current glyph color: white for dark panels. Kept current by the theme
/// watcher; `icon_pixmap` reads it on every host request.
static WHITE: AtomicBool = AtomicBool::new(true);
/// The download arrow shown instead of the glyph; `icon_pixmap` reads it.
/// With whether it is animated.
static FRAME: Mutex<(IconFrame, bool)> = Mutex::new((IconFrame::Idle, false));
/// Sheen position of the animated arrow, advanced by the animation thread.
static PHASE: AtomicU8 = AtomicU8::new(0);
/// The animation thread, parked while there is nothing to animate.
static ANIMATOR: OnceLock<Option<std::thread::Thread>> = OnceLock::new();

struct HydraTray;

impl ksni::Tray for HydraTray {
    fn id(&self) -> String {
        // Equal to the .desktop basename so the shell can tie the item back
        // to the application entry.
        "hydra".into()
    }

    fn title(&self) -> String {
        crate::i18n::tr("Hydra Download Manager")
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::log::debug(&format!(
            "tray: host fetched icon pixmap (white={})",
            WHITE.load(Ordering::Relaxed)
        ));
        // The monochrome silhouette, not the color logo: panel icons on
        // Linux follow the same menu-bar convention as macOS and Windows,
        // and the color mark turns to mud at 22 px on a dark top bar. The
        // white/black choice tracks the panel (see `panel_wants_white`);
        // pixmap only — naming the themed color icon here would override it.
        let white = WHITE.load(Ordering::Relaxed);
        match FRAME.lock().map(|f| *f) {
            // Panel sizes only: an animated arrow is re-sent several times
            // a second, and a 256 px pixmap is a quarter megabyte on the bus.
            Ok((frame, animate)) if frame != IconFrame::Idle => {
                let sheen = animate.then(|| PHASE.load(Ordering::Relaxed));
                [64u32, 32]
                    .iter()
                    .map(|&px| {
                        pixmap(
                            &crate::icons::download_rgba(frame, sheen, px, false),
                            px,
                            px,
                        )
                    })
                    .collect()
            }
            _ => pixmaps(white).clone(),
        }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Hydra".into(),
            description: crate::i18n::tr("Hydra Download Manager"),
            ..Default::default()
        }
    }

    /// Left click: bring the window back, matching the Windows tray.
    fn activate(&mut self, _x: i32, _y: i32) {
        crate::log::debug("tray: activate (left click)");
        let _ = crate::menubus::sender().send(super::ICON_CLICKED.into());
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        match MODEL.lock() {
            Ok(model) => {
                crate::log::debug(&format!(
                    "tray: host fetched menu ({} top-level entries)",
                    model.len()
                ));
                render(&model)
            }
            Err(_) => Vec::new(),
        }
    }

    fn watcher_online(&self) {
        OFFLINE.store(false, Ordering::Relaxed);
        ACTIVE.store(true, Ordering::Relaxed);
        crate::log::info("tray: status-notifier watcher back online");
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        OFFLINE.store(true, Ordering::Relaxed);
        ACTIVE.store(false, Ordering::Relaxed);
        crate::log::warn(&format!(
            "tray unavailable ({reason:?}): no StatusNotifierWatcher on the session bus. \
             KDE Plasma, Cinnamon, Budgie, LXQt and xfce4-panel 4.18+ serve one \
             natively; GNOME needs the AppIndicator extension \
             (gnome-shell-extension-appindicator, then enable \"AppIndicator and \
             KStatusNotifierItem Support\"); bar-only Wayland sessions need one \
             with SNI support (waybar, ironbar)."
        ));
        // Keep the service alive: it re-registers by itself when a watcher
        // appears.
        true
    }
}

/// D-Bus menu labels take "_" as a mnemonic marker ("__" prints one
/// underscore), so a queue named `my_queue` would otherwise lose it.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

fn send(id: &str) -> Box<dyn Fn(&mut HydraTray) + Send> {
    let id = id.to_string();
    Box::new(move |_| {
        let _ = crate::menubus::sender().send(id.clone());
    })
}

fn render(entries: &[Entry]) -> Vec<MenuItem<HydraTray>> {
    entries
        .iter()
        .map(|e| match e {
            Entry::Separator => MenuItem::Separator,
            Entry::Item { id, label } => StandardItem {
                label: escape(label),
                activate: send(id),
                ..Default::default()
            }
            .into(),
            Entry::Check { id, label, checked } => CheckmarkItem {
                label: escape(label),
                checked: *checked,
                activate: send(id),
                ..Default::default()
            }
            .into(),
            Entry::Sub { label, items } => SubMenu {
                label: escape(label),
                submenu: render(items),
                ..Default::default()
            }
            .into(),
        })
        .collect()
}

fn set_model(entries: Vec<Entry>) {
    if let Ok(mut model) = MODEL.lock() {
        *model = entries;
    }
}

pub fn install(entries: Vec<Entry>) {
    set_model(entries);
    // Idempotent, and the reason `install` is safe to call on every window
    // open: after the first call it is exactly a menu refresh.
    notify();
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let (tx, rx) = mpsc::channel::<()>();
    // Published before the thread starts, so a rebuild that lands while the
    // D-Bus registration is still in flight queues up instead of being lost.
    let _ = REFRESH.set(tx);
    // Its own thread because registration is a blocking D-Bus round trip
    // (and can sit through a timeout when the bus is unhealthy) — never
    // something to do on the render thread.
    let spawned = std::thread::Builder::new()
        .name("hydra-tray".into())
        .spawn(move || {
            watch_theme();
            // `assume_sni_available`: a missing watcher becomes a
            // `watcher_offline` callback the service recovers from, instead
            // of a hard error that would give up for the whole session — the
            // shell often finishes starting after an autostarted Hydra.
            match HydraTray.assume_sni_available(true).spawn() {
                Ok(handle) => {
                    if !OFFLINE.load(Ordering::Relaxed) {
                        ACTIVE.store(true, Ordering::Relaxed);
                        crate::log::info("tray registered (StatusNotifierItem)");
                    }
                    // ksni serves the item on its own executor; this thread
                    // exists to forward menu rebuilds and does nothing else.
                    while rx.recv().is_ok() {
                        let _ = handle.update(|_| {});
                    }
                }
                Err(e) => crate::log::warn(&format!("tray unavailable: {e}")),
            }
        });
    if let Err(e) = spawned {
        crate::log::warn(&format!("tray thread failed to start: {e}"));
        STARTED.store(false, Ordering::SeqCst);
    }
}

pub fn reinstall(entries: Vec<Entry>) {
    set_model(entries);
    notify();
}

/// Tell the tray thread the model changed. Hosts cache the layout, and
/// ksni's `update` is what makes it re-emit one.
fn notify() {
    if let Some(tx) = REFRESH.get() {
        let _ = tx.send(());
    }
}

pub fn set_progress(frame: IconFrame, animate: bool) {
    let changed = FRAME
        .lock()
        .map(|mut f| std::mem::replace(&mut *f, (frame, animate)) != (frame, animate))
        .unwrap_or(false);
    if !changed {
        return;
    }
    notify();
    if animate && frame != IconFrame::Idle {
        if let Some(animator) = ANIMATOR.get_or_init(spawn_animator) {
            animator.unpark();
        }
    }
}

/// Step the sheen while an animated arrow is up; parked otherwise. Its own
/// thread, so a step is a D-Bus icon update and never an iced message.
fn spawn_animator() -> Option<std::thread::Thread> {
    let interval =
        std::time::Duration::from_millis(u64::from(crate::dl_indicator::SHEEN_INTERVAL_MS));
    std::thread::Builder::new()
        .name("hydra-tray-anim".into())
        .spawn(move || loop {
            let moving = FRAME
                .lock()
                .map(|f| f.1 && f.0 != IconFrame::Idle)
                .unwrap_or(false);
            if !moving {
                std::thread::park();
                continue;
            }
            std::thread::sleep(interval);
            let next = (PHASE.load(Ordering::Relaxed) + 1) % crate::dl_indicator::SHEEN_PHASES;
            PHASE.store(next, Ordering::Relaxed);
            notify();
        })
        .ok()
        .map(|handle| handle.thread().clone())
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Whether the panel this icon sits on wants a white glyph.
///
/// GNOME-family shells (Ubuntu's Yaru included) draw the top bar dark in
/// BOTH color schemes, so the scheme alone would put a black glyph on a
/// black bar; treat those desktops as dark panels outright. Elsewhere (KDE,
/// Xfce, LXQt, MATE) panels follow the theme, so ask the XDG portal for the
/// color scheme — the same system-theme pick the Windows backend makes.
fn panel_wants_white() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ["gnome", "unity", "cinnamon", "budgie", "pantheon"]
        .iter()
        .any(|d| desktop.contains(d))
    {
        return true;
    }
    matches!(dark_light::detect(), Ok(dark_light::Mode::Dark))
}

/// Set the glyph color for the current theme, and (once) subscribe to the
/// portal's change stream so a light/dark switch re-tints the icon live —
/// hosts re-fetch the pixmap on the NewIcon signal ksni emits when it
/// changes. Runs on the tray thread: the portal calls are blocking D-Bus.
fn watch_theme() {
    let white = panel_wants_white();
    crate::log::info(&format!(
        "tray: glyph {} (desktop {:?}, scheme {:?})",
        if white { "white" } else { "black" },
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        dark_light::detect()
            .map(|m| format!("{m:?}"))
            .unwrap_or_else(|e| format!("err: {e}")),
    ));
    WHITE.store(white, Ordering::Relaxed);
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let Ok(watcher) = dark_light::subscribe() else {
        return; // no portal: keep the startup pick, like Windows
    };
    let _ = std::thread::Builder::new()
        .name("hydra-tray-theme".into())
        .spawn(move || {
            for mode in watcher.iter() {
                let white = panel_wants_white();
                if WHITE.swap(white, Ordering::Relaxed) != white {
                    crate::log::info(&format!(
                        "tray: theme changed ({mode:?}), glyph now {}",
                        if white { "white" } else { "black" }
                    ));
                    notify();
                }
            }
        });
}

/// The mono silhouette as SNI pixmaps: ARGB32, network byte order,
/// non-premultiplied (what both GdkPixbuf and QImage::Format_ARGB32
/// expect). Several sizes so the host can pick one near the panel height
/// instead of scaling 256 px down to 22. Both variants are cached; the
/// theme watcher only changes which one `icon_pixmap` serves.
fn pixmaps(white: bool) -> &'static Vec<ksni::Icon> {
    static WHITE_ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    static BLACK_ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    let cell = if white { &WHITE_ICONS } else { &BLACK_ICONS };
    cell.get_or_init(|| {
        crate::icons::logo_mono_rgba(white)
            .map(|(rgba, w, h)| scaled_pixmaps(&rgba, w, h))
            .unwrap_or_default()
    })
}

fn scaled_pixmaps(rgba: &[u8], w: u32, h: u32) -> Vec<ksni::Icon> {
    // 256 -> 32 and 64 are exact box averages; anything not divisible is
    // left to the host.
    [8usize, 4, 1]
        .iter()
        .filter_map(|&factor| {
            let (px, pw, ph) = if factor == 1 {
                (rgba.to_vec(), w, h)
            } else if (w as usize).is_multiple_of(factor) && (h as usize).is_multiple_of(factor) {
                (
                    box_downscale(rgba, w as usize, h as usize, factor),
                    w / factor as u32,
                    h / factor as u32,
                )
            } else {
                return None;
            };
            Some(pixmap(&px, pw, ph))
        })
        .collect()
}

/// Straight RGBA as an SNI pixmap (ARGB32, network byte order).
fn pixmap(rgba: &[u8], width: u32, height: u32) -> ksni::Icon {
    let mut argb = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0 {
        argb.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
    }
    ksni::Icon {
        width: width as i32,
        height: height as i32,
        data: argb,
    }
}

/// Average `factor`x`factor` blocks. Alpha-weighted on the colour channels so
/// transparent pixels do not bleed their (undefined) colour into the edges.
fn box_downscale(rgba: &[u8], w: usize, h: usize, factor: usize) -> Vec<u8> {
    let (ow, oh) = (w / factor, h / factor);
    let mut out = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        for ox in 0..ow {
            let (mut acc, mut alpha) = ([0u64; 3], 0u64);
            for y in oy * factor..(oy + 1) * factor {
                for x in ox * factor..(ox + 1) * factor {
                    let p = &rgba[(y * w + x) * 4..][..4];
                    let a = p[3] as u64;
                    for (c, v) in acc.iter_mut().zip(&p[..3]) {
                        *c += *v as u64 * a;
                    }
                    alpha += a;
                }
            }
            // A fully transparent block has no colour to average, and
            // `checked_div` lands on the transparent pixel it should be.
            let n = (factor * factor) as u64;
            out.extend_from_slice(&[
                acc[0].checked_div(alpha).unwrap_or(0) as u8,
                acc[1].checked_div(alpha).unwrap_or(0) as u8,
                acc[2].checked_div(alpha).unwrap_or(0) as u8,
                (alpha / n) as u8,
            ]);
        }
    }
    out
}

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless tiny-skia harness: runs the real views through iced's
//! `UserInterface` and rasterizes them the way the software compositor does,
//! into a framebuffer that keeps its pixels between frames and is repainted
//! only where the layer diff says, like a surface whose buffer age is 1.
//!
//! The tests redraw every frame from scratch as well and demand the damaged
//! framebuffer match it — a difference is a stale pixel the user would see.
//! Set `HYDRA_RENDER_DUMP=<dir>` to get both frames as PNGs when one fails.
//! The `#[ignore]`d profile times each stage per frame:
//! `cargo test -p hya-gui --release render_check -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use iced::advanced::clipboard;
use iced::advanced::renderer::Style;
use iced::{mouse, window, Event, Point, Rectangle, Size};
use iced_graphics::{damage, Viewport};
use iced_renderer::fallback;
use iced_runtime::user_interface::{self, UserInterface};
use iced_tiny_skia::Layer;

use crate::app::{App, WinKind};
use crate::model::{Column, ColumnPref, DlState, DownloadItem};

/// Largest per-channel difference a damaged frame may keep from a full
/// repaint: redrawing an anti-aliased edge inside a clip can round a level
/// off. A stale glyph or rule is tens of levels.
const VISIBLE_DELTA: u32 = 2;

#[derive(Default, Clone, Copy)]
struct Stages {
    update: Duration,
    view: Duration,
    draw: Duration,
    raster: Duration,
    damaged: f64,
}

struct Harness {
    app: App,
    id: window::Id,
    logical: Size,
    scale: f32,
    renderer: iced::Renderer,
    cache: user_interface::Cache,
    cursor: mouse::Cursor,
    shown: Vec<u32>,
    clip: tiny_skia::Mask,
    last: Option<Vec<Layer>>,
    verify: bool,
    /// Top of the first data row in the main window, found by
    /// [`main_window`]: the toolbar above it is taller where the menu bar
    /// lives in the window rather than in the system's.
    list_top: f32,
    frames: Vec<Stages>,
    mismatches: Vec<String>,
}

impl Harness {
    fn new(mut app: App, kind: WinKind, logical: Size, scale: f32, verify: bool) -> Self {
        load_bundled_font();
        let id = window::Id::unique();
        app.windows.insert(id, kind);
        if kind == WinKind::Main {
            app.main_id = Some(id);
            app.main_size = logical;
        }
        let phys = physical(logical, scale);
        Self {
            app,
            id,
            logical,
            scale,
            renderer: fallback::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                iced::Font::default(),
                iced::Pixels(16.0),
            )),
            cache: user_interface::Cache::default(),
            cursor: mouse::Cursor::Unavailable,
            shown: vec![0; (phys.width * phys.height) as usize],
            clip: tiny_skia::Mask::new(phys.width, phys.height).expect("clip mask"),
            last: None,
            verify,
            list_top: 0.0,
            frames: vec![],
            mismatches: vec![],
        }
    }

    /// One trip through the shell's loop: deliver `events` and apply the
    /// messages they produce, then rebuild, draw and rasterize.
    fn step(&mut self, label: &str, events: &[Event]) {
        let mut s = Stages::default();
        for e in events {
            if let Event::Mouse(mouse::Event::CursorMoved { position }) = e {
                self.cursor = mouse::Cursor::Available(*position);
            }
        }
        if !events.is_empty() {
            let t = Instant::now();
            let cursor = self.cursor;
            let mut messages = vec![];
            let mut ui = UserInterface::build(
                crate::view(&self.app, self.id),
                self.logical,
                std::mem::take(&mut self.cache),
                &mut self.renderer,
            );
            let _ = ui.update(
                events,
                cursor,
                &mut self.renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            self.cache = ui.into_cache();
            for m in messages {
                let _ = self.app.update(m);
            }
            s.update = t.elapsed();
        }

        let theme = crate::theme_of(&self.app, self.id);
        let style = crate::style_of(&self.app, &theme);
        let cursor = self.cursor;
        let t = Instant::now();
        let mut ui = UserInterface::build(
            crate::view(&self.app, self.id),
            self.logical,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        s.view = t.elapsed();
        let t = Instant::now();
        ui.draw(
            &mut self.renderer,
            &theme,
            &Style {
                text_color: style.text_color,
            },
            cursor,
        );
        s.draw = t.elapsed();
        self.cache = ui.into_cache();

        let t = Instant::now();
        let fallback::Renderer::Secondary(ts) = &mut self.renderer else {
            unreachable!("the harness only builds the tiny-skia renderer")
        };
        let viewport = Viewport::with_physical_size(physical(self.logical, self.scale), self.scale);
        let phys = viewport.physical_size();
        let whole = Rectangle::with_size(viewport.logical_size());
        let layers = ts.layers().to_vec();
        let damage = match &self.last {
            Some(last) => damage::diff(last, &layers, |l| vec![l.bounds], Layer::damage),
            None => vec![whole],
        };
        if !damage.is_empty() {
            let damage = damage::group(damage, whole);
            s.damaged =
                damage.iter().map(|r| f64::from(r.area())).sum::<f64>() / f64::from(whole.area());
            ts.draw(
                &mut pixmap(&mut self.shown, phys),
                &mut self.clip,
                &viewport,
                &damage,
                style.background_color,
            );
        }
        s.raster = t.elapsed();
        self.last = Some(layers);

        if self.verify {
            let mut fresh = vec![0u32; self.shown.len()];
            let mut clip = tiny_skia::Mask::new(phys.width, phys.height).expect("clip mask");
            ts.draw(
                &mut pixmap(&mut fresh, phys),
                &mut clip,
                &viewport,
                &[whole],
                style.background_color,
            );
            if let Some(msg) = compare(&self.shown, &fresh, phys) {
                self.mismatches.push(format!("{label}: {msg}"));
                dump(label, &self.shown, &fresh, phys);
            }
        }
        self.frames.push(s);
    }

    fn hover(&mut self, label: &str, p: Point) {
        self.step(
            label,
            &[Event::Mouse(mouse::Event::CursorMoved { position: p })],
        );
    }

    fn assert_clean(&self, what: &str) {
        assert!(
            self.mismatches.is_empty(),
            "{what} at scale {}: damaged frames diverged from a full repaint:\n{}",
            self.scale,
            self.mismatches.join("\n")
        );
    }

    fn report(&self, name: &str) {
        let frames = &self.frames[1.min(self.frames.len())..];
        if frames.is_empty() {
            return;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let stat = |f: fn(&Stages) -> Duration| {
            let mut v: Vec<Duration> = frames.iter().map(f).collect();
            v.sort();
            let avg = v.iter().sum::<Duration>() / v.len() as u32;
            format!(
                "{:>6.2}/{:>6.2}",
                ms(avg),
                ms(v[(v.len() * 95 / 100).min(v.len() - 1)])
            )
        };
        let total = frames
            .iter()
            .map(|s| ms(s.update + s.view + s.draw + s.raster))
            .sum::<f64>()
            / frames.len() as f64;
        let damaged = frames.iter().map(|s| s.damaged).sum::<f64>() / frames.len() as f64;
        println!(
            "{name:<26} {:>3} frames {total:>7.2} ms | update {} view+layout {} draw {} raster {} (avg/p95 ms) | damage {:>5.1}%",
            frames.len(),
            stat(|s| s.update),
            stat(|s| s.view),
            stat(|s| s.draw),
            stat(|s| s.raster),
            damaged * 100.0
        );
    }
}

fn load_bundled_font() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        iced_graphics::text::font_system()
            .write()
            .expect("font system")
            .load_font(std::borrow::Cow::Borrowed(include_bytes!(
                "../assets/fonts/Vazirmatn-Regular.ttf"
            )));
    });
}

fn physical(logical: Size, scale: f32) -> Size<u32> {
    Size::new(
        (logical.width * scale).round() as u32,
        (logical.height * scale).round() as u32,
    )
}

fn pixmap(buf: &mut [u32], size: Size<u32>) -> tiny_skia::PixmapMut<'_> {
    tiny_skia::PixmapMut::from_bytes(bytemuck::cast_slice_mut(buf), size.width, size.height)
        .expect("pixmap")
}

fn compare(shown: &[u32], fresh: &[u32], size: Size<u32>) -> Option<String> {
    let w = size.width as usize;
    let (mut n, mut worst) = (0usize, 0u32);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, (a, b)) in shown.iter().zip(fresh).enumerate() {
        let delta = a
            .to_le_bytes()
            .iter()
            .zip(b.to_le_bytes())
            .map(|(p, q)| u32::from(p.abs_diff(q)))
            .max()
            .unwrap_or(0);
        if delta > VISIBLE_DELTA {
            worst = worst.max(delta);
            n += 1;
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    (n > 0)
        .then(|| format!("{n} px differ in ({x0},{y0})-({x1},{y1}), worst channel delta {worst}"))
}

/// Saves the damaged and the full frame as PNGs. The first frame to carry a
/// label wins: a stale pixel stays wrong in every frame after the one that
/// left it, and that one is the frame worth looking at.
fn dump(label: &str, shown: &[u32], fresh: &[u32], size: Size<u32>) {
    let Some(dir) = std::env::var_os("HYDRA_RENDER_DUMP") else {
        return;
    };
    let name = label.replace([' ', '/'], "_");
    for (buf, suffix) in [(shown, "damaged"), (fresh, "full")] {
        let path = std::path::Path::new(&dir).join(format!("{name}-{suffix}.png"));
        if path.exists() {
            continue;
        }
        let mut pm = tiny_skia::Pixmap::new(size.width, size.height).expect("pixmap");
        // The framebuffer is softbuffer's 0RGB; PNG wants RGBA.
        for (dst, px) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(buf) {
            let [b, g, r, _] = px.to_le_bytes();
            dst.copy_from_slice(&[r, g, b, 255]);
        }
        let _ = pm.save_png(path);
    }
}

fn download(i: u64) -> DownloadItem {
    let names = [
        "ubuntu-24.04.1-desktop-amd64.iso",
        "Big.Buck.Bunny.2008.1080p.BluRay.x264.mkv",
        "setup.exe",
        "The quick brown fox — a very long file name that will not fit.zip",
        "رمان فارسی.pdf",
        "track 07.flac",
    ];
    let state = match i % 6 {
        0 | 1 => DlState::Complete,
        2 => DlState::Receiving,
        3 => DlState::Paused,
        4 => DlState::Error,
        _ => DlState::Queued,
    };
    let size = 1_000_000 + i * 7_654_321;
    DownloadItem {
        id: i,
        url: format!("https://example.com/{i}"),
        file_name: format!("{i:04} {}", names[i as usize % names.len()]),
        save_dir: "/tmp".into(),
        category: None,
        description: if i.is_multiple_of(3) {
            "from the browser".into()
        } else {
            String::new()
        },
        size: Some(size),
        downloaded: if state == DlState::Complete {
            size
        } else {
            size / 3
        },
        state,
        error: (state == DlState::Error).then(|| "connection reset".into()),
        resume: Some(true),
        added: 0,
        last_try: Some(1_790_000_000 + i as i64 * 3600),
        queue: i.is_multiple_of(5).then(|| "Main Download Queue".into()),
        q_order: i as u32,
        auth: None,
        cookies: None,
        cookie_source: None,
        referer: None,
        speed_limit: None,
        limit_paused: false,
        held: vec![],
        part_path: None,
        rate: if state == DlState::Receiving {
            1_234_567.0
        } else {
            0.0
        },
        retries: 3,
        disp_progress: 0.0,
        eta_secs: (state == DlState::Receiving).then_some(125),
        recorded_secs: None,
        conns: vec![],
        plugin_details: vec![],
        status_line: String::new(),
        shutdown_after: false,
        shutdown_action: Default::default(),
        stream: None,
        plugin_plan: None,
        metalink: None,
        name_locked: false,
        proxy: Default::default(),
    }
}

fn app_with(downloads: u64) -> App {
    let mut app = App::default();
    // What `load_config` puts in place of an empty column list.
    app.cfg.settings.columns = Column::ALL.into_iter().map(ColumnPref::new).collect();
    app.state.downloads = (0..downloads).map(download).collect();
    app
}

fn main_window(downloads: u64, logical: Size, scale: f32, verify: bool) -> Harness {
    let mut h = Harness::new(app_with(downloads), WinKind::Main, logical, scale, verify);
    h.step("first", &[]);
    h.list_top = (0..logical.height as u32)
        .step_by(2)
        .map(|y| y as f32)
        .find(|&y| {
            h.hover("find the list", Point::new(LIST_X, y));
            h.app.hover_row.is_some()
        })
        .expect("no row of the list reacts to the pointer");
    h
}

/// A column inside the list, clear of the category tree on its left.
const LIST_X: f32 = 420.0;

fn row_point(h: &Harness, row: usize) -> Point {
    Point::new(LIST_X, h.list_top + 25.0 * row as f32 + 12.0)
}

fn sweep(h: &mut Harness, rows: usize) {
    for r in 0..rows {
        let p = row_point(h, r);
        h.hover(&format!("hover row {r}"), p);
    }
}

fn tick_progress(h: &mut Harness, frames: usize) {
    for f in 0..frames {
        for d in h
            .app
            .state
            .downloads
            .iter_mut()
            .filter(|d| d.state == DlState::Receiving)
        {
            d.downloaded += 123_457;
            d.disp_progress = d.progress();
            d.rate = 1_000_000.0 + (f as f64 * 7919.0) % 900_000.0;
            d.eta_secs = Some(300 - f as u64 % 300);
        }
        h.step(&format!("progress {f}"), &[]);
    }
}

fn scroll(h: &mut Harness, steps: usize) {
    let p = row_point(h, 3);
    h.hover("scroll start", p);
    for s in 0..steps {
        h.step(
            &format!("scroll {s}"),
            &[Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -37.0 },
            })],
        );
    }
}

/// The checks below prove nothing if the simulated pointer misses the list.
#[test]
fn the_simulated_pointer_hovers_and_scrolls_the_real_list() {
    let mut h = main_window(60, Size::new(1100.0, 700.0), 1.0, false);
    let p = row_point(&h, 2);
    h.hover("row 2", p);
    assert_eq!(
        h.app.hover_row,
        Some(57),
        "the third row of the default sort"
    );
    assert!(h.frames.last().is_some_and(|f| f.damaged > 0.0));

    scroll(&mut h, 5);
    assert!(h.app.table_scroll > 0.0, "the wheel reached the list");
}

#[test]
fn the_main_window_repaints_cleanly_across_hover_progress_and_scroll() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut h = main_window(60, Size::new(1100.0, 700.0), scale, true);
        sweep(&mut h, 20);
        tick_progress(&mut h, 10);
        scroll(&mut h, 15);
        sweep(&mut h, 10);
        h.assert_clean("main window");
    }
}

#[test]
fn the_progress_window_repaints_cleanly_while_a_download_runs() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let mut h = Harness::new(
            app_with(6),
            WinKind::Progress(2),
            Size::new(520.0, 420.0),
            scale,
            true,
        );
        h.step("first", &[]);
        tick_progress(&mut h, 30);
        h.assert_clean("progress window");
    }
}

#[test]
#[ignore = "profile: run with --release --ignored --nocapture"]
fn profile_main_window() {
    for (w, hgt, scale) in [
        (1100.0, 700.0, 1.0),
        (1100.0, 700.0, 2.0),
        (1600.0, 1000.0, 1.0),
    ] {
        for n in [40, 200] {
            println!("--- main window {w}x{hgt} @{scale}x, {n} downloads");
            let size = Size::new(w, hgt);

            let mut h = main_window(n, size, scale, false);
            for _ in 0..20 {
                h.last = None;
                h.step("full", &[]);
            }
            h.report("full repaint");

            let mut h = main_window(n, size, scale, false);
            sweep(&mut h, 20);
            sweep(&mut h, 20);
            h.report("hover sweep");

            let mut h = main_window(n, size, scale, false);
            tick_progress(&mut h, 40);
            h.report("progress tick");

            let mut h = main_window(n, size, scale, false);
            scroll(&mut h, 40);
            h.report("wheel scroll");
        }
    }
}

#[test]
fn ffmpeg_path_and_badge_repaint_after_browse_and_reset() {
    use crate::app::{Message, OptField, OptTab};

    let mut app = app_with(0);
    app.options.tab = OptTab::MediaTools;
    let mut h = Harness::new(app, WinKind::Options, Size::new(900.0, 660.0), 1.0, true);
    let executable = std::env::current_exe().unwrap().display().to_string();
    for (label, field) in [
        ("ffmpeg-selected", OptField::FfmpegPicked(Some(executable))),
        (
            "ffmpeg-missing",
            OptField::FfmpegPicked(Some("F:/Portable/Tools/ffmpeg.exe".into())),
        ),
        ("ffmpeg-reset", OptField::FfmpegPath(String::new())),
    ] {
        let _ = h.app.update(Message::OptDraft(field));
        h.step(label, &[]);
        dump(label, &h.shown, &h.shown, physical(h.logical, h.scale));
    }
    h.assert_clean("FFmpeg path and badge");
}

#[test]
fn plugin_list_settings_and_info_render() {
    use crate::app::OptTab;
    use crate::plugins::Detail;
    use hya_plugin_api::{Field, FieldKind, Manifest, Value};
    let mut app = app_with(0);
    app.options.tab = OptTab::Plugins;
    let mut manifest: Manifest = toml::from_str(include_str!(
        "../../../plugins/hydra-youtube/hydra-plugin.toml"
    ))
    .unwrap();
    manifest.settings = [
        (
            FieldKind::Text,
            "Output prefix",
            Value::Text("Video".into()),
        ),
        (FieldKind::Choice, "Quality", Value::Text("Best".into())),
        (FieldKind::Number, "Maximum results", Value::Number(10.0)),
        (FieldKind::Radio, "Container", Value::Text("MP4".into())),
        (FieldKind::Checkbox, "Save subtitles", Value::Bool(true)),
        (FieldKind::Bool, "Use cookies", Value::Bool(false)),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (kind, label, value))| Field {
        key: format!("setting{i}"),
        label: label.into(),
        kind,
        default: Some(value),
        help: None,
        options: if kind == FieldKind::Radio {
            vec!["MP4".into(), "WebM".into()]
        } else {
            vec!["Best".into(), "720p".into()]
        },
    })
    .collect();
    app.options
        .plugins
        .installed
        .push(hya_plugin::manager::Installed {
            grants: manifest.permissions.clone(),
            manifest,
            directory: "/plugins/youtube".into(),
            dev: false,
            signing: Default::default(),
            enabled: true,
            pins: Default::default(),
            settings: Default::default(),
            failures: 0,
            module_sha256: String::new(),
            native_sha256: Default::default(),
            previous: None,
        });
    app.options.plugins.indexes = vec![
        hya_plugin::distribution::IndexSource {
            url: hya_plugin::distribution::DEFAULT_INDEX_URL.into(),
            key: None,
        },
        hya_plugin::distribution::IndexSource {
            url: "https://example.com/plugins.json".into(),
            key: None,
        },
    ];
    let mut h = Harness::new(app, WinKind::Options, Size::new(760.0, 700.0), 1.0, true);
    let settings = h.app.options.plugins.installed[0].manifest.settings.clone();
    for (mode, name) in [
        (crate::model::ThemeMode::Light, "light"),
        (crate::model::ThemeMode::Dark, "dark"),
    ] {
        h.app.cfg.settings.theme_mode = Some(mode);
        h.app.options.plugins.installed[0].manifest.settings = settings.clone();
        h.app.options.plugins.error = None;
        for (label, detail) in [
            ("plugins-list", None),
            (
                "plugins-settings",
                Some(Detail::Settings("hydra.youtube".into())),
            ),
            ("plugins-info", Some(Detail::Info("hydra.youtube".into()))),
            ("plugins-logs", Some(Detail::Logs("hydra.youtube".into()))),
        ] {
            h.app.options.plugins.installed[0].signing = if label == "plugins-info" {
                hya_plugin::package::Signing::Verified {
                    fingerprint: "b567c8a9f94ea0a5".into(),
                }
            } else {
                hya_plugin::package::Signing::Unsigned
            };
            h.app.options.plugins.detail = detail;
            h.step(label, &[]);
            dump(
                &format!("{label}-{name}"),
                &h.shown,
                &h.shown,
                physical(h.logical, h.scale),
            );
        }
        h.app.options.plugins.detail = None;
        let mut update = hya_plugin::distribution::parse_index(
            include_bytes!("../../../docs/plugins.json"),
            None,
            None,
        )
        .unwrap()
        .plugins
        .remove(0);
        update.version = "0.2.0".into();
        h.app.options.plugins.updates = vec![update];
        h.step("plugins-update-badge", &[]);
        dump(
            &format!("plugins-update-badge-{name}"),
            &h.shown,
            &h.shown,
            physical(h.logical, h.scale),
        );
        h.app.options.plugins.busy = true;
        h.step("plugins-update-busy", &[]);
        h.app.options.plugins.busy = false;
        h.app.options.plugins.updates.clear();
        h.step("plugins-update-cleared", &[]);
        h.app.options.plugins.installed[0].signing = hya_plugin::package::Signing::Unsigned;
        h.app.options.plugins.detail = Some(Detail::Info("hydra.youtube".into()));
        h.step("plugins-info-unsigned", &[]);
        h.app.options.plugins.detail = None;
        h.app.options.plugins.review = Some(h.app.options.plugins.installed[0].manifest.clone());
        h.step("plugins-install-review", &[]);
        h.app.options.plugins.permission_details = true;
        h.step("plugins-permission-details", &[]);
        h.app.options.plugins.permission_details = false;
        dump(
            &format!("plugins-review-{name}"),
            &h.shown,
            &h.shown,
            physical(h.logical, h.scale),
        );
        h.app.options.plugins.error = Some("The package could not be installed.".into());
        h.step("plugins-install-error", &[]);
        h.app.options.plugins.busy = true;
        h.step("plugins-install-busy", &[]);
        h.app.options.plugins.busy = false;
        h.app.options.plugins.review = None;
        h.app.options.plugins.welcome = Some((
            "YouTube resolver".into(),
            "Install yt-dlp and ffmpeg, then choose quality or audio output.".into(),
        ));
        h.app.options.plugins.detail = None;
        h.step("plugins-welcome", &[]);
        h.app.options.plugins.welcome = None;
        h.app.options.plugins.detail = Some(Detail::Settings("hydra.youtube".into()));
        h.app.options.plugins.installed[0].manifest.settings.clear();
        h.step("plugins-no-settings", &[]);
        h.assert_clean("plugin settings navigation");
    }
}

#[test]
fn add_url_optional_status_rows_render_without_empty_panels() {
    for (name, probing, error) in [
        ("add-url-empty", false, false),
        ("add-url-reading", true, false),
        ("add-url-failure", false, true),
    ] {
        let mut app = app_with(0);
        app.add_url.address = "https://example.com/video".into();
        app.add_url.plugin_probing = probing;
        app.add_url.stream_probing = probing;
        app.add_url.metalink_probing = probing;
        app.add_url.cookies_importing = probing;
        if error {
            app.add_url.cookie_note = Some("No browser cookies were found.".into());
            app.add_url.error = Some("The plugin could not resolve this address.".into());
        }
        let mut h = Harness::new(app, WinKind::AddUrl, Size::new(760.0, 300.0), 1.0, true);
        h.step(name, &[]);
        h.assert_clean(name);
    }
}

#[test]
fn plugin_file_actions_and_native_file_selection_render_in_add_url() {
    let mut app = app_with(0);
    app.options.plugins.installed = vec![crate::plugins::tests::file_plugin(
        "example.x",
        "Browse X File",
        "x",
    )];
    app.add_url.address = "file:///tmp/input.x".into();
    app.add_url.plugin_plan = Some(crate::plugins::tests::transfer_plan());
    let mut harness = Harness::new(app, WinKind::AddUrl, Size::new(760.0, 360.0), 1.0, true);
    harness.step("native-file-input", &[]);
    harness.assert_clean("native-file-input");
}

#[test]
fn plugin_connections_render_the_plugins_columns_and_rows() {
    let mut app = app_with(1);
    let id = app.state.downloads[0].id;
    let mut info = crate::plugins::tests::transfer_plan();
    info.plan.transfer.as_mut().unwrap().details = Some(hya_plugin_api::TransferDetails {
        title: "Peer connections".into(),
        columns: vec!["Peer".into(), "Downloaded".into(), "Client".into()],
    });
    app.state.downloads[0].plugin_plan = Some(info);
    let _ = app.update(crate::app::Message::Engine(
        crate::engine::Event::PluginDetails {
            id,
            rows: vec![vec![
                "127.0.0.1:6881".into(),
                "16 KB".into(),
                "Test peer".into(),
            ]],
        },
    ));
    let mut harness = Harness::new(
        app,
        WinKind::Progress(id),
        Size::new(680.0, 616.0),
        1.0,
        true,
    );
    harness.step("plugin-peer-details", &[]);
    harness.assert_clean("plugin-peer-details");
}

#[test]
fn add_url_keeps_long_extractor_errors_scrollable() {
    let error = "tool_failed: hydra.youtube: exited with 1: WARNING: [youtube] No title found in player responses; falling back to title from initial data. Other metadata may also be missing\nERROR: [youtube] EKkzbbLYPuI: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication. See https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp for how to manually pass cookies.";
    let height = crate::windows::add_url::error_panel_height(error);
    assert!(height > crate::windows::add_url::error_panel_height("Invalid URL"));
    assert_eq!(
        crate::windows::add_url::error_panel_height(&error.repeat(10)),
        height
    );
    for scale in [1.0, 2.0] {
        let mut app = app_with(0);
        app.add_url.address = "https://www.youtube.com/watch?v=EKkzbbLYPuI".into();
        app.add_url.error = Some(error.into());
        let mut h = Harness::new(
            app,
            WinKind::AddUrl,
            Size::new(760.0, 146.0 + height),
            scale,
            true,
        );
        h.step("youtube-inspection-error", &[]);
        dump(
            "youtube-inspection-error",
            &h.shown,
            &h.shown,
            physical(h.logical, h.scale),
        );
        h.app.add_url.error = Some(error.repeat(10));
        h.step("oversized-inspection-error", &[]);
        let before_scroll = h.shown.clone();
        h.step(
            "scroll-inspection-error",
            &[
                Event::Mouse(mouse::Event::CursorMoved {
                    position: iced::Point::new(300.0, 180.0),
                }),
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0.0, y: -3.0 },
                }),
            ],
        );
        assert_ne!(before_scroll, h.shown);
        h.assert_clean("YouTube inspection error");
    }
}

#[test]
fn address_loading_indicator_animates_and_clears_without_stale_pixels() {
    for scale in [1.0, 2.0] {
        let mut app = app_with(0);
        app.add_url.address = "https://www.youtube.com/playlist?list=example".into();
        app.add_url.plugin_probing = true;
        let mut h = Harness::new(app, WinKind::AddUrl, Size::new(760.0, 260.0), scale, true);
        h.step("address-loading", &[]);
        let first = h.shown.clone();
        let _ = h.app.update(crate::app::Message::AnimTick);
        h.step("address-loading-next-frame", &[]);
        assert_ne!(first, h.shown);
        dump(
            "address-loading",
            &h.shown,
            &h.shown,
            physical(h.logical, h.scale),
        );
        h.app.add_url.plugin_probing = false;
        h.step("address-loading-finished", &[]);
        assert_ne!(first, h.shown);
        h.assert_clean("address loading indicator");
    }
}

#[test]
fn plugin_dialog_fits_short_and_long_media_lists() {
    for (name, audio_only, entries, subtitles, long_title) in [
        ("plugin-video-no-subtitles", false, 0, 0, false),
        ("plugin-audio-no-subtitles", true, 0, 0, false),
        ("plugin-audio-track-without-video", false, 0, 0, false),
        ("plugin-long-title-many-subtitles", false, 0, 30, true),
        ("plugin-single-playlist-item", false, 1, 0, false),
        ("plugin-large-playlist", true, 50, 0, true),
    ] {
        let mut app = app_with(0);
        app.add_url.address = "https://www.youtube.com/watch?v=example".into();
        let mut info = crate::plugins::tests::plan();
        info.preferences.audio_only = audio_only;
        if name == "plugin-audio-track-without-video" {
            info.plan
                .tracks
                .retain(|track| track.kind != hya_plugin_api::TrackKind::Video);
        }
        info.plan
            .tracks
            .retain(|track| track.kind != hya_plugin_api::TrackKind::Subtitle);
        for i in 0..subtitles {
            let mut subtitle = crate::plugins::tests::plan().plan.tracks.pop().unwrap();
            subtitle.id = format!("subtitle-{i}");
            info.plan.tracks.push(subtitle);
        }
        if entries > 0 {
            info.plan.tracks.clear();
            info.plan.entries = (0..entries)
                .map(|i| hya_plugin_api::PlaylistEntry {
                    id: i.to_string(),
                    url: format!("https://example.com/video/{i}"),
                    title: Some(format!("Video {i}")),
                })
                .collect();
        }
        if long_title {
            info.plan.title =
                Some("A long YouTube video title with details and descriptions ".repeat(8));
        }
        app.add_url.plugin_plan = Some(info);
        let height = 146.0 + crate::windows::add_url::plugin_panel_height(&app.add_url);
        assert!(height < 500.0, "{name} should remain a compact dialog");
        let mut h = Harness::new(app, WinKind::AddUrl, Size::new(760.0, height), 1.0, true);
        h.step(name, &[]);
        dump(name, &h.shown, &h.shown, physical(h.logical, h.scale));
        h.assert_clean(name);
    }
}

#[test]
fn plugin_track_picker_and_subtitles_render() {
    let mut app = app_with(0);
    app.add_url.address = "https://example.com/video".into();
    app.add_url.plugin_plan = Some(crate::plugins::tests::plan());
    let height = 146.0 + crate::windows::add_url::plugin_panel_height(&app.add_url);
    let mut h = Harness::new(app, WinKind::AddUrl, Size::new(760.0, height), 1.0, true);
    h.step("plugin-tracks", &[]);
    dump(
        "plugin-tracks",
        &h.shown,
        &h.shown,
        physical(h.logical, h.scale),
    );
    h.app
        .add_url
        .plugin_plan
        .as_mut()
        .unwrap()
        .preferences
        .track_ids
        .push("s".into());
    h.step("plugin-subtitle-selected", &[]);
    h.assert_clean("plugin subtitle selection");
    let info = h.app.add_url.plugin_plan.as_mut().unwrap();
    info.preferences.audio_only = true;
    info.preferences.audio_format = Some("mp3".into());
    let height = 146.0 + crate::windows::add_url::plugin_panel_height(&h.app.add_url);
    let mut h = Harness::new(h.app, WinKind::AddUrl, Size::new(760.0, height), 1.0, true);
    h.step("plugin-audio-only", &[]);
    dump(
        "plugin-audio-only",
        &h.shown,
        &h.shown,
        physical(h.logical, h.scale),
    );
    h.hover("plugin-audio-picker-hover", Point::new(250.0, 224.0));
    h.assert_clean("plugin audio selection");
    let info = h.app.add_url.plugin_plan.as_mut().unwrap();
    info.plan.tracks.clear();
    info.plan.entries = (1..=5)
        .map(|i| hya_plugin_api::PlaylistEntry {
            id: i.to_string(),
            url: format!("https://example.com/watch?v={i}"),
            title: Some(format!("Playlist video {i}")),
        })
        .collect();
    let height = 146.0 + crate::windows::add_url::plugin_panel_height(&h.app.add_url);
    let mut h = Harness::new(h.app, WinKind::AddUrl, Size::new(760.0, height), 1.0, true);
    h.step("plugin-playlist-selection", &[]);
    dump(
        "plugin-playlist-selection",
        &h.shown,
        &h.shown,
        physical(h.logical, h.scale),
    );
    h.assert_clean("plugin track selection");
}

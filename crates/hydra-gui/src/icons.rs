// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vector icons: thin colourful gradient
//! outlines for the toolbar, small filled glyphs for the category tree.
//!
//! Icons are generated SVG strings (stroke paths over a two-stop gradient)
//! rather than shipped bitmap files so they scale with any DPI and the
//! disabled state is just a grey re-stroke of the same geometry.

use iced::widget::svg;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// ------------------------------------------------------------------ app logo

const LOGO_PNG: &[u8] = include_bytes!("../../../docs/logo.png");

/// docs/logo.png decoded to straight RGBA (shared by the tray icon and the
/// window/taskbar icon).
pub fn logo_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(LOGO_PNG));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

/// Monochrome silhouette of the logo, menu-bar style: opaque pixels become
/// the mark, the white "H" strokes become transparent cutouts so the glyph
/// stays readable at 16 px. `white` selects the variant for dark panels.
/// Shared by every tray backend: macOS templates it, Windows and Linux pick
/// the variant from the system theme.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn logo_mono_rgba(white: bool) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = logo_rgba()?;
    let v = if white { 255 } else { 0 };
    let mut out = vec![0u8; rgba.len()];
    for (src, dst) in rgba.chunks(4).zip(out.chunks_mut(4)) {
        let whiteish = src[0] > 220 && src[1] > 220 && src[2] > 220;
        dst[0] = v;
        dst[1] = v;
        dst[2] = v;
        dst[3] = if whiteish { 0 } else { src[3] };
    }
    Some((out, w, h))
}

/// The icon shown instead of the logo while downloads run: a download arrow
/// over a tray line, green down to the aggregate progress and grey below it.
/// Straight RGBA, `size` pixels square. `sheen` is the animation phase: a
/// bright band at that position on its way down the arrow, `None` for a
/// still icon. `tile` sets it on a dark rounded square, the shape a Dock
/// icon wants; a tray or taskbar draws it bare.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn download_rgba(
    frame: crate::dl_indicator::IconFrame,
    sheen: Option<u8>,
    size: u32,
    tile: bool,
) -> Vec<u8> {
    let n = size as usize;
    let mut out = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            // 2x2 supersampling: the arrow head is all diagonal.
            let (mut sum, mut hits) = ([0u32; 3], 0u32);
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let u = (x as f32 + dx) / n as f32;
                let v = (y as f32 + dy) / n as f32;
                if let Some(color) = download_sample(frame, sheen, u, v, tile) {
                    for (s, c) in sum.iter_mut().zip(color) {
                        *s += u32::from(c);
                    }
                    hits += 1;
                }
            }
            let px = sum.map(|s| s.checked_div(hits).unwrap_or(0) as u8);
            out.extend_from_slice(&[px[0], px[1], px[2], (hits * 255 / 4) as u8]);
        }
    }
    out
}

/// The colour of the download icon at `(u, v)` in the unit square, `None`
/// where it is transparent.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn download_sample(
    frame: crate::dl_indicator::IconFrame,
    sheen: Option<u8>,
    u: f32,
    v: f32,
    tile: bool,
) -> Option<[u8; 3]> {
    use crate::dl_indicator::{IconFrame, ICON_STEPS, SHEEN_PHASES};
    const GREEN: [u8; 3] = [46, 204, 89];
    const GREEN_SHEEN: [u8; 3] = [170, 255, 190];
    const REST: [u8; 3] = [150, 156, 163];
    const REST_SHEEN: [u8; 3] = [214, 218, 223];
    const SHEEN_HALF: f32 = 0.07;
    const TILE: [u8; 3] = [28, 34, 43];
    const TOP: f32 = 0.06;
    const BOTTOM: f32 = 0.94;

    let (gu, gv) = if tile {
        ((u - 0.2) / 0.6, (v - 0.2) / 0.6)
    } else {
        (u, v)
    };
    let off = (gu - 0.5).abs();
    let arrow = ((TOP..0.5).contains(&gv) && off < 0.15)
        || ((0.5..0.8).contains(&gv) && off < 0.36 * (0.8 - gv) / 0.3);
    let base = (0.86..BOTTOM).contains(&gv) && off < 0.36;
    if arrow || base {
        let lit = match frame {
            IconFrame::Bar(n) => {
                let done = f32::from(n.min(ICON_STEPS)) / f32::from(ICON_STEPS);
                gv < TOP + (BOTTOM - TOP) * done
            }
            // Activity with no fraction to draw: the arrow alone is lit.
            IconFrame::Indeterminate => arrow,
            IconFrame::Idle => false,
        };
        let shining = sheen.is_some_and(|phase| {
            let at = (f32::from(phase % SHEEN_PHASES) + 0.5) / f32::from(SHEEN_PHASES);
            (gv - (TOP + (BOTTOM - TOP) * at)).abs() < SHEEN_HALF
        });
        return Some(match (lit, shining) {
            (true, false) => GREEN,
            (true, true) => GREEN_SHEEN,
            (false, false) => REST,
            (false, true) => REST_SHEEN,
        });
    }
    if tile {
        const HALF: f32 = 0.44;
        const RADIUS: f32 = 0.2;
        let (cx, cy) = ((u - 0.5).abs(), (v - 0.5).abs());
        let corner = HALF - RADIUS;
        let inside = cx <= HALF
            && cy <= HALF
            && (cx <= corner || cy <= corner || (cx - corner).hypot(cy - corner) <= RADIUS);
        return inside.then_some(TILE);
    }
    None
}

/// Straight RGBA as a PNG file, the form AppKit takes an icon image in.
#[cfg(target_os = "macos")]
pub fn png_bytes(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().ok()?.write_image_data(rgba).ok()?;
    Some(out)
}

/// The window icon for a download state: the logo when idle, the download
/// arrow otherwise. Windows and Linux only — a macOS window carries no icon.
#[cfg(not(target_os = "macos"))]
pub fn window_icon_for(frame: crate::dl_indicator::IconFrame) -> Option<iced::window::Icon> {
    if frame == crate::dl_indicator::IconFrame::Idle {
        return window_icon();
    }
    iced::window::icon::from_rgba(download_rgba(frame, None, 64, false), 64, 64).ok()
}

/// The logo as a window icon: title bar + taskbar on Windows, dock/panel on
/// Linux. macOS windows carry no icon (the app bundle provides the Dock one).
pub fn window_icon() -> Option<iced::window::Icon> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let (rgba, w, h) = logo_rgba()?;
    iced::window::icon::from_rgba(rgba, w, h).ok()
}

fn gradient_icon(paths: &str, a: &str, b: &str, enabled: bool) -> svg::Handle {
    let stroke = if enabled {
        r##"url(#g)"##.to_string()
    } else {
        r##"#B0B0B0"##.to_string()
    };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
<stop offset="0" stop-color="{a}"/><stop offset="1" stop-color="{b}"/></linearGradient></defs>
<g fill="none" stroke="{stroke}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{paths}</g></svg>"##
    );
    svg::Handle::from_memory(svg.into_bytes())
}

fn flat_icon(body: &str) -> svg::Handle {
    let svg =
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">{body}</svg>"##);
    svg::Handle::from_memory(svg.into_bytes())
}

// --------------------------------------------------------------- toolbar set

pub fn add_url(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| {
            gradient_icon(
                r#"<path d="M16 8 v16 M8 16 h16"/>"#,
                "#2FA84F",
                "#69B7E8",
                enabled,
            )
        };
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn resume(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<circle cx="16" cy="16" r="11"/><path d="M16 10 v9 M12 15.5 L16 19.5 L20 15.5"/>"#,
        "#38B6A0",
        "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn stop(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<circle cx="16" cy="16" r="11"/><rect x="12" y="12" width="8" height="8" rx="1"/>"#,
        "#8A97A8",
        "#B08BD8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn stop_all(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<path d="M11 15 V7.5 a1.6 1.6 0 0 1 3.2 0 V13 M14.2 13 V6.4 a1.6 1.6 0 0 1 3.2 0 V13 M17.4 13 V7.5 a1.6 1.6 0 0 1 3.2 0 V14 M20.6 14 v-3.4 a1.6 1.6 0 0 1 3.2 0 V19 a7.5 7.5 0 0 1 -7.5 7.5 h-1.4 a7.3 7.3 0 0 1 -6 -3.2 L6 19.2 a2 2 0 0 1 3.2 -2.3 l1.8 2.3"/>"#,
        "#8FA0B8",
        "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn delete(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<path d="M9 11 h14 M13 11 V9 a1.5 1.5 0 0 1 1.5 -1.5 h3 A1.5 1.5 0 0 1 19 9 v2 M10.5 11 l1 13.5 a1.8 1.8 0 0 0 1.8 1.6 h5.4 a1.8 1.8 0 0 0 1.8 -1.6 L21.5 11 M14 15 v7 M18 15 v7"/>"#,
        "#9A7FE8",
        "#D86FA8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn delete_completed(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<path d="M9 10 h14 M13 10 V8.4 a1.4 1.4 0 0 1 1.4 -1.4 h3.2 A1.4 1.4 0 0 1 19 8.4 V10 M10.5 10 l1 14.5 a1.8 1.8 0 0 0 1.8 1.6 h5.4 a1.8 1.8 0 0 0 1.8 -1.6 L21.5 10 M12.8 15.5 l2.6 2.6 4.4 -4.8"/>"#,
        "#D86FA8",
        "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn options(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<circle cx="16" cy="16" r="4"/><path d="M16 5.5 l1.6 3.1 3.4 -0.9 0.9 3.4 3.1 1.6 -1.6 3.3 1.6 3.3 -3.1 1.6 -0.9 3.4 -3.4 -0.9 -1.6 3.1 -1.6 -3.1 -3.4 0.9 -0.9 -3.4 -3.1 -1.6 1.6 -3.3 -1.6 -3.3 3.1 -1.6 0.9 -3.4 3.4 0.9 z"/>"#,
        "#E89A4F",
        "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn refresh(enabled: bool) -> svg::Handle {
    static CACHE: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    CACHE.get_or_init(|| {
        let make = |enabled| gradient_icon(
            r#"<path d="M25 14 a9 9 0 0 0 -15.5 -5.5 M9.5 8.5 H15 M9.5 8.5 V3 M7 18 a9 9 0 0 0 15.5 5.5 M22.5 23.5 H17 M22.5 23.5 V29"/>"#,
            "#38B6A0", "#4F8FE8", enabled,
        );
        [make(false), make(true)]
    })[enabled as usize].clone()
}

pub fn scheduler(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<circle cx="16" cy="17" r="9.5"/><path d="M16 12 v5 l3.5 2.5 M8 8.5 L10.5 6 M24 8.5 L21.5 6"/>"#,
        "#E86F9A",
        "#E8A44F",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn start_queue(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<rect x="6.5" y="6.5" width="14" height="14" rx="2.5"/><path d="M11 25.5 h9 a5.5 5.5 0 0 0 5.5 -5.5 v-9 M13.5 11 v6.5 M10 14 l3.5 3.6 3.5 -3.6"/>"#,
        "#4F8FE8",
        "#38B6A0",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn stop_queue(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<rect x="6.5" y="6.5" width="14" height="14" rx="2.5"/><path d="M11 25.5 h9 a5.5 5.5 0 0 0 5.5 -5.5 v-9"/><rect x="10.5" y="10.5" width="6" height="6" rx="1"/>"#,
        "#E86F9A",
        "#B08BD8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

/// Speedometer: a dial arc with a needle, for the Speed Limiter.
pub fn speed_limit(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<path d="M5.5 22 a11.5 11.5 0 1 1 21 0"/><path d="M16 20.5 L22 12.5"/><circle cx="16" cy="21.5" r="1.8"/><path d="M7.5 14.5 l1.8 1 M16 8 v2 M24.5 14.5 l-1.8 1"/>"#,
        "#E8A33D",
        "#4F8FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

pub fn extensions(enabled: bool) -> svg::Handle {
    // Rasterized-icon handles are cached: rebuilding the SVG string every
    // frame re-hashed kilobytes per icon per redraw for identical pixels.
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
        r#"<path d="M13 6.5 h3.4 a2.4 2.4 0 0 1 0 4.8 h3.6 a1.4 1.4 0 0 1 1.4 1.4 v3.6 a2.4 2.4 0 0 1 4.8 0 v3.4 a1.4 1.4 0 0 1 -1.4 1.4 h-3.4 v3.4 a1.4 1.4 0 0 1 -1.4 1.4 H8.4 A1.4 1.4 0 0 1 7 24.5 V13 a1.4 1.4 0 0 1 1.4 -1.4 H12 A2.4 2.4 0 0 1 13 6.5 z"/>"#,
        "#4F8FE8",
        "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

/// A mains plug: two prongs, a rounded body and the cable leaving it.
pub fn plugins(enabled: bool) -> svg::Handle {
    static C: OnceLock<[svg::Handle; 2]> = OnceLock::new();
    C.get_or_init(|| {
        let make = |enabled| gradient_icon(
            r#"<path d="M13 5.5 v7 M19 5.5 v7"/><path d="M9.5 12.5 h13 v4.5 a6.5 6.5 0 0 1 -13 0 z"/><path d="M16 23.5 v3 a2.5 2.5 0 0 0 2.5 2.5 H22"/>"#,
            "#38B6A0",
            "#9A7FE8",
            enabled,
        );
        [make(false), make(true)]
    })[enabled as usize]
        .clone()
}

// ------------------------------------------------------------ browser marks

/// Vendor artwork, kept verbatim in `assets/brand/` rather than redrawn: the
/// Edge and Firefox marks are too intricate to approximate convincingly.
const EDGE_SVG: &str = include_str!("../assets/brand/edge.svg");
const FIREFOX_SVG: &str = include_str!("../assets/brand/firefox.svg");

/// Full-colour brand mark, 32x32, for the Extensions page. Unlike the
/// toolbar set these are filled shapes rather than gradient outlines: a
/// browser logo is only recognisable in its own colours.
fn brand_icon(body: &str) -> svg::Handle {
    let svg =
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">{body}</svg>"##);
    svg::Handle::from_memory(svg.into_bytes())
}

/// Chrome: three 120-degree sectors around the blue hub.
pub fn browser_chrome() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| {
        brand_icon(
            r##"<path d="M16 16 L1 16 A15 15 0 0 1 23.5 3.01 Z" fill="#EA4335"/>
<path d="M16 16 L23.5 3.01 A15 15 0 0 1 23.5 28.99 Z" fill="#FBBC04"/>
<path d="M16 16 L23.5 28.99 A15 15 0 0 1 1 16 Z" fill="#34A853"/>
<circle cx="16" cy="16" r="7.6" fill="#FFFFFF"/><circle cx="16" cy="16" r="6" fill="#4285F4"/>"##,
        )
    })
    .clone()
}

/// Chromium: the Chrome geometry in the project's blue-greys, for the
/// "other Chromium browsers" row.
pub fn browser_chromium() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| {
        brand_icon(
            r##"<path d="M16 16 L1 16 A15 15 0 0 1 23.5 3.01 Z" fill="#7C99B8"/>
<path d="M16 16 L23.5 3.01 A15 15 0 0 1 23.5 28.99 Z" fill="#A8C0D6"/>
<path d="M16 16 L23.5 28.99 A15 15 0 0 1 1 16 Z" fill="#5B7B9C"/>
<circle cx="16" cy="16" r="7.6" fill="#FFFFFF"/><circle cx="16" cy="16" r="6" fill="#33648F"/>"##,
        )
    })
    .clone()
}

/// Edge: Microsoft's own brand mark.
pub fn browser_edge() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| svg::Handle::from_memory(EDGE_SVG.as_bytes()))
        .clone()
}

/// Firefox: Mozilla's own brand mark.
pub fn browser_firefox() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| svg::Handle::from_memory(FIREFOX_SVG.as_bytes()))
        .clone()
}

/// Safari: the compass rose.
pub fn browser_safari() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| {
        brand_icon(
            r##"<defs><linearGradient id="s" x1="0" y1="0" x2="0" y2="1">
<stop offset="0" stop-color="#3DB4F2"/><stop offset="1" stop-color="#0F6FD8"/></linearGradient></defs>
<circle cx="16" cy="16" r="15" fill="url(#s)"/><circle cx="16" cy="16" r="12.2" fill="#F4F7FA"/>
<path d="M16 4.4 v2 M16 25.6 v2 M4.4 16 h2 M25.6 16 h2" stroke="#7E93A8" stroke-width="1.1" stroke-linecap="round"/>
<path d="M22.6 9.4 L17.6 17.6 9.4 22.6 14.4 14.4 z" fill="#F04B3C"/>
<path d="M9.4 22.6 L14.4 14.4 17.6 17.6 z" fill="#E9EEF3"/>"##,
        )
    })
    .clone()
}

// ----------------------------------------------------------- tree / row set

pub fn folder_all() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M1 3.5 A1.2 1.2 0 0 1 2.2 2.3 h3.4 l1.4 1.6 h6.8 A1.2 1.2 0 0 1 15 5.1 v7.4 a1.2 1.2 0 0 1 -1.2 1.2 h-11.6 A1.2 1.2 0 0 1 1 12.5 z" fill="#F6D97A" stroke="#C9A93F" stroke-width="0.8"/>"##,
    ))
    .clone()
}

pub fn folder_compressed() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<rect x="3" y="1.5" width="10" height="13" rx="1.2" fill="#EED9A0" stroke="#B9924A" stroke-width="0.8"/><path d="M8 2 v2 M8 5 v2 M8 8 v2" stroke="#8A6A2F" stroke-width="1.6"/><rect x="6.7" y="10" width="2.6" height="3" fill="#8A6A2F"/>"##,
    ))
    .clone()
}

pub fn folder_documents() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M4 1.5 h6 l3 3 v10 h-9 z" fill="#FFFFFF" stroke="#8AA0B8" stroke-width="0.9"/><path d="M10 1.5 v3 h3" fill="none" stroke="#8AA0B8" stroke-width="0.9"/><path d="M5.5 7 h5 M5.5 9 h5 M5.5 11 h3.5" stroke="#9AB0C8" stroke-width="0.9"/>"##,
    ))
    .clone()
}

pub fn folder_music() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M6 12.2 V4 l7 -1.6 v8.4" fill="none" stroke="#4F6FD8" stroke-width="1.2"/><circle cx="4.6" cy="12.4" r="1.9" fill="#4F6FD8"/><circle cx="11.6" cy="11" r="1.9" fill="#4F6FD8"/>"##,
    ))
    .clone()
}

pub fn folder_programs() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<rect x="1.5" y="2.5" width="13" height="11" rx="1" fill="#EAF2FB" stroke="#5A7A9A" stroke-width="0.9"/><rect x="1.5" y="2.5" width="13" height="3" fill="#5A8FD8"/><circle cx="3.2" cy="4" r="0.6" fill="#fff"/><path d="M4.5 9 l2 -1.6 v3.2 z" fill="#3A9E3A"/><rect x="8" y="8" width="4" height="1.4" fill="#5A7A9A"/>"##,
    ))
    .clone()
}

pub fn folder_video() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<rect x="1.5" y="3" width="13" height="10" rx="1" fill="#3A4A6A" stroke="#26334A" stroke-width="0.8"/><rect x="3" y="4.5" width="2" height="1.6" fill="#fff"/><rect x="3" y="7.2" width="2" height="1.6" fill="#fff"/><rect x="3" y="9.9" width="2" height="1.6" fill="#fff"/><rect x="11" y="4.5" width="2" height="1.6" fill="#fff"/><rect x="11" y="7.2" width="2" height="1.6" fill="#fff"/><rect x="11" y="9.9" width="2" height="1.6" fill="#fff"/><path d="M7 6 l3 2 -3 2 z" fill="#fff"/>"##,
    ))
    .clone()
}

/// Model weights and dataset containers: a small feed-forward net.
///
/// Purple on purpose — every other category glyph is already claimed (yellow
/// folder, blue programs, navy video, white documents), and at 16px the tree
/// is read by colour before shape.
pub fn folder_ai() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M4 4.6 L8 8 M4 11.4 L8 8 M8 8 L12 4.6 M8 8 L12 11.4" fill="none" stroke="#7A5AF8" stroke-width="1" stroke-linecap="round"/><circle cx="4" cy="4.6" r="1.9" fill="#C7B8FF" stroke="#7A5AF8" stroke-width="0.8"/><circle cx="4" cy="11.4" r="1.9" fill="#C7B8FF" stroke="#7A5AF8" stroke-width="0.8"/><circle cx="12" cy="4.6" r="1.9" fill="#C7B8FF" stroke="#7A5AF8" stroke-width="0.8"/><circle cx="12" cy="11.4" r="1.9" fill="#C7B8FF" stroke="#7A5AF8" stroke-width="0.8"/><circle cx="8" cy="8" r="2.2" fill="#7A5AF8" stroke="#5B3FD0" stroke-width="0.8"/>"##,
    ))
    .clone()
}

pub fn folder_unfinished() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M1 4 A1.2 1.2 0 0 1 2.2 2.8 h3.2 l1.3 1.5 h6.1 A1.2 1.2 0 0 1 14 5.5 v7 a1.2 1.2 0 0 1 -1.2 1.2 h-10.6 A1.2 1.2 0 0 1 1 12.5 z" fill="#D8E8C8" stroke="#8AA86A" stroke-width="0.8"/><path d="M8 6 v4 M6.2 8.4 L8 10.3 9.8 8.4" fill="none" stroke="#3A9E3A" stroke-width="1.3"/>"##,
    ))
    .clone()
}

pub fn folder_finished() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M1 4 A1.2 1.2 0 0 1 2.2 2.8 h3.2 l1.3 1.5 h6.1 A1.2 1.2 0 0 1 14 5.5 v7 a1.2 1.2 0 0 1 -1.2 1.2 h-10.6 A1.2 1.2 0 0 1 1 12.5 z" fill="#EAF2FB" stroke="#8AA0B8" stroke-width="0.8"/><path d="M4.5 8.5 l2.3 2.3 4.4 -4.8" fill="none" stroke="#2E7D32" stroke-width="1.6"/>"##,
    ))
    .clone()
}

#[allow(dead_code)]
pub fn grabber() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<circle cx="8" cy="8" r="6.2" fill="#EAF6FF" stroke="#3A7ACD" stroke-width="0.9"/><path d="M2 8 h12 M8 1.8 a9.5 9.5 0 0 1 0 12.4 M8 1.8 a9.5 9.5 0 0 0 0 12.4" fill="none" stroke="#3A7ACD" stroke-width="0.8"/>"##,
    ))
    .clone()
}

pub fn queues() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M2 5 A1 1 0 0 1 3 4 h2.8 l1.2 1.3 h5 A1 1 0 0 1 13 6.3 v1 h-11 z" fill="#F6D97A" stroke="#C9A93F" stroke-width="0.7"/><path d="M2 8 h11 v4.5 a1 1 0 0 1 -1 1 h-9 a1 1 0 0 1 -1 -1 z" fill="#FBE9A8" stroke="#C9A93F" stroke-width="0.7"/>"##,
    ))
    .clone()
}

/// A user-made category. The stock folder shape in a neutral slate: it has
/// to read as a category without borrowing one of the built-in glyphs, which
/// say what is inside them.
pub fn folder_custom() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M2 5 A1 1 0 0 1 3 4 h2.8 l1.2 1.3 h5 A1 1 0 0 1 13 6.3 v1 h-11 z" fill="#9FB4C7" stroke="#6B8299" stroke-width="0.7"/><path d="M2 8 h11 v4.5 a1 1 0 0 1 -1 1 h-9 a1 1 0 0 1 -1 -1 z" fill="#C8D6E2" stroke="#6B8299" stroke-width="0.7"/>"##,
    ))
    .clone()
}

/// The queues folder in a queue's own colour; `None` is the stock yellow
/// [`queues`] icon. One handle per colour is kept, for the same reason the
/// toolbar icons are: a rebuilt SVG re-hashes every frame for the same
/// pixels.
pub fn queue_folder(color: Option<u32>) -> svg::Handle {
    let Some(rgb) = color else {
        return queues();
    };
    static C: OnceLock<Mutex<HashMap<u32, svg::Handle>>> = OnceLock::new();
    let cache = C.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .entry(rgb)
        .or_insert_with(|| {
            let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
            // The back flap in the colour itself, the front a lighter tint,
            // the outline a darker shade — the same three-tone folder the
            // stock icon draws in yellow.
            let tint = |v: u8| v as u32 + (255 - v as u32) * 45 / 100;
            let shade = |v: u8| v as u32 * 65 / 100;
            flat_icon(&format!(
                r##"<path d="M2 5 A1 1 0 0 1 3 4 h2.8 l1.2 1.3 h5 A1 1 0 0 1 13 6.3 v1 h-11 z" fill="#{r:02X}{g:02X}{b:02X}" stroke="#{:02X}{:02X}{:02X}" stroke-width="0.7"/><path d="M2 8 h11 v4.5 a1 1 0 0 1 -1 1 h-9 a1 1 0 0 1 -1 -1 z" fill="#{:02X}{:02X}{:02X}" stroke="#{:02X}{:02X}{:02X}" stroke-width="0.7"/>"##,
                shade(r), shade(g), shade(b),
                tint(r), tint(g), tint(b),
                shade(r), shade(g), shade(b),
            ))
        })
        .clone()
}

pub fn file_generic() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M4 1.5 h6 l3 3 v10 h-9 z" fill="#FFFFFF" stroke="#9AA8B8" stroke-width="0.9"/><path d="M10 1.5 v3 h3" fill="none" stroke="#9AA8B8" stroke-width="0.9"/>"##,
    ))
    .clone()
}

pub fn expander(open: bool) -> svg::Handle {
    let glyph = if open {
        r##"<path d="M4 8 h8" stroke="#5A5A5A" stroke-width="1.2"/>"##
    } else {
        r##"<path d="M4 8 h8 M8 4 v8" stroke="#5A5A5A" stroke-width="1.2"/>"##
    };
    flat_icon(&format!(
        r##"<rect x="2.5" y="2.5" width="11" height="11" fill="#FFFFFF" stroke="#9AA8B8" stroke-width="0.9"/>{glyph}"##
    ))
}

/// Download glyph (arrow into tray) used in the "Download File Info" dialog
/// corner instead of the app logo.
pub fn warning() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<path d="M8 1.5 L15 14 H1 z" fill="#F6C744" stroke="#C99A20" stroke-width="0.8"/><path d="M8 5.5 v4.2" stroke="#4A3A00" stroke-width="1.5"/><circle cx="8" cy="11.8" r="0.9" fill="#4A3A00"/>"##,
    ))
    .clone()
}

/// Blue "i" bubble for informational dialogs (e.g. "you are up to date").
pub fn info() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<circle cx="8" cy="8" r="6.5" fill="#2D7DD2"/><circle cx="8" cy="8" r="6.5" fill="none" stroke="#1F5FA8" stroke-width="0.8"/><circle cx="8" cy="4.9" r="1.0" fill="#FFFFFF"/><path d="M8 7.2 v4.2" stroke="#FFFFFF" stroke-width="1.6" stroke-linecap="round"/>"##,
    ))
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(img: &[u8], n: usize, x: usize, y: usize) -> [u8; 4] {
        img[(y * n + x) * 4..][..4].try_into().unwrap()
    }

    #[test]
    fn the_download_arrow_fills_from_the_top_with_progress() {
        use crate::dl_indicator::IconFrame;
        const N: usize = 100;
        const GREEN: [u8; 4] = [46, 204, 89, 255];
        const REST: [u8; 4] = [150, 156, 163, 255];
        let draw = |frame| download_rgba(frame, None, N as u32, false);

        let empty = draw(IconFrame::Bar(0));
        let half = draw(IconFrame::Bar(10));
        let full = draw(IconFrame::Bar(200));
        assert_eq!(empty.len(), N * N * 4);
        assert_eq!(pixel(&empty, N, 50, 20), REST, "shaft");
        assert_eq!(pixel(&empty, N, 50, 70), REST, "head");
        assert_eq!(pixel(&empty, N, 50, 90), REST, "tray line");
        assert_eq!(pixel(&empty, N, 5, 20)[3], 0, "beside the shaft");
        assert_eq!(pixel(&empty, N, 50, 83)[3], 0, "between head and line");
        assert_eq!(pixel(&half, N, 50, 20), GREEN);
        assert_eq!(pixel(&half, N, 50, 70), REST);
        assert_eq!(pixel(&full, N, 50, 70), GREEN);
        assert_eq!(pixel(&full, N, 50, 90), GREEN);

        let busy = draw(IconFrame::Indeterminate);
        assert_eq!(pixel(&busy, N, 50, 70), GREEN);
        assert_eq!(pixel(&busy, N, 50, 90), REST);
        assert_eq!(pixel(&draw(IconFrame::Idle), N, 50, 20), REST);

        let edge = (0..N)
            .map(|x| pixel(&empty, N, x, 65)[3])
            .any(|a| a > 0 && a < 255);
        assert!(edge, "the head's diagonal is antialiased");
    }

    #[test]
    fn the_sheen_travels_down_the_arrow_and_wraps() {
        use crate::dl_indicator::{IconFrame, SHEEN_PHASES};
        const N: usize = 100;
        let draw = |phase| download_rgba(IconFrame::Bar(10), Some(phase), N as u32, false);
        let band = |img: &[u8]| -> Vec<usize> {
            (0..N)
                .filter(|&y| {
                    let px = pixel(img, N, 50, y);
                    px == [170, 255, 190, 255] || px == [214, 218, 223, 255]
                })
                .collect()
        };
        let still = download_rgba(IconFrame::Bar(10), None, N as u32, false);
        assert!(band(&still).is_empty());

        let first = draw(0);
        assert_eq!(pixel(&first, N, 50, 10), [170, 255, 190, 255], "lit part");
        let mut last_top = 0;
        for phase in 0..SHEEN_PHASES {
            let rows = band(&draw(phase));
            assert!(!rows.is_empty(), "phase {phase} shows a band");
            assert!(rows[0] >= last_top, "phase {phase} moves down");
            last_top = rows[0];
        }
        assert_eq!(
            pixel(&draw(5), N, 50, 62),
            [214, 218, 223, 255],
            "unlit part"
        );
        assert_eq!(draw(SHEEN_PHASES), first, "wraps");
    }

    #[test]
    fn the_dock_variant_sits_on_a_rounded_tile() {
        use crate::dl_indicator::IconFrame;
        const N: usize = 100;
        let tile = download_rgba(IconFrame::Bar(20), None, N as u32, true);
        assert_eq!(pixel(&tile, N, 50, 10), [28, 34, 43, 255], "tile");
        assert_eq!(pixel(&tile, N, 10, 50), [28, 34, 43, 255], "tile");
        assert_eq!(pixel(&tile, N, 7, 7)[3], 0, "rounded corner");
        assert_eq!(pixel(&tile, N, 2, 50)[3], 0, "margin");
        assert_eq!(pixel(&tile, N, 50, 40), [46, 204, 89, 255], "arrow");
        assert_eq!(pixel(&tile, N, 50, 74), [46, 204, 89, 255], "tray line");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_dock_icon_round_trips_through_png() {
        use crate::dl_indicator::IconFrame;
        let rgba = download_rgba(IconFrame::Bar(7), None, 32, true);
        let file = png_bytes(&rgba, 32, 32).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(file))
            .read_info()
            .unwrap();
        let mut back = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut back).unwrap();
        assert_eq!((info.width, info.height), (32, 32));
        assert_eq!(&back[..info.buffer_size()], &rgba[..]);
        assert!(png_bytes(&rgba, 32, 99).is_none(), "size mismatch");
    }

    /// Windows accepts PNG-compressed icon images only at 256x256; below that
    /// the entry has to be a BITMAPINFOHEADER DIB. An all-PNG .ico still looks
    /// right in Explorer and the Start menu, which decode through the shell,
    /// but the small-icon GDI path draws nothing — which is how the app came
    /// to have a blank row in Task Manager.
    #[test]
    fn windows_icon_is_dib_below_256px() {
        const ICO: &[u8] = include_bytes!("../../../scripts/windows/hydra.ico");
        const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\x0a";

        let count = u16::from_le_bytes([ICO[4], ICO[5]]) as usize;
        assert!(count > 0);

        let mut sizes = Vec::new();
        for i in 0..count {
            let e = &ICO[6 + i * 16..][..16];
            let width = if e[0] == 0 { 256 } else { e[0] as u32 };
            let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
            let off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
            let image = &ICO[off..off + len];
            sizes.push(width);

            if width >= 256 {
                continue;
            }
            assert_ne!(&image[..8], PNG_MAGIC, "{width}px entry is PNG-compressed");
            let header_size = u32::from_le_bytes([image[0], image[1], image[2], image[3]]);
            let dib_width = i32::from_le_bytes([image[4], image[5], image[6], image[7]]);
            let dib_height = i32::from_le_bytes([image[8], image[9], image[10], image[11]]);
            let bit_count = u16::from_le_bytes([image[14], image[15]]);
            let compression = u32::from_le_bytes([image[16], image[17], image[18], image[19]]);
            assert_eq!(header_size, 40, "{width}px entry is not a BITMAPINFOHEADER");
            assert_eq!(dib_width, width as i32);
            // The DIB stacks the colour image on the AND mask, so it is twice
            // as tall as the icon it draws.
            assert_eq!(dib_height, width as i32 * 2, "{width}px entry has no mask");
            assert_eq!(bit_count, 32);
            assert_eq!(compression, 0, "{width}px entry is compressed");

            let stride = width.div_ceil(32) * 4;
            assert_eq!(len, 40 + (width * width * 4 + stride * width) as usize);
        }

        for want in [16, 32, 48, 256] {
            assert!(sizes.contains(&want), "no {want}px entry");
        }
    }
}

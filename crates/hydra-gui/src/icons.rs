// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vector icons: thin colourful gradient
//! outlines for the toolbar, small filled glyphs for the category tree.
//!
//! Icons are generated SVG strings (stroke paths over a two-stop gradient)
//! rather than shipped bitmap files so they scale with any DPI and the
//! disabled state is just a grey re-stroke of the same geometry.

use iced::widget::svg;
use std::sync::OnceLock;

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

/// The hydra app glyph used in dialog corners.
pub fn logo() -> svg::Handle {
    static C: OnceLock<svg::Handle> = OnceLock::new();
    C.get_or_init(|| flat_icon(
        r##"<circle cx="8" cy="8" r="6.5" fill="#2FA84F"/><circle cx="8" cy="8" r="6.5" fill="none" stroke="#1E7A38" stroke-width="0.8"/><path d="M5 10.5 V5.2 M5 7.8 h6 M11 10.5 V5.2" stroke="#FFFFFF" stroke-width="1.5" fill="none"/>"##,
    ))
    .clone()
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

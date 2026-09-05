// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod about;
pub mod add_url;
pub mod batch;
pub mod complete;
pub mod confirm;
pub mod file_info;
pub mod main_win;
pub mod options;
pub mod permissions;
pub mod power;
pub mod progress;
pub mod scheduler;
pub mod shortcuts;
pub mod update;
pub mod zip_preview;

use crate::app::{El, Message};
use crate::theme;
use iced::widget::{button, text};

/// Wrap a link-ish element with a hover hint describing the file type its
/// URL points at (`DMG — Apple Disk Image (macOS)`). URLs without a known
/// extension come back unwrapped — no tooltip beats a useless one.
pub fn ext_hint<'a>(el: impl Into<El<'a>>, url: &str) -> El<'a> {
    match crate::ext_info::hint_for_url(url) {
        Some(hint) => iced::widget::tooltip(
            el,
            iced::widget::container(text(hint).size(theme::FONT_SIZE - 1.0))
                .padding(8)
                .max_width(360.0)
                .style(theme::menu_panel),
            iced::widget::tooltip::Position::Bottom,
        )
        .into(),
        None => el.into(),
    }
}

/// Spacer-centred text for any Fill-width context: `Text::center()`
/// mis-places some RTL runs, spacers never do.
pub fn centered<'a>(label: String, size: f32) -> El<'a> {
    iced::widget::row![
        iced::widget::space::horizontal(),
        text(label).size(size),
        iced::widget::space::horizontal(),
    ]
    .width(iced::Length::Fill)
    .into()
}

/// Centred label for a fixed-width dialog button. Spacer-centred rather than
/// `Text::center()` — text-level centring misrendered some RTL runs.
fn btn_label<'a>(label: String) -> El<'a> {
    iced::widget::row![
        iced::widget::space::horizontal(),
        text(label).size(theme::FONT_SIZE),
        iced::widget::space::horizontal(),
    ]
    .width(iced::Length::Fill)
    .into()
}

/// A dialog button sized to its label — for rows whose labels vary too much
/// for a uniform width ("Download as new file" vs "Cancel").
pub fn dlg_btn_auto<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(text(label).size(theme::FONT_SIZE))
        .padding([5, 16])
        .style(theme::btn);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// As [`dlg_btn_auto`] with the accent border.
pub fn dlg_btn_auto_primary<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(text(label).size(theme::FONT_SIZE))
        .padding([5, 16])
        .style(theme::btn_primary);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// A classic dialog push button: uniform width, wide enough for the longer
/// Persian/Arabic labels.
pub fn dlg_btn<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(btn_label(label))
        .padding([5, 8])
        .width(132.0)
        .style(theme::btn);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// The dialog's default button (accent border).
pub fn dlg_btn_primary<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(btn_label(label))
        .padding([5, 8])
        .width(132.0)
        .style(theme::btn_primary);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

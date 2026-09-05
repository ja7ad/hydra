// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Zip preview": what is inside an archive, before it is downloaded.
//!
//! Laid out like IDM's: the archive's name and entry count on one line, a
//! four-column list (name with a type icon, size, packed size, modified),
//! and an OK button. The listing comes from the archive's tail — see
//! `engine::peek_zip` — so it appears in the time one small request takes,
//! whatever the archive's size.

use crate::app::{App, El, Message, WinKind, ZipPeek};
use crate::windows::dlg_btn_primary;
use crate::{fmt, i18n::tr, icons, model, theme};
use hya_net::zipdir::{DosTime, Entry};
use iced::widget::{column, container, row, scrollable, svg, text};
use iced::{Background, Length};

const CELL_H: f32 = 22.0;
const ICON: f32 = 16.0;
const SIZE_W: f32 = 90.0;
const PACKED_W: f32 = 90.0;
const MODIFIED_W: f32 = 150.0;

pub fn view(app: &App) -> El<'_> {
    let st = &app.zip_preview;

    let heading = match &st.result {
        ZipPeek::Listed(entries) => {
            let files = entries.iter().filter(|e| !e.is_dir()).count();
            format!(
                "{},   {}",
                st.file_name,
                tr("total {n} files").replace("{n}", &files.to_string())
            )
        }
        _ => st.file_name.clone(),
    };

    let body: El<'_> = match &st.result {
        ZipPeek::Loading => centered_note(
            tr("Reading the archive's file list..."),
            theme::dim_text(&iced::Theme::Light),
        ),
        ZipPeek::Failed(why) => {
            centered_note(why.clone(), iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
        }
        ZipPeek::Listed(entries) => {
            // Files only, as IDM lists them: a directory entry says nothing
            // the paths of the files inside it do not already say.
            let mut rows = column![].spacing(0);
            for e in entries.iter().filter(|e| !e.is_dir()) {
                rows = rows.push(row_el(app, e));
            }
            column![header(), hairline(), scrollable(rows).height(Length::Fill)]
                .spacing(0)
                .into()
        }
    };

    let list = container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::panel);

    container(
        column![
            crate::windows::centered(heading, theme::FONT_SIZE),
            list,
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(
                    tr("OK"),
                    app.win_of(WinKind::ZipPreview(st.dl))
                        .map(Message::CloseThis)
                ),
                iced::widget::space::horizontal(),
            ],
        ]
        .spacing(10)
        .padding(14),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

fn centered_note<'a>(s: String, color: iced::Color) -> El<'a> {
    container(
        text(s)
            .size(theme::FONT_SIZE)
            .color(color)
            .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center(Length::Fill)
    .padding(12)
    .into()
}

fn header<'a>() -> El<'a> {
    let cell = |label: String, w: Option<f32>| {
        container(
            text(label)
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .padding([3, 6])
        .width(w.map(Length::from).unwrap_or(Length::Fill))
        .height(CELL_H)
        .clip(true)
        .style(theme::header_cell(false))
    };
    row![
        cell(tr("File Name"), None),
        cell(tr("Size"), Some(SIZE_W)),
        cell(tr("Packed"), Some(PACKED_W)),
        cell(tr("Modified"), Some(MODIFIED_W)),
    ]
    .spacing(0)
    .align_y(iced::Alignment::Center)
    .into()
}

fn row_el<'a>(app: &App, e: &Entry) -> El<'a> {
    // WinRAR's convention: an encrypted entry's name carries a trailing `*`.
    let name = if e.encrypted {
        format!("{}*", e.name)
    } else {
        e.name.clone()
    };
    let name_cell = row![
        svg(entry_icon(app, e)).width(ICON).height(ICON),
        plain(name),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    row![
        cell(name_cell.into(), None),
        cell(plain(fmt::size2(e.size)), Some(SIZE_W)),
        cell(plain(fmt::size2(e.packed)), Some(PACKED_W)),
        cell(
            plain(e.modified.map(stamp).unwrap_or_default()),
            Some(MODIFIED_W)
        ),
    ]
    .spacing(0)
    .align_y(iced::Alignment::Center)
    .into()
}

/// The type icon the download list would give this name: the category
/// folder for a known extension, the plain file for anything else.
fn entry_icon(app: &App, e: &Entry) -> svg::Handle {
    let leaf = e.name.rsplit('/').next().unwrap_or(&e.name);
    if !leaf.contains('.') {
        return icons::file_generic();
    }
    match model::categorize(leaf, &app.cfg.categories) {
        Some(cat) => crate::ui::categories::cat_icon(&cat),
        None => icons::file_generic(),
    }
}

/// `04 Sep 2026 13:27`, as the archiver's clock recorded it.
fn stamp(t: DosTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{:02} {} {} {:02}:{:02}",
        t.day,
        MONTHS[(t.month as usize - 1).min(11)],
        t.year,
        t.hour,
        t.minute
    )
}

fn plain<'a>(s: String) -> El<'a> {
    text(s)
        .size(theme::FONT_SIZE)
        .wrapping(iced::widget::text::Wrapping::None)
        .into()
}

fn cell<'a>(content: El<'a>, w: Option<f32>) -> El<'a> {
    container(content)
        .width(w.map(Length::from).unwrap_or(Length::Fill))
        .height(CELL_H)
        .padding([2, 6])
        .clip(true)
        .into()
}

/// The 1 px rule under the header, in the list's own grid colour.
fn hairline<'a>() -> El<'a> {
    container(iced::widget::space::horizontal())
        .width(Length::Fill)
        .height(1.0)
        .style(|t| container::Style {
            background: Some(Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
        .into()
}

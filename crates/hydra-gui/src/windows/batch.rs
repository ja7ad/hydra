// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Add batch download" — paste a list of URLs, check the ones to add,
//! choose where they are saved ("Download All Links" flow).

use crate::app::{App, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{i18n::tr, theme};
use iced::widget::{
    checkbox, column, container, pick_list, radio, row, scrollable, text, text_editor, text_input,
};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let st = &app.batch;

    let editor = text_editor(&st.text)
        .placeholder("https://example.com/file1.zip\nhttps://example.com/file2.zip")
        .on_action(Message::BatchEdit)
        .size(theme::FONT_SIZE)
        .height(180.0);

    // Link table: File Name | Size | Download from | Save to.
    //
    // The name is the probe's answer where there is one: a redirector link
    // (`href.li/?<url>`) has no filename of its own and would otherwise list —
    // and save — as `index.html`. Until the probe answers, the URL's own last
    // segment stands in.
    let name_for = |url: &str| -> String {
        st.names
            .get(url)
            .cloned()
            .unwrap_or_else(|| crate::engine::file_name_from_url(url))
    };
    let save_to_for = |url: &str| -> String {
        if st.to_dir {
            st.dir.clone()
        } else {
            let name = name_for(url);
            let cat = if st.to_category {
                Some(st.category.clone())
            } else {
                crate::model::categorize(&name, &app.cfg.categories)
            };
            app.cat_dir(cat.as_deref())
                .or_else(|| app.cat_dir(None))
                .unwrap_or_default()
        }
    };
    let mut checks = column![].spacing(1);
    checks = checks.push(
        row![
            container(text("").size(theme::FONT_SIZE)).width(28.0),
            container(text(tr("File Name")).size(theme::FONT_SIZE)).width(220.0),
            container(text(tr("Size")).size(theme::FONT_SIZE)).width(70.0),
            container(text(tr("Download from")).size(theme::FONT_SIZE)).width(Length::Fill),
            container(text(tr("Save to")).size(theme::FONT_SIZE)).width(220.0),
        ]
        .spacing(4),
    );
    let blocked_color = iced::Color::from_rgb8(0xC0, 0x2B, 0x2B);
    for (i, (url, on)) in st.checks.iter().enumerate() {
        let blocked = crate::app::site_blocked(url, &app.cfg.settings.dont_start_sites);
        let from_text = if blocked {
            text(format!("{url} ({})", tr("blocked site")))
                .size(theme::FONT_SIZE)
                .color(blocked_color)
        } else {
            text(url.clone()).size(theme::FONT_SIZE)
        };
        checks = checks.push(
            row![
                container(
                    checkbox(*on)
                        .on_toggle(move |b| Message::BatchCheck(i, b))
                        .size(15.0)
                        .style(theme::check)
                )
                .width(28.0),
                container(text(name_for(url)).size(theme::FONT_SIZE))
                    .width(220.0)
                    .clip(true),
                container(
                    text(
                        st.sizes
                            .get(url)
                            .map(|s| crate::fmt::size2(*s))
                            .unwrap_or_default()
                    )
                    .size(theme::FONT_SIZE)
                )
                .width(70.0),
                container(from_text).width(Length::Fill).clip(true),
                container(text(save_to_for(url)).size(theme::FONT_SIZE))
                    .width(220.0)
                    .clip(true),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        );
    }

    let cats: Vec<String> = app.cfg.categories.iter().map(|c| c.name.clone()).collect();
    let mode = if st.to_category {
        1u8
    } else if st.to_dir {
        2
    } else {
        0
    };

    container(
        column![
            text(tr("Please check the links, which you want to add to the download list, and click OK button."))
                .size(theme::FONT_SIZE),
            editor,
            row![
                dlg_btn(tr("Check All"), Some(Message::BatchCheckAll(true))),
                dlg_btn(tr("Uncheck All"), Some(Message::BatchCheckAll(false))),
            ]
            .spacing(8),
            container(scrollable(checks).height(Length::Fill))
                .padding(6)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel),
            text(tr("Save To")).size(theme::FONT_SIZE + 1.0),
            radio(
                tr("Every file to the directory according to the category of the file"),
                0u8,
                Some(mode),
                Message::BatchSaveMode,
            )
            .size(15.0)
            .text_size(theme::FONT_SIZE),
            row![
                radio(tr("All files to one category"), 1u8, Some(mode), Message::BatchSaveMode)
                    .size(15.0)
                    .text_size(theme::FONT_SIZE),
                pick_list(cats, Some(st.category.clone()), Message::BatchCategory)
                    .text_size(theme::FONT_SIZE)
                    .style(theme::picker)
                    .width(220.0),
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
            row![
                radio(tr("All files to one directory"), 2u8, Some(mode), Message::BatchSaveMode)
                    .size(15.0)
                    .text_size(theme::FONT_SIZE),
                text_input("", &st.dir)
                    .on_input(Message::BatchDir)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(Length::Fill),
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
            // Only shown when the list actually contains a manifest: a
            // quality picker over a list of ordinary files would be noise.
            streams_row(st),
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(tr("OK"), Some(Message::BatchOk)),
                dlg_btn(tr("Cancel"), app.win_of(WinKind::Batch).map(Message::CloseThis)),
            ]
            .spacing(10),
        ]
        .spacing(10)
        .padding(14),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

/// The one stream setting a batch needs, when it has any streams in it.
///
/// A picker per URL would mean probing every manifest to build it, and
/// answering the same question once per link. One preference for the batch
/// is resolved against each manifest's own ladder as it starts.
fn streams_row(st: &crate::app::BatchState) -> El<'_> {
    let n = st
        .checks
        .iter()
        .filter(|(u, on)| *on && crate::app::manifest_address(u))
        .count();
    if n == 0 {
        return iced::widget::space::horizontal().height(0.0).into();
    }
    let container = if st.stream_container.is_empty() {
        "MP4".to_string()
    } else {
        st.stream_container.clone()
    };
    row![
        text(format!(
            "{n} {}",
            tr("stream(s) in this list — download at")
        ))
        .size(theme::FONT_SIZE),
        pick_list(
            crate::app::BatchQuality::ALL.to_vec(),
            Some(st.stream_quality),
            Message::BatchStreamQuality
        )
        .text_size(theme::FONT_SIZE)
        .style(theme::picker)
        .width(150.0),
        pick_list(
            vec!["MP4".to_string(), "TS".to_string()],
            Some(container),
            Message::BatchStreamContainer
        )
        .text_size(theme::FONT_SIZE)
        .style(theme::picker)
        .width(90.0),
    ]
    .spacing(12)
    .align_y(iced::Alignment::Center)
    .into()
}

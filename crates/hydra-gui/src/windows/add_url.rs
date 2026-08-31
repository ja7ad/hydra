// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Enter new address to download" — Add URL dialog.

use crate::app::{App, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{i18n::tr, icons, theme};
use iced::widget::{checkbox, column, container, pick_list, row, svg, text, text_input};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let st = &app.add_url;
    let title = row![
        svg(icons::add_url(true)).width(20.0).height(20.0),
        text(tr("Enter new address to download")).size(theme::FONT_SIZE + 1.0),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);

    let address = row![
        text(tr("Address"))
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(70.0),
        crate::windows::ext_hint(
            text_input("http://", &st.address)
                .on_input(Message::AddrChanged)
                .on_submit(Message::AddUrlOk)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            &st.address,
        ),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let auth = checkbox(st.use_auth)
        .label(tr("Use authorization"))
        .on_toggle(Message::AddrAuthToggled)
        .size(15.0)
        .text_size(theme::FONT_SIZE)
        .style(theme::check);

    let mut creds = row![].spacing(12).align_y(iced::Alignment::Center);
    if st.use_auth {
        creds = creds
            .push(
                text(tr("Login"))
                    .size(theme::FONT_SIZE)
                    .wrapping(iced::widget::text::Wrapping::None)
                    .width(70.0),
            )
            .push(
                text_input("", &st.login)
                    .on_input(Message::AddrLogin)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(220.0),
            )
            .push(text(tr("Password")).size(theme::FONT_SIZE))
            .push(
                text_input("", &st.password)
                    .on_input(Message::AddrPass)
                    .secure(true)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(220.0),
            );
    }

    // A manifest is not a file: which rendition, which container, and — for
    // a live stream — how long to record all have to be settled BEFORE the
    // first segment, because there is no changing your mind halfway through.
    let stream: El<'_> = if st.stream_probing {
        text(tr("Reading the stream's quality list..."))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light))
            .into()
    } else if let Some(p) = &st.stream {
        if let Some(drm) = &p.drm {
            // The same sentence the engine reports, so the dialog and the
            // download list never explain this differently.
            text(crate::engine::drm_refusal(drm))
                .size(theme::FONT_SIZE)
                .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
                .into()
        } else {
            let mut rows = column![].spacing(8);
            let kind = if p.live {
                format!("{} — {}", p.protocol.to_uppercase(), tr("live"))
            } else {
                match p.duration {
                    Some(d) => format!(
                        "{}  {}",
                        p.protocol.to_uppercase(),
                        crate::fmt::eta(d as u64)
                    ),
                    None => p.protocol.to_uppercase(),
                }
            };
            rows = rows.push(
                row![
                    text(tr("Stream"))
                        .size(theme::FONT_SIZE)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .width(70.0),
                    text(kind).size(theme::FONT_SIZE),
                    text(if p.separate_audio {
                        tr("video + audio")
                    } else {
                        String::new()
                    })
                    .size(theme::FONT_SIZE - 1.0)
                    .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            rows = rows.push(
                row![
                    text(tr("Quality"))
                        .size(theme::FONT_SIZE)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .width(70.0),
                    // Styled like every other picker in the app. Without
                    // this it falls back to iced's default surface, which is
                    // near-white and unreadable under the dark theme — this
                    // was the one picker in the app that skipped it.
                    pick_list(
                        p.qualities.clone(),
                        st.quality.clone(),
                        Message::AddrQuality
                    )
                    .text_size(theme::FONT_SIZE)
                    .style(theme::picker)
                    .padding([5, 8])
                    .width(250.0),
                    text(tr("Save as")).size(theme::FONT_SIZE),
                    pick_list(
                        // Renaming fragmented MP4 to .ts would produce a file
                        // that lies about itself, so TS is only offered when
                        // the segments really are MPEG-TS.
                        if p.is_ts {
                            vec!["MP4".to_string(), "TS".to_string()]
                        } else {
                            vec!["MP4".to_string()]
                        },
                        Some(st.container.clone()),
                        Message::AddrContainer
                    )
                    .text_size(theme::FONT_SIZE)
                    .style(theme::picker)
                    .padding([5, 8])
                    .width(100.0),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            if p.live {
                rows = rows.push(
                    row![
                        text(tr("Record"))
                            .size(theme::FONT_SIZE)
                            .wrapping(iced::widget::text::Wrapping::None)
                            .width(70.0),
                        text_input("", &st.record_minutes)
                            .on_input(Message::AddrRecordMinutes)
                            .size(theme::FONT_SIZE)
                            .style(theme::input)
                            .width(70.0),
                        text(tr(
                            "minutes, then finish the file. Leave empty to record until you press Stop."
                        ))
                        .size(theme::FONT_SIZE - 1.0)
                        .color(theme::dim_text(&iced::Theme::Light)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                );
            }
            rows.into()
        }
    } else {
        iced::widget::space::horizontal().height(0.0).into()
    };

    let error: El<'_> = match &st.error {
        Some(e) => text(e.clone())
            .size(theme::FONT_SIZE)
            .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
            .into(),
        None if !st.address.trim().is_empty()
            && crate::app::site_blocked(st.address.trim(), &app.cfg.settings.dont_start_sites) =>
        {
            text(tr(
                "This site is on the auto-start block list; it will be added, not started.",
            ))
            .size(theme::FONT_SIZE)
            .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
            .into()
        }
        None => iced::widget::space::horizontal().height(0.0).into(),
    };

    let buttons = column![
        dlg_btn_primary(tr("OK"), Some(Message::AddUrlOk)),
        dlg_btn(
            tr("Cancel"),
            app.win_of(WinKind::AddUrl).map(Message::CloseThis),
        ),
    ]
    .spacing(8);

    let left = column![address, auth, creds, stream, error]
        .spacing(10)
        .width(Length::Fill);

    container(
        column![title, row![left, buttons].spacing(16).padding([8, 0]),]
            .spacing(4)
            .padding(12),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

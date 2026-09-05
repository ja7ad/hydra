// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Enter new address to download" — Add URL dialog.

use crate::app::{App, El, Message, WinKind};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{i18n::tr, theme};
use iced::widget::{checkbox, column, container, pick_list, row, text, text_input};
use iced::Length;

/// The label column, matching the Download File Info dialog's.
const LABEL_W: f32 = 70.0;
const GAP: f32 = 8.0;

fn label<'a>(s: String) -> El<'a> {
    text(s)
        .size(theme::FONT_SIZE)
        .wrapping(iced::widget::text::Wrapping::None)
        .width(LABEL_W)
        .into()
}

pub fn view(app: &App) -> El<'_> {
    let st = &app.add_url;

    // No in-window heading: the OS title bar already names the dialog, and
    // IDM's puts the Address box on the very first line.
    let address = row![
        label(tr("Address")),
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
    .spacing(GAP)
    .align_y(iced::Alignment::Center);

    let auth = checkbox(st.use_auth)
        .label(tr("Use authorization"))
        .on_toggle(Message::AddrAuthToggled)
        .size(15.0)
        .text_size(theme::FONT_SIZE)
        .style(theme::check);

    // Login and Password stay on screen and grey out until the box is
    // ticked, the way IDM draws them: the dialog keeps one shape instead of
    // growing a row under the pointer.
    let enabled = st.use_auth;
    let cred_label = |s: String| {
        text(s)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .style(move |t: &iced::Theme| iced::widget::text::Style {
                color: Some(if enabled {
                    theme::text_color(t)
                } else {
                    theme::dim_text(t)
                }),
            })
    };
    let creds = row![
        iced::widget::space::horizontal().width(LABEL_W),
        cred_label(tr("Login")),
        text_input("", &st.login)
            .on_input_maybe(st.use_auth.then_some(Message::AddrLogin))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
        iced::widget::space::horizontal().width(16.0),
        cred_label(tr("Password")),
        text_input("", &st.password)
            .on_input_maybe(st.use_auth.then_some(Message::AddrPass))
            .secure(true)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
    ]
    .spacing(GAP)
    .align_y(iced::Alignment::Center);

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
                    label(tr("Stream")),
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
                    label(tr("Quality")),
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
                        label(tr("Record")),
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
            // A blank line under the block, so the mirror list or an error
            // line below reads as its own section.
            rows = rows.push(iced::widget::space::vertical().height(GAP));
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

    // A mirror list is not a file either, and what it changes is worth seeing
    // before OK: how many files are about to be added, how many mirrors each
    // has, and whether it can be verified per chunk. A list that silently loses
    // two thirds of its entries to a scheme this build cannot fetch is worth
    // knowing about before a multi-gigabyte download rather than after.
    let metalink: El<'_> = if st.metalink_probing {
        text(tr("Reading the mirror list..."))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light))
            .into()
    } else if let Some(m) = &st.metalink {
        let mut rows = column![].spacing(6);
        rows = rows.push(
            row![
                label(tr("Metalink")),
                text(format!(
                    "{}  —  {} {}",
                    m.version,
                    m.files.len(),
                    if m.files.len() == 1 {
                        tr("file")
                    } else {
                        tr("files")
                    }
                ))
                .size(theme::FONT_SIZE),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        // A summary, not a listing: a document with forty files would push the
        // OK button off the window. `metalink_panel_height` reserves space for
        // exactly this many rows plus the "+N" line, and the two must agree.
        for f in m.files.iter().take(crate::app::METALINK_PANEL_ROWS) {
            let usable = f.info.mirrors.len();
            let mut parts = vec![match f.info.size {
                Some(n) => crate::fmt::size2(n),
                None => tr("size not stated"),
            }];
            parts.push(format!("{usable}/{} {}", f.mirrors_listed, tr("mirrors")));
            if f.piece_count > 0 {
                parts.push(format!("{} {}", f.piece_count, tr("verified chunks")));
            } else if f.info.digest.is_some() {
                parts.push(tr("whole-file checksum"));
            } else {
                parts.push(tr("no checksum published"));
            }
            if f.info.signed {
                parts.push(tr("signed (not verified)"));
            }
            rows = rows.push(
                row![
                    iced::widget::space::horizontal().width(LABEL_W),
                    text(f.name.clone()).size(theme::FONT_SIZE),
                    text(parts.join("  ·  "))
                        .size(theme::FONT_SIZE - 1.0)
                        .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        if m.files.len() > crate::app::METALINK_PANEL_ROWS {
            rows = rows.push(
                row![
                    iced::widget::space::horizontal().width(LABEL_W),
                    text(format!(
                        "+{}",
                        m.files.len() - crate::app::METALINK_PANEL_ROWS
                    ))
                    .size(theme::FONT_SIZE - 1.0)
                    .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8),
            );
        }
        rows.into()
    } else {
        iced::widget::space::horizontal().height(0.0).into()
    };

    // A blank line under Login/Password separates the fixed part of the
    // dialog from whatever a probed address adds beneath.
    let left = column![
        address,
        auth,
        creds,
        iced::widget::space::vertical().height(GAP),
        stream,
        metalink,
        error
    ]
    .spacing(GAP)
    .width(Length::Fill);

    // OK over Cancel on the right, level with the Address box, as IDM's.
    container(row![left, buttons].spacing(16).padding(12))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two dialogs sharing one state:
//!
//! * New download — "Download File Info": category/save-as/description while
//!   the transfer already runs in the background.
//! * Existing download — "File Properties": status/size, editable Address
//!   (switch mirrors and CONTINUE the same bytes), login/password, cookies,
//!   last try and result.

use crate::app::{App, El, Message};
use crate::model::DlState;
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, icons, theme};
use iced::widget::{checkbox, column, container, pick_list, row, svg, text, text_input};
use iced::Length;

fn labeled<'a>(label: String, content: El<'a>) -> El<'a> {
    row![
        text(label)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(125.0),
        content,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let st = &app.file_info;
    let item = app.item(st.dl);
    let size_txt = item
        .and_then(|d| d.size)
        .map(|s| format!("{} ({s} Bytes)", fmt::size2(s)))
        .unwrap_or_else(|| "?".into());
    let cats: Vec<String> = app.cfg.categories.iter().map(|c| c.name.clone()).collect();
    let complete = item.map(|d| d.state == DlState::Complete).unwrap_or(false);

    let mut form = column![].spacing(10).width(Length::Fill);

    if !st.is_new {
        form = form.push(labeled(
            tr("Status:"),
            text(item.map(|d| d.status_text()).unwrap_or_default())
                .size(theme::FONT_SIZE)
                .into(),
        ));
        form = form.push(labeled(
            tr("Size:"),
            text(size_txt.clone()).size(theme::FONT_SIZE).into(),
        ));
    }

    // Address: read-only while the fresh probe is running, editable on an
    // existing entry — that edit is the mirror switch.
    let addr: El<'_> = if st.is_new {
        text_input("", &st.url)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill)
            .into()
    } else {
        text_input("", &st.url)
            .on_input(Message::FiUrl)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill)
            .into()
    };
    form = form.push(labeled(
        tr("Address:"),
        crate::windows::ext_hint(addr, &st.url),
    ));

    form = form.push(labeled(
        tr("Category"),
        pick_list(cats, Some(st.category.clone()), Message::FiCategory)
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(280.0)
            .into(),
    ));
    form = form.push(labeled(
        tr("Save As"),
        row![
            text_input("", &st.save_dir)
                .on_input(Message::FiSaveDir)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            dlg_btn(tr("Browse"), Some(Message::FiBrowseDir)),
        ]
        .spacing(8)
        .into(),
    ));
    form = form.push(labeled(
        tr("File name"),
        text_input("", &st.file_name)
            .on_input(Message::FiFileName)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill)
            .into(),
    ));
    if st.is_new {
        form = form.push(
            // Off — and honestly so — for a file type that is not in
            // Options > File types: nothing is running behind this dialog.
            checkbox(app.cfg.settings.bg_download && !st.bg_blocked)
                .label(tr("Download in background while choosing options"))
                .on_toggle(Message::FiBgToggle)
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
        );
        // Name the folder the tick actually writes: General while
        // category folders are switched off (see FiStartDownload).
        let remember_cat = if app.cfg.settings.no_category_dirs {
            app.cfg
                .categories
                .first()
                .map(|c| c.name.as_str())
                .unwrap_or(st.category.as_str())
        } else {
            st.category.as_str()
        };
        form = form.push(
            checkbox(st.remember)
                .label(format!(
                    "{} \"{}\" {}",
                    tr("Remember this path for"),
                    remember_cat,
                    tr("category")
                ))
                .on_toggle(Message::FiRemember)
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
        );
    }
    form = form.push(labeled(
        tr("Description"),
        text_input("", &st.description)
            .on_input(Message::FiDescription)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill)
            .into(),
    ));

    if !st.is_new {
        form = form.push(labeled(
            tr("Login"),
            row![
                text_input("", &st.login)
                    .on_input(Message::FiLogin)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(180.0),
                text(tr("Password")).size(theme::FONT_SIZE),
                text_input("", &st.password)
                    .on_input(Message::FiPass)
                    .secure(true)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(180.0),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        ));
        form = form.push(labeled(
            tr("Cookies"),
            text_input("name=value; name2=value2", &st.cookies)
                .on_input(Message::FiCookies)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill)
                .into(),
        ));
        let last_try = item
            .and_then(|d| d.last_try)
            .map(fmt::date)
            .unwrap_or_default();
        form = form.push(labeled(
            tr("Last try date:"),
            text(last_try).size(theme::FONT_SIZE).into(),
        ));
        if let Some(err) = item.and_then(|d| d.error.clone()) {
            form = form.push(labeled(
                tr("Result:"),
                text(err)
                    .size(theme::FONT_SIZE)
                    .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
                    .into(),
            ));
        }
    }

    // Preview box: big file-type icon with the size under it.
    let side = column![
        iced::widget::space::vertical().height(26.0),
        svg(crate::ui::categories::cat_icon(&st.category))
            .width(64.0)
            .height(64.0),
        iced::widget::space::vertical().height(6.0),
        text(
            item.and_then(|d| d.size)
                .map(fmt::size2)
                .unwrap_or_else(|| "?".into())
        )
        .size(theme::FONT_SIZE),
    ]
    .spacing(0)
    .align_x(iced::Alignment::Center)
    .width(110.0);

    let buttons: El<'_> = if st.is_new {
        row![
            dlg_btn(tr("Download Later"), Some(Message::FiDownloadLater)),
            dlg_btn_primary(tr("Start Download"), Some(Message::FiStartDownload)),
            dlg_btn(tr("Cancel"), Some(Message::FiCancel)),
        ]
        .spacing(10)
        .into()
    } else {
        row![
            dlg_btn(tr("Open"), complete.then_some(Message::OpenFile(st.dl))),
            dlg_btn(tr("Start Download"), Some(Message::FiStartDownload)),
            dlg_btn_primary(tr("OK"), Some(Message::FiDownloadLater)),
        ]
        .spacing(10)
        .into()
    };

    let title = if st.is_new {
        tr("Download File Info")
    } else {
        tr("File Properties")
    };
    container(
        column![
            row![
                svg(icons::download()).width(20.0).height(20.0),
                text(title).size(theme::FONT_SIZE + 1.0),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            row![form, side].spacing(14),
            row![
                iced::widget::space::horizontal(),
                buttons,
                iced::widget::space::horizontal()
            ]
            .width(Length::Fill),
        ]
        .spacing(14)
        .padding(14),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

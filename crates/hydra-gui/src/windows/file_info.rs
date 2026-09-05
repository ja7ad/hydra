// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two dialogs sharing one state, laid out "Download File Info":
//! a narrow label column, one Save As box holding the whole path with a
//! "..." save dialog beside it, the folder the tick would remember shown
//! greyed under the checkbox, the file-type icon and size in a side column,
//! and the buttons left-aligned under the fields.
//!
//! * New download — "Download File Info": category/save-as/description while
//!   the transfer already runs in the background.
//! * Existing download — "File Properties": status/size, editable Address
//!   (switch mirrors and CONTINUE the same bytes), login/password, cookies,
//!   last try and result.

use crate::app::{App, El, Message};
use crate::model::DlState;
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, theme};
use iced::widget::{button, checkbox, column, container, pick_list, row, svg, text, text_input};
use iced::Length;

/// The label column: is about this wide at the same font size.
const LABEL_W: f32 = 84.0;
const GAP: f32 = 8.0;

fn labeled<'a>(label: String, content: El<'a>) -> El<'a> {
    row![
        text(label)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(LABEL_W),
        content,
    ]
    .spacing(GAP)
    .align_y(iced::Alignment::Center)
    .into()
}

/// A row that starts at the field column (no label).
fn indented(content: El) -> El {
    row![iced::widget::space::horizontal().width(LABEL_W), content]
        .spacing(GAP)
        .align_y(iced::Alignment::Center)
        .into()
}

fn field<'a>(value: &str, on_input: fn(String) -> Message) -> text_input::TextInput<'a, Message> {
    text_input("", value)
        .on_input(on_input)
        .size(theme::FONT_SIZE)
        .style(theme::input)
        .width(Length::Fill)
}

/// small square "..." button beside Save As.
fn small_btn<'a>(label: &'a str, msg: Message) -> El<'a> {
    button(
        text(label)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .padding([3, 9])
    .style(theme::btn)
    .on_press(msg)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let st = &app.file_info;
    let item = app.item(st.dl);
    let size = item.and_then(|d| d.size);
    let cats: Vec<String> = app.cfg.categories.iter().map(|c| c.name.clone()).collect();
    let complete = item.map(|d| d.state == DlState::Complete).unwrap_or(false);

    let mut form = column![].spacing(GAP).width(Length::Fill);

    if !st.is_new {
        form = form.push(labeled(
            tr("Status:"),
            text(item.map(|d| d.status_text()).unwrap_or_default())
                .size(theme::FONT_SIZE)
                .into(),
        ));
        form = form.push(labeled(
            tr("Size:"),
            text(
                size.map(|s| format!("{} ({s} Bytes)", fmt::size2(s)))
                    .unwrap_or_else(|| "?".into()),
            )
            .size(theme::FONT_SIZE)
            .into(),
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
        field(&st.url, Message::FiUrl).into()
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
            .width(200.0)
            .into(),
    ));
    form = form.push(labeled(
        tr("Save As"),
        row![
            field(&st.save_as(), Message::FiSaveAs),
            small_btn("...", Message::FiBrowse),
        ]
        .spacing(GAP)
        .align_y(iced::Alignment::Center)
        .into(),
    ));
    if st.is_new {
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
        form = form.push(indented(
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
                .style(theme::check)
                .into(),
        ));
        // The folder that tick would store, greyed out.
        form = form.push(indented(
            text_input("", &st.save_dir)
                .size(theme::FONT_SIZE)
                .style(|t, s| {
                    let mut style = theme::input(t, s);
                    style.value = theme::dim_text(t);
                    style
                })
                .width(Length::Fill)
                .into(),
        ));
        form = form.push(indented(
            // Off — and honestly so — for a file type that is not in
            // Options > File types: nothing is running behind this dialog.
            checkbox(app.cfg.settings.bg_download && !st.bg_blocked)
                .label(tr("Download in background while choosing options"))
                .on_toggle(Message::FiBgToggle)
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check)
                .into(),
        ));
    }
    form = form.push(labeled(
        tr("Description"),
        field(&st.description, Message::FiDescription).into(),
    ));

    if !st.is_new {
        form = form.push(labeled(
            tr("Login"),
            row![
                text_input("", &st.login)
                    .on_input(Message::FiLogin)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(Length::Fill),
                text(tr("Password")).size(theme::FONT_SIZE),
                text_input("", &st.password)
                    .on_input(Message::FiPass)
                    .secure(true)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(Length::Fill),
            ]
            .spacing(GAP)
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

    // Side column: file-type icon with the size under it, level with the
    // Category/Save As rows, and Preview button beneath — live for an
    // archive whose index can be read off its tail (see `engine::peek_zip`),
    // greyed for everything else so the column keeps one shape.
    let previewable = hya_net::zipdir::is_zip_name(&st.file_name);
    let side = column![
        iced::widget::space::vertical().height(if st.is_new { 30.0 } else { 86.0 }),
        svg(crate::ui::categories::cat_icon(&st.category))
            .width(48.0)
            .height(48.0),
        text(size.map(fmt::size2).unwrap_or_else(|| "?".into())).size(theme::FONT_SIZE),
        iced::widget::space::vertical().height(6.0),
        button(crate::windows::centered(tr("Preview"), theme::FONT_SIZE))
            .padding([4, 8])
            .width(88.0)
            .style(theme::btn)
            .on_press_maybe(previewable.then_some(Message::FiPreview)),
    ]
    .spacing(6)
    .align_x(iced::Alignment::Center)
    .width(96.0);

    let buttons: El<'_> = if st.is_new {
        row![
            dlg_btn(tr("Download Later"), Some(Message::FiDownloadLater)),
            dlg_btn_primary(tr("Start Download"), Some(Message::FiStartDownload)),
            dlg_btn(tr("Cancel"), Some(Message::FiCancel)),
        ]
        .spacing(GAP)
        .into()
    } else {
        row![
            dlg_btn(tr("Open"), complete.then_some(Message::OpenFile(st.dl))),
            dlg_btn(tr("Start Download"), Some(Message::FiStartDownload)),
            dlg_btn_primary(tr("OK"), Some(Message::FiDownloadLater)),
        ]
        .spacing(GAP)
        .into()
    };

    // Buttons centred under the fields, inside the form so the side column
    // does not pull them off-centre.
    form = form.push(
        column![
            iced::widget::space::vertical().height(4.0),
            row![
                iced::widget::space::horizontal().width(LABEL_W + GAP),
                iced::widget::space::horizontal(),
                buttons,
                iced::widget::space::horizontal(),
            ],
        ]
        .width(Length::Fill),
    );

    // No in-window heading: the OS title bar already names the dialog.
    container(row![form, side].spacing(GAP).padding(14))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

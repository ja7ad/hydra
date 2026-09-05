// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Scheduler window: queues on the left; Schedule and "Files in the
//! queue" tabs on the right.

use crate::app::{App, El, Message, SchField, SchTab, WinKind};
use crate::model::{DlState, PowerAction, QueueDef};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, icons, theme};
use iced::widget::{
    button, checkbox, column, container, mouse_area, radio, row, scrollable, svg, text, text_input,
};

use iced::Length;

fn s(f: SchField) -> Message {
    Message::SchField(f)
}

/// Width of the queue list box; the New queue / Delete pair shares it.
const LIST_W: f32 = 240.0;

/// Vertical gap between the schedule tab's rows: a checkbox row is only
/// 18 px, and at the ordinary dialog gap the ticks read as one dense block.
const ROW_GAP: f32 = 16.0;

/// A dialog button that takes the width it is given rather than the fixed
/// dialog-button width, for a row sized to something else.
fn wide_btn<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(crate::windows::centered(label, theme::FONT_SIZE))
        .padding([5, 8])
        .width(Length::Fill)
        .style(theme::btn);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

fn queue_list(app: &App) -> El<'_> {
    let mut list = column![].spacing(2);
    for q in &app.cfg.queues {
        let selected = app.sch.queue == q.name;
        // Renaming in place: the selected row becomes the edit field.
        if selected && app.sch.renaming {
            list = list.push(
                row![
                    svg(icons::queue_folder(q.color)).width(16.0).height(16.0),
                    text_input("", &app.sch.rename_draft)
                        .id("sch-rename")
                        .on_input(Message::SchNameDraft)
                        .on_submit(Message::SchNameCommit)
                        .size(theme::FONT_SIZE)
                        .style(theme::input)
                        .width(Length::Fill),
                ]
                .spacing(6)
                .padding([2, 6])
                .align_y(iced::Alignment::Center),
            );
            continue;
        }
        // Double-click renames — the pair is timed in the `SchQueue` handler,
        // not by a `mouse_area`: this button captures the mouse event, so an
        // enclosing area's `on_double_click` would never be reached. Stock
        // queues (Main/Synchronization) are excluded in the handler.
        list = list.push(
            button(
                row![
                    svg(icons::queue_folder(q.color)).width(16.0).height(16.0),
                    text(tr(&q.name)).size(theme::FONT_SIZE),
                    iced::widget::space::horizontal(),
                    text(if q.running { "▶" } else { "" }).size(theme::FONT_SIZE - 2.0),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            )
            .padding([3, 6])
            .width(Length::Fill)
            .style(theme::btn_row(selected))
            .on_press(Message::SchQueue(q.name.clone())),
        );
    }
    column![
        row![
            svg(icons::scheduler(true)).width(28.0).height(28.0),
            text(tr("Queues")).size(theme::FONT_SIZE + 2.0),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        container(scrollable(list).height(Length::Fill))
            .padding(4)
            .width(LIST_W)
            .height(Length::Fill)
            .style(theme::panel),
        // The pair spans exactly the list box above it, half each.
        row![
            wide_btn(tr("New queue"), Some(Message::SchNewQueue)),
            wide_btn(tr("Delete"), Some(Message::SchDeleteQueue)),
        ]
        .spacing(8)
        .width(LIST_W),
    ]
    .spacing(8)
    .into()
}

fn schedule_tab<'a>(q: &'a QueueDef) -> El<'a> {
    let sc = &q.schedule;
    let days = [
        ("Sunday", 0usize),
        ("Monday", 1),
        ("Tuesday", 2),
        ("Wednesday", 3),
        ("Thursday", 4),
        ("Friday", 5),
        ("Saturday", 6),
    ];
    let mut day_cols = row![].spacing(14);
    for chunk in days.chunks(3) {
        let mut c = column![].spacing(8);
        for (name, i) in chunk {
            let i = *i;
            c = c.push(
                checkbox(sc.days[i])
                    .label(tr(name))
                    .on_toggle(move |b| s(SchField::Day(i, b)))
                    .size(15.0)
                    .text_size(theme::FONT_SIZE)
                    .style(theme::check),
            );
        }
        day_cols = day_cols.push(c);
    }

    let mut col = column![
        row![
            radio(tr("One-time downloading"), false, Some(sc.periodic), |v| {
                s(SchField::Periodic(v))
            })
            .size(15.0)
            .text_size(theme::FONT_SIZE),
            radio(
                tr("Periodic synchronization"),
                true,
                Some(sc.periodic),
                |v| { s(SchField::Periodic(v)) }
            )
            .size(15.0)
            .text_size(theme::FONT_SIZE),
        ]
        .spacing(30),
        checkbox(sc.start_on_startup)
            .label(tr("Start download on Hydra startup"))
            .on_toggle(|b| s(SchField::OnStartup(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        row![
            checkbox(sc.start_enabled)
                .label(tr("Start download at"))
                .on_toggle(|b| s(SchField::StartEnabled(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            text_input("23:00", &sc.start_at)
                .on_input(|v| s(SchField::StartAt(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(90.0),
            text(tr("(HH:MM)")).size(theme::FONT_SIZE - 1.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            radio(tr("Once"), true, Some(sc.once), |v| s(SchField::Once(v)))
                .size(15.0)
                .text_size(theme::FONT_SIZE),
            radio(tr("Daily"), false, Some(sc.once), |v| s(SchField::Once(v)))
                .size(15.0)
                .text_size(theme::FONT_SIZE),
        ]
        .spacing(30),
        day_cols,
        row![
            checkbox(sc.stop_enabled)
                .label(tr("Stop download at"))
                .on_toggle(|b| s(SchField::StopEnabled(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            text_input("07:30", &sc.stop_at)
                .on_input(|v| s(SchField::StopAt(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(90.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            checkbox(sc.retries_enabled)
                .label(tr(
                    "Number of retries for each file if downloading failed :"
                ))
                .on_toggle(|b| s(SchField::RetriesEnabled(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            text_input("10", &sc.retries.to_string())
                .on_input(|v| s(SchField::Retries(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(70.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            checkbox(sc.open_file_enabled)
                .label(tr("Open the following file when done:"))
                .on_toggle(|b| s(SchField::OpenFileEnabled(b)))
                .size(15.0)
                .text_size(theme::FONT_SIZE)
                .style(theme::check),
            text_input("", &sc.open_file)
                .on_input(|v| s(SchField::OpenFile(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        checkbox(sc.exit_when_done)
            .label(tr("Exit Hydra when done"))
            .on_toggle(|b| s(SchField::ExitDone(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        checkbox(sc.shutdown_when_done)
            .label(tr("Shut down / log off / sleep computer when done"))
            .on_toggle(|b| s(SchField::ShutdownDone(b)))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
    ]
    .spacing(ROW_GAP);

    if sc.shutdown_when_done {
        col = col.push(
            row![
                radio(
                    tr("Shutdown"),
                    PowerAction::Shutdown,
                    Some(sc.shutdown_action),
                    |v| s(SchField::ShutdownAction(v)),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
                radio(
                    tr("Log off"),
                    PowerAction::LogOff,
                    Some(sc.shutdown_action),
                    |v| s(SchField::ShutdownAction(v)),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
                radio(
                    tr("Sleep"),
                    PowerAction::Sleep,
                    Some(sc.shutdown_action),
                    |v| s(SchField::ShutdownAction(v)),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
            ]
            .spacing(30),
        );
    }

    col.into()
}

fn files_tab(app: &App) -> El<'_> {
    let name = app.sch.queue.clone();
    let q = app.cfg.queues.iter().find(|q| q.name == name);
    let files_at_once = q.map(|q| q.files_at_once).unwrap_or(4);

    let mut members: Vec<&crate::model::DownloadItem> = app
        .state
        .downloads
        .iter()
        .filter(|d| d.queue.as_deref() == Some(name.as_str()))
        .collect();
    members.sort_by_key(|d| d.q_order);

    let mut list = column![].spacing(1);
    list = list.push(
        row![
            container(text(tr("File Name")).size(theme::FONT_SIZE)).width(Length::Fill),
            container(text(tr("Size")).size(theme::FONT_SIZE)).width(100.0),
            container(text(tr("Status")).size(theme::FONT_SIZE)).width(100.0),
            container(text(tr("Time left")).size(theme::FONT_SIZE)).width(100.0),
        ]
        .spacing(4)
        .padding([2, 4]),
    );
    for d in &members {
        let selected = app.sch.sel_file == Some(d.id);
        let dragging = app.sch.drag == Some(d.id);
        // Press starts a drag (and selects); dragging over another row
        // reorders live; global mouse-up ends the drag.
        list = list.push(
            mouse_area(
                container(
                    row![
                        container(text(d.file_name.clone()).size(theme::FONT_SIZE))
                            .width(Length::Fill),
                        container(
                            text(d.size.map(fmt::size2).unwrap_or_default()).size(theme::FONT_SIZE)
                        )
                        .width(100.0),
                        container(text(d.status_text()).size(theme::FONT_SIZE)).width(100.0),
                        container(
                            text(match d.state {
                                DlState::Receiving => d.eta_secs.map(fmt::eta).unwrap_or_default(),
                                _ => String::new(),
                            })
                            .size(theme::FONT_SIZE)
                        )
                        .width(100.0),
                    ]
                    .spacing(4),
                )
                .padding([1, 4])
                .width(Length::Fill)
                .style(move |t: &iced::Theme| container::Style {
                    background: Some(iced::Background::Color(if selected || dragging {
                        if theme::is_dark(t) {
                            iced::Color::from_rgb8(0x26, 0x4F, 0x78)
                        } else {
                            iced::Color::from_rgb8(0xCC, 0xE8, 0xFF)
                        }
                    } else {
                        iced::Color::TRANSPARENT
                    })),
                    text_color: Some(theme::text_color(t)),
                    ..Default::default()
                }),
            )
            .interaction(iced::mouse::Interaction::Grabbing)
            .on_press(Message::SchDragStart(d.id))
            .on_enter(Message::SchDragOver(d.id)),
        );
    }

    column![
        row![
            text(tr("Download")).size(theme::FONT_SIZE),
            text_input("4", &files_at_once.to_string())
                .on_input(|v| s(SchField::FilesAtOnce(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(50.0),
            text(tr("files at the same time")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        container(scrollable(list).height(Length::Fill))
            .padding(4)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::panel),
        row![
            dlg_btn(tr("⬇"), Some(Message::SchFileDown)),
            dlg_btn(tr("⬆"), Some(Message::SchFileUp)),
            dlg_btn(tr("✕"), Some(Message::SchFileRemove)),
        ]
        .spacing(8),
    ]
    .spacing(8)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let name = app.sch.queue.clone();
    let q = app
        .cfg
        .queues
        .iter()
        .find(|q| q.name == name)
        .or_else(|| app.cfg.queues.first());
    let Some(q) = q else {
        return container(text("")).style(theme::window).into();
    };

    let tabs = row![
        button(text(tr("Schedule")).size(theme::FONT_SIZE))
            .padding([4, 12])
            .style(theme::btn_tab(app.sch.tab == SchTab::Schedule))
            .on_press(Message::SchTabSet(SchTab::Schedule)),
        button(text(tr("Files in the queue")).size(theme::FONT_SIZE))
            .padding([4, 12])
            .style(theme::btn_tab(app.sch.tab == SchTab::Files))
            .on_press(Message::SchTabSet(SchTab::Files)),
    ]
    .spacing(1);

    let body: El<'_> = match app.sch.tab {
        SchTab::Schedule => schedule_tab(q),
        SchTab::Files => files_tab(app),
    };

    let right = column![
        row![
            iced::widget::space::horizontal(),
            text(tr(&q.name)).size(theme::FONT_SIZE + 2.0),
            iced::widget::space::horizontal(),
        ]
        .width(Length::Fill),
        tabs,
        container(body)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::panel),
        // One run of buttons at the right, as IDM lays its row out: the
        // queue's actions first, Close last.
        row![
            iced::widget::space::horizontal(),
            dlg_btn_primary(tr("Start now"), Some(Message::SchStartNow)),
            dlg_btn(tr("Stop"), Some(Message::SchStop)),
            dlg_btn(
                tr("Close"),
                app.win_of(WinKind::Scheduler).map(Message::CloseThis)
            ),
        ]
        .spacing(10),
    ]
    .spacing(8)
    .width(Length::Fill)
    .height(Length::Fill);

    container(
        row![queue_list(app), right]
            .spacing(12)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}

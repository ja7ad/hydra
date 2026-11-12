// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Scheduler window: queues on the left; Schedule and "Files in the
//! queue" tabs on the right.

use crate::app::{App, El, Message, SchField, SchTab, WinKind};
use crate::model::{DlState, PowerAction, QueueDef};
use crate::windows::{cell, check, dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, icons, theme};
use iced::widget::{
    button, column, container, mouse_area, radio, row, scrollable, svg, text, text_input,
};

use iced::Length;

fn s(f: SchField) -> Message {
    Message::SchField(f)
}

/// Width of the queue list box; the New queue / Delete pair shares it.
const LIST_W: f32 = 240.0;

const COL_GRIP: f32 = 6.0;

/// Vertical gap between the schedule tab's rows: a checkbox row is only
/// 18 px, and at the ordinary dialog gap the ticks read as one dense block.
const ROW_GAP: f32 = 16.0;

/// Spin-arrow geometry: the pair stacks to the height of the text input it
/// sits against — `FONT_SIZE` plus the input's own padding — so the box and
/// its arrows read as one control.
const ARROW_W: f32 = 16.0;
const ARROW_H: f32 = 13.0;

/// One step of a number field, or `None` at the end of its range — an arrow
/// with nothing to move to is disabled rather than firing a change that
/// clamps straight back.
fn step(value: u32, up: bool, range: &std::ops::RangeInclusive<u32>) -> Option<u32> {
    let next = if up {
        value.checked_add(1)?
    } else {
        value.checked_sub(1)?
    };
    range.contains(&next).then_some(next)
}

/// A number field with the spin arrows beside it. Typing still works; the
/// arrows are there because the field only takes text that parses, so a box
/// cleared to retype keeps showing the old number and a value cannot be
/// nudged without retyping it whole.
fn spin<'a>(
    value: u32,
    range: std::ops::RangeInclusive<u32>,
    field: fn(String) -> SchField,
    width: f32,
) -> El<'a> {
    let arrow = |glyph: &'static str, next: Option<u32>| -> El<'a> {
        let mut b =
            button(container(text(glyph).size(theme::FONT_SIZE - 5.0)).center(Length::Fill))
                .padding(0)
                .width(ARROW_W)
                .height(ARROW_H)
                .style(theme::btn);
        if let Some(n) = next {
            b = b.on_press(s(field(n.to_string())));
        }
        b.into()
    };
    row![
        text_input("", &value.to_string())
            .on_input(move |v| s(field(v)))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(width),
        column![
            arrow("▲", step(value, true, &range)),
            arrow("▼", step(value, false, &range)),
        ]
        .spacing(1),
    ]
    .spacing(2)
    .align_y(iced::Alignment::Center)
    .into()
}

/// A time box goes red while what is in it names no minute: a queue
/// scheduled for "9.00" or "25:00" would otherwise sit there never firing.
fn time_style(
    typed: &str,
) -> fn(&iced::Theme, iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    if crate::model::parse_hhmm(typed).is_some() {
        theme::input
    } else {
        theme::input_invalid
    }
}

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
    let mut list = column![].spacing(2).width(Length::Fill);
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
        container(
            scrollable(container(list).padding(iced::padding::right(14).bottom(14)))
                .width(Length::Fill)
                .height(Length::Fill),
        )
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
            c = c.push(check(sc.days[i], tr(name)).on_toggle(move |b| s(SchField::Day(i, b))));
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
        check(sc.start_on_startup, tr("Start download on Hydra startup"))
            .on_toggle(|b| s(SchField::OnStartup(b))),
        row![
            check(sc.start_enabled, tr("Start download at"))
                .on_toggle(|b| s(SchField::StartEnabled(b))),
            text_input("23:00", &sc.start_at)
                .on_input(|v| s(SchField::StartAt(v)))
                .size(theme::FONT_SIZE)
                .style(time_style(&sc.start_at))
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
            check(sc.stop_enabled, tr("Stop download at"))
                .on_toggle(|b| s(SchField::StopEnabled(b))),
            text_input("07:30", &sc.stop_at)
                .on_input(|v| s(SchField::StopAt(v)))
                .size(theme::FONT_SIZE)
                .style(time_style(&sc.stop_at))
                .width(90.0),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            check(
                sc.retries_enabled,
                tr("Number of retries for each file if downloading failed :")
            )
            .on_toggle(|b| s(SchField::RetriesEnabled(b))),
            spin(
                sc.retries,
                crate::model::QUEUE_RETRIES,
                SchField::Retries,
                70.0,
            ),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            check(
                sc.open_file_enabled,
                tr("Open the following file when done:")
            )
            .on_toggle(|b| s(SchField::OpenFileEnabled(b))),
            text_input("", &sc.open_file)
                .on_input(|v| s(SchField::OpenFile(v)))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        check(sc.exit_when_done, tr("Exit Hydra when done"))
            .on_toggle(|b| s(SchField::ExitDone(b))),
        check(
            sc.shutdown_when_done,
            tr("Shut down / log off / sleep computer when done")
        )
        .on_toggle(|b| s(SchField::ShutdownDone(b))),
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

/// Cell `index` of a queue member's row, as [`files_tab`] paints it and
/// [`fit_width`] measures it.
fn file_text(d: &crate::model::DownloadItem, index: usize) -> String {
    match index {
        0 => d.file_name.clone(),
        1 => d.size.map(fmt::size2).unwrap_or_default(),
        2 => d.status_text(),
        _ => match d.state {
            DlState::Receiving => d.eta_secs.map(fmt::eta).unwrap_or_default(),
            _ => String::new(),
        },
    }
}

/// The four cells of a queue member's row.
fn file_texts(d: &crate::model::DownloadItem) -> [String; 4] {
    std::array::from_fn(|i| file_text(d, i))
}

const FILE_HEADERS: [&str; 4] = ["File Name", "Size", "Status", "Time Left"];

/// Whether `d` belongs to the queue selected on the left: a row
/// [`files_tab`] paints and [`fit_width`] measures.
fn in_selected_queue(app: &App, d: &crate::model::DownloadItem) -> bool {
    d.queue.as_deref() == Some(app.sch.queue.as_str())
}

/// Width at which Files-tab column `index` shows its header and every file
/// of the selected queue untruncated, within the draggable range. `None` for
/// an index past the last column.
pub fn fit_width(app: &App, index: usize) -> Option<f32> {
    let header = FILE_HEADERS.get(index)?;
    // The header cell is narrower than its column by the divider's width.
    let mut w = crate::font::line_width(&tr(header), theme::FONT_SIZE) + COL_GRIP;
    for d in app
        .state
        .downloads
        .iter()
        .filter(|d| in_selected_queue(app, d))
    {
        let text = file_text(d, index);
        w = w.max(crate::font::line_width(&text, theme::FONT_SIZE));
    }
    // `cell` has no padding; one pixel each side is rounding slack, so a text
    // measured at a fractional width is never cut by its last pixel.
    Some(crate::model::clamp_col_width(w.ceil() + 2.0))
}

fn files_tab(app: &App) -> El<'_> {
    let q = app.cfg.queues.iter().find(|q| q.name == app.sch.queue);
    let files_at_once = q.map(|q| q.files_at_once).unwrap_or(4);

    let mut members: Vec<&crate::model::DownloadItem> = app
        .state
        .downloads
        .iter()
        .filter(|d| in_selected_queue(app, d))
        .collect();
    members.sort_by_key(|d| d.q_order);

    let widths = app.cfg.settings.scheduler_column_widths;
    let table_width = widths.iter().sum::<f32>() + 8.0;
    let mut header = row![].spacing(0);
    for (index, (label, width)) in FILE_HEADERS.into_iter().zip(widths).enumerate() {
        header = header.push(cell(tr(label), width - COL_GRIP)).push(
            mouse_area(
                container(iced::widget::rule::vertical(1))
                    .center_x(COL_GRIP)
                    .height(20.0),
            )
            .interaction(iced::mouse::Interaction::ResizingHorizontally)
            .on_press(Message::SchColResizeStart(index))
            .on_double_click(Message::SchColAutoFit(index)),
        );
    }
    let mut list = column![header.padding([2, 4])]
        .spacing(1)
        .width(table_width);
    for d in &members {
        let [name, size, status, time_left] = file_texts(d);
        let selected = app.sch.sel_file == Some(d.id);
        let dragging = app.sch.drag == Some(d.id);
        // Press starts a drag (and selects); dragging over another row
        // reorders live; global mouse-up ends the drag.
        list = list.push(
            mouse_area(
                container(
                    row![
                        cell(name, widths[0]),
                        cell(size, widths[1]),
                        cell(status, widths[2]),
                        cell(time_left, widths[3]),
                    ]
                    .spacing(0),
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
            spin(
                files_at_once,
                crate::model::FILES_AT_ONCE,
                SchField::FilesAtOnce,
                50.0,
            ),
            text(tr("files at the same time")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        container(
            scrollable(container(list).padding(iced::padding::right(14).bottom(14)))
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .height(Length::Fill),
        )
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
            dlg_btn(tr("Save"), Some(Message::SchSave)),
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

    column![
        container(
            row![queue_list(app), right]
                .spacing(12)
                .padding(12)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window),
        crate::ui::probe::cursor_probe(app.cursor_cell.clone()),
    ]
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arrow_at_the_end_of_its_range_has_nowhere_to_go() {
        let r = crate::model::FILES_AT_ONCE;
        assert_eq!(step(4, true, &r), Some(5));
        assert_eq!(step(4, false, &r), Some(3));
        assert_eq!(step(*r.end(), true, &r), None);
        assert_eq!(step(*r.start(), false, &r), None);
    }

    /// A config file written by hand can hold a number below the field's
    /// floor. The up arrow is then the way out of it, not one more control
    /// that does nothing.
    #[test]
    fn a_value_under_the_range_can_still_be_stepped_into_it() {
        let r = crate::model::QUEUE_RETRIES;
        assert_eq!(step(0, true, &r), Some(1));
        assert_eq!(step(0, false, &r), None);
    }

    /// An arrow with no `on_press` is a different widget tree from one with
    /// it, and a tree that fails to build takes the window down with it.
    #[test]
    fn the_spin_box_builds_at_both_ends_of_its_range() {
        let r = crate::model::FILES_AT_ONCE;
        let _floor: El<'_> = spin(*r.start(), r.clone(), SchField::FilesAtOnce, 50.0);
        let _ceiling: El<'_> = spin(*r.end(), r, SchField::FilesAtOnce, 50.0);
    }
}

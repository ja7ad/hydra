// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The download-progress window: Download status / Speed Limiter / Options on
//! completion tabs, overall progress bar, the "start positions and download
//! progress by connections" strip, and the per-connection table.

use crate::app::{App, El, Message, ProgTab, ScanState};
use crate::model::{DlState, DownloadItem, PowerAction};
use crate::windows::{dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, theme};
use iced::widget::{
    button, checkbox, column, container, progress_bar, radio, row, scrollable, text, text_input,
};
use iced::Length;

fn tab_btn<'a>(label: String, selected: bool, msg: Message) -> El<'a> {
    button(text(label).size(theme::FONT_SIZE))
        .padding([4, 12])
        .style(theme::btn_tab(selected))
        .on_press(msg)
        .into()
}

fn kv<'a>(k: String, v: String) -> El<'a> {
    row![
        text(k).size(theme::FONT_SIZE).width(130.0),
        text(v).size(theme::FONT_SIZE),
    ]
    .spacing(8)
    .into()
}

fn status_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let recording = d.stream.as_ref().is_some_and(|s| s.live);
    let pct = d
        .size
        .map(|s| fmt::pct(d.downloaded, s))
        .unwrap_or_default();
    column![
        crate::windows::ext_hint(
            text(d.url.clone())
                .size(theme::FONT_SIZE - 1.0)
                .wrapping(iced::widget::text::Wrapping::None),
            &d.url,
        ),
        row![
            text(tr("Status")).size(theme::FONT_SIZE).width(130.0),
            text(if d.status_line.is_empty() {
                d.status_text()
            } else {
                d.status_line.clone()
            })
            .size(theme::FONT_SIZE)
            // A virus verdict is the one status worth shouting: red for
            // "infected" or a scanner that could not run, the usual blue for
            // everything else, scanning included.
            .color(match app.scans.get(&d.id) {
                Some(st) if !st.running() => iced::Color::from_rgb8(0xC4, 0x1F, 0x1F),
                _ => iced::Color::from_rgb8(0x1F, 0x3F, 0xC4),
            }),
        ]
        .spacing(8),
        // A live recording has no size and never will: it ends when someone
        // stops it. Reporting "?" against a label that expects a number says
        // nothing, so the row becomes the one measure that IS knowable —
        // how much media is on disk.
        if recording {
            let done = d
                .recorded_secs
                .map(|s| fmt::eta(s as u64))
                .unwrap_or_else(|| "0:00".into());
            kv(
                tr("Recorded"),
                match d.stream.as_ref().and_then(|s| s.max_seconds) {
                    // Asked for a fixed length: say how much of it is done.
                    Some(limit) => format!("{done} / {}", fmt::eta(limit)),
                    None => done,
                },
            )
        } else {
            kv(
                tr("File size"),
                d.size.map(fmt::size3).unwrap_or_else(|| "?".into()),
            )
        },
        kv(
            tr("Downloaded"),
            format!(
                "{}{}",
                fmt::size3(d.downloaded),
                if pct.is_empty() {
                    String::new()
                } else {
                    format!("  ( {pct} )")
                }
            ),
        ),
        kv(
            tr("Transfer rate"),
            fmt::rate_capped(d.rate, app.effective_limit(d))
        ),
        kv(
            tr("Time left"),
            d.eta_secs.map(fmt::eta).unwrap_or_default()
        ),
        kv(
            tr("Resume capability"),
            match d.resume {
                Some(true) => tr("Yes"),
                Some(false) => tr("No"),
                None => "?".into(),
            },
        ),
    ]
    .spacing(7)
    .into()
}

fn speed_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let p = app.prog.get(&d.id).cloned().unwrap_or_default();
    column![
        kv(
            tr("Transfer rate"),
            fmt::rate_capped(d.rate, app.effective_limit(d))
        ),
        checkbox(p.limit_on)
            .label(tr("Use Speed Limiter"))
            .on_toggle(move |b| Message::ProgLimitOn(d.id, b))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        text(tr("Maximum download speed:"))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light)),
        row![
            text_input("10", &p.limit_kb)
                .on_input(move |s| Message::ProgLimitKb(d.id, s))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(120.0),
            text(tr("KBytes/sec")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        checkbox(p.remember_limit)
            .label(tr(
                "Remember Speed Limiter settings for this file on download stop/resume"
            ))
            .on_toggle(move |b| Message::ProgLimitRemember(d.id, b))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        if app.cfg.settings.show_hide_buttons {
            crate::windows::dlg_btn(tr("Hide tab"), Some(Message::ProgHideTab(d.id, 1)))
        } else {
            iced::widget::space::horizontal().height(0.0).into()
        },
    ]
    .spacing(10)
    .into()
}

fn completion_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let mut col = column![
        row![
            text(tr("Save To:")).size(theme::FONT_SIZE).width(70.0),
            text(d.full_path().to_string_lossy().into_owned()).size(theme::FONT_SIZE),
        ]
        .spacing(8),
        checkbox(app.cfg.settings.show_complete_dialog)
            .label(tr("Show download complete dialog"))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
        text(tr(
            "These settings are unavailable when \"Show download complete dialog\" is turned on"
        ))
        .size(theme::FONT_SIZE)
        .color(theme::dim_text(&iced::Theme::Light)),
        checkbox(d.shutdown_after)
            .label(tr("Shut down / log off computer when done"))
            .on_toggle(move |b| Message::ProgShutdownAfter(d.id, b))
            .size(15.0)
            .text_size(theme::FONT_SIZE)
            .style(theme::check),
    ]
    .spacing(10);

    if d.shutdown_after {
        col = col.push(
            row![
                radio(
                    tr("Shutdown"),
                    PowerAction::Shutdown,
                    Some(d.shutdown_action),
                    move |v| Message::ProgShutdownAction(d.id, v),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
                radio(
                    tr("Log off"),
                    PowerAction::LogOff,
                    Some(d.shutdown_action),
                    move |v| Message::ProgShutdownAction(d.id, v),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
            ]
            .spacing(30),
        );
    }

    if app.cfg.settings.show_hide_buttons {
        col = col.push(crate::windows::dlg_btn(
            tr("Hide tab"),
            Some(Message::ProgHideTab(d.id, 2)),
        ));
    }

    col.into()
}

/// The indeterminate bar shown while the virus scanner works: iced's
/// `progress_bar` is determinate only, so the moving block is a FillPortion
/// row inside the same track the real bar draws.
fn marquee<'a>(sweep: f32) -> El<'a> {
    const TOTAL: u16 = 1000;
    const BLOCK: u16 = 220;
    let travel = f32::from(TOTAL - BLOCK);
    // Both spacers keep a portion: FillPortion(0) would divide the row by a
    // zero total.
    let left = (sweep.clamp(0.0, 1.0) * travel)
        .round()
        .clamp(1.0, travel - 1.0) as u16;
    let right = (TOTAL - BLOCK).saturating_sub(left).max(1);
    let bar = row![
        iced::widget::space::horizontal().width(Length::FillPortion(left)),
        container(iced::widget::space::horizontal())
            .width(Length::FillPortion(BLOCK))
            .height(Length::Fill)
            .style(theme::scan_block),
        iced::widget::space::horizontal().width(Length::FillPortion(right)),
    ]
    .height(Length::Fill);
    container(bar)
        .width(Length::Fill)
        .height(18.0)
        .style(theme::scan_track)
        .into()
}

/// The scanner's console output, in the box the per-connection table uses.
/// Anchored to the bottom so the newest line is the one on screen.
fn scan_log(st: &ScanState) -> El<'_> {
    const ROW_H: f32 = 20.0;
    // The connections box is a header plus 8 rows; the log has no header, so
    // it takes the ninth row instead and the panel keeps the same footprint.
    const VISIBLE: usize = 9;
    let mut rows = column![].width(Length::Fill);
    let n_rows = st.log.len().max(VISIBLE);
    for i in 0..n_rows {
        let line = st.log.get(i).cloned().unwrap_or_default();
        rows = rows.push(
            container(
                container(
                    text(line)
                        .size(theme::FONT_SIZE)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .padding([1, 6])
                .height(ROW_H),
            )
            .width(Length::Fill)
            .style(|t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(if theme::is_dark(t) {
                    iced::Color::from_rgb8(0x33, 0x2E, 0x38)
                } else {
                    iced::Color::from_rgb8(0xC9, 0xBF, 0xC9)
                })),
                text_color: Some(theme::text_color(t)),
                ..Default::default()
            }),
        );
    }
    container(
        scrollable(rows)
            .height(ROW_H * VISIBLE as f32)
            .width(Length::Fill)
            .anchor_bottom(),
    )
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

/// The blue chunk strip: 120 buckets over the object, filled where held.
fn chunk_strip<'a>(d: &DownloadItem) -> El<'a> {
    const BUCKETS: usize = 120;
    let mut filled = [false; BUCKETS];
    if let Some(size) = d.size.filter(|s| *s > 0) {
        for &(lo, hi) in &d.held {
            let a = (lo as f64 / size as f64 * BUCKETS as f64) as usize;
            let b = ((hi as f64 / size as f64 * BUCKETS as f64).ceil() as usize).min(BUCKETS);
            for f in filled.iter_mut().take(b).skip(a) {
                *f = true;
            }
        }
    }
    let mut r = row![].spacing(0).width(Length::Fill).height(10.0);
    for on in filled {
        r = r.push(
            container(iced::widget::space::horizontal())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(if on {
                    theme::chunk_on
                } else {
                    theme::chunk_off
                }),
        );
    }
    container(r)
        .width(Length::Fill)
        .padding(1)
        .style(theme::panel)
        .into()
}

fn conn_table<'a>(d: &'a DownloadItem) -> El<'a> {
    // Header stays put; only the rows scroll.
    let header = row![
        container(text(tr("N.")).size(theme::FONT_SIZE))
            .width(50.0)
            .padding([2, 6]),
        container(text(tr("Downloaded")).size(theme::FONT_SIZE))
            .width(150.0)
            .padding([2, 6]),
        container(text(tr("Info")).size(theme::FONT_SIZE))
            .width(Length::Fill)
            .padding([2, 6]),
    ]
    .spacing(0);
    // Exactly 8 rows are visible (blank band rows pad shorter transfers so
    // the box looks identical for 4 vs 8 connections); more connections
    // scroll inside the same viewport instead of growing the box.
    const ROW_H: f32 = 20.0;
    const VISIBLE: usize = 8;
    let mut rows = column![].width(Length::Fill);
    let n_rows = d.conns.len().max(VISIBLE);
    let blank = crate::model::ConnRow::default();
    for i in 0..n_rows {
        let c = d.conns.get(i).unwrap_or(&blank);
        rows = rows.push(
            container(
                row![
                    container(text(format!("{}", i + 1)).size(theme::FONT_SIZE))
                        .width(50.0)
                        .padding([1, 6]),
                    container(
                        text(if c.downloaded > 0 {
                            fmt::size3(c.downloaded)
                        } else {
                            String::new()
                        })
                        .size(theme::FONT_SIZE)
                    )
                    .width(150.0)
                    .padding([1, 6]),
                    container(text(c.info.clone()).size(theme::FONT_SIZE))
                        .width(Length::Fill)
                        .padding([1, 6]),
                ]
                .spacing(0)
                .height(ROW_H),
            )
            .style(|t: &iced::Theme| container::Style {
                background: Some(iced::Background::Color(if theme::is_dark(t) {
                    iced::Color::from_rgb8(0x33, 0x2E, 0x38)
                } else {
                    iced::Color::from_rgb8(0xC9, 0xBF, 0xC9)
                })),
                text_color: Some(theme::text_color(t)),
                ..Default::default()
            }),
        );
    }
    // The viewport is exactly VISIBLE rows tall — the dialog ends right after
    // the table instead of stretching an empty band below the connections.
    container(column![
        header,
        scrollable(rows)
            .height(ROW_H * VISIBLE as f32)
            .width(Length::Fill),
    ])
    .width(Length::Fill)
    .style(theme::panel)
    .into()
}

pub fn view(app: &App, id: crate::model::DlId) -> El<'_> {
    let Some(d) = app.item(id) else {
        return container(text(""))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::window)
            .into();
    };
    let p = app.prog.get(&id).cloned().unwrap_or_default();
    let s = &app.cfg.settings;

    let mut tabs = row![tab_btn(
        tr("Download status"),
        p.tab == ProgTab::Status,
        Message::ProgTabSet(id, ProgTab::Status),
    )]
    .spacing(1);
    if s.show_speed_tab {
        tabs = tabs.push(tab_btn(
            tr("Speed Limiter"),
            p.tab == ProgTab::Speed,
            Message::ProgTabSet(id, ProgTab::Speed),
        ));
    }
    if s.show_completion_tab {
        tabs = tabs.push(tab_btn(
            tr("Options on completion"),
            p.tab == ProgTab::Completion,
            Message::ProgTabSet(id, ProgTab::Completion),
        ));
    }

    let tab_body: El<'_> = match p.tab {
        ProgTab::Status => status_tab(app, d),
        ProgTab::Speed => speed_tab(app, d),
        ProgTab::Completion => completion_tab(app, d),
    };

    let active = d.state.is_active();
    let pause_label = if active { tr("Pause") } else { tr("Start") };
    let scan = app.scans.get(&id);
    let scanning = scan.map(ScanState::running).unwrap_or(false);
    // A verdict still on screen: the scanner found something (or could not
    // run) and the file is waiting on the user's decision.
    let verdict = scan.is_some() && !scanning;

    // The tab body is pinned to one height for every tab, so the progress
    // bar, buttons and connections panel below never jump when switching.
    let mut col = column![
        tabs,
        container(scrollable(tab_body))
            .width(Length::Fill)
            .height(210.0)
            .padding(12)
            .style(theme::panel),
        // The bytes are all in while a scan runs: the determinate bar has
        // nothing left to say, so it gives way to the marquee.
        if scanning {
            marquee(scan.map(ScanState::sweep).unwrap_or(0.0))
        } else {
            progress_bar(0.0..=1.0, d.disp_progress.clamp(0.0, 1.0))
                .girth(18.0)
                .style(theme::progress)
                .into()
        },
        row![
            dlg_btn(
                if p.details {
                    format!("<< {}", tr("Hide details"))
                } else {
                    format!("{} >>", tr("Show details"))
                },
                Some(Message::ProgToggleDetails(id)),
            ),
            iced::widget::space::horizontal(),
            // Scanning: Pause becomes Skip (the transfer is over, only the
            // scanner can still be stopped) and Cancel goes read-only —
            // there is nothing left to cancel, the file is on disk. Once a
            // verdict is in, the pair becomes the decision it asks for:
            // keep the file, or take it off the list and the disk. Keep is
            // the default button — deleting is the irreversible one.
            if scanning {
                dlg_btn_primary(tr("Skip"), Some(Message::ProgScanSkip(id)))
            } else if verdict {
                dlg_btn_primary(tr("Keep file"), Some(Message::ProgScanKeep(id)))
            } else {
                dlg_btn_primary(pause_label, Some(Message::ProgPauseResume(id)))
            },
            if verdict {
                dlg_btn(tr("Delete file"), Some(Message::ProgScanDelete(id)))
            } else {
                dlg_btn(tr("Cancel"), (!scanning).then_some(Message::ProgCancel(id)))
            },
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(10)
    .padding(12)
    .width(Length::Fill);

    if p.details {
        // Once the scanner has the file, the details panel is its console:
        // the chunk strip and connection rows describe a finished transfer.
        if let Some(st) = scan {
            col = col.push(crate::windows::centered(
                tr("Virus scanner output"),
                theme::FONT_SIZE,
            ));
            col = col.push(scan_log(st));
        } else {
            col = col.push(crate::windows::centered(
                tr("Start positions and download progress by connections"),
                theme::FONT_SIZE,
            ));
            col = col.push(chunk_strip(d));
            col = col.push(conn_table(d));
        }
    } else {
        col = col.push(iced::widget::space::vertical());
    }
    let col = col.height(Length::Fill);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

/// Window title: `4% meilisearch-linux-aarch64` while receiving.
pub fn title(app: &App, id: crate::model::DlId) -> String {
    match app.item(id) {
        Some(d) => {
            // A recording has no total, so there is no percentage to show.
            // The elapsed media is the number someone watching it wants.
            if d.stream.as_ref().is_some_and(|s| s.live) && d.state != DlState::Complete {
                let clock = d
                    .recorded_secs
                    .map(|s| crate::fmt::eta(s as u64))
                    .unwrap_or_else(|| "0:00".into());
                return format!("{clock} {}", d.file_name);
            }
            let pct = d
                .size
                .filter(|s| *s > 0 && d.state != DlState::Complete)
                .map(|s| format!("{:.0}% ", d.downloaded as f64 * 100.0 / s as f64))
                .unwrap_or_default();
            format!("{pct}{}", d.file_name)
        }
        None => "Hydra".into(),
    }
}

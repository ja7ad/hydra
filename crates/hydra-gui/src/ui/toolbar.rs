// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The toolbar: big gradient-outline icons, with labels underneath unless
//! View > Hide toolbar text is on — then the label becomes a hover tooltip.

use crate::app::{App, El, MenuAction, Message};
use crate::model::{DlState, ToolbarItem, TOOLBAR_MAX_VISIBLE};
use crate::{i18n::tr, icons, theme};
use iced::widget::{button, column, container, row, svg, text, tooltip};
use iced::{Alignment, Length};

/// Geometry of the row, shared by `view` and [`min_width`]: the icon
/// square, the padding either side of it, the gap between tools, a split
/// button's arrow, and the padding at each end of the row.
const ICON_W: f32 = 34.0;
const TOOL_PAD_X: f32 = 10.0;
const GAP: f32 = 4.0;
const ARROW_W: f32 = 14.0;
const ROW_PAD_X: f32 = 8.0;

/// What one button takes in this locale: its label, any dropdown arrow, and
/// the gap before that arrow.
fn item_width(item: ToolbarItem) -> f32 {
    let tool =
        crate::font::line_width(&tr(item.label()), theme::FONT_SIZE).max(ICON_W) + 2.0 * TOOL_PAD_X;
    if item.is_split() {
        tool + GAP + ARROW_W
    } else {
        tool
    }
}

/// The width the toolbar needs in this locale, labels and all.
///
/// The row does not wrap or scroll: past the right edge iced squeezes the
/// last tools to nothing and draws their labels over each other. The
/// window's floor comes from here rather than from a number measured once
/// against English, which every longer translation then overran. Which
/// buttons are shown is the user's choice, so the floor covers the widest
/// [`TOOLBAR_MAX_VISIBLE`] of them.
pub fn min_width() -> f32 {
    let mut widths: Vec<f32> = ToolbarItem::ALL.into_iter().map(item_width).collect();
    widths.sort_by(|a, b| b.total_cmp(a));
    widths.truncate(TOOLBAR_MAX_VISIBLE);
    let gaps = widths.len().saturating_sub(1) as f32 * GAP;
    widths.iter().sum::<f32>() + gaps + 2.0 * ROW_PAD_X
}

fn tool<'a>(icon: svg::Handle, label: String, msg: Option<Message>, labels: bool) -> El<'a> {
    let icon = svg(icon).width(ICON_W).height(ICON_W);
    let content: El<'a> = if labels {
        column![icon, text(label.clone()).size(theme::FONT_SIZE)]
            .spacing(3)
            .align_x(Alignment::Center)
            .into()
    } else {
        icon.into()
    };
    let mut b = button(content)
        .padding([6.0, TOOL_PAD_X])
        .style(theme::btn_toolbar);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    if labels {
        return b.into();
    }
    // Without the label the icon alone has to say what the button does, so
    // the text that was under it moves to the hover hint.
    tooltip(
        b,
        container(text(label).size(theme::FONT_SIZE))
            .padding(6)
            .style(theme::menu_panel),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The Speed Limiter button's text: the cap in force rather than a fixed
/// word — a limit left on by mistake is the failure mode worth making
/// visible. With the labels hidden that text is only a tooltip, where a bare
/// number names nothing, so the button's own name rides along with it.
fn speed_label(limit: Option<u64>, labels: bool) -> String {
    match limit {
        None => tr("Speed Limit"),
        Some(_) if labels => crate::fmt::limit(limit),
        Some(_) => format!(
            "{} \u{2014} {}",
            tr("Speed Limit"),
            crate::fmt::limit(limit)
        ),
    }
}

pub fn view(app: &App) -> El<'_> {
    let sel_some = !app.selected.is_empty();
    let in_sel = |d: &&crate::model::DownloadItem| app.selected.contains(&d.id);
    let sel_active = app
        .state
        .downloads
        .iter()
        .filter(in_sel)
        .any(|d| d.state.is_active());
    let sel_resumable = app
        .state
        .downloads
        .iter()
        .filter(in_sel)
        .any(|d| matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued));
    let any_active = app.state.downloads.iter().any(|d| d.state.is_active());
    let any_queue_running = app.cfg.queues.iter().any(|q| q.running);
    let limit = app.cfg.settings.global_limit();
    let labels = app.cfg.settings.show_toolbar_labels;
    let main_q = app
        .cfg
        .queues
        .first()
        .map(|q| q.name.clone())
        .unwrap_or_else(|| "Main download queue".into());

    let mut bar = row![]
        .spacing(GAP)
        .padding([4.0, ROW_PAD_X])
        .width(Length::Fill);
    for pref in app.cfg.settings.toolbar.iter().filter(|p| p.visible) {
        let item = pref.id;
        let name = tr(item.label());
        let (tool_el, arrow) = match item {
            ToolbarItem::AddUrl => (
                tool(
                    icons::add_url(true),
                    name,
                    Some(Message::Menu(MenuAction::AddNewDownload)),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Resume => (
                tool(
                    icons::resume(sel_resumable),
                    name,
                    sel_resumable.then_some(Message::ToolbarResume),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Stop => (
                tool(
                    icons::stop(sel_active),
                    name,
                    sel_active.then_some(Message::ToolbarStop),
                    labels,
                ),
                None,
            ),
            ToolbarItem::StopAll => (
                tool(
                    icons::stop_all(any_active),
                    name,
                    any_active.then_some(Message::Menu(MenuAction::StopAll)),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Delete => (
                tool(
                    icons::delete(sel_some),
                    name,
                    sel_some.then_some(Message::ToolbarDelete),
                    labels,
                ),
                None,
            ),
            ToolbarItem::DeleteCompleted => (
                tool(
                    icons::delete_completed(true),
                    name,
                    Some(Message::Menu(MenuAction::DeleteAllCompleted)),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Options => (
                tool(
                    icons::options(true),
                    name,
                    Some(Message::Menu(MenuAction::Options)),
                    labels,
                ),
                None,
            ),
            // Shortcuts into Options: the browser add-on is how most
            // downloads reach Hydra, and plugins are what teach it new sites.
            ToolbarItem::Extensions => (
                tool(
                    icons::extensions(true),
                    name,
                    Some(Message::Menu(MenuAction::Extensions)),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Plugins => (
                tool(
                    icons::plugins(true),
                    name,
                    Some(Message::Menu(MenuAction::Plugins)),
                    labels,
                ),
                None,
            ),
            ToolbarItem::Scheduler => (
                tool(
                    icons::scheduler(true),
                    name,
                    Some(Message::Menu(MenuAction::Scheduler)),
                    labels,
                ),
                None,
            ),
            // Split buttons: the big button acts on the MAIN queue,
            // the arrow opens the list of all queues.
            ToolbarItem::StartQueue => (
                tool(
                    icons::start_queue(!any_queue_running),
                    name,
                    (!any_queue_running)
                        .then_some(Message::Menu(MenuAction::StartQueue(main_q.clone()))),
                    labels,
                ),
                Some(Message::QueueMenuOpen(true)),
            ),
            ToolbarItem::StopQueue => (
                tool(
                    icons::stop_queue(any_queue_running),
                    name,
                    any_queue_running
                        .then_some(Message::Menu(MenuAction::StopQueue(main_q.clone()))),
                    labels,
                ),
                Some(Message::QueueMenuOpen(false)),
            ),
            // Speed Limiter, one click from the queue it is throttling: the
            // big button switches the cap on and off, the arrow picks a profile.
            ToolbarItem::SpeedLimit => (
                tool(
                    icons::speed_limit(limit.is_some()),
                    speed_label(limit, labels),
                    Some(Message::Menu(MenuAction::SpeedLimiterToggle)),
                    labels,
                ),
                Some(Message::SpeedMenuOpen),
            ),
        };
        bar = bar.push(tool_el);
        if let Some(open) = arrow {
            bar = bar.push(
                button(text("▾").size(theme::FONT_SIZE))
                    .padding([4, 3])
                    .style(theme::btn_toolbar)
                    .on_press(open),
            );
        }
    }
    bar.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_asks_for_at_least_what_its_widest_buttons_take() {
        let mut widths: Vec<f32> = ToolbarItem::ALL.into_iter().map(item_width).collect();
        widths.sort_by(|a, b| b.total_cmp(a));
        let widest: f32 = widths[..TOOLBAR_MAX_VISIBLE].iter().sum();
        assert!(min_width() > widest, "no room for the chrome around them");
        assert_eq!(crate::app::main_min_w(), min_width());
    }

    #[test]
    fn a_split_button_is_wider_than_its_label_by_the_arrow() {
        let plain = item_width(ToolbarItem::Stop);
        assert!(item_width(ToolbarItem::StopQueue) > plain + ARROW_W);
    }

    #[test]
    fn a_hidden_label_leaves_the_speed_cap_named_in_its_hover_hint() {
        let cap = Some(512 * 1024);
        let shown = speed_label(cap, true);
        let hint = speed_label(cap, false);
        assert_eq!(shown, "512 KB/s");
        assert_eq!(hint, "Speed Limit \u{2014} 512 KB/s");
        // No cap: the button is named the same either way.
        assert_eq!(speed_label(None, true), speed_label(None, false));
    }
}

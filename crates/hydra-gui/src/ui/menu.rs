// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The menu model (shared with the native macOS menu bar) and the in-window
//! menu bar + dropdowns drawn on Windows/Linux.

use crate::app::{App, El, MenuAction, MenuBarKind, Message, SortKey};
use crate::model::DlState;
use crate::{i18n::tr, theme};
use iced::widget::{button, column, container, mouse_area, row, space, text};
use iced::Length;

pub struct Entry {
    pub action: Option<MenuAction>,
    pub label: String,
    pub enabled: bool,
    /// Draw a separator above this entry.
    pub sep: bool,
    pub checked: bool,
    pub submenu: Vec<Entry>,
}

impl Entry {
    fn item(label: String, action: MenuAction) -> Self {
        Entry {
            action: Some(action),
            label,
            enabled: true,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    /// Public constructor for ad-hoc dropdowns (toolbar split buttons).
    pub fn plain(label: String, action: MenuAction, enabled: bool) -> Self {
        Entry {
            action: Some(action),
            label,
            enabled,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    fn disabled(label: String) -> Self {
        Entry {
            action: None,
            label,
            enabled: false,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    fn sub(label: String, submenu: Vec<Entry>) -> Self {
        Entry {
            action: None,
            label,
            enabled: true,
            sep: false,
            checked: false,
            submenu,
        }
    }

    fn sep(mut self) -> Self {
        self.sep = true;
        self
    }

    fn check(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

pub const BAR: [(MenuBarKind, &str); 5] = [
    (MenuBarKind::Tasks, "Tasks"),
    (MenuBarKind::File, "File"),
    (MenuBarKind::Downloads, "Downloads"),
    (MenuBarKind::View, "View"),
    (MenuBarKind::Help, "Help"),
];

/// Entries of one top-level menu, with enabled state derived from `app`.
pub fn entries(kind: MenuBarKind, app: &App) -> Vec<Entry> {
    let sel = app.selected_item();
    let sel_active = sel.map(|d| d.state.is_active()).unwrap_or(false);
    let sel_stopped = sel
        .map(|d| matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued))
        .unwrap_or(false);
    let any_active = app.state.downloads.iter().any(|d| d.state.is_active());
    match kind {
        MenuBarKind::Tasks => vec![
            Entry::item(tr("Add new download"), MenuAction::AddNewDownload),
            Entry::item(tr("Add batch download"), MenuAction::AddBatch),
            Entry::item(
                tr("Add batch download from clipboard"),
                MenuAction::AddBatchClipboard,
            ),
            Entry::item(
                tr("Add batch download from text file"),
                MenuAction::AddBatchFile,
            ),
            Entry::disabled(tr("Run site grabber")),
            Entry::disabled(tr("Show drop target")).sep(),
            Entry::item(tr("Export"), MenuAction::ExportList).sep(),
            Entry::item(tr("Import"), MenuAction::ImportList),
            Entry::item(tr("Exit"), MenuAction::Exit).sep(),
        ],
        MenuBarKind::File => vec![
            Entry {
                enabled: sel_active,
                ..Entry::item(tr("Stop Download"), MenuAction::StopDownload)
            },
            Entry {
                enabled: sel.is_some(),
                ..Entry::item(tr("Remove"), MenuAction::Remove)
            },
            Entry {
                enabled: sel_stopped,
                ..Entry::item(tr("Download Now"), MenuAction::DownloadNow)
            },
            Entry {
                enabled: sel.is_some(),
                ..Entry::item(tr("Redownload"), MenuAction::Redownload)
            },
        ],
        MenuBarKind::Downloads => {
            let queues: Vec<Entry> = app
                .cfg
                .queues
                .iter()
                .map(|q| Entry::item(tr(&q.name), MenuAction::StartQueue(q.name.clone())))
                .collect();
            let stops: Vec<Entry> = app
                .cfg
                .queues
                .iter()
                .map(|q| Entry::item(tr(&q.name), MenuAction::StopQueue(q.name.clone())))
                .collect();
            vec![
                Entry {
                    enabled: any_active,
                    ..Entry::item(tr("Pause All"), MenuAction::PauseAll)
                },
                Entry {
                    enabled: any_active,
                    ..Entry::item(tr("Stop All"), MenuAction::StopAll)
                },
                Entry::item(tr("Delete All Completed"), MenuAction::DeleteAllCompleted).sep(),
                Entry::disabled(tr("Find")).sep(),
                Entry::item(tr("Scheduler"), MenuAction::Scheduler).sep(),
                Entry::sub(tr("Start queue"), queues),
                Entry::sub(tr("Stop queue"), stops),
                Entry::item(tr("Speed Limiter"), MenuAction::SpeedLimiterToggle)
                    .check(app.cfg.settings.speed_limiter_on)
                    .sep(),
                Entry::item(tr("Options"), MenuAction::Options).sep(),
            ]
        }
        MenuBarKind::View => vec![
            Entry::item(tr("Hide categories"), MenuAction::HideCategories)
                .check(!app.cfg.settings.show_categories),
            Entry::sub(
                tr("Arrange files"),
                [
                    ("File Name", SortKey::Name),
                    ("Size", SortKey::Size),
                    ("Status", SortKey::Status),
                    ("Time left", SortKey::TimeLeft),
                    ("Transfer rate", SortKey::Rate),
                    ("Last Try Date", SortKey::LastTry),
                    ("Description", SortKey::Description),
                ]
                .into_iter()
                .map(|(l, k)| Entry::item(tr(l), MenuAction::ArrangeBy(k)))
                .collect(),
            ),
            Entry::sub(
                tr("Theme"),
                crate::theme::THEME_CHOICES
                    .into_iter()
                    .map(|(l, m)| {
                        Entry::item(tr(l), MenuAction::SetTheme(m))
                            .check(app.cfg.settings.theme() == m)
                    })
                    .collect(),
            )
            .sep(),
            Entry::sub(
                tr("Font"),
                crate::theme::FONT_CHOICES
                    .into_iter()
                    .map(|(l, s)| {
                        Entry::item(tr(l), MenuAction::FontSize(s))
                            .check(app.cfg.settings.font_size == s)
                    })
                    .collect(),
            ),
            Entry::sub(
                tr("Language"),
                crate::i18n::available()
                    .into_iter()
                    .map(|tag| {
                        let cur = app.cfg.language.clone().unwrap_or_else(|| "en".into());
                        let cur = if cur == "English" {
                            "en".to_string()
                        } else {
                            cur
                        };
                        Entry::item(
                            crate::i18n::display_name(&tag),
                            MenuAction::Language(tag.clone()),
                        )
                        .check(cur == tag)
                    })
                    .collect(),
            ),
        ],
        MenuBarKind::Help => vec![
            Entry::item(tr("Hydra Home Page"), MenuAction::HomePage),
            Entry::item(tr("Contribute on GitHub"), MenuAction::Contribute),
            Entry::item(tr("Keyboard Shortcuts"), MenuAction::Shortcuts),
            Entry::item(tr("Permissions"), MenuAction::Permissions),
            Entry::item(tr("Logs"), MenuAction::Logs),
            Entry::item(tr("Report an Issue"), MenuAction::ReportIssue),
            Entry::item(tr("Check for updates"), MenuAction::CheckUpdates).sep(),
            Entry::item(tr("About Hydra"), MenuAction::About).sep(),
        ],
    }
}

/// Context-menu entries for the selected download row.
pub fn context_entries(app: &App) -> Vec<Entry> {
    let Some(d) = app.selected_item() else {
        return vec![];
    };
    let mut v = vec![];
    if d.state == DlState::Complete {
        v.push(Entry::item(tr("Open"), MenuAction::OpenSel));
    }
    v.push(Entry::item(tr("Open folder"), MenuAction::OpenFolderSel));
    v.push(Entry {
        enabled: matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued),
        ..Entry::item(tr("Resume Download"), MenuAction::DownloadNow).sep()
    });
    v.push(Entry {
        enabled: d.state.is_active(),
        ..Entry::item(tr("Stop Download"), MenuAction::StopDownload)
    });
    v.push(Entry::item(tr("Redownload"), MenuAction::Redownload));
    v.push(Entry::item(tr("Delete"), MenuAction::Remove).sep());
    let queues: Vec<Entry> = app
        .cfg
        .queues
        .iter()
        .map(|q| Entry::item(tr(&q.name), MenuAction::MoveToQueue(q.name.clone())))
        .collect();
    v.push(Entry::sub(tr("Move to queue"), queues).sep());
    v.push(Entry {
        enabled: d.queue.is_some(),
        ..Entry::item(tr("Remove from queue"), MenuAction::RemoveFromQueue)
    });
    v.push(Entry::item(tr("Properties"), MenuAction::Properties).sep());
    v
}

/// The in-window menu bar (Windows/Linux; macOS uses the native bar).
pub fn bar(app: &App) -> El<'_> {
    let mut r = row![].spacing(2).padding([2, 6]);
    for (kind, label) in BAR {
        // The title of the open menu stays highlighted while its dropdown is
        // up, so hovering across the bar clearly shows which menu is showing.
        let open = app.open_menu == Some(kind);
        r = r.push(
            button(text(tr(label)).size(theme::FONT_SIZE + 1.0))
                .padding([3, 10])
                .style(move |t: &iced::Theme, s: button::Status| {
                    theme::btn_menu(t, if open { button::Status::Hovered } else { s })
                })
                .on_press(Message::MenuOpen(kind)),
        );
    }
    r.into()
}

/// Invisible clone of a menu-bar button: identical text/padding, so it
/// occupies exactly the same space as the real one without being seen.
fn ghost<'a>(label: &str) -> iced::widget::Button<'a, Message> {
    button(
        text(tr(label))
            .size(theme::FONT_SIZE + 1.0)
            .color(iced::Color::TRANSPARENT),
    )
    .padding([3, 10])
    .style(|_t: &iced::Theme, _s: button::Status| button::Style::default())
}

/// Full-window overlay for a menu-bar dropdown. Instead of estimating pixel
/// offsets, the bar's layout is replayed with invisible button clones and the
/// panel slots in right after them — so it lands exactly under its button for
/// any font, size, or locale.
pub fn bar_overlay(app: &App, kind: MenuBarKind) -> El<'_> {
    let items = entries(kind, app);
    let panel = dropdown(&items, app.open_submenu);
    // Same height as the real bar row (+1 for the rule below it). Every title
    // is replayed here, not just the first: the overlay covers the real bar,
    // so these ghosts are what the pointer actually reaches while a menu is
    // open. Hovering one switches the dropdown to it (menu tracking, like IDM
    // and every native Windows/Linux menu bar); pressing the open title again
    // closes the bar.
    let mut bar_ghost = row![].spacing(2).padding([2, 6]);
    for (k, label) in BAR {
        bar_ghost = bar_ghost.push(
            mouse_area(ghost(label))
                .on_enter(Message::MenuHover(k))
                .on_press(Message::MenuOpen(k)),
        );
    }
    // The buttons left of the open one push the panel to its x position.
    let mut below = row![].spacing(2).padding(iced::Padding {
        left: 6.0,
        ..iced::Padding::ZERO
    });
    for (k, label) in BAR {
        if k == kind {
            break;
        }
        below = below.push(ghost(label));
    }
    below = below.push(panel);
    let content = column![bar_ghost, space::vertical().height(1.0), below];
    iced::widget::stack![
        mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill))
            .on_press(Message::MenuClose)
            .on_right_press(Message::MenuClose),
        content,
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The separator wrapper above an entry: only the inner 1px line is painted;
/// the padded wrapper stays transparent (styling the wrapper fills its
/// padding too and renders a thick gray band instead of a hairline). The
/// `visible: false` variant is a ghost spacer for flyout alignment.
fn sep_row<'a>(visible: bool) -> El<'a> {
    let line = container(space::horizontal().height(1.0)).width(Length::Fill);
    let line = if visible {
        line.style(|t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
    } else {
        line
    };
    container(line).width(Length::Fill).padding([3, 6]).into()
}

/// One dropdown entry row (label + optional check mark / submenu arrow).
fn entry_row<'a>(e: &Entry, i: usize, open_submenu: Option<usize>) -> El<'a> {
    let check = if e.checked { "✓  " } else { "    " };
    let has_sub = !e.submenu.is_empty();
    let is_open = has_sub && open_submenu == Some(i);
    let label_row = row![
        text(format!("{check}{}", e.label)).size(theme::FONT_SIZE),
        space::horizontal(),
        text(if has_sub { "›" } else { "" }).size(theme::FONT_SIZE),
    ]
    .width(Length::Fill);
    let msg = if has_sub {
        Some(Message::SubmenuHover(Some(i)))
    } else {
        e.action.clone().filter(|_| e.enabled).map(Message::Menu)
    };
    let enabled = e.enabled;
    let mut b = button(label_row)
        .padding([5, 14])
        .width(Length::Fill)
        .style(move |t: &iced::Theme, s: button::Status| {
            if !enabled {
                return button::Style {
                    background: None,
                    text_color: theme::dim_text(t),
                    ..button::Style::default()
                };
            }
            // The parent of an open flyout stays highlighted like macOS.
            theme::btn_menu(t, if is_open { button::Status::Hovered } else { s })
        });
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    // Hovering opens an entry's flyout and closes any sibling's — the macOS
    // behaviour; entries without children close the open flyout instead.
    mouse_area(b)
        .on_enter(Message::SubmenuHover(has_sub.then_some(i)))
        .into()
}

/// Invisible clone of an entry row: identical text/padding so it occupies the
/// exact height of the real row, used to align the flyout with its parent.
fn ghost_entry<'a>(e: &Entry) -> El<'a> {
    button(
        text(format!("    {}", e.label))
            .size(theme::FONT_SIZE)
            .color(iced::Color::TRANSPARENT),
    )
    .padding([5, 14])
    .width(Length::Fill)
    .style(|_t: &iced::Theme, _s: button::Status| button::Style::default())
    .into()
}

/// A dropdown panel for `entries`, used by both the menu bar and the context
/// menu. An entry with children opens a macOS-style flyout box to the right
/// of the panel, top-aligned with its parent row (`open_submenu`).
pub fn dropdown<'a>(items: &[Entry], open_submenu: Option<usize>) -> El<'a> {
    let mut col = column![].spacing(0).width(Length::Shrink);
    for (i, e) in items.iter().enumerate() {
        if e.sep {
            col = col.push(sep_row(true));
        }
        col = col.push(entry_row(e, i, open_submenu));
    }
    let panel = container(col)
        .padding(4)
        .width(260.0)
        .style(theme::menu_panel);

    let open = open_submenu
        .and_then(|i| items.get(i).map(|e| (i, e)))
        .filter(|(_, e)| !e.submenu.is_empty());
    let Some((idx, parent)) = open else {
        return panel.into();
    };

    // The flyout column replays the panel's top padding and every row above
    // the parent as invisible ghosts, so the flyout's top edge lands exactly
    // on the parent row for any font, size, or locale.
    let mut spacer = column![space::vertical().height(4.0)].spacing(0);
    for e in items.iter().take(idx) {
        if e.sep {
            spacer = spacer.push(sep_row(false));
        }
        spacer = spacer.push(ghost_entry(e));
    }
    if parent.sep {
        spacer = spacer.push(sep_row(false));
    }

    let mut sub = column![].spacing(0).width(Length::Shrink);
    for c in &parent.submenu {
        let check = if c.checked { "✓  " } else { "    " };
        let mut cb = button(text(format!("{check}{}", c.label)).size(theme::FONT_SIZE))
            .padding([5, 14])
            .width(Length::Fill)
            .style(theme::btn_menu);
        if let Some(a) = c.action.clone().filter(|_| c.enabled) {
            cb = cb.on_press(Message::Menu(a));
        }
        sub = sub.push(cb);
    }
    let flyout = container(sub)
        .padding(4)
        .width(230.0)
        .style(theme::menu_panel);

    row![panel, column![spacer, flyout]].into()
}

/// Full-window overlay: click-away layer + positioned dropdown.
pub fn overlay<'a>(app: &'a App, items: Vec<Entry>, at: iced::Point) -> El<'a> {
    let panel = dropdown(&items, app.open_submenu);
    iced::widget::stack![
        mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill))
            .on_press(Message::MenuClose)
            .on_right_press(Message::MenuClose),
        iced::widget::pin(panel).x(at.x).y(at.y),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

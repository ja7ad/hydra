// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application state and update logic — every window shares this one `App`.

use crate::engine::{self, Cmd, StartSpec};
use crate::model::{
    self, categorize, ConfigFile, DlId, DlQuota, DlState, DownloadItem, PowerAction, ProxyMode,
    SiteLogin, StateFile, ThemeMode,
};
use crate::sounds;
use crate::{fmt, i18n};
use iced::window;
use iced::{Point, Task};
use std::collections::HashMap;
use std::time::Instant;

pub type El<'a> = iced::Element<'a, Message>;

// ------------------------------------------------------------------- windows

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WinKind {
    Main,
    AddUrl,
    FileInfo(DlId),
    Progress(DlId),
    Complete(DlId),
    Options,
    Scheduler,
    Batch,
    About,
    Confirm,
    Permissions,
    Shortcuts,
    Update,
}

// ---------------------------------------------------------------- selections

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TreeSel {
    All,
    Cat(String),
    Unfinished,
    UnfCat(String),
    Finished,
    FinCat(String),
    Queues,
    Queue(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Queue,
    Size,
    Status,
    TimeLeft,
    Rate,
    LastTry,
    Description,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuBarKind {
    Tasks,
    File,
    Downloads,
    View,
    Help,
}

/// Every menu-driven command, shared by the in-window menu bar and the native
/// macOS menu (which addresses them by the string id in [`MenuAction::id`]).
#[derive(Clone, PartialEq, Debug)]
pub enum MenuAction {
    AddNewDownload,
    AddBatch,
    AddBatchClipboard,
    AddBatchFile,
    SiteGrabber,
    DropTarget,
    ExportList,
    ImportList,
    Exit,
    StopDownload,
    Remove,
    DownloadNow,
    Redownload,
    PauseAll,
    StopAll,
    DeleteAllCompleted,
    Find,
    Scheduler,
    StartQueue(String),
    StopQueue(String),
    SpeedLimiterToggle,
    Options,
    /// Options, opened straight on the Extensions page (toolbar shortcut).
    Extensions,
    HideCategories,
    ArrangeBy(SortKey),
    SetTheme(ThemeMode),
    FontSize(u16),
    Language(String),
    HomePage,
    Contribute,
    ReportIssue,
    About,
    Permissions,
    MoveToQueue(String),
    RemoveFromQueue,
    Properties,
    OpenSel,
    OpenFolderSel,
    PowerSaveToggle,
    Shortcuts,
    /// Open the session log in the system's default viewer.
    Logs,
    /// Help > Check for updates: a manual, user-visible version check —
    /// unlike the silent startup check it also reports "up to date" and
    /// connection failures.
    CheckUpdates,
}

impl MenuAction {
    // Only the native menu/tray integrations need string ids: the macOS menu
    // bar, and the tray on every platform that has one.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    pub fn id(&self) -> String {
        match self {
            MenuAction::AddNewDownload => "add_new".into(),
            MenuAction::AddBatch => "add_batch".into(),
            MenuAction::AddBatchClipboard => "add_batch_clip".into(),
            MenuAction::AddBatchFile => "add_batch_file".into(),
            MenuAction::SiteGrabber => "grabber".into(),
            MenuAction::DropTarget => "drop_target".into(),
            MenuAction::ExportList => "export".into(),
            MenuAction::ImportList => "import".into(),
            MenuAction::Exit => "exit".into(),
            MenuAction::StopDownload => "stop_dl".into(),
            MenuAction::Remove => "remove".into(),
            MenuAction::DownloadNow => "dl_now".into(),
            MenuAction::Redownload => "redownload".into(),
            MenuAction::PauseAll => "pause_all".into(),
            MenuAction::StopAll => "stop_all".into(),
            MenuAction::DeleteAllCompleted => "del_completed".into(),
            MenuAction::Find => "find".into(),
            MenuAction::Scheduler => "scheduler".into(),
            MenuAction::StartQueue(q) => format!("start_queue:{q}"),
            MenuAction::StopQueue(q) => format!("stop_queue:{q}"),
            MenuAction::SpeedLimiterToggle => "speed_limiter".into(),
            MenuAction::Options => "options".into(),
            MenuAction::Extensions => "extensions".into(),
            MenuAction::HideCategories => "hide_cats".into(),
            MenuAction::ArrangeBy(k) => format!("arrange:{k:?}"),
            MenuAction::SetTheme(m) => format!("theme:{m:?}"),
            MenuAction::FontSize(s) => format!("font:{s}"),
            MenuAction::Language(l) => format!("lang:{l}"),
            MenuAction::HomePage => "homepage".into(),
            MenuAction::Contribute => "contribute".into(),
            MenuAction::ReportIssue => "report_issue".into(),
            MenuAction::About => "about".into(),
            MenuAction::Permissions => "permissions".into(),
            MenuAction::MoveToQueue(q) => format!("move_q:{q}"),
            MenuAction::RemoveFromQueue => "rm_q".into(),
            MenuAction::Properties => "props".into(),
            MenuAction::OpenSel => "open_sel".into(),
            MenuAction::OpenFolderSel => "open_folder_sel".into(),
            MenuAction::PowerSaveToggle => "power_save".into(),
            MenuAction::Shortcuts => "shortcuts".into(),
            MenuAction::Logs => "logs".into(),
            MenuAction::CheckUpdates => "check_updates".into(),
        }
    }

    pub fn from_id(id: &str) -> Option<MenuAction> {
        if let Some(q) = id.strip_prefix("start_queue:") {
            return Some(MenuAction::StartQueue(q.into()));
        }
        if let Some(q) = id.strip_prefix("stop_queue:") {
            return Some(MenuAction::StopQueue(q.into()));
        }
        if let Some(k) = id.strip_prefix("arrange:") {
            let key = match k {
                "Name" => SortKey::Name,
                "Size" => SortKey::Size,
                "Status" => SortKey::Status,
                "TimeLeft" => SortKey::TimeLeft,
                "Rate" => SortKey::Rate,
                "LastTry" => SortKey::LastTry,
                "Description" => SortKey::Description,
                _ => SortKey::Name,
            };
            return Some(MenuAction::ArrangeBy(key));
        }
        if let Some(m) = id.strip_prefix("theme:") {
            let mode = match m {
                "Light" => ThemeMode::Light,
                "Dark" => ThemeMode::Dark,
                _ => ThemeMode::System,
            };
            return Some(MenuAction::SetTheme(mode));
        }
        if let Some(s) = id.strip_prefix("font:") {
            return s.parse().ok().map(MenuAction::FontSize);
        }
        if let Some(l) = id.strip_prefix("lang:") {
            return Some(MenuAction::Language(l.into()));
        }
        if let Some(q) = id.strip_prefix("move_q:") {
            return Some(MenuAction::MoveToQueue(q.into()));
        }
        Some(match id {
            "add_new" => MenuAction::AddNewDownload,
            "add_batch" => MenuAction::AddBatch,
            "add_batch_clip" => MenuAction::AddBatchClipboard,
            "add_batch_file" => MenuAction::AddBatchFile,
            "grabber" => MenuAction::SiteGrabber,
            "drop_target" => MenuAction::DropTarget,
            "export" => MenuAction::ExportList,
            "import" => MenuAction::ImportList,
            "exit" => MenuAction::Exit,
            "stop_dl" => MenuAction::StopDownload,
            "remove" => MenuAction::Remove,
            "dl_now" => MenuAction::DownloadNow,
            "redownload" => MenuAction::Redownload,
            "pause_all" => MenuAction::PauseAll,
            "stop_all" => MenuAction::StopAll,
            "del_completed" => MenuAction::DeleteAllCompleted,
            "find" => MenuAction::Find,
            "scheduler" => MenuAction::Scheduler,
            "speed_limiter" => MenuAction::SpeedLimiterToggle,
            "options" => MenuAction::Options,
            "extensions" => MenuAction::Extensions,
            "hide_cats" => MenuAction::HideCategories,
            "homepage" => MenuAction::HomePage,
            "contribute" => MenuAction::Contribute,
            "report_issue" => MenuAction::ReportIssue,
            "about" => MenuAction::About,
            "permissions" => MenuAction::Permissions,
            "rm_q" => MenuAction::RemoveFromQueue,
            "props" => MenuAction::Properties,
            "open_sel" => MenuAction::OpenSel,
            "open_folder_sel" => MenuAction::OpenFolderSel,
            "power_save" => MenuAction::PowerSaveToggle,
            "shortcuts" => MenuAction::Shortcuts,
            "logs" => MenuAction::Logs,
            "check_updates" => MenuAction::CheckUpdates,
            _ => return None,
        })
    }
}

// ------------------------------------------------------------- dialog states

#[derive(Clone, Debug, Default)]
pub struct AddUrlState {
    pub address: String,
    pub use_auth: bool,
    pub login: String,
    pub password: String,
    pub error: Option<String>,
    /// Cookie header captured by the browser extension; applied to the item
    /// right after `add_item` so a background start already sends it.
    pub capture_cookies: Option<String>,
    /// Filename the browser had already resolved (Content-Disposition et
    /// al.) — better than what the URL path implies.
    pub capture_name: Option<String>,
    /// What the address turned out to be, when it is a manifest. A stream
    /// has to be asked which rendition BEFORE it starts — there is no
    /// changing your mind halfway through a hundred segments.
    pub stream: Option<crate::engine::StreamProbe>,
    /// The address `stream` describes, so an edited address invalidates it.
    pub stream_of: String,
    pub stream_probing: bool,
    pub stream_error: Option<String>,
    pub quality: Option<crate::engine::StreamQuality>,
    /// "MP4" or "TS".
    pub container: String,
    /// Minutes to record a live stream for, as typed. Empty means "until I
    /// press Stop".
    pub record_minutes: String,
}

/// A capture parked behind the duplicate-confirmation dialog.
#[derive(Clone, Debug)]
pub struct PendingAdd {
    pub url: String,
    pub auth: Option<(String, String)>,
    pub cookies: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileInfoState {
    pub dl: DlId,
    pub category: String,
    pub save_dir: String,
    pub file_name: String,
    pub description: String,
    pub remember: bool,
    /// Editable in Properties mode: changing it and starting continues the
    /// same bytes from a different mirror.
    pub url: String,
    pub login: String,
    pub password: String,
    pub cookies: String,
    /// New-download dialog (auto-started in the background, Cancel removes
    /// the item) vs Properties on an existing entry.
    pub is_new: bool,
    /// The file type is not in Options > File types, so nothing may start
    /// on its own: the background checkbox reads off until the user ticks it.
    pub bg_blocked: bool,
    /// Fields the user edited; the background probe stops auto-filling them.
    pub name_touched: bool,
    pub cat_touched: bool,
    pub dir_touched: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProgTab {
    #[default]
    Status,
    Speed,
    Completion,
}

#[derive(Clone, Debug, Default)]
pub struct ProgState {
    pub tab: ProgTab,
    pub details: bool,
    pub limit_on: bool,
    pub limit_kb: String,
    pub remember_limit: bool,
}

/// A virus scan over one finished file: what the scanner has printed so far,
/// and the verdict once it exits. Lives beside the download rather than in
/// it — a scan belongs to this session, not to the persisted item.
#[derive(Debug, Default)]
pub struct ScanState {
    /// Console output, oldest first (see `scan::MAX_LINES` for the cap).
    pub log: Vec<String>,
    /// The last line came in as a terminal redraw, so the next redraw
    /// overwrites it instead of piling up behind it.
    redraw_tail: bool,
    /// `None` while the scanner is still running.
    pub outcome: Option<crate::scan::Outcome>,
    /// Marquee position of the indeterminate bar, ping-ponging over 0..2;
    /// advanced by `AnimTick`.
    pub phase: f32,
}

impl ScanState {
    /// Append one console line, or overwrite the previous one when both it
    /// and this one were drawn over the same terminal row.
    pub fn push(&mut self, text: String, redraw: bool) {
        match self.log.last_mut() {
            Some(last) if redraw && self.redraw_tail => *last = text,
            _ => self.log.push(text),
        }
        self.redraw_tail = redraw;
    }

    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    /// 0..1 sweep of the marquee block, folded from the 0..2 phase so the
    /// block travels back the way it came instead of jumping to the start.
    pub fn sweep(&self) -> f32 {
        if self.phase <= 1.0 {
            self.phase
        } else {
            2.0 - self.phase
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptTab {
    General,
    FileTypes,
    SaveTo,
    Downloads,
    Connection,
    Proxy,
    Sites,
    Extensions,
    Sounds,
}

#[derive(Debug)]
pub struct OptionsState {
    pub tab: OptTab,
    pub draft: crate::model::Settings,
    /// Multiline editors for the File-types lists.
    pub auto_types_edit: iced::widget::text_editor::Content,
    pub sites_edit: iced::widget::text_editor::Content,
    pub draft_cats: Vec<crate::model::CategoryDef>,
    pub sel_category: String,
    pub sel_login: Option<usize>,
    pub login_site: String,
    pub login_user: String,
    pub login_pass: String,
    pub conn_exc_server: String,
    pub conn_exc_n: String,
    /// Text buffers for the Download-limit numbers. The draft holds `u64`,
    /// and binding an input straight to `n.to_string()` makes the field
    /// un-clearable: an empty string fails to parse, the commit is skipped
    /// and the old number is re-rendered on the next frame, so the user can
    /// never backspace past the first digit. The buffer holds what was
    /// typed; the draft takes it whenever it parses.
    pub dl_limit_mb_txt: String,
    pub dl_limit_hours_txt: String,
}

impl Default for OptionsState {
    fn default() -> Self {
        OptionsState {
            tab: OptTab::General,
            draft: crate::model::Settings::default(),
            auto_types_edit: iced::widget::text_editor::Content::new(),
            sites_edit: iced::widget::text_editor::Content::new(),
            draft_cats: crate::model::default_categories(),
            sel_category: "General".into(),
            sel_login: None,
            login_site: String::new(),
            login_user: String::new(),
            login_pass: String::new(),
            conn_exc_server: String::new(),
            conn_exc_n: String::new(),
            dl_limit_mb_txt: String::new(),
            dl_limit_hours_txt: String::new(),
        }
    }
}

/// State of the update dialog (`WinKind::Update`).
#[derive(Debug, Default)]
pub struct UpdateUiState {
    /// The newer release, filled by the startup check.
    pub info: Option<crate::update::UpdateInfo>,
    pub phase: UpdatePhase,
    /// Cooperative cancel for the in-flight download; the stream checks it
    /// on every chunk.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The pending check came from Help > Check for updates: report
    /// "up to date" and failures too, where the startup check stays silent.
    pub manual: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdatePhase {
    /// Notes are on screen; nothing has been downloaded.
    #[default]
    Idle,
    Downloading {
        got: u64,
        total: Option<u64>,
    },
    Verifying,
    Preparing,
    /// The finisher process is live; the app is about to exit.
    Restarting,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SchTab {
    #[default]
    Schedule,
    Files,
}

#[derive(Clone, Debug)]
pub struct SchState {
    pub queue: String,
    pub tab: SchTab,
    pub sel_file: Option<DlId>,
    /// Item being dragged to a new position in "Files in the queue".
    pub drag: Option<DlId>,
    /// Draft of the selected queue's name; committed on Enter.
    pub rename_draft: String,
    /// Double-click on the title switches it into the rename field.
    pub renaming: bool,
}

impl Default for SchState {
    fn default() -> Self {
        SchState {
            queue: String::new(),
            tab: SchTab::Schedule,
            sel_file: None,
            drag: None,
            rename_draft: String::new(),
            renaming: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct BatchState {
    pub text: iced::widget::text_editor::Content,
    pub checks: Vec<(String, bool)>,
    /// Probed sizes for the Size column (absent = probe still running).
    pub sizes: std::collections::HashMap<String, u64>,
    /// Resolved names for the File Name column, by URL.
    ///
    /// Kept separately from the URL-implied name because they disagree exactly
    /// where it matters: a redirector link (`href.li/?<url>`) implies
    /// `index.html`, and only the probe knows it forwards to `setup.exe`.
    pub names: std::collections::HashMap<String, String>,
    pub parsed: bool,
    pub to_category: bool,
    pub category: String,
    pub to_dir: bool,
    pub dir: String,
    /// Which rendition to take for any HLS/DASH manifest in the list.
    ///
    /// One choice for the whole batch rather than a picker per URL: probing
    /// fifty manifests to build fifty dropdowns would be slow and nobody
    /// wants to answer the same question fifty times. The engine resolves
    /// this against each manifest's own ladder when it starts.
    pub stream_quality: BatchQuality,
    /// "MP4" or "TS".
    pub stream_container: String,
}

/// The rendition a batch asks for, as a preference rather than an exact
/// rendition: each manifest has its own ladder and may not offer this height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchQuality {
    #[default]
    Best,
    P1080,
    P720,
    P480,
    P360,
}

impl BatchQuality {
    pub const ALL: [BatchQuality; 5] = [
        BatchQuality::Best,
        BatchQuality::P1080,
        BatchQuality::P720,
        BatchQuality::P480,
        BatchQuality::P360,
    ];

    pub fn height(self) -> Option<u32> {
        match self {
            BatchQuality::Best => None,
            BatchQuality::P1080 => Some(1080),
            BatchQuality::P720 => Some(720),
            BatchQuality::P480 => Some(480),
            BatchQuality::P360 => Some(360),
        }
    }
}

impl std::fmt::Display for BatchQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            BatchQuality::Best => crate::i18n::tr("Best quality"),
            BatchQuality::P1080 => "1080p".into(),
            BatchQuality::P720 => "720p".into(),
            BatchQuality::P480 => "480p".into(),
            BatchQuality::P360 => "360p".into(),
        })
    }
}

#[derive(Clone, Debug)]
pub enum ConfirmKind {
    DeleteItems(Vec<DlId>),
    DeleteCompleted,
    NoneChecked,
    /// The URL is already in the list (`existing`) and/or its target file is
    /// already on disk (`file`): resume / open / download as a renamed copy.
    Duplicate {
        existing: Option<DlId>,
        file: Option<String>,
    },
    /// Startup check could not write into the download folder.
    PermissionWarn {
        dir: String,
    },
    /// Help > Check for updates found nothing newer (info box, OK).
    UpToDate,
    /// Help > Check for updates could not reach the release server.
    UpdateCheckFailed(String),
    /// "Warn me before stopping downloads" (Connection tab): the stop only
    /// happens once the user confirms. `stop_queues` carries the Pause All /
    /// Stop All variant, which also halts queue processing.
    StopWarn {
        ids: Vec<DlId>,
        stop_queues: bool,
    },
}

// ------------------------------------------------------------------ messages

#[derive(Clone, Debug)]
pub enum Message {
    Noop,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    WindowCloseRequested(window::Id),
    WinMoved(window::Id, Point),
    WinResized(window::Id, iced::Size),
    Engine(engine::Event),
    Tick,
    ClipboardSeen(Option<String>),
    /// Fast animation tick (only subscribed while a download is active):
    /// glides each item's displayed progress toward the real fraction.
    AnimTick,
    NativeMenu(String),
    /// The OS switched between light and dark appearance (or the startup
    /// query answered). Only View > Theme > System Default acts on it.
    SystemTheme(iced::theme::Mode),
    // main window chrome
    MenuOpen(MenuBarKind),
    /// Moving the pointer over a menu-bar title while another menu is already
    /// open: switches to that menu without closing the bar (IDM/Windows menu
    /// tracking). Unlike [`Message::MenuOpen`] it never toggles a menu shut.
    MenuHover(MenuBarKind),
    MenuClose,
    /// Hovering (or clicking) a dropdown entry: `Some(i)` opens entry `i`'s
    /// flyout, `None` closes it — macOS submenu behaviour.
    SubmenuHover(Option<usize>),
    Menu(MenuAction),
    TreeSelect(TreeSel),
    TreeToggle(u8),
    /// Double-click on a user-created queue's sidebar row: switch it into
    /// an inline rename field. Stock queues (Main/Synchronization) ignore
    /// this.
    TreeQueueRenameStart(String),
    TreeQueueRenameDraft(String),
    TreeQueueRenameCommit,
    RowClick(DlId),
    RowRightClick(DlId),
    /// Sample the pointer while a rubber-band or a column resize is in
    /// flight. Motion itself publishes nothing (see [`crate::ui::probe`]);
    /// this ticks at a fixed rate instead, because every message repaints the
    /// window and a 120 Hz pointer would repaint it 120 times a second.
    DragTick,
    /// The download table scrolled: (vertical offset, viewport height).
    /// Drives the virtual row window in `ui::table`.
    TableScrolled(f32, f32),
    Mods(iced::keyboard::Modifiers),
    RawKey(iced::keyboard::Key, iced::keyboard::Modifiers),
    SelectAll,
    RowEnter(DlId),
    RowExit(DlId),
    /// Left press on the empty ruled area below the last download.
    EmptyPress,
    /// A drag swept onto the empty ruled area below the last download.
    EmptyEnter,
    HeaderEnter(usize),
    HeaderExit(usize),
    QueueMenuOpen(bool),
    ClipboardAddStart(Option<String>),
    ShortcutEdit(String, String),
    MouseUp,
    ColResizeStart(usize),
    SortBy(SortKey),
    ToolbarResume,
    ToolbarStop,
    ToolbarDelete,
    // add url
    AddrChanged(String),
    AddrAuthToggled(bool),
    AddrLogin(String),
    AddrPass(String),
    AddUrlOk,
    /// Requests arriving from the browser extension over the extbus socket.
    Ext(crate::extbus::ExtEvent),
    // file info
    FiCategory(String),
    FiFileName(String),
    FiSaveDir(String),
    FiBgToggle(bool),
    FiBrowseDir,
    FiDirPicked(Option<String>),
    FiDescription(String),
    FiRemember(bool),
    FiUrl(String),
    FiLogin(String),
    FiPass(String),
    FiCookies(String),
    FiDownloadLater,
    FiStartDownload,
    FiCancel,
    // progress dialog
    ProgTabSet(DlId, ProgTab),
    ProgToggleDetails(DlId),
    ProgPauseResume(DlId),
    ProgCancel(DlId),
    ProgHideTab(DlId, u8),
    ProgLimitOn(DlId, bool),
    ProgLimitKb(DlId, String),
    ProgLimitRemember(DlId, bool),
    ProgShutdownAfter(DlId, bool),
    ProgShutdownAction(DlId, PowerAction),
    /// Skip the virus scan running over the finished file.
    ProgScanSkip(DlId),
    /// Infected (or unscannable): keep the file, close the dialog.
    ProgScanKeep(DlId),
    /// Infected (or unscannable): take the file off the list and the disk.
    ProgScanDelete(DlId),
    /// Console line / verdict from the virus scanner.
    Scan(crate::scan::ScanEvent),
    // complete dialog
    OpenFile(DlId),
    OpenFolder(DlId),
    // options
    OptTabSet(OptTab),
    /// Extensions page: open a browser's add-on store in the default browser.
    OptExtStore(&'static str),
    // Add URL: stream inspection
    /// The address looks like a manifest; go and read what it offers.
    AddrProbeStream,
    AddrStreamProbed(Box<Result<crate::engine::StreamProbe, String>>),
    AddrQuality(crate::engine::StreamQuality),
    AddrContainer(String),
    AddrRecordMinutes(String),
    OptOk,
    OptDraft(OptField),
    // update dialog
    /// Startup (or manual) check finished: newer release / up to date / error.
    UpdateChecked(Result<Option<crate::update::UpdateInfo>, String>),
    UpdateNow,
    UpdateCancel,
    UpdateOpenPage,
    /// Open a link out of the release notes (the "Full Changelog" compare
    /// URL) in the browser.
    UpdateOpenUrl(String),
    /// Progress of a running update, streamed from `update::run`.
    UpdateEvent(crate::update::UpdateEvent),
    // scheduler
    SchQueue(String),
    SchNameEdit,
    SchNameDraft(String),
    SchNameCommit,
    SchTabSet(SchTab),
    SchField(SchField),
    SchStartNow,
    SchStop,
    SchNewQueue,
    SchDeleteQueue,
    SchDragStart(DlId),
    SchDragOver(DlId),
    SchFileUp,
    SchFileDown,
    SchFileRemove,
    // batch
    BatchEdit(iced::widget::text_editor::Action),
    BatchLoaded(Option<String>),
    BatchProbed(String, Option<engine::LinkMeta>),
    /// A File Info dialog's own probe answered: the real name and size of a
    /// link that is not being downloaded yet.
    InfoProbed(DlId, Option<engine::LinkMeta>),
    BatchCheck(usize, bool),
    BatchCheckAll(bool),
    BatchSaveMode(u8),
    BatchCategory(String),
    BatchDir(String),
    BatchOk,
    BatchStreamQuality(BatchQuality),
    BatchStreamContainer(String),
    // generic dialog buttons
    ConfirmYes,
    ConfirmRemoveFile(bool),
    AddrPrefill(Option<String>),
    OpenPermissions,
    PermOpenPane(&'static str),
    PermRefresh,
    DupResume,
    DupOpen,
    DupNew,
    CloseThis(window::Id),
}

/// Options-dialog field edits, kept in one variant so `Message` stays legible.
#[derive(Clone, Debug)]
pub enum OptField {
    LaunchStartup(bool),
    CheckUpdates(bool),
    BetaChannel(bool),
    StartInTray(bool),
    CloseToTray(bool),
    /// Only where the Dock/taskbar checkbox exists (macOS, Windows, Linux).
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    HideTaskbar(bool),
    PowerSave(bool),
    GpuRender(bool),
    Clipboard(bool),
    Browser(usize, bool),
    AutoTypesEdit(iced::widget::text_editor::Action),
    SitesEdit(iced::widget::text_editor::Action),
    ExcDialog(bool),
    RememberLast(bool),
    ServerDate(bool),
    NoCatDirs(bool),
    ShowFileInfo(bool),
    StartMinimized(bool),
    SpeedTab(bool),
    CompletionTab(bool),
    HideButtons(bool),
    CompleteDialog(bool),
    UserAgent(String),
    VirusScanner(String),
    VirusArgs(String),
    BrowseVirus,
    VirusPicked(Option<String>),
    DefaultConns(usize),
    AdaptiveConns(bool),
    ExcServer(String),
    ExcConns(String),
    ExcAdd,
    DlLimit(bool),
    DlLimitMb(String),
    DlLimitHours(String),
    WarnStop(bool),
    ProxyMode(ProxyMode),
    ProxyScript(String),
    ProxyHost(String),
    ProxyPort(String),
    ProxyUser(String),
    ProxyPass(String),
    ProxyHttp(bool),
    ProxyHttps(bool),
    ProxyFtp(bool),
    FtpPasv(bool),
    SelCategory(String),
    CatDir(String),
    BrowseCatDir,
    CatDirPicked(Option<String>),
    LoginSel(usize),
    LoginSite(String),
    LoginUser(String),
    LoginPass(String),
    LoginAdd,
    LoginRemove,
    Sound(usize, bool),
    SoundBrowse(usize),
    SoundPlay(usize),
}

#[derive(Clone, Debug)]
pub enum SchField {
    Periodic(bool),
    OnStartup(bool),
    StartEnabled(bool),
    StartAt(String),
    Once(bool),
    Day(usize, bool),
    StopEnabled(bool),
    StopAt(String),
    RetriesEnabled(bool),
    Retries(String),
    OpenFileEnabled(bool),
    OpenFile(String),
    ExitDone(bool),
    ShutdownDone(bool),
    ShutdownAction(PowerAction),
    FilesAtOnce(String),
}

// ----------------------------------------------------------------------- app

pub struct App {
    pub cfg: ConfigFile,
    pub state: StateFile,
    pub windows: HashMap<window::Id, WinKind>,
    pub main_id: Option<window::Id>,
    /// Multi-selection: click selects, Cmd/Ctrl-click toggles, Shift-click
    /// extends from the anchor. First entry is the primary item.
    pub selected: Vec<DlId>,
    pub sel_anchor: Option<DlId>,
    pub mods: iced::keyboard::Modifiers,
    /// Live column widths of the download table (drag the header edges).
    pub col_widths: Vec<f32>,
    /// (column, grab x, width at grab) while a header edge is being dragged.
    pub resizing: Option<(usize, f32, f32)>,
    pub tree_sel: TreeSel,
    pub tree_open: [bool; 4], // all, unfinished, finished, queues
    /// Sidebar inline rename: the queue currently being renamed, if any,
    /// and its draft text. The draft is committed on Enter and dropped on
    /// Escape or on selecting another row; an empty, duplicate or stock
    /// name is refused and leaves the queue as it was.
    pub renaming_queue: Option<String>,
    pub queue_rename_draft: String,
    pub open_menu: Option<MenuBarKind>,
    pub open_submenu: Option<usize>,
    pub cursor: Point,
    pub ctx_at: Option<Point>,
    pub last_click: Option<(DlId, Instant)>,
    /// Last click on a queue row, for the rename double-click. The rows are
    /// buttons, and a button captures the mouse event before an enclosing
    /// `mouse_area` is asked, so `on_double_click` never fires on one:
    /// the pair is timed here instead, as the download table already does
    /// with [`Self::last_click`].
    pub last_queue_click: Option<(String, Instant)>,
    pub sort: (SortKey, bool),
    pub add_url: AddUrlState,
    pub file_info: FileInfoState,
    pub prog: HashMap<DlId, ProgState>,
    /// Virus scans in flight (and verdicts still on screen), by download.
    pub scans: HashMap<DlId, ScanState>,
    pub options: OptionsState,
    pub updater: UpdateUiState,
    pub sch: SchState,
    pub batch: BatchState,
    pub confirm: Option<ConfirmKind>,
    /// "Also remove file from disk" in the delete confirmations. Unchecked
    /// every time a confirmation opens.
    pub confirm_remove_file: bool,
    /// Items being deleted once the engine confirms their stop, with the
    /// moment the delete was requested. The instant is a deadline, not a
    /// promise: `Tick` sweeps entries whose stop event never arrived.
    pub pending_delete: Vec<(DlId, Instant)>,
    /// Deferred-persistence flags: mutations mark these, and `flush_saves`
    /// writes at most once per `Tick` (plus unconditionally on exit paths),
    /// so a 200-link batch add is one db write instead of 200.
    pub state_dirty: bool,
    pub cfg_dirty: bool,
    /// `(used, window_start)` as last written to the state db, so the
    /// download-limit counter is persisted only when it actually moved
    /// instead of once a second regardless.
    pub quota_saved: (u64, i64),
    /// Last clipboard text seen by the URL watcher (deduplication).
    pub last_clipboard: String,
    /// URL+auth waiting on the duplicate/exists decision dialog.
    pub pending_add: Option<PendingAdd>,
    /// Next capture-dialog window (File Info / Confirm / Batch) must be
    /// forced above the browser: at capture time this app is usually a
    /// background tray process, and a normal-level window opens behind the
    /// browser unfocused — the dialog floats on top.
    pub capture_raise: bool,
    /// Open split-button dropdown on the toolbar: Some(true)=Start queue,
    /// Some(false)=Stop queue.
    pub queue_menu: Option<bool>,
    /// Anchor row of an in-progress drag-selection: the first row the sweep
    /// touched. `None` while a drag that started on empty space has not
    /// reached a row yet.
    pub list_press: Option<DlId>,
    /// A left button is down somewhere in the download list — on a row or on
    /// the empty ruled area below it. Drag-selection only extends while this
    /// is set, so plain hovering never moves the selection.
    pub list_drag: bool,
    /// Rubber-band rectangle of the drag in progress, as (press point,
    /// current point) in window coordinates — the translucent box the sweep
    /// paints over the list.
    pub band: Option<(Point, Point)>,
    /// The drag started on the empty ruled area, which is always *below* the
    /// last item — so the band's fixed edge is the end of the list, not a row
    /// the sweep happened to touch. Keeping it positional means a fast sweep
    /// whose motion events coalesce still selects every row it passed.
    pub list_drag_from_empty: bool,
    /// Row under the pointer. The list rows are plain containers (a `button`
    /// would force the hand cursor and only report its press on release, which
    /// breaks drag-selection), so the hover highlight is tracked here.
    pub hover_row: Option<DlId>,
    /// Header cell under the pointer, for the same reason.
    pub hover_col: Option<usize>,
    /// Visible order snapshotted when a drag-selection starts. The sweep
    /// extends the selection on every row it crosses, and re-deriving the
    /// order there meant re-filtering and re-sorting the whole list per row.
    pub drag_order: Vec<DlId>,
    /// Scroll offset and viewport height last reported by the download
    /// table, so `ui::table` can build only the rows actually on screen.
    pub table_scroll: f32,
    pub table_vh: f32,
    /// Pointer position, kept without a message per motion event — see
    /// [`crate::ui::probe`]. Read through [`App::cursor_now`].
    pub cursor_cell: std::sync::Arc<crate::ui::probe::CursorCell>,
    /// Live results shown in the Permissions window.
    pub perm_status: crate::windows::permissions::PermStatus,
    /// The OS appearance: read at startup (`theme::system_is_dark`) and kept
    /// current by the `system::theme_changes` subscription. Only the System
    /// Default entry of View > Theme paints from it, but it is tracked
    /// whatever the setting says, so switching to it needs no round trip.
    pub system_dark: bool,
    /// Main window origin/size on screen, tracked so dialogs open centred
    /// over the application rather than the monitor.
    pub main_pos: Option<Point>,
    pub main_size: iced::Size,
}

impl App {
    pub fn item(&self, id: DlId) -> Option<&DownloadItem> {
        self.state.downloads.iter().find(|d| d.id == id)
    }

    pub fn item_mut(&mut self, id: DlId) -> Option<&mut DownloadItem> {
        self.state.downloads.iter_mut().find(|d| d.id == id)
    }

    /// A virus scanner is still working on some file.
    pub fn scanning(&self) -> bool {
        self.scans.values().any(|s| s.running())
    }

    pub fn selected_item(&self) -> Option<&DownloadItem> {
        self.selected.first().and_then(|id| self.item(*id))
    }

    /// Folder a download filed under `cat` saves into, honouring Options >
    /// Save to > "Do not create category folders". `None` means the named
    /// category is gone — the caller keeps whatever folder it already had.
    pub fn cat_dir(&self, cat: Option<&str>) -> Option<String> {
        crate::model::category_dir(
            &self.cfg.categories,
            cat,
            self.cfg.settings.no_category_dirs,
        )
    }

    /// Recompute the Permissions window's status dots.
    pub fn refresh_perm_status(&mut self) {
        let dir = self
            .cfg
            .categories
            .first()
            .map(|c| c.dir.clone())
            .unwrap_or_default();
        self.perm_status =
            crate::windows::permissions::probe(&dir, self.cfg.settings.launch_on_startup);
    }

    /// Probe write access to the default download folder; a denial opens the
    /// warning dialog so the user grants access up front instead of at the
    /// first failed download.
    pub fn check_folder_access(&mut self) -> Task<Message> {
        let Some(dir) = self.cfg.categories.first().map(|c| c.dir.clone()) else {
            return Task::none();
        };
        let probe = std::path::Path::new(&dir).join(".hydra-access-probe");
        let ok = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&probe, b"ok"))
            .map(|()| {
                let _ = std::fs::remove_file(&probe);
            })
            .is_ok();
        if ok {
            Task::none()
        } else {
            crate::log::warn(&format!("no write access to {dir}"));
            self.confirm = Some(ConfirmKind::PermissionWarn { dir });
            self.open_window(WinKind::Confirm)
        }
    }

    /// Rebuild the native macOS menu bar so its check marks match state.
    #[cfg(target_os = "macos")]
    pub fn refresh_native_menu(&self) {
        let state = self.native_menu_state();
        let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
        crate::macos_menu::reinstall(&state, &queues, &i18n::available());
    }

    /// Re-tick the menu bar for a setting it displays, without rebuilding it.
    ///
    /// muda ticks a check item on click, whatever the app then does with the
    /// activation, so the menu has to be set back to what the settings say —
    /// see `macos_menu::sync`. Only the language and the queue submenus need
    /// a rebuild; a toggle does not, and rebuilding for one used to leave
    /// View > Font showing two sizes ticked at once.
    #[cfg(target_os = "macos")]
    pub fn sync_native_menu(&self) {
        if !crate::macos_menu::sync(&self.native_menu_state()) {
            self.refresh_native_menu();
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn sync_native_menu(&self) {}

    #[cfg(target_os = "macos")]
    fn native_menu_state(&self) -> crate::macos_menu::MenuState {
        crate::macos_menu::MenuState {
            theme_mode: self.cfg.settings.theme(),
            show_categories: self.cfg.settings.show_categories,
            font_size: self.cfg.settings.font_size,
            language: {
                let l = self.cfg.language.clone().unwrap_or_else(|| "en".into());
                if l == "English" {
                    "en".into()
                } else {
                    l
                }
            },
            speed_limiter: self.cfg.settings.speed_limiter_on,
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn refresh_native_menu(&self) {}

    pub fn win_of(&self, kind: WinKind) -> Option<window::Id> {
        self.windows
            .iter()
            .find(|(_, k)| **k == kind)
            .map(|(id, _)| *id)
    }

    /// Re-fits a dialog already open at `kind` to whatever [`Self::window_size`]
    /// now answers for it — a row that only shows up in some states (an
    /// error, a credentials row) otherwise has nowhere to go until the
    /// dialog is closed and reopened.
    fn resize_open(&self, kind: WinKind) -> Task<Message> {
        match self.win_of(kind) {
            Some(id) => {
                let (w, h) = self.window_size(kind);
                window::resize(id, iced::Size::new(w, h))
            }
            None => Task::none(),
        }
    }

    /// The pointer, right now. Motion publishes no messages at all, so menu
    /// placement, the start of a drag and `Message::DragTick` all read the
    /// position from the probe.
    pub fn cursor_now(&self) -> Point {
        self.cursor_cell.get()
    }

    /// Ids of [`Self::visible`], in the same order.
    pub fn visible_ids(&self) -> Vec<DlId> {
        self.visible().iter().map(|d| d.id).collect()
    }

    /// Items shown for the current tree selection, sorted.
    pub fn visible(&self) -> Vec<&DownloadItem> {
        let mut v: Vec<&DownloadItem> = self
            .state
            .downloads
            .iter()
            .filter(|d| match &self.tree_sel {
                TreeSel::All => true,
                TreeSel::Cat(c) => d.category.as_deref() == Some(c.as_str()),
                TreeSel::Unfinished => d.state != DlState::Complete,
                TreeSel::UnfCat(c) => {
                    d.state != DlState::Complete && d.category.as_deref() == Some(c.as_str())
                }
                TreeSel::Finished => d.state == DlState::Complete,
                TreeSel::FinCat(c) => {
                    d.state == DlState::Complete && d.category.as_deref() == Some(c.as_str())
                }
                TreeSel::Queues => d.queue.is_some(),
                TreeSel::Queue(q) => d.queue.as_deref() == Some(q.as_str()),
            })
            .collect();
        let (key, asc) = self.sort;
        // Name and Status build a key per item instead of comparing on the
        // fly: `to_lowercase` and `status_text` each allocate, and this runs
        // on every rebuild — two allocations per *comparison* put O(n log n)
        // of them on the pointer's event rate during a drag.
        match key {
            SortKey::Name => sort_keyed(&mut v, asc, |d| d.file_name.to_lowercase()),
            SortKey::Status => sort_keyed(&mut v, asc, DownloadItem::status_text),
            _ => v.sort_by(|a, b| {
                let ord = match key {
                    SortKey::Queue => a.queue.cmp(&b.queue).then(a.q_order.cmp(&b.q_order)),
                    SortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
                    SortKey::TimeLeft => a
                        .eta_secs
                        .unwrap_or(u64::MAX)
                        .cmp(&b.eta_secs.unwrap_or(u64::MAX)),
                    SortKey::Rate => a
                        .rate
                        .partial_cmp(&b.rate)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    SortKey::LastTry => a.last_try.cmp(&b.last_try),
                    SortKey::Description => a.description.cmp(&b.description),
                    // Handled above.
                    SortKey::Name | SortKey::Status => std::cmp::Ordering::Equal,
                };
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            }),
        }
        v
    }

    /// Mark the download list for persistence. The actual write happens in
    /// [`Self::flush_saves`] — `model::save_state` rewrites the whole redb
    /// table, so per-mutation writes made a 200-link batch add O(n²) and put
    /// a full serialise-and-commit on the UI thread every second.
    fn save_state(&mut self) {
        self.state_dirty = true;
    }

    /// Mark the configuration for persistence (see [`Self::save_state`]).
    fn save_config(&mut self) {
        self.cfg_dirty = true;
    }

    /// Write whatever is marked dirty. Called once per `Tick` and on every
    /// exit path, so at most one second of changes is ever in flight.
    pub fn flush_saves(&mut self) {
        if self.state_dirty {
            self.state_dirty = false;
            model::save_state(&self.state);
        }
        if self.cfg_dirty {
            self.cfg_dirty = false;
            model::save_config(&self.cfg);
            // The extension mirrors capture settings; every socket reply
            // carries the latest snapshot.
            crate::extbus::publish_config(&self.cfg);
        }
    }

    /// Attach what the browser knew that `add_item` could not: session
    /// cookies and the already-resolved filename. Must run before any start
    /// so the first request carries the Cookie header; renaming re-runs
    /// categorization because the URL-derived name may have had no
    /// extension at all.
    fn apply_capture_extras(&mut self, id: DlId, cookies: Option<String>, name: Option<String>) {
        if cookies.is_none() && name.is_none() {
            return;
        }
        let cats = self.cfg.categories.clone();
        if let Some(d) = self.item_mut(id) {
            if let Some(c) = cookies {
                d.cookies = Some(c);
            }
            if let Some(n) = name {
                d.file_name = n.clone();
                d.category = categorize(&n, &cats);
                if let Some(dir) = d
                    .category
                    .as_deref()
                    .and_then(|c| cats.iter().find(|k| k.name == c))
                    .map(|c| c.dir.clone())
                {
                    d.save_dir = dir;
                }
            }
        }
    }

    // -------------------------------------------------------------- windows

    /// Open the Configuration window on a fresh draft of the saved settings.
    /// `tab` forces a page (the toolbar's Extensions shortcut); `None` keeps
    /// whichever page was last visited.
    fn open_options(&mut self, tab: Option<OptTab>) -> Task<Message> {
        self.options.draft = self.cfg.settings.clone();
        self.options.draft_cats = self.cfg.categories.clone();
        self.options.sel_category = "General".into();
        self.options.dl_limit_mb_txt = self.cfg.settings.dl_limit_mb.to_string();
        self.options.dl_limit_hours_txt = self.cfg.settings.dl_limit_hours.to_string();
        self.options.auto_types_edit =
            iced::widget::text_editor::Content::with_text(&self.cfg.settings.auto_types);
        self.options.sites_edit =
            iced::widget::text_editor::Content::with_text(&self.cfg.settings.dont_start_sites);
        if let Some(t) = tab {
            self.options.tab = t;
        }
        self.open_window(WinKind::Options)
    }

    /// The View > Font ratio the windows are laid out at.
    fn ui_scale(&self) -> f32 {
        crate::theme::ui_scale(self.cfg.settings.font_size)
    }

    /// The size a window opens at, in OS points.
    ///
    /// Dialogs are laid out against `theme::FONT_SIZE`, and View > Font
    /// scales the interface by the ratio to it (`scale_of` in main.rs), so
    /// the window has to grow by the same ratio or a larger font just loses
    /// the bottom row of the dialog. The main window is the exception: it is
    /// resizable and reopens at whatever size it was left at.
    fn window_size(&self, kind: WinKind) -> (f32, f32) {
        if kind == WinKind::Main {
            // Restore the last size when it is still sane for a screen;
            // first run (or nonsense values) derives from the display. The
            // floor only rejects nonsense: `min_size` below holds the window
            // to a full toolbar row whatever the saved size says, and that
            // floor moves with the font ratio while this range does not.
            let saved = self
                .cfg
                .settings
                .window_size
                .filter(|(w, h)| (400.0..=4000.0).contains(w) && (300.0..=2500.0).contains(h));
            let s = saved
                .map(|(w, h)| iced::Size::new(w, h))
                .unwrap_or_else(main_window_size);
            return (s.width, s.height);
        }
        let (w, h) = match kind {
            WinKind::Main => unreachable!("handled above"),
            // Base layout plus the credentials row when "Use authorization"
            // is on, plus the error/blocked-site line when one is showing —
            // both add a real row that the fixed base height has no room
            // for, so the message otherwise runs past the window's bottom.
            WinKind::AddUrl => {
                let mut h = 168.0;
                if self.add_url.use_auth {
                    h += 40.0;
                }
                let warn = self.add_url.error.is_some()
                    || (!self.add_url.address.trim().is_empty()
                        && site_blocked(
                            self.add_url.address.trim(),
                            &self.cfg.settings.dont_start_sites,
                        ));
                if warn {
                    h += 40.0;
                }
                // The stream panel is a BLOCK, not a line: a "Stream" row,
                // the quality and container pickers, and for a live stream
                // the record-minutes row. None of it was accounted for, so a
                // probed manifest pushed its own pickers past the bottom of
                // the window — the quality list the user is being asked to
                // choose from was exactly the part that fell off.
                h += match &self.add_url.stream {
                    _ if self.add_url.stream_probing => 28.0,
                    // A refusal offers nothing to choose, but the sentence
                    // explaining it wraps to two or three lines.
                    Some(p) if p.drm.is_some() => 64.0,
                    Some(p) if p.live => 108.0,
                    Some(_) => 68.0,
                    None => 0.0,
                };
                (760.0, h)
            }
            // New-download layout is short; Properties adds status/size/
            // login/cookies/history rows. Size the window to the mode so
            // neither shows dead space.
            WinKind::FileInfo(_) => {
                if self.file_info.is_new {
                    (720.0, 356.0)
                } else {
                    (720.0, 560.0)
                }
            }
            // Matches ProgToggleDetails: a box whose details are hidden
            // must not spring back open when the font ratio resizes it.
            WinKind::Progress(id) => {
                let details = self.prog.get(&id).map(|p| p.details).unwrap_or(true);
                (680.0, if details { 582.0 } else { 352.0 })
            }
            WinKind::Complete(_) => (600.0, 230.0),
            WinKind::Options => (760.0, 700.0),
            WinKind::Scheduler => (950.0, 660.0),
            WinKind::Batch => (950.0, 700.0),
            WinKind::About => (460.0, 300.0),
            WinKind::Shortcuts => (520.0, 460.0),
            WinKind::Confirm => (500.0, 200.0),
            WinKind::Permissions => (640.0, 580.0),
            WinKind::Update => (560.0, 520.0),
        };
        let s = self.ui_scale();
        (w * s, h * s)
    }

    pub fn open_window(&mut self, kind: WinKind) -> Task<Message> {
        if let Some(id) = self.win_of(kind) {
            return window::gain_focus(id);
        }
        // One File Info dialog at a time — see close_file_info_windows for
        // why a second one strands the first. The download behind the
        // dismissed dialog is already in the list (and may be pulling bytes
        // in the background), so nothing is lost by closing its window.
        let dismissed = if matches!(kind, WinKind::FileInfo(_)) {
            if self
                .windows
                .values()
                .any(|k| matches!(k, WinKind::FileInfo(_)))
            {
                crate::log::info("file info: superseding the open dialog with a new one");
            }
            self.close_file_info_windows()
        } else {
            Task::none()
        };
        // Dialogs are fixed-size and cannot minimize; only the main window
        // and the download-progress box resize, and only they minimize to
        // the dock on their own.
        let size = self.window_size(kind);
        let resizable = matches!(kind, WinKind::Main | WinKind::Progress(_));
        let minimizable = resizable;
        // Centre sub-windows over the main window when its bounds are known.
        let position = match (kind, self.main_pos) {
            (WinKind::Main, _) | (_, None) => window::Position::Centered,
            (_, Some(origin)) => window::Position::Specific(Point::new(
                origin.x + (self.main_size.width - size.0) / 2.0,
                origin.y + (self.main_size.height - size.1) / 2.0,
            )),
        };
        // "Hide from taskbar" on Windows is a per-window creation flag, so
        // it reaches windows opened after the toggle (the main window picks
        // it up on its next open from the tray).
        #[cfg(target_os = "windows")]
        let platform_specific = window::settings::PlatformSpecific {
            skip_taskbar: self.cfg.settings.hide_from_taskbar,
            ..Default::default()
        };
        // Linux: the app id is how the desktop matches a window back to its
        // .desktop file — GNOME/KDE read the Wayland app_id (and the X11
        // WM_CLASS) and look up Icon= there for the dock, the alt-tab
        // switcher and the process list. winit leaves it empty by default,
        // and Wayland has no per-window icon protocol, so the `icon:` field
        // below is X11/Windows-only: without this the shell has nothing to
        // match on and falls back to a generic icon even though the app
        // menu entry shows the logo. Must stay equal to the basename of
        // hydra.desktop (scripts/package-linux.sh, install.sh).
        #[cfg(target_os = "linux")]
        let platform_specific = window::settings::PlatformSpecific {
            application_id: "hydra".to_string(),
            ..Default::default()
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let platform_specific = window::settings::PlatformSpecific::default();
        let (id, task) = window::open(window::Settings {
            size: iced::Size::new(size.0, size.1),
            // Floor: just enough for the full toolbar row; the default
            // stays proportional to the display.
            min_size: (kind == WinKind::Main).then(|| {
                let s = self.ui_scale();
                iced::Size::new(900.0 * s, 600.0 * s)
            }),
            resizable,
            minimizable,
            position,
            exit_on_close_request: false,
            // Title-bar/taskbar logo on Windows and Linux (None on macOS,
            // where the app bundle supplies the Dock icon).
            icon: crate::icons::window_icon(),
            platform_specific,
            ..window::Settings::default()
        });
        if kind == WinKind::Main {
            self.main_id = Some(id);
        }
        self.windows.insert(id, kind);
        let opened = task.map(Message::WindowOpened);
        if matches!(kind, WinKind::Progress(_)) && self.cfg.settings.start_minimized {
            return Task::batch([dismissed, opened, window::minimize(id, true)]);
        }
        Task::batch([dismissed, opened])
    }

    /// Re-assert "Hide from taskbar" on one window. Windows carries the flag
    /// at creation and macOS hides the whole app through the Dock policy, so
    /// this only does anything on Linux — where it is an X11 window-manager
    /// hint (Wayland has no equivalent; see `linux_taskbar`).
    #[allow(unused_variables)]
    fn skip_taskbar_task(&self, id: window::Id) -> Task<Message> {
        #[cfg(target_os = "linux")]
        {
            let hide = self.cfg.settings.hide_from_taskbar;
            window::run(id, move |w| crate::linux_taskbar::apply(w, hide)).map(|_| Message::Noop)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Task::none()
        }
    }

    fn close_window(&mut self, kind: WinKind) -> Task<Message> {
        if let Some(id) = self.win_of(kind) {
            self.windows.remove(&id);
            window::close(id)
        } else {
            Task::none()
        }
    }

    /// Close every Download File Info window, whichever download each one
    /// was opened for.
    ///
    /// The dialog's state is a single slot (`self.file_info`) while the
    /// window identity carries a `DlId`, so two of them can never be
    /// consistent: both windows render the newer download, and the older
    /// one's buttons act on it and close the *other* window — after which
    /// they close nothing at all and the dialog is stuck on screen. The
    /// dialog is therefore opened one at a time, and its buttons dismiss
    /// whatever File Info window exists rather than one specific id.
    fn close_file_info_windows(&mut self) -> Task<Message> {
        let ids: Vec<window::Id> = self
            .windows
            .iter()
            .filter(|(_, k)| matches!(k, WinKind::FileInfo(_)))
            .map(|(id, _)| *id)
            .collect();
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            self.windows.remove(&id);
            tasks.push(window::close(id));
        }
        Task::batch(tasks)
    }

    // ------------------------------------------------------------ downloads

    /// Effective connection count for a URL (Connection tab exceptions).
    fn conns_for(&self, url: &str) -> usize {
        let host = engine::parse_url(url).map(|u| u.host).unwrap_or_default();
        self.cfg
            .settings
            .conn_exceptions
            .iter()
            .find(|(server, _)| {
                !server.is_empty() && host.ends_with(server.trim_start_matches("*."))
            })
            .map(|(_, n)| *n)
            .unwrap_or(self.cfg.settings.default_conns)
            .clamp(1, 32)
    }

    /// Saved credentials for a URL from Options > Sites Logins.
    fn login_for(&self, url: &str) -> Option<(String, String)> {
        find_login(url, &self.cfg.settings.logins).map(|l| (l.user.clone(), l.pass.clone()))
    }

    /// The cap this download is actually running under: its own if it set one,
    /// otherwise the global Speed Limiter's while that is switched on. This is
    /// the figure the engine was handed, so it is also the one the views may
    /// present as the transfer's ceiling.
    pub(crate) fn effective_limit(&self, d: &DownloadItem) -> Option<u64> {
        d.speed_limit.or(if self.cfg.settings.speed_limiter_on {
            self.cfg.settings.global_speed_limit
        } else {
            None
        })
    }

    // ------------------------------------------------- download limit

    fn quota_exhausted(&self) -> bool {
        quota_over_cap(&self.cfg.settings, &self.state.dl_quota)
    }

    /// Charge freshly arrived bytes against the current window.
    ///
    /// The caller passes the delta rather than a running total because
    /// `downloaded` can move *backwards* — a repair shrinks the held set —
    /// and a repair must not refund bytes that crossed the wire.
    fn quota_account(&mut self, bytes: u64) {
        if bytes == 0 || quota_cap(&self.cfg.settings).is_none() {
            return;
        }
        let q = &mut self.state.dl_quota;
        // The window opens at the first accounted byte, not at app start: an
        // idle Hydra must not burn through periods it never downloaded in.
        if q.window_start == 0 {
            q.window_start = fmt::now_unix();
        }
        q.used = q.used.saturating_add(bytes);
    }

    /// Roll the window when it has elapsed, then reconcile the list with the
    /// quota: over the cap, active transfers are parked; under it (rollover,
    /// a raised cap, or the limit switched off), the transfers the limiter
    /// itself parked come back.
    fn quota_tick(&mut self) -> Task<Message> {
        let q = &self.state.dl_quota;
        if (q.used, q.window_start) != self.quota_saved {
            self.quota_saved = (q.used, q.window_start);
            model::save_quota(q);
        }
        if quota_window_elapsed(&self.cfg.settings, &self.state.dl_quota, fmt::now_unix()) {
            crate::log::info(&format!(
                "download limit: window elapsed after {}; counter reset",
                fmt::size2(self.state.dl_quota.used)
            ));
            self.state.dl_quota = DlQuota::default();
            self.save_state();
        }
        if self.quota_exhausted() {
            self.quota_park()
        } else {
            self.quota_release()
        }
    }

    /// Stop everything still transferring and mark it as the limiter's doing,
    /// so the next window can tell it apart from a hand-paused item.
    fn quota_park(&mut self) -> Task<Message> {
        let ids = quota_park_targets(&self.state.downloads);
        if ids.is_empty() {
            return Task::none();
        }
        crate::log::info(&format!(
            "download limit reached ({} of {} MB in {} h): pausing {} transfer(s)",
            fmt::size2(self.state.dl_quota.used),
            self.cfg.settings.dl_limit_mb,
            self.cfg.settings.dl_limit_hours,
            ids.len(),
        ));
        for id in ids {
            self.stop_download(id);
            if let Some(d) = self.item_mut(id) {
                d.limit_paused = true;
                d.status_line = i18n::tr("Download limit reached");
            }
        }
        self.save_state();
        Task::none()
    }

    /// Release what the limiter parked. Queue members go back to `Queued` and
    /// let `queue_tick` pace them against `files_at_once`; direct downloads
    /// start again immediately.
    fn quota_release(&mut self) -> Task<Message> {
        let released = quota_release_targets(&mut self.state.downloads);
        // The common case, on every tick: nothing was parked, nothing to do —
        // and in particular nothing to mark dirty, since a save rewrites the
        // whole download table.
        if released.is_empty() {
            return Task::none();
        }
        crate::log::info(&format!(
            "download limit: window open again, resuming {} transfer(s)",
            released.len()
        ));
        let mut tasks = Vec::new();
        for (id, requeued) in released {
            if !requeued {
                tasks.push(self.start_download(id, false));
            }
        }
        self.save_state();
        Task::batch(tasks)
    }

    pub fn start_download(&mut self, id: DlId, open_progress: bool) -> Task<Message> {
        // Over the window's cap nothing new goes on the wire. The item is
        // marked as the limiter's, so the tick that reopens the window picks
        // it up instead of leaving it paused until someone notices.
        if self.quota_exhausted() {
            if let Some(d) = self.item_mut(id) {
                if !d.state.is_active() {
                    d.limit_paused = true;
                    d.status_line = i18n::tr("Download limit reached");
                }
            }
            return Task::none();
        }
        let user_agent = self.cfg.settings.user_agent.clone();
        let adaptive = self.cfg.settings.adaptive_conns;
        // Read here with the others: the item is borrowed mutably below.
        let remote_time = self.cfg.settings.server_file_date;
        // Read before the item is borrowed mutably below; a stream uses the
        // same connection count a file download would.
        let stream_conns = self
            .item(id)
            .map(|d| self.conns_for(&d.url))
            .unwrap_or_default();
        // Same source the ranged path uses, read before the mutable borrow.
        let stream_limit = self.item(id).and_then(|d| self.effective_limit(d));
        // Re-downloading over a file a scan flagged: the old verdict and its
        // log describe bytes that are being replaced.
        crate::scan::skip(id);
        self.scans.remove(&id);
        let Some(d) = self.item_mut(id) else {
            return Task::none();
        };
        if d.state.is_active() {
            return Task::none();
        }
        d.state = DlState::Connecting;
        // Whatever parked it, it is running now: the flag must not survive
        // into the next window and resume the same download twice.
        d.limit_paused = false;
        d.status_line = i18n::tr("Connecting...");
        d.last_try = Some(fmt::now_unix());
        d.conns.clear();
        let part = d.part_file().to_string_lossy().into_owned();
        d.part_path = Some(part.clone());
        if let Some(si) = d.stream.clone() {
            // Streams keep their own per-track checkpoints beside this
            // staging path; the span bookkeeping below describes byte ranges
            // of ONE file and would wrongly wipe `downloaded` here. What is
            // already on disk is reported by the engine's first Progress.
            let ss = engine::StreamSpec {
                id,
                manifest: d.url.clone(),
                protocol: si.protocol,
                variant_url: si.variant_url,
                height: si.height,
                bandwidth: si.bandwidth,
                container: si.container,
                cookies: d.cookies.clone(),
                referer: si.referer,
                // The browser's UA when the extension sent one: the origin
                // handed the manifest to THAT client, not to Hydra.
                user_agent: si.user_agent.unwrap_or(user_agent),
                temp_path: part,
                final_path: d.full_path().to_string_lossy().into_owned(),
                // The same connection count a file download would use.
                conns: stream_conns,
                max_seconds: si.max_seconds,
                limit: stream_limit,
            };
            engine::send(Cmd::StartStream(Box::new(ss)));
            self.save_state();
            return if open_progress {
                let seed = self.item(id).and_then(|d| d.speed_limit);
                self.prog.entry(id).or_insert_with(|| prog_state_seed(seed));
                self.open_window(WinKind::Progress(id))
            } else {
                Task::none()
            };
        }
        // A resume is only a resume while the staging file still matches the
        // spans we recorded for it. Persisted spans are sanitized first —
        // `mark_done` must never be handed overlapping or inverted ranges
        // from a stale state file — and the `.part` is checked by length AND
        // allocation, because `SparseSink::create` calls `set_len(size)`
        // before the first byte: a file that received 1 byte and a file that
        // received everything both report `size`, so length alone would let
        // a truncated or foreign file "complete" over holes of zeros.
        d.held = sanitize_spans(&d.held, d.size);
        let part_ok = part_matches(std::path::Path::new(&part), d.size, &d.held);
        if !d.held.is_empty() && !part_ok {
            crate::log::warn(&format!(
                "#{id} staging file does not match recorded spans: restarting from zero"
            ));
            d.held.clear();
        }
        // `downloaded` is derived from `held`, never stored independently:
        // the two drifting apart is what made the bar and the chunk strip
        // disagree after a repair.
        d.downloaded = sum_spans(&d.held);
        if d.held.is_empty() {
            d.disp_progress = 0.0;
        }
        let spec = StartSpec {
            id,
            url: d.url.clone(),
            auth: d.auth.clone(),
            conns: 0, // filled below (borrow)
            user_agent,
            temp_path: part,
            final_path: d.full_path().to_string_lossy().into_owned(),
            held: d.held.clone(),
            expected_size: d.size,
            cookies: d.cookies.clone(),
            limit: None,
            adaptive,
            remote_time,
        };
        let url = d.url.clone();
        let limit = self.effective_limit(self.item(id).unwrap());
        let spec = StartSpec {
            conns: self.conns_for(&url),
            limit,
            auth: spec.auth.clone().or_else(|| self.login_for(&url)),
            ..spec
        };
        engine::send(Cmd::Start(Box::new(spec)));
        self.save_state();
        if open_progress {
            let seed = self.item(id).and_then(|d| d.speed_limit);
            self.prog.entry(id).or_insert_with(|| prog_state_seed(seed));
            self.open_window(WinKind::Progress(id))
        } else {
            Task::none()
        }
    }

    fn stop_download(&mut self, id: DlId) {
        if let Some(d) = self.item_mut(id) {
            if d.state.is_active() {
                engine::send(Cmd::Stop(id));
                d.status_line = i18n::tr("Pause");
            } else if d.state == DlState::Queued {
                d.state = DlState::Paused;
            }
        }
    }

    /// Stop the given items, honouring "Warn me before stopping downloads":
    /// with the flag set and at least one target actually transferring, the
    /// stop is parked behind a confirmation. `stop_queues` carries the
    /// Pause All / Stop All variant, which also halts queue processing.
    fn stop_ids_confirming(&mut self, ids: Vec<DlId>, stop_queues: bool) -> Task<Message> {
        let any_active = ids
            .iter()
            .any(|id| self.item(*id).map(|d| d.state.is_active()).unwrap_or(false));
        if self.cfg.settings.warn_before_stop && any_active {
            self.confirm = Some(ConfirmKind::StopWarn { ids, stop_queues });
            return self.open_window(WinKind::Confirm);
        }
        self.stop_ids(ids, stop_queues);
        Task::none()
    }

    fn stop_ids(&mut self, ids: Vec<DlId>, stop_queues: bool) {
        for id in ids {
            self.stop_download(id);
        }
        if stop_queues {
            for q in &mut self.cfg.queues {
                q.running = false;
            }
        }
    }

    fn delete_item(&mut self, id: DlId) -> Task<Message> {
        self.delete_item_opts(id, false)
    }

    fn delete_item_opts(&mut self, id: DlId, remove_file: bool) -> Task<Message> {
        let active = self.item(id).map(|d| d.state.is_active()).unwrap_or(false);
        if active {
            engine::send(Cmd::Stop(id));
            self.pending_delete.push((id, Instant::now()));
        } else {
            self.remove_item_opts(id, remove_file);
        }
        let mut task = self.close_window(WinKind::Progress(id));
        if self.win_of(WinKind::FileInfo(id)).is_some() {
            task = Task::batch([task, self.close_window(WinKind::FileInfo(id))]);
        }
        task
    }

    /// `pending_delete` is a deadline, not a promise: the engine's stop
    /// acknowledgement can be lost in the start/stop race window, and an id
    /// stuck in the list would silently delete a future download. Remove an
    /// entry as soon as its item stopped being active, and after a few
    /// seconds regardless.
    fn sweep_pending_deletes(&mut self) {
        if self.pending_delete.is_empty() {
            return;
        }
        let expired: Vec<DlId> = self
            .pending_delete
            .iter()
            .filter(|(id, at)| {
                at.elapsed().as_secs() >= 5
                    || !self.item(*id).map(|d| d.state.is_active()).unwrap_or(false)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.pending_delete.retain(|(x, _)| *x != id);
            self.remove_item(id);
        }
    }

    fn remove_item(&mut self, id: DlId) {
        self.remove_item_opts(id, false);
    }

    fn remove_item_opts(&mut self, id: DlId, remove_file: bool) {
        if let Some(d) = self.item(id) {
            if d.state != DlState::Complete {
                let _ = std::fs::remove_file(d.part_file());
            } else if remove_file {
                let _ = std::fs::remove_file(d.full_path());
            }
        }
        self.state.downloads.retain(|d| d.id != id);
        self.selected.retain(|x| *x != id);
        self.prog.remove(&id);
        // A scanner still holding the file the row just took with it has
        // nothing left to report.
        crate::scan::skip(id);
        self.scans.remove(&id);
        self.save_state();
    }

    /// Create a new list entry for a URL; returns its id. Every download
    /// belongs to a queue (the first one — "Main download queue" — unless the
    /// caller names another), so Start/Stop Queue always govern the whole
    /// list.
    pub fn add_item(
        &mut self,
        url: String,
        auth: Option<(String, String)>,
        queue: Option<String>,
    ) -> DlId {
        let queue = queue.or_else(|| self.cfg.queues.first().map(|q| q.name.clone()));
        let id = self.state.next_id;
        self.state.next_id += 1;
        let file_name = engine::file_name_from_url(&url);
        let category = categorize(&file_name, &self.cfg.categories);
        let save_dir = self
            .cat_dir(category.as_deref())
            .or_else(|| self.cat_dir(None))
            .unwrap_or_else(|| ".".into());
        let q_order = self
            .state
            .downloads
            .iter()
            .filter(|d| d.queue == queue && queue.is_some())
            .map(|d| d.q_order + 1)
            .max()
            .unwrap_or(0);
        self.state.downloads.push(DownloadItem {
            id,
            url,
            file_name,
            save_dir,
            category,
            description: String::new(),
            size: None,
            downloaded: 0,
            state: DlState::Paused,
            error: None,
            resume: None,
            added: fmt::now_unix(),
            last_try: None,
            queue,
            q_order,
            auth,
            cookies: None,
            speed_limit: None,
            limit_paused: false,
            held: vec![],
            part_path: None,
            rate: 0.0,
            retries: 0,
            disp_progress: 0.0,
            eta_secs: None,
            recorded_secs: None,
            conns: vec![],
            status_line: String::new(),
            shutdown_after: false,
            shutdown_action: PowerAction::default(),
            stream: None,
        });
        self.save_state();
        id
    }

    // -------------------------------------------------------- virus scan

    /// Hand a finished file to the configured scanner. `true` when a scan
    /// really started — the caller then defers the completion tail
    /// ([`Self::finish_completion`]) until the verdict arrives.
    fn start_virus_scan(&mut self, id: DlId) -> bool {
        let program = self.cfg.settings.virus_scanner.trim().to_string();
        if program.is_empty() {
            return false;
        }
        let Some(path) = self.item(id).map(|d| d.full_path()) else {
            return false;
        };
        // A scanner path that no longer resolves must not swallow the
        // completion tail: the file is downloaded either way.
        if !std::path::Path::new(&program).is_file() {
            crate::log::warn(&format!("virus scanner not found: {program}"));
            return false;
        }
        let args = self.cfg.settings.virus_args.clone();
        let mut st = ScanState::default();
        st.push(i18n::tr("Start scanning downloaded data..."), false);
        self.scans.insert(id, st);
        if let Some(d) = self.item_mut(id) {
            d.status_line = i18n::tr("Virus scanning...");
            // The connection rows describe a transfer that is over; the
            // details panel shows the scanner log in their place.
            d.conns.clear();
        }
        crate::scan::start(id, &program, &args, &path.to_string_lossy());
        true
    }

    /// The tail every finished download ends with: chime, close the progress
    /// dialog, pop the complete dialog. Runs straight away when no scanner is
    /// configured, and after a clean (or skipped) scan otherwise.
    fn finish_completion(&mut self, id: DlId) -> Task<Message> {
        if let Some(row) = self.cfg.settings.sounds.first().filter(|r| r.enabled) {
            sounds::play(
                (!row.file.is_empty()).then(|| row.file.clone()),
                sounds::Event::DownloadComplete,
            );
        }
        let task = self.close_window(WinKind::Progress(id));
        // Shutting down makes the complete dialog moot — flush state first,
        // since the OS call may end the process almost immediately.
        if let Some(action) = self
            .item(id)
            .filter(|d| d.shutdown_after)
            .map(|d| d.shutdown_action)
        {
            self.save_state();
            self.save_config();
            self.flush_saves();
            run_power_action(action);
            return Task::batch([task, iced::exit()]);
        }
        if self.cfg.settings.show_complete_dialog {
            return Task::batch([task, self.open_window(WinKind::Complete(id))]);
        }
        task
    }

    // --------------------------------------------------------------- queues

    /// Records a click on the queue row `name` and answers whether it closed
    /// a double-click. `None` is a click that landed somewhere other than a
    /// queue row, which restarts the count.
    fn queue_click(&mut self, name: Option<&str>) -> bool {
        double_click(&mut self.last_queue_click, name)
    }

    /// Renames a queue and every download filed under it, then refreshes
    /// the menu bar / tray submenus that carry queue names. Refuses an
    /// empty name, a name already taken by another queue, or renaming a
    /// stock queue (Main download queue / Synchronization queue). Returns
    /// whether the rename went through.
    fn rename_queue(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        let taken = self
            .cfg
            .queues
            .iter()
            .any(|q| q.name == new && q.name != old);
        let builtin = self
            .cfg
            .queues
            .iter()
            .find(|q| q.name == old)
            .map(|q| q.builtin)
            .unwrap_or(true);
        if new.is_empty() || taken || new == old || builtin {
            return false;
        }
        if let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == old) {
            q.name = new.to_string();
        }
        for d in &mut self.state.downloads {
            if d.queue.as_deref() == Some(old) {
                d.queue = Some(new.to_string());
            }
        }
        self.save_config();
        self.save_state();
        self.refresh_native_menu();
        let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
        crate::tray::reinstall(&queues, self.cfg.settings.power_save);
        true
    }

    fn queue_tick(&mut self) -> Task<Message> {
        let mut to_start: Vec<DlId> = vec![];
        let mut worked: Vec<String> = vec![];
        for q in &self.cfg.queues {
            if !q.running {
                continue;
            }
            let active = self
                .state
                .downloads
                .iter()
                .filter(|d| d.queue.as_deref() == Some(q.name.as_str()) && d.state.is_active())
                .count();
            if active > 0 {
                worked.push(q.name.clone());
            }
            if active >= q.files_at_once as usize {
                continue;
            }
            let mut queued: Vec<&DownloadItem> = self
                .state
                .downloads
                .iter()
                .filter(|d| {
                    d.queue.as_deref() == Some(q.name.as_str()) && d.state == DlState::Queued
                })
                .collect();
            queued.sort_by_key(|d| d.q_order);
            if !queued.is_empty() && active == 0 {
                worked.push(q.name.clone());
            }
            for d in queued.iter().take(q.files_at_once as usize - active) {
                to_start.push(d.id);
            }
        }
        for name in worked {
            if let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == name) {
                q.did_work = true;
            }
        }
        let tasks: Vec<Task<Message>> = to_start
            .into_iter()
            .map(|id| self.start_download(id, false))
            .collect();
        // A running queue with nothing active and nothing queued is finished:
        // apply its "when done" actions — but only when this run actually
        // processed something. A queue started with nothing to do (or whose
        // scheduled start found no members) must not play the stop sound or
        // fire `exit_when_done` on the very tick it began.
        let finished: Vec<String> = self
            .cfg
            .queues
            .iter()
            .filter(|q| {
                q.running
                    && !self.state.downloads.iter().any(|d| {
                        d.queue.as_deref() == Some(q.name.as_str())
                            // `limit_paused` counts as outstanding work: a
                            // queue whose files the download limit parked is
                            // waiting for the next window, not finished, and
                            // must not fire its sound or `exit_when_done`.
                            && (d.state.is_active()
                                || d.state == DlState::Queued
                                || d.limit_paused)
                    })
            })
            .map(|q| q.name.clone())
            .collect();
        let mut exit_app = false;
        let mut power_action: Option<PowerAction> = None;
        for name in &finished {
            let mut acted = false;
            if let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == *name) {
                q.running = false;
                acted = std::mem::take(&mut q.did_work);
                let sc = &q.schedule;
                if acted {
                    if sc.open_file_enabled && !sc.open_file.trim().is_empty() {
                        let _ = open::that_detached(sc.open_file.trim());
                    }
                    if sc.exit_when_done {
                        exit_app = true;
                    }
                    if sc.shutdown_when_done {
                        power_action = Some(sc.shutdown_action);
                    }
                }
            }
            if acted {
                if let Some(row) = self.cfg.settings.sounds.get(3).filter(|r| r.enabled) {
                    sounds::play(
                        (!row.file.is_empty()).then(|| row.file.clone()),
                        sounds::Event::QueueStopped,
                    );
                }
                crate::log::info(&format!("queue finished: {name}"));
            }
        }
        if exit_app || power_action.is_some() {
            self.save_state();
            self.save_config();
            self.flush_saves();
            if let Some(action) = power_action {
                run_power_action(action);
            }
            return Task::batch([Task::batch(tasks), iced::exit()]);
        }
        Task::batch(tasks)
    }

    fn schedule_tick(&mut self) {
        use chrono::Datelike;
        let now = chrono::Local::now();
        let hhmm = fmt::hhmm(&now);
        let weekday = now.weekday().num_days_from_sunday() as usize; // Sun=0
        let mut start: Vec<String> = vec![];
        let mut stop: Vec<String> = vec![];
        for q in &self.cfg.queues {
            let s = &q.schedule;
            if schedule_start_due(s, &hhmm, weekday, q.running) {
                start.push(q.name.clone());
            }
            if s.stop_enabled && s.stop_at == hhmm && q.running {
                stop.push(q.name.clone());
            }
        }
        for name in start {
            // "Once" means once: disarm after firing, or the same wall-clock
            // minute would re-trigger it every day.
            if let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == name) {
                if q.schedule.once {
                    q.schedule.start_enabled = false;
                    self.save_config();
                }
            }
            self.set_queue_running(&name, true);
        }
        for name in stop {
            self.set_queue_running(&name, false);
        }
    }

    pub fn set_queue_running(&mut self, name: &str, run: bool) {
        let was = self
            .cfg
            .queues
            .iter()
            .find(|q| q.name == name)
            .map(|q| q.running)
            .unwrap_or(false);
        // Promotion lives HERE, on the one function every start path shares
        // (menu, toolbar, scheduler wall-clock, boot): a queue whose members
        // are all Paused would otherwise turn `running = true`, find nothing
        // Queued, and be classified as instantly finished.
        if run {
            promote_queue_members(&mut self.state.downloads, name);
        }
        if let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == name) {
            q.running = run;
        }
        if run != was {
            let idx = if run { 2 } else { 3 };
            if let Some(row) = self.cfg.settings.sounds.get(idx).filter(|r| r.enabled) {
                sounds::play(
                    (!row.file.is_empty()).then(|| row.file.clone()),
                    sounds::Event::from_index(idx),
                );
            }
        }
        if !run {
            let ids: Vec<DlId> = self
                .state
                .downloads
                .iter()
                .filter(|d| d.queue.as_deref() == Some(name) && d.state.is_active())
                .map(|d| d.id)
                .collect();
            for id in ids {
                engine::send(Cmd::Stop(id));
            }
            // Anything still queued stays queued for the next start.
        }
    }

    // -------------------------------------------------------------- engine

    /// Adopt what a probe learned about `id`: its size, and the name the
    /// object actually landed under — a `Content-Disposition`, or the last
    /// segment of a URL the engine had to resolve redirects to reach.
    ///
    /// Shared by the two places a probe can answer, because both must produce
    /// the same result: the running transfer's `Probed` event, and the
    /// speculative probe fired for the File Info dialog when no background
    /// download is running behind it.
    fn adopt_probed(&mut self, id: DlId, name: Option<String>, size: Option<u64>) -> Task<Message> {
        let mut adopted = false;
        if let Some(d) = self.item_mut(id) {
            // A probe answers once, so its size is adopted once — but a
            // stream has no probe. Its size is a running projection from the
            // segments that have landed, and it gets better as they do, so
            // for those the newer number always wins. Without this the bar
            // is drawn against the manifest's first bitrate guess for the
            // whole transfer and then jumps when the real size arrives.
            let refine = d.stream.is_some();
            if d.size.is_none() || (refine && size.is_some()) {
                d.size = size;
            }
            // Only adopt a server name if the user hasn't renamed.
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                if d.downloaded == 0 && d.held.is_empty() {
                    adopted = d.file_name != name;
                    d.file_name = name;
                }
            }
        }
        // The File Info dialog fills itself as the probe answers: real name
        // from Content-Disposition, category by extension, size via the item
        // it displays.
        if self.file_info.dl == id && self.win_of(WinKind::FileInfo(id)).is_some() {
            let probed_name = self.item(id).map(|d| d.file_name.clone());
            if let Some(name) = probed_name {
                if !self.file_info.name_touched {
                    self.file_info.file_name = name.clone();
                }
                if !self.file_info.cat_touched {
                    if let Some(cat) = categorize(&name, &self.cfg.categories) {
                        let dir = self.cat_dir(Some(&cat));
                        self.file_info.category = cat.clone();
                        if let (false, Some(dir)) = (self.file_info.dir_touched, dir.clone()) {
                            self.file_info.save_dir = dir;
                        }
                        let dir_untouched = !self.file_info.dir_touched;
                        if let Some(d) = self.item_mut(id) {
                            d.category = Some(cat);
                            if let (Some(dir), true) = (dir, dir_untouched) {
                                d.save_dir = dir;
                            }
                        }
                    }
                }
            }
        }
        // A transfer already under way was started under the name the URL
        // implied, and the probe has just found the real one. The engine
        // renames the `.part` into its final path when the transfer ends, so
        // that path has to be corrected too; without this the list showed
        // `setup.exe` while the disk still got `index.html`.
        if adopted {
            if let Some(path) = self.item(id).map(|d| d.full_path()) {
                engine::send(Cmd::SetFinalPath(id, path.to_string_lossy().into_owned()));
            }
        }
        self.save_state();
        Task::none()
    }

    fn on_engine(&mut self, ev: engine::Event) -> Task<Message> {
        match ev {
            engine::Event::Probed {
                id,
                size,
                ranges,
                file_name,
            } => {
                if let Some(d) = self.item_mut(id) {
                    d.resume = Some(ranges);
                    // The state stays Connecting: the probe answered, but no
                    // byte has arrived — the first Progress event promotes to
                    // Receiving. (Jumping early made the Status column show
                    // "0.00%" while the dialog still said Connecting.)
                }
                self.adopt_probed(id, file_name, size)
            }
            engine::Event::Status { id, line } => {
                if let Some(d) = self.item_mut(id) {
                    d.status_line = line;
                }
                Task::none()
            }
            engine::Event::Progress {
                id,
                done,
                rate,
                recorded,
                eta,
                conns,
                held,
            } => {
                let mut fresh = 0u64;
                if let Some(d) = self.item_mut(id) {
                    // `held` is authoritative for the byte total wherever it
                    // exists: after a repair the scheduler can shrink the
                    // held set below a previously-reported `done`, and
                    // storing the two independently made the bar and the
                    // chunk strip disagree from that point on.
                    let total = if held.is_empty() {
                        done
                    } else {
                        sum_spans(&held)
                    };
                    fresh = total.saturating_sub(d.downloaded);
                    d.downloaded = total;
                    d.rate = rate;
                    d.eta_secs = eta;
                    d.conns = conns;
                    d.held = held;
                    // Only a live recording reports one; carrying the last
                    // value forward would freeze the clock on a paused row.
                    d.recorded_secs = recorded;
                    if d.state == DlState::Connecting {
                        d.state = DlState::Receiving;
                    }
                }
                self.quota_account(fresh);
                // Enforced here rather than left to the one-second tick: at
                // 100 MB/s a tick of slack is 100 MB past the cap, and in
                // power-save mode the tick is three seconds apart.
                if self.quota_exhausted() {
                    return self.quota_park();
                }
                Task::none()
            }
            engine::Event::Finished { id, elapsed, size } => {
                crate::log::log(&format!("#{id} complete: {size} bytes in {elapsed:.1}s"));
                // A delete was pending but the transfer won the race and
                // finished: honour the delete anyway — the row must not
                // reappear as Complete after the user removed it.
                if self.pending_delete.iter().any(|(x, _)| *x == id) {
                    self.pending_delete.retain(|(x, _)| *x != id);
                    self.remove_item(id);
                    return Task::none();
                }
                let remember_limit = self
                    .prog
                    .get(&id)
                    .map(|p| p.remember_limit)
                    .unwrap_or(false);
                if let Some(d) = self.item_mut(id) {
                    if !remember_limit {
                        d.speed_limit = None;
                    }
                    d.state = DlState::Complete;
                    d.downloaded = size;
                    d.size = Some(size);
                    d.rate = 0.0;
                    d.disp_progress = 1.0;
                    d.eta_secs = None;
                    d.held = vec![(0, size)];
                    d.part_path = None;
                    // Finished files leave their queue: "Files in the queue"
                    // lists pending work, not history.
                    d.queue = None;
                    d.status_line = i18n::tr("Complete");
                }
                self.save_state();
                // The ask-box for THIS download is obsolete once the
                // background transfer lands: close it, the complete dialog
                // (below) takes over.
                let mut fi_close = Task::none();
                if self.file_info.is_new
                    && self.file_info.dl == id
                    && self.win_of(WinKind::FileInfo(id)).is_some()
                {
                    fi_close = self.close_window(WinKind::FileInfo(id));
                }
                // With a scanner configured the file goes to it first: the
                // progress dialog stays open showing the scan, and the chime
                // plus complete dialog wait for the verdict.
                if self.start_virus_scan(id) {
                    return Task::batch([fi_close, self.queue_tick()]);
                }
                let task = self.finish_completion(id);
                Task::batch([fi_close, task, self.queue_tick()])
            }
            engine::Event::Stopped { id, done, held } => {
                if self.pending_delete.iter().any(|(x, _)| *x == id) {
                    self.pending_delete.retain(|(x, _)| *x != id);
                    self.remove_item(id);
                    return Task::none();
                }
                let remember_limit = self
                    .prog
                    .get(&id)
                    .map(|p| p.remember_limit)
                    .unwrap_or(false);
                if let Some(d) = self.item_mut(id) {
                    let by_limit = d.limit_paused;
                    d.state = DlState::Paused;
                    // `held` authoritative, counter derived (see Progress).
                    if !held.is_empty() {
                        d.held = held;
                        d.downloaded = sum_spans(&d.held);
                    } else if done > 0 {
                        d.downloaded = done;
                    }
                    if !remember_limit {
                        d.speed_limit = None;
                    }
                    // Keep the last measured rate/ETA on screen:
                    // a paused dialog still shows what the transfer was doing.
                    d.status_line = if by_limit {
                        i18n::tr("Download limit reached")
                    } else {
                        i18n::tr("Pause")
                    };
                    for c in &mut d.conns {
                        c.info = i18n::tr("Disconnect.");
                    }
                }
                self.save_state();
                self.queue_tick()
            }
            engine::Event::Failed {
                id,
                error,
                done,
                held,
                permission_denied,
            } => {
                if self.pending_delete.iter().any(|(x, _)| *x == id) {
                    self.pending_delete.retain(|(x, _)| *x != id);
                    self.remove_item(id);
                    return Task::none();
                }
                // Scheduler retries: a failed file in a running queue with
                // "Number of retries" enabled goes back to Queued until its
                // budget is spent.
                if !permission_denied {
                    let retry_budget = self
                        .item(id)
                        .and_then(|d| d.queue.clone())
                        .and_then(|qn| self.cfg.queues.iter().find(|q| q.name == qn))
                        .filter(|q| q.running && q.schedule.retries_enabled)
                        .map(|q| q.schedule.retries);
                    if let Some(budget) = retry_budget {
                        if let Some(d) = self.item_mut(id) {
                            if d.retries < budget {
                                d.retries += 1;
                                d.state = DlState::Queued;
                                if !held.is_empty() {
                                    d.held = held.clone();
                                    d.downloaded = sum_spans(&d.held);
                                } else if done > 0 {
                                    d.downloaded = done;
                                }
                                crate::log::info(&format!(
                                    "#{id} retry {}/{budget}: {error}",
                                    d.retries
                                ));
                                self.save_state();
                                return self.queue_tick();
                            }
                        }
                    }
                }
                if permission_denied {
                    // OS-native consent instead of sudo or manual settings:
                    // a folder chosen through the system panel is implicitly
                    // granted, so offer the picker and resume right there.
                    let start_dir = self.item(id).map(|d| d.save_dir.clone());
                    let mut dlg = rfd::FileDialog::new();
                    if let Some(dir) = &start_dir {
                        dlg = dlg.set_directory(dir);
                    }
                    if let Some(picked) = dlg.pick_folder() {
                        let picked = picked.to_string_lossy().into_owned();
                        crate::log::info(&format!("#{id} folder re-granted: {picked}"));
                        if let Some(d) = self.item_mut(id) {
                            d.save_dir = picked;
                            d.part_path = None;
                            d.state = DlState::Paused;
                            if !held.is_empty() {
                                d.held = held.clone();
                            }
                        }
                        self.save_state();
                        return self.start_download(id, false);
                    }
                }
                if let Some(d) = self.item_mut(id) {
                    d.state = DlState::Error;
                    if !held.is_empty() {
                        d.held = held;
                        d.downloaded = sum_spans(&d.held);
                    } else if done > 0 {
                        d.downloaded = done;
                    }
                    d.error = Some(error.clone());
                    d.status_line = error;
                    for c in &mut d.conns {
                        c.info = i18n::tr("Disconnect.");
                    }
                }
                if let Some(row) = self.cfg.settings.sounds.get(1).filter(|r| r.enabled) {
                    sounds::play(
                        (!row.file.is_empty()).then(|| row.file.clone()),
                        sounds::Event::DownloadFailed,
                    );
                }
                self.save_state();
                self.queue_tick()
            }
        }
    }

    // -------------------------------------------------------------- update

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::WindowOpened(id) => {
                #[cfg(target_os = "macos")]
                let pin_surface = iced::window::run(id, |w| {
                    crate::macos_surface::pin_color_space(w);
                })
                .discard();
                #[cfg(not(target_os = "macos"))]
                let pin_surface = Task::none();
                #[cfg(target_os = "macos")]
                {
                    // Back to Regular before installing the menu: with the
                    // Dock hidden the app sat in Accessory, which has no
                    // menu bar to install into.
                    crate::macos_dock::sync(self.cfg.settings.hide_from_taskbar, true);
                    let state = crate::macos_menu::MenuState {
                        theme_mode: self.cfg.settings.theme(),
                        show_categories: self.cfg.settings.show_categories,
                        font_size: self.cfg.settings.font_size,
                        language: self.cfg.language.clone().unwrap_or_else(|| "en".into()),
                        speed_limiter: self.cfg.settings.speed_limiter_on,
                    };
                    let queues: Vec<String> =
                        self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                    crate::macos_menu::install(&state, &queues, &crate::i18n::available());
                }
                {
                    let queues: Vec<String> =
                        self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                    crate::tray::install(&queues, self.cfg.settings.power_save);
                }
                // "Hide from taskbar" is a per-window creation flag on
                // Windows; on X11 it is a hint the window manager only takes
                // once the window exists, so it is applied here instead.
                let skip_taskbar = self.skip_taskbar_task(id);
                // Browser-capture dialogs float above everything: at that
                // moment this app is in the background and a normal-level
                // window would open behind the browser.
                if self.capture_raise
                    && matches!(
                        self.windows.get(&id),
                        Some(WinKind::FileInfo(_) | WinKind::Confirm | WinKind::Batch)
                    )
                {
                    self.capture_raise = false;
                    return Task::batch([
                        pin_surface,
                        skip_taskbar,
                        window::set_level(id, window::Level::AlwaysOnTop),
                        window::gain_focus(id),
                    ]);
                }
                Task::batch([pin_surface, skip_taskbar, window::gain_focus(id)])
            }
            Message::WindowClosed(id) => {
                let kind = self.windows.remove(&id);
                // Last window gone + hide-Dock on: drop to Accessory now
                // that no menu bar is needed (tray-only from here).
                #[cfg(target_os = "macos")]
                crate::macos_dock::sync(
                    self.cfg.settings.hide_from_taskbar,
                    !self.windows.is_empty(),
                );
                match kind {
                    Some(WinKind::Main) => {
                        self.save_state();
                        self.save_config();
                        self.flush_saves();
                        if self.cfg.settings.close_to_tray && crate::tray::is_active() {
                            // Hydra lives on in the system tray; Exit is in
                            // the tray menu (and the app menu on macOS).
                            // Without a tray there would be no way back to
                            // the app, so closing quits instead — and so it
                            // does when Options > General turns close-to-tray
                            // off, which asks for a plain quit.
                            self.main_id = None;
                            Task::none()
                        } else {
                            iced::exit()
                        }
                    }
                    Some(WinKind::Confirm) => {
                        self.confirm = None;
                        // A duplicate decision dismissed with the OS close
                        // button must not leave its URL parked: the next
                        // "Download as new file" would add the stale one.
                        self.pending_add = None;
                        Task::none()
                    }
                    Some(WinKind::FileInfo(dl)) => {
                        // OS close button on the new-download dialog is
                        // Cancel: drop the auto-started item.
                        if self.file_info.is_new && self.file_info.dl == dl {
                            self.delete_item(dl)
                        } else {
                            Task::none()
                        }
                    }
                    Some(WinKind::Update) => {
                        // OS close button is Cancel: stop an in-flight
                        // download and forget the offer (unless the finisher
                        // is already live — then the exit is imminent).
                        if self.updater.phase != UpdatePhase::Restarting {
                            if let Some(c) = &self.updater.cancel {
                                c.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            self.updater = UpdateUiState::default();
                        }
                        Task::none()
                    }
                    _ => Task::none(),
                }
            }
            Message::WindowCloseRequested(id) => {
                // `exit_on_close_request: false` means the close button only
                // ASKS; without this handler every red button was dead.
                // Always honour it, tracked or not: a window we have lost
                // track of must still be closable by its own close button.
                // Bookkeeping happens in WindowClosed once it's gone.
                window::close(id)
            }
            // Window geometry reaches us divided by the View > Font scale
            // factor, because that is the space the interface is laid out
            // in; `window::open` and `window::resize` speak OS points. Undo
            // the ratio here so everything stored is in OS points and the
            // two agree at any font size.
            Message::WinMoved(id, p) => {
                if self.main_id == Some(id) {
                    let s = self.ui_scale();
                    self.main_pos = Some(Point::new(p.x * s, p.y * s));
                }
                Task::none()
            }
            Message::WinResized(id, size) => {
                if self.main_id == Some(id) {
                    let s = self.ui_scale();
                    self.main_size = iced::Size::new(size.width * s, size.height * s);
                    // Remember it (written with the next config save).
                    if size.width >= 900.0 && size.height >= 600.0 {
                        self.cfg.settings.window_size =
                            Some((self.main_size.width, self.main_size.height));
                    }
                }
                Task::none()
            }
            Message::Engine(ev) => self.on_engine(ev),
            Message::Tick => {
                if self.win_of(WinKind::Permissions).is_some() {
                    self.refresh_perm_status();
                }
                self.sweep_pending_deletes();
                self.schedule_tick();
                let quota = self.quota_tick();
                let queue = Task::batch([quota, self.queue_tick()]);
                self.flush_saves();
                if self.cfg.settings.monitor_clipboard {
                    Task::batch([queue, iced::clipboard::read().map(Message::ClipboardSeen)])
                } else {
                    queue
                }
            }
            Message::ClipboardSeen(text) => {
                let Some(text) = text else {
                    return Task::none();
                };
                let text = text.trim().to_string();
                if text.is_empty() || text == self.last_clipboard {
                    return Task::none();
                }
                self.last_clipboard = text.clone();
                let urls: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| looks_downloadable(l, &self.cfg.settings.auto_types))
                    .filter(|l| !site_blocked(l, &self.cfg.settings.dont_start_sites))
                    .filter(|l| !self.state.downloads.iter().any(|d| d.url == *l))
                    .map(str::to_string)
                    .collect();
                match urls.len() {
                    0 => Task::none(),
                    1 => {
                        crate::log::info(&format!("clipboard capture: {}", urls[0]));
                        self.add_url = AddUrlState {
                            address: urls[0].clone(),
                            ..AddUrlState::default()
                        };
                        self.update(Message::AddUrlOk)
                    }
                    _ => {
                        // Many links: the "Download All Links" box.
                        crate::log::info(&format!("clipboard capture: {} links", urls.len()));
                        self.batch = BatchState::default();
                        self.batch.category = "General".into();
                        let open = self.open_window(WinKind::Batch);
                        let text = urls.join("\n");
                        Task::batch([open, self.update(Message::BatchLoaded(Some(text)))])
                    }
                }
            }
            Message::AnimTick => {
                // The indeterminate scan bar: one sweep every ~1.6 s, folded
                // at 2.0 so the block returns instead of snapping back.
                for st in self.scans.values_mut().filter(|s| s.running()) {
                    st.phase = (st.phase + 0.05) % 2.0;
                }
                for d in &mut self.state.downloads {
                    let target = d.progress();
                    let diff = target - d.disp_progress;
                    if diff == 0.0 {
                        // Idle rows (history, paused items already at their
                        // fraction) must not be touched 12.5 times a second.
                        continue;
                    }
                    if !(0.0..=0.2).contains(&diff) {
                        // Backwards (redownload) or a jump the glide would
                        // turn into a long crawl: snap.
                        d.disp_progress = target;
                    } else if diff > 0.0005 {
                        d.disp_progress += diff * 0.35;
                    } else {
                        d.disp_progress = target;
                    }
                }
                Task::none()
            }
            Message::NativeMenu(id) => {
                if id == "show_main" {
                    return self.open_window(WinKind::Main);
                }
                match MenuAction::from_id(&id) {
                    Some(a) => self.update(Message::Menu(a)),
                    None => Task::none(),
                }
            }
            Message::SystemTheme(mode) => {
                // `Mode::None` is "the platform did not say" — iced answers
                // it until a window exists, and it must not overwrite the
                // appearance read at startup.
                match mode {
                    iced::theme::Mode::Light => self.system_dark = false,
                    iced::theme::Mode::Dark => self.system_dark = true,
                    iced::theme::Mode::None => {}
                }
                Task::none()
            }
            Message::MenuOpen(kind) => {
                self.open_menu = if self.open_menu == Some(kind) {
                    None
                } else {
                    Some(kind)
                };
                self.open_submenu = None;
                Task::none()
            }
            Message::MenuHover(kind) => {
                if self.open_menu != Some(kind) {
                    self.open_menu = Some(kind);
                    self.open_submenu = None;
                }
                Task::none()
            }
            Message::MenuClose => {
                self.open_menu = None;
                self.open_submenu = None;
                self.ctx_at = None;
                self.queue_menu = None;
                Task::none()
            }
            Message::SubmenuHover(i) => {
                self.open_submenu = i;
                Task::none()
            }
            Message::Menu(action) => {
                self.open_menu = None;
                self.open_submenu = None;
                self.ctx_at = None;
                self.queue_menu = None;
                self.on_menu(action)
            }
            Message::TreeSelect(sel) => {
                self.tree_sel = sel.clone();
                self.renaming_queue = None;
                let queue = match &sel {
                    TreeSel::Queue(name) => Some(name.clone()),
                    _ => None,
                };
                if self.queue_click(queue.as_deref()) {
                    // Second click on the same queue: rename it in place.
                    // Stock queues are refused by the handler.
                    return self.update(Message::TreeQueueRenameStart(queue.unwrap()));
                }
                Task::none()
            }
            Message::TreeQueueRenameStart(name) => {
                let builtin = self
                    .cfg
                    .queues
                    .iter()
                    .find(|q| q.name == name)
                    .map(|q| q.builtin)
                    .unwrap_or(true);
                if builtin {
                    return Task::none();
                }
                self.queue_rename_draft = name.clone();
                self.renaming_queue = Some(name);
                iced::widget::operation::focus("tree-queue-rename")
            }
            Message::TreeQueueRenameDraft(v) => {
                self.queue_rename_draft = v;
                Task::none()
            }
            Message::TreeQueueRenameCommit => {
                if let Some(old) = self.renaming_queue.take() {
                    let new = self.queue_rename_draft.clone();
                    // The sidebar addresses a queue by name, so a rename
                    // that landed has to carry the selection over with it.
                    if self.rename_queue(&old, &new) && self.tree_sel == TreeSel::Queue(old) {
                        self.tree_sel = TreeSel::Queue(new);
                    }
                }
                Task::none()
            }
            Message::TreeToggle(i) => {
                if let Some(f) = self.tree_open.get_mut(i as usize) {
                    *f = !*f;
                }
                Task::none()
            }
            Message::RowClick(id) => {
                let at = self.cursor_now();
                self.cursor = at;
                self.band = Some((at, at));
                if self.mods.command() || self.mods.control() {
                    // Toggle membership.
                    if self.selected.contains(&id) {
                        self.selected.retain(|x| *x != id);
                    } else {
                        self.selected.push(id);
                    }
                    self.sel_anchor = Some(id);
                    self.last_click = None;
                    return Task::none();
                }
                if self.mods.shift() {
                    // Range from the anchor within the visible ordering.
                    let anchor = self.sel_anchor.unwrap_or(id);
                    let order: Vec<DlId> = self.visible_ids();
                    let a = order.iter().position(|x| *x == anchor);
                    let b = order.iter().position(|x| *x == id);
                    if let (Some(a), Some(b)) = (a, b) {
                        let (lo, hi) = (a.min(b), a.max(b));
                        self.selected = order[lo..=hi].to_vec();
                    }
                    self.last_click = None;
                    return Task::none();
                }
                let double = self
                    .last_click
                    .map(|(last, at)| last == id && at.elapsed().as_millis() < 400)
                    .unwrap_or(false);
                self.last_click = Some((id, Instant::now()));
                self.selected = vec![id];
                self.sel_anchor = Some(id);
                self.list_press = Some(id);
                self.list_drag = true;
                self.list_drag_from_empty = false;
                self.drag_order = self.visible_ids();
                if double {
                    let st = self.item(id).map(|d| d.state);
                    match st {
                        // Active transfer: its progress box. Anything else:
                        // the File Properties dialog.
                        Some(s) if s.is_active() => self.open_window(WinKind::Progress(id)),
                        Some(_) => self.update(Message::Menu(MenuAction::Properties)),
                        None => Task::none(),
                    }
                } else {
                    Task::none()
                }
            }
            Message::RowRightClick(id) => {
                // Right-click keeps an existing multi-selection if the row is
                // part of it.
                if !self.selected.contains(&id) {
                    self.selected = vec![id];
                    self.sel_anchor = Some(id);
                }
                self.ctx_at = Some(self.cursor_now());
                Task::none()
            }
            Message::TableScrolled(offset, viewport_h) => {
                self.table_scroll = offset;
                self.table_vh = viewport_h;
                Task::none()
            }
            Message::DragTick => {
                let p = self.cursor_now();
                self.cursor = p;
                if self.list_drag {
                    if let Some(band) = self.band.as_mut() {
                        band.1 = p;
                    }
                }
                if let Some((col, grab_x, start_w)) = self.resizing {
                    if let Some(w) = self.col_widths.get_mut(col) {
                        *w = (start_w + (p.x - grab_x)).clamp(40.0, 800.0);
                    }
                }
                Task::none()
            }
            Message::Mods(m) => {
                self.mods = m;
                Task::none()
            }
            Message::RawKey(key, mods) => {
                // Escape backs out of an inline rename, leaving the queue
                // under its old name — the draft is only committed on Enter.
                if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                    && self.renaming_queue.is_some()
                {
                    self.renaming_queue = None;
                    return Task::none();
                }
                let combo = combo_string(&key, mods);
                let Some(combo) = combo else {
                    return Task::none();
                };
                if combo == "cmd+a" {
                    return self.update(Message::SelectAll);
                }
                let action = self
                    .cfg
                    .shortcuts
                    .iter()
                    .find(|(_, c)| **c == combo)
                    .map(|(id, _)| id.clone());
                match action.as_deref() {
                    Some("add_url") => self.update(Message::Menu(MenuAction::AddNewDownload)),
                    Some("clipboard_add") => {
                        iced::clipboard::read().map(Message::ClipboardAddStart)
                    }
                    Some("options") => self.update(Message::Menu(MenuAction::Options)),
                    Some("scheduler") => self.update(Message::Menu(MenuAction::Scheduler)),
                    Some("resume_last") => {
                        let id = self
                            .state
                            .downloads
                            .iter()
                            .filter(|d| {
                                matches!(
                                    d.state,
                                    DlState::Paused | DlState::Error | DlState::Queued
                                )
                            })
                            .max_by_key(|d| d.added)
                            .map(|d| d.id);
                        match id {
                            Some(id) => self.start_download(id, true),
                            None => Task::none(),
                        }
                    }
                    Some("stop_last") => {
                        let id = self
                            .state
                            .downloads
                            .iter()
                            .filter(|d| d.state.is_active())
                            .max_by_key(|d| d.added)
                            .map(|d| d.id);
                        if let Some(id) = id {
                            self.stop_download(id);
                        }
                        Task::none()
                    }
                    Some("start_main_queue") => {
                        let q = self.cfg.queues.first().map(|q| q.name.clone());
                        match q {
                            Some(q) => self.update(Message::Menu(MenuAction::StartQueue(q))),
                            None => Task::none(),
                        }
                    }
                    Some("stop_main_queue") => {
                        let q = self.cfg.queues.first().map(|q| q.name.clone());
                        match q {
                            Some(q) => self.update(Message::Menu(MenuAction::StopQueue(q))),
                            None => Task::none(),
                        }
                    }
                    _ => Task::none(),
                }
            }
            Message::SelectAll => {
                self.selected = self.visible_ids();
                self.sel_anchor = self.selected.first().copied();
                Task::none()
            }
            Message::RowEnter(id) => {
                self.hover_row = Some(id);
                // Rubber-band selection: the button went down in the list and
                // the sweep arrived here still holding it.
                if !self.list_drag {
                    return Task::none();
                }
                let anchor = if self.list_drag_from_empty {
                    // Band pinned to the end of the list: the press was below
                    // every row.
                    match self.drag_order.last() {
                        Some(last) => *last,
                        None => return Task::none(),
                    }
                } else {
                    let a = self.list_press.unwrap_or(id);
                    self.list_press = Some(a);
                    a
                };
                let a = self.drag_order.iter().position(|x| *x == anchor);
                let b = self.drag_order.iter().position(|x| *x == id);
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = (a.min(b), a.max(b));
                    self.selected = self.drag_order[lo..=hi].to_vec();
                    self.sel_anchor = Some(anchor);
                }
                Task::none()
            }
            Message::EmptyPress => {
                // Pressing the empty ruled area arms a sweep pinned to the end
                // of the list and drops the selection, the way a listview does.
                self.list_drag = true;
                self.list_drag_from_empty = true;
                self.list_press = None;
                self.drag_order = self.visible_ids();
                let at = self.cursor_now();
                self.cursor = at;
                self.band = Some((at, at));
                self.selected.clear();
                self.sel_anchor = None;
                self.last_click = None;
                Task::none()
            }
            Message::EmptyEnter => {
                self.hover_row = None;
                if !self.list_drag {
                    return Task::none();
                }
                if self.list_drag_from_empty {
                    // Both ends of the band are below the last row: it covers
                    // nothing.
                    self.selected.clear();
                    self.sel_anchor = None;
                    return Task::none();
                }
                // Swept off the bottom of the data: the band reaches the end.
                let Some(anchor) = self.list_press else {
                    return Task::none();
                };
                if let Some(a) = self.drag_order.iter().position(|x| *x == anchor) {
                    self.selected = self.drag_order[a..].to_vec();
                    self.sel_anchor = Some(anchor);
                }
                Task::none()
            }
            Message::RowExit(id) => {
                if self.hover_row == Some(id) {
                    self.hover_row = None;
                }
                Task::none()
            }
            Message::HeaderEnter(col) => {
                self.hover_col = Some(col);
                Task::none()
            }
            Message::HeaderExit(col) => {
                if self.hover_col == Some(col) {
                    self.hover_col = None;
                }
                Task::none()
            }
            Message::QueueMenuOpen(start) => {
                self.queue_menu = Some(start);
                self.ctx_at = Some(self.cursor_now());
                Task::none()
            }
            Message::ClipboardAddStart(text) => {
                let Some(text) = text else {
                    return Task::none();
                };
                let url = text.lines().map(str::trim).find(|l| {
                    looks_downloadable(l, &self.cfg.settings.auto_types)
                        && !site_blocked(l, &self.cfg.settings.dont_start_sites)
                });
                let Some(url) = url else { return Task::none() };
                if self.state.downloads.iter().any(|d| d.url == url) {
                    return Task::none();
                }
                let id = self.add_item(url.to_string(), None, None);
                self.start_download(id, true)
            }
            Message::ShortcutEdit(action, combo) => {
                self.cfg
                    .shortcuts
                    .insert(action, combo.trim().to_ascii_lowercase());
                self.save_config();
                Task::none()
            }
            Message::MouseUp => {
                if self.resizing.take().is_some() {
                    self.cfg.settings.column_widths = self.col_widths.clone();
                    self.save_config();
                }
                if self.sch.drag.take().is_some() {
                    self.save_state();
                }
                self.list_press = None;
                self.list_drag = false;
                self.list_drag_from_empty = false;
                self.drag_order = Vec::new();
                self.band = None;
                Task::none()
            }
            Message::ColResizeStart(col) => {
                if let Some(w) = self.col_widths.get(col) {
                    self.resizing = Some((col, self.cursor_now().x, *w));
                }
                Task::none()
            }
            Message::SortBy(key) => {
                if self.sort.0 == key {
                    self.sort.1 = !self.sort.1;
                } else {
                    self.sort = (key, true);
                }
                Task::none()
            }
            Message::ToolbarResume => {
                let ids: Vec<DlId> = self
                    .selected
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.item(*id)
                            .map(|d| {
                                matches!(
                                    d.state,
                                    DlState::Paused | DlState::Error | DlState::Queued
                                )
                            })
                            .unwrap_or(false)
                    })
                    .collect();
                // A single resume shows its progress window; a batch resume
                // would bury the screen in dialogs.
                let show_progress = ids.len() == 1;
                let tasks: Vec<Task<Message>> = ids
                    .into_iter()
                    .map(|id| self.start_download(id, show_progress))
                    .collect();
                Task::batch(tasks)
            }
            Message::ToolbarStop => self.stop_ids_confirming(self.selected.clone(), false),
            Message::ToolbarDelete => {
                if !self.selected.is_empty() {
                    self.confirm = Some(ConfirmKind::DeleteItems(self.selected.clone()));
                    self.confirm_remove_file = false;
                    self.open_window(WinKind::Confirm)
                } else {
                    Task::none()
                }
            }

            // ------------------------------------------------------ add url
            Message::AddrPrefill(text) => {
                if self.add_url.address.is_empty() {
                    if let Some(t) = text {
                        let t = t.trim();
                        if !t.contains('\n') && looks_downloadable(t, &self.cfg.settings.auto_types)
                        {
                            self.add_url.address = t.to_string();
                        }
                    }
                }
                Task::none()
            }
            Message::AddrChanged(s) => {
                self.add_url.address = s;
                self.add_url.error = None;
                // An edited address no longer describes the manifest we
                // read; showing its quality list would be a lie.
                let addr = self.add_url.address.trim().to_string();
                if self.add_url.stream_of != addr {
                    self.add_url.stream = None;
                    self.add_url.stream_error = None;
                    self.add_url.quality = None;
                }
                let resize = self.resize_open(WinKind::AddUrl);
                // Automatic: a manifest address inspects itself, so the user
                // is choosing a quality rather than discovering afterwards
                // that they could have.
                if manifest_address(&addr)
                    && !self.add_url.stream_probing
                    && self.add_url.stream_of != addr
                {
                    return Task::batch([resize, self.update(Message::AddrProbeStream)]);
                }
                resize
            }
            Message::AddrProbeStream => {
                let url = self.add_url.address.trim().to_string();
                if url.is_empty() || self.add_url.stream_probing {
                    return Task::none();
                }
                self.add_url.stream_probing = true;
                self.add_url.stream_error = None;
                self.add_url.stream_of = url.clone();
                let ua = self.cfg.settings.user_agent.clone();
                let cookies = self.add_url.capture_cookies.clone();
                Task::perform(crate::engine::probe_stream(url, ua, cookies), |r| {
                    Message::AddrStreamProbed(Box::new(r))
                })
            }
            Message::AddrStreamProbed(result) => {
                self.add_url.stream_probing = false;
                match *result {
                    Ok(probe) => {
                        // Default to the best rendition, which is what
                        // someone who does not touch the picker means.
                        self.add_url.quality = probe.qualities.first().cloned();
                        // MPEG-TS can be saved as-is; fragmented MP4 cannot,
                        // so do not offer a container that needs ffmpeg the
                        // user may not have.
                        self.add_url.container = "MP4".into();
                        self.add_url.stream = Some(probe);
                    }
                    Err(e) => {
                        // Not a manifest after all: fall back silently to an
                        // ordinary download rather than blocking the dialog.
                        self.add_url.stream = None;
                        self.add_url.stream_error = Some(e);
                    }
                }
                self.resize_open(WinKind::AddUrl)
            }
            Message::AddrQuality(q) => {
                self.add_url.quality = Some(q);
                Task::none()
            }
            Message::AddrContainer(c) => {
                self.add_url.container = c;
                Task::none()
            }
            Message::AddrRecordMinutes(v) => {
                // Digits only: this becomes a duration, and a half-typed
                // number should not silently mean something else.
                self.add_url.record_minutes =
                    v.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
                Task::none()
            }
            Message::AddrAuthToggled(b) => {
                self.add_url.use_auth = b;
                self.resize_open(WinKind::AddUrl)
            }
            Message::AddrLogin(s) => {
                self.add_url.login = s;
                Task::none()
            }
            Message::AddrPass(s) => {
                self.add_url.password = s;
                Task::none()
            }
            Message::AddUrlOk => {
                let url = self.add_url.address.trim().to_string();
                if let Err(e) = engine::parse_url(&url) {
                    self.add_url.error = Some(e);
                    return self.resize_open(WinKind::AddUrl);
                }
                let auth = self
                    .add_url
                    .use_auth
                    .then(|| (self.add_url.login.clone(), self.add_url.password.clone()));
                // Same URL already listed, or the target file already on
                // disk? Ask instead of silently downloading twice.
                let existing = self
                    .state
                    .downloads
                    .iter()
                    .find(|d| d.url == url)
                    .map(|d| d.id);
                let cap_cookies = self
                    .add_url
                    .capture_cookies
                    .take()
                    .filter(|s| !s.is_empty());
                let cap_name = self.add_url.capture_name.take().filter(|s| !s.is_empty());
                // A manifest that was inspected becomes a STREAM item: the
                // chosen rendition and container decide the filename, which
                // the URL path cannot.
                let stream = self
                    .add_url
                    .stream
                    .clone()
                    .filter(|p| p.drm.is_none())
                    .map(|p| {
                        let q = self.add_url.quality.clone();
                        let container = if self.add_url.container.eq_ignore_ascii_case("ts") {
                            "TS"
                        } else {
                            "MP4"
                        };
                        let minutes: u64 = self.add_url.record_minutes.parse().unwrap_or(0);
                        crate::model::StreamInfo {
                            protocol: p.protocol.clone(),
                            variant_url: q.as_ref().and_then(|q| q.url.clone()),
                            height: q.as_ref().and_then(|q| q.height),
                            bandwidth: q.as_ref().and_then(|q| q.bandwidth),
                            container: container.into(),
                            referer: None,
                            user_agent: None,
                            live: p.live,
                            max_seconds: (p.live && minutes > 0).then(|| minutes * 60),
                        }
                    });
                let name = match &stream {
                    Some(si) => {
                        let ext = if si.container.eq_ignore_ascii_case("ts") {
                            "ts"
                        } else {
                            "mp4"
                        };
                        format!("{}.{ext}", stream_base_name(&url))
                    }
                    None => cap_name
                        .clone()
                        .unwrap_or_else(|| engine::file_name_from_url(&url)),
                };
                let cat = crate::model::categorize(&name, &self.cfg.categories);
                let dir = self
                    .cat_dir(cat.as_deref())
                    .or_else(|| self.cat_dir(None))
                    .unwrap_or_default();
                let on_disk = std::path::Path::new(&dir).join(&name);
                let file = on_disk
                    .exists()
                    .then(|| on_disk.to_string_lossy().into_owned());
                if existing.is_some() || file.is_some() {
                    self.pending_add = Some(PendingAdd {
                        url,
                        auth,
                        cookies: cap_cookies,
                        name: cap_name,
                    });
                    self.confirm = Some(ConfirmKind::Duplicate { existing, file });
                    self.add_url = AddUrlState::default();
                    let close = self.close_window(WinKind::AddUrl);
                    return Task::batch([close, self.open_window(WinKind::Confirm)]);
                }
                let id = self.add_item(url, auth, None);
                self.apply_capture_extras(id, cap_cookies, cap_name);
                if let Some(si) = stream {
                    let cat = crate::model::categorize(&name, &self.cfg.categories);
                    let dir = self
                        .cat_dir(cat.as_deref())
                        .or_else(|| self.cat_dir(None))
                        .unwrap_or_default();
                    if let Some(d) = self.item_mut(id) {
                        d.file_name = name.clone();
                        d.category = cat;
                        d.save_dir = dir;
                        // Resumable by whole segments, not byte ranges.
                        d.resume = Some(true);
                        d.stream = Some(si);
                    }
                }
                self.add_url = AddUrlState::default();
                let close = self.close_window(WinKind::AddUrl);
                if self.cfg.settings.show_file_info_dialog {
                    let d = self.item(id).unwrap();
                    self.file_info = FileInfoState {
                        dl: id,
                        category: d.category.clone().unwrap_or_else(|| "General".into()),
                        save_dir: d.save_dir.clone(),
                        file_name: d.file_name.clone(),
                        description: String::new(),
                        remember: true,
                        is_new: true,
                        url: d.url.clone(),
                        login: d.auth.clone().map(|a| a.0).unwrap_or_default(),
                        password: d.auth.clone().map(|a| a.1).unwrap_or_default(),
                        cookies: d.cookies.clone().unwrap_or_default(),
                        bg_blocked: !self.auto_start_type(id),
                        ..FileInfoState::default()
                    };
                    let bg = self.file_info_prefetch(id);
                    Task::batch([close, self.open_window(WinKind::FileInfo(id)), bg])
                } else if self
                    .item(id)
                    .map(|d| site_blocked(&d.url, &self.cfg.settings.dont_start_sites))
                    .unwrap_or(false)
                {
                    // Blocked site, no File Info dialog to surface the block
                    // through: the item is added, queued, but left for the
                    // user to start by hand.
                    close
                } else {
                    Task::batch([close, self.start_download(id, true)])
                }
            }

            // ------------------------------------------- browser extension
            Message::Ext(ev) => match ev {
                crate::extbus::ExtEvent::Download(dl) => {
                    // Same flow as a manual Add URL, so duplicate detection,
                    // categorization, and the File Info dialog all behave
                    // consistently for browser capture.
                    crate::log::info(&format!("ext: capture dialog for {}", dl.url));
                    self.capture_raise = true;
                    self.add_url = AddUrlState {
                        address: dl.url,
                        capture_cookies: dl.cookies,
                        capture_name: dl.filename,
                        ..AddUrlState::default()
                    };
                    self.update(Message::AddUrlOk)
                }
                crate::extbus::ExtEvent::Stream(s) => {
                    // Not the capture dialog: a manifest cannot be probed
                    // for a size or a name, so the entry is built from what
                    // the extension already read out of it and started.
                    crate::log::info(&format!("ext: stream capture for {}", s.url));
                    self.capture_raise = true;
                    let container = s.container.clone().unwrap_or_else(|| "MP4".into());
                    let ext = if container.eq_ignore_ascii_case("ts") {
                        "ts"
                    } else {
                        "mp4"
                    };
                    let base = s
                        .filename
                        .as_deref()
                        .map(sanitize_file_name)
                        .filter(|b| !b.is_empty())
                        .unwrap_or_else(|| "stream".to_string());
                    let file_name = format!("{base}.{ext}");
                    // The URL says `.m3u8`; the FILE is a video, so the
                    // category has to be decided from the name we chose.
                    let category = categorize(&file_name, &self.cfg.categories);
                    let save_dir = self
                        .cat_dir(category.as_deref())
                        .or_else(|| self.cat_dir(None))
                        .unwrap_or_else(|| ".".into());
                    let id = self.add_item(s.url.clone(), None, None);
                    if let Some(d) = self.item_mut(id) {
                        d.file_name = file_name;
                        d.category = category;
                        d.save_dir = save_dir;
                        d.cookies = s.cookies.clone();
                        d.size = s.size;
                        // Resumable, but by whole segments rather than byte
                        // ranges: a paused stream carries on from the last
                        // segment that landed.
                        d.resume = Some(true);
                        d.stream = Some(crate::model::StreamInfo {
                            protocol: s.protocol.clone().unwrap_or_else(|| "hls".into()),
                            variant_url: s
                                .variant_url
                                .clone()
                                .or_else(|| s.variant.as_ref().and_then(|v| v.url.clone())),
                            height: s.variant.as_ref().and_then(|v| v.height),
                            bandwidth: s.variant.as_ref().and_then(|v| v.bandwidth),
                            container,
                            referer: s.referer.clone().or_else(|| s.tab_url.clone()),
                            user_agent: s.user_agent.clone(),
                            live: s.live,
                            max_seconds: None,
                        });
                    }
                    self.start_download(id, true)
                }
                crate::extbus::ExtEvent::Links(urls) => {
                    self.capture_raise = true;
                    // "Download all links": the batch window, like a
                    // multi-line clipboard capture.
                    self.batch = BatchState::default();
                    self.batch.category = "General".into();
                    let open = self.open_window(WinKind::Batch);
                    let text = urls.join("\n");
                    Task::batch([open, self.update(Message::BatchLoaded(Some(text)))])
                }
                crate::extbus::ExtEvent::Open => self.open_window(WinKind::Main),
                crate::extbus::ExtEvent::Shutdown => {
                    // A newer build is taking over the single-instance slot
                    // (extbus::signal_existing): leave the way tray Exit does.
                    crate::log::info("ext: shutdown requested by a newer build; exiting");
                    self.save_state();
                    self.save_config();
                    self.flush_saves();
                    iced::exit()
                }
            },

            // ---------------------------------------------------- file info
            Message::FiCategory(c) => {
                if let Some(dir) = self.cat_dir(Some(&c)) {
                    if !self.file_info.dir_touched {
                        self.file_info.save_dir = dir;
                    }
                }
                self.file_info.category = c;
                self.file_info.cat_touched = true;
                Task::none()
            }
            Message::FiFileName(s) => {
                self.file_info.file_name = s;
                self.file_info.name_touched = true;
                Task::none()
            }
            Message::FiSaveDir(s) => {
                self.file_info.save_dir = s;
                self.file_info.dir_touched = true;
                Task::none()
            }
            Message::FiBgToggle(b) => {
                self.cfg.settings.bg_download = b;
                self.save_config();
                let dl = self.file_info.dl;
                if self.file_info.is_new {
                    // Ticking the box by hand is the user overruling the
                    // file-type gate for this one download.
                    self.file_info.bg_blocked = false;
                    let active = self.item(dl).map(|d| d.state.is_active()).unwrap_or(false);
                    if b && !active {
                        return self.start_download(dl, false);
                    }
                    if !b && active {
                        self.stop_download(dl);
                    }
                }
                Task::none()
            }
            Message::FiBrowseDir => {
                let dir = rfd::FileDialog::new()
                    .pick_folder()
                    .map(|p| p.to_string_lossy().into_owned());
                self.update(Message::FiDirPicked(dir))
            }
            Message::FiDirPicked(Some(dir)) => {
                self.file_info.save_dir = dir;
                self.file_info.dir_touched = true;
                Task::none()
            }
            Message::FiDirPicked(None) => Task::none(),
            Message::FiDescription(s) => {
                self.file_info.description = s;
                Task::none()
            }
            Message::FiRemember(b) => {
                self.file_info.remember = b;
                Task::none()
            }
            Message::FiUrl(v) => {
                self.file_info.url = v;
                Task::none()
            }
            Message::FiLogin(v) => {
                self.file_info.login = v;
                Task::none()
            }
            Message::FiPass(v) => {
                self.file_info.password = v;
                Task::none()
            }
            Message::FiCookies(v) => {
                self.file_info.cookies = v;
                Task::none()
            }
            Message::FiDownloadLater | Message::FiStartDownload => {
                let start = matches!(message, Message::FiStartDownload);
                let fi = self.file_info.clone();
                if fi.remember {
                    // "Remember this folder" writes where the folder is
                    // actually read from: the named category normally, and
                    // the General one while category folders are switched
                    // off — otherwise the tick would silently do nothing.
                    let target = if self.cfg.settings.no_category_dirs {
                        self.cfg.categories.first().map(|c| c.name.clone())
                    } else {
                        Some(fi.category.clone())
                    };
                    if let Some(cat) =
                        target.and_then(|n| self.cfg.categories.iter_mut().find(|k| k.name == n))
                    {
                        cat.dir = fi.save_dir.clone();
                        self.save_config();
                    }
                }
                let old_path = self.item(fi.dl).map(|d| d.full_path());
                let mut was = None;
                let url_changed = self
                    .item(fi.dl)
                    .map(|d| !fi.url.trim().is_empty() && d.url != fi.url.trim())
                    .unwrap_or(false)
                    && engine::parse_url(fi.url.trim()).is_ok();
                if url_changed {
                    // Mirror switch: keep the held byte spans — the engine
                    // verifies the new source reports the same size and
                    // continues where mirror A stopped.
                    crate::log::info(&format!("#{} mirror -> {}", fi.dl, fi.url.trim()));
                    if self
                        .item(fi.dl)
                        .map(|d| d.state.is_active())
                        .unwrap_or(false)
                    {
                        engine::send(Cmd::Stop(fi.dl));
                    }
                }
                if let Some(d) = self.item_mut(fi.dl) {
                    d.category = Some(fi.category.clone());
                    d.save_dir = fi.save_dir.clone();
                    if !fi.file_name.trim().is_empty() {
                        d.file_name = fi.file_name.trim().to_string();
                    }
                    d.description = fi.description.clone();
                    if url_changed {
                        d.url = fi.url.trim().to_string();
                        d.state = DlState::Paused;
                        d.error = None;
                    }
                    d.auth =
                        (!fi.login.is_empty()).then(|| (fi.login.clone(), fi.password.clone()));
                    d.cookies =
                        (!fi.cookies.trim().is_empty()).then(|| fi.cookies.trim().to_string());
                    was = Some(d.state);
                }
                // Aim the running transfer at the edited destination; if a
                // small file already finished under the provisional name,
                // move it.
                if let Some(d) = self.item(fi.dl) {
                    let new_path = d.full_path();
                    engine::send(Cmd::SetFinalPath(
                        fi.dl,
                        new_path.to_string_lossy().into_owned(),
                    ));
                    if was == Some(DlState::Complete) {
                        if let Some(old) = old_path.filter(|o| *o != new_path) {
                            if let Some(dir) = new_path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            let _ = std::fs::rename(old, &new_path);
                        }
                    }
                }
                self.save_state();
                let close = self.close_file_info_windows();
                match (start, was) {
                    // Reveal the already-running background transfer.
                    (true, Some(st)) if st.is_active() => {
                        let seed = self.item(fi.dl).and_then(|d| d.speed_limit);
                        self.prog
                            .entry(fi.dl)
                            .or_insert_with(|| prog_state_seed(seed));
                        Task::batch([close, self.open_window(WinKind::Progress(fi.dl))])
                    }
                    (true, _) => Task::batch([close, self.start_download(fi.dl, true)]),
                    // Download Later: park the background transfer, keep the
                    // bytes for resume, and leave it Queued for Start Queue.
                    (false, Some(st)) if st.is_active() => {
                        self.stop_download(fi.dl);
                        let main_q = self.cfg.queues.first().map(|q| q.name.clone());
                        if let Some(d) = self.item_mut(fi.dl) {
                            if d.queue.is_none() {
                                d.queue = main_q;
                            }
                        }
                        close
                    }
                    (false, _) => {
                        if let Some(d) = self.item_mut(fi.dl) {
                            if matches!(d.state, DlState::Paused) {
                                d.state = DlState::Queued;
                            }
                        }
                        self.save_state();
                        close
                    }
                }
            }
            Message::FiCancel => {
                let fi = self.file_info.clone();
                let close = self.close_file_info_windows();
                if fi.is_new {
                    // The dialog was aborted: the auto-started item goes away
                    // with its temp data on cancel.
                    Task::batch([close, self.delete_item(fi.dl)])
                } else {
                    close
                }
            }

            // ----------------------------------------------------- progress
            Message::ProgTabSet(id, tab) => {
                self.prog.entry(id).or_default().tab = tab;
                Task::none()
            }
            Message::ProgToggleDetails(id) => {
                let p = self.prog.entry(id).or_default();
                p.details = !p.details;
                let details = p.details;
                // Collapse the dialog itself: no dead space below the
                // buttons when the details are hidden.
                if let Some(win) = self.win_of(WinKind::Progress(id)) {
                    // In OS points, so the collapsed box tracks View > Font
                    // the same way the dialog it opened at does.
                    let scale = self.ui_scale();
                    let h = if details { 582.0 } else { 352.0 };
                    window::resize(win, iced::Size::new(680.0 * scale, h * scale))
                } else {
                    Task::none()
                }
            }
            Message::ProgPauseResume(id) => {
                let active = self.item(id).map(|d| d.state.is_active()).unwrap_or(false);
                if active {
                    self.stop_download(id);
                    Task::none()
                } else {
                    self.start_download(id, false)
                }
            }
            Message::ProgCancel(id) => {
                // Cancel: pause the transfer (keep the partial file and
                // the list entry) and dismiss the progress dialog.
                if self.item(id).map(|d| d.state.is_active()).unwrap_or(false) {
                    self.stop_download(id);
                }
                if let Some(win) = self.win_of(WinKind::Progress(id)) {
                    self.windows.remove(&win);
                    window::close(win)
                } else {
                    Task::none()
                }
            }
            Message::ProgHideTab(id, which) => {
                if which == 1 {
                    self.cfg.settings.show_speed_tab = false;
                } else {
                    self.cfg.settings.show_completion_tab = false;
                }
                self.prog.entry(id).or_default().tab = ProgTab::Status;
                self.save_config();
                Task::none()
            }
            Message::ProgScanSkip(id) => {
                // Instant, not "ask the scanner to stop and wait": the file
                // is downloaded, the user said move on. The killed process
                // reports nothing back — its Done is never sent.
                crate::scan::skip(id);
                self.scans.remove(&id);
                if let Some(d) = self.item_mut(id) {
                    d.status_line = i18n::tr("Complete");
                }
                self.finish_completion(id)
            }
            Message::ProgScanKeep(id) => {
                // The verdict has been read: drop it and let the dialog go.
                // The file stays exactly where the download left it.
                self.scans.remove(&id);
                self.close_window(WinKind::Progress(id))
            }
            Message::ProgScanDelete(id) => {
                self.scans.remove(&id);
                let close = self.close_window(WinKind::Progress(id));
                // Straight through, no confirmation: the user is answering a
                // dialog that already names the file and the signature.
                self.remove_item_opts(id, true);
                close
            }
            Message::Scan(ev) => match ev {
                crate::scan::ScanEvent::Line { id, text, redraw } => {
                    if let Some(st) = self.scans.get_mut(&id) {
                        st.push(text, redraw);
                    }
                    Task::none()
                }
                crate::scan::ScanEvent::Done(id, outcome) => {
                    // Skip removed the entry already; a late verdict for a
                    // scan nobody is waiting on is dropped.
                    if !self.scans.contains_key(&id) {
                        return Task::none();
                    }
                    let (status, keep_open) = match &outcome {
                        crate::scan::Outcome::Clean => (i18n::tr("No virus found"), false),
                        crate::scan::Outcome::Infected(sig) if sig.is_empty() => {
                            (i18n::tr("Virus detected"), true)
                        }
                        crate::scan::Outcome::Infected(sig) => {
                            (format!("{}: {sig}", i18n::tr("Virus detected")), true)
                        }
                        crate::scan::Outcome::Failed(e) => {
                            (format!("{}: {e}", i18n::tr("Virus scan failed")), true)
                        }
                    };
                    if let Some(st) = self.scans.get_mut(&id) {
                        st.push(status.clone(), false);
                        st.outcome = Some(outcome);
                    }
                    if let Some(d) = self.item_mut(id) {
                        d.status_line = status;
                    }
                    if keep_open {
                        // The downloaded file is kept: an infected (or
                        // unscannable) file is reported, never deleted behind
                        // the user's back. The dialog stays up with the log.
                        crate::log::log(&format!("#{id} virus scan: kept, dialog left open"));
                        return Task::none();
                    }
                    self.scans.remove(&id);
                    if let Some(d) = self.item_mut(id) {
                        d.status_line = i18n::tr("Complete");
                    }
                    self.finish_completion(id)
                }
            },
            Message::ProgLimitOn(id, on) => {
                self.prog.entry(id).or_default().limit_on = on;
                let kb: u64 = self
                    .prog
                    .get(&id)
                    .and_then(|p| p.limit_kb.parse().ok())
                    .unwrap_or(10);
                if let Some(d) = self.item_mut(id) {
                    d.speed_limit = on.then_some(kb * 1024);
                }
                let lim = self.item(id).and_then(|d| self.effective_limit(d));
                engine::send(Cmd::SetLimit(id, lim));
                Task::none()
            }
            Message::ProgLimitRemember(id, b) => {
                // The live cap stays as it is; the flag only decides whether
                // it survives the transfer (Stopped/Finished clear the item's
                // limit when the box is unchecked).
                self.prog.entry(id).or_default().remember_limit = b;
                Task::none()
            }
            Message::ProgLimitKb(id, s) => {
                if s.chars().all(|c| c.is_ascii_digit()) && s.len() <= 9 {
                    let p = self.prog.entry(id).or_default();
                    p.limit_kb = s;
                    if p.limit_on {
                        let kb: u64 = p.limit_kb.parse().unwrap_or(10);
                        if let Some(d) = self.item_mut(id) {
                            d.speed_limit = Some(kb * 1024);
                        }
                        engine::send(Cmd::SetLimit(id, Some(kb * 1024)));
                    }
                }
                Task::none()
            }
            Message::ProgShutdownAfter(id, b) => {
                if let Some(d) = self.item_mut(id) {
                    d.shutdown_after = b;
                }
                self.save_state();
                Task::none()
            }
            Message::ProgShutdownAction(id, action) => {
                if let Some(d) = self.item_mut(id) {
                    d.shutdown_action = action;
                }
                self.save_state();
                Task::none()
            }

            // ----------------------------------------------------- complete
            Message::OpenFile(id) => {
                if let Some(d) = self.item(id) {
                    let _ = open::that_detached(d.full_path());
                }
                self.close_window(WinKind::Complete(id))
            }
            Message::OpenFolder(id) => {
                if let Some(d) = self.item(id) {
                    let _ = open::that_detached(&d.save_dir);
                }
                Task::none()
            }

            // ------------------------------------------------------ options
            Message::OptTabSet(t) => {
                self.options.tab = t;
                Task::none()
            }
            Message::OptExtStore(url) => {
                let _ = open::that_detached(url);
                Task::none()
            }
            Message::OptOk => {
                self.cfg.settings = self.options.draft.clone();
                self.cfg.categories = self.options.draft_cats.clone();
                self.save_config();
                // Re-assert Dock policy for the new setting. Windows are
                // still open here (Options itself), so this stays Regular;
                // the actual hide happens when the last window closes —
                // Accessory apps get no menu bar, so hiding the Dock while
                // a window is up would strip every Hydra menu.
                #[cfg(target_os = "macos")]
                crate::macos_dock::sync(self.cfg.settings.hide_from_taskbar, true);
                crate::autostart::apply(
                    self.cfg.settings.launch_on_startup,
                    self.cfg.settings.start_in_tray,
                );
                // X11: the hint is per window and lives on the windows that
                // are already open, so re-assert it on all of them.
                let skip_taskbar = Task::batch(
                    self.windows
                        .keys()
                        .copied()
                        .map(|id| self.skip_taskbar_task(id))
                        .collect::<Vec<_>>(),
                );
                Task::batch([skip_taskbar, self.close_window(WinKind::Options)])
            }
            Message::OptDraft(f) => self.on_opt_field(f),

            // ------------------------------------------------------- update
            Message::UpdateChecked(res) => {
                let manual = std::mem::take(&mut self.updater.manual);
                match res {
                    Ok(Some(info)) => {
                        crate::log::info(&format!("update available: {}", info.version));
                        self.updater = UpdateUiState {
                            info: Some(info),
                            ..UpdateUiState::default()
                        };
                        self.open_window(WinKind::Update)
                    }
                    Ok(None) if manual => {
                        self.confirm = Some(ConfirmKind::UpToDate);
                        self.open_window(WinKind::Confirm)
                    }
                    Ok(None) => Task::none(),
                    Err(e) if manual => {
                        crate::log::warn(&format!("update check failed: {e}"));
                        self.confirm = Some(ConfirmKind::UpdateCheckFailed(e));
                        self.open_window(WinKind::Confirm)
                    }
                    Err(e) => {
                        // A failed startup check is a log line, never a
                        // dialog: offline startups are normal.
                        crate::log::warn(&format!("update check failed: {e}"));
                        Task::none()
                    }
                }
            }
            Message::UpdateNow => {
                let Some(info) = self.updater.info.clone() else {
                    return Task::none();
                };
                crate::log::info(&format!(
                    "update: user chose Update Now for {}",
                    info.version
                ));
                // The dialog does not offer this for a packaged install; a
                // shortcut or a stale frame must not start a download the
                // finisher would then be unable to apply.
                if !info.in_place {
                    return Task::none();
                }
                if !matches!(
                    self.updater.phase,
                    UpdatePhase::Idle | UpdatePhase::Failed(_)
                ) {
                    return Task::none();
                }
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.updater.cancel = Some(cancel.clone());
                self.updater.phase = UpdatePhase::Downloading {
                    got: 0,
                    total: (info.size > 0).then_some(info.size),
                };
                Task::run(crate::update::run(info, cancel), Message::UpdateEvent)
            }
            Message::UpdateCancel => {
                match self.updater.phase {
                    // Mid-download: raise the flag; the stream answers with
                    // Cancelled once the transfer notices, which closes the
                    // window and cleans the partial file.
                    UpdatePhase::Downloading { .. } => {
                        if let Some(c) = &self.updater.cancel {
                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        Task::none()
                    }
                    // Too late to stop; ignore.
                    UpdatePhase::Verifying | UpdatePhase::Preparing | UpdatePhase::Restarting => {
                        Task::none()
                    }
                    _ => {
                        self.updater = UpdateUiState::default();
                        self.close_window(WinKind::Update)
                    }
                }
            }
            Message::UpdateOpenPage => {
                if let Some(info) = &self.updater.info {
                    if !info.html_url.is_empty() {
                        let _ = open::that_detached(&info.html_url);
                    }
                }
                Task::none()
            }
            Message::UpdateOpenUrl(url) => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    let _ = open::that_detached(url);
                }
                Task::none()
            }
            Message::UpdateEvent(ev) => {
                use crate::update::UpdateEvent as Ev;
                match ev {
                    Ev::Progress(got, total) => {
                        if let UpdatePhase::Downloading { total: known, .. } = self.updater.phase {
                            self.updater.phase = UpdatePhase::Downloading {
                                got,
                                total: total.or(known),
                            };
                        }
                        Task::none()
                    }
                    Ev::Verifying => {
                        self.updater.phase = UpdatePhase::Verifying;
                        Task::none()
                    }
                    Ev::Preparing => {
                        self.updater.phase = UpdatePhase::Preparing;
                        Task::none()
                    }
                    Ev::Cancelled => {
                        self.updater = UpdateUiState::default();
                        self.close_window(WinKind::Update)
                    }
                    Ev::Failed(e) => {
                        self.updater.phase = UpdatePhase::Failed(e);
                        self.updater.cancel = None;
                        Task::none()
                    }
                    Ev::ReadyToRestart => {
                        // The finisher waits for this process to exit before
                        // swapping files; leave with everything persisted.
                        self.updater.phase = UpdatePhase::Restarting;
                        self.save_state();
                        self.save_config();
                        self.flush_saves();
                        iced::exit()
                    }
                }
            }

            // ---------------------------------------------------- scheduler
            Message::SchQueue(q) => {
                self.sch.rename_draft = q.clone();
                self.sch.queue = q.clone();
                self.sch.renaming = false;
                if self.queue_click(Some(&q)) {
                    // Second click on the same queue: rename it in place.
                    // Stock queues are refused by the handler.
                    return self.update(Message::SchNameEdit);
                }
                Task::none()
            }
            Message::SchNameEdit => {
                // Stock queues keep their names; only user-created queues
                // rename.
                let builtin = self
                    .cfg
                    .queues
                    .iter()
                    .find(|q| q.name == self.sch.queue)
                    .map(|q| q.builtin)
                    .unwrap_or(false);
                if builtin {
                    return Task::none();
                }
                self.sch.renaming = true;
                self.sch.rename_draft = self.sch.queue.clone();
                iced::widget::operation::focus("sch-rename")
            }
            Message::SchNameDraft(v) => {
                self.sch.rename_draft = v;
                Task::none()
            }
            Message::SchNameCommit => {
                let old = self.sch.queue.clone();
                let new = self.sch.rename_draft.trim().to_string();
                self.sch.renaming = false;
                if self.rename_queue(&old, &new) {
                    self.sch.queue = new.clone();
                    self.sch.rename_draft = new;
                } else {
                    // Revert an empty/duplicate/stock-queue draft.
                    self.sch.rename_draft = old;
                }
                Task::none()
            }
            Message::SchTabSet(t) => {
                self.sch.tab = t;
                Task::none()
            }
            Message::SchField(f) => {
                self.on_sch_field(f);
                self.save_config();
                Task::none()
            }
            Message::SchStartNow => {
                let name = self.sch.queue.clone();
                // set_queue_running promotes paused members back to Queued.
                self.set_queue_running(&name, true);
                self.queue_tick()
            }
            Message::SchStop => {
                let name = self.sch.queue.clone();
                self.set_queue_running(&name, false);
                Task::none()
            }
            Message::SchNewQueue => {
                let n = self.cfg.queues.len() + 1;
                let name = format!("{} {n}", i18n::tr("Queue"));
                self.cfg.queues.push(crate::model::QueueDef {
                    name: name.clone(),
                    files_at_once: 4,
                    schedule: crate::model::Schedule::default(),
                    builtin: false,
                    running: false,
                    did_work: false,
                });
                self.sch.rename_draft = name.clone();
                self.sch.queue = name;
                self.save_config();
                Task::none()
            }
            Message::SchDeleteQueue => {
                let name = self.sch.queue.clone();
                let builtin = self
                    .cfg
                    .queues
                    .iter()
                    .find(|q| q.name == name)
                    .map(|q| q.builtin)
                    .unwrap_or(false);
                if builtin {
                    return Task::none();
                }
                if self.cfg.queues.len() > 1 {
                    self.cfg.queues.retain(|q| q.name != name);
                    for d in &mut self.state.downloads {
                        if d.queue.as_deref() == Some(name.as_str()) {
                            d.queue = None;
                        }
                    }
                    self.sch.queue = self.cfg.queues[0].name.clone();
                    self.sch.rename_draft = self.sch.queue.clone();
                    self.save_config();
                    self.save_state();
                }
                Task::none()
            }
            Message::SchDragStart(id) => {
                self.sch.sel_file = Some(id);
                self.sch.drag = Some(id);
                Task::none()
            }
            Message::SchDragOver(target) => {
                let Some(src) = self.sch.drag else {
                    return Task::none();
                };
                if src == target {
                    return Task::none();
                }
                let queue = self.sch.queue.clone();
                let mut order: Vec<DlId> = {
                    let mut m: Vec<&DownloadItem> = self
                        .state
                        .downloads
                        .iter()
                        .filter(|d| d.queue.as_deref() == Some(queue.as_str()))
                        .collect();
                    m.sort_by_key(|d| d.q_order);
                    m.iter().map(|d| d.id).collect()
                };
                let (Some(from), Some(to)) = (
                    order.iter().position(|x| *x == src),
                    order.iter().position(|x| *x == target),
                ) else {
                    return Task::none();
                };
                let moved = order.remove(from);
                order.insert(to, moved);
                // One pass over the list, not `item_mut` per member: this runs
                // for every row the drag crosses, and the linear lookup made
                // reordering a long queue quadratic in the download count.
                let rank: HashMap<DlId, u32> = order
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (*id, i as u32))
                    .collect();
                for d in &mut self.state.downloads {
                    if let Some(i) = rank.get(&d.id) {
                        d.q_order = *i;
                    }
                }
                Task::none()
            }
            Message::SchFileUp | Message::SchFileDown => {
                let up = matches!(message, Message::SchFileUp);
                if let Some(sel) = self.sch.sel_file {
                    let queue = self.sch.queue.clone();
                    let mut members: Vec<usize> = self
                        .state
                        .downloads
                        .iter()
                        .enumerate()
                        .filter(|(_, d)| d.queue.as_deref() == Some(queue.as_str()))
                        .map(|(i, _)| i)
                        .collect();
                    members.sort_by_key(|&i| self.state.downloads[i].q_order);
                    if let Some(pos) = members
                        .iter()
                        .position(|&i| self.state.downloads[i].id == sel)
                    {
                        let swap_with = if up {
                            pos.checked_sub(1)
                        } else {
                            pos.checked_add(1)
                        };
                        if let Some(other) = swap_with.filter(|&o| o < members.len()) {
                            let (i, j) = (members[pos], members[other]);
                            let tmp = self.state.downloads[i].q_order;
                            self.state.downloads[i].q_order = self.state.downloads[j].q_order;
                            self.state.downloads[j].q_order = tmp;
                            self.save_state();
                        }
                    }
                }
                Task::none()
            }
            Message::SchFileRemove => {
                if let Some(sel) = self.sch.sel_file {
                    if let Some(d) = self.item_mut(sel) {
                        d.queue = None;
                        if d.state == DlState::Queued {
                            d.state = DlState::Paused;
                        }
                    }
                    self.sch.sel_file = None;
                    self.save_state();
                }
                Task::none()
            }

            // -------------------------------------------------------- batch
            Message::BatchEdit(a) => {
                self.batch.text.perform(a);
                self.batch.parsed = false;
                self.parse_batch();
                Task::none()
            }
            Message::BatchLoaded(Some(text)) => {
                self.batch
                    .text
                    .perform(iced::widget::text_editor::Action::Edit(
                        iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                    ));
                self.batch.parsed = false;
                self.parse_batch();
                // Probe each link's size in the background ("you may wait
                // until it checks and fills all file types").
                let ua = self.cfg.settings.user_agent.clone();
                let probes: Vec<Task<Message>> = self
                    .batch
                    .checks
                    .iter()
                    .filter(|(u, _)| !self.batch.names.contains_key(u))
                    .map(|(u, _)| {
                        let url = u.clone();
                        let ua = ua.clone();
                        Task::perform(engine::probe_link(url.clone(), ua), move |meta| {
                            Message::BatchProbed(url.clone(), meta)
                        })
                    })
                    .collect();
                Task::batch(probes)
            }
            Message::BatchLoaded(None) => Task::none(),
            Message::InfoProbed(id, meta) => match meta {
                Some(meta) => self.adopt_probed(id, Some(meta.file_name), meta.size),
                None => Task::none(),
            },
            Message::BatchProbed(url, meta) => {
                if let Some(meta) = meta {
                    if let Some(size) = meta.size {
                        self.batch.sizes.insert(url.clone(), size);
                    }
                    if !meta.file_name.is_empty() {
                        self.batch.names.insert(url, meta.file_name);
                    }
                }
                Task::none()
            }
            Message::BatchCheck(i, b) => {
                if let Some(c) = self.batch.checks.get_mut(i) {
                    c.1 = b;
                }
                Task::none()
            }
            Message::BatchCheckAll(b) => {
                self.parse_batch();
                for c in &mut self.batch.checks {
                    c.1 = b;
                }
                Task::none()
            }
            Message::BatchSaveMode(m) => {
                self.batch.to_category = m == 1;
                self.batch.to_dir = m == 2;
                Task::none()
            }
            Message::BatchCategory(c) => {
                self.batch.category = c;
                Task::none()
            }
            Message::BatchDir(d) => {
                self.batch.dir = d;
                Task::none()
            }
            Message::BatchStreamQuality(q) => {
                self.batch.stream_quality = q;
                Task::none()
            }
            Message::BatchStreamContainer(c) => {
                self.batch.stream_container = c;
                Task::none()
            }
            Message::BatchOk => {
                self.parse_batch();
                let checked: Vec<String> = self
                    .batch
                    .checks
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(|(u, _)| u.clone())
                    .collect();
                if checked.is_empty() {
                    self.confirm = Some(ConfirmKind::NoneChecked);
                    return self.open_window(WinKind::Confirm);
                }
                let cat_override = self.batch.to_category.then(|| self.batch.category.clone());
                let dir_override = self.batch.to_dir.then(|| self.batch.dir.clone());
                let queue = Some("Main download queue".to_string());
                let want_height = self.batch.stream_quality.height();
                let container = if self.batch.stream_container.eq_ignore_ascii_case("ts") {
                    "TS"
                } else {
                    "MP4"
                };
                for url in checked {
                    // A manifest in the list is a STREAM, not a file: the
                    // batch's one quality choice is resolved against each
                    // manifest's own ladder when it starts.
                    let stream = manifest_address(&url).then(|| crate::model::StreamInfo {
                        protocol: if url
                            .split(['?', '#'])
                            .next()
                            .unwrap_or(&url)
                            .to_ascii_lowercase()
                            .ends_with(".mpd")
                        {
                            "dash".into()
                        } else {
                            "hls".into()
                        },
                        variant_url: None,
                        height: want_height,
                        bandwidth: None,
                        container: container.into(),
                        referer: None,
                        user_agent: None,
                        live: false,
                        max_seconds: None,
                    });
                    // A name the probe resolved (a redirector's real target, or
                    // a `Content-Disposition`) beats the one the pasted URL
                    // implies, and it decides the category with it.
                    let probed = self.batch.names.get(&url).cloned();
                    let stream_name = stream.as_ref().map(|si| {
                        let ext = if si.container.eq_ignore_ascii_case("ts") {
                            "ts"
                        } else {
                            "mp4"
                        };
                        format!("{}.{ext}", stream_base_name(&url))
                    });
                    let id = self.add_item(url, None, queue.clone());
                    if let Some(si) = stream {
                        if let Some(d) = self.item_mut(id) {
                            d.resume = Some(true);
                            d.stream = Some(si);
                        }
                    }
                    let probed = stream_name.or(probed);
                    if let Some(name) = probed.filter(|n| !n.is_empty()) {
                        let cat = categorize(&name, &self.cfg.categories);
                        let dir = self.cat_dir(cat.as_deref());
                        if let Some(d) = self.item_mut(id) {
                            d.file_name = name;
                            if let Some(c) = cat {
                                d.category = Some(c);
                            }
                            if let Some(dir) = dir {
                                d.save_dir = dir;
                            }
                        }
                    }
                    if let Some(c) = &cat_override {
                        let dir = self.cat_dir(Some(c));
                        if let Some(d) = self.item_mut(id) {
                            d.category = Some(c.clone());
                            if let Some(dir) = dir {
                                d.save_dir = dir;
                            }
                        }
                    }
                    if let Some(dir) = &dir_override {
                        if let Some(d) = self.item_mut(id) {
                            d.save_dir = dir.clone();
                        }
                    }
                    if let Some(d) = self.item_mut(id) {
                        d.state = DlState::Queued;
                    }
                }
                self.save_state();
                self.batch = BatchState::default();
                self.close_window(WinKind::Batch)
            }

            // ------------------------------------------------------ dialogs
            Message::ConfirmYes => {
                let kind = self.confirm.take();
                let remove_file = self.confirm_remove_file;
                let close = self.close_window(WinKind::Confirm);
                match kind {
                    Some(ConfirmKind::DeleteItems(ids)) => {
                        let mut tasks = vec![close];
                        for id in ids {
                            tasks.push(self.delete_item_opts(id, remove_file));
                        }
                        Task::batch(tasks)
                    }
                    Some(ConfirmKind::DeleteCompleted) => {
                        let ids: Vec<DlId> = self
                            .state
                            .downloads
                            .iter()
                            .filter(|d| d.state == DlState::Complete)
                            .map(|d| d.id)
                            .collect();
                        for id in ids {
                            self.remove_item_opts(id, remove_file);
                        }
                        self.save_state();
                        close
                    }
                    Some(ConfirmKind::StopWarn { ids, stop_queues }) => {
                        self.stop_ids(ids, stop_queues);
                        close
                    }
                    _ => close,
                }
            }
            Message::ConfirmRemoveFile(b) => {
                self.confirm_remove_file = b;
                Task::none()
            }
            Message::OpenPermissions => {
                // The guide window explains and deep-links; nothing can be
                // granted programmatically by design.
                self.confirm = None;
                let close = self.close_window(WinKind::Confirm);
                self.refresh_perm_status();
                Task::batch([close, self.open_window(WinKind::Permissions)])
            }
            Message::PermOpenPane(url) => {
                let _ = open::that_detached(url);
                Task::none()
            }
            Message::PermRefresh => {
                self.refresh_perm_status();
                Task::none()
            }
            Message::DupResume => {
                let existing = match self.confirm.take() {
                    Some(ConfirmKind::Duplicate { existing, .. }) => existing,
                    _ => None,
                };
                self.pending_add = None;
                let close = self.close_window(WinKind::Confirm);
                match existing {
                    Some(id) => {
                        self.selected = vec![id];
                        Task::batch([close, self.start_download(id, true)])
                    }
                    None => close,
                }
            }
            Message::DupOpen => {
                if let Some(ConfirmKind::Duplicate { file: Some(f), .. }) = self.confirm.take() {
                    let _ = open::that_detached(f);
                }
                self.pending_add = None;
                self.close_window(WinKind::Confirm)
            }
            Message::DupNew => {
                self.confirm = None;
                let close = self.close_window(WinKind::Confirm);
                let Some(pending) = self.pending_add.take() else {
                    return close;
                };
                let id = self.add_item(pending.url, pending.auth, None);
                self.apply_capture_extras(id, pending.cookies, pending.name);
                // `name_1.ext`, `name_2.ext`, ... until it collides with
                // neither the disk nor another list entry.
                if let Some(d) = self.item(id) {
                    let unique =
                        unique_file_name(&d.save_dir, &d.file_name, &self.state.downloads, id);
                    if let Some(d) = self.item_mut(id) {
                        d.file_name = unique;
                    }
                }
                self.save_state();
                if self.cfg.settings.show_file_info_dialog {
                    let d = self.item(id).unwrap();
                    self.file_info = FileInfoState {
                        dl: id,
                        category: d.category.clone().unwrap_or_else(|| "General".into()),
                        save_dir: d.save_dir.clone(),
                        file_name: d.file_name.clone(),
                        description: String::new(),
                        remember: true,
                        is_new: true,
                        url: d.url.clone(),
                        name_touched: true,
                        bg_blocked: !self.auto_start_type(id),
                        ..FileInfoState::default()
                    };
                    let bg = self.file_info_prefetch(id);
                    Task::batch([close, self.open_window(WinKind::FileInfo(id)), bg])
                } else {
                    Task::batch([close, self.start_download(id, true)])
                }
            }
            Message::CloseThis(id) => {
                if self.windows.remove(&id) == Some(WinKind::Confirm) {
                    self.confirm = None;
                    self.pending_add = None;
                }
                window::close(id)
            }
        }
    }

    /// May this new download start on its own behind the File Info dialog?
    /// Only for a file type still listed under Options > File types — a type
    /// the user took out of that list waits for Start Download — and only
    /// for a host that isn't on the "don't start downloading automatically"
    /// site list.
    fn auto_start_type(&self, id: DlId) -> bool {
        self.item(id)
            .map(|d| {
                auto_start_type(&d.file_name, &d.url, &self.cfg.settings.auto_types)
                    && !site_blocked(&d.url, &self.cfg.settings.dont_start_sites)
            })
            .unwrap_or(false)
    }

    /// What runs behind the new-download dialog: the transfer itself when
    /// background download is on and the type is listed (Start Download then
    /// only reveals the progress), otherwise a plain probe.
    ///
    /// Without either, nothing would answer what this file is called or how
    /// big it is: the fields would keep whatever the URL implied until the
    /// user pressed Start. That is wrong for any link whose name is not in
    /// its own path — a redirector (`href.li/?<url>`) reads as `index.html`
    /// — so ask the network the question the transfer would have asked.
    fn file_info_prefetch(&mut self, id: DlId) -> Task<Message> {
        if self.cfg.settings.bg_download && self.auto_start_type(id) {
            return self.start_download(id, false);
        }
        let ua = self.cfg.settings.user_agent.clone();
        let url = self.item(id).map(|d| d.url.clone()).unwrap_or_default();
        Task::perform(engine::probe_link(url, ua), move |meta| {
            Message::InfoProbed(id, meta)
        })
    }

    fn parse_batch(&mut self) {
        if self.batch.parsed {
            return;
        }
        let existing: Vec<(String, bool)> = self.batch.checks.clone();
        let sites = &self.cfg.settings.dont_start_sites;
        self.batch.checks = self
            .batch
            .text
            .text()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && engine::parse_url(l).is_ok())
            .map(|l| {
                let prev = existing.iter().find(|(u, _)| u == l).map(|(_, b)| *b);
                // A link whose host is on the "don't start downloading
                // automatically" list starts out unchecked: the user must
                // opt back in explicitly, same as the extension's own
                // pre-filter would have refused to hand it to Hydra at all.
                (
                    l.to_string(),
                    prev.unwrap_or_else(|| !site_blocked(l, sites)),
                )
            })
            .collect();
        self.batch.parsed = true;
    }

    // ------------------------------------------------------------ menu bar

    fn on_menu(&mut self, action: MenuAction) -> Task<Message> {
        match action {
            MenuAction::AddNewDownload => {
                self.add_url = AddUrlState::default();
                // Pre-fill the address when the clipboard holds a
                // plausible download link.
                Task::batch([
                    self.open_window(WinKind::AddUrl),
                    iced::clipboard::read().map(Message::AddrPrefill),
                ])
            }
            MenuAction::AddBatch => {
                self.batch = BatchState::default();
                self.batch.category = "General".into();
                self.open_window(WinKind::Batch)
            }
            MenuAction::AddBatchClipboard => {
                self.batch = BatchState::default();
                self.batch.category = "General".into();
                let open = self.open_window(WinKind::Batch);
                Task::batch([
                    open,
                    iced::clipboard::read().map(|text| match text {
                        Some(t) => Message::BatchEdit(iced::widget::text_editor::Action::Edit(
                            iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(t)),
                        )),
                        None => Message::Noop,
                    }),
                ])
            }
            MenuAction::AddBatchFile => {
                self.batch = BatchState::default();
                self.batch.category = "General".into();
                let open = self.open_window(WinKind::Batch);
                // Synchronous picker on purpose: AppKit dialogs must run on
                // the main thread — the async variant on a worker hangs.
                let text = rfd::FileDialog::new()
                    .add_filter("Text", &["txt", "text", "lst"])
                    .pick_file()
                    .and_then(|f| std::fs::read_to_string(f).ok());
                Task::batch([open, self.update(Message::BatchLoaded(text))])
            }
            MenuAction::SiteGrabber | MenuAction::DropTarget | MenuAction::Find => Task::none(),
            MenuAction::ExportList => {
                let urls: Vec<String> =
                    self.state.downloads.iter().map(|d| d.url.clone()).collect();
                let path = model::app_dir().join("export.txt");
                let _ = std::fs::write(&path, urls.join("\n"));
                let _ = open::that_detached(model::app_dir());
                Task::none()
            }
            MenuAction::ImportList => {
                let path = model::app_dir().join("export.txt");
                if let Ok(text) = std::fs::read_to_string(path) {
                    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        if engine::parse_url(line).is_ok() {
                            self.add_item(line.to_string(), None, None);
                        }
                    }
                }
                Task::none()
            }
            MenuAction::Exit => {
                self.save_state();
                self.save_config();
                self.flush_saves();
                iced::exit()
            }
            MenuAction::StopDownload => self.stop_ids_confirming(self.selected.clone(), false),
            MenuAction::Remove => self.update(Message::ToolbarDelete),
            MenuAction::DownloadNow | MenuAction::Redownload => {
                let ids = self.selected.clone();
                let show_progress = ids.len() == 1;
                let mut tasks = vec![];
                for id in ids {
                    if action == MenuAction::Redownload {
                        // A running transfer owns its `.part`; leave it be.
                        if self.item(id).map(|d| d.state.is_active()).unwrap_or(false) {
                            continue;
                        }
                        // Progress reset alone is not enough: the pinned
                        // `.part` would satisfy the resume check and the new
                        // object would be written into a file that still has
                        // the old one's bytes (and, via an edited URL, no
                        // record of which object it holds). Drop both.
                        if let Some(d) = self.item(id) {
                            let _ = std::fs::remove_file(d.part_file());
                        }
                        if let Some(d) = self.item_mut(id) {
                            d.held.clear();
                            d.downloaded = 0;
                            d.disp_progress = 0.0;
                            d.state = DlState::Paused;
                            d.part_path = None;
                            d.error = None;
                        }
                    }
                    tasks.push(self.start_download(id, show_progress));
                }
                Task::batch(tasks)
            }
            MenuAction::PauseAll | MenuAction::StopAll => {
                let ids: Vec<DlId> = self
                    .state
                    .downloads
                    .iter()
                    .filter(|d| d.state.is_active() || d.state == DlState::Queued)
                    .map(|d| d.id)
                    .collect();
                self.stop_ids_confirming(ids, true)
            }
            MenuAction::DeleteAllCompleted => {
                self.confirm = Some(ConfirmKind::DeleteCompleted);
                self.confirm_remove_file = false;
                self.open_window(WinKind::Confirm)
            }
            MenuAction::Scheduler => {
                if self.sch.queue.is_empty() {
                    self.sch.queue = self
                        .cfg
                        .queues
                        .first()
                        .map(|q| q.name.clone())
                        .unwrap_or_default();
                }
                self.sch.rename_draft = self.sch.queue.clone();
                self.open_window(WinKind::Scheduler)
            }
            MenuAction::StartQueue(name) => {
                // set_queue_running promotes paused members back to Queued.
                self.set_queue_running(&name, true);
                self.queue_tick()
            }
            MenuAction::StopQueue(name) => {
                self.set_queue_running(&name, false);
                Task::none()
            }
            MenuAction::SpeedLimiterToggle => {
                self.cfg.settings.speed_limiter_on = !self.cfg.settings.speed_limiter_on;
                if self.cfg.settings.global_speed_limit.is_none() {
                    self.cfg.settings.global_speed_limit = Some(128 * 1024);
                }
                let updates: Vec<(DlId, Option<u64>)> = self
                    .state
                    .downloads
                    .iter()
                    .filter(|d| d.state.is_active())
                    .map(|d| (d.id, self.effective_limit(d)))
                    .collect();
                for (id, lim) in updates {
                    engine::send(Cmd::SetLimit(id, lim));
                }
                self.save_config();
                self.sync_native_menu();
                Task::none()
            }
            MenuAction::Options => self.open_options(None),
            MenuAction::Extensions => self.open_options(Some(OptTab::Extensions)),
            MenuAction::CheckUpdates => {
                // A known offer just reopens its dialog; otherwise run a
                // fresh check in manual mode so the result is always shown.
                if self.updater.info.is_some() {
                    return self.open_window(WinKind::Update);
                }
                self.updater.manual = true;
                Task::perform(
                    crate::update::check(self.cfg.settings.beta_channel),
                    Message::UpdateChecked,
                )
            }
            MenuAction::HideCategories => {
                self.cfg.settings.show_categories = !self.cfg.settings.show_categories;
                self.save_config();
                self.sync_native_menu();
                Task::none()
            }
            MenuAction::ArrangeBy(key) => self.update(Message::SortBy(key)),
            MenuAction::SetTheme(mode) => {
                let changed = self.cfg.settings.theme() != mode;
                self.cfg.settings.theme_mode = Some(mode);
                // Like View > Font: even a click on the mode already in use
                // has to be written back to the menu, because muda unticked
                // it on the way in.
                self.sync_native_menu();
                if changed {
                    self.save_config();
                }
                Task::none()
            }
            MenuAction::FontSize(size) => {
                let changed = self.cfg.settings.font_size != size;
                self.cfg.settings.font_size = size;
                // Even a click on the size already in use has to be written
                // back to the menu: muda unticked it on the way in.
                self.sync_native_menu();
                if !changed {
                    return Task::none();
                }
                self.save_config();
                // The new ratio reaches the interface on the next redraw,
                // but a dialog is sized for the ratio it opened at: resize
                // the fixed ones now, or the extra rows a larger font needs
                // have nowhere to go until the dialog is reopened.
                let resizes: Vec<Task<Message>> = self
                    .windows
                    .iter()
                    .filter(|(_, k)| **k != WinKind::Main)
                    .map(|(id, kind)| {
                        let (w, h) = self.window_size(*kind);
                        window::resize(*id, iced::Size::new(w, h))
                    })
                    .collect();
                Task::batch(resizes)
            }
            MenuAction::Language(l) => {
                i18n::set_locale(&l);
                self.cfg.language = Some(l);
                self.save_config();
                // Native surfaces captured the old locale at build time.
                self.refresh_native_menu();
                {
                    let queues: Vec<String> =
                        self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                    crate::tray::reinstall(&queues, self.cfg.settings.power_save);
                }
                Task::none()
            }
            MenuAction::HomePage => {
                let _ = open::that_detached("https://hydra.javad.dev");
                Task::none()
            }
            MenuAction::Contribute => {
                let _ = open::that_detached("https://github.com/ja7ad/hydra");
                Task::none()
            }
            MenuAction::ReportIssue => {
                let _ = open::that_detached("https://github.com/ja7ad/hydra/issues");
                Task::none()
            }
            MenuAction::About => self.open_window(WinKind::About),
            MenuAction::Permissions => self.update(Message::OpenPermissions),
            MenuAction::MoveToQueue(q) => {
                for id in self.selected.clone() {
                    let order = self
                        .state
                        .downloads
                        .iter()
                        .filter(|d| d.queue.as_deref() == Some(q.as_str()))
                        .map(|d| d.q_order + 1)
                        .max()
                        .unwrap_or(0);
                    if let Some(d) = self.item_mut(id) {
                        d.queue = Some(q.clone());
                        d.q_order = order;
                        if d.state == DlState::Paused {
                            d.state = DlState::Queued;
                        }
                    }
                }
                self.save_state();
                Task::none()
            }
            MenuAction::RemoveFromQueue => {
                for id in self.selected.clone() {
                    if let Some(d) = self.item_mut(id) {
                        d.queue = None;
                        if d.state == DlState::Queued {
                            d.state = DlState::Paused;
                        }
                    }
                }
                self.save_state();
                Task::none()
            }
            MenuAction::Shortcuts => self.open_window(WinKind::Shortcuts),
            MenuAction::Logs => {
                // The log is the first thing to ask for when a stream failed
                // in a way the row could not explain, so it is one click
                // from the Help menu rather than a path to be looked up.
                let path = crate::log::path();
                if !path.exists() {
                    // Nothing has been written yet; an empty file beats
                    // "nothing happened when I clicked it".
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(&path, "");
                }
                let _ = open::that_detached(&path);
                Task::none()
            }
            MenuAction::PowerSaveToggle => {
                self.cfg.settings.power_save = !self.cfg.settings.power_save;
                engine::set_power_save(self.cfg.settings.power_save);
                crate::log::info(&format!("power save: {}", self.cfg.settings.power_save));
                self.save_config();
                {
                    let queues: Vec<String> =
                        self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                    crate::tray::reinstall(&queues, self.cfg.settings.power_save);
                }
                Task::none()
            }
            MenuAction::OpenSel => {
                if let Some(d) = self.selected_item() {
                    let _ = open::that_detached(d.full_path());
                }
                Task::none()
            }
            MenuAction::OpenFolderSel => {
                if let Some(d) = self.selected_item() {
                    let _ = open::that_detached(&d.save_dir);
                }
                Task::none()
            }
            MenuAction::Properties => {
                let fi = self.selected_item().map(|d| FileInfoState {
                    dl: d.id,
                    category: d.category.clone().unwrap_or_else(|| "General".into()),
                    save_dir: d.save_dir.clone(),
                    file_name: d.file_name.clone(),
                    description: d.description.clone(),
                    remember: false,
                    is_new: false,
                    url: d.url.clone(),
                    login: d.auth.clone().map(|a| a.0).unwrap_or_default(),
                    password: d.auth.clone().map(|a| a.1).unwrap_or_default(),
                    cookies: d.cookies.clone().unwrap_or_default(),
                    ..FileInfoState::default()
                });
                if let Some(fi) = fi {
                    let id = fi.dl;
                    self.file_info = fi;
                    self.open_window(WinKind::FileInfo(id))
                } else {
                    Task::none()
                }
            }
        }
    }

    fn on_opt_field(&mut self, f: OptField) -> Task<Message> {
        // Editor actions first: they need `self.options` whole.
        match f {
            OptField::AutoTypesEdit(a) => {
                self.options.auto_types_edit.perform(a);
                self.options.draft.auto_types = self.options.auto_types_edit.text();
                return Task::none();
            }
            OptField::SitesEdit(a) => {
                self.options.sites_edit.perform(a);
                self.options.draft.dont_start_sites = self.options.sites_edit.text();
                return Task::none();
            }
            // The Download-limit numbers keep a text buffer beside the draft:
            // digits only, and the draft takes the value only when it parses,
            // so a momentarily empty field is a legal editing state instead of
            // an ignored keystroke.
            OptField::DlLimitMb(v) => {
                let v: String = v.chars().filter(|c| c.is_ascii_digit()).take(9).collect();
                if let Ok(n) = v.parse() {
                    self.options.draft.dl_limit_mb = n;
                }
                self.options.dl_limit_mb_txt = v;
                return Task::none();
            }
            OptField::DlLimitHours(v) => {
                let v: String = v.chars().filter(|c| c.is_ascii_digit()).take(5).collect();
                if let Ok(n) = v.parse() {
                    self.options.draft.dl_limit_hours = n;
                }
                self.options.dl_limit_hours_txt = v;
                return Task::none();
            }
            _ => {}
        }
        let s = &mut self.options.draft;
        match f {
            OptField::LaunchStartup(b) => s.launch_on_startup = b,
            OptField::CheckUpdates(b) => s.check_updates_on_startup = b,
            OptField::BetaChannel(b) => s.beta_channel = b,
            OptField::StartInTray(b) => s.start_in_tray = b,
            OptField::CloseToTray(b) => s.close_to_tray = b,
            #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
            OptField::HideTaskbar(b) => s.hide_from_taskbar = b,
            OptField::PowerSave(b) => {
                s.power_save = b;
                engine::set_power_save(b);
            }
            OptField::GpuRender(b) => s.gpu_render = b,
            OptField::Clipboard(b) => s.monitor_clipboard = b,
            OptField::Browser(i, b) => {
                if let Some(x) = s.capture_browsers.get_mut(i) {
                    x.1 = b;
                }
            }
            OptField::AutoTypesEdit(_)
            | OptField::SitesEdit(_)
            | OptField::DlLimitMb(_)
            | OptField::DlLimitHours(_) => unreachable!(),
            OptField::ExcDialog(b) => s.show_exception_dialog = b,
            OptField::RememberLast(b) => s.remember_last_dir = b,
            OptField::ServerDate(b) => s.server_file_date = b,
            OptField::NoCatDirs(b) => s.no_category_dirs = b,
            OptField::ShowFileInfo(b) => s.show_file_info_dialog = b,
            OptField::StartMinimized(b) => s.start_minimized = b,
            OptField::SpeedTab(b) => s.show_speed_tab = b,
            OptField::CompletionTab(b) => s.show_completion_tab = b,
            OptField::HideButtons(b) => s.show_hide_buttons = b,
            OptField::CompleteDialog(b) => s.show_complete_dialog = b,
            OptField::UserAgent(v) => s.user_agent = v,
            OptField::VirusScanner(v) => s.virus_scanner = v,
            OptField::VirusArgs(v) => s.virus_args = v,
            OptField::BrowseVirus => {
                let p = rfd::FileDialog::new()
                    .pick_file()
                    .map(|p| p.to_string_lossy().into_owned());
                return self.on_opt_field(OptField::VirusPicked(p));
            }
            OptField::VirusPicked(Some(p)) => s.virus_scanner = p,
            OptField::VirusPicked(None) => {}
            OptField::DefaultConns(n) => s.default_conns = n,
            OptField::AdaptiveConns(b) => s.adaptive_conns = b,
            OptField::ExcServer(v) => self.options.conn_exc_server = v,
            OptField::ExcConns(v) => self.options.conn_exc_n = v,
            OptField::ExcAdd => {
                let server = self.options.conn_exc_server.trim().to_string();
                let n: usize = self.options.conn_exc_n.trim().parse().unwrap_or(0);
                if !server.is_empty() && n > 0 {
                    self.options
                        .draft
                        .conn_exceptions
                        .push((server, n.clamp(1, 32)));
                    self.options.conn_exc_server.clear();
                    self.options.conn_exc_n.clear();
                }
            }
            OptField::DlLimit(b) => s.dl_limit_enabled = b,
            OptField::WarnStop(b) => s.warn_before_stop = b,
            OptField::ProxyMode(m) => s.proxy_mode = m,
            OptField::ProxyScript(v) => s.proxy_script = v,
            OptField::ProxyHost(v) => s.proxy_host = v,
            OptField::ProxyPort(v) => s.proxy_port = v,
            OptField::ProxyUser(v) => s.proxy_user = v,
            OptField::ProxyPass(v) => s.proxy_pass = v,
            OptField::ProxyHttp(b) => s.proxy_http = b,
            OptField::ProxyHttps(b) => s.proxy_https = b,
            OptField::ProxyFtp(b) => s.proxy_ftp = b,
            OptField::FtpPasv(b) => s.ftp_pasv = b,
            OptField::SelCategory(c) => self.options.sel_category = c,
            OptField::CatDir(v) => {
                let sel = self.options.sel_category.clone();
                if let Some(c) = self.options.draft_cats.iter_mut().find(|c| c.name == sel) {
                    c.dir = v;
                }
            }
            OptField::BrowseCatDir => {
                let p = rfd::FileDialog::new()
                    .pick_folder()
                    .map(|p| p.to_string_lossy().into_owned());
                return self.on_opt_field(OptField::CatDirPicked(p));
            }
            OptField::CatDirPicked(Some(p)) => {
                let sel = self.options.sel_category.clone();
                if let Some(c) = self.options.draft_cats.iter_mut().find(|c| c.name == sel) {
                    c.dir = p;
                }
            }
            OptField::CatDirPicked(None) => {}
            OptField::LoginSel(i) => {
                self.options.sel_login = Some(i);
                if let Some(l) = self.options.draft.logins.get(i) {
                    self.options.login_site = l.site.clone();
                    self.options.login_user = l.user.clone();
                    self.options.login_pass = l.pass.clone();
                }
            }
            OptField::LoginSite(v) => self.options.login_site = v,
            OptField::LoginUser(v) => self.options.login_user = v,
            OptField::LoginPass(v) => self.options.login_pass = v,
            OptField::LoginAdd => {
                let site = self.options.login_site.trim().to_string();
                if !site.is_empty() {
                    self.options.draft.logins.push(SiteLogin {
                        site,
                        user: self.options.login_user.clone(),
                        pass: self.options.login_pass.clone(),
                    });
                    self.options.login_site.clear();
                    self.options.login_user.clear();
                    self.options.login_pass.clear();
                }
            }
            OptField::LoginRemove => {
                if let Some(i) = self.options.sel_login.take() {
                    if i < self.options.draft.logins.len() {
                        self.options.draft.logins.remove(i);
                    }
                }
            }
            OptField::Sound(i, b) => {
                if let Some(row) = s.sounds.get_mut(i) {
                    row.enabled = b;
                }
            }
            OptField::SoundBrowse(i) => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Audio", &["wav", "ogg"])
                    .pick_file()
                {
                    if let Some(row) = s.sounds.get_mut(i) {
                        row.file = p.to_string_lossy().into_owned();
                    }
                }
            }
            OptField::SoundPlay(i) => {
                if let Some(row) = s.sounds.get(i) {
                    sounds::play(
                        (!row.file.is_empty()).then(|| row.file.clone()),
                        sounds::Event::from_index(i),
                    );
                }
            }
        }
        Task::none()
    }

    fn on_sch_field(&mut self, f: SchField) {
        let name = self.sch.queue.clone();
        let Some(q) = self.cfg.queues.iter_mut().find(|q| q.name == name) else {
            return;
        };
        let s = &mut q.schedule;
        match f {
            SchField::Periodic(b) => s.periodic = b,
            SchField::OnStartup(b) => s.start_on_startup = b,
            SchField::StartEnabled(b) => s.start_enabled = b,
            SchField::StartAt(v) => s.start_at = v,
            SchField::Once(b) => s.once = b,
            SchField::Day(i, b) => {
                if let Some(d) = s.days.get_mut(i) {
                    *d = b;
                }
            }
            SchField::StopEnabled(b) => s.stop_enabled = b,
            SchField::StopAt(v) => s.stop_at = v,
            SchField::RetriesEnabled(b) => s.retries_enabled = b,
            SchField::Retries(v) => {
                if let Ok(n) = v.parse() {
                    s.retries = n;
                }
            }
            SchField::OpenFileEnabled(b) => s.open_file_enabled = b,
            SchField::OpenFile(v) => s.open_file = v,
            SchField::ExitDone(b) => s.exit_when_done = b,
            SchField::ShutdownDone(b) => s.shutdown_when_done = b,
            SchField::ShutdownAction(a) => s.shutdown_action = a,
            SchField::FilesAtOnce(v) => {
                if let Ok(n) = v.parse::<u32>() {
                    q.files_at_once = n.clamp(1, 16);
                }
            }
        }
    }
}

/// Default main-window size: scaled from the primary display's logical
/// resolution at the reference ratio (1009x606 on a 1512x982 screen — i.e.
/// two thirds of the width, ~62% of the height), clamped to the layout's
/// 900x600 floor. Falls back to 1009x606 when the display cannot be queried.
/// Stable sort on a precomputed key, applied in place.
///
/// The equivalent of `sort_by(|a, b| key(a).cmp(&key(b)))` with the key built
/// once per item rather than once per comparison; ties keep their order in
/// both directions, exactly as reversing the comparison would.
fn sort_keyed<'a, K: Ord>(v: &mut [&'a DownloadItem], asc: bool, key: impl Fn(&DownloadItem) -> K) {
    let mut keyed: Vec<(K, &'a DownloadItem)> = v.iter().map(|d| (key(d), *d)).collect();
    keyed.sort_by(|a, b| if asc { a.0.cmp(&b.0) } else { b.0.cmp(&a.0) });
    for (slot, (_, d)) in v.iter_mut().zip(keyed) {
        *slot = d;
    }
}

/// Shuts down or logs off the machine for a "when done" action. Errors (no
/// permission, an unsupported desktop session) only get a log line — by the
/// time this runs, the queue or download it was guarding is already done.
fn run_power_action(action: PowerAction) {
    let result = match action {
        PowerAction::Shutdown => system_shutdown::shutdown(),
        PowerAction::LogOff => system_shutdown::logout(),
    };
    if let Err(e) = result {
        crate::log::warn(&format!("power action {action:?} failed: {e}"));
    }
}

pub fn main_window_size() -> iced::Size {
    let fallback = iced::Size::new(1009.0, 606.0);
    let Ok(displays) = display_info::DisplayInfo::all() else {
        return fallback;
    };
    let Some(d) = displays.iter().find(|d| d.is_primary).or(displays.first()) else {
        return fallback;
    };
    let (mut w, mut h) = (d.width as f32, d.height as f32);
    // Some backends report physical pixels; normalize to logical points.
    if d.scale_factor > 1.0 && w / d.scale_factor >= 1000.0 {
        w /= d.scale_factor;
        h /= d.scale_factor;
    }
    iced::Size::new(
        (w * (1009.0 / 1512.0)).max(900.0),
        (h * (606.0 / 982.0)).max(600.0),
    )
}

/// Does this clipboard line look like a download link? Two signals, either
/// suffices: the file extension is in the Options > File types list, or the
/// URL matches a known download-host pattern that carries no extension
/// (GitHub releases and friends).
pub fn looks_downloadable(url: &str, auto_types: &str) -> bool {
    if engine::parse_url(url).is_err() {
        return false;
    }
    let name = engine::file_name_from_url(url);
    if let Some((_, ext)) = name.rsplit_once('.') {
        if type_listed(ext, auto_types) {
            return true;
        }
    }
    const PATTERNS: [&str; 6] = [
        "/releases/download/",
        "githubusercontent.com/",
        "/-/releases/",
        "sourceforge.net/projects/",
        "/files/download",
        "dl.google.com/",
    ];
    PATTERNS.iter().any(|p| url.contains(p))
}

/// Is this extension one of the types in Options > File types? The list
/// accepts commas, spaces or newlines as separators.
pub fn type_listed(ext: &str, auto_types: &str) -> bool {
    auto_types
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .any(|t| t.eq_ignore_ascii_case(ext))
}

/// Auto-start policy for the "Download File Info" dialog. A type the user
/// removed from Options > File types must not pull bytes on its own, so only
/// a listed extension qualifies. A name that carries no extension at all
/// cannot be judged by the list and stays allowed — the list filters types,
/// it is not a whitelist of links.
pub fn auto_start_type(file_name: &str, url: &str, auto_types: &str) -> bool {
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_string())
        .or_else(|| {
            engine::file_name_from_url(url)
                .rsplit_once('.')
                .map(|(_, e)| e.to_string())
        });
    match ext {
        Some(e) => type_listed(&e, auto_types),
        None => true,
    }
}

/// Does this URL's host match an entry in the Options > File types
/// "Don't start downloading automatically from the following sites" list?
/// Entries are separated by whitespace or commas; a host matches a pattern
/// when it equals it or is one of its subdomains — the optional `*.`
/// prefix some entries carry is cosmetic, mirroring the browser
/// extensions' `siteSkipped`.
pub fn site_blocked(url: &str, dont_start_sites: &str) -> bool {
    let host = match engine::parse_url(url) {
        Ok(p) => p.host.to_ascii_lowercase(),
        Err(_) => return false,
    };
    dont_start_sites
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|p| !p.is_empty())
        .any(|pat| {
            let pat = pat.to_ascii_lowercase();
            let dom = pat.strip_prefix("*.").unwrap_or(&pat);
            host == dom || host.ends_with(&format!(".{dom}"))
        })
}

/// Which Options > Sites Logins entry, if any, applies to this URL. A site
/// matches by host (or subdomain), optionally scoped to a path prefix when
/// the entry carries one (`example.com/private`); when several entries
/// match, the one with the longest `site` string wins, since it is the most
/// specific.
pub fn find_login<'a>(url: &str, logins: &'a [SiteLogin]) -> Option<&'a SiteLogin> {
    let parsed = engine::parse_url(url).ok()?;
    let host = parsed.host.to_ascii_lowercase();
    let path = parsed.path.trim_start_matches('/').to_ascii_lowercase();
    logins
        .iter()
        .filter(|l| {
            let site = l.site.trim().to_ascii_lowercase();
            let site = site
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_start_matches("ftp://");
            let (site_host, site_path) = site.split_once('/').unwrap_or((site, ""));
            let site_host = site_host.trim_start_matches("*.");
            !site_host.is_empty()
                && (host == site_host || host.ends_with(&format!(".{site_host}")))
                && (site_path.is_empty() || path.starts_with(site_path))
        })
        .max_by_key(|l| l.site.len())
}

/// Times a click against the one before it, for rows that cannot use
/// `mouse_area::on_double_click` — a `button` captures the mouse event
/// before an enclosing area is asked, so the area's own detection never
/// runs. Two clicks on the same `name` within 400ms (the window the
/// download table already uses) read as a double; a click on a different
/// target, or on none at all, restarts the count.
pub fn double_click(last: &mut Option<(String, Instant)>, name: Option<&str>) -> bool {
    let Some(name) = name else {
        *last = None;
        return false;
    };
    let double = last
        .as_ref()
        .is_some_and(|(prev, at)| prev == name && at.elapsed().as_millis() < 400);
    // A completed pair ends the sequence: a third click starts a new one
    // rather than reading as a second double.
    *last = if double {
        None
    } else {
        Some((name.to_string(), Instant::now()))
    };
    double
}

/// Directory comparison for collision checks: `/a/b` and `/a/b/` (or a `..`
/// variant) are the same place on disk and must compare equal — a raw string
/// comparison let two spellings of one directory collide silently.
/// Existing directories canonicalize (resolving symlinks and case); paths
/// not on disk yet fall back to a structural normalisation.
pub fn normalize_dir(p: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(p);
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `name.ext` -> first of `name.ext`, `name_1.ext`, `name_2.ext`, ... that
/// exists neither on disk in `dir` nor as another list entry saving there.
pub fn unique_file_name(
    dir: &str,
    name: &str,
    downloads: &[DownloadItem],
    skip_id: DlId,
) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), Some(e.to_string())),
        _ => (name.to_string(), None),
    };
    let candidate = |n: u32| -> String {
        let stem = if n == 0 {
            stem.clone()
        } else {
            format!("{stem}_{n}")
        };
        match &ext {
            Some(e) => format!("{stem}.{e}"),
            None => stem,
        }
    };
    let dir_norm = normalize_dir(dir);
    let taken: Vec<&str> = downloads
        .iter()
        .filter(|d| d.id != skip_id && normalize_dir(&d.save_dir) == dir_norm)
        .map(|d| d.file_name.as_str())
        .collect();
    for n in 0..1000 {
        let c = candidate(n);
        let on_disk = std::path::Path::new(dir).join(&c).exists();
        if !on_disk && !taken.iter().any(|t| *t == c) {
            return c;
        }
    }
    candidate(1000)
}

/// Total bytes covered by a span list.
pub fn sum_spans(spans: &[(u64, u64)]) -> u64 {
    spans.iter().map(|(lo, hi)| hi.saturating_sub(*lo)).sum()
}

/// Sort, clamp and merge persisted byte spans. `Scheduler::mark_done` is
/// told "these bytes arrived, never fetch them again", so spans read back
/// from disk are not trusted as-is: inverted spans drop, spans past the
/// object end clamp, and overlapping or adjacent spans merge.
/// A page title made safe to be a filename on any of the three platforms.
///
/// The extension already trims what it can see, but the name arrives from a
/// web page and must never be trusted to be a leaf name: a `/` or a `..` in
/// it would place the finished file outside the category directory.
/// Whether an address is worth reading as a manifest.
///
/// The body decides in the end, but fetching every address the user types
/// would be both slow and rude; the extension is the cheap filter.
/// A filename for a stream, from its manifest URL.
///
/// Manifests are called `index.m3u8` or `master.mpd` almost universally, so
/// the directory above is what actually names the asset.
pub fn stream_base_name(url: &str) -> String {
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    let path = match bare.find("://") {
        Some(i) => {
            let rest = &bare[i + 3..];
            rest.find('/').map(|j| &rest[j..]).unwrap_or("")
        }
        None => bare,
    };
    let mut parts = path.rsplit('/').filter(|p| !p.is_empty());
    let file = parts.next().unwrap_or("stream");
    let stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
    const GENERIC: &[&str] = &[
        "index", "master", "manifest", "playlist", "mono", "stream", "media", "video", "main",
    ];
    let picked = if GENERIC.contains(&stem.to_ascii_lowercase().as_str()) {
        parts.next().unwrap_or(stem)
    } else {
        stem
    };
    let cleaned = sanitize_file_name(picked);
    if cleaned.is_empty() {
        "stream".into()
    } else {
        cleaned
    }
}

pub fn manifest_address(url: &str) -> bool {
    let lower = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && (lower.ends_with(".m3u8") || lower.ends_with(".m3u") || lower.ends_with(".mpd"))
}

pub fn sanitize_file_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Leading dots hide the file; a name that is only dots is not a name.
    let trimmed = collapsed.trim_matches(['.', ' ']).to_string();
    trimmed
        .chars()
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn sanitize_spans(spans: &[(u64, u64)], size: Option<u64>) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = spans
        .iter()
        .map(|&(lo, hi)| match size {
            Some(s) => (lo.min(s), hi.min(s)),
            None => (lo, hi),
        })
        .filter(|&(lo, hi)| lo < hi)
        .collect();
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (lo, hi) in v {
        match out.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

/// Does the `.part` staging file plausibly contain the recorded spans?
///
/// Length must match the recorded size exactly — `SparseSink::create` calls
/// `set_len(size)` up front, so any other length means another object or a
/// truncation. On unix the allocated blocks are checked against the bytes
/// the spans claim: holes are not allocated, so a `.part` that received one
/// byte cannot pass for one that received everything (file length says
/// nothing about completeness — the lesson the CLI already recorded).
pub fn part_matches(part: &std::path::Path, size: Option<u64>, held: &[(u64, u64)]) -> bool {
    let Ok(meta) = std::fs::metadata(part) else {
        return false;
    };
    if meta.len() == 0 {
        return false;
    }
    if let Some(s) = size {
        if meta.len() != s {
            return false;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let allocated = meta.blocks().saturating_mul(512);
        let claimed = sum_spans(held);
        // Filesystems account allocation in blocks; allow one block of
        // rounding at each end of every span.
        let slop = (held.len() as u64 + 1).saturating_mul(2 * 4096);
        if allocated.saturating_add(slop) < claimed {
            return false;
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::io::Read;
        // On Windows, detect sparse files by checking if we can read actual
        // data from the claimed parts. A file created with set_len but no
        // actual data written will fail this check.
        if !held.is_empty() {
            if let Ok(mut file) = std::fs::File::open(part) {
                let mut buf = vec![0u8; 4096.min(meta.len() as usize)];
                // Try to read from the first claimed part
                if let Ok(n) = file.read(&mut buf) {
                    if n == 0 {
                        // File is sparse: no data could be read
                        return false;
                    }
                }
            }
        }
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    let _ = held;
    true
}

/// Is a queue's scheduled start due this minute?
///
/// The day checkboxes belong to the *Daily* mode: "Once" fires at the next
/// matching wall-clock minute regardless of the ticked days (the caller
/// disarms it afterwards), while "Daily" fires only on ticked days. The
/// previous gating had the polarity inverted, so Once obeyed the day list
/// and Daily ignored it.
pub fn schedule_start_due(
    s: &crate::model::Schedule,
    hhmm: &str,
    weekday: usize,
    running: bool,
) -> bool {
    if !s.start_enabled || s.start_at != hhmm || running {
        return false;
    }
    s.once || s.days.get(weekday).copied().unwrap_or(false)
}

/// Bytes the Connection tab's download limit allows per window, or `None`
/// while the limit is switched off.
pub fn quota_cap(s: &crate::model::Settings) -> Option<u64> {
    s.dl_limit_enabled
        .then(|| s.dl_limit_mb.saturating_mul(1024 * 1024))
}

/// Window length in seconds. Zero hours would describe a window that never
/// rolls — a permanent block once the cap is hit — so it floors at one.
pub fn quota_window_secs(s: &crate::model::Settings) -> i64 {
    (s.dl_limit_hours.max(1) as i64).saturating_mul(3600)
}

/// Is the cap reached? False whenever the limit is off, so every caller can
/// ask this one question instead of re-testing the checkbox.
pub fn quota_over_cap(s: &crate::model::Settings, q: &DlQuota) -> bool {
    matches!(quota_cap(s), Some(cap) if q.used >= cap)
}

/// Has the running window aged out? A window that never opened
/// (`window_start == 0`) has nothing to roll.
pub fn quota_window_elapsed(s: &crate::model::Settings, q: &DlQuota, now: i64) -> bool {
    q.window_start > 0 && now.saturating_sub(q.window_start) >= quota_window_secs(s)
}

/// Everything the download limit must park: whatever is transferring, plus
/// what is merely waiting for a slot — a queued item would otherwise be
/// started by the very next `queue_tick` and immediately blocked, leaving
/// the list saying "Queued" when nothing can move for hours.
pub fn quota_park_targets(downloads: &[DownloadItem]) -> Vec<DlId> {
    downloads
        .iter()
        .filter(|d| d.state.is_active() || d.state == DlState::Queued)
        .map(|d| d.id)
        .collect()
}

/// Hand back what the limiter parked, and only that: a hand-paused download
/// carries `limit_paused == false` and must stay paused across a rollover.
///
/// Queue members go back to `Queued` so their queue paces them against
/// `files_at_once`; every released id is returned, paired with whether the
/// queue took it back — the caller starts the rest itself.
pub fn quota_release_targets(downloads: &mut [DownloadItem]) -> Vec<(DlId, bool)> {
    let mut released = vec![];
    for d in downloads {
        if !d.limit_paused {
            continue;
        }
        d.limit_paused = false;
        d.status_line.clear();
        let requeued = d.queue.is_some();
        if requeued {
            d.state = DlState::Queued;
        }
        released.push((d.id, requeued));
    }
    released
}

/// Put a queue's paused/errored members back to `Queued` with a fresh retry
/// budget. Shared by every start path (menu, toolbar, scheduler wall-clock,
/// startup) via [`App::set_queue_running`].
pub fn promote_queue_members(downloads: &mut [DownloadItem], queue: &str) {
    for d in downloads {
        if d.queue.as_deref() == Some(queue) && matches!(d.state, DlState::Paused | DlState::Error)
        {
            d.state = DlState::Queued;
            d.retries = 0;
        }
    }
}

/// Progress-dialog state seeded from the item's live cap, so the Speed
/// Limiter tab tells the truth about a persisted per-download limit instead
/// of showing an unchecked box while the cap is active.
fn prog_state_seed(speed_limit: Option<u64>) -> ProgState {
    ProgState {
        details: true,
        limit_on: speed_limit.is_some(),
        limit_kb: (speed_limit.unwrap_or(10 * 1024) / 1024).to_string(),
        remember_limit: speed_limit.is_some(),
        ..ProgState::default()
    }
}

/// Normalize a key press into a combo string ("cmd+shift+v"). `cmd` is ⌘ on
/// macOS and Ctrl elsewhere (`Modifiers::command()`); presses without a
/// command modifier are not shortcuts.
pub fn combo_string(key: &iced::keyboard::Key, mods: iced::keyboard::Modifiers) -> Option<String> {
    if !mods.command() {
        return None;
    }
    let base = match key {
        iced::keyboard::Key::Character(c) => c.to_string().to_ascii_lowercase(),
        _ => return None,
    };
    let mut combo = String::from("cmd+");
    if mods.shift() {
        combo.push_str("shift+");
    }
    if mods.alt() {
        combo.push_str("alt+");
    }
    combo.push_str(&base);
    Some(combo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Schedule;

    fn item(id: DlId, dir: &str, name: &str, queue: Option<&str>, state: DlState) -> DownloadItem {
        DownloadItem {
            id,
            url: format!("https://example.com/{name}"),
            file_name: name.into(),
            save_dir: dir.into(),
            category: None,
            description: String::new(),
            size: None,
            downloaded: 0,
            state,
            error: None,
            resume: None,
            added: 0,
            last_try: None,
            queue: queue.map(str::to_string),
            q_order: 0,
            auth: None,
            cookies: None,
            speed_limit: None,
            limit_paused: false,
            held: vec![],
            part_path: None,
            rate: 0.0,
            retries: 3,
            disp_progress: 0.0,
            eta_secs: None,
            recorded_secs: None,
            conns: vec![],
            status_line: String::new(),
            shutdown_after: false,
            shutdown_action: PowerAction::default(),
            stream: None,
        }
    }

    #[test]
    fn auto_start_type_follows_the_file_types_list() {
        const TYPES: &str = "ZIP, EXE MP4\nISO";
        // Listed type: the dialog may pull bytes on its own.
        assert!(auto_start_type(
            "setup.exe",
            "https://ex.com/setup.exe",
            TYPES
        ));
        assert!(auto_start_type(
            "disk.ISO",
            "https://ex.com/disk.ISO",
            TYPES
        ));
        // Removed from the list (no RAR here): nothing starts by itself.
        assert!(!auto_start_type(
            "IObit.Smart.Defrag.12.0.0.536.rar",
            "https://dl.ex.com/soft/i/IObit.rar?123",
            TYPES
        ));
        // The captured name wins over the URL's own suffix.
        assert!(!auto_start_type(
            "archive.rar",
            "https://ex.com/download.zip",
            TYPES
        ));
        // Nothing to judge: an extensionless link stays allowed.
        assert!(auto_start_type(
            "download",
            "https://ex.com/download",
            TYPES
        ));
    }

    #[test]
    fn site_blocked_matches_hosts_and_subdomains() {
        const SITES: &str = "*.update.microsoft.com download.windowsupdate.com, *.example.com";
        assert!(site_blocked(
            "https://sub.update.microsoft.com/x.exe",
            SITES
        ));
        assert!(site_blocked("https://update.microsoft.com/x.exe", SITES));
        assert!(site_blocked(
            "https://download.windowsupdate.com/x.exe",
            SITES
        ));
        assert!(!site_blocked("https://example.org/x.exe", SITES));
        // A host that merely contains the pattern as a substring, without
        // being a subdomain, must not match.
        assert!(!site_blocked("https://evilexample.com/x.exe", SITES));
    }

    #[test]
    fn site_blocked_example_com_patterns() {
        // Bare host, no wildcard: exact host and any subdomain match.
        const BARE: &str = "example.com";
        assert!(site_blocked("https://example.com/x.exe", BARE));
        assert!(site_blocked("http://EXAMPLE.COM/x.exe", BARE));
        assert!(site_blocked("https://www.example.com/x.exe", BARE));
        assert!(site_blocked("https://a.b.example.com/x.exe", BARE));
        assert!(site_blocked("ftp://example.com/x.exe", BARE));
        assert!(!site_blocked("https://notexample.com/x.exe", BARE));
        assert!(!site_blocked("https://example.com.evil.net/x.exe", BARE));
        assert!(!site_blocked("https://example.org/x.exe", BARE));

        // Wildcard-prefixed entry: same behaviour, the "*." is cosmetic.
        const WILDCARD: &str = "*.example.com";
        assert!(site_blocked("https://example.com/x.exe", WILDCARD));
        assert!(site_blocked("https://sub.example.com/x.exe", WILDCARD));
        assert!(site_blocked("https://deep.sub.example.com/x.exe", WILDCARD));
        assert!(!site_blocked("https://example.co/x.exe", WILDCARD));

        // Comma- and whitespace-separated entries, mixed on one line.
        const MIXED: &str = "example.com, *.example.net\texample.org";
        assert!(site_blocked("https://example.com/x.exe", MIXED));
        assert!(site_blocked("https://cdn.example.net/x.exe", MIXED));
        assert!(site_blocked("https://example.org/x.exe", MIXED));
        assert!(!site_blocked("https://example.io/x.exe", MIXED));

        // No pattern at all: nothing is blocked.
        assert!(!site_blocked("https://example.com/x.exe", ""));
        assert!(!site_blocked("https://example.com/x.exe", "   "));
    }

    #[test]
    fn double_click_pairs_only_consecutive_clicks_on_one_row() {
        // A lone click is never a double; the one right after it is.
        let mut last = None;
        assert!(!double_click(&mut last, Some("Queue 3")));
        assert!(double_click(&mut last, Some("Queue 3")));
        // The pair is consumed, so a third click opens a new sequence
        // instead of reading as another double.
        assert!(!double_click(&mut last, Some("Queue 3")));
        assert!(double_click(&mut last, Some("Queue 3")));

        // A detour through another row breaks the pair.
        let mut last = None;
        assert!(!double_click(&mut last, Some("Queue 3")));
        assert!(!double_click(&mut last, Some("Queue 4")));
        assert!(!double_click(&mut last, Some("Queue 3")));

        // A click that landed on no queue row at all clears the count.
        let mut last = None;
        assert!(!double_click(&mut last, Some("Queue 3")));
        assert!(!double_click(&mut last, None));
        assert!(last.is_none());
        assert!(!double_click(&mut last, Some("Queue 3")));

        // Two clicks too far apart in time are two single clicks.
        let mut last = Some((
            "Queue 3".to_string(),
            Instant::now() - std::time::Duration::from_millis(600),
        ));
        assert!(!double_click(&mut last, Some("Queue 3")));
    }

    #[test]
    fn find_login_matches_host_and_prefers_the_most_specific_path() {
        let logins = vec![
            SiteLogin {
                site: "example.com".into(),
                user: "site-user".into(),
                pass: "site-pass".into(),
            },
            SiteLogin {
                site: "example.com/private".into(),
                user: "private-user".into(),
                pass: "private-pass".into(),
            },
            SiteLogin {
                site: "*.ftp.example.org".into(),
                user: "ftp-user".into(),
                pass: "ftp-pass".into(),
            },
        ];
        // Bare host entry applies anywhere on the site.
        assert_eq!(
            find_login("https://example.com/public/x.zip", &logins).map(|l| l.user.as_str()),
            Some("site-user")
        );
        // The more specific path-scoped entry wins under that path.
        assert_eq!(
            find_login("https://example.com/private/x.zip", &logins).map(|l| l.user.as_str()),
            Some("private-user")
        );
        // Subdomain wildcard entry matches a subdomain, not an unrelated host.
        assert_eq!(
            find_login("ftp://files.ftp.example.org/x.zip", &logins).map(|l| l.user.as_str()),
            Some("ftp-user")
        );
        assert!(find_login("https://unrelated.com/x.zip", &logins).is_none());
    }

    #[test]
    fn progress_never_draws_past_a_full_bar() {
        // A stream's size is a projection, so it can lag the bytes already
        // on disk. The fraction must still be a fraction.
        let mut d = item(1, "/tmp", "x.mp4", None, DlState::Receiving);
        d.size = Some(1_000);
        d.downloaded = 1_200;
        assert_eq!(d.progress(), 1.0);
        d.downloaded = 250;
        assert_eq!(d.progress(), 0.25);
        // No size at all is no progress, not a divide.
        d.size = None;
        assert_eq!(d.progress(), 0.0);
        d.size = Some(0);
        assert_eq!(d.progress(), 0.0);
    }

    #[test]
    fn a_timed_recording_has_a_real_progress_bar() {
        use crate::model::live_progress;
        // Asked for 300 s and 150 s are captured: genuinely half done.
        assert!((live_progress(Some(300), Some(150.0)) - 0.5).abs() < 1e-6);
        // Overrunning the limit cannot draw more than a full bar.
        assert!((live_progress(Some(300), Some(400.0)) - 1.0).abs() < 1e-6);
        // No limit set, or nothing recorded yet: there is no fraction to
        // show, and bytes must not stand in for one — a live stream has no
        // size for them to be a fraction OF.
        assert_eq!(live_progress(None, Some(150.0)), 0.0);
        assert_eq!(live_progress(Some(300), None), 0.0);
        assert_eq!(live_progress(Some(0), Some(10.0)), 0.0);
    }

    #[test]
    fn a_stream_is_named_after_its_asset_not_its_manifest() {
        // Every stream on the internet is called index.m3u8; the directory
        // above it is what actually distinguishes them.
        assert_eq!(
            stream_base_name("https://hls.ex/live_cdn/abc/emcQJ0pGpremocy/index.m3u8"),
            "emcQJ0pGpremocy"
        );
        assert_eq!(stream_base_name("https://cdn.ex/a/master.m3u8"), "a");
        assert_eq!(stream_base_name("https://cdn.ex/x/mono.m3u8"), "x");
        // A manifest with a real name keeps it.
        assert_eq!(
            stream_base_name("https://cdn.ex/a/bbb_30fps.mpd"),
            "bbb_30fps"
        );
        // Query strings are not part of a name.
        assert_eq!(stream_base_name("https://cdn.ex/a/show.m3u8?t=1"), "show");
        // Nothing usable still yields a filename, and never a path.
        assert_eq!(stream_base_name("https://cdn.ex/"), "stream");
        assert!(!stream_base_name("https://e/a/../../etc/passwd.m3u8").contains('/'));
    }

    #[test]
    fn only_manifest_addresses_are_inspected() {
        assert!(manifest_address("https://cdn.ex/a/index.m3u8"));
        assert!(manifest_address("http://cdn.ex/a/x.mpd"));
        assert!(manifest_address("https://cdn.ex/a/x.m3u8?token=abc"));
        // Case does not matter.
        assert!(manifest_address("https://cdn.ex/A/INDEX.M3U8"));
        // An ordinary file is not probed, so typing one costs nothing.
        assert!(!manifest_address("https://cdn.ex/a/video.mp4"));
        assert!(!manifest_address("https://cdn.ex/a/"));
        // Nor is anything that is not even an http address yet.
        assert!(!manifest_address("cdn.ex/a/index.m3u8"));
        assert!(!manifest_address("ftp://cdn.ex/a/index.m3u8"));
        assert!(!manifest_address(""));
    }

    #[test]
    fn a_page_title_cannot_escape_the_save_directory() {
        // The name arrives from a web page: separators and traversal must
        // not survive into a path.
        assert_eq!(sanitize_file_name("../../etc/passwd"), "etc passwd");
        assert_eq!(
            sanitize_file_name("a/b\\c:d*e?f\"g<h>i|j"),
            "a b c d e f g h i j"
        );
        assert_eq!(sanitize_file_name("  ..  "), "");
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        assert_eq!(
            sanitize_file_name("Free HLS Player - Online M3U8 Player & Stream Tester | Castr"),
            "Free HLS Player - Online M3U8 Player & Stream Tester Castr"
        );
        // Control characters are not names either.
        assert_eq!(sanitize_file_name("a\u{0}b\nc"), "a b c");
        assert!(sanitize_file_name(&"x".repeat(400)).chars().count() <= 120);
    }

    #[test]
    fn sanitize_spans_merges_clamps_and_drops() {
        // Unsorted with an overlap and an adjacency: one merged span.
        assert_eq!(
            sanitize_spans(&[(50, 80), (0, 60), (80, 90)], Some(100)),
            vec![(0, 90)]
        );
        // Inverted and empty spans drop; spans past the end clamp.
        assert_eq!(
            sanitize_spans(&[(30, 10), (5, 5), (90, 500)], Some(100)),
            vec![(90, 100)]
        );
        // A span entirely past the end vanishes rather than surviving as
        // (size, size).
        assert_eq!(sanitize_spans(&[(200, 300)], Some(100)), vec![]);
        // Unknown size: no clamping, still sorted and merged.
        assert_eq!(sanitize_spans(&[(10, 20), (0, 15)], None), vec![(0, 20)]);
        assert_eq!(sum_spans(&[(0, 90), (100, 110)]), 100);
    }

    #[test]
    fn part_verification_rejects_holes_and_wrong_length() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("hydra-part-test-{}.part", std::process::id()));
        let _ = std::fs::remove_file(&path);

        // Missing file never resumes.
        assert!(!part_matches(&path, Some(1000), &[(0, 1000)]));

        // Sparse file: set_len only, no byte written. Length matches, but
        // nothing is allocated — claiming the whole object must fail.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(1_000_000).unwrap();
        drop(f);
        assert!(!part_matches(&path, Some(1_000_000), &[(0, 1_000_000)]));
        // ...while claiming nothing (fresh start) is fine.
        assert!(part_matches(&path, Some(1_000_000), &[]));

        // Fully written file passes.
        std::fs::write(&path, vec![0xAB; 1_000_000]).unwrap();
        assert!(part_matches(&path, Some(1_000_000), &[(0, 1_000_000)]));
        // Recorded size disagreeing with the file on disk fails.
        assert!(!part_matches(&path, Some(2_000_000), &[(0, 1_000_000)]));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schedule_once_ignores_days_daily_honours_them() {
        let mut s = Schedule {
            start_enabled: true,
            start_at: "23:00".into(),
            ..Schedule::default()
        };
        s.days = [false; 7];

        // Daily (once == false) with no day ticked: never due.
        s.once = false;
        assert!(!schedule_start_due(&s, "23:00", 3, false));
        // Daily with Wednesday ticked: due on Wednesday only.
        s.days[3] = true;
        assert!(schedule_start_due(&s, "23:00", 3, false));
        assert!(!schedule_start_due(&s, "23:00", 4, false));

        // Once fires regardless of the day list.
        s.once = true;
        s.days = [false; 7];
        assert!(schedule_start_due(&s, "23:00", 0, false));

        // Wrong minute, already running, or disarmed: not due.
        assert!(!schedule_start_due(&s, "22:59", 0, false));
        assert!(!schedule_start_due(&s, "23:00", 0, true));
        s.start_enabled = false;
        assert!(!schedule_start_due(&s, "23:00", 0, false));
    }

    #[test]
    fn weekday_maps_sunday_to_zero() {
        use chrono::Datelike;
        // 2026-08-16 is a Sunday; the `days` array is UI-indexed Sun=0.
        let sun = chrono::NaiveDate::from_ymd_opt(2026, 8, 16).unwrap();
        assert_eq!(sun.weekday().num_days_from_sunday(), 0);
        let mon = chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap();
        assert_eq!(mon.weekday().num_days_from_sunday(), 1);
        let sat = chrono::NaiveDate::from_ymd_opt(2026, 8, 22).unwrap();
        assert_eq!(sat.weekday().num_days_from_sunday(), 6);
    }

    fn limit_settings(mb: u64, hours: u64) -> crate::model::Settings {
        crate::model::Settings {
            dl_limit_enabled: true,
            dl_limit_mb: mb,
            dl_limit_hours: hours,
            ..crate::model::Settings::default()
        }
    }

    #[test]
    fn quota_counts_only_while_the_limit_is_on() {
        let mut off = limit_settings(2, 5);
        off.dl_limit_enabled = false;
        assert_eq!(quota_cap(&off), None);
        // Off means off even with a counter left over from an earlier window:
        // nothing may be blocked by a limit the user switched away.
        let stale = DlQuota {
            used: 999 * 1024 * 1024,
            window_start: 1,
        };
        assert!(!quota_over_cap(&off, &stale));
        assert_eq!(quota_cap(&limit_settings(2, 5)), Some(2 * 1024 * 1024));
    }

    #[test]
    fn quota_blocks_at_the_cap_not_before() {
        let s = limit_settings(2, 5);
        let cap = 2 * 1024 * 1024;
        let under = DlQuota {
            used: cap - 1,
            window_start: 100,
        };
        let at = DlQuota {
            used: cap,
            window_start: 100,
        };
        assert!(!quota_over_cap(&s, &under));
        assert!(quota_over_cap(&s, &at));
    }

    #[test]
    fn quota_window_rolls_on_the_hour_it_was_given() {
        let s = limit_settings(2, 5);
        let q = DlQuota {
            used: 10,
            window_start: 1000,
        };
        assert!(!quota_window_elapsed(&s, &q, 1000 + 5 * 3600 - 1));
        assert!(quota_window_elapsed(&s, &q, 1000 + 5 * 3600));
        // A window that never opened has nothing to roll, whatever the clock
        // says — otherwise a fresh install would "reset" on every tick.
        let never = DlQuota {
            used: 0,
            window_start: 0,
        };
        assert!(!quota_window_elapsed(&s, &never, 9_999_999));
        // Zero hours would describe a window that never rolls: floored at one
        // so the cap can never become a permanent block.
        assert_eq!(quota_window_secs(&limit_settings(2, 0)), 3600);
    }

    #[test]
    fn quota_parks_running_and_waiting_work_only() {
        let downloads = vec![
            item(1, "/d", "a.zip", None, DlState::Receiving),
            item(2, "/d", "b.zip", None, DlState::Connecting),
            item(3, "/d", "c.zip", Some("Q"), DlState::Queued),
            item(4, "/d", "d.zip", None, DlState::Paused),
            item(5, "/d", "e.zip", None, DlState::Complete),
            item(6, "/d", "f.zip", None, DlState::Error),
        ];
        // Queued is included: leaving it alone would have `queue_tick` start
        // it a second later only for the cap to refuse it, over and over.
        assert_eq!(quota_park_targets(&downloads), vec![1, 2, 3]);
    }

    #[test]
    fn quota_releases_only_what_it_parked() {
        let mut downloads = vec![
            item(1, "/d", "a.zip", None, DlState::Paused),
            item(2, "/d", "b.zip", Some("Q"), DlState::Paused),
            item(3, "/d", "c.zip", None, DlState::Paused),
        ];
        downloads[0].limit_paused = true;
        downloads[1].limit_paused = true;
        downloads[1].status_line = "Download limit reached".into();
        // #3 is the user's own pause and carries no flag.
        let released = quota_release_targets(&mut downloads);
        assert_eq!(released, vec![(1, false), (2, true)]);
        // The queue member is handed back to its queue rather than started
        // behind the queue's back; the direct download is the caller's to
        // start, and stays paused until it does.
        assert_eq!(downloads[1].state, DlState::Queued);
        assert!(downloads[1].status_line.is_empty());
        assert_eq!(downloads[0].state, DlState::Paused);
        assert!(!downloads[0].limit_paused && !downloads[1].limit_paused);
        // Hand-paused: untouched.
        assert_eq!(downloads[2].state, DlState::Paused);
        // Idempotent — a second tick releases nothing.
        assert!(quota_release_targets(&mut downloads).is_empty());
    }

    #[test]
    fn promote_resets_paused_and_errored_members_only() {
        let mut downloads = vec![
            item(1, "/d", "a.zip", Some("Q"), DlState::Paused),
            item(2, "/d", "b.zip", Some("Q"), DlState::Error),
            item(3, "/d", "c.zip", Some("Q"), DlState::Complete),
            item(4, "/d", "d.zip", Some("Q"), DlState::Receiving),
            item(5, "/d", "e.zip", Some("Other"), DlState::Paused),
            item(6, "/d", "f.zip", None, DlState::Paused),
        ];
        promote_queue_members(&mut downloads, "Q");
        assert_eq!(downloads[0].state, DlState::Queued);
        assert_eq!(downloads[0].retries, 0);
        assert_eq!(downloads[1].state, DlState::Queued);
        assert_eq!(downloads[1].retries, 0);
        assert_eq!(downloads[2].state, DlState::Complete);
        assert_eq!(downloads[3].state, DlState::Receiving);
        assert_eq!(downloads[4].state, DlState::Paused);
        assert_eq!(downloads[5].state, DlState::Paused);
    }

    #[test]
    fn unique_file_name_sees_through_dir_spelling() {
        // Same directory spelled with a trailing slash and a `.` segment:
        // the list entry must still count as a collision.
        let dir = "/nonexistent-hydra-test/downloads";
        let listed = item(
            1,
            "/nonexistent-hydra-test/./downloads/",
            "a.zip",
            None,
            DlState::Paused,
        );
        let name = unique_file_name(dir, "a.zip", &[listed], 99);
        assert_eq!(name, "a_1.zip");
        // No collision: the name is kept.
        let other = item(
            1,
            "/nonexistent-hydra-test/elsewhere",
            "a.zip",
            None,
            DlState::Paused,
        );
        assert_eq!(unique_file_name(dir, "a.zip", &[other], 99), "a.zip");
        // The entry being renamed does not collide with itself.
        let me = item(7, dir, "a.zip", None, DlState::Paused);
        assert_eq!(unique_file_name(dir, "a.zip", &[me], 7), "a.zip");
    }

    #[test]
    fn prog_state_seed_reflects_persisted_limit() {
        let p = prog_state_seed(Some(512 * 1024));
        assert!(p.limit_on);
        assert!(p.remember_limit);
        assert_eq!(p.limit_kb, "512");
        let p = prog_state_seed(None);
        assert!(!p.limit_on);
        assert!(!p.remember_limit);
        assert_eq!(p.limit_kb, "10");
    }
}

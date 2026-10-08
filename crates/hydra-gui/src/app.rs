// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Application state and update logic — every window shares this one `App`.

use crate::engine::{self, Cmd, StartSpec};
use crate::model::{
    self, categorize, CategoryDef, Column, ColumnPref, ConfigFile, DlId, DlQuota, DlState,
    DownloadItem, PowerAction, ProxyChoice, ProxyMode, ProxyPick, Settings, SiteLogin, SortKey,
    StateFile, ThemeMode,
};
use crate::picker::{self, Ask};
use crate::sounds;
use crate::{fmt, hydata, i18n};
use iced::window;
use iced::{Point, Task};
use std::collections::HashMap;
use std::time::Instant;

pub type El<'a> = iced::Element<'a, Message>;

/// How far a press on a header cell has to travel before it reorders the
/// columns instead of sorting by them on release.
const HEADER_DRAG_SLOP: f32 = 4.0;

/// How long the batch dialog's URL box must stand still before its links are
/// probed. Long enough that typing a URL out by hand measures it once at the
/// end rather than once per keystroke, short enough that a paste fills the
/// table in without a visible pause.
const BATCH_PROBE_IDLE: std::time::Duration = std::time::Duration::from_millis(400);

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
    /// Which columns the download table shows, in what order.
    Columns,
    Update,
    /// The cancellable countdown shown before a "when done" power action.
    Power,
    /// What is inside a ZIP archive, read from its tail before the download.
    ZipPreview(DlId),
}

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
    ExportUrls,
    ExportSettings,
    ImportSettings,
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
    /// Switch the Speed Limiter to the named profile, cap and all.
    SpeedProfile(String),
    /// Options, opened on the Connection page where the profiles are edited.
    SpeedLimitSettings,
    Options,
    /// Options, opened straight on the Extensions page (toolbar shortcut).
    Extensions,
    HideCategories,
    /// View > Hide toolbar text: toolbar icons keep their labels or trade
    /// them for hover tooltips.
    HideToolbarText,
    ArrangeBy(SortKey),
    /// Set ascending (`true`) or descending (`false`) sort direction.
    SortDirection(bool),
    /// View > Columns, and the header menu's own entry: the manage dialog.
    ManageColumns,
    /// Header menu: show or hide one column, or move it left (`true`).
    ToggleColumn(Column),
    MoveColumn(Column, bool),
    SetTheme(ThemeMode),
    /// View > Scale, in percent (`theme::SCALE_STEPS`).
    UiScale(u16),
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
    /// Hand the finished file to an application of the user's choosing,
    /// without changing what its file type opens with by default.
    OpenWithSel,
    /// Move the finished file somewhere else, or give it another name, and
    /// keep the row pointing at it.
    MoveRenameSel,
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
            MenuAction::ExportUrls => "export_urls".into(),
            MenuAction::ExportSettings => "export_settings".into(),
            MenuAction::ImportSettings => "import_settings".into(),
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
            MenuAction::SpeedProfile(p) => format!("speed_profile:{p}"),
            MenuAction::SpeedLimitSettings => "speed_limit_settings".into(),
            MenuAction::Options => "options".into(),
            MenuAction::Extensions => "extensions".into(),
            MenuAction::HideCategories => "hide_cats".into(),
            MenuAction::HideToolbarText => "hide_toolbar_text".into(),
            MenuAction::ArrangeBy(k) => format!("arrange:{}", k.id()),
            MenuAction::SortDirection(asc) => {
                if *asc {
                    "sort_dir:asc".into()
                } else {
                    "sort_dir:desc".into()
                }
            }
            MenuAction::ManageColumns => "columns".into(),
            MenuAction::ToggleColumn(c) => format!("col_show:{}", c.id()),
            MenuAction::MoveColumn(c, left) => {
                format!("col_move:{}:{}", c.id(), if *left { "l" } else { "r" })
            }
            MenuAction::SetTheme(m) => format!("theme:{m:?}"),
            MenuAction::UiScale(s) => format!("scale:{s}"),
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
            MenuAction::OpenWithSel => "open_with_sel".into(),
            MenuAction::MoveRenameSel => "move_rename_sel".into(),
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
        if let Some(p) = id.strip_prefix("speed_profile:") {
            return Some(MenuAction::SpeedProfile(p.into()));
        }
        if let Some(k) = id.strip_prefix("arrange:") {
            return Some(MenuAction::ArrangeBy(
                SortKey::from_id(k).unwrap_or(SortKey::Column(Column::Name)),
            ));
        }
        if id == "sort_dir:asc" {
            return Some(MenuAction::SortDirection(true));
        }
        if id == "sort_dir:desc" {
            return Some(MenuAction::SortDirection(false));
        }
        if let Some(c) = id.strip_prefix("col_show:").and_then(Column::from_id) {
            return Some(MenuAction::ToggleColumn(c));
        }
        if let Some((c, side)) = id
            .strip_prefix("col_move:")
            .and_then(|rest| rest.split_once(':'))
        {
            if let Some(c) = Column::from_id(c) {
                return Some(MenuAction::MoveColumn(c, side == "l"));
            }
        }
        if let Some(m) = id.strip_prefix("theme:") {
            let mode = match m {
                "Light" => ThemeMode::Light,
                "Dark" => ThemeMode::Dark,
                _ => ThemeMode::System,
            };
            return Some(MenuAction::SetTheme(mode));
        }
        if let Some(s) = id.strip_prefix("scale:") {
            return s.parse().ok().map(MenuAction::UiScale);
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
            "export_urls" => MenuAction::ExportUrls,
            "export_settings" => MenuAction::ExportSettings,
            "import_settings" => MenuAction::ImportSettings,
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
            "speed_limit_settings" => MenuAction::SpeedLimitSettings,
            "options" => MenuAction::Options,
            "extensions" => MenuAction::Extensions,
            "hide_cats" => MenuAction::HideCategories,
            "hide_toolbar_text" => MenuAction::HideToolbarText,
            "columns" => MenuAction::ManageColumns,
            "homepage" => MenuAction::HomePage,
            "contribute" => MenuAction::Contribute,
            "report_issue" => MenuAction::ReportIssue,
            "about" => MenuAction::About,
            "permissions" => MenuAction::Permissions,
            "rm_q" => MenuAction::RemoveFromQueue,
            "props" => MenuAction::Properties,
            "open_sel" => MenuAction::OpenSel,
            "open_folder_sel" => MenuAction::OpenFolderSel,
            "open_with_sel" => MenuAction::OpenWithSel,
            "move_rename_sel" => MenuAction::MoveRenameSel,
            "power_save" => MenuAction::PowerSaveToggle,
            "shortcuts" => MenuAction::Shortcuts,
            "logs" => MenuAction::Logs,
            "check_updates" => MenuAction::CheckUpdates,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct AddUrlState {
    pub plugin_plan: Option<crate::plugins::PlanInfo>,
    pub input_plugin: Option<String>,
    pub plugin_ctl: Option<std::sync::Arc<hya_plugin::runtime::CallCtl>>,
    pub plugin_of: String,
    pub plugin_probing: bool,
    pub loading_frame: u8,
    /// A cookie edit invalidates the session used by an in-flight inspection.
    pub plugin_probe_stale: bool,
    pub browser_cookies: Option<hya_net::CookieJar>,
    pub address: String,
    pub use_auth: bool,
    pub login: String,
    pub password: String,
    pub error: Option<String>,
    /// What the browser knew about a captured download; applied to the item
    /// right after `add_item` so a background start already has it.
    pub capture: CaptureExtras,
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
    /// What the address turned out to be, when it is a Metalink document.
    ///
    /// Read BEFORE the download starts, like a stream manifest and for a
    /// related reason: a mirror list decides how many files are about to be
    /// added and what they will be called, and neither is changeable halfway
    /// through.
    pub metalink: Option<crate::engine::MetalinkProbe>,
    /// The address `metalink` describes, so an edited address invalidates it.
    pub metalink_of: String,
    pub metalink_probing: bool,
    pub metalink_error: Option<String>,
    /// The address whose cookies were imported, so an edited one re-imports
    /// rather than carrying the previous site's session to a new host.
    pub cookies_of: String,
    pub cookies_importing: bool,
    /// `capture.cookies` holds what an import produced, not what the user
    /// typed, so an address on another host clears it rather than carrying it.
    pub cookies_imported: bool,
    /// One line naming the store the cookies were read from, or why they
    /// could not be. Shown under the field, which is what makes the import
    /// something the user is told about rather than something that happens.
    pub cookie_note: Option<String>,
}

/// Height the Add URL dialog must reserve for the mirror-list panel.
///
/// A heading row, then one line per file the panel shows, then a "+N" line when
/// there are more than it shows. Kept as a function of what the panel DRAWS
/// rather than inlined at the call site because the two have to agree and
/// nothing at run time notices when they do not: the window simply opens too
/// short and the OK button is below the bottom edge — which is how the stream
/// panel shipped a probed manifest whose quality picker could not be reached.
///
/// `MAX_ROWS` is the same cap `windows::add_url` draws to.
fn metalink_panel_height(probing: bool, files: Option<usize>) -> f32 {
    const HEADING: f32 = 34.0;
    const ROW: f32 = 24.0;
    if probing {
        return 28.0;
    }
    match files {
        None => 0.0,
        Some(n) => {
            let rows = n.min(METALINK_PANEL_ROWS) + usize::from(n > METALINK_PANEL_ROWS);
            HEADING + ROW * rows as f32
        }
    }
}

/// Files the mirror-list panel lists before collapsing into a "+N" line.
///
/// One definition, read by both the drawing and the measuring, because a panel
/// that draws more rows than the window reserved is a dialog whose OK button
/// cannot be clicked.
pub const METALINK_PANEL_ROWS: usize = 3;

/// A capture parked behind the duplicate-confirmation dialog.
#[derive(Clone, Debug)]
pub struct PendingAdd {
    pub plugin_plan: Option<crate::plugins::PlanInfo>,
    pub url: String,
    pub auth: Option<(String, String)>,
    pub capture: CaptureExtras,
}

/// What the browser knew about a captured download and Hydra cannot work out
/// for itself.
///
/// One value rather than four `Option<String>`s in a row: they travel
/// together from [`crate::extbus::ExtDownload`] through the duplicate dialog
/// to the item, and four interchangeable strings as positional arguments is
/// a swap that compiles.
#[derive(Clone, Debug, Default)]
pub struct CaptureExtras {
    /// Cookie header the extension assembled for this URL.
    pub cookies: Option<String>,
    /// Where `cookies` came from, in one line, for Properties to show. The
    /// description travels rather than a second copy of the value.
    pub cookie_source: Option<String>,
    /// Filename the browser had already resolved (Content-Disposition et
    /// al.) — better than what the URL path implies.
    pub name: Option<String>,
    /// Page the browser was on when it captured this file. A CDN with
    /// hotlink protection answers `403` without it.
    pub referer: Option<String>,
    /// The proxy the browser itself is using, as a full specification.
    pub proxy: Option<String>,
}

impl CaptureExtras {
    /// The same fields with blanks dropped, which is what an extension that
    /// had nothing to say sends.
    fn taken(&mut self) -> Self {
        let take = |v: &mut Option<String>| v.take().filter(|s| !s.is_empty());
        Self {
            cookies: take(&mut self.cookies),
            cookie_source: take(&mut self.cookie_source),
            name: take(&mut self.name),
            referer: take(&mut self.referer),
            proxy: take(&mut self.proxy),
        }
    }
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
    /// Which proxy this download takes. The address box keeps its text while
    /// the picker sits on Default or No proxy, so switching back does not
    /// mean retyping it.
    pub proxy_pick: ProxyPick,
    pub proxy_spec: String,
    /// Set when OK or Start Download was refused because "this download
    /// only" names no address. It is what turns the empty box red: an
    /// address not yet typed is not a mistake until it is used.
    pub proxy_needs_address: bool,
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

impl FileInfoState {
    /// The Save As box shows folder and file name as one path, as  does.
    pub fn save_as(&self) -> String {
        if self.save_dir.is_empty() {
            self.file_name.clone()
        } else {
            std::path::Path::new(&self.save_dir)
                .join(&self.file_name)
                .to_string_lossy()
                .into_owned()
        }
    }

    /// Split an edited Save As path back into folder and file name, marking
    /// only the part that actually changed as user-edited so the background
    /// probe keeps filling the other one.
    pub fn set_save_as(&mut self, path: &str) {
        let (dir, name) = split_save_as(path);
        if dir != self.save_dir {
            self.save_dir = dir;
            self.dir_touched = true;
        }
        if name != self.file_name {
            self.file_name = name;
            self.name_touched = true;
        }
    }
}

/// `(folder, file name)` of a typed path. No separator means the user
/// typed a bare file name: the folder is empty and the start handler falls
/// back to the category folder.
fn split_save_as(path: &str) -> (String, String) {
    match path.rfind(['/', '\\']) {
        Some(0) => (path[..1].to_string(), path[1..].to_string()),
        Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ProgTab {
    #[default]
    Status,
    Speed,
    /// Which proxy this one download takes.
    Proxy,
    Completion,
}

#[derive(Clone, Debug, Default)]
pub struct ProgState {
    pub tab: ProgTab,
    pub details: bool,
    pub limit_on: bool,
    pub limit_kb: String,
    pub remember_limit: bool,
    /// The proxy row, seeded from the item when the window opens. Edits go
    /// straight onto the item — the transfer is stopped while they are
    /// possible, so there is nothing to keep a draft for.
    pub proxy_pick: ProxyPick,
    pub proxy_spec: String,
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
    /// Connection's four sections are four leaves, so the sub-tab row can
    /// address them the same way it addresses [`OptTab::Proxy`] — and so a
    /// menu item can deep-link to the one it means rather than to a tab the
    /// user then has to scroll.
    Connection,
    Cookies,
    SpeedLimit,
    Quota,
    Proxy,
    Sites,
    Extensions,
    MediaTools,
    Plugins,
    Sounds,
}

#[derive(Debug)]
pub struct OptionsState {
    pub plugins: crate::plugins::Page,
    pub tab: OptTab,
    pub draft: crate::model::Settings,
    /// The settings the dialog opened on. OK merges against these — see
    /// [`crate::model::Settings::apply_options_draft`].
    pub base: crate::model::Settings,
    /// Why OK was refused, shown beside the buttons until the field is fixed.
    pub error: Option<String>,
    /// Multiline editors for the File-types lists.
    pub auto_types_edit: iced::widget::text_editor::Content,
    pub sites_edit: iced::widget::text_editor::Content,
    pub draft_cats: Vec<crate::model::CategoryDef>,
    pub sel_category: String,
    /// The selected category's file types, as typed. Folded back into
    /// `draft_cats` when the selection leaves the category, not per
    /// keystroke — see [`App::commit_cat_exts`].
    pub cat_exts_edit: iced::widget::text_editor::Content,
    /// Name box under the category picker: what New would create, and what
    /// Rename would rename the selected category to.
    pub cat_name: String,
    /// Renames made in this visit, original name -> current name, so the
    /// downloads already filed under the old name follow the category when
    /// OK commits.
    pub cat_renames: HashMap<String, String>,
    pub sel_login: Option<usize>,
    pub login_site: String,
    pub login_user: String,
    pub login_pass: String,
    /// Row picked in the Connection tab's exception list, so it can be edited
    /// or removed. `None` is "nothing picked", which is what Remove needs to
    /// distinguish from "row 0".
    pub sel_exc: Option<usize>,
    pub conn_exc_server: String,
    pub conn_exc_n: String,
    /// Whether the chosen browser's cookie store can actually be read, and
    /// which file it is — or why not.
    ///
    /// Asked when the browser is PICKED rather than when a download needs it:
    /// on macOS every browser profile is behind the system privacy control, so
    /// a picker that accepted a browser and then failed on every address would
    /// be reporting a configuration problem as a download problem.
    pub cookie_check: Option<Result<String, String>>,
    pub cookie_checking: bool,
    /// Which check is the current one. Every keystroke in the profile box
    /// starts another, and they finish in whatever order the disk allows; only
    /// the newest is allowed to answer.
    pub cookie_check_gen: u64,
    /// Text buffers for the Download-limit numbers. The draft holds `u64`,
    /// and binding an input straight to `n.to_string()` makes the field
    /// un-clearable: an empty string fails to parse, the commit is skipped
    /// and the old number is re-rendered on the next frame, so the user can
    /// never backspace past the first digit. The buffer holds what was
    /// typed; the draft takes it whenever it parses.
    pub dl_limit_mb_txt: String,
    pub dl_limit_hours_txt: String,
    /// Text buffer for the global speed cap, in KB/sec — same reason as the
    /// download-limit buffers above.
    pub speed_limit_kb_txt: String,
    /// Row picked in the speed-profile list, and the name/speed boxes that
    /// edit it. `None` is "nothing picked", which is what Remove needs to
    /// tell apart from "row 0".
    pub sel_profile: Option<usize>,
    pub profile_name: String,
    pub profile_kb: String,
}

impl Default for OptionsState {
    fn default() -> Self {
        OptionsState {
            plugins: crate::plugins::Page::default(),
            tab: OptTab::General,
            draft: crate::model::Settings::default(),
            base: crate::model::Settings::default(),
            error: None,
            auto_types_edit: iced::widget::text_editor::Content::new(),
            sites_edit: iced::widget::text_editor::Content::new(),
            draft_cats: crate::model::default_categories(),
            sel_category: model::DEFAULT_CATEGORY.into(),
            cat_exts_edit: iced::widget::text_editor::Content::new(),
            cat_name: String::new(),
            cat_renames: HashMap::new(),
            sel_login: None,
            login_site: String::new(),
            login_user: String::new(),
            login_pass: String::new(),
            sel_exc: None,
            conn_exc_server: String::new(),
            conn_exc_n: String::new(),
            cookie_check: None,
            cookie_checking: false,
            cookie_check_gen: 0,
            dl_limit_mb_txt: String::new(),
            dl_limit_hours_txt: String::new(),
            speed_limit_kb_txt: String::new(),
            sel_profile: None,
            profile_name: String::new(),
            profile_kb: String::new(),
        }
    }
}

/// The Save-to tab's category editing. The draft list lives here, so the
/// rules that keep it usable — one General at the front, unique names, an
/// extension in one list only — live here with it.
impl OptionsState {
    /// Point the tab at `name`, folding the file-types editor back into the
    /// category it was opened on first.
    pub fn select_category(&mut self, name: String) {
        self.commit_cat_exts();
        self.sel_category = name;
        self.load_cat_editors();
    }

    /// Fill the name box and file-types editor from the selected category.
    fn load_cat_editors(&mut self) {
        let exts = self
            .draft_cats
            .iter()
            .find(|c| c.name == self.sel_category)
            .map(|c| c.exts.join(" ").to_uppercase())
            .unwrap_or_default();
        self.cat_name = self.sel_category.clone();
        self.cat_exts_edit = iced::widget::text_editor::Content::with_text(&exts);
    }

    /// Fold the file-types editor back into the draft category list.
    ///
    /// Called when the selection leaves the category and when OK commits,
    /// never per keystroke: an extension claimed here is taken away from the
    /// other categories, and doing that on every keystroke would delete `z`
    /// from Compressed before the user finished typing `zip`.
    ///
    /// General has no list of its own — it is what nothing else claimed — so
    /// its editor is read-only and there is nothing to fold back.
    pub fn commit_cat_exts(&mut self) {
        if self.sel_category == model::DEFAULT_CATEGORY {
            return;
        }
        let exts = model::parse_exts(&self.cat_exts_edit.text());
        model::set_category_exts(&mut self.draft_cats, &self.sel_category, exts);
    }

    /// Whether the name box holds a name a category could take: usable at
    /// all, and not one another category already has.
    ///
    /// Matched case-insensitively, because the name is also a folder name
    /// and Windows and macOS would hand "Programs" and "programs" the same
    /// directory.
    pub fn can_name_category(&self) -> bool {
        let (name, _) = model::parse_category_entry(&self.cat_name);
        model::valid_category_name(name)
            && !self
                .draft_cats
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Whether Rename may act. It takes a name, not a list: a box that also
    /// carries file types is a New, and renaming to the name half of it
    /// would quietly drop the types the user typed.
    pub fn can_rename_category(&self) -> bool {
        self.cat_is_removable()
            && self.can_name_category()
            && model::parse_category_entry(&self.cat_name).1.is_empty()
    }

    /// Whether the selected category's file types are the user's to edit.
    ///
    /// General's are not: it is the catch-all for what no other list claims,
    /// so it is described rather than edited.
    pub fn cat_types_editable(&self) -> bool {
        self.sel_category != model::DEFAULT_CATEGORY
    }

    /// Whether the selected category may be renamed or removed — only the
    /// ones the user made. The stock categories are named by the tree icons,
    /// by `categorize`'s fallback and by the AI seeding, so their identity
    /// is not the user's to change; their file types and folder still are.
    pub fn cat_is_removable(&self) -> bool {
        self.draft_cats
            .iter()
            .find(|c| c.name == self.sel_category)
            .is_some_and(|c| !c.builtin)
    }

    /// Create the category the box describes — `Pictures: .png .jpg` names
    /// its file types along with it — and select it.
    pub fn add_category(&mut self) {
        if !self.can_name_category() {
            return;
        }
        let (name, exts) = model::parse_category_entry(&self.cat_name);
        let name = name.to_string();
        self.commit_cat_exts();
        self.draft_cats.push(model::CategoryDef::new(&name));
        model::set_category_exts(&mut self.draft_cats, &name, exts);
        self.sel_category = name;
        self.load_cat_editors();
    }

    /// Rename the selected category to the name in the box, remembering the
    /// old name so the downloads filed under it follow when OK commits.
    pub fn rename_category(&mut self) {
        if !self.can_rename_category() {
            return;
        }
        self.commit_cat_exts();
        let new = model::parse_category_entry(&self.cat_name).0.to_string();
        let old = std::mem::replace(&mut self.sel_category, new);
        if let Some(c) = self.draft_cats.iter_mut().find(|c| c.name == old) {
            c.name = self.sel_category.clone();
        }
        record_rename(&mut self.cat_renames, &old, &self.sel_category);
        self.load_cat_editors();
    }

    /// Delete the selected category and fall back to General.
    pub fn remove_category(&mut self) {
        if !self.cat_is_removable() {
            return;
        }
        // Deliberately no `commit_cat_exts` first: the editor holds the list
        // of the category being deleted, and folding it back in would take
        // those extensions off the other categories on the way out.
        self.draft_cats.retain(|c| c.name != self.sel_category);
        self.sel_category = model::DEFAULT_CATEGORY.into();
        self.load_cat_editors();
    }
}

/// State of the "Zip preview" window (`WinKind::ZipPreview`): the archive's
/// listing, or why there is none. One slot, like the File Info dialog it is
/// opened from, and it belongs to that dialog's download.
#[derive(Clone, Debug, Default)]
pub struct ZipPreviewState {
    pub dl: DlId,
    /// The archive's name, for the heading.
    pub file_name: String,
    pub result: ZipPeek,
}

#[derive(Clone, Debug, Default)]
pub enum ZipPeek {
    /// The tail is being fetched.
    #[default]
    Loading,
    Listed(Vec<hya_net::zipdir::Entry>),
    /// A translated sentence.
    Failed(String),
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
    /// Bumped by every Update Now and every cancel, so events from a run
    /// the user has since called off are dropped instead of exiting the app.
    pub generation: u64,
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

#[derive(Debug)]
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
    /// URLs a probe has already been launched for, answered or not.
    ///
    /// The list is re-read after every edit, so without this a second paste
    /// into a box that already holds fifty links would measure all fifty
    /// again — and an unanswerable link would be retried on every keystroke
    /// that follows it.
    pub probing: std::collections::HashSet<String>,
    /// Counts edits to the URL box so a probe pass can tell whether it is
    /// still the most recent one. See [`App::update`]'s `BatchProbeIdle`.
    pub edit_gen: u64,
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
    /// Mirror lists among the pasted links, by URL.
    ///
    /// A Metalink in a batch is not one download but a list of them, so it is
    /// read in the background alongside the size probes and expanded when OK is
    /// pressed. Reading it at OK instead would either block the dialog on the
    /// network or add the document itself as a 6 KB file named after the object
    /// the user wanted.
    pub metalinks: std::collections::HashMap<String, crate::engine::MetalinkProbe>,
    /// Column the table is ordered by and whether ascending; `None` keeps
    /// the pasted order.
    pub sort: Option<(BatchSortKey, bool)>,
    /// Drop links that resolve to web pages (`.html`, `.php`, …): what a
    /// page-wide "download all links" mostly turns up.
    pub hide_html: bool,
    /// Show each distinct URL once, however often it was pasted.
    pub hide_dups: bool,
    /// Rows highlighted for "Check Selected", as indices into `checks`.
    ///
    /// The same identity the checkboxes use, so sorting or hiding rows can
    /// never move a highlight onto another link.
    pub sel: std::collections::HashSet<usize>,
    /// Where a Shift-click measures its run from. Held across clicks, so
    /// shrinking a range by Shift-clicking nearer the anchor works.
    pub sel_anchor: Option<usize>,
}

impl Default for BatchState {
    fn default() -> Self {
        Self {
            text: Default::default(),
            checks: Vec::new(),
            sizes: Default::default(),
            names: Default::default(),
            probing: Default::default(),
            edit_gen: 0,
            parsed: false,
            to_category: false,
            category: String::new(),
            to_dir: false,
            dir: String::new(),
            stream_quality: BatchQuality::default(),
            stream_container: String::new(),
            metalinks: Default::default(),
            sort: None,
            sel: Default::default(),
            sel_anchor: None,
            hide_html: false,
            // A duplicate never adds a second download, so hiding it is the
            // sensible default — the same one  ships with.
            hide_dups: true,
        }
    }
}

impl BatchState {
    /// The links in the box that nothing has gone out to measure yet, marked
    /// as measured on the way out.
    ///
    /// Every URL is handed back exactly once for the life of the dialog,
    /// however often the box is edited afterwards and however often the same
    /// link appears in it.
    pub fn take_unprobed(&mut self) -> Vec<String> {
        let probing = &mut self.probing;
        self.checks
            .iter()
            .filter(|(u, _)| probing.insert(u.clone()))
            .map(|(u, _)| u.clone())
            .collect()
    }
}

/// A sortable column of the batch dialog's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchSortKey {
    Name,
    Kind,
    Size,
    Source,
    Dest,
}

/// One line of the batch dialog's table, resolved: name and size from the
/// probe where it has answered, destination from the Save To choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchRow {
    /// Index into `BatchState::checks` — the row's identity across filtering
    /// and sorting, so a checkbox toggles the link it shows.
    pub idx: usize,
    pub url: String,
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
    pub save_to: String,
    pub blocked: bool,
    pub checked: bool,
}

/// The batch table as shown: hidden duplicates and web pages left out, then
/// ordered by the chosen column. `save_to` maps a resolved file name to the
/// folder it would land in; `blocked` says whether a URL's host is on the
/// "don't start automatically" list. Both are passed in so the view, the OK
/// handler and the tests all see the same rows.
pub fn batch_rows(
    st: &BatchState,
    save_to: impl Fn(&str) -> String,
    blocked: impl Fn(&str) -> bool,
) -> Vec<BatchRow> {
    let mut seen = std::collections::HashSet::new();
    let mut rows: Vec<BatchRow> = st
        .checks
        .iter()
        .enumerate()
        .filter_map(|(idx, (url, on))| {
            if st.hide_dups && !seen.insert(url.as_str()) {
                return None;
            }
            // The probe's answer where there is one: a redirector link
            // (`href.li/?<url>`) has no filename of its own and would
            // otherwise list — and save — as `index.html`.
            let name = st
                .names
                .get(url)
                .cloned()
                .unwrap_or_else(|| engine::file_name_from_url(url));
            if st.hide_html && crate::ext_info::is_web_page(&name) {
                return None;
            }
            let kind = if st.metalinks.contains_key(url) {
                i18n::tr("Metalink mirror list")
            } else {
                crate::ext_info::kind_for_name(&name)
            };
            Some(BatchRow {
                idx,
                save_to: save_to(&name),
                blocked: blocked(url),
                size: st.sizes.get(url).copied(),
                url: url.clone(),
                name,
                kind,
                checked: *on,
            })
        })
        .collect();
    if let Some((key, asc)) = st.sort {
        // Stable, so ties keep the pasted order. Unknown sizes stay at the
        // bottom whichever way the column is sorted — "biggest first" must
        // not put the files nobody has measured yet on top.
        rows.sort_by(|a, b| {
            let flip = |o: std::cmp::Ordering| if asc { o } else { o.reverse() };
            match key {
                BatchSortKey::Name => flip(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                BatchSortKey::Kind => flip(a.kind.cmp(&b.kind)),
                BatchSortKey::Size => match (a.size, b.size) {
                    (None, None) => std::cmp::Ordering::Equal,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (Some(x), Some(y)) => flip(x.cmp(&y)),
                },
                BatchSortKey::Source => flip(a.url.cmp(&b.url)),
                BatchSortKey::Dest => flip(a.save_to.cmp(&b.save_to)),
            }
        });
    }
    rows
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
        /// The URL waiting on the decision; travels with the question so a
        /// second confirmation queued behind this one cannot lose it.
        pending: Box<PendingAdd>,
    },
    /// Startup check could not write into the download folder.
    PermissionWarn {
        dir: String,
    },
    /// Help > Check for updates found nothing newer (info box, OK).
    UpToDate,
    /// View > Language moved the interface onto a different face than the
    /// running renderer was built with (info box, OK).
    FontNeedsRestart,
    /// Help > Check for updates could not reach the release server.
    UpdateCheckFailed(String),
    /// Move/Rename could not move the finished file (info box, OK); carries
    /// what the filesystem said.
    MoveFailed(String),
    /// "Warn me before stopping downloads" (Connection tab): the stop only
    /// happens once the user confirms. `stop_queues` carries the Pause All /
    /// Stop All variant, which also halts queue processing.
    StopWarn {
        ids: Vec<DlId>,
        stop_queues: bool,
    },
    /// File > Import settings replaced the configuration (info box, OK).
    SettingsImported,
    /// File > Export settings could not write the file; carries why.
    SettingsExportFailed(String),
    /// File > Import settings refused the file; carries why.
    SettingsImportFailed(String),
    /// An extension with no token to show keeps knocking (see
    /// `extbus::TrustLedger`); Yes admits its origin from then on.
    TrustExtension(String),
}

#[derive(Clone, Debug)]
pub enum Message {
    Plugin(crate::plugins::Message),
    InstallPluginFile(std::path::PathBuf),
    InstallPluginSource(String),
    PluginBrowse(String, usize),
    PluginFilePicked(String, Option<std::path::PathBuf>),
    TransferFileSelected(u32, bool),
    PluginProbeFinished(
        String,
        std::sync::Arc<hya_plugin::runtime::CallCtl>,
        Box<Result<Option<crate::plugins::PlanInfo>, String>>,
    ),
    PluginProbe,
    PluginAudioOnly(bool),
    PluginAudioFormat(String),
    PluginMaxHeight(Option<u32>),
    PluginPlaylistEntry(String, bool),
    PluginProbed(
        String,
        Box<Result<Option<crate::plugins::PlanInfo>, String>>,
    ),
    PluginTrack(hya_plugin_api::TrackKind, String),
    PluginSubtitle(String, bool),
    PluginContainer(String),
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
    /// One second of the "when done" power-action countdown. Subscribed only
    /// while that countdown is on screen, and at a fixed 1 s whatever power
    /// save does to the main tick — the dialog shows the number.
    PowerTick,
    /// Run the pending power action without waiting the countdown out.
    PowerNow,
    /// Call the pending power action off. The machine stays as it is.
    PowerCancel,
    /// The OS switched between light and dark appearance (or the startup
    /// query answered). Only View > Theme > System Default acts on it.
    SystemTheme(iced::theme::Mode),
    /// Main window chrome: a menu-bar menu opened.
    MenuOpen(MenuBarKind),
    /// Moving the pointer over a menu-bar title while another menu is already
    /// open: switches to that menu without closing the bar (Windows menu
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
    TableScrolled(f32, f32, f32),
    Mods(iced::keyboard::Modifiers),
    /// A key press no widget consumed, with the window it was aimed at —
    /// `close_window` has to know which one to shut.
    RawKey(iced::keyboard::Key, iced::keyboard::Modifiers, window::Id),
    SelectAll,
    RowEnter(DlId),
    RowExit(DlId),
    /// Left press on the empty ruled area below the last download.
    EmptyPress,
    /// A drag swept onto the empty ruled area below the last download.
    EmptyEnter,
    HeaderEnter(Column),
    HeaderExit(Column),
    QueueMenuOpen(bool),
    /// The Speed Limit split button's arrow: open the profile list.
    SpeedMenuOpen,
    ClipboardAddStart(Option<String>),
    ShortcutEdit(String, String),
    MouseUp,
    ColResizeStart(Column),
    /// Left press on a header cell: the start of either a sort (release
    /// without moving) or a drag that reorders the columns.
    HeaderPress(Column),
    /// Right press on a header cell: opens the column menu over it.
    HeaderRightClick(Column),
    SortBy(SortKey),
    SortDirection(bool),
    /// Show or hide one column, from the header menu or the manage dialog.
    ColToggle(Column),
    /// Move one column one place towards the given side. `true` is left.
    ColMove(Column, bool),
    /// Put the table's columns back to the stock order, widths and visibility.
    ColReset,
    ToolbarResume,
    ToolbarStop,
    ToolbarDelete,
    /// Add URL dialog: the address box changed.
    AddrChanged(String),
    AddrAuthToggled(bool),
    AddrLogin(String),
    AddrPass(String),
    /// The Cookies field, typed by hand.
    AddrCookies(String),
    /// Options > Connection names a browser: read that host's cookies out of
    /// it while the dialog is open, so pressing OK starts a download that
    /// already carries the session.
    AddrImportCookies,
    AddrCookiesImported(Box<Result<crate::engine::ImportedCookies, String>>),
    /// Whether Options' chosen browser can be read, answered off the executor,
    /// tagged with the generation that asked.
    OptCookieChecked(u64, Box<Result<String, String>>),
    AddUrlOk,
    /// Requests arriving from the browser extension over the extbus socket.
    Ext(crate::extbus::ExtEvent),
    /// File Info dialog: the category picker changed.
    FiCategory(String),
    /// TheSave As box: one full path, folder and file name.
    FiSaveAs(String),
    FiBgToggle(bool),
    FiBrowse,
    /// The Save As path chosen for download `.0`. Carries the download
    /// because the panel no longer freezes the app while it is up: a browser
    /// capture arriving in the meantime supersedes the File Info dialog, and
    /// the late answer must not land on the one that replaced it.
    FiPathPicked(DlId, String),
    FiDescription(String),
    FiRemember(bool),
    FiUrl(String),
    FiLogin(String),
    FiPass(String),
    FiCookies(String),
    FiProxyPick(ProxyPick),
    FiProxySpec(String),
    FiDownloadLater,
    FiStartDownload,
    /// Properties mode's OK: commit the fields and close, leaving the
    /// transfer in whatever state it was.
    FiOk,
    FiCancel,
    /// Preview: list the archive's contents without downloading it.
    FiPreview,
    ZipPeeked(DlId, Result<Vec<hya_net::zipdir::Entry>, String>),
    /// Progress dialog: a tab was picked.
    ProgTabSet(DlId, ProgTab),
    ProgToggleDetails(DlId),
    ProgPauseResume(DlId),
    ProgCancel(DlId),
    ProgHideTab(DlId, u8),
    ProgLimitOn(DlId, bool),
    ProgLimitKb(DlId, String),
    ProgLimitRemember(DlId, bool),
    ProgProxyPick(DlId, ProxyPick),
    ProgProxySpec(DlId, String),
    ProgShutdownAfter(DlId, bool),
    ProgShutdownAction(DlId, PowerAction),
    /// Options-on-completion tab mirror of Options > Downloads >
    /// "Remove completed downloads from the list": the setting is global,
    /// so toggling it here writes straight to the config.
    ProgRemoveCompleted(bool),
    /// The same tab's mirror of "Show download complete dialog".
    ProgShowCompleteDialog(bool),
    /// Skip the virus scan running over the finished file.
    ProgScanSkip(DlId),
    /// Infected (or unscannable): keep the file, close the dialog.
    ProgScanKeep(DlId),
    /// Infected (or unscannable): take the file off the list and the disk.
    ProgScanDelete(DlId),
    /// Console line / verdict from the virus scanner.
    Scan(crate::scan::ScanEvent),
    /// Complete dialog: open the finished file.
    OpenFile(DlId),
    OpenFolder(DlId),
    MoveRename(DlId),
    /// Options dialog: a tab was picked.
    OptTabSet(OptTab),
    /// Extensions page: open a browser's add-on store in the default browser.
    OptExtStore(&'static str),
    /// Put a value the page only displays (the ffmpeg path) on the clipboard.
    OptCopy(String),
    // Add URL: stream inspection
    /// The address looks like a manifest; go and read what it offers.
    AddrProbeStream,
    AddrStreamProbed(Box<Result<crate::engine::StreamProbe, String>>),
    /// The address looks like a Metalink document; go and read what it offers.
    AddrProbeMetalink,
    AddrMetalinkProbed(Box<Result<crate::engine::MetalinkProbe, String>>),
    AddrQuality(crate::engine::StreamQuality),
    AddrContainer(String),
    AddrRecordMinutes(String),
    OptOk,
    OptDraft(OptField),
    /// Update dialog. Startup (or manual) check finished: newer release / up to date / error.
    UpdateChecked(Result<Option<crate::update::UpdateInfo>, String>),
    UpdateNow,
    UpdateCancel,
    UpdateOpenPage,
    /// Open a link out of the release notes (the "Full Changelog" compare
    /// URL) in the browser.
    UpdateOpenUrl(String),
    /// Progress of a running update, streamed from `update::run`, tagged
    /// with the generation that started it so a cancelled run's tail is
    /// ignored.
    UpdateEvent(u64, crate::update::UpdateEvent),
    /// Scheduler dialog: the queue picker changed.
    SchQueue(String),
    SchNameEdit,
    SchNameDraft(String),
    SchNameCommit,
    SchTabSet(SchTab),
    SchField(SchField),
    SchStartNow,
    SchStop,
    SchSave,
    SchNewQueue,
    SchDeleteQueue,
    SchDragStart(DlId),
    SchDragOver(DlId),
    SchFileUp,
    SchFileDown,
    SchFileRemove,
    /// Batch dialog: the list editor changed.
    BatchEdit(iced::widget::text_editor::Action),
    BatchLoaded(Option<String>),
    /// The URL box has stood still since edit `.0` — measure what it holds.
    BatchProbeIdle(u64),
    BatchProbed(String, Option<engine::LinkMeta>),
    /// A pasted link turned out to be a Metalink document.
    BatchMetalinkProbed(String, Box<Option<engine::MetalinkProbe>>),
    /// A File Info dialog's own probe answered: the real name and size of a
    /// link that is not being downloaded yet.
    InfoProbed(DlId, Option<engine::LinkMeta>),
    BatchCheck(usize, bool),
    BatchCheckAll(bool),
    /// A click anywhere on a batch row but its checkbox: selects it, and
    /// extends or toggles the selection under Shift / Cmd-Ctrl.
    BatchRowClick(usize),
    /// Check or uncheck every highlighted row.
    BatchCheckSel(bool),
    BatchSaveMode(u8),
    BatchCategory(String),
    BatchDir(String),
    BatchOk,
    BatchStreamQuality(BatchQuality),
    BatchStreamContainer(String),
    BatchSort(BatchSortKey),
    BatchHideHtml(bool),
    BatchHideDups(bool),
    BatchBrowseDir,
    BatchDirPicked(String),
    /// Confirm dialog: Yes.
    ConfirmYes,
    ConfirmRemoveFile(bool),
    AddrPrefill(Option<String>),
    OpenPermissions,
    PermOpenPane(&'static str),
    PermRefresh,
    DupResume,
    DupOpen,
    DupNew,
    /// Where the "Move/Rename..." panel said the finished file should go.
    MoveRenameTo(DlId, std::path::PathBuf),
    /// The folder the user chose after a download failed for want of write
    /// permission on the one it had.
    SaveDirRegranted(DlId, std::path::PathBuf),
    /// Tasks > Export download URLs: where the picker said the list should go.
    ExportUrlsTo(Option<std::path::PathBuf>),
    /// File > Export settings: where the picker said the file should go.
    ExportSettingsTo(Option<std::path::PathBuf>),
    /// File > Import settings: the file the picker chose.
    ImportSettingsFrom(Option<std::path::PathBuf>),
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
    /// Only offered by a `--config DIR` instance (Options > Extensions).
    PortableCapture(bool),
    /// Remove a trusted extension origin, by index.
    Untrust(usize),
    AutoTypesEdit(iced::widget::text_editor::Action),
    SitesEdit(iced::widget::text_editor::Action),
    RememberLast(bool),
    ServerDate(bool),
    NoCatDirs(bool),
    ShowFileInfo(bool),
    BgDownload(bool),
    StartMinimized(bool),
    SpeedTab(bool),
    CompletionTab(bool),
    HideButtons(bool),
    ConnDetails(bool),
    CompleteDialog(bool),
    RemoveCompleted(bool),
    UserAgent(String),
    VirusScanner(String),
    VirusArgs(String),
    BrowseVirus,
    VirusPicked(Option<String>),
    DefaultConns(usize),
    AdaptiveConns(bool),
    /// Which browser downloads added by hand take their cookies from, as the
    /// picker's label. The empty label is "do not".
    CookiesBrowser(String),
    /// The profile within that browser, blank for its default.
    CookiesProfile(String),
    ExcSel(usize),
    ExcServer(String),
    ExcConns(String),
    ExcAdd,
    ExcRemove,
    DlLimit(bool),
    DlLimitMb(String),
    DlLimitHours(String),
    SpeedLimiter(bool),
    SpeedLimitKb(String),
    ProfileSel(usize),
    ProfileName(String),
    ProfileKb(String),
    ProfileAdd,
    ProfileRemove,
    WarnStop(bool),
    ProxyMode(ProxyMode),
    ProxyScript(String),
    ProxyHost(String),
    ProxyPort(String),
    ProxyUser(String),
    ProxyPass(String),
    ProxyType(crate::model::ProxyType),
    SelCategory(String),
    CatDir(String),
    BrowseCatDir,
    CatDirPicked(Option<String>),
    CatExtsEdit(iced::widget::text_editor::Action),
    CatName(String),
    CatAdd,
    CatRename,
    CatRemove,
    LoginSel(usize),
    LoginSite(String),
    LoginUser(String),
    LoginPass(String),
    LoginAdd,
    LoginRemove,
    Sound(usize, bool),
    SoundBrowse(usize),
    SoundPicked(usize, String),
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
    /// (column, grab x, width at grab) while a header edge is being dragged.
    /// The width itself lives in `cfg.settings.columns` and is written there
    /// as the pointer moves; the config file is only saved once the drag ends.
    pub resizing: Option<(Column, f32, f32)>,
    /// A header cell is held down: the column, the pointer x the press is
    /// measured from, and whether it has since moved far enough to count as a
    /// reorder drag rather than a click on the title. A press that never
    /// moves sorts by the column on release.
    pub header_drag: Option<(Column, f32, bool)>,
    /// Column the header context menu was opened on, if it is showing.
    pub header_ctx: Option<Column>,
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
    pub zip_preview: ZipPreviewState,
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
    /// Confirmations raised while one is already on screen, oldest first.
    /// The window is one slot; a question must wait its turn, not overwrite
    /// the one the user is still reading.
    pub confirm_queue: std::collections::VecDeque<ConfirmKind>,
    /// Next capture-dialog window (File Info / Confirm / Batch) must be
    /// forced above the browser: at capture time this app is usually a
    /// background tray process, and a normal-level window opens behind the
    /// browser unfocused — the dialog floats on top.
    pub capture_raise: bool,
    /// Open split-button dropdown on the toolbar: Some(true)=Start queue,
    /// Some(false)=Stop queue.
    pub queue_menu: Option<bool>,
    /// The Speed Limit split button's profile dropdown is open.
    pub speed_menu: bool,
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
    pub hover_col: Option<Column>,
    /// Visible order snapshotted when a drag-selection starts. The sweep
    /// extends the selection on every row it crosses, and re-deriving the
    /// order there meant re-filtering and re-sorting the whole list per row.
    pub drag_order: Vec<DlId>,
    /// Scroll offset and viewport height last reported by the download
    /// table, so `ui::table` can build only the rows actually on screen.
    pub table_scroll: f32,
    /// How far the list is scrolled sideways. The ruled empty grid below the
    /// last download is drawn outside the scrollable (see `ui::table`), so it
    /// has to shift its column hairlines by hand.
    pub table_scroll_x: f32,
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
    /// The primary display in OS points ([`display_points`]), or
    /// [`iced::Size::ZERO`] when the platform would not say. Dialogs are
    /// held inside it so none opens with its buttons off the screen.
    pub display: iced::Size,
    /// Progress boxes opened by *starting* a download while "Start download
    /// progress dialog minimized" is on. They can only be minimized once
    /// they exist, so [`Message::WindowOpened`] does it and clears the id.
    pub minimize_on_open: std::collections::HashSet<window::Id>,
    /// A "when done" power action waiting out its countdown, if any. Set by
    /// [`App::arm_power_action`] and cleared when the countdown fires or is
    /// cancelled; its presence is what puts the 1 s [`Message::PowerTick`]
    /// on the subscription list.
    pub power: Option<PowerPrompt>,
}

/// The pending "when done" power action behind [`WinKind::Power`].
#[derive(Clone, Copy, Debug)]
pub struct PowerPrompt {
    pub action: PowerAction,
    /// Seconds left. Counts down to zero, then the action runs.
    pub secs: u8,
    /// Quit Hydra when the countdown ends even if the action leaves the
    /// session up — a queue's "Exit Hydra when done", which is set
    /// independently of the power action and outlives cancelling it.
    pub exit_after: bool,
}

/// How long the cancel window lasts. Long enough to catch the dialog on the
/// way past, short enough not to sit in front of a finished queue.
pub const POWER_COUNTDOWN_SECS: u8 = 10;

/// The connection count Options > Connection sets aside for `host`, if any.
///
/// A `*.` prefix is accepted and means the same thing the bare suffix does; the
/// match is on the suffix either way, so `example.com` also covers
/// `cdn.example.com`. FIRST match wins — see [`upsert_exception`] for why that
/// makes one entry per server an invariant rather than a nicety.
fn exception_for(list: &[(String, usize)], host: &str) -> Option<usize> {
    list.iter()
        .find(|(server, _)| !server.is_empty() && host.ends_with(server.trim_start_matches("*.")))
        .map(|(_, n)| *n)
}

/// Set `server`'s exception to `n`, replacing any entry it already has.
///
/// Appending instead would leave the older entry in front of the newer one, and
/// [`exception_for`] reads the first: the number the user just typed would be
/// stored, displayed, and never used.
pub(crate) fn upsert_exception(list: &mut Vec<(String, usize)>, server: String, n: usize) {
    match list.iter_mut().find(|(s, _)| *s == server) {
        Some(row) => row.1 = n,
        None => list.push((server, n)),
    }
}

/// Retune `name`'s profile, or add it when the list has no such name.
///
/// Names are what the menus address a profile by ([`MenuAction::SpeedProfile`]),
/// so a second profile under a name already taken would be unreachable — the
/// menu entry would always resolve to the first.
pub(crate) fn upsert_profile(
    list: &mut Vec<crate::model::SpeedProfile>,
    name: String,
    limit: Option<u64>,
) {
    match list.iter_mut().find(|p| p.name == name) {
        Some(row) => row.limit = limit,
        None => list.push(crate::model::SpeedProfile { name, limit }),
    }
}

/// Store `login`, replacing the entry its site already has.
///
/// `find_login` takes the first match, so a second row for the same site
/// would be one the user can see, edit and never use.
pub(crate) fn upsert_login(list: &mut Vec<SiteLogin>, login: SiteLogin) {
    match list.iter_mut().find(|l| l.site == login.site) {
        Some(row) => *row = login,
        None => list.push(login),
    }
}

/// Note that the category `old` was renamed to `new`.
///
/// Renaming the same category twice in one visit to Options must still map
/// back to the name the downloads were actually filed under, so a second
/// rename retargets the existing entry rather than adding a chain nobody
/// follows.
fn record_rename(renames: &mut HashMap<String, String>, old: &str, new: &str) {
    let mut chained = false;
    for v in renames.values_mut() {
        if v == old {
            *v = new.to_string();
            chained = true;
        }
    }
    if !chained {
        renames.insert(old.to_string(), new.to_string());
    }
}

/// Follow the category edits into the download list: an item filed under a
/// renamed category keeps its grouping, and one whose category was deleted
/// goes back to uncategorised rather than staying filed under a name no tree
/// node shows any more. Files on disk are left where they are — only the
/// grouping moves. Returns whether anything did.
fn refile_downloads(
    downloads: &mut [DownloadItem],
    renames: &HashMap<String, String>,
    cats: &[CategoryDef],
) -> bool {
    let mut moved = false;
    for d in downloads {
        let Some(cat) = d.category.as_mut() else {
            continue;
        };
        if let Some(new) = renames.get(cat.as_str()) {
            *cat = new.clone();
            moved = true;
        }
        if !cats.iter().any(|c| c.name == *cat) {
            d.category = None;
            moved = true;
        }
    }
    moved
}

/// Where a tree selection points after the same edits.
///
/// A selection on a renamed category moves with it; one whose category is
/// gone falls back to the parent node, because the node it names is no
/// longer drawn — the list would be empty with nothing to click back to.
fn follow_tree_sel(
    sel: &TreeSel,
    renames: &HashMap<String, String>,
    cats: &[CategoryDef],
) -> TreeSel {
    let follow = |c: &String| -> Option<String> {
        let name = renames.get(c.as_str()).unwrap_or(c);
        cats.iter().any(|k| k.name == *name).then(|| name.clone())
    };
    match sel {
        TreeSel::Cat(c) => follow(c).map_or(TreeSel::All, TreeSel::Cat),
        TreeSel::UnfCat(c) => follow(c).map_or(TreeSel::Unfinished, TreeSel::UnfCat),
        TreeSel::FinCat(c) => follow(c).map_or(TreeSel::Finished, TreeSel::FinCat),
        other => other.clone(),
    }
}

/// A freshly started app with nothing loaded: no windows, no selection, no
/// download list. `boot` overrides the handful of fields it reads off disk or
/// asks the system for, and the rest of this struct is exactly what it used
/// to spell out field by field.
impl Default for App {
    fn default() -> Self {
        App {
            cfg: ConfigFile::default(),
            state: StateFile::default(),
            windows: HashMap::new(),
            main_id: None,
            selected: vec![],
            sel_anchor: None,
            mods: iced::keyboard::Modifiers::default(),
            resizing: None,
            header_drag: None,
            header_ctx: None,
            tree_sel: TreeSel::All,
            // The download list open, the three folders below it closed.
            tree_open: [true, false, false, false],
            renaming_queue: None,
            queue_rename_draft: String::new(),
            open_menu: None,
            open_submenu: None,
            cursor: Point::ORIGIN,
            ctx_at: None,
            last_click: None,
            last_queue_click: None,
            sort: (SortKey::Column(Column::LastTry), false),
            add_url: AddUrlState::default(),
            file_info: FileInfoState::default(),
            zip_preview: ZipPreviewState::default(),
            prog: HashMap::new(),
            scans: HashMap::new(),
            options: OptionsState::default(),
            updater: UpdateUiState::default(),
            sch: SchState::default(),
            batch: BatchState::default(),
            confirm: None,
            confirm_remove_file: false,
            pending_delete: vec![],
            state_dirty: false,
            cfg_dirty: false,
            quota_saved: (0, 0),
            last_clipboard: String::new(),
            confirm_queue: std::collections::VecDeque::new(),
            capture_raise: false,
            queue_menu: None,
            speed_menu: false,
            list_press: None,
            list_drag: false,
            band: None,
            list_drag_from_empty: false,
            hover_row: None,
            hover_col: None,
            drag_order: Vec::new(),
            table_scroll: 0.0,
            table_scroll_x: 0.0,
            table_vh: 0.0,
            cursor_cell: std::sync::Arc::new(crate::ui::probe::CursorCell::default()),
            perm_status: crate::windows::permissions::PermStatus::default(),
            system_dark: false,
            main_pos: None,
            main_size: iced::Size::new(0.0, 0.0),
            display: iced::Size::ZERO,
            minimize_on_open: std::collections::HashSet::new(),
            power: None,
        }
    }
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

    /// The batch dialog's table, filtered and ordered as shown.
    pub fn batch_rows(&self) -> Vec<BatchRow> {
        let st = &self.batch;
        batch_rows(
            st,
            |name| {
                if st.to_dir {
                    st.dir.clone()
                } else {
                    let cat = if st.to_category {
                        Some(st.category.clone())
                    } else {
                        categorize(name, &self.cfg.categories)
                    };
                    self.cat_dir(cat.as_deref())
                        .or_else(|| self.cat_dir(None))
                        .unwrap_or_default()
                }
            },
            |url| site_blocked(url, &self.cfg.settings.dont_start_sites),
        )
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
            self.ask(ConfirmKind::PermissionWarn { dir })
        }
    }

    /// Move a download's file or partial staging file, or give it another name,
    /// and keep the row pointing at where it went.
    ///
    /// An active transfer owns its open `.part` file and cannot be moved;
    /// stopped, paused or completed transfers can. The new name is pinned
    /// (`name_locked`) so later server probes do not undo it.
    ///
    /// `owner` is the window that asked: the complete dialog is often up while
    /// the main window is hidden in the tray, and a sheet cannot hang off that.
    fn move_rename(&mut self, id: DlId, owner: Option<window::Id>) -> Task<Message> {
        let Some(d) = self.item(id) else {
            return Task::none();
        };
        if d.state.is_active() {
            return Task::none();
        }
        if d.state == DlState::Complete && !d.full_path().is_file() {
            return Task::none();
        }
        let ask = Ask {
            file_name: Some(d.file_name.clone()),
            ..Ask::in_dir(&d.save_dir)
        };
        picker::save(owner, ask).and_then(move |to| Task::done(Message::MoveRenameTo(id, to)))
    }

    /// Carry out the move the "Move/Rename..." panel asked for.
    fn move_rename_to(&mut self, id: DlId, to: std::path::PathBuf) -> Task<Message> {
        let Some(d) = self.item(id) else {
            return Task::none();
        };
        if d.state.is_active() {
            return Task::none();
        }
        let from = d.full_path();
        if to == from {
            return Task::none();
        }
        let is_complete = d.state == DlState::Complete;
        let from_part = d.part_file();
        let dir = to.parent().map(|p| p.to_string_lossy().into_owned());
        let raw_name = to.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = raw_name
            .as_deref()
            .and_then(hya_net::filename::portable)
            .or(raw_name);
        let to_part = dir
            .as_deref()
            .zip(name.as_deref())
            .map(|(dir, name)| std::path::Path::new(dir).join(format!("{name}.part")));

        if is_complete {
            if let Err(e) = crate::files::move_file(&from, &to) {
                crate::log::warn(&format!("move {} -> {}: {e}", from.display(), to.display()));
                return self.ask(ConfirmKind::MoveFailed(e.to_string()));
            }
        } else if from_part.exists() {
            if let Some(to_part_path) = &to_part {
                if let Err(e) = crate::files::move_file(&from_part, to_part_path) {
                    crate::log::warn(&format!(
                        "move {} -> {}: {e}",
                        from_part.display(),
                        to_part_path.display()
                    ));
                    return self.ask(ConfirmKind::MoveFailed(e.to_string()));
                }
                move_stream_companions(&from_part, to_part_path);
            }
        } else if from.exists() {
            if let Err(e) = crate::files::move_file(&from, &to) {
                crate::log::warn(&format!("move {} -> {}: {e}", from.display(), to.display()));
                return self.ask(ConfirmKind::MoveFailed(e.to_string()));
            }
        }

        if let Some(d) = self.item_mut(id) {
            if let Some(dir) = dir {
                d.save_dir = dir;
            }
            if let Some(name) = name {
                d.set_file_name(&name);
                d.name_locked = true;
            }
            if !is_complete {
                if let Some(tp) = to_part {
                    d.part_path = Some(tp.to_string_lossy().into_owned());
                }
            }
        }
        self.save_state();
        Task::none()
    }

    /// Rebuild the native macOS menu bar so its check marks match state.
    #[cfg(target_os = "macos")]
    pub fn refresh_native_menu(&self) {
        let state = self.native_menu_state();
        let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
        crate::macos_menu::reinstall(
            &state,
            &queues,
            &self.cfg.settings.speed_profiles,
            &i18n::available(),
        );
    }

    /// Re-tick the menu bar for a setting it displays, without rebuilding it.
    ///
    /// muda ticks a check item on click, whatever the app then does with the
    /// activation, so the menu has to be set back to what the settings say —
    /// see `macos_menu::sync`. Only the language and the queue submenus need
    /// a rebuild; a toggle does not, and rebuilding for one used to leave
    /// View > Scale showing two percentages ticked at once.
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
            show_toolbar_labels: self.cfg.settings.show_toolbar_labels,
            ui_scale_pct: self.cfg.settings.ui_scale_pct,
            language: {
                let l = self.cfg.language.clone().unwrap_or_else(|| "en".into());
                if l == "English" {
                    "en".into()
                } else {
                    l
                }
            },
            speed_limiter: self.cfg.settings.speed_limiter_on,
            speed_profile: self.cfg.settings.active_profile(),
            sort: self.sort,
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

    /// Keep the dialog `id` above the main window (see `dialog_parent`).
    ///
    /// Not every window is the main window's: a progress box and a Download
    /// File Info dialog belong to their download, are opened by browser
    /// capture with no main window at all, and have to keep working after
    /// the main window closes to the tray — an owned window would be hidden
    /// with its owner. Those, the main window itself, and any dialog opened
    /// while the main window is closed stay top-level.
    fn attach_to_main(&self, id: window::Id) -> Task<Message> {
        let main = self.win_of(WinKind::Main);
        let parent = match self.windows.get(&id) {
            None | Some(WinKind::Main | WinKind::Progress(_) | WinKind::FileInfo(_)) => None,
            // The archive listing is the File Info dialog's own sub-dialog:
            // owned by it, it stays above it and hands focus back to it when
            // it closes. Owned by the main window instead, closing the
            // listing raised the main window over the dialog, which then
            // looked closed while its transfer carried on behind.
            Some(WinKind::ZipPreview(dl)) => self.win_of(WinKind::FileInfo(*dl)).or(main),
            Some(_) => main,
        };
        let Some(parent) = parent else {
            return Task::none();
        };
        window::run(parent, crate::dialog_parent::token).then(move |token| match token {
            Some(t) => {
                window::run(id, move |w| crate::dialog_parent::attach(w, t)).map(|()| Message::Noop)
            }
            None => Task::none(),
        })
    }

    /// The folder colour of the queue called `name`; `None` for the stock
    /// yellow, and for a name no queue carries any more.
    pub fn queue_color(&self, name: &str) -> Option<u32> {
        self.cfg
            .queues
            .iter()
            .find(|q| q.name == name)
            .and_then(|q| q.color)
    }

    /// Re-fits a dialog already open at `kind` to whatever [`Self::window_size`]
    /// now answers for it — a row that only shows up in some states (an
    /// error, a credentials row) otherwise has nowhere to go until the
    /// dialog is closed and reopened.
    fn resize_open(&self, kind: WinKind) -> Task<Message> {
        match self.win_of(kind) {
            Some(id) => {
                let (w, h) = self.window_size(kind);
                let resize = window::resize(id, iced::Size::new(w, h));
                if matches!(kind, WinKind::Progress(_)) {
                    Task::batch([
                        window::set_min_size(id, Some(iced::Size::new(w, h))),
                        resize,
                    ])
                } else {
                    resize
                }
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

    /// The main window in interface units — the ones the cursor probe reads
    /// and the overlays are laid out in, so the units a menu has to be kept
    /// inside.
    pub fn main_viewport(&self) -> iced::Size {
        main_viewport(
            self.main_size,
            self.window_size(WinKind::Main),
            self.ui_scale(),
        )
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
            SortKey::Column(Column::Name) => {
                sort_keyed(&mut v, asc, |d| d.file_name.to_lowercase())
            }
            SortKey::Column(Column::Status) => sort_keyed(&mut v, asc, DownloadItem::status_text),
            SortKey::OrderOfAddition => v.sort_by(|a, b| {
                let ord = a.added.cmp(&b.added).then_with(|| a.id.cmp(&b.id));
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            }),
            SortKey::Column(col) => v.sort_by(|a, b| {
                let ord = match col {
                    Column::Queue => a.queue.cmp(&b.queue).then(a.q_order.cmp(&b.q_order)),
                    Column::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
                    Column::TimeLeft => a
                        .eta_secs
                        .unwrap_or(u64::MAX)
                        .cmp(&b.eta_secs.unwrap_or(u64::MAX)),
                    Column::Rate => a
                        .rate
                        .partial_cmp(&b.rate)
                        .unwrap_or(std::cmp::Ordering::Equal),
                    Column::LastTry => {
                        let ta = a.last_try.unwrap_or(a.added);
                        let tb = b.last_try.unwrap_or(b.added);
                        ta.cmp(&tb)
                    }
                    Column::Description => a.description.cmp(&b.description),
                    Column::Name | Column::Status => std::cmp::Ordering::Equal,
                };
                let ord = ord.then_with(|| a.id.cmp(&b.id));
                if asc {
                    ord
                } else {
                    ord.reverse()
                }
            }),
        }
        v
    }

    /// One column's stored presentation, for the two things that change it:
    /// a width drag, and the show/hide toggle.
    fn column_mut(&mut self, col: Column) -> Option<&mut ColumnPref> {
        self.cfg.settings.columns.iter_mut().find(|p| p.id == col)
    }

    /// Carry a header drag to pointer x. A press only counts as a reorder
    /// once it has travelled; until then it is still a click on the title,
    /// which sorts the list on release.
    fn drag_header_to(&mut self, x: f32) {
        let Some((col, from_x, moved)) = self.header_drag else {
            return;
        };
        let moved = moved || (x - from_x).abs() > HEADER_DRAG_SLOP;
        let from_x = model::drag_column(&mut self.cfg.settings.columns, col, from_x, x);
        self.header_drag = Some((col, from_x, moved));
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
    /// cookies, the page the file was linked from, and the already-resolved
    /// filename. Must run before any start so the first request carries the
    /// `Cookie` and `Referer` headers — a hotlink-protected CDN answers
    /// `403` without the referer, whatever the cookies say. Renaming re-runs
    /// categorization because the URL-derived name may have had no extension
    /// at all.
    fn apply_capture_extras(&mut self, id: DlId, extras: CaptureExtras) {
        // Resolved before the item is borrowed mutably, and through
        // `cat_dir` — the one accessor that honours Options > Save to > "Do
        // not create category folders". Reading the category's own `dir`
        // here ignored that setting, so `add_item` filed the download in the
        // single General folder the user asked for and this overwrote it
        // with a per-category folder they had switched off.
        let filing = extras.name.as_deref().map(|n| {
            let cat = categorize(n, &self.cfg.categories);
            let dir = self.cat_dir(cat.as_deref());
            (cat, dir)
        });
        if let Some(d) = self.item_mut(id) {
            write_capture_extras(d, extras, filing);
        }
    }

    /// Open the Configuration window on a fresh draft of the saved settings.
    /// `tab` forces a page (the toolbar's Extensions shortcut); `None` keeps
    /// whichever page was last visited.
    fn open_options(&mut self, tab: Option<OptTab>) -> Task<Message> {
        self.options.draft = self.cfg.settings.clone();
        self.options.base = self.cfg.settings.clone();
        self.options.error = None;
        self.options.draft_cats = self.cfg.categories.clone();
        self.options.sel_category = model::DEFAULT_CATEGORY.into();
        self.options.cat_renames.clear();
        self.options.load_cat_editors();
        // Selections are indexes into lists this line has just replaced, so a
        // selection held from the previous visit points at whatever now happens
        // to sit at that position — and Remove would take that row instead.
        self.options.sel_exc = None;
        self.options.sel_login = None;
        self.options.sel_profile = None;
        self.options.profile_name.clear();
        self.options.profile_kb.clear();
        self.options.speed_limit_kb_txt = self
            .cfg
            .settings
            .global_speed_limit
            .map(|b| (b / 1024).to_string())
            .unwrap_or_default();
        self.options.dl_limit_mb_txt = self.cfg.settings.dl_limit_mb.to_string();
        self.options.dl_limit_hours_txt = self.cfg.settings.dl_limit_hours.to_string();
        self.options.auto_types_edit =
            iced::widget::text_editor::Content::with_text(&self.cfg.settings.auto_types);
        self.options.sites_edit =
            iced::widget::text_editor::Content::with_text(&self.cfg.settings.dont_start_sites);
        if let Some(t) = tab {
            self.options.tab = t;
        }
        // Answered on OPEN as well as on a change: a browser chosen in an
        // earlier session is the case where "can this still be read?" matters
        // most, because a profile that moved or a permission that was revoked
        // would otherwise only show up as failing downloads.
        Task::batch([
            self.open_window(WinKind::Options),
            self.check_cookie_source(),
            crate::plugins::load(),
        ])
    }

    /// Follow the category edits into the download list and the tree.
    fn apply_category_edits(&mut self) {
        let renames = &self.options.cat_renames;
        let moved = refile_downloads(&mut self.state.downloads, renames, &self.cfg.categories);
        self.tree_sel = follow_tree_sel(&self.tree_sel, renames, &self.cfg.categories);
        if moved {
            self.save_state();
        }
    }

    /// Carry a settings change out to what is already running: the
    /// transfers, the menus, the proxy, the OS integrations and the open
    /// progress boxes. `before` is the settings as they were.
    fn settings_changed(&mut self, before: &Settings) -> Task<Message> {
        let details = self.cfg.settings.show_conn_details;
        let details_changed = details != before.show_conn_details;
        let capture_changed = self.cfg.settings.portable_capture != before.portable_capture;
        if self.cfg.settings.power_save != before.power_save {
            self.set_power_save(self.cfg.settings.power_save);
        }
        // A tab switched off here has nothing to show in a progress
        // box already sitting on it.
        for p in self.prog.values_mut() {
            let gone = (p.tab == ProgTab::Speed && !self.cfg.settings.show_speed_tab)
                || (p.tab == ProgTab::Completion && !self.cfg.settings.show_completion_tab);
            if gone {
                p.tab = ProgTab::Status;
            }
        }
        // The Speed Limiter is editable here as well as from the
        // toolbar, and the transfers it applies to are running while
        // this window is open.
        self.apply_speed_limiter();
        // The profile list feeds the native menu's Speed limit
        // submenu, so a renamed or deleted profile has to rebuild it.
        self.refresh_native_menu();
        // A portable copy taking browser capture over (or handing it
        // back) does so now rather than at the next start: the
        // manifests and the pointer file are all it takes, and this
        // instance is already publishing the socket they lead to.
        if capture_changed {
            crate::nmhost::ensure_registered(self.cfg.settings.portable_capture);
        }
        // Re-resolve the proxy here rather than at the next transfer:
        // a route the app cannot take must be reported while the user
        // is still looking at the tab they set it on.
        crate::proxy::apply(&self.cfg.settings);
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
        // A progress box keeps its own Show/Hide details state for
        // the session, so without this the new default would not
        // reach a download whose box has already been opened once —
        // the very boxes the person changing it is looking at.
        let details_task = if details_changed {
            for p in self.prog.values_mut() {
                p.details = details;
            }
            Task::batch(
                self.prog
                    .keys()
                    .map(|id| self.resize_open(WinKind::Progress(*id)))
                    .collect::<Vec<_>>(),
            )
        } else {
            Task::none()
        };
        Task::batch([skip_taskbar, details_task])
    }

    fn export_settings(&mut self, path: &std::path::Path) -> Task<Message> {
        let written = hydata::encode(&self.cfg, dirs::home_dir().as_deref())
            .and_then(|bytes| std::fs::write(path, bytes).map_err(|e| e.to_string()));
        match written {
            Ok(()) => Task::none(),
            Err(e) => {
                crate::log::warn(&format!("export settings {}: {e}", path.display()));
                self.ask(ConfirmKind::SettingsExportFailed(e))
            }
        }
    }

    /// Replace the configuration with the one in `path` and apply it as
    /// Options > OK would. A file that fails to decode changes nothing.
    fn import_settings(&mut self, path: &std::path::Path) -> Task<Message> {
        let decoded = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| hydata::decode(&bytes, dirs::home_dir().as_deref()));
        let cfg = match decoded {
            Ok(cfg) => cfg,
            Err(e) => {
                crate::log::warn(&format!("import settings {}: {e}", path.display()));
                return self.ask(ConfirmKind::SettingsImportFailed(e));
            }
        };
        crate::log::info(&format!("imported settings from {}", path.display()));
        let (before, language) = self.adopt_config(cfg);
        let applied = self.settings_changed(&before.settings);
        let relabelled = match language {
            Some(l) => self.on_menu(MenuAction::Language(l)),
            None => {
                let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                crate::tray::reinstall(&queues, self.cfg.settings.power_save);
                Task::none()
            }
        };
        // Its draft was taken from the configuration just replaced.
        let options = self.close_window(WinKind::Options);
        let done = self.ask(ConfirmKind::SettingsImported);
        Task::batch([applied, relabelled, options, done])
    }

    /// Swap `cfg` in for the configuration in force, keeping what an export
    /// leaves out, and refile the downloads onto its categories. Returns the
    /// configuration replaced and the language still to switch to: the
    /// Language action compares against the one in force, so that stays
    /// until it runs.
    fn adopt_config(&mut self, mut cfg: ConfigFile) -> (ConfigFile, Option<String>) {
        hydata::keep_local(&mut cfg, &self.cfg);
        let language = cfg
            .language
            .take()
            .filter(|l| Some(l) != self.cfg.language.as_ref());
        cfg.language = self.cfg.language.clone();
        let before = std::mem::replace(&mut self.cfg, cfg);
        let no_renames = HashMap::new();
        if refile_downloads(&mut self.state.downloads, &no_renames, &self.cfg.categories) {
            self.save_state();
        }
        self.tree_sel = follow_tree_sel(&self.tree_sel, &no_renames, &self.cfg.categories);
        self.save_config();
        (before, language)
    }

    /// The View > Scale ratio the windows are laid out at.
    fn ui_scale(&self) -> f32 {
        crate::theme::ui_scale(self.cfg.settings.ui_scale_pct)
    }

    pub(crate) fn address_loading(&self) -> bool {
        self.add_url.plugin_probing
            || self.add_url.stream_probing
            || self.add_url.metalink_probing
            || self.add_url.cookies_importing
    }

    /// The size a window opens at, in interface units — the units
    /// `window::open` and `window::resize` speak.
    ///
    /// Dialogs are laid out against `theme::FONT_SIZE`, and View > Scale
    /// scales the interface by the ratio to it (`scale_of` in main.rs). iced
    /// applies that ratio to the size handed to `window::open` itself, so
    /// the constants below are written once, at the ratio the layout was
    /// drawn at, and must *not* be scaled again here: doing so squares the
    /// ratio and opens a Large-font dialog a third bigger than the screen
    /// space its content asks for. The main window is the exception: it is
    /// resizable and reopens at whatever size it was left at, and that is
    /// remembered in OS points, so it converts back.
    pub(crate) fn animation_interval_ms(&self) -> u64 {
        if self.windows.values().any(|window| match window {
            WinKind::Progress(id) => self.item(*id).is_some_and(|item| {
                item.state.is_active()
                    && item
                        .plugin_plan
                        .as_ref()
                        .is_some_and(|info| info.plan.transfer.is_some())
            }),
            _ => false,
        }) {
            33
        } else {
            80
        }
    }

    fn window_size(&self, kind: WinKind) -> (f32, f32) {
        let scale = self.ui_scale();
        if kind == WinKind::Main {
            return main_open_size(self.cfg.settings.window_size, scale);
        }
        let (w, h) = match kind {
            WinKind::Main => unreachable!("handled above"),
            WinKind::AddUrl => {
                let mut h: f32 = 156.0;
                h = h.max(30.0 + (crate::plugins::input_actions(self).len() + 2) as f32 * 38.0);
                // The cookie note is a real row, and a store path wraps to two
                // lines at this width on every platform; one line while probing.
                if self.add_url.cookies_importing {
                    h += 22.0;
                } else if self.add_url.cookie_note.is_some() {
                    h += 44.0;
                }
                let warn = self.add_url.error.is_some()
                    || self.add_url.stream_error.is_some()
                    || self.add_url.metalink_error.is_some()
                    || (!self.add_url.address.trim().is_empty()
                        && site_blocked(
                            self.add_url.address.trim(),
                            &self.cfg.settings.dont_start_sites,
                        ));
                if warn {
                    h += self
                        .add_url
                        .error
                        .as_ref()
                        .or(self.add_url.stream_error.as_ref())
                        .or(self.add_url.metalink_error.as_ref())
                        .map_or(40.0, |error| {
                            crate::windows::add_url::error_panel_height(error)
                        });
                }
                // The stream panel is a BLOCK, not a line: a "Stream" row,
                // the quality and container pickers, and for a live stream
                // the record-minutes row. None of it was accounted for, so a
                // probed manifest pushed its own pickers past the bottom of
                // the window — the quality list the user is being asked to
                // choose from was exactly the part that fell off.
                h += crate::windows::add_url::plugin_panel_height(&self.add_url);
                h += match &self.add_url.stream {
                    _ if self.add_url.stream_probing => 28.0,
                    // A refusal offers nothing to choose, but the sentence
                    // explaining it wraps to two or three lines.
                    Some(p) if p.drm.is_some() => 64.0,
                    // Each block ends in a blank line of its own.
                    Some(p) if p.live => 104.0,
                    Some(_) => 72.0,
                    None => 0.0,
                };
                h += metalink_panel_height(
                    self.add_url.metalink_probing,
                    self.add_url.metalink.as_ref().map(|m| m.files.len()),
                );
                (760.0, h)
            }
            // New-download layout is short; Properties adds the status,
            // size and last-try lines and the login/cookies rows, plus a
            // Result line when the last attempt left an error to show.
            // Size the window to the rows it actually draws: the dialog
            // does not scroll, so too little clips the buttons and too
            // much leaves a dead strip under them.
            WinKind::FileInfo(dl) => {
                // The proxy row is always drawn, and costs what any other
                // field row costs: the input's own height plus the gap above
                // it. The line under it is only there when the address typed
                // into the row is not a proxy.
                let proxy = 40.0
                    + if crate::windows::file_info::proxy_problem(&self.file_info).is_some() {
                        24.0
                    } else {
                        0.0
                    };
                if self.file_info.is_new {
                    (680.0, 276.0 + proxy)
                } else {
                    let result = self
                        .item(dl)
                        .and_then(|d| d.error.as_deref())
                        .map(result_row_height)
                        .unwrap_or(0.0);
                    // The line naming where the cookies came from, and only
                    // present when there are any. Two lines, for the same
                    // reason the Add URL dialog reserves two: a browser
                    // profile path wraps at this width on every platform.
                    let source = self
                        .item(dl)
                        .and_then(|d| d.cookie_source.as_ref())
                        .map_or(0.0, |_| 48.0);
                    (680.0, 336.0 + result + proxy + source)
                }
            }
            // Matches ProgToggleDetails: a box whose details are hidden
            // must not spring back open when the scale resizes it.
            WinKind::Progress(id) => {
                let details = self
                    .prog
                    .get(&id)
                    .map(|p| p.details)
                    .unwrap_or(self.cfg.settings.show_conn_details);
                crate::windows::progress::standard_size(details)
            }
            WinKind::Complete(_) => (crate::windows::complete::width(), 180.0),
            WinKind::Options => (760.0, 700.0),
            WinKind::Scheduler => (950.0, 660.0),
            WinKind::Batch => (950.0, 700.0),
            WinKind::About => (460.0, 225.0),
            WinKind::Shortcuts => (520.0, 520.0),
            WinKind::Columns => (420.0, 400.0),
            WinKind::Confirm => (500.0, 200.0),
            WinKind::Permissions => (640.0, 410.0),
            WinKind::Update => (560.0, 520.0),
            WinKind::Power => (500.0, 200.0),
            WinKind::ZipPreview(_) => (640.0, 420.0),
        };
        fit_to_display((w, h), self.display, scale)
    }

    /// Open a download's progress box as part of *starting* it — the only
    /// case "Start download progress dialog minimized" covers. Explicitly
    /// asking to see a transfer (double-click, File Properties) still opens
    /// the box in front.
    fn open_progress_window(&mut self, dl: DlId) -> Task<Message> {
        self.sync_prog_state(dl);
        let kind = WinKind::Progress(dl);
        let existed = self.win_of(kind).is_some();
        let task = self.open_window(kind);
        if !existed && self.cfg.settings.start_minimized {
            if let Some(win) = self.win_of(kind) {
                self.minimize_on_open.insert(win);
            }
        }
        task
    }

    pub fn open_window(&mut self, kind: WinKind) -> Task<Message> {
        if let Some(id) = self.win_of(kind) {
            return if kind == WinKind::Main {
                Task::batch([window::minimize(id, false), window::gain_focus(id)])
            } else {
                window::gain_focus(id)
            };
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
        // The main window's bounds are tracked in OS points (see WinMoved),
        // and so is the position handed to winit, so the dialog's size
        // crosses into OS points for the arithmetic.
        let scale = self.ui_scale();
        let (os_w, os_h) = (size.0 * scale, size.1 * scale);
        let position = match (kind, self.main_pos) {
            (WinKind::Main, _) => {
                main_open_position(self.cfg.settings.window_pos, os_w, &display_bounds())
                    .map_or(window::Position::Centered, window::Position::Specific)
            }
            (_, None) => window::Position::Centered,
            (_, Some(origin)) => window::Position::Specific(Point::new(
                origin.x + (self.main_size.width - os_w) / 2.0,
                origin.y + (self.main_size.height - os_h) / 2.0,
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
            application_id: linux_application_id(),
            ..Default::default()
        };
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let platform_specific = window::settings::PlatformSpecific::default();
        let (id, task) = window::open(window::Settings {
            size: iced::Size::new(size.0, size.1),
            // Floor: just enough for the full toolbar row; the default
            // stays proportional to the display. Unlike `size`, iced passes
            // a minimum straight to winit, so this one is in OS points and
            // does carry the scale.
            min_size: match kind {
                WinKind::Main => Some(iced::Size::new(main_min_w() * scale, MAIN_MIN_H * scale)),
                WinKind::Progress(_) => Some(iced::Size::new(os_w, os_h)),
                _ => None,
            },
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
        // "Start progress dialog minimized" is honoured in `WindowOpened`,
        // not here: the window does not exist yet at this point, so a
        // minimize queued now is dropped by the runtime.
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
        if kind == WinKind::AddUrl {
            if let Some(ctl) = &self.add_url.plugin_ctl {
                ctl.cancel();
            }
        }
        if kind == WinKind::Options {
            if let Some(prompt) = self.options.plugins.prompt.take() {
                let _ = prompt.reply.send(Err(hya_plugin_api::PluginError::new(
                    hya_plugin_api::ErrorCode::Cancelled,
                    "plugin prompt closed",
                )));
            }
        }
        if let Some(id) = self.win_of(kind) {
            self.windows.remove(&id);
            window::close(id)
        } else {
            Task::none()
        }
    }

    /// Put a question to the user. While one is already on screen it waits
    /// in line rather than replacing it — an answer is only meaningful for
    /// the question that was showing when it was given.
    fn ask(&mut self, kind: ConfirmKind) -> Task<Message> {
        if self.win_of(WinKind::Confirm).is_some() {
            self.confirm_queue.push_back(kind);
            return Task::none();
        }
        self.confirm = Some(kind);
        self.confirm_remove_file = false;
        self.open_window(WinKind::Confirm)
    }

    /// The confirmation window is gone: raise the next question waiting, if
    /// any. Called from every path that dismisses one.
    fn next_confirm(&mut self) -> Task<Message> {
        self.confirm = None;
        match self.confirm_queue.pop_front() {
            Some(kind) => self.ask(kind),
            None => Task::none(),
        }
    }

    fn trust_extension(&mut self, origin: String) {
        let allowed = &mut self.cfg.settings.allowed_extensions;
        if !allowed.contains(&origin) {
            crate::log::info(&format!("extbus: trusting {origin}"));
            allowed.push(origin);
            self.save_config();
        }
    }

    /// Close the confirmation window and move on to the next question.
    fn dismiss_confirm(&mut self) -> Task<Message> {
        let close = self.close_window(WinKind::Confirm);
        Task::batch([close, self.next_confirm()])
    }

    /// Close every Download File Info window, whichever download each one
    /// was opened for — and any Zip preview, which is a File Info dialog's
    /// sub-dialog and has nothing to show once its dialog is gone.
    ///
    /// The dialog's state is a single slot (`self.file_info`) while the
    /// window identity carries a `DlId`, so two of them can never be
    /// consistent: both windows render the newer download, and the older
    /// one's buttons act on it and close the *other* window — after which
    /// they close nothing at all and the dialog is stuck on screen. The
    /// dialog is therefore opened one at a time, and its buttons dismiss
    /// whatever File Info window exists rather than one specific id.
    /// A window whose kind was dropped before the runtime created it: the
    /// `window::close` issued then was lost, so the surface arrives with no
    /// owner and must be closed again on arrival.
    pub(crate) fn window_is_orphan(&self, id: window::Id) -> bool {
        !self.windows.contains_key(&id)
    }

    fn close_file_info_windows(&mut self) -> Task<Message> {
        let ids: Vec<window::Id> = self
            .windows
            .iter()
            .filter(|(_, k)| matches!(k, WinKind::FileInfo(_) | WinKind::ZipPreview(_)))
            .map(|(id, _)| *id)
            .collect();
        let mut tasks = Vec::with_capacity(ids.len());
        for id in ids {
            self.windows.remove(&id);
            tasks.push(window::close(id));
        }
        Task::batch(tasks)
    }

    /// Effective connection count for a URL (Connection tab exceptions).
    fn conns_for(&self, url: &str) -> usize {
        let host = engine::parse_url(url).map(|u| u.host).unwrap_or_default();
        exception_for(&self.cfg.settings.conn_exceptions, &host)
            .unwrap_or(self.cfg.settings.default_conns)
            .clamp(1, crate::model::MAX_CONNECTIONS)
    }

    /// Saved credentials for a URL from Options > Sites Logins.
    fn login_for(&self, url: &str) -> Option<(String, String)> {
        find_login(url, &self.cfg.settings.logins).map(|l| (l.user.clone(), l.pass.clone()))
    }

    /// The ceiling this download cannot exceed on its own: the lower of its
    /// own cap and the Speed Limiter's, for the views to present.
    ///
    /// Not what the engine is handed. The Speed Limiter is an aggregate over
    /// every running transfer, so the rate a single download may actually
    /// reach is this figure or less, depending on what else is running.
    pub(crate) fn effective_limit(&self, d: &DownloadItem) -> Option<u64> {
        match (d.speed_limit, self.cfg.settings.global_limit()) {
            (Some(own), Some(global)) => Some(own.min(global)),
            (own, global) => own.or(global),
        }
    }

    /// Put the Speed Limiter's cap in force.
    ///
    /// One call, not one per transfer: the engine holds a single bucket every
    /// download draws from, and it binds the transfers already in flight —
    /// which is the case the setting exists for, freeing bandwidth while
    /// something large runs.
    fn apply_speed_limiter(&self) {
        engine::set_global_limit(self.cfg.settings.global_limit());
    }

    /// Put the power-save setting in force: the engine's tick, and the tray
    /// menu's tick mark, which is rebuilt rather than synced.
    fn set_power_save(&self, on: bool) {
        engine::set_power_save(on);
        crate::log::info(&format!("power save: {on}"));
        let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
        crate::tray::reinstall(&queues, on);
    }

    /// The first thing in the Options draft OK cannot accept, and the page
    /// it is on: a value that would be stored and then quietly do nothing,
    /// or something worse than nothing.
    fn options_problem(&self) -> Option<(OptTab, String)> {
        let st = &self.options;
        let s = &st.draft;
        if s.dl_limit_enabled {
            let mb = st.dl_limit_mb_txt.trim().parse::<u64>().unwrap_or(0);
            let hours = st.dl_limit_hours_txt.trim().parse::<u64>().unwrap_or(0);
            if mb == 0 || hours == 0 {
                return Some((
                    OptTab::Quota,
                    i18n::tr("Enter how many MBytes per how many hours, both at least 1."),
                ));
            }
        }
        if s.speed_limiter_on && s.global_speed_limit.is_none() {
            return Some((
                OptTab::SpeedLimit,
                i18n::tr("Enter a speed in KB/sec, or untick \"Limit download speed\"."),
            ));
        }
        match s.proxy_mode {
            ProxyMode::Script => {
                return Some((OptTab::Proxy, crate::proxy::PAC_UNSUPPORTED.to_string()));
            }
            ProxyMode::Manual => {
                if let Some(why) = crate::proxy::manual_problem(s) {
                    return Some((OptTab::Proxy, why));
                }
            }
            _ => {}
        }
        None
    }

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
        // The transfer's own cap only — the Speed Limiter reaches it as an
        // aggregate the engine already holds.
        let stream_limit = self.item(id).and_then(|d| d.speed_limit);
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
        repair_unstarted_name(d);
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
                proxy: d.proxy.clone(),
            };
            engine::send(Cmd::StartStream(Box::new(ss)));
            self.save_state();
            return if open_progress {
                self.open_progress_window(id)
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
            force_stream: false,
            plugin_headers: Vec::new(),
            plugin_plan: d.plugin_plan.clone(),
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
            referer: d.referer.clone(),
            limit: None,
            adaptive,
            remote_time,
            // A Metalink item carries its mirrors, its attested size and its
            // digests into every start — including a resume after a restart,
            // which is why they are persisted with the item rather than held
            // only in the dialog that created it.
            mirrors: d
                .metalink
                .as_ref()
                .map(|m| m.mirrors.clone())
                .unwrap_or_default(),
            attested_size: d.metalink.as_ref().and_then(|m| m.size),
            attested_digest: d.metalink.as_ref().and_then(|m| m.digest.clone()),
            pieces: d.metalink.as_ref().and_then(|m| m.pieces.clone()),
            proxy: d.proxy.clone(),
        };
        let url = d.url.clone();
        let limit = self.item(id).and_then(|d| d.speed_limit);
        let spec = StartSpec {
            conns: self.conns_for(&url),
            limit,
            auth: spec.auth.clone().or_else(|| self.login_for(&url)),
            ..spec
        };
        engine::send(Cmd::Start(Box::new(spec)));
        self.save_state();
        if open_progress {
            self.open_progress_window(id)
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
            return self.ask(ConfirmKind::StopWarn { ids, stop_queues });
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
        // Every window that describes the row goes with it: buttons on a
        // dialog over a download that no longer exists do nothing.
        Task::batch([
            self.close_window(WinKind::Progress(id)),
            self.close_window(WinKind::FileInfo(id)),
            self.close_window(WinKind::Complete(id)),
            self.close_window(WinKind::ZipPreview(id)),
        ])
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

    /// The "Download complete" dialog for `dl` is going away — by its Close
    /// button, by Open, or by the window's own close button. When Options >
    /// Downloads asks for it, this is the moment the finished row leaves the
    /// list: it outlived the transfer only so the dialog had something to
    /// describe.
    fn complete_dismissed(&mut self, dl: DlId) {
        if self.cfg.settings.remove_completed {
            self.remove_item(dl);
        }
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

    /// What a progress window's controls start out showing for `id`.
    fn prog_seed_of(&self, id: DlId) -> (Option<u64>, ProxyChoice) {
        match self.item(id) {
            Some(d) => (d.speed_limit, d.proxy.clone()),
            None => (None, ProxyChoice::default()),
        }
    }

    /// Give `id` a progress-window state if it has none, and re-read the
    /// proxy row from the item.
    ///
    /// Re-read rather than seeded once: File Info edits the same setting, so
    /// a window opened again after a change there would otherwise still show
    /// the route the download no longer takes. Only the address box's text is
    /// the window's own — it survives unless the item names an address.
    fn sync_prog_state(&mut self, id: DlId) {
        let (seed, proxy) = self.prog_seed_of(id);
        let details = self.cfg.settings.show_conn_details;
        let p = self
            .prog
            .entry(id)
            .or_insert_with(|| prog_state_seed(seed, &proxy, details));
        p.proxy_pick = proxy.pick();
        if !proxy.spec().is_empty() {
            p.proxy_spec = proxy.spec().to_string();
        }
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
            last_try: Some(fmt::now_unix()),
            queue,
            q_order,
            auth,
            cookies: None,
            cookie_source: None,
            referer: None,
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
            plugin_details: vec![],
            status_line: String::new(),
            shutdown_after: false,
            shutdown_action: PowerAction::default(),
            stream: None,
            plugin_plan: None,
            metalink: None,
            name_locked: false,
            proxy: ProxyChoice::default(),
        });
        self.save_state();
        id
    }

    /// Name a freshly listed item after something better than its URL — a
    /// stream's chosen rendition, a Metalink `<file>` — and file it under the
    /// category that name implies, in that category's folder.
    fn name_new_item(&mut self, id: DlId, name: String) {
        let cat = categorize(&name, &self.cfg.categories);
        let dir = self
            .cat_dir(cat.as_deref())
            .or_else(|| self.cat_dir(None))
            .unwrap_or_default();
        if let Some(d) = self.item_mut(id) {
            d.set_file_name(&name);
            d.category = cat;
            d.save_dir = dir;
        }
    }

    /// One list entry for one `<file>` of a Metalink document, carrying the
    /// whole mirror list, the document's size and digest, and its `<pieces>`.
    fn adopt_metalink_file(
        &mut self,
        f: &engine::MetalinkChoice,
        auth: Option<(String, String)>,
        queue: Option<String>,
    ) -> DlId {
        let id = self.add_item(f.primary.clone(), auth, queue);
        // The document names the file. A redirector URL
        // (`metalink?repo=fedora-40`) names nothing, and the last path
        // segment of the first mirror is a guess the document does not need
        // us to make.
        self.name_new_item(id, f.name.clone());
        if let Some(d) = self.item_mut(id) {
            d.name_locked = true;
            d.size = f.info.size;
            // Every mirror in the list honours ranges — a source that does
            // not is dropped at probe time — and the document's size is what
            // makes resuming across them safe.
            d.resume = Some(true);
            d.metalink = Some(f.info.clone());
        }
        id
    }

    /// The Batch dialog's "all files to one category / one directory"
    /// overrides, and the Queued state every batch row is left in.
    fn file_batch_row(
        &mut self,
        id: DlId,
        cat_override: &Option<String>,
        dir_override: &Option<String>,
    ) {
        if let Some(c) = cat_override {
            let dir = self.cat_dir(Some(c));
            if let Some(d) = self.item_mut(id) {
                d.category = Some(c.clone());
                if let Some(dir) = dir {
                    d.save_dir = dir;
                }
            }
        }
        if let Some(d) = self.item_mut(id) {
            if let Some(dir) = dir_override {
                d.save_dir = dir.clone();
            }
            d.state = DlState::Queued;
        }
    }

    /// The File Info dialog's fields for an item that was just listed.
    fn file_info_for_new(&self, id: DlId) -> FileInfoState {
        let d = self.item(id).expect("a just-listed item is in the list");
        FileInfoState {
            dl: id,
            category: d
                .category
                .clone()
                .unwrap_or_else(|| model::DEFAULT_CATEGORY.into()),
            save_dir: d.save_dir.clone(),
            file_name: d.file_name.clone(),
            description: String::new(),
            // Off by default: an edited Save As folder applies to this one
            // download. Only an explicit tick writes it back to the category.
            remember: false,
            is_new: true,
            url: d.url.clone(),
            login: d.auth.clone().map(|a| a.0).unwrap_or_default(),
            password: d.auth.clone().map(|a| a.1).unwrap_or_default(),
            cookies: d.cookies.clone().unwrap_or_default(),
            proxy_pick: d.proxy.pick(),
            proxy_spec: d.proxy.spec().to_string(),
            bg_blocked: !self.auto_start_type(id),
            ..FileInfoState::default()
        }
    }

    /// What happens to a just-listed item once the dialog that added it has
    /// closed: the File Info dialog when Options asks for one, else a start
    /// — unless the address is on the do-not-start list, or is a signed URL
    /// that would expire while a dialog waits.
    fn offer_new_item(&mut self, id: DlId, close: Task<Message>) -> Task<Message> {
        // A signed URL may expire within seconds (`s3q.ait.dtu.dk` allows ten):
        // a dialog that waits for a person is a guaranteed 403, so start now.
        let perishable = self.item(id).is_some_and(|d| expiring_soon(&d.url));
        if perishable {
            crate::log::info(&format!(
                "expiring link: starting at once, skipping the File Info dialog for {}",
                self.item(id).map(|d| d.url.as_str()).unwrap_or("-")
            ));
        }
        if self.cfg.settings.show_file_info_dialog && !perishable {
            self.file_info = self.file_info_for_new(id);
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

    fn add_plugin_playlist(&mut self, info: crate::plugins::PlanInfo) -> Task<Message> {
        let entries: Vec<_> = info
            .plan
            .entries
            .iter()
            .filter(|entry| {
                info.preferences
                    .playlist_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&entry.id))
            })
            .collect();
        if entries.is_empty() {
            self.add_url.error = Some(i18n::tr("Select at least one playlist item."));
            return self.resize_open(WinKind::AddUrl);
        }
        let queue = self.cfg.queues.first().map(|queue| queue.name.clone());
        for entry in entries {
            if self
                .state
                .downloads
                .iter()
                .any(|item| item.url == entry.url)
            {
                continue;
            }
            let id = self.add_item(entry.url.clone(), None, queue.clone());
            self.apply_capture_extras(id, self.add_url.capture.clone());
            let mut preferences = info.preferences.clone();
            preferences.playlist_ids = None;
            preferences.track_ids.clear();
            let pending = crate::plugins::PlanInfo {
                plugin: info.plugin.clone(),
                plan: hya_plugin_api::Plan::single(
                    entry.title.as_deref().unwrap_or(&entry.id),
                    hya_plugin_api::Track::file(&entry.id, &entry.url),
                ),
                preferences,
            };
            let name = hya_net::filename::portable(entry.title.as_deref().unwrap_or(&entry.id))
                .unwrap_or_else(|| entry.id.clone());
            self.name_new_item(
                id,
                format!(
                    "{name}-{}.{}",
                    entry.id,
                    info.preferences.audio_format.as_deref().unwrap_or("mkv")
                ),
            );
            if let Some(item) = self.item_mut(id) {
                item.plugin_plan = Some(pending);
                item.state = DlState::Queued;
            }
        }
        self.add_url = AddUrlState::default();
        self.save_state();
        self.close_window(WinKind::AddUrl)
    }

    fn add_url_ok(&mut self) -> Task<Message> {
        // What the address IS is still being read; adding it now
        // would download the manifest or the mirror list as a file.
        if self.add_url.stream_probing
            || self.add_url.metalink_probing
            || self.add_url.plugin_probing
        {
            return Task::none();
        }
        let url = self.add_url.address.trim().to_string();
        if let Some(info) = self
            .add_url
            .plugin_plan
            .clone()
            .filter(|info| !info.plan.entries.is_empty() && self.add_url.plugin_of == url)
        {
            return self.add_plugin_playlist(info);
        }
        // A mirror list is not a download; it is a list of them. Handled
        // before the URL check because the address may be a local
        // `.meta4` path, which `parse_url` correctly refuses.
        if self.add_url.metalink.is_some() && self.add_url.metalink_of == url {
            return self.add_metalink_items();
        }
        let transfer = self
            .add_url
            .plugin_plan
            .as_ref()
            .filter(|_| self.add_url.plugin_of == url)
            .is_some_and(|info| info.plan.transfer.is_some());
        if transfer
            && self.add_url.plugin_plan.as_ref().is_some_and(|info| {
                info.preferences
                    .transfer_files
                    .as_ref()
                    .is_some_and(Vec::is_empty)
            })
        {
            self.add_url.error = Some("Select at least one transfer file.".into());
            return self.resize_open(WinKind::AddUrl);
        }
        let plugin_resolved = self.add_url.plugin_plan.is_some() && self.add_url.plugin_of == url;
        if let Err(e) = engine::parse_url(&url).map(|_| ()).or_else(|error| {
            if plugin_resolved {
                Ok(())
            } else {
                Err(error)
            }
        }) {
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
            .find(|d| {
                d.url == url
                    || (transfer
                        && self.add_url.plugin_plan.as_ref().is_some_and(|new| {
                            d.plugin_plan.as_ref().is_some_and(|existing| {
                                existing.plugin == new.plugin && existing.plan.id == new.plan.id
                            })
                        }))
            })
            .map(|d| d.id);
        let captured = self.add_url.capture.taken();
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
        let plugin_name = self
            .add_url
            .plugin_plan
            .as_ref()
            .filter(|_| self.add_url.plugin_of == url)
            .map(crate::plugins::file_name);
        let name = match &stream {
            Some(si) => format!("{}.{}", stream_base_name(&url), si.ext()),
            None => plugin_name
                .clone()
                .or_else(|| captured.name.clone())
                .unwrap_or_else(|| engine::file_name_from_url(&url)),
        };
        let cat = crate::model::categorize(&name, &self.cfg.categories);
        let dir = self
            .cat_dir(cat.as_deref())
            .or_else(|| self.cat_dir(None))
            .unwrap_or_default();
        // A capture name and a stream name are the name the file
        // will really be saved under; so is a URL that names one.
        let named =
            captured.name.is_some() || stream.is_some() || engine::url_file_name(&url).is_some();
        let file = collision_file(&dir, &name, named);
        if existing.is_some() || file.is_some() {
            // Logged: the dialog names a path, and the report that
            // it names one that "does not exist" cannot be told
            // apart from a real leftover without knowing what Hydra
            // saw at this instant.
            crate::log::info(&format!(
                "duplicate: list={} disk={}",
                existing
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "-".into()),
                file.as_deref().unwrap_or("-")
            ));
            let pending = Box::new(PendingAdd {
                plugin_plan: self
                    .add_url
                    .plugin_plan
                    .clone()
                    .filter(|_| self.add_url.plugin_of == self.add_url.address.trim()),
                url,
                auth,
                capture: captured,
            });
            self.add_url = AddUrlState::default();
            let close = self.close_window(WinKind::AddUrl);
            let ask = self.ask(ConfirmKind::Duplicate {
                existing,
                file,
                pending,
            });
            return Task::batch([close, ask]);
        }
        let id = self.add_item(url, auth, None);
        self.apply_capture_extras(id, captured);
        let plugin_plan = self
            .add_url
            .plugin_plan
            .clone()
            .filter(|_| self.add_url.plugin_of == self.add_url.address.trim());
        if let Some(info) = plugin_plan {
            self.name_new_item(id, name.clone());
            self.apply_plugin_plan(id, info);
        }
        if let Some(si) = stream {
            self.name_new_item(id, name);
            if let Some(d) = self.item_mut(id) {
                // Resumable by whole segments, not byte ranges.
                d.resume = Some(true);
                d.stream = Some(si);
            }
        }
        self.add_url = AddUrlState::default();
        let close = self.close_window(WinKind::AddUrl);
        self.offer_new_item(id, close)
    }

    fn apply_plugin_plan(&mut self, id: DlId, info: crate::plugins::PlanInfo) {
        if let Some(d) = self.item_mut(id) {
            if let Some(transfer) = &info.plan.transfer {
                d.size = Some(
                    transfer
                        .files
                        .iter()
                        .filter(|file| {
                            info.preferences
                                .transfer_files
                                .as_ref()
                                .is_none_or(|files| files.contains(&file.index))
                        })
                        .map(|file| file.size)
                        .sum(),
                );
                d.resume = Some(true);
            }
            d.plugin_plan = Some(info);
        }
    }

    fn on_ext_event(&mut self, ev: crate::extbus::ExtEvent) -> Task<Message> {
        match ev {
            crate::extbus::ExtEvent::Download(dl, ack) => {
                // Same flow as a manual Add URL, so duplicate detection,
                // categorization, and the File Info dialog all behave
                // consistently for browser capture.
                crate::log::info(&format!("ext: capture dialog for {}", dl.url));
                self.capture_raise = true;
                self.add_url = AddUrlState {
                    address: dl.url,
                    capture: CaptureExtras {
                        cookies: dl.cookies,
                        cookie_source: None,
                        name: dl.filename,
                        referer: dl.referer,
                        proxy: dl.proxy,
                    },
                    ..AddUrlState::default()
                };
                let first_new = self.state.next_id;
                let task = self.update(Message::AddUrlOk);
                // The item is listed (or a duplicate dialog is up over
                // it): hydra owns this download, so the browser may drop
                // its own paused copy. Until this, it must not — and if
                // the browser has already been told to keep it, this
                // copy is the duplicate and goes away again.
                if ack.confirm() {
                    return task;
                }
                crate::log::warn("ext: capture already handed back to the browser; dropping it");
                let added: Vec<DlId> = self
                    .state
                    .downloads
                    .iter()
                    .filter(|d| d.id >= first_new)
                    .map(|d| d.id)
                    .collect();
                let mut tasks = vec![task];
                for id in added {
                    tasks.push(self.delete_item_opts(id, false));
                }
                if matches!(self.confirm, Some(ConfirmKind::Duplicate { .. })) {
                    tasks.push(self.dismiss_confirm());
                }
                Task::batch(tasks)
            }
            crate::extbus::ExtEvent::Stream(s) => {
                // Not the capture dialog: a manifest cannot be probed
                // for a size or a name, so the entry is built from what
                // the extension already read out of it and started.
                crate::log::info(&format!("ext: stream capture for {}", s.url));
                self.capture_raise = true;
                let container = s.container.clone().unwrap_or_else(|| "MP4".into());
                let ext = crate::model::container_ext(&container);
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
                    d.set_file_name(&file_name);
                    d.category = category;
                    d.save_dir = save_dir;
                    d.cookies = s.cookies.clone();
                    d.size = s.size;
                    if let Some(px) = captured_proxy(s.proxy.clone()) {
                        d.proxy = px;
                    }
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
                self.batch.category = model::DEFAULT_CATEGORY.into();
                let open = self.open_window(WinKind::Batch);
                let text = urls.join("\n");
                Task::batch([open, self.update(Message::BatchLoaded(Some(text)))])
            }
            crate::extbus::ExtEvent::InstallPluginUrl(source) => {
                self.update(Message::InstallPluginSource(source))
            }
            crate::extbus::ExtEvent::InstallPlugin(path) => {
                self.update(Message::InstallPluginFile(path))
            }
            crate::extbus::ExtEvent::Open => self.open_window(WinKind::Main),
            crate::extbus::ExtEvent::TrustRequest(origin) => {
                self.ask(ConfirmKind::TrustExtension(origin))
            }
            crate::extbus::ExtEvent::Shutdown => {
                // A newer build is taking over the single-instance slot
                // (extbus::signal_existing): leave the way tray Exit does.
                crate::log::info("ext: shutdown requested by a newer build; exiting");
                self.save_state();
                self.save_config();
                self.flush_saves();
                iced::exit()
            }
        }
    }

    /// OK, Start Download or Download Later in the File Info dialog: commit
    /// the fields to the item, then start it, park it, or leave it as found.
    fn commit_file_info(&mut self, start: bool, park: bool) -> Task<Message> {
        // A route the transport cannot take stops here, with the
        // reason under the row. Letting the dialog close would start
        // a download that fails a moment later for a reason the user
        // can no longer see — which is how "empty proxy
        // specification" ended up in the Result line instead of
        // beside the box that caused it.
        self.file_info.proxy_needs_address = true;
        if crate::windows::file_info::proxy_problem(&self.file_info).is_some() {
            return self.resize_open(WinKind::FileInfo(self.file_info.dl));
        }
        self.file_info.proxy_needs_address = false;
        let mut fi = self.file_info.clone();
        if fi.save_dir.trim().is_empty() {
            // A bare file name in Save As: keep the folder the item
            // already has, else the category's.
            fi.save_dir = self
                .item(fi.dl)
                .map(|d| d.save_dir.clone())
                .filter(|d| !d.is_empty())
                .or_else(|| self.cat_dir(Some(&fi.category)))
                .unwrap_or_default();
        }
        // Options > Save to > "Change folder for category on last
        // selected" is the same tick, made for every dialog whose
        // folder the user actually edited.
        let remember = fi.remember || (self.cfg.settings.remember_last_dir && fi.dir_touched);
        if remember && !fi.save_dir.is_empty() {
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
        let (new_proxy, proxy_changed) = match self.item(fi.dl) {
            Some(d) => reroute(d, &fi),
            None => (
                ProxyChoice::from_parts(fi.proxy_pick, &fi.proxy_spec),
                false,
            ),
        };
        if proxy_changed {
            crate::log::info(&format!("#{} proxy -> {:?}", fi.dl, new_proxy.pick()));
        }
        if url_changed {
            // Mirror switch: keep the held byte spans — the engine
            // verifies the new source reports the same size and
            // continues where mirror A stopped.
            crate::log::info(&format!("#{} mirror -> {}", fi.dl, fi.url.trim()));
        }
        if (url_changed || proxy_changed)
            && self
                .item(fi.dl)
                .map(|d| d.state.is_active())
                .unwrap_or(false)
        {
            engine::send(Cmd::Stop(fi.dl));
        }
        if let Some(d) = self.item_mut(fi.dl) {
            d.category = Some(fi.category.clone());
            d.save_dir = fi.save_dir.clone();
            if !fi.file_name.trim().is_empty() {
                d.set_file_name(&fi.file_name);
                // Typing in the box is the user naming the file:
                // pin it so the probe this Start Download is about
                // to fire cannot hand the name back to the server.
                d.name_locked |= fi.name_touched;
            }
            d.description = fi.description.clone();
            if url_changed {
                d.url = fi.url.trim().to_string();
            }
            if url_changed || proxy_changed {
                d.state = DlState::Paused;
                d.error = None;
            }
            d.auth = (!fi.login.is_empty()).then(|| (fi.login.clone(), fi.password.clone()));
            let typed = fi.cookies.trim();
            // An edited value is the user's, whatever it was before:
            // leaving the old attribution would credit a browser for
            // a session they replaced by hand.
            if d.cookies.as_deref().unwrap_or("") != typed {
                d.cookie_source =
                    (!typed.is_empty()).then(|| crate::i18n::tr("edited in Properties"));
            }
            d.cookies = (!typed.is_empty()).then(|| typed.to_string());
            d.proxy = new_proxy;
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
                Task::batch([close, self.open_progress_window(fi.dl)])
            }
            // "Start Download As New": the finished file is being
            // fetched again, so nothing left over from the first
            // transfer may be taken for a resume.
            (true, Some(DlState::Complete)) => {
                if let Some(d) = self.item_mut(fi.dl) {
                    reset_for_redownload(d);
                }
                Task::batch([close, self.start_download(fi.dl, true)])
            }
            (true, _) => Task::batch([close, self.start_download(fi.dl, true)]),
            // Download Later: park the background transfer, keep the
            // bytes for resume, and leave it Queued for Start Queue.
            (false, Some(st)) if park && st.is_active() => {
                self.stop_download(fi.dl);
                let main_q = self.cfg.queues.first().map(|q| q.name.clone());
                if let Some(d) = self.item_mut(fi.dl) {
                    if d.queue.is_none() {
                        d.queue = main_q;
                    }
                }
                close
            }
            (false, _) if park => {
                if let Some(d) = self.item_mut(fi.dl) {
                    if matches!(d.state, DlState::Paused) {
                        d.state = DlState::Queued;
                    }
                }
                self.save_state();
                close
            }
            // Properties OK: the fields are committed, the transfer
            // is left exactly as it was found.
            (false, _) => close,
        }
    }

    fn batch_ok(&mut self) -> Task<Message> {
        self.parse_batch();
        // Only what the table shows: a hidden duplicate or a hidden
        // web page is not added however its checkbox was left.
        let checked: Vec<BatchRow> = self
            .batch_rows()
            .into_iter()
            .filter(|r| r.checked)
            .collect();
        if checked.is_empty() {
            return self.ask(ConfirmKind::NoneChecked);
        }
        // "All files to one directory" with no directory would file
        // the batch wherever the process happens to be running.
        if self.batch.to_dir && self.batch.dir.trim().is_empty() {
            return Task::none();
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
        for row in checked {
            let url = row.url;
            // A mirror list in the batch is not one download but a list
            // of them: one item per file entry, each carrying the whole
            // mirror list, the document's size and digest, and its
            // `<pieces>`. Adding the document itself would save a few
            // kilobytes of XML under the name of the object the user
            // wanted.
            if let Some(doc) = self.batch.metalinks.get(&url).cloned() {
                for f in &doc.files {
                    if self.state.downloads.iter().any(|d| d.url == f.primary) {
                        continue;
                    }
                    let id = self.adopt_metalink_file(f, None, queue.clone());
                    self.file_batch_row(id, &cat_override, &dir_override);
                }
                continue;
            }
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
            let stream_name = stream
                .as_ref()
                .map(|si| format!("{}.{}", stream_base_name(&url), si.ext()));
            let id = self.add_item(url, None, queue.clone());
            if let Some(si) = stream {
                if let Some(d) = self.item_mut(id) {
                    d.resume = Some(true);
                    d.stream = Some(si);
                }
            } else if let Some(size) = row.size {
                // What the dialog measured is what the list shows. A
                // "Download Later" item never starts on its own, so
                // without this its Size column stays empty until the
                // user runs it — the answer was already on hand.
                //
                // Not for a stream: the row holds the size of the
                // MANIFEST, a couple of kilobytes of text, and the
                // media's own size is a projection the transfer
                // refines as segments land.
                if let Some(d) = self.item_mut(id) {
                    d.size = Some(size);
                }
            }
            let probed = stream_name.or(probed);
            if let Some(name) = probed.filter(|n| !n.is_empty()) {
                let cat = categorize(&name, &self.cfg.categories);
                let dir = self.cat_dir(cat.as_deref());
                if let Some(d) = self.item_mut(id) {
                    d.set_file_name(&name);
                    if let Some(c) = cat {
                        d.category = Some(c);
                    }
                    if let Some(dir) = dir {
                        d.save_dir = dir;
                    }
                }
            }
            self.file_batch_row(id, &cat_override, &dir_override);
        }
        self.save_state();
        self.batch = BatchState::default();
        self.close_window(WinKind::Batch)
    }

    /// Turn the Metalink document in the Add URL dialog into download items.
    ///
    /// One item per file entry, because one download fetches one object and a
    /// document may describe several. Each carries the whole mirror list, the
    /// document's size and digest, and its `<pieces>` — which is the difference
    /// between a list of URLs and a mirror list: the size admits every agreeing
    /// mirror without the `ETag` match independent operators cannot produce, the
    /// surplus mirrors become a reserve bench, and a corrupt chunk costs one
    /// chunk refetched from elsewhere rather than the whole object.
    ///
    /// Started sequentially through the ordinary queue, not fanned out: a mirror
    /// list is a list of volunteer machines, and opening four connections per
    /// file across six files against the same hosts is precisely what the
    /// politeness ceilings exist to prevent.
    fn add_metalink_items(&mut self) -> Task<Message> {
        let Some(doc) = self.add_url.metalink.clone() else {
            return Task::none();
        };
        let auth = self
            .add_url
            .use_auth
            .then(|| (self.add_url.login.clone(), self.add_url.password.clone()));
        let mut added = 0usize;
        for f in &doc.files {
            // The same duplicate rule the single-URL path follows: an address
            // already in the list is not added twice.
            if self.state.downloads.iter().any(|d| d.url == f.primary) {
                continue;
            }
            let id = self.adopt_metalink_file(f, auth.clone(), None);
            if f.info.signed {
                crate::log::warn(&format!(
                    "#{id} the mirror list carries an OpenPGP signature over {}; Hydra records it but does not verify it",
                    f.name
                ));
            }
            added += 1;
        }
        crate::log::info(&format!(
            "metalink {} ({}): added {added} of {} file(s)",
            doc.origin,
            doc.version,
            doc.files.len()
        ));
        self.add_url = AddUrlState::default();
        let close = self.close_window(WinKind::AddUrl);
        if added == 0 {
            return close;
        }
        let starts: Vec<DlId> = self
            .state
            .downloads
            .iter()
            .rev()
            .take(added)
            .map(|d| d.id)
            .collect();
        let mut tasks = vec![close];
        for (i, id) in starts.into_iter().rev().enumerate() {
            // Only the first opens a progress window: a document describing six
            // files would otherwise bury the desktop under six of them.
            tasks.push(self.start_download(id, i == 0));
        }
        self.save_state();
        Task::batch(tasks)
    }

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
        // A pending power action makes the complete dialog moot: the
        // countdown takes the screen instead, and whatever the user does
        // with it decides whether Hydra is still here afterwards.
        if let Some(action) = self
            .item(id)
            .filter(|d| d.shutdown_after)
            .map(|d| d.shutdown_action)
        {
            let armed = self.arm_power_action(action, false);
            return Task::batch([task, armed]);
        }
        if self.cfg.settings.show_complete_dialog {
            // The dialog reads the row it describes, so a row asked to
            // disappear on completion is dropped only once that dialog is
            // closed (in `WindowClosed`), not here.
            return Task::batch([task, self.open_window(WinKind::Complete(id))]);
        }
        if self.cfg.settings.remove_completed {
            self.remove_item(id);
        }
        task
    }

    /// Settle the shortcut table once the dialog is done with it: every
    /// combo in the one spelling a key press produces, and one that no press
    /// could ever match back to its default rather than stored as dead.
    fn normalize_shortcuts(&mut self) {
        let mut changed = false;
        let mut taken: Vec<String> = Vec::new();
        for (id, default, _) in crate::model::SHORTCUT_ACTIONS {
            let typed = self.cfg.shortcuts.get(id).cloned().unwrap_or_default();
            // Earlier rows win a conflict: the later one would never fire.
            let settled = model::normalize_combo(&typed)
                .filter(|c| !taken.contains(c))
                .unwrap_or_else(|| model::platform_default(default));
            taken.push(settled.clone());
            if settled != typed {
                self.cfg.shortcuts.insert(id.to_string(), settled);
                changed = true;
            }
        }
        if changed {
            self.save_config();
        }
    }

    /// Call a running update off: raise its cancel flag and retire its
    /// generation, so nothing it still reports can act on the app.
    fn cancel_update(&mut self) {
        if let Some(c) = &self.updater.cancel {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.updater = UpdateUiState {
            generation: self.updater.generation + 1,
            ..UpdateUiState::default()
        };
    }

    /// Arm a "when done" power action behind its cancellable countdown.
    ///
    /// Nothing happens to the machine here: the dialog goes up, ticks down
    /// from [`POWER_COUNTDOWN_SECS`], and only then calls the OS. State is
    /// flushed up front all the same — the countdown ends in a call that can
    /// stop the process where it stands.
    ///
    /// `exit_after` quits Hydra once the countdown resolves even if the
    /// action itself leaves the session running (a queue's "Exit Hydra when
    /// done"); cancelling the power action does not cancel that.
    ///
    /// A second call while one countdown is already up is ignored: two
    /// downloads finishing together ask for one dialog, not two.
    fn arm_power_action(&mut self, action: PowerAction, exit_after: bool) -> Task<Message> {
        self.save_state();
        self.save_config();
        self.flush_saves();
        if let Some(p) = &mut self.power {
            // The stricter of the two wins: a queue that also wanted the app
            // gone must not lose that because a download got there first.
            p.exit_after |= exit_after;
            return Task::none();
        }
        crate::log::info(&format!(
            "power action {action:?} armed, {POWER_COUNTDOWN_SECS}s to cancel"
        ));
        self.power = Some(PowerPrompt {
            action,
            secs: POWER_COUNTDOWN_SECS,
            exit_after,
        });
        self.open_window(WinKind::Power)
    }

    /// The countdown ran out (or "now" was pressed): do it.
    fn fire_power_action(&mut self) -> Task<Message> {
        let Some(p) = self.power.take() else {
            return Task::none();
        };
        let close = self.close_window(WinKind::Power);
        self.save_state();
        self.save_config();
        self.flush_saves();
        run_power_action(p.action);
        // Shutdown and log off end the process anyway; sleeping only
        // suspends the machine, so Hydra stays up and is still here on wake
        // unless the queue also asked it to exit.
        if p.action.ends_session() || p.exit_after {
            Task::batch([close, iced::exit()])
        } else {
            close
        }
    }

    /// Call the pending power action off — the Cancel button, or the OS
    /// close button on the countdown window. "Exit Hydra when done" is a
    /// separate instruction and still stands.
    fn cancel_power_action(&mut self) -> Task<Message> {
        let Some(p) = self.power.take() else {
            return Task::none();
        };
        crate::log::info(&format!("power action {:?} cancelled", p.action));
        let close = self.close_window(WinKind::Power);
        if p.exit_after {
            Task::batch([close, iced::exit()])
        } else {
            close
        }
    }

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
        if self.sch.queue == old {
            self.sch.queue = new.to_string();
        }
        self.save_config();
        self.save_state();
        self.refresh_native_menu();
        let queues: Vec<String> = self.cfg.queues.iter().map(|q| q.name.clone()).collect();
        crate::tray::reinstall(&queues, self.cfg.settings.power_save);
        true
    }

    /// The queue the Scheduler window is showing, by name.
    ///
    /// The window keeps a name, and a name can go stale — the queue was
    /// renamed from the sidebar, or deleted. The view already falls back to
    /// the first queue in that case; the handlers must act on the same one
    /// rather than on nothing.
    fn sch_queue(&mut self) -> String {
        if !self.cfg.queues.iter().any(|q| q.name == self.sch.queue) {
            self.sch.queue = self
                .cfg
                .queues
                .first()
                .map(|q| q.name.clone())
                .unwrap_or_default();
            self.sch.rename_draft = self.sch.queue.clone();
        }
        self.sch.queue.clone()
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
            // With a power action pending, the countdown owns the exit too:
            // "Exit Hydra when done" still happens, but only once the user
            // has had their ten seconds to call the machine's fate off.
            if let Some(action) = power_action {
                let armed = self.arm_power_action(action, exit_app);
                return Task::batch([Task::batch(tasks), armed]);
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
            if s.stop_enabled && same_minute(&s.stop_at, &hhmm) && q.running {
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
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                if may_adopt_name(d) {
                    adopted = d.set_file_name(&name);
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
            engine::Event::PluginDetails { id, rows } => {
                if let Some(item) = self.item_mut(id) {
                    item.plugin_details = rows;
                }
                Task::none()
            }
            engine::Event::PluginPrompt(prompt) => {
                if let Some(old) = self.options.plugins.prompt.take() {
                    let _ = old.reply.send(Err(hya_plugin_api::PluginError::new(
                        hya_plugin_api::ErrorCode::Cancelled,
                        "another prompt replaced this one",
                    )));
                }
                self.options.plugins.answers = prompt
                    .form
                    .fields
                    .iter()
                    .filter_map(|f| {
                        f.default.as_ref().map(|v| {
                            (
                                f.key.clone(),
                                match v {
                                    hya_plugin_api::Value::Text(s) => s.clone(),
                                    hya_plugin_api::Value::Bool(v) => v.to_string(),
                                    hya_plugin_api::Value::Number(v) => v.to_string(),
                                },
                            )
                        })
                    })
                    .collect();
                self.options.plugins.prompt = Some(prompt);
                self.options.tab = OptTab::Plugins;
                self.open_window(WinKind::Options)
            }
            engine::Event::Probed {
                id,
                size,
                ranges,
                file_name,
            } => {
                if let Some(d) = self.item_mut(id) {
                    d.resume = Some(
                        ranges
                            || d.plugin_plan
                                .as_ref()
                                .is_some_and(|info| info.plan.transfer.is_some()),
                    );
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
                // What was typed into the dialog is not obsolete just because
                // the bytes beat the person to the button: apply it, and move
                // the finished file to where they said. See
                // `adopt_dialog_edits`.
                if self.file_info.is_new
                    && self.file_info.dl == id
                    && self.win_of(WinKind::FileInfo(id)).is_some()
                {
                    let fi = self.file_info.clone();
                    if let Some(d) = self.item_mut(id) {
                        match adopt_dialog_edits(d, &fi) {
                            Ok(Some(to)) => crate::log::info(&format!(
                                "#{id} finished under the provisional name; moved to {}",
                                to.display()
                            )),
                            Ok(None) => {}
                            Err(e) => crate::log::warn(&format!(
                                "#{id} could not move the finished file to the name typed in File Info: {e}"
                            )),
                        }
                    }
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
            engine::Event::Discarded { id } => {
                if let Some(d) = self.item_mut(id) {
                    d.held.clear();
                    d.downloaded = 0;
                    d.disp_progress = 0.0;
                    d.size = None;
                    d.resume = None;
                    d.rate = 0.0;
                    d.eta_secs = None;
                }
                self.save_state();
                Task::none()
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
                let tick = self.queue_tick();
                if !permission_denied {
                    return tick;
                }
                // OS-native consent instead of sudo or manual settings: a
                // folder chosen through the system panel is implicitly
                // granted, so offer the picker and resume right there. The
                // row carries the failure while the panel is up, because the
                // panel no longer holds the event loop and the user can see
                // the list behind it.
                let start = self
                    .item(id)
                    .map(|d| d.save_dir.clone())
                    .unwrap_or_default();
                let owner = self
                    .win_of(WinKind::Progress(id))
                    .or_else(|| self.win_of(WinKind::FileInfo(id)))
                    .or_else(|| self.win_of(WinKind::Main));
                let ask = Ask::in_dir(&start);
                let regrant = picker::folder(owner, ask)
                    .and_then(move |dir| Task::done(Message::SaveDirRegranted(id, dir)));
                Task::batch([tick, regrant])
            }
        }
    }

    /// File `id` under the folder the permission panel granted, ready to be
    /// started again.
    ///
    /// The transfer keeps the bytes it already has: `held` was written to the
    /// item when the failure came in, and only the `.part` path is dropped,
    /// because it named a file under the folder that could not be written.
    fn save_dir_regranted(&mut self, id: DlId, dir: std::path::PathBuf) {
        let dir = picker::into_string(dir);
        crate::log::info(&format!("#{id} folder re-granted: {dir}"));
        let Some(d) = self.item_mut(id) else {
            return;
        };
        d.save_dir = dir;
        d.part_path = None;
        d.state = DlState::Paused;
        d.error = None;
        self.save_state();
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
        engine::set_progress_window_open(
            self.windows
                .values()
                .any(|k| matches!(k, WinKind::Progress(_))),
        );
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Noop => Task::none(),
            Message::WindowOpened(id) => {
                if self.window_is_orphan(id) {
                    return window::close(id);
                }
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
                    let state = self.native_menu_state();
                    let queues: Vec<String> =
                        self.cfg.queues.iter().map(|q| q.name.clone()).collect();
                    crate::macos_menu::install(
                        &state,
                        &queues,
                        &self.cfg.settings.speed_profiles,
                        &crate::i18n::available(),
                    );
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
                let parent = self.attach_to_main(id);
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
                        parent,
                        window::set_level(id, window::Level::AlwaysOnTop),
                        window::gain_focus(id),
                    ]);
                }
                // A progress box asked to start minimized goes down instead
                // of being focused — focusing it would restore it, which is
                // what made a manually started download pop up in front.
                let reveal = if self.minimize_on_open.remove(&id) {
                    window::minimize(id, true)
                } else {
                    window::gain_focus(id)
                };
                // A dialog whose first act is typing puts the caret in the
                // box itself. Focusing the window alone leaves every text
                // input unfocused, so Ctrl-V and the context menu's Paste
                // had nowhere to land until the box had been clicked — which
                // is not what any other Add-URL dialog asks of you. Queued
                // here rather than beside `window::open`: the widget the
                // operation looks for does not exist until the window does.
                let caret = match self.windows.get(&id) {
                    Some(WinKind::AddUrl) => {
                        iced::widget::operation::focus(crate::windows::add_url::ADDRESS_ID)
                    }
                    _ => Task::none(),
                };
                Task::batch([pin_surface, skip_taskbar, parent, reveal, caret])
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
                    Some(WinKind::Confirm) => self.next_confirm(),
                    Some(WinKind::Shortcuts) => {
                        self.normalize_shortcuts();
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
                    Some(WinKind::Complete(dl)) => {
                        self.complete_dismissed(dl);
                        Task::none()
                    }
                    // OS close button on the listing: back to its dialog.
                    Some(WinKind::ZipPreview(dl)) => self
                        .win_of(WinKind::FileInfo(dl))
                        .map(window::gain_focus)
                        .unwrap_or_else(Task::none),
                    // OS close button on the countdown is Cancel. The window
                    // is already gone, so this only settles the state (and
                    // any pending "Exit Hydra when done").
                    Some(WinKind::Power) => self.cancel_power_action(),
                    Some(WinKind::Update) => {
                        // OS close button is Cancel: stop an in-flight
                        // download and forget the offer (unless the finisher
                        // is already live — then the exit is imminent).
                        if self.updater.phase != UpdatePhase::Restarting {
                            self.cancel_update();
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
            // Window geometry reaches us divided by the View > Scale
            // factor, because that is the space the interface is laid out
            // in. Undo the ratio here so what is stored is in OS points:
            // that is the unit winit measures a window position in, and it
            // is what the remembered main-window size has to be for the
            // window to come back the same size at another font.
            Message::WinMoved(id, p) => {
                let s = self.ui_scale();
                let origin = Point::new(p.x * s, p.y * s);
                if self.main_id == Some(id) && !is_parked(origin) {
                    self.main_pos = Some(origin);
                    let moved = Some((origin.x, origin.y));
                    if self.cfg.settings.window_pos != moved {
                        self.cfg.settings.window_pos = moved;
                        self.save_config();
                    }
                }
                Task::none()
            }
            Message::WinResized(id, size) => {
                if self.main_id == Some(id) {
                    let s = self.ui_scale();
                    self.main_size = iced::Size::new(size.width * s, size.height * s);
                    // Remember it. Marking the config dirty rather than
                    // writing costs nothing per event — a drag delivers
                    // hundreds — and `flush_saves` puts it on disk within
                    // the second, so a session that ends without passing
                    // an exit path (a machine shutting down under a
                    // tray-resident app) still comes back the same size.
                    // The floor is `min_size`, in the same units the event
                    // arrives in: below it the size is not one the user
                    // could have dragged to.
                    let resized = Some((self.main_size.width, self.main_size.height));
                    if size.width >= main_min_w()
                        && size.height >= MAIN_MIN_H
                        && self.cfg.settings.window_size != resized
                    {
                        self.cfg.settings.window_size = resized;
                        self.save_config();
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
                        self.batch.category = model::DEFAULT_CATEGORY.into();
                        let open = self.open_window(WinKind::Batch);
                        let text = urls.join("\n");
                        Task::batch([open, self.update(Message::BatchLoaded(Some(text)))])
                    }
                }
            }
            Message::AnimTick => {
                if self.address_loading() {
                    self.add_url.loading_frame = (self.add_url.loading_frame + 1) % 12;
                }
                let fast = self.animation_interval_ms() == 33;
                let glide = if fast { 0.163 } else { 0.35 };
                // The indeterminate scan bar: one sweep every ~1.6 s, folded
                // at 2.0 so the block returns instead of snapping back.
                for st in self.scans.values_mut().filter(|s| s.running()) {
                    st.phase = (st.phase + if fast { 0.020625 } else { 0.05 }) % 2.0;
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
                    } else if diff
                        > if d
                            .plugin_plan
                            .as_ref()
                            .is_some_and(|info| info.plan.transfer.is_some())
                        {
                            0.000005
                        } else {
                            0.0005
                        }
                    {
                        d.disp_progress += diff * glide;
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
                if id == crate::tray::THEME_CHANGED {
                    crate::tray::refresh_icon();
                    return Task::none();
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
                self.speed_menu = false;
                self.header_ctx = None;
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
                self.speed_menu = false;
                self.header_ctx = None;
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
                        Some(s) if s.is_active() => {
                            self.sync_prog_state(id);
                            self.open_window(WinKind::Progress(id))
                        }
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
            Message::TableScrolled(offset, offset_x, viewport_h) => {
                self.table_scroll = offset;
                self.table_scroll_x = offset_x;
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
                    if let Some(pref) = self.column_mut(col) {
                        pref.width =
                            (start_w + (p.x - grab_x)).clamp(model::COL_MIN_W, model::COL_MAX_W);
                    }
                }
                self.drag_header_to(p.x);
                Task::none()
            }
            Message::Mods(m) => {
                self.mods = m;
                Task::none()
            }
            Message::RawKey(key, mods, win) => {
                // Escape backs out of an inline rename, leaving the queue
                // under its old name — the draft is only committed on Enter.
                if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                    && self.renaming_queue.is_some()
                {
                    self.renaming_queue = None;
                    return Task::none();
                }
                // Alt+F4 is the Windows quit convention, not a preference, so
                // it is not in the editable table. Windows sends the window a
                // close request of its own as well; quitting outright is what
                // the shortcut means, close-to-tray or not.
                if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::F4) && mods.alt() {
                    return self.update(Message::Menu(MenuAction::Exit));
                }
                // Escape is every dialog's Cancel and Enter its default
                // button, whichever one has the focus.
                if let iced::keyboard::Key::Named(named) = &key {
                    use iced::keyboard::key::Named;
                    if matches!(named, Named::Escape | Named::Enter) {
                        let kind = self.windows.get(&win).copied();
                        let msg = match (named, kind) {
                            (Named::Escape, Some(k)) => dialog_cancel(self, k, win),
                            (_, Some(k)) => dialog_primary(self, k, win),
                            (_, None) => None,
                        };
                        return match msg {
                            Some(m) => self.update(m),
                            None => Task::none(),
                        };
                    }
                }
                let combo = combo_string(&key, mods);
                let Some(combo) = combo else {
                    return Task::none();
                };
                let action = self
                    .cfg
                    .shortcuts
                    .iter()
                    .find(|(_, c)| model::normalize_combo(c).as_deref() == Some(combo.as_str()))
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
                    Some("select_all") => self.update(Message::SelectAll),
                    // Same path as the toolbar's Delete and the Downloads >
                    // Remove menu entry: it asks before anything is dropped.
                    Some("remove_selected") => self.update(Message::ToolbarDelete),
                    // Exactly what the window's own close button does, which
                    // is what Cmd+W means everywhere else.
                    Some("close_window") => self.update(Message::WindowCloseRequested(win)),
                    Some("quit") => {
                        self.update(resolve_quit_action(win, cfg!(target_os = "windows")))
                    }
                    _ => {
                        if cfg!(target_os = "windows") && combo == "ctrl+q" {
                            self.update(Message::WindowCloseRequested(win))
                        } else {
                            Task::none()
                        }
                    }
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
            Message::HeaderPress(col) => {
                self.header_drag = Some((col, self.cursor_now().x, false));
                Task::none()
            }
            Message::HeaderRightClick(col) => {
                // A press that opened the menu must not also sort on release.
                self.header_drag = None;
                self.header_ctx = Some(col);
                self.ctx_at = Some(self.cursor_now());
                Task::none()
            }
            Message::ColToggle(col) => {
                // File Name identifies the row; the table stays readable only
                // while it is there, so its entry is the one that cannot go.
                if col != Column::Name {
                    if let Some(p) = self.column_mut(col) {
                        p.visible = !p.visible;
                    }
                    self.save_config();
                }
                Task::none()
            }
            Message::ColMove(col, left) => {
                if model::move_column(&mut self.cfg.settings.columns, col, left, false) {
                    self.save_config();
                }
                Task::none()
            }
            Message::ColReset => {
                self.cfg.settings.columns = Column::ALL.into_iter().map(ColumnPref::new).collect();
                self.save_config();
                Task::none()
            }
            Message::QueueMenuOpen(start) => {
                self.queue_menu = Some(start);
                self.ctx_at = Some(self.cursor_now());
                Task::none()
            }
            Message::SpeedMenuOpen => {
                self.speed_menu = true;
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
                let prev = self.cfg.shortcuts.get(&action).cloned().unwrap_or_default();
                let completed = model::autocomplete_combo(&combo, &prev);
                self.cfg.shortcuts.insert(action, completed);
                self.save_config();
                Task::none()
            }
            Message::MouseUp => {
                if self.resizing.take().is_some() {
                    self.save_config();
                }
                // A header press that never became a drag is a click on the
                // title, which is what sorts the list; one that moved has
                // already reordered the columns as it went.
                if let Some((col, _, moved)) = self.header_drag.take() {
                    if moved {
                        self.save_config();
                    } else {
                        return self.update(Message::SortBy(col.into()));
                    }
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
                if let Some(pref) = self.column_mut(col) {
                    let w = pref.width;
                    self.resizing = Some((col, self.cursor_now().x, w));
                }
                Task::none()
            }
            Message::SortBy(key) => {
                if self.sort.0 == key {
                    self.sort.1 = !self.sort.1;
                } else {
                    self.sort = (key, true);
                }
                self.sync_native_menu();
                Task::none()
            }
            Message::SortDirection(asc) => {
                self.sort.1 = asc;
                self.sync_native_menu();
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
                    self.ask(ConfirmKind::DeleteItems(self.selected.clone()))
                } else {
                    Task::none()
                }
            }

            Message::AddrPrefill(text) => {
                if self.add_url.address.is_empty() {
                    if let Some(t) = text {
                        let t = t.trim();
                        if !t.contains('\n') && looks_downloadable(t, &self.cfg.settings.auto_types)
                        {
                            return self.update(Message::AddrChanged(t.to_string()));
                        }
                    }
                }
                Task::none()
            }
            Message::AddrChanged(s) => {
                if let Some(ctl) = &self.add_url.plugin_ctl {
                    ctl.cancel();
                }
                self.add_url.input_plugin = None;
                self.add_url.address = s;
                self.add_url.error = None;
                // An edited address no longer describes the manifest we
                // read; showing its quality list would be a lie.
                let addr = self.add_url.address.trim().to_string();
                if self.add_url.plugin_of != addr {
                    self.add_url.plugin_plan = None;
                    self.add_url.plugin_of.clear();
                }
                if self.add_url.stream_of != addr {
                    self.add_url.stream = None;
                    self.add_url.stream_of.clear();
                    self.add_url.stream_error = None;
                    self.add_url.quality = None;
                }
                if self.add_url.metalink_of != addr {
                    self.add_url.metalink = None;
                    self.add_url.metalink_of.clear();
                    self.add_url.metalink_error = None;
                }
                let mut tasks = vec![self.resize_open(WinKind::AddUrl)];
                // A new host is a new session. Whatever an earlier import
                // attached belonged to the previous one and goes NOW, before
                // any re-import answers: pressing OK in between must not send
                // it. Then re-import, if a browser is named. Typing out the
                // path of the same host is not a new host and reads nothing.
                if !same_host(&self.add_url.cookies_of, &addr) {
                    if self.add_url.cookies_imported {
                        self.add_url.capture.cookies = None;
                        self.add_url.capture.cookie_source = None;
                        self.add_url.cookie_note = None;
                        self.add_url.cookies_imported = false;
                        self.add_url.browser_cookies = None;
                    }
                    if !self.cfg.settings.cookies_from_browser.trim().is_empty() && !addr.is_empty()
                    {
                        tasks.push(self.update(Message::AddrImportCookies));
                    }
                }
                if !addr.is_empty() && self.add_url.plugin_of != addr {
                    tasks.push(self.update(Message::PluginProbe));
                }
                Task::batch(tasks)
            }
            Message::AddrCookies(v) => {
                let v = v.trim().to_string();
                let cookies = (!v.is_empty()).then(|| v.clone());
                let changed = self.add_url.capture.cookies != cookies
                    || self.add_url.browser_cookies.is_some();
                self.add_url.capture.cookies = cookies;
                self.add_url.browser_cookies = None;
                // Typed beats imported, and says so: the note under the field
                // would otherwise keep crediting a browser for a value the
                // user has since replaced.
                self.add_url.capture.cookie_source =
                    (!v.is_empty()).then(|| crate::i18n::tr("typed in the Add URL dialog"));
                self.add_url.cookie_note = None;
                self.add_url.cookies_imported = false;
                if changed {
                    self.add_url.plugin_probe_stale = true;
                    self.update(Message::PluginProbe)
                } else {
                    Task::none()
                }
            }
            Message::AddrImportCookies => {
                let url = self.add_url.address.trim().to_string();
                let spec = self.cfg.settings.cookies_from_browser.trim().to_string();
                if url.is_empty() || spec.is_empty() || self.add_url.cookies_importing {
                    return Task::none();
                }
                self.add_url.cookies_importing = true;
                self.add_url.cookies_of = url.clone();
                self.add_url.cookie_note = None;
                Task::perform(crate::engine::import_cookies(spec, url), |r| {
                    Message::AddrCookiesImported(Box::new(r))
                })
            }
            Message::AddrCookiesImported(result) => {
                self.add_url.cookies_importing = false;
                // The address moved to another host while this was running:
                // the answer is for a host no longer in the box, and applying
                // it would attach that host's session to this one. Ask again
                // for the address that is there now.
                if !same_host(&self.add_url.cookies_of, self.add_url.address.trim()) {
                    self.add_url.cookies_of.clear();
                    return self.update(Message::AddrImportCookies);
                }
                match *result {
                    // An empty header means the browser simply holds nothing
                    // for this host. That is an answer, not a failure, and
                    // overwriting a typed value with it would be a loss.
                    Ok(import) if !import.header.is_empty() => {
                        self.add_url.plugin_probe_stale = true;
                        self.add_url.capture.cookies = Some(import.header);
                        self.add_url.capture.cookie_source = Some(import.source.clone());
                        self.add_url.cookie_note = Some(import.source);
                        self.add_url.cookies_imported = true;
                        self.add_url.browser_cookies = Some(import.jar);
                    }
                    Ok(import) => self.add_url.cookie_note = Some(import.source),
                    Err(e) => self.add_url.cookie_note = Some(e),
                }
                Task::batch([
                    self.resize_open(WinKind::AddUrl),
                    self.update(Message::PluginProbe),
                ])
            }
            Message::OptCookieChecked(generation, result) => {
                if generation != self.options.cookie_check_gen {
                    return Task::none();
                }
                self.options.cookie_checking = false;
                self.options.cookie_check = Some(*result);
                self.resize_open(WinKind::Options)
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
                let cookies = self.add_url.capture.cookies.clone();
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
            Message::AddrProbeMetalink => {
                let src = self.add_url.address.trim().to_string();
                if src.is_empty() || self.add_url.metalink_probing {
                    return Task::none();
                }
                self.add_url.metalink_probing = true;
                self.add_url.metalink_error = None;
                self.add_url.metalink_of = src.clone();
                let ua = self.cfg.settings.user_agent.clone();
                Task::perform(crate::engine::probe_metalink(src, ua), |r| {
                    Message::AddrMetalinkProbed(Box::new(r))
                })
            }
            Message::AddrMetalinkProbed(result) => {
                self.add_url.metalink_probing = false;
                match *result {
                    Ok(doc) => self.add_url.metalink = Some(doc),
                    Err(e) => {
                        // Not a mirror list after all: fall back silently to an
                        // ordinary download rather than blocking the dialog.
                        self.add_url.metalink = None;
                        self.add_url.metalink_error = Some(e);
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
                Task::none()
            }
            Message::AddrLogin(s) => {
                self.add_url.login = s;
                Task::none()
            }
            Message::AddrPass(s) => {
                self.add_url.password = s;
                Task::none()
            }
            Message::AddUrlOk => self.add_url_ok(),

            Message::Ext(ev) => self.on_ext_event(ev),

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
            Message::FiSaveAs(s) => {
                self.file_info.set_save_as(&s);
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
            Message::FiBrowse => {
                let dl = self.file_info.dl;
                picker::save(
                    self.win_of(WinKind::FileInfo(dl)),
                    Ask {
                        file_name: Some(self.file_info.file_name.clone()),
                        ..Ask::in_dir(&self.file_info.save_dir)
                    },
                )
                .and_then(move |p| Task::done(Message::FiPathPicked(dl, picker::into_string(p))))
            }
            Message::FiPathPicked(dl, path) => {
                if self.file_info.dl == dl {
                    self.file_info.set_save_as(&path);
                }
                Task::none()
            }
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
            // Both can add or remove the line under the row, and the dialog
            // does not scroll: without the re-fit the sentence explaining the
            // address would sit below the window's bottom edge.
            Message::FiProxyPick(p) => {
                self.file_info.proxy_pick = p;
                self.resize_open(WinKind::FileInfo(self.file_info.dl))
            }
            Message::FiProxySpec(v) => {
                self.file_info.proxy_spec = v;
                self.resize_open(WinKind::FileInfo(self.file_info.dl))
            }
            Message::FiDownloadLater | Message::FiStartDownload | Message::FiOk => self
                .commit_file_info(
                    matches!(message, Message::FiStartDownload),
                    matches!(message, Message::FiDownloadLater),
                ),
            Message::FiCancel => {
                let fi = self.file_info.clone();
                let close = self.close_file_info_windows();
                if fi.is_new {
                    // The dialog was aborted: the auto-started item goes away
                    // with its temp data on cancel — and with the finished
                    // file too when the background transfer beat the user to
                    // Cancel. A small file that landed while the dialog was
                    // open used to stay on disk under the server's name, and
                    // the next capture of that link then warned that a file
                    // with this name already existed.
                    let done = self
                        .item(fi.dl)
                        .map(|d| d.state == DlState::Complete)
                        .unwrap_or(false);
                    Task::batch([close, self.delete_item_opts(fi.dl, done)])
                } else {
                    close
                }
            }

            Message::FiPreview => {
                let fi = &self.file_info;
                let dl = fi.dl;
                self.zip_preview = ZipPreviewState {
                    dl,
                    file_name: fi.file_name.clone(),
                    result: ZipPeek::Loading,
                };
                // The dialog's credentials, not the item's: an edited login
                // or cookie must reach the peek before it reaches the item.
                let auth =
                    (!fi.login.is_empty()).then_some((fi.login.as_str(), fi.password.as_str()));
                let referer = self.item(dl).and_then(|d| d.referer.clone());
                let headers = engine::request_headers(auth, Some(&fi.cookies), referer.as_deref());
                let url = fi.url.clone();
                let ua = self.cfg.settings.user_agent.clone();
                // The size the probe (or the transfer running behind the
                // dialog) already found saves the peek two round trips.
                let size = self.item(dl).and_then(|d| d.size);
                let proxy = self.item(dl).map(|d| d.proxy.clone()).unwrap_or_default();
                let peek =
                    Task::perform(engine::peek_zip(url, ua, headers, size, proxy), move |r| {
                        Message::ZipPeeked(dl, r)
                    });
                Task::batch([self.open_window(WinKind::ZipPreview(dl)), peek])
            }
            Message::ZipPeeked(dl, result) => {
                // A listing for a window since closed, or superseded by
                // another archive's, has nowhere to go.
                if self.zip_preview.dl == dl && self.win_of(WinKind::ZipPreview(dl)).is_some() {
                    self.zip_preview.result = match result {
                        Ok(entries) => ZipPeek::Listed(entries),
                        Err(why) => ZipPeek::Failed(why),
                    };
                }
                Task::none()
            }
            Message::ProgTabSet(id, tab) => {
                self.prog.entry(id).or_default().tab = tab;
                Task::none()
            }
            Message::ProgToggleDetails(id) => {
                let p = self.prog.entry(id).or_default();
                p.details = !p.details;
                // Collapse the dialog itself: no dead space below the
                // buttons when the details are hidden. `window_size` sizes
                // this box from the flag just flipped, so re-fitting the
                // window is the whole of the collapse.
                self.resize_open(WinKind::Progress(id))
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
                let p = self.prog.entry(id).or_default();
                p.limit_on = on;
                // A blank or zero box is not a cap; the box shows the number
                // the transfer is actually held to.
                let kb = p.limit_kb.parse().ok().filter(|kb| *kb > 0).unwrap_or(10);
                p.limit_kb = kb.to_string();
                if let Some(d) = self.item_mut(id) {
                    d.speed_limit = on.then_some(kb * 1024);
                }
                let lim = self.item(id).and_then(|d| d.speed_limit);
                engine::send(Cmd::SetLimit(id, lim));
                Task::none()
            }
            // The proxy is editable in this dialog only while the transfer is
            // stopped (the view withholds the handlers otherwise), so the
            // edit lands on the item and takes effect on the next start —
            // there is no running connection to move.
            Message::ProgProxyPick(id, pick) => {
                let p = self.prog.entry(id).or_default();
                p.proxy_pick = pick;
                let choice = ProxyChoice::from_parts(pick, &p.proxy_spec);
                if let Some(d) = self.item_mut(id) {
                    d.proxy = choice;
                }
                self.save_state();
                Task::none()
            }
            Message::ProgProxySpec(id, spec) => {
                let p = self.prog.entry(id).or_default();
                p.proxy_spec = spec;
                let choice = ProxyChoice::from_parts(p.proxy_pick, &p.proxy_spec);
                if let Some(d) = self.item_mut(id) {
                    d.proxy = choice;
                }
                self.save_state();
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
                    // Blank or zero mid-edit keeps the cap in force rather
                    // than lifting it: a box being retyped is not "unlimited".
                    let kb: Option<u64> = p.limit_kb.parse().ok().filter(|kb| *kb > 0);
                    if let (true, Some(kb)) = (p.limit_on, kb) {
                        if let Some(d) = self.item_mut(id) {
                            d.speed_limit = Some(kb * 1024);
                        }
                        engine::send(Cmd::SetLimit(id, Some(kb * 1024)));
                    }
                }
                Task::none()
            }
            Message::PowerTick => {
                let Some(p) = &mut self.power else {
                    return Task::none();
                };
                p.secs = p.secs.saturating_sub(1);
                if p.secs == 0 {
                    self.fire_power_action()
                } else {
                    Task::none()
                }
            }
            Message::PowerNow => self.fire_power_action(),
            Message::PowerCancel => self.cancel_power_action(),
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
            Message::ProgRemoveCompleted(b) => {
                self.cfg.settings.remove_completed = b;
                self.save_config();
                Task::none()
            }
            Message::ProgShowCompleteDialog(b) => {
                self.cfg.settings.show_complete_dialog = b;
                self.save_config();
                Task::none()
            }

            Message::OpenFile(id) => {
                if let Some(d) = self.item(id) {
                    let _ = open::that_detached(d.full_path());
                }
                let task = self.close_window(WinKind::Complete(id));
                self.complete_dismissed(id);
                task
            }
            Message::OpenFolder(id) => {
                if let Some(d) = self.item(id) {
                    crate::files::reveal(&d.full_path());
                }
                // Same as Open: the dialog has done its job once the user
                // has acted on the finished file, so it dismisses itself
                // instead of staying up behind the file manager.
                let task = self.close_window(WinKind::Complete(id));
                self.complete_dismissed(id);
                task
            }
            // Unlike Open, the dialog stays up: it redraws with the new path,
            // and Open / Open folder then act on the file where it now is.
            Message::MoveRename(id) => self.move_rename(id, self.win_of(WinKind::Complete(id))),

            Message::InstallPluginFile(path) => self.update(Message::InstallPluginSource(
                path.to_string_lossy().into_owned(),
            )),
            Message::InstallPluginSource(source) => {
                let open = self.open_options(Some(OptTab::Plugins));
                if self.options.plugins.busy {
                    self.options.plugins.pending_source = Some(source);
                    return open;
                }
                self.options.plugins.detail = None;
                self.options.plugins.prompt = None;
                self.options.plugins.review = None;
                self.options.plugins.prepared = None;
                self.options.plugins.welcome = None;
                self.options.plugins.path = source;
                let review = crate::plugins::update(self, crate::plugins::Message::Review);
                Task::batch([open, review])
            }
            Message::PluginBrowse(id, index) => {
                let action = self
                    .options
                    .plugins
                    .installed
                    .iter()
                    .find(|plugin| plugin.enabled && plugin.manifest.id == id)
                    .and_then(|plugin| plugin.manifest.input_actions.get(index))
                    .cloned();
                let Some(action) = action else {
                    return Task::none();
                };
                picker::file(
                    self.win_of(WinKind::AddUrl),
                    Ask {
                        title: Some(action.label.clone()),
                        owned_filter: Some((action.label, action.extensions)),
                        ..Default::default()
                    },
                )
                .map(move |path| Message::PluginFilePicked(id.clone(), path))
            }
            Message::PluginFilePicked(id, path) => {
                let Some(path) = path else {
                    return Task::none();
                };
                if self.win_of(WinKind::AddUrl).is_none() {
                    return Task::none();
                }
                match url::Url::from_file_path(path) {
                    Ok(url) => {
                        if let Some(ctl) = &self.add_url.plugin_ctl {
                            ctl.cancel();
                        }
                        self.add_url = AddUrlState {
                            address: url.into(),
                            input_plugin: Some(id),
                            plugin_ctl: self.add_url.plugin_ctl.clone(),
                            plugin_probing: self.add_url.plugin_probing,
                            ..Default::default()
                        };
                        self.update(Message::PluginProbe)
                    }
                    Err(()) => {
                        self.add_url.error = Some("Cannot open this file path.".into());
                        Task::none()
                    }
                }
            }
            Message::TransferFileSelected(index, selected) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    if let Some(transfer) = &info.plan.transfer {
                        if !transfer.files.iter().any(|file| file.index == index) {
                            return Task::none();
                        }
                        let indices = info.preferences.transfer_files.get_or_insert_with(|| {
                            transfer.files.iter().map(|file| file.index).collect()
                        });
                        indices.retain(|i| *i != index);
                        if selected {
                            indices.push(index);
                        }
                    }
                }
                Task::none()
            }
            Message::PluginProbeFinished(url, ctl, result) => {
                if !self
                    .add_url
                    .plugin_ctl
                    .as_ref()
                    .is_some_and(|current| std::sync::Arc::ptr_eq(current, &ctl))
                {
                    return Task::none();
                }
                self.update(Message::PluginProbed(url, result))
            }
            Message::PluginProbe => {
                if self.add_url.plugin_probe_stale {
                    self.add_url.plugin_plan = None;
                    self.add_url.plugin_of.clear();
                    self.add_url.error = None;
                }
                if self.add_url.plugin_probing || self.add_url.cookies_importing {
                    return Task::none();
                }
                let url = self.add_url.address.trim().to_string();
                if url.is_empty() {
                    return Task::none();
                }
                self.add_url.plugin_probing = true;
                self.add_url.plugin_probe_stale = false;
                let referer = self.add_url.capture.referer.clone();
                let cookies = self.add_url.capture.cookies.clone();
                let key = url.clone();
                let browser_cookies = self.add_url.browser_cookies.clone();
                let only = self.add_url.input_plugin.clone();
                let ctl = hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(150));
                self.add_url.plugin_ctl = Some(ctl.clone());
                let reply_ctl = ctl.clone();
                Task::perform(
                    crate::plugins::resolve(url, referer, cookies, browser_cookies, only, ctl),
                    move |r| {
                        Message::PluginProbeFinished(key.clone(), reply_ctl.clone(), Box::new(r))
                    },
                )
            }
            Message::PluginProbed(url, result) => {
                self.add_url.plugin_probing = false;
                if self.add_url.address.trim() != url
                    || self.add_url.plugin_probe_stale
                    || self.add_url.cookies_importing
                {
                    return self.update(Message::PluginProbe);
                }
                self.add_url.plugin_of = url.clone();
                match *result {
                    Ok(plan) => {
                        self.add_url.plugin_plan = plan;
                        self.add_url.error = None;
                        if self.add_url.plugin_plan.is_none() {
                            if crate::engine::metalink_address(&url)
                                && self.add_url.metalink_of != url
                            {
                                return self.update(Message::AddrProbeMetalink);
                            }
                            if manifest_address(&url) && self.add_url.stream_of != url {
                                return self.update(Message::AddrProbeStream);
                            }
                        }
                    }
                    Err(e) => self.add_url.error = Some(e),
                }
                self.resize_open(WinKind::AddUrl)
            }
            Message::PluginAudioOnly(value) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences.audio_only = value;
                    if value && info.preferences.audio == hya_plugin_api::AudioPref::None {
                        info.preferences.audio = hya_plugin_api::AudioPref::Best;
                    }
                }
                self.resize_open(WinKind::AddUrl)
            }
            Message::PluginAudioFormat(format) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences.audio_format = (format != "Original").then_some(format);
                }
                Task::none()
            }
            Message::PluginMaxHeight(height) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences.max_height = height;
                }
                Task::none()
            }
            Message::PluginPlaylistEntry(id, checked) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    let ids = info.preferences.playlist_ids.get_or_insert_with(|| {
                        info.plan
                            .entries
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect()
                    });
                    ids.retain(|entry| entry != &id);
                    if checked {
                        ids.push(id);
                    }
                }
                Task::none()
            }
            Message::PluginTrack(kind, id) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences
                        .track_ids
                        .retain(|id| info.plan.track(id).is_none_or(|t| t.kind != kind));
                    if id != "Best" && id != "None" {
                        info.preferences.track_ids.push(id.clone());
                    }
                    if kind == hya_plugin_api::TrackKind::Audio {
                        info.preferences.audio = if id == "None" {
                            hya_plugin_api::AudioPref::None
                        } else {
                            hya_plugin_api::AudioPref::Best
                        };
                    }
                }
                Task::none()
            }
            Message::PluginSubtitle(id, selected) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences.track_ids.retain(|value| value != &id);
                    if selected {
                        info.preferences.track_ids.push(id);
                    }
                }
                Task::none()
            }
            Message::PluginContainer(value) => {
                if let Some(info) = &mut self.add_url.plugin_plan {
                    info.preferences.container = Some(value);
                }
                Task::none()
            }
            Message::Plugin(message) => crate::plugins::update(self, message),
            Message::OptTabSet(t) => {
                self.options.tab = t;
                if t == OptTab::Plugins {
                    crate::plugins::load()
                } else {
                    Task::none()
                }
            }
            Message::OptExtStore(url) => {
                let _ = open::that_detached(url);
                Task::none()
            }
            Message::OptCopy(value) => iced::clipboard::write(value),
            Message::OptOk => {
                // A draft the transfers could not act on stays on screen,
                // on the page that holds the problem, with the reason beside
                // the buttons.
                if let Some((tab, why)) = self.options_problem() {
                    self.options.tab = tab;
                    self.options.error = Some(why);
                    return Task::none();
                }
                self.options.error = None;
                self.options.commit_cat_exts();
                let before = self.cfg.settings.clone();
                let (base, draft) = (self.options.base.clone(), self.options.draft.clone());
                self.cfg.settings.apply_options_draft(&base, &draft);
                self.cfg.categories = self.options.draft_cats.clone();
                self.apply_category_edits();
                self.save_config();
                let applied = self.settings_changed(&before);
                Task::batch([applied, self.close_window(WinKind::Options)])
            }
            Message::OptDraft(f) => self.on_opt_field(f),

            Message::UpdateChecked(res) => {
                let manual = std::mem::take(&mut self.updater.manual);
                match res {
                    Ok(Some(info)) => {
                        crate::log::info(&format!("update available: {}", info.version));
                        self.updater = UpdateUiState {
                            info: Some(info),
                            generation: self.updater.generation,
                            ..UpdateUiState::default()
                        };
                        self.open_window(WinKind::Update)
                    }
                    Ok(None) if manual => self.ask(ConfirmKind::UpToDate),
                    Ok(None) => Task::none(),
                    Err(e) if manual => {
                        crate::log::warn(&format!("update check failed: {e}"));
                        self.ask(ConfirmKind::UpdateCheckFailed(e))
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
                // The dialog does not offer this for a packaged install or
                // a release with nothing for this machine; a shortcut or a
                // stale frame must not start a download the finisher would
                // then be unable to apply.
                if !info.in_place || !info.has_bundle {
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
                self.updater.generation += 1;
                let generation = self.updater.generation;
                Task::run(crate::update::run(info, cancel), move |ev| {
                    Message::UpdateEvent(generation, ev)
                })
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
                    // The finisher is live and waiting for this process to
                    // exit; too late to stop.
                    UpdatePhase::Restarting => Task::none(),
                    // Verifying/Preparing: the flag stops the run before the
                    // finisher is spawned, and the generation bump makes sure
                    // a ReadyToRestart that slipped through cannot exit the
                    // app the user just chose to keep.
                    _ => {
                        self.cancel_update();
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
            Message::UpdateEvent(generation, ev) => {
                use crate::update::UpdateEvent as Ev;
                if generation != self.updater.generation {
                    crate::log::info(&format!("update: ignoring {ev:?} from a cancelled run"));
                    return Task::none();
                }
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
                let name = self.sch_queue();
                // set_queue_running promotes paused members back to Queued.
                self.set_queue_running(&name, true);
                self.queue_tick()
            }
            Message::SchStop => {
                let name = self.sch_queue();
                self.set_queue_running(&name, false);
                Task::none()
            }
            Message::SchSave => {
                // The fields write through as they are edited, so there is
                // nothing here to commit — what this does is put the queue on
                // disk now instead of at the next tick, and close the window,
                // which is the acknowledgement a dialog that saves as you
                // type otherwise never gives. The times are the exception:
                // typed freely, spelled `HH:MM` once the dialog is done.
                for q in &mut self.cfg.queues {
                    q.schedule.start_at = model::normalize_hhmm(&q.schedule.start_at);
                    q.schedule.stop_at = model::normalize_hhmm(&q.schedule.stop_at);
                }
                self.save_config();
                self.flush_saves();
                self.close_window(WinKind::Scheduler)
            }
            Message::SchNewQueue => {
                let name = model::free_queue_name(&self.cfg.queues, &i18n::tr("Queue"));
                let color = Some(crate::model::pick_queue_color(&self.cfg.queues));
                self.cfg.queues.push(crate::model::QueueDef {
                    name: name.clone(),
                    files_at_once: 4,
                    schedule: crate::model::Schedule::default(),
                    builtin: false,
                    color,
                    running: false,
                    did_work: false,
                });
                self.sch.rename_draft = name.clone();
                self.sch.queue = name;
                self.save_config();
                Task::none()
            }
            Message::SchDeleteQueue => {
                let name = self.sch_queue();
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

            Message::BatchEdit(a) => {
                let edited = a.is_edit();
                self.batch.text.perform(a);
                self.batch.parsed = false;
                self.parse_batch();
                if !edited {
                    return Task::none();
                }
                // Not measured on the spot: a link typed by hand walks
                // through a dozen parseable prefixes on its way in, and each
                // one would put a request on the wire. Wait for the box to
                // stand still, then measure what it settled on — a paste,
                // which is one edit, is measured as soon as the wait is up.
                self.batch.edit_gen += 1;
                let gen = self.batch.edit_gen;
                Task::future(async move {
                    tokio::time::sleep(BATCH_PROBE_IDLE).await;
                    Message::BatchProbeIdle(gen)
                })
            }
            Message::BatchProbeIdle(gen) => {
                // A later edit (or a new dialog, which resets the counter)
                // has its own wait running; this one is stale.
                if gen != self.batch.edit_gen {
                    return Task::none();
                }
                self.probe_batch()
            }
            Message::BatchLoaded(Some(text)) => {
                self.batch
                    .text
                    .perform(iced::widget::text_editor::Action::Edit(
                        iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                    ));
                self.batch.parsed = false;
                self.parse_batch();
                self.probe_batch()
            }
            Message::BatchLoaded(None) => Task::none(),
            Message::InfoProbed(id, meta) => match meta {
                Some(meta) => self.adopt_probed(id, meta.file_name, meta.size),
                None => Task::none(),
            },
            Message::BatchProbed(url, meta) => {
                if let Some(meta) = meta {
                    // A redirector URL reveals itself only here. Reading the
                    // document now is what lets the batch add every file it
                    // describes, rather than the engine falling back to the
                    // first one at transfer time.
                    if meta.is_metalink && !self.batch.metalinks.contains_key(&url) {
                        let ua = self.cfg.settings.user_agent.clone();
                        return Task::perform(engine::probe_metalink(url.clone(), ua), move |r| {
                            Message::BatchMetalinkProbed(url.clone(), Box::new(r.ok()))
                        });
                    }
                    if let Some(size) = meta.size {
                        self.batch.sizes.insert(url.clone(), size);
                    }
                    if let Some(name) = meta.file_name {
                        self.batch.names.insert(url, name);
                    }
                }
                Task::none()
            }
            Message::BatchMetalinkProbed(url, doc) => {
                match *doc {
                    Some(doc) => {
                        // Show what the list will actually add, so the row does
                        // not read as one 6 KB XML file. The size column gets
                        // the TOTAL, because that is what the batch is about to
                        // pull down.
                        let total: u64 = doc.files.iter().filter_map(|f| f.info.size).sum();
                        if total > 0 {
                            self.batch.sizes.insert(url.clone(), total);
                        }
                        self.batch.names.insert(
                            url.clone(),
                            if doc.files.len() == 1 {
                                doc.files[0].name.clone()
                            } else {
                                format!("{} ({} files)", i18n::tr("Mirror list"), doc.files.len())
                            },
                        );
                        self.batch.metalinks.insert(url, doc);
                    }
                    // Not a mirror list after all — a `.meta4` that 404s, or a
                    // document this build cannot use. Fill the row in from the
                    // URL alone rather than re-probing: `BatchProbed` routes a
                    // metalink `Content-Type` straight back here, so a document
                    // that parses badly but is served correctly would bounce
                    // between the two forever.
                    None => {
                        self.batch
                            .names
                            .entry(url.clone())
                            .or_insert_with(|| engine::file_name_from_url(&url));
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
                // Only the rows on screen: "Check All" with web pages hidden
                // must not quietly opt the hidden pages in.
                let shown: Vec<usize> = self.batch_rows().iter().map(|r| r.idx).collect();
                for i in shown {
                    if let Some(c) = self.batch.checks.get_mut(i) {
                        c.1 = b;
                    }
                }
                Task::none()
            }
            Message::BatchRowClick(i) => {
                if self.mods.command() || self.mods.control() {
                    if !self.batch.sel.remove(&i) {
                        self.batch.sel.insert(i);
                    }
                    self.batch.sel_anchor = Some(i);
                    return Task::none();
                }
                let order: Vec<usize> = self.batch_rows().iter().map(|r| r.idx).collect();
                if self.mods.shift() {
                    // Run between the anchor and the click in the order the
                    // table is showing, which is what the user is pointing at:
                    // sorted by size, a range is a size range.
                    //
                    // A filter can have hidden the anchor since it was set;
                    // the click then starts a run of its own rather than
                    // silently selecting nothing.
                    let anchor = self
                        .batch
                        .sel_anchor
                        .filter(|a| order.contains(a))
                        .unwrap_or(i);
                    let a = order.iter().position(|x| *x == anchor);
                    let b = order.iter().position(|x| *x == i);
                    if let (Some(a), Some(b)) = (a, b) {
                        self.batch.sel = order[a.min(b)..=a.max(b)].iter().copied().collect();
                        self.batch.sel_anchor = Some(anchor);
                    }
                    return Task::none();
                }
                self.batch.sel = std::iter::once(i).collect();
                self.batch.sel_anchor = Some(i);
                Task::none()
            }
            Message::BatchCheckSel(b) => {
                for i in self.batch.sel.clone() {
                    if let Some(c) = self.batch.checks.get_mut(i) {
                        c.1 = b;
                    }
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
            Message::BatchSort(k) => {
                // Same column again flips the direction, like the main list.
                self.batch.sort = Some(match self.batch.sort {
                    Some((cur, asc)) if cur == k => (k, !asc),
                    _ => (k, true),
                });
                Task::none()
            }
            Message::BatchHideHtml(b) => {
                self.batch.hide_html = b;
                self.batch_prune_sel();
                Task::none()
            }
            Message::BatchHideDups(b) => {
                self.batch.hide_dups = b;
                self.batch_prune_sel();
                Task::none()
            }
            Message::BatchBrowseDir => picker::folder(self.win_of(WinKind::Batch), Ask::default())
                .and_then(|p| Task::done(Message::BatchDirPicked(picker::into_string(p)))),
            // Choosing a folder is also what says where the batch goes: the
            // "to this folder" mode is what the user meant by browsing, not
            // a second checkbox to remember afterwards.
            Message::BatchDirPicked(dir) => {
                self.batch.dir = dir;
                self.batch.to_dir = true;
                self.batch.to_category = false;
                Task::none()
            }
            Message::BatchOk => self.batch_ok(),

            Message::ConfirmYes => {
                let kind = self.confirm.take();
                let remove_file = self.confirm_remove_file;
                let close = self.dismiss_confirm();
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
                    Some(ConfirmKind::TrustExtension(origin)) => {
                        self.trust_extension(origin);
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
                let close = self.dismiss_confirm();
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
                let close = self.dismiss_confirm();
                match existing {
                    Some(id) => {
                        self.selected = vec![id];
                        // A finished entry has nothing to resume: starting
                        // it again would fetch the file over the copy that
                        // is already there. Show it instead.
                        if self.item(id).is_some_and(|d| d.state == DlState::Complete) {
                            return close;
                        }
                        Task::batch([close, self.start_download(id, true)])
                    }
                    None => close,
                }
            }
            Message::DupOpen => {
                if let Some(ConfirmKind::Duplicate { existing, file, .. }) = self.confirm.take() {
                    let path = file
                        .map(std::path::PathBuf::from)
                        .or_else(|| existing.and_then(|id| self.item(id)).map(|d| d.full_path()));
                    if let Some(p) = path {
                        let _ = open::that_detached(p);
                    }
                }
                self.dismiss_confirm()
            }
            Message::DupNew => {
                let pending = match self.confirm.take() {
                    Some(ConfirmKind::Duplicate { pending, .. }) => Some(pending),
                    _ => None,
                };
                let close = self.dismiss_confirm();
                let Some(pending) = pending else {
                    return close;
                };
                let id = self.add_item(pending.url, pending.auth, None);
                self.apply_capture_extras(id, pending.capture);
                if let Some(info) = pending.plugin_plan {
                    let name = crate::plugins::file_name(&info);
                    self.name_new_item(id, name);
                    self.apply_plugin_plan(id, info);
                }
                // `name_1.ext`, `name_2.ext`, ... until it collides with
                // neither the disk nor another list entry. Locked, because
                // this copy exists precisely so the file already on disk is
                // not written over: a probe adopting the server's name back
                // would aim the transfer straight at it again.
                if let Some(d) = self.item(id) {
                    let unique =
                        unique_file_name(&d.save_dir, &d.file_name, &self.state.downloads, id);
                    if let Some(d) = self.item_mut(id) {
                        d.file_name = unique;
                        d.name_locked = true;
                    }
                }
                self.save_state();
                let task = self.offer_new_item(id, close);
                // When that opened the dialog, the name in it is the user's:
                // the probe must not hand it back to the server's suggestion.
                if self.file_info.dl == id {
                    self.file_info.name_touched = true;
                }
                task
            }
            Message::MoveRenameTo(id, to) => self.move_rename_to(id, to),
            Message::ExportUrlsTo(path) => {
                if let Some(path) = path {
                    let urls: Vec<&str> = self
                        .state
                        .downloads
                        .iter()
                        .map(|d| d.url.as_str())
                        .collect();
                    if let Err(e) = std::fs::write(&path, urls.join("\n")) {
                        crate::log::warn(&format!("export {}: {e}", path.display()));
                    }
                }
                Task::none()
            }
            Message::ExportSettingsTo(path) => match path {
                Some(path) => self.export_settings(&path),
                None => Task::none(),
            },
            Message::ImportSettingsFrom(path) => match path {
                Some(path) => self.import_settings(&path),
                None => Task::none(),
            },
            Message::SaveDirRegranted(id, dir) => {
                self.save_dir_regranted(id, dir);
                self.start_download(id, false)
            }
            Message::CloseThis(id) => {
                match self.windows.remove(&id) {
                    Some(WinKind::Confirm) => {
                        let next = self.next_confirm();
                        return Task::batch([window::close(id), next]);
                    }
                    Some(WinKind::AddUrl) => {
                        if let Some(ctl) = &self.add_url.plugin_ctl {
                            ctl.cancel();
                        }
                    }
                    Some(WinKind::Shortcuts) => self.normalize_shortcuts(),
                    Some(WinKind::Complete(dl)) => self.complete_dismissed(dl),
                    // The countdown is normally dismissed by its own Cancel
                    // button; reaching it through the generic close path
                    // must call the action off just the same. `WindowClosed`
                    // cannot: the entry is gone from `windows` by then.
                    Some(WinKind::Power) => {
                        let cancelled = self.cancel_power_action();
                        return Task::batch([cancelled, window::close(id)]);
                    }
                    // Back to the dialog the listing was opened from.
                    Some(WinKind::ZipPreview(dl)) => {
                        if let Some(fi) = self.win_of(WinKind::FileInfo(dl)) {
                            return Task::batch([window::close(id), window::gain_focus(fi)]);
                        }
                    }
                    _ => {}
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
        if self.item(id).is_some_and(|item| {
            item.plugin_plan
                .as_ref()
                .is_some_and(|info| info.plan.transfer.is_some())
        }) {
            return Task::none();
        }
        if self.cfg.settings.bg_download && self.auto_start_type(id) {
            return self.start_download(id, false);
        }
        let ua = self.cfg.settings.user_agent.clone();
        // The item's own route, not the app's: a capture that arrived with
        // the browser's proxy is reachable through that proxy and nowhere
        // else, and a probe that leaves by a different door reports a file
        // of unknown size the transfer then downloads perfectly well.
        let (url, headers, proxy) = self
            .item(id)
            .map(|d| {
                (
                    d.url.clone(),
                    engine::request_headers(
                        d.auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                        d.cookies.as_deref(),
                        d.referer.as_deref(),
                    ),
                    d.proxy.clone(),
                )
            })
            .unwrap_or_default();
        Task::perform(engine::probe_link(url, ua, headers, proxy), move |meta| {
            Message::InfoProbed(id, meta)
        })
    }

    /// Measure every link in the box that has not been measured yet, in the
    /// background ("you may wait until it checks and fills all file types").
    ///
    /// Runs for any route the links arrived by — a pasted list, a `.txt`, the
    /// clipboard item — because the Size and File Name columns are filled by
    /// this and nothing else.
    fn probe_batch(&mut self) -> Task<Message> {
        let ua = self.cfg.settings.user_agent.clone();
        let probes: Vec<Task<Message>> = self
            .batch
            .take_unprobed()
            .into_iter()
            .map(|url| {
                let ua = ua.clone();
                // A mirror list answers a different question than a size
                // probe: not "how big is this file" but "which files are
                // these, and where else do they live". One request either
                // way, so it replaces the size probe rather than being added
                // to it.
                if engine::metalink_address(&url) {
                    Task::perform(engine::probe_metalink(url.clone(), ua), move |r| {
                        Message::BatchMetalinkProbed(url.clone(), Box::new(r.ok()))
                    })
                } else {
                    // A pasted link has no item, so the app-wide route is the
                    // only one there is.
                    Task::perform(
                        engine::probe_link(url.clone(), ua, vec![], ProxyChoice::Default),
                        move |meta| Message::BatchProbed(url.clone(), meta),
                    )
                }
            })
            .collect();
        Task::batch(probes)
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
            .filter_map(url_in_line)
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
        // A highlight is a position in this list, so a line added or removed
        // above it would leave it pointing at another link. Checked state does
        // survive the edit: it is carried over by URL, just above.
        if self
            .batch
            .checks
            .iter()
            .map(|(u, _)| u)
            .ne(existing.iter().map(|(u, _)| u))
        {
            self.batch.sel.clear();
            self.batch.sel_anchor = None;
        }
        self.batch.parsed = true;
    }

    /// Drop from the batch selection every row the table is no longer
    /// showing: "Check Selected" must not reach a link hidden behind a
    /// filter, the same rule "Check All" already follows.
    fn batch_prune_sel(&mut self) {
        let shown: std::collections::HashSet<usize> =
            self.batch_rows().iter().map(|r| r.idx).collect();
        self.batch.sel.retain(|i| shown.contains(i));
    }

    fn on_menu(&mut self, action: MenuAction) -> Task<Message> {
        match action {
            MenuAction::AddNewDownload => {
                // Already up: bring it forward with whatever is typed in it.
                if self.win_of(WinKind::AddUrl).is_some() {
                    return self.open_window(WinKind::AddUrl);
                }
                self.add_url = AddUrlState::default();
                // Pre-fill the address when the clipboard holds a
                // plausible download link.
                Task::batch([
                    self.open_window(WinKind::AddUrl),
                    iced::clipboard::read().map(Message::AddrPrefill),
                ])
            }
            MenuAction::AddBatch => {
                if self.win_of(WinKind::Batch).is_some() {
                    return self.open_window(WinKind::Batch);
                }
                self.batch = BatchState::default();
                self.batch.category = model::DEFAULT_CATEGORY.into();
                self.open_window(WinKind::Batch)
            }
            MenuAction::AddBatchClipboard => {
                self.batch = BatchState::default();
                self.batch.category = model::DEFAULT_CATEGORY.into();
                let open = self.open_window(WinKind::Batch);
                // Through `BatchLoaded`, like the `.txt` picker and the
                // clipboard monitor: pasting the text into the editor is only
                // half of taking a list in — the other half is measuring it.
                Task::batch([open, iced::clipboard::read().map(Message::BatchLoaded)])
            }
            MenuAction::AddBatchFile => {
                self.batch = BatchState::default();
                self.batch.category = model::DEFAULT_CATEGORY.into();
                let open = self.open_window(WinKind::Batch);
                let ask = Ask {
                    filter: Some(("Text", &["txt", "text", "lst"])),
                    ..Ask::default()
                };
                let pick = picker::file(self.win_of(WinKind::Batch), ask)
                    .map(|p| Message::BatchLoaded(p.and_then(|p| std::fs::read_to_string(p).ok())));
                // Chained, not batched: the list window owns the picker, and
                // `open_window` has only reserved its id at this point — the
                // window itself arrives when the runtime has created it.
                open.chain(pick)
            }
            MenuAction::SiteGrabber | MenuAction::DropTarget | MenuAction::Find => Task::none(),
            MenuAction::ExportUrls => {
                let ask = Ask {
                    file_name: Some("hydra-downloads.txt".into()),
                    filter: Some(("Text", &["txt"])),
                    ..Ask::default()
                };
                picker::save(self.win_of(WinKind::Main), ask).map(Message::ExportUrlsTo)
            }
            MenuAction::ExportSettings => {
                let ask = Ask {
                    file_name: Some(format!("hydra-settings.{}", hydata::EXTENSION)),
                    filter: Some(("Hydra settings", &[hydata::EXTENSION])),
                    ..Ask::default()
                };
                picker::save(self.win_of(WinKind::Main), ask).map(Message::ExportSettingsTo)
            }
            MenuAction::ImportSettings => {
                let ask = Ask {
                    filter: Some(("Hydra settings", &[hydata::EXTENSION])),
                    ..Ask::default()
                };
                picker::file(self.win_of(WinKind::Main), ask).map(Message::ImportSettingsFrom)
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
                        if let Some(d) = self.item_mut(id) {
                            reset_for_redownload(d);
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
            MenuAction::DeleteAllCompleted => self.ask(ConfirmKind::DeleteCompleted),
            MenuAction::Scheduler => {
                self.sch_queue();
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
                self.apply_speed_limiter();
                self.save_config();
                self.sync_native_menu();
                Task::none()
            }
            MenuAction::SpeedProfile(name) => {
                let Some(limit) = self
                    .cfg
                    .settings
                    .speed_profiles
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| p.limit)
                else {
                    return Task::none();
                };
                self.cfg.settings.speed_limiter_on = limit.is_some();
                // An unlimited profile switches the limiter off and leaves
                // the number alone, so the Speed Limit button can put the
                // last cap back without a trip through the profile list.
                if limit.is_some() {
                    self.cfg.settings.global_speed_limit = limit;
                }
                self.apply_speed_limiter();
                self.save_config();
                self.sync_native_menu();
                Task::none()
            }
            MenuAction::SpeedLimitSettings => self.open_options(Some(OptTab::SpeedLimit)),
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
            MenuAction::HideToolbarText => {
                self.cfg.settings.show_toolbar_labels = !self.cfg.settings.show_toolbar_labels;
                self.save_config();
                self.sync_native_menu();
                Task::none()
            }
            MenuAction::ArrangeBy(key) => self.update(Message::SortBy(key)),
            MenuAction::SortDirection(asc) => self.update(Message::SortDirection(asc)),
            MenuAction::ManageColumns => self.open_window(WinKind::Columns),
            MenuAction::ToggleColumn(col) => self.update(Message::ColToggle(col)),
            MenuAction::MoveColumn(col, left) => {
                // From the header, where only shown columns are next to each
                // other; the dialog moves within its own full list instead.
                if model::move_column(&mut self.cfg.settings.columns, col, left, true) {
                    self.save_config();
                }
                Task::none()
            }
            MenuAction::SetTheme(mode) => {
                let changed = self.cfg.settings.theme() != mode;
                self.cfg.settings.theme_mode = Some(mode);
                // Like View > Scale: even a click on the mode already in use
                // has to be written back to the menu, because muda unticked
                // it on the way in.
                self.sync_native_menu();
                if changed {
                    self.save_config();
                }
                Task::none()
            }
            MenuAction::UiScale(pct) => {
                let changed = self.cfg.settings.ui_scale_pct != pct;
                self.cfg.settings.ui_scale_pct = pct;
                // Even a click on the scale already in use has to be written
                // back to the menu: muda unticked it on the way in.
                self.sync_native_menu();
                if !changed {
                    return Task::none();
                }
                self.save_config();
                // The new ratio reaches the interface on the next redraw,
                // but a dialog is sized for the ratio it opened at: resize
                // the fixed ones now, or the extra rows a larger scale needs
                // have nowhere to go until the dialog is reopened.
                let mut tasks: Vec<Task<Message>> = self
                    .windows
                    .iter()
                    .filter(|(_, k)| **k != WinKind::Main)
                    .map(|(id, kind)| {
                        let (w, h) = self.window_size(*kind);
                        let resize = window::resize(*id, iced::Size::new(w, h));
                        if matches!(kind, WinKind::Progress(_)) {
                            Task::batch([
                                window::set_min_size(*id, Some(iced::Size::new(w, h))),
                                resize,
                            ])
                        } else {
                            resize
                        }
                    })
                    .collect();
                // The main window keeps its size but not its floor, which was
                // fixed at creation for the old ratio. Unlike `min_size` there,
                // `set_min_size` is converted by iced through the new ratio
                // itself, so it takes interface units: scaling it here too
                // squared the ratio and forced the window past the screen.
                if let Some(id) = self.main_id {
                    tasks.push(window::set_min_size(
                        id,
                        Some(iced::Size::new(main_min_w(), MAIN_MIN_H)),
                    ));
                }
                Task::batch(tasks)
            }
            MenuAction::Language(l) => {
                // The face is chosen from the locale, but the renderer took
                // its default font when the window system came up: a switch
                // onto (or off) the bundled Persian/Arabic face only reaches
                // the interface on the next launch, and the user has to be
                // told rather than left looking at the wrong one.
                let face_changes =
                    crate::font::changes_face(self.cfg.language.as_deref(), Some(&l));
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
                // The new labels reach the toolbar on the next redraw, and
                // the row is what the main window's floor is measured from:
                // without this a window at the old floor cannot be widened
                // to fit the tools the longer language needs.
                let floor = self.main_id.map(|id| {
                    let scale = self.ui_scale();
                    window::set_min_size(
                        id,
                        Some(iced::Size::new(main_min_w() * scale, MAIN_MIN_H * scale)),
                    )
                });
                if face_changes {
                    let confirm = self.ask(ConfirmKind::FontNeedsRestart);
                    return match floor {
                        Some(floor) => Task::batch([floor, confirm]),
                        None => confirm,
                    };
                }
                floor.unwrap_or_else(Task::none)
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
                let on = !self.cfg.settings.power_save;
                self.cfg.settings.power_save = on;
                self.save_config();
                self.set_power_save(on);
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
                    crate::files::reveal(&d.full_path());
                }
                Task::none()
            }
            MenuAction::OpenWithSel => match self.selected_item() {
                Some(d) => crate::files::open_with(self.win_of(WinKind::Main), &d.full_path()),
                None => Task::none(),
            },
            MenuAction::MoveRenameSel => match self.selected_item().map(|d| d.id) {
                Some(id) => self.move_rename(id, self.win_of(WinKind::Main)),
                None => Task::none(),
            },
            MenuAction::Properties => {
                let fi = self.selected_item().map(|d| FileInfoState {
                    dl: d.id,
                    category: d
                        .category
                        .clone()
                        .unwrap_or_else(|| model::DEFAULT_CATEGORY.into()),
                    save_dir: d.save_dir.clone(),
                    file_name: d.file_name.clone(),
                    description: d.description.clone(),
                    remember: false,
                    is_new: false,
                    url: d.url.clone(),
                    login: d.auth.clone().map(|a| a.0).unwrap_or_default(),
                    password: d.auth.clone().map(|a| a.1).unwrap_or_default(),
                    cookies: d.cookies.clone().unwrap_or_default(),
                    proxy_pick: d.proxy.pick(),
                    proxy_spec: d.proxy.spec().to_string(),
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

    /// Ask whether the browser Options names can actually be read.
    ///
    /// Answered here, when the choice is made, rather than when a download
    /// needs it: the common failure is a permission the user has to grant
    /// elsewhere, and finding that out one download at a time is finding it out
    /// in the wrong place. Does not decrypt, so choosing a Chromium in a
    /// dropdown cannot put a Keychain prompt on screen.
    fn check_cookie_source(&mut self) -> Task<Message> {
        let spec = self.options.draft.cookies_from_browser.trim().to_string();
        self.options.cookie_check = None;
        self.options.cookie_check_gen += 1;
        if spec.is_empty() {
            self.options.cookie_checking = false;
            return Task::none();
        }
        self.options.cookie_checking = true;
        let generation = self.options.cookie_check_gen;
        Task::perform(crate::engine::check_cookie_source(spec), move |r| {
            Message::OptCookieChecked(generation, Box::new(r))
        })
    }

    fn on_opt_field(&mut self, f: OptField) -> Task<Message> {
        match f {
            OptField::BrowseVirus => picker::file(self.win_of(WinKind::Options), Ask::default())
                .map(|p| Message::OptDraft(OptField::VirusPicked(p.map(picker::into_string)))),
            OptField::BrowseCatDir => picker::folder(self.win_of(WinKind::Options), Ask::default())
                .map(|p| Message::OptDraft(OptField::CatDirPicked(p.map(picker::into_string)))),
            OptField::SoundBrowse(i) => {
                let ask = Ask {
                    filter: Some(("Audio", &["wav", "ogg"])),
                    ..Ask::default()
                };
                picker::file(self.win_of(WinKind::Options), ask).and_then(move |p| {
                    Task::done(Message::OptDraft(OptField::SoundPicked(
                        i,
                        picker::into_string(p),
                    )))
                })
            }
            f @ (OptField::CookiesBrowser(_) | OptField::CookiesProfile(_)) => {
                self.options.apply(f);
                self.check_cookie_source()
            }
            f => {
                self.options.apply(f);
                Task::none()
            }
        }
    }

    fn on_sch_field(&mut self, f: SchField) {
        let name = self.sch_queue();
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
                if let Ok(n) = v.parse::<u32>() {
                    s.retries = crate::model::clamp_to(n, crate::model::QUEUE_RETRIES);
                }
            }
            SchField::OpenFileEnabled(b) => s.open_file_enabled = b,
            SchField::OpenFile(v) => s.open_file = v,
            SchField::ExitDone(b) => s.exit_when_done = b,
            SchField::ShutdownDone(b) => s.shutdown_when_done = b,
            SchField::ShutdownAction(a) => s.shutdown_action = a,
            SchField::FilesAtOnce(v) => {
                if let Ok(n) = v.parse::<u32>() {
                    q.files_at_once = crate::model::clamp_to(n, crate::model::FILES_AT_ONCE);
                }
            }
        }
    }
}

/// Default main-window size: scaled from the primary display's logical
/// resolution at the reference ratio (1009x606 on a 1512x982 screen — i.e.
/// two thirds of the width, ~62% of the height), clamped to the layout's
/// [`main_min_w`]x[`MAIN_MIN_H`] floor. Falls back to 1009x606 when the
/// display cannot be queried.
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

/// Shuts down, logs off or sleeps the machine for a "when done" action.
/// Errors (no permission, an unsupported desktop session) only get a log
/// line — by the time this runs, the queue or download it was guarding is
/// already done.
fn run_power_action(action: PowerAction) {
    let result = match action {
        PowerAction::Shutdown => system_shutdown::shutdown(),
        PowerAction::LogOff => system_shutdown::logout(),
        PowerAction::Sleep => system_shutdown::sleep(),
    };
    if let Err(e) = result {
        crate::log::warn(&format!("power action {action:?} failed: {e}"));
    }
}

/// The primary display in OS points, or `None` when the platform will not
/// say. Read once at startup: enumerating displays is a system call, and
/// the two things that consult it — the first-run main window size and the
/// dialog cap — only need it to be roughly right.
pub fn display_points() -> Option<iced::Size> {
    let displays = display_info::DisplayInfo::all().ok()?;
    let d = displays
        .iter()
        .find(|d| d.is_primary)
        .or(displays.first())?;
    Some(display_rect(d).size())
}

/// Every connected display's bounds in OS points, empty when the platform
/// will not say.
fn display_bounds() -> Vec<iced::Rectangle> {
    display_info::DisplayInfo::all()
        .map(|displays| displays.iter().map(display_rect).collect())
        .unwrap_or_default()
}

fn display_rect(d: &display_info::DisplayInfo) -> iced::Rectangle {
    display_normalized(
        iced::Rectangle::new(
            Point::new(d.x as f32, d.y as f32),
            iced::Size::new(d.width as f32, d.height as f32),
        ),
        d.scale_factor,
    )
}

/// One display's reported bounds in OS points. Some backends answer in
/// physical pixels and some already in points, and nothing in the reply
/// says which: a reported width that is still a desktop's worth of points
/// after dividing is the physical one, and a screen under 1000 points wide
/// either way is left alone rather than halved into a phone.
fn display_normalized(r: iced::Rectangle, scale: f32) -> iced::Rectangle {
    if scale > 1.0 && r.width / scale >= 1000.0 {
        iced::Rectangle {
            x: r.x / scale,
            y: r.y / scale,
            width: r.width / scale,
            height: r.height / scale,
        }
    } else {
        r
    }
}

/// Narrowest the main window may get, in interface units: enough for the
/// whole toolbar row, which does not wrap — anything past the right edge is
/// clipped, and a clipped tool is one the user cannot reach.
///
/// The floor is the row as this locale actually draws it. 1050 was measured
/// against the English labels, and every language that writes "Delete
/// Completed" as one long compound overran it — so the last tools were
/// squeezed to nothing and their labels drawn over each other on a window
/// nobody could narrow any further.
pub const MAIN_MIN_H: f32 = 600.0;

pub fn main_min_w() -> f32 {
    crate::ui::toolbar::min_width().max(1050.0)
}

pub fn main_window_size() -> iced::Size {
    // Proportions of the 1512x982 desktop the layout was drawn on, so the
    // first run fills the same share of a bigger or smaller screen.
    let d = display_points().unwrap_or(iced::Size::new(1512.0, 982.0));
    iced::Size::new(
        (d.width * (1009.0 / 1512.0)).max(main_min_w()),
        (d.height * (606.0 / 982.0)).max(MAIN_MIN_H),
    )
}

/// The main window in interface units, from what a resize last reported.
/// `main_size` is in OS points, so it converts back through the View > Scale
/// ratio; before the first resize event it is zero and `opened_at` — the
/// size the window was asked to open at, already in interface units — stands
/// in, so a menu is placed against the right window from the first click.
fn main_viewport(main_size: iced::Size, opened_at: (f32, f32), scale: f32) -> iced::Size {
    if main_size.width > 0.0 && main_size.height > 0.0 {
        return iced::Size::new(main_size.width / scale, main_size.height / scale);
    }
    iced::Size::new(opened_at.0, opened_at.1)
}

/// The size the main window opens at, in interface units, from the size a
/// resize remembered in OS points.
///
/// A saved size that no longer describes a screen — a monitor that is gone,
/// a hand-edited config — derives from the display instead. The range only
/// rejects nonsense: `min_size` holds the window to a full toolbar row
/// whatever the saved size says, and that floor moves with the scale
/// while this range does not.
fn main_open_size(saved: Option<(f32, f32)>, scale: f32) -> (f32, f32) {
    let os = saved
        .filter(|(w, h)| (400.0..=4000.0).contains(w) && (300.0..=2500.0).contains(h))
        .map(|(w, h)| iced::Size::new(w, h))
        .unwrap_or_else(main_window_size);
    (os.width / scale, os.height / scale)
}

/// Where the main window opens: where it was left, while the middle of its
/// title bar still lands on a connected display, and `None` (centred)
/// otherwise — an unplugged monitor must not strand the window off-screen.
/// `saved` and `displays` are in OS points, and so is `width`.
fn main_open_position(
    saved: Option<(f32, f32)>,
    width: f32,
    displays: &[iced::Rectangle],
) -> Option<Point> {
    // Below the top edge, so a window maximized or snapped on Windows —
    // whose outer frame starts a few points off the screen — still counts.
    const TITLE_BAR_GRIP: f32 = 16.0;
    match saved {
        Some((x, y))
            if displays
                .iter()
                .any(|d| d.contains(Point::new(x + width / 2.0, y + TITLE_BAR_GRIP))) =>
        {
            Some(Point::new(x, y))
        }
        _ => None,
    }
}

/// Windows parks a minimized window at (-32000, -32000) pixels, and reports
/// that as a move. No display arrangement puts a real window that far up
/// and to the left, even divided by the largest display scale.
fn is_parked(origin: Point) -> bool {
    const PARKED: f32 = -6000.0;
    origin.x <= PARKED && origin.y <= PARKED
}

/// Hold a dialog inside the screen. `size` and the answer are in interface
/// units, `display` is the whole screen in OS points, and `scale` is the
/// View > Scale ratio between the two.
///
/// A dialog is fixed-size and cannot be maximized, so anything that opens
/// past the bottom edge of the display — the OK/Cancel row lives there —
/// cannot be brought back at all. Capping costs nothing where it bites:
/// every dialog tall enough to reach the cap (Configuration, Scheduler,
/// Batch, the progress box) puts its body in a scrollable under a pinned
/// button row, so a shortened window still reaches every control. The short
/// ones — About, Confirm, File Info — are nowhere near it on any screen a
/// desktop has.
fn fit_to_display(size: (f32, f32), display: iced::Size, scale: f32) -> (f32, f32) {
    // What is left of the display once the system bar (taskbar, dock, menu
    // bar) and the window's own title bar have taken their share. Neither is
    // reported by `display-info`, so this is an allowance, not a measurement.
    const USABLE_W: f32 = 0.95;
    const USABLE_H: f32 = 0.85;
    if display.width <= 0.0 || display.height <= 0.0 || scale <= 0.0 {
        return size;
    }
    (
        size.0.min(display.width * USABLE_W / scale),
        size.1.min(display.height * USABLE_H / scale),
    )
}

/// The link a pasted line carries, or `None` when it carries none.
///
/// A list copied from a page or exported by another manager rarely holds bare
/// URLs: writes `title|http://host/file`, numbered lists prefix `1. `, and
/// markup leaves quotes around the address. Take the first scheme that appears
/// and read to the first character no URL can hold, rather than asking the
/// whole line to parse.
pub fn url_in_line(line: &str) -> Option<&str> {
    const SCHEMES: [&str; 3] = ["http://", "https://", "ftp://"];
    let at = SCHEMES.iter().filter_map(|s| line.find(s)).min()?;
    let rest = &line[at..];
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '|' | '\\'))
        .unwrap_or(rest.len());
    let url = &rest[..end];
    engine::parse_url(url).is_ok().then_some(url)
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

/// Is this a signed URL whose window is too short to survive a dialog?
///
/// The hour is a policy, not a property of the URL: a link good for a week can
/// wait behind a confirmation like any other, while the fifteen-minute and
/// ten-second windows object stores actually hand out cannot. Erring long is
/// the safe direction — the cost of being wrong is a dialog the user did not
/// get, against a download that could not have happened at all.
pub(crate) fn expiring_soon(url: &str) -> bool {
    hya_net::signed::perishable(url, crate::fmt::now_unix().max(0) as u64)
}

/// Put what the browser knew onto the item, leaving whatever it did not say
/// alone: a field the extension could not fill must not blank the one
/// `add_item` derived from the URL.
///
/// `filing` is where the capture's own filename files it — the category it
/// matches and that category's folder — because only a rename may move the
/// download, and resolving it is the half that needs the app's category
/// table.
/// Two addresses name the same origin — or are the same text, when either is
/// not yet an address. A browser session belongs to a host and a scheme, so
/// that is what decides whether a store is read again or an import kept.
fn same_host(a: &str, b: &str) -> bool {
    match (crate::engine::parse_url(a), crate::engine::parse_url(b)) {
        (Ok(x), Ok(y)) => x.tls == y.tls && x.host.eq_ignore_ascii_case(&y.host),
        _ => a == b,
    }
}

fn write_capture_extras(
    d: &mut DownloadItem,
    extras: CaptureExtras,
    filing: Option<(Option<String>, Option<String>)>,
) {
    if let Some(c) = extras.cookies {
        d.cookies = Some(c);
        // An extension capture carries no description of its own; saying where
        // it came from is the point of the field, so it gets the one true
        // thing known about it rather than nothing.
        d.cookie_source = Some(
            extras
                .cookie_source
                .unwrap_or_else(|| crate::i18n::tr("captured by the browser extension")),
        );
    }
    if let Some(r) = extras.referer {
        d.referer = Some(r);
    }
    if let Some(p) = captured_proxy(extras.proxy) {
        d.proxy = p;
    }
    if let Some(n) = extras.name {
        d.set_file_name(&n);
        if let Some((cat, dir)) = filing {
            d.category = cat;
            if let Some(dir) = dir {
                d.save_dir = dir;
            }
        }
    }
}

/// The browser's own proxy, as this download's route.
///
/// `None` means "leave the item's choice alone", which is what an extension
/// that is not allowed to read the setting — or a browser that only says it
/// is following the machine — sends. An address that cannot be parsed is the
/// same answer plus a log line, never an error: the download itself is fine,
/// and refusing it over a setting the user made in a different program would
/// be a capture that silently stopped working.
fn captured_proxy(spec: Option<String>) -> Option<ProxyChoice> {
    let spec = spec
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    match crate::proxy::spec_error(&spec) {
        None => Some(ProxyChoice::Custom(spec)),
        Some(why) => {
            crate::log::warn(&format!(
                "ext: ignoring the browser's proxy {spec:?}: {why}"
            ));
            None
        }
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

/// May a probe's name replace the one the item carries?
///
/// The byte counts alone cannot answer this. A probe belongs to the transfer
/// that issued it, so its answer arrives while `downloaded` is still zero —
/// which is to say AFTER `Start Download` wrote the name the user typed into
/// the File Info dialog. Reading "no bytes yet" as "nobody has named this
/// file" therefore handed every rename straight back to the server, and did
/// the same to the renamed copy the duplicate dialog hands out, aiming it at
/// the very file it had just warned about. `name_locked` is what separates
/// the two cases.
fn may_adopt_name(d: &DownloadItem) -> bool {
    !d.name_locked && d.downloaded == 0 && d.held.is_empty()
}

fn move_stream_companions(from_part: &std::path::Path, to_part: &std::path::Path) {
    let Some(parent) = from_part.parent() else {
        return;
    };
    let Some(from_prefix) = from_part.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let Some(to_prefix) = to_part.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix_with_dot = format!("{from_prefix}.");
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(suffix) = fname.strip_prefix(&prefix_with_dot) {
                    let companion_dest = to_part.with_file_name(format!("{to_prefix}.{suffix}"));
                    let _ = crate::files::move_file(&path, &companion_dest);
                }
            }
        }
    }
}

/// Make the name of a download with nothing on disk yet portable.
///
/// A list saved before every name went through `set_file_name` can hold one
/// Windows refuses (`a:b.mp4`); such an entry failed on every start with
/// `os error 123` and had to be deleted. With no bytes written there is no
/// file to lose, so the name, and the staging path derived from it, are fixed.
fn repair_unstarted_name(d: &mut DownloadItem) {
    let name = d.file_name.clone();
    if d.downloaded == 0 && d.held.is_empty() && d.set_file_name(&name) {
        d.part_path = None;
    }
}

/// Carry the edits typed into a still-open File Info dialog onto its item, and
/// move the finished file to match.
///
/// A new download can be running behind the dialog ("download in background
/// while choosing options"), and a small file lands before the person has
/// finished typing. The completion used to close the dialog and drop
/// everything in it: the name they had just written, the folder they had just
/// picked. Measured on a 2.6 MB archive over a fast link — it finished in
/// about a second, under the provisional `_1` name the duplicate prompt had
/// given it, while a Persian suffix was still being typed into the name box.
///
/// Only fields the person actually touched are applied; the rest are what the
/// probe filled in and already match the item. Returns the path the file was
/// moved to, or `None` when nothing changed on disk.
fn adopt_dialog_edits(
    d: &mut DownloadItem,
    fi: &FileInfoState,
) -> std::io::Result<Option<std::path::PathBuf>> {
    let old = d.full_path();
    let name = fi.file_name.trim();
    if fi.name_touched && !name.is_empty() {
        d.set_file_name(name);
        d.name_locked = true;
    }
    if fi.dir_touched && !fi.save_dir.trim().is_empty() {
        d.save_dir = fi.save_dir.clone();
    }
    if fi.cat_touched {
        d.category = Some(fi.category.clone());
    }
    if !fi.description.trim().is_empty() {
        d.description = fi.description.clone();
    }
    let new = d.full_path();
    if new == old || !old.is_file() {
        return Ok(None);
    }
    if let Some(dir) = new.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::rename(&old, &new)?;
    Ok(Some(new))
}

/// Height the File Info dialog's Result row needs for `error`.
///
/// The message is whatever the server or the transport had to say, and a bot
/// challenge, a refused proxy or a connection error easily runs past one
/// line. The dialog does not scroll and is sized to the rows it draws, so a
/// two-line message measured as one pushes everything below it down — and
/// what iced then squeezes is the LAST row, which is the proxy row: its
/// picker came out visibly shorter than the Category picker above it.
fn result_row_height(error: &str) -> f32 {
    // Characters that fit on one line of the value column at the dialog's
    // font, once the label column and the icon column have taken theirs.
    const PER_LINE: usize = 70;
    // A row costs its text plus the gap above it; each further line costs
    // only the text. Three lines is where the message stops being read and
    // starts being scrolled past, so the dialog stops growing there.
    let lines = error.len().div_ceil(PER_LINE).clamp(1, 3);
    32.0 + (lines - 1) as f32 * 18.0
}

/// The route the File Info dialog is asking for, and whether it differs from
/// the one the item already has.
///
/// A connection's route is chosen when the socket is opened, so a transfer
/// already pulling bytes cannot be moved onto a new one: a difference here
/// means stop and start again, keeping the spans already on disk. Accepting
/// the change without that would leave the download running through the old
/// route — which, when the new choice is "no proxy" or a different tunnel, is
/// the leak the setting exists to prevent.
fn reroute(d: &DownloadItem, fi: &FileInfoState) -> (ProxyChoice, bool) {
    let want = ProxyChoice::from_parts(fi.proxy_pick, &fi.proxy_spec);
    let changed = d.proxy != want;
    (want, changed)
}

/// Forget everything that would let the next start resume this item.
///
/// Progress reset alone is not enough: the pinned `.part` would satisfy the
/// resume check and the new object would be written into a file that still
/// has the old one's bytes (and, via an edited URL, no record of which object
/// it holds). Drop both.
///
/// The caller owns the check that the transfer is stopped — a running one
/// owns its `.part`.
fn reset_for_redownload(d: &mut DownloadItem) {
    let _ = std::fs::remove_file(d.part_file());
    d.held.clear();
    d.downloaded = 0;
    d.disp_progress = 0.0;
    d.state = DlState::Paused;
    d.part_path = None;
    d.error = None;
}

/// The file the duplicate dialog should warn about, if any.
///
/// `named` says whether `name` is the name this download will really be
/// saved under. It is not always: for a typed URL that names no file,
/// `file_name_from_url` invents `index.html` as somewhere to put the bytes
/// until the probe resolves the real name a moment later. Warning on that
/// placeholder reported "a file with this name already exists" about an
/// unrelated leftover, for a download that was never going to be written
/// there.
///
/// `is_file` rather than `exists`, because the dialog's answer is "Open
/// existing file": with Options > Save to > "Do not create category folders"
/// on, the one download folder is also where the category directories sit,
/// and a capture named after one of them — the extension sends the page
/// title, which carries no extension — matched a directory and offered to
/// open it as though it were the file.
///
/// An EMPTY file is not one either, and that exclusion is what makes browser
/// capture usable at all. Gecko cannot park a download while Hydra decides
/// (`downloads.pause` there is a one-way cancel — see the extension's
/// background.js), so Firefox's own transfer is still running when the
/// capture arrives. Firefox reserves its target name the instant a download
/// starts by creating a zero-byte file under the FINAL name, and writes the
/// bytes into a `name.<random>.ext.part` sibling. That placeholder is the
/// file this check used to find: every captured download reported "a file
/// with this name already exists", and by the time the person read the
/// dialog Hydra had acknowledged the capture, the extension had cancelled
/// the browser's copy, and Firefox had deleted the placeholder again —
/// leaving them to look at an empty folder and a dialog naming a file that
/// was not there. Nothing is lost by ignoring it: an empty file gives the
/// "Open existing file" answer nothing to open, and a download that lands
/// on top of one replaces no bytes.
fn collision_file(dir: &str, name: &str, named: bool) -> Option<String> {
    if !named {
        return None;
    }
    let path = std::path::Path::new(dir).join(name);
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() > 0).then(|| path.to_string_lossy().into_owned())
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

/// A filename for a stream, from its manifest URL.
///
/// Manifests are called `index.m3u8` or `master.mpd` almost universally, so
/// the directory above is what actually names the asset.
pub fn stream_base_name(url: &str) -> String {
    let cleaned = sanitize_file_name(&hya_stream::url::stream_base_name(url));
    if cleaned.is_empty() {
        "stream".into()
    } else {
        cleaned
    }
}

/// Whether an address is worth reading as a manifest.
///
/// The body decides in the end, but fetching every address the user types
/// would be both slow and rude; the extension is the cheap filter.
pub fn manifest_address(url: &str) -> bool {
    let lower = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && (lower.ends_with(".m3u8") || lower.ends_with(".m3u") || lower.ends_with(".mpd"))
}

/// A page title made safe to be a filename on any of the three platforms.
///
/// The extension already trims what it can see, but the name arrives from a
/// web page and must never be trusted to be a leaf name: a `/` or a `..` in
/// it would place the finished file outside the category directory.
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
    // Two caps, because they bind on different names. 120 characters keeps a
    // page-title capture from becoming an unreadable line of text. The byte
    // cap is what the filesystem actually enforces: ext4, APFS and exFAT stop
    // at 255 bytes, and 120 characters of Hangul, Persian or Chinese is 360
    // of them — a cap counted only in characters passes every Latin test and
    // fails the write with ENAMETOOLONG on exactly the names that need it.
    // The margin under 255 is for the extension callers append afterwards.
    const MAX_BYTES: usize = 250;
    let capped: String = trimmed.chars().take(120).collect();
    let mut cut = capped.len().min(MAX_BYTES);
    // Half a character is not a character: the cut lands on a boundary.
    while cut > 0 && !capped.is_char_boundary(cut) {
        cut -= 1;
    }
    capped[..cut].trim().to_string()
}

/// Sort, clamp and merge persisted byte spans. `Scheduler::mark_done` is
/// told "these bytes arrived, never fetch them again", so spans read back
/// from disk are not trusted as-is: inverted spans drop, spans past the
/// object end clamp, and overlapping or adjacent spans merge.
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

/// What Escape means in window `kind`: its Cancel button, or nothing for
/// the windows that have none (the main window, a progress box).
fn dialog_cancel(app: &App, kind: WinKind, id: window::Id) -> Option<Message> {
    if !app.windows.contains_key(&id) {
        return None;
    }
    Some(match kind {
        WinKind::Main | WinKind::Progress(_) => return None,
        WinKind::FileInfo(_) => Message::FiCancel,
        WinKind::Power => Message::PowerCancel,
        WinKind::Update => Message::UpdateCancel,
        _ => Message::CloseThis(id),
    })
}

/// What Enter means in window `kind`: the button the dialog draws as its
/// default. `None` where pressing it blind would be wrong — a duplicate
/// question has three equal answers, and the countdown's default is Cancel.
fn dialog_primary(app: &App, kind: WinKind, id: window::Id) -> Option<Message> {
    Some(match kind {
        WinKind::Main | WinKind::Progress(_) => return None,
        WinKind::Confirm => match &app.confirm {
            Some(
                ConfirmKind::DeleteItems(_)
                | ConfirmKind::DeleteCompleted
                | ConfirmKind::StopWarn { .. },
            ) => Message::ConfirmYes,
            Some(ConfirmKind::PermissionWarn { .. }) => Message::OpenPermissions,
            // Trusting an extension is not a question to answer blind.
            Some(ConfirmKind::Duplicate { .. } | ConfirmKind::TrustExtension(_)) | None => {
                return None
            }
            Some(_) => Message::CloseThis(id),
        },
        WinKind::AddUrl => Message::AddUrlOk,
        WinKind::FileInfo(_) if app.file_info.is_new => Message::FiStartDownload,
        WinKind::FileInfo(_) => Message::FiOk,
        WinKind::Batch => Message::BatchOk,
        WinKind::Options => Message::OptOk,
        WinKind::Scheduler => Message::SchSave,
        WinKind::Complete(dl) => Message::OpenFile(dl),
        WinKind::Power => Message::PowerCancel,
        WinKind::Update => match (&app.updater.phase, &app.updater.info) {
            (UpdatePhase::Idle, Some(info)) if !info.has_bundle => return None,
            (UpdatePhase::Idle, Some(info)) if info.in_place => Message::UpdateNow,
            (UpdatePhase::Idle, Some(info)) => match &info.package {
                Some((_, url, _)) => Message::UpdateOpenUrl(url.clone()),
                None => return None,
            },
            (UpdatePhase::Failed(_), _) => Message::UpdateNow,
            _ => return None,
        },
        WinKind::About
        | WinKind::Shortcuts
        | WinKind::Columns
        | WinKind::Permissions
        | WinKind::ZipPreview(_) => Message::CloseThis(id),
    })
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
    if !s.start_enabled || !same_minute(&s.start_at, hhmm) || running {
        return false;
    }
    s.once || s.days.get(weekday).copied().unwrap_or(false)
}

/// Whether two typed times name the same minute: `9:00` is `09:00`.
fn same_minute(a: &str, b: &str) -> bool {
    match (model::parse_hhmm(a), model::parse_hhmm(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Bytes the Connection tab's download limit allows per window, or `None`
/// while the limit is switched off. A zero cap would refuse every start for
/// good, so it floors at one MB like the hours floor at one.
pub fn quota_cap(s: &crate::model::Settings) -> Option<u64> {
    s.dl_limit_enabled
        .then(|| s.dl_limit_mb.max(1).saturating_mul(1024 * 1024))
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
/// of showing an unchecked box while the cap is active. `details` is the
/// Downloads-tab default for the connections panel; the window's own
/// Show/Hide details button takes over from there.
fn prog_state_seed(speed_limit: Option<u64>, proxy: &ProxyChoice, details: bool) -> ProgState {
    ProgState {
        details,
        limit_on: speed_limit.is_some(),
        limit_kb: (speed_limit.unwrap_or(10 * 1024) / 1024).to_string(),
        remember_limit: speed_limit.is_some(),
        proxy_pick: proxy.pick(),
        proxy_spec: proxy.spec().to_string(),
        ..ProgState::default()
    }
}

/// Resolve the Latin character for a key press taking into account the active
/// keyboard layout and modifiers.
///
/// On Windows and Linux, when the user switches keyboard layout (e.g. Persian,
/// Arabic, Cyrillic), `key` carries the layout's localized character rather
/// than the Latin key code (e.g. 'ش' for 'a'). Using iced's `modified_key` and
/// `physical_key` translations resolves the physical Latin key so shortcuts
/// like Ctrl+A, Ctrl+W, Ctrl+Q continue to work across all layouts.
pub fn resolve_latin_char(
    key: &iced::keyboard::Key,
    modified_key: &iced::keyboard::Key,
    physical_key: iced::keyboard::key::Physical,
) -> Option<char> {
    if let Some(c) = modified_key.to_latin(physical_key) {
        if !c.is_ascii_control() {
            return Some(c.to_ascii_lowercase());
        }
    }
    if let Some(c) = key.to_latin(physical_key) {
        if !c.is_ascii_control() {
            return Some(c.to_ascii_lowercase());
        }
    }
    if let iced::keyboard::key::Physical::Code(code) = physical_key {
        let punct = match code {
            iced::keyboard::key::Code::Comma => Some(','),
            iced::keyboard::key::Code::Period => Some('.'),
            iced::keyboard::key::Code::Slash => Some('/'),
            iced::keyboard::key::Code::Minus => Some('-'),
            iced::keyboard::key::Code::Equal => Some('='),
            iced::keyboard::key::Code::Semicolon => Some(';'),
            iced::keyboard::key::Code::Quote => Some('\''),
            iced::keyboard::key::Code::BracketLeft => Some('['),
            iced::keyboard::key::Code::BracketRight => Some(']'),
            iced::keyboard::key::Code::Backslash => Some('\\'),
            _ => None,
        };
        if punct.is_some() {
            return punct;
        }
    }
    if let iced::keyboard::Key::Character(s) = key {
        let mut chars = s.chars();
        if let Some(c) = chars.next() {
            if chars.next().is_none() && !c.is_ascii_control() && c.is_ascii() {
                return Some(c.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Normalize a key press into a combo string ("cmd+shift+v" or "ctrl+shift+v").
/// `cmd` is ⌘ on macOS and `ctrl` is Ctrl elsewhere (`Modifiers::command()`);
/// presses without a command modifier are not shortcuts.
pub fn combo_string(key: &iced::keyboard::Key, mods: iced::keyboard::Modifiers) -> Option<String> {
    if !mods.command() {
        return None;
    }
    let base = match key {
        iced::keyboard::Key::Character(c) => c.to_string().to_ascii_lowercase(),
        _ => return None,
    };
    let mut combo = format!("{}+", crate::model::primary_modifier());
    if mods.shift() {
        combo.push_str("shift+");
    }
    if mods.alt() {
        combo.push_str("alt+");
    }
    combo.push_str(&base);
    Some(combo)
}

/// Resolve the action for the quit shortcut: on Windows, closes the active window
/// (same as Ctrl+W) rather than terminating the application.
pub(crate) fn resolve_quit_action(win: iced::window::Id, is_windows: bool) -> Message {
    if is_windows {
        Message::WindowCloseRequested(win)
    } else {
        Message::Menu(MenuAction::Exit)
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_application_id() -> String {
    std::env::var("FLATPAK_ID").unwrap_or_else(|_| "hydra".to_string())
}

#[cfg(test)]
mod tests {
    /// A combo the table ships but a key press can never produce is an
    /// action nobody can reach, and two actions on one combo means the
    /// alphabetically later id silently never fires.
    #[test]
    fn native_progress_glides_small_verified_steps_without_changing_bytes() {
        let mut app = App::default();
        let mut download = item(1, "/tmp", "native", None, DlState::Receiving);
        download.plugin_plan = Some(crate::plugins::tests::transfer_plan());
        download.size = Some(1_000_000);
        download.downloaded = 100;
        download.disp_progress = 0.0;
        app.state.downloads.push(download);
        app.windows
            .insert(window::Id::unique(), WinKind::Progress(1));
        assert_eq!(app.animation_interval_ms(), 33);
        let _ = app.update(Message::AnimTick);
        let item = app.item(1).unwrap();
        assert_eq!(item.downloaded, 100);
        assert!(item.disp_progress > 0.0 && item.disp_progress < item.progress());
        app.item_mut(1).unwrap().state = DlState::Paused;
        assert_eq!(app.animation_interval_ms(), 80);
    }

    #[test]
    fn default_shortcuts_are_unique_and_match_what_a_key_press_produces() {
        use iced::keyboard::{Key, Modifiers};

        let command = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        let mut seen = std::collections::BTreeSet::new();
        for (id, combo, _) in crate::model::SHORTCUT_ACTIONS {
            let combo = crate::model::platform_default(combo);
            assert!(
                seen.insert(combo.clone()),
                "'{combo}' is bound twice, once by '{id}'"
            );
            let mut parts: Vec<&str> = combo.split('+').collect();
            let base = parts.pop().expect("split always yields the base key");
            let mut mods = Modifiers::empty();
            for part in parts {
                mods |= match part {
                    "cmd" | "ctrl" => command,
                    "shift" => Modifiers::SHIFT,
                    "alt" => Modifiers::ALT,
                    other => panic!("'{id}' uses an unknown modifier '{other}'"),
                };
            }
            assert_eq!(
                super::combo_string(&Key::Character(base.into()), mods).as_deref(),
                Some(combo.as_str()),
                "'{id}' ships a combo no key press normalizes to"
            );
        }
    }

    #[test]
    fn resolve_latin_char_handles_localized_keyboard_layouts() {
        use iced::keyboard::key::{Code, Named, Physical};
        use iced::keyboard::Key;

        // Latin layout
        assert_eq!(
            super::resolve_latin_char(
                &Key::Character("a".into()),
                &Key::Character("a".into()),
                Physical::Code(Code::KeyA)
            ),
            Some('a')
        );

        // Persian (Farsi) layout: 'ش' is on physical KeyA
        assert_eq!(
            super::resolve_latin_char(
                &Key::Character("ش".into()),
                &Key::Character("ش".into()),
                Physical::Code(Code::KeyA)
            ),
            Some('a')
        );

        // Russian Cyrillic: 'ц' is on physical KeyW
        assert_eq!(
            super::resolve_latin_char(
                &Key::Character("ц".into()),
                &Key::Character("ц".into()),
                Physical::Code(Code::KeyW)
            ),
            Some('w')
        );

        // Windows Ctrl+A control code \x01 with modified_key "a"
        assert_eq!(
            super::resolve_latin_char(
                &Key::Character("\x01".into()),
                &Key::Character("a".into()),
                Physical::Code(Code::KeyA)
            ),
            Some('a')
        );

        // Physical Comma on non-Latin layout
        assert_eq!(
            super::resolve_latin_char(
                &Key::Character("،".into()),
                &Key::Character("،".into()),
                Physical::Code(Code::Comma)
            ),
            Some(',')
        );

        // Named key returns None (preserved for Escape, Enter, F4)
        assert_eq!(
            super::resolve_latin_char(
                &Key::Named(Named::Escape),
                &Key::Named(Named::Escape),
                Physical::Code(Code::Escape)
            ),
            None
        );
    }

    #[test]
    fn quit_shortcut_action_per_platform() {
        let win = iced::window::Id::unique();
        assert!(matches!(
            super::resolve_quit_action(win, true),
            super::Message::WindowCloseRequested(w) if w == win
        ));
        assert!(matches!(
            super::resolve_quit_action(win, false),
            super::Message::Menu(super::MenuAction::Exit)
        ));
    }

    /// The Connection tab's list is only as good as the entry the transfer
    /// actually reads. Re-typing a server has to REPLACE its number, because
    /// the lookup stops at the first match and a shadowed second entry is a
    /// setting the user can see, edit and never use.
    #[test]
    fn a_re_entered_server_replaces_its_exception_instead_of_shadowing_it() {
        use super::{exception_for, upsert_exception};
        let mut list = vec![];
        upsert_exception(&mut list, "cdn.example.com".into(), 4);
        upsert_exception(&mut list, "slow.example.org".into(), 1);
        upsert_exception(&mut list, "cdn.example.com".into(), 16);

        assert_eq!(list.len(), 2, "a repeat is an edit, not a second rule");
        assert_eq!(exception_for(&list, "cdn.example.com"), Some(16));
        assert_eq!(exception_for(&list, "slow.example.org"), Some(1));
    }

    /// Menus address a profile by name, so a second profile under a name the
    /// list already carries would be one the user can see, edit and never
    /// reach — every menu entry resolves to the first match.
    #[test]
    fn a_re_saved_profile_is_retuned_rather_than_shadowed() {
        use super::upsert_profile;
        let mut list = vec![];
        upsert_profile(&mut list, "Calls".into(), Some(64 * 1024));
        upsert_profile(&mut list, "Night".into(), None);
        upsert_profile(&mut list, "Calls".into(), Some(128 * 1024));

        assert_eq!(list.len(), 2, "a repeat is an edit, not a second profile");
        let calls = list.iter().find(|p| p.name == "Calls").expect("kept");
        assert_eq!(calls.limit, Some(128 * 1024));
        // A profile with no cap is the one that clears the limiter; it must
        // not be mistaken for a half-filled row and dropped.
        assert!(list.iter().any(|p| p.name == "Night" && p.limit.is_none()));
    }

    /// The native macOS menu carries actions as strings, so a profile is only
    /// clickable there if its name survives the round trip — including the
    /// separator the ids are built with.
    #[test]
    fn a_profile_menu_id_comes_back_naming_the_same_profile() {
        for name in ["Night", "Background", "500:1000", "Работа"] {
            let action = MenuAction::SpeedProfile(name.to_string());
            assert_eq!(MenuAction::from_id(&action.id()), Some(action.clone()));
        }
        assert_eq!(
            MenuAction::from_id(&MenuAction::SpeedLimitSettings.id()),
            Some(MenuAction::SpeedLimitSettings)
        );
    }

    /// View > Hide toolbar text is clicked in the native macOS bar, which
    /// carries actions as strings: an id that does not come back is an entry
    /// that does nothing there.
    #[test]
    fn the_toolbar_text_toggle_comes_back_from_its_menu_id() {
        assert_eq!(
            MenuAction::from_id(&MenuAction::HideToolbarText.id()),
            Some(MenuAction::HideToolbarText)
        );
    }

    /// The View entry is a checkbox over a setting, not a counter of clicks:
    /// it has to flip the setting, keep it for the next launch, and show the
    /// tick the setting says it should.
    #[test]
    // The toggle syncs the native menu bar, and muda builds an NSMenu on the
    // main thread only — which a test thread is not. Linux/Windows, where CI
    // runs the suite, have no native bar to sync.
    #[cfg_attr(target_os = "macos", ignore = "muda needs the main thread")]
    fn hiding_the_toolbar_text_flips_the_setting_and_its_tick() {
        let ticked = |app: &App| {
            crate::ui::menu::entries(MenuBarKind::View, app)
                .into_iter()
                .find(|e| e.action == Some(MenuAction::HideToolbarText))
                .expect("View offers the toolbar-text toggle")
                .checked
        };
        let mut app = App::default();
        assert!(app.cfg.settings.show_toolbar_labels);
        assert!(!ticked(&app));

        let _ = app.update(Message::Menu(MenuAction::HideToolbarText));
        assert!(!app.cfg.settings.show_toolbar_labels);
        assert!(app.cfg_dirty, "the choice has to outlive the session");
        assert!(ticked(&app));

        let _ = app.update(Message::Menu(MenuAction::HideToolbarText));
        assert!(app.cfg.settings.show_toolbar_labels);
        assert!(!ticked(&app));
    }

    /// Removing the row the user picked must leave the others alone — and must
    /// take the rule out of force, not merely off the screen.
    #[test]
    fn removing_an_exception_takes_its_rule_out_of_force() {
        use super::{exception_for, upsert_exception};
        let mut list = vec![];
        upsert_exception(&mut list, "a.example.com".into(), 2);
        upsert_exception(&mut list, "b.example.com".into(), 8);

        list.remove(0);

        assert_eq!(exception_for(&list, "a.example.com"), None);
        assert_eq!(exception_for(&list, "b.example.com"), Some(8));
    }

    /// A `*.` prefix is how the field is usually filled in, and a bare suffix
    /// has to mean the same thing. An empty server would match every host, so
    /// it must match none.
    #[test]
    fn an_exception_matches_subdomains_and_never_matches_on_an_empty_server() {
        use super::exception_for;
        let list = vec![
            (String::new(), 32),
            ("*.example.com".to_string(), 4),
            ("uplod.ir".to_string(), 1),
        ];
        assert_eq!(exception_for(&list, "cdn.example.com"), Some(4));
        assert_eq!(exception_for(&list, "example.com"), Some(4));
        assert_eq!(exception_for(&list, "s7.uplod.ir"), Some(1));
        assert_eq!(exception_for(&list, "other.net"), None);
    }

    #[test]
    fn connection_limits_preserve_large_defaults_and_server_exceptions() {
        let mut app = App::default();
        assert_eq!(app.conns_for("https://example.org/file"), 8);
        for n in crate::model::CONNECTION_OPTIONS {
            app.cfg.settings.default_conns = n;
            assert_eq!(app.conns_for("https://example.org/file"), n);
        }
        app.cfg.settings.conn_exceptions = vec![("example.com".into(), 128)];
        assert_eq!(app.conns_for("https://cdn.example.com/file"), 128);
        assert_eq!(app.conns_for("https://example.org/file"), 256);
        for (input, expected) in [(0, 1), (257, 256), (usize::MAX, 256)] {
            app.cfg.settings.default_conns = input;
            app.cfg.settings.conn_exceptions[0].1 = input;
            assert_eq!(app.conns_for("https://example.org/file"), expected);
            assert_eq!(app.conns_for("https://example.com/file"), expected);
        }
    }

    #[test]
    fn show_hydra_recreates_a_closed_main_window_without_duplicates() {
        let mut app = App::default();
        for _ in 0..3 {
            let _ = app.update(Message::NativeMenu("show_main".into()));
            let main = app.main_id.unwrap();
            assert_eq!(app.win_of(WinKind::Main), Some(main));
            let reveal = app.update(Message::NativeMenu("show_main".into()));
            let actions = iced::futures::executor::block_on(async {
                use iced::futures::StreamExt;
                iced_runtime::task::into_stream(reveal)
                    .unwrap()
                    .collect::<Vec<_>>()
                    .await
            });
            assert!(actions.iter().any(|action| matches!(
                action,
                iced_runtime::Action::Window(iced_runtime::window::Action::Minimize(id, false))
                    if *id == main
            )));
            assert!(actions.iter().any(|action| matches!(
                action,
                iced_runtime::Action::Window(iced_runtime::window::Action::GainFocus(id))
                    if *id == main
            )));
            assert_eq!(app.main_id, Some(main));
            assert_eq!(app.windows.len(), 1);
            let _ = app.update(Message::WindowClosed(main));
            assert!(app.win_of(WinKind::Main).is_none());
        }
    }

    #[test]
    fn save_as_splits_folder_and_name() {
        use super::split_save_as as split;
        assert_eq!(split("/tmp/dl/a.zip"), ("/tmp/dl".into(), "a.zip".into()));
        assert_eq!(
            split("C:\\Users\\me\\a.zip"),
            ("C:\\Users\\me".into(), "a.zip".into())
        );
        assert_eq!(split("/a.zip"), ("/".into(), "a.zip".into()));
        assert_eq!(split("a.zip"), (String::new(), "a.zip".into()));
        assert_eq!(split("/tmp/dl/"), ("/tmp/dl".into(), String::new()));
    }

    #[test]
    fn save_as_edit_marks_only_changed_part() {
        let mut fi = super::FileInfoState {
            save_dir: "/tmp/dl".into(),
            file_name: "a.zip".into(),
            ..Default::default()
        };
        assert_eq!(fi.save_as(), "/tmp/dl/a.zip");
        fi.set_save_as("/tmp/dl/b.zip");
        assert!(fi.name_touched && !fi.dir_touched);
        fi.set_save_as("/var/x/b.zip");
        assert!(fi.dir_touched);
        assert_eq!(fi.save_dir, "/var/x");
    }

    use super::*;
    use crate::model::Schedule;
    use iced::widget::text_editor;

    fn batch_with(urls: &[&str]) -> BatchState {
        BatchState {
            checks: urls.iter().map(|u| (u.to_string(), true)).collect(),
            ..Default::default()
        }
    }

    /// The batch selection, in index order — a `HashSet` has none of its own.
    fn sel(app: &App) -> Vec<usize> {
        let mut v: Vec<usize> = app.batch.sel.iter().copied().collect();
        v.sort_unstable();
        v
    }

    fn checked(app: &App) -> Vec<&str> {
        app.batch
            .checks
            .iter()
            .filter(|(_, on)| *on)
            .map(|(u, _)| u.as_str())
            .collect()
    }

    fn shown(st: &BatchState) -> Vec<usize> {
        batch_rows(st, |_| "d".into(), |_| false)
            .iter()
            .map(|r| r.idx)
            .collect()
    }

    #[test]
    fn batch_rows_hide_duplicates_and_web_pages() {
        let mut st = batch_with(&[
            "https://a.b/x.zip",
            "https://a.b/index.html",
            "https://a.b/x.zip",
            "https://a.b/y.iso",
        ]);
        // Duplicates hidden by default; the first copy stays.
        assert_eq!(shown(&st), vec![0, 1, 3]);
        st.hide_html = true;
        assert_eq!(shown(&st), vec![0, 3]);
        st.hide_dups = false;
        st.hide_html = false;
        assert_eq!(shown(&st), vec![0, 1, 2, 3]);
        // A probed name decides, not the pasted URL's own tail.
        st.hide_html = true;
        st.names
            .insert("https://a.b/y.iso".into(), "page.php".into());
        assert_eq!(shown(&st), vec![0, 2]);
    }

    /// The size column is filled by the probe and by nothing else, so a link
    /// that is never handed to one shows no size for as long as the dialog is
    /// open — which is what a clipboard paste used to do. The box is re-read
    /// after every edit, so "measure what is new" also has to mean "and only
    /// what is new".
    #[test]
    fn every_batch_link_is_measured_once_however_often_the_box_changes() {
        let mut st = batch_with(&[
            "https://a.b/x.zip",
            "https://a.b/y.iso",
            "https://a.b/x.zip",
        ]);
        assert_eq!(
            st.take_unprobed(),
            vec![
                "https://a.b/x.zip".to_string(),
                "https://a.b/y.iso".to_string()
            ],
            "the same link pasted twice is one request, not two"
        );
        assert!(
            st.take_unprobed().is_empty(),
            "a second look at an unchanged list must not re-measure it"
        );
        st.checks.push(("https://a.b/z.bin".into(), true));
        assert_eq!(st.take_unprobed(), vec!["https://a.b/z.bin".to_string()]);
    }

    /// The reported bug: the same list measured when it came from a `.txt`
    /// and not when it came from the clipboard, because only one of the two
    /// routes asked for a probe. Both arrive as `BatchLoaded` now.
    #[test]
    fn a_list_handed_to_the_dialog_is_measured_however_it_arrived() {
        let mut app = App::default();
        let list = "https://a.b/x.cab\nhttps://a.b/y.cab\n";
        let _ = app.update(Message::BatchLoaded(Some(list.into())));
        assert_eq!(app.batch.checks.len(), 2, "both links reached the table");
        assert_eq!(
            app.batch.probing.len(),
            2,
            "a link nothing was sent out for can only ever show an empty Size"
        );
    }

    /// The reported bug: a list in own clipboard format, every line a
    /// title, a bar and the address. Requiring the whole line to parse threw
    /// all of it away and opened an empty table.
    #[test]
    fn a_titled_list_line_still_yields_its_link() {
        let mut app = App::default();
        let list = "15、 TYPE-C和 MICRO HDMI模块|http://z.example/a/15%E3%80%81.sdrm\n\
                    1. https://z.example/b.zip\n\
                    <a href=\"https://z.example/c.zip\">c</a>\n\
                    没有链接\n";
        let _ = app.update(Message::BatchLoaded(Some(list.into())));
        assert_eq!(
            app.batch
                .checks
                .iter()
                .map(|(u, _)| u.as_str())
                .collect::<Vec<_>>(),
            vec![
                "http://z.example/a/15%E3%80%81.sdrm",
                "https://z.example/b.zip",
                "https://z.example/c.zip",
            ],
            "a prefix, a number or surrounding markup must not hide the link"
        );
    }

    #[test]
    fn a_line_without_a_usable_address_yields_nothing() {
        assert_eq!(url_in_line("https://a.b/x.zip"), Some("https://a.b/x.zip"));
        assert_eq!(url_in_line("  ftp://a.b/x.zip\r"), Some("ftp://a.b/x.zip"));
        assert_eq!(url_in_line(""), None);
        assert_eq!(url_in_line("just some prose"), None);
        assert_eq!(url_in_line("mailto:someone@a.b"), None);
        assert_eq!(url_in_line("see http:// for details"), None);
        assert_eq!(
            url_in_line("a|http://a.b/x.zip|b"),
            Some("http://a.b/x.zip"),
            "the bar that opened the address also closes it"
        );
    }

    /// Typing a URL out by hand walks through a dozen parseable prefixes. The
    /// probe therefore waits for the box to stand still, and only the newest
    /// wait is allowed to act on what it finds.
    #[test]
    fn a_hand_edited_url_box_is_measured_once_it_stops_changing() {
        let mut app = App::default();
        let paste = |t: &str| {
            Message::BatchEdit(text_editor::Action::Edit(text_editor::Edit::Paste(
                std::sync::Arc::new(t.to_string()),
            )))
        };
        let _ = app.update(paste("https://a.b/x.cab\n"));
        assert!(
            app.batch.probing.is_empty(),
            "the edit itself must not reach the network"
        );
        let stale = app.batch.edit_gen;
        let _ = app.update(paste("https://a.b/y.cab\n"));
        let _ = app.update(Message::BatchProbeIdle(stale));
        assert!(
            app.batch.probing.is_empty(),
            "the wait started by the first edit was overtaken by the second"
        );
        let _ = app.update(Message::BatchProbeIdle(app.batch.edit_gen));
        assert_eq!(app.batch.probing.len(), 2);
        // A move of the caret is not an edit and starts no new wait, so the
        // links already measured stay measured and nothing is re-sent.
        let _ = app.update(Message::BatchEdit(text_editor::Action::Move(
            iced::widget::text_editor::Motion::Home,
        )));
        let _ = app.update(Message::BatchProbeIdle(app.batch.edit_gen));
        assert_eq!(app.batch.probing.len(), 2);
    }

    /// A batch adds its files as "Download Later": nothing starts, so nothing
    /// asks the server how big they are a second time. The number the dialog
    /// showed has to travel with them — except for a manifest, whose measured
    /// size is the playlist's rather than the media's.
    #[test]
    fn a_queued_batch_item_keeps_the_size_the_dialog_measured() {
        let mut app = App::default();
        app.batch.checks = vec![
            ("https://a.b/x.zip".into(), true),
            ("https://a.b/live.m3u8".into(), true),
            ("https://a.b/unmeasured.iso".into(), true),
        ];
        app.batch.parsed = true;
        app.batch.sizes.insert("https://a.b/x.zip".into(), 4096);
        app.batch.sizes.insert("https://a.b/live.m3u8".into(), 812);
        let _ = app.update(Message::BatchOk);

        let item_at = |url: &str| {
            app.state
                .downloads
                .iter()
                .find(|d| d.url == url)
                .unwrap_or_else(|| panic!("{url} was not added"))
        };
        assert_eq!(item_at("https://a.b/x.zip").size, Some(4096));
        assert_eq!(item_at("https://a.b/live.m3u8").size, None);
        assert_eq!(item_at("https://a.b/unmeasured.iso").size, None);
        assert!(app
            .state
            .downloads
            .iter()
            .all(|d| d.state == DlState::Queued));
    }

    #[test]
    fn batch_rows_sort_keeps_unknown_sizes_last() {
        let mut st = batch_with(&[
            "https://a.b/big.zip",
            "https://a.b/none.zip",
            "https://a.b/small.zip",
        ]);
        st.sizes.insert("https://a.b/big.zip".into(), 100);
        st.sizes.insert("https://a.b/small.zip".into(), 1);
        st.sort = Some((BatchSortKey::Size, true));
        assert_eq!(shown(&st), vec![2, 0, 1]);
        st.sort = Some((BatchSortKey::Size, false));
        assert_eq!(shown(&st), vec![0, 2, 1]);
        st.sort = Some((BatchSortKey::Name, false));
        assert_eq!(shown(&st), vec![2, 1, 0]);
        let rows = batch_rows(&st, |n| format!("/dl/{n}"), |u| u.contains("none"));
        assert_eq!(rows[1].save_to, "/dl/none.zip");
        assert!(rows[1].blocked && !rows[0].blocked);
        assert_eq!(rows[0].kind, "ZIP archive");
    }

    /// Four links, sorted by size. The workflow the multi-row selection was
    /// asked for: click the first row of a run, Shift-click the last, check
    /// exactly that run and nothing above it.
    #[test]
    fn a_shift_click_checks_the_run_between_the_two_clicks() {
        let mut app = App::default();
        app.batch.checks = vec![
            ("https://a.b/huge.iso".into(), false),
            ("https://a.b/small.zip".into(), false),
            ("https://a.b/mid.zip".into(), false),
            ("https://a.b/tiny.txt".into(), false),
        ];
        app.batch.parsed = true;
        for (u, n) in [
            ("https://a.b/huge.iso", 900_000_000),
            ("https://a.b/small.zip", 20_000_000),
            ("https://a.b/mid.zip", 90_000_000),
            ("https://a.b/tiny.txt", 1_000),
        ] {
            app.batch.sizes.insert(u.into(), n);
        }
        app.batch.sort = Some((BatchSortKey::Size, true));
        assert_eq!(shown(&app.batch), vec![3, 1, 2, 0], "smallest first");

        let _ = app.update(Message::BatchRowClick(3));
        app.mods = iced::keyboard::Modifiers::SHIFT;
        let _ = app.update(Message::BatchRowClick(2));
        assert_eq!(sel(&app), vec![1, 2, 3], "the run is taken in table order");

        // Shift-clicking back towards the anchor shrinks the same run rather
        // than starting a new one from the last click.
        let _ = app.update(Message::BatchRowClick(1));
        assert_eq!(sel(&app), vec![1, 3]);
        app.mods = iced::keyboard::Modifiers::default();

        let _ = app.update(Message::BatchCheckSel(true));
        assert_eq!(
            checked(&app),
            vec!["https://a.b/small.zip", "https://a.b/tiny.txt"],
            "the files above the run stay out of the batch"
        );
        let _ = app.update(Message::BatchCheckSel(false));
        assert!(checked(&app).is_empty());
    }

    /// Cmd/Ctrl-click adds and removes one row at a time, leaving the rest of
    /// the selection where it was.
    #[test]
    fn a_command_click_toggles_one_row_of_the_selection() {
        let mut app = App::default();
        app.batch.checks = vec![
            ("https://a.b/x.zip".into(), false),
            ("https://a.b/y.zip".into(), false),
            ("https://a.b/z.zip".into(), false),
        ];
        app.batch.parsed = true;
        let _ = app.update(Message::BatchRowClick(0));
        app.mods = iced::keyboard::Modifiers::COMMAND;
        let _ = app.update(Message::BatchRowClick(2));
        assert_eq!(sel(&app), vec![0, 2]);
        let _ = app.update(Message::BatchRowClick(2));
        assert_eq!(sel(&app), vec![0], "the second click takes it back out");
        app.mods = iced::keyboard::Modifiers::default();
        // A plain click is a fresh selection, not another addition.
        let _ = app.update(Message::BatchRowClick(1));
        assert_eq!(sel(&app), vec![1]);
    }

    /// A row that a filter hides leaves the selection with it: "Check
    /// Selected" must not opt in a link the table is not showing, the same
    /// rule "Check All" follows.
    #[test]
    fn hiding_a_row_drops_it_from_the_selection() {
        let mut app = App::default();
        app.batch.checks = vec![
            ("https://a.b/x.zip".into(), false),
            ("https://a.b/index.html".into(), false),
            ("https://a.b/y.iso".into(), false),
        ];
        app.batch.parsed = true;
        let _ = app.update(Message::BatchRowClick(0));
        app.mods = iced::keyboard::Modifiers::SHIFT;
        let _ = app.update(Message::BatchRowClick(2));
        assert_eq!(sel(&app), vec![0, 1, 2]);
        app.mods = iced::keyboard::Modifiers::default();

        let _ = app.update(Message::BatchHideHtml(true));
        assert_eq!(sel(&app), vec![0, 2]);
        let _ = app.update(Message::BatchCheckSel(true));
        assert_eq!(
            checked(&app),
            vec!["https://a.b/x.zip", "https://a.b/y.iso"],
            "the hidden page was never checked"
        );
    }

    /// The same rule for the other filter: a repeat of a link is a row of its
    /// own while duplicates are shown, and goes with them when they are hidden.
    #[test]
    fn hiding_duplicates_drops_the_repeat_from_the_selection() {
        let mut app = App::default();
        app.batch.checks = vec![
            ("https://a.b/x.zip".into(), false),
            ("https://a.b/x.zip".into(), false),
            ("https://a.b/y.iso".into(), false),
        ];
        app.batch.parsed = true;
        let _ = app.update(Message::BatchHideDups(false));
        let _ = app.update(Message::BatchRowClick(0));
        app.mods = iced::keyboard::Modifiers::SHIFT;
        let _ = app.update(Message::BatchRowClick(2));
        assert_eq!(sel(&app), vec![0, 1, 2]);
        app.mods = iced::keyboard::Modifiers::default();

        let _ = app.update(Message::BatchHideDups(true));
        assert_eq!(sel(&app), vec![0, 2]);
        let _ = app.update(Message::BatchCheckSel(true));
        assert_eq!(
            checked(&app),
            vec!["https://a.b/x.zip", "https://a.b/y.iso"]
        );
    }

    /// Editing the box re-numbers the rows, so a highlight made before the
    /// edit no longer points at the link it was drawn on. Checked state is
    /// carried over by URL and does survive.
    #[test]
    fn editing_the_url_box_drops_a_selection_made_before_it() {
        let mut app = App::default();
        let paste = |t: &str| {
            Message::BatchEdit(text_editor::Action::Edit(text_editor::Edit::Paste(
                std::sync::Arc::new(t.to_string()),
            )))
        };
        let _ = app.update(paste("https://a.b/x.zip\nhttps://a.b/y.zip\n"));
        let _ = app.update(Message::BatchRowClick(1));
        let _ = app.update(Message::BatchCheckSel(false));
        assert_eq!(sel(&app), vec![1]);

        let _ = app.update(paste("https://a.b/z.zip\n"));
        assert!(sel(&app).is_empty() && app.batch.sel_anchor.is_none());
        assert_eq!(
            checked(&app),
            vec!["https://a.b/x.zip", "https://a.b/z.zip"],
            "unchecking y.zip outlived the edit"
        );
    }

    /// A background transfer that finishes while File Info is still open must
    /// not discard what was typed there. The 2.6 MB case: landed under the
    /// provisional `_1` name a second after the duplicate prompt, while the
    /// real name was still being typed.
    #[test]
    fn edits_typed_before_a_fast_finish_are_applied_to_the_finished_file() {
        let dir = std::env::temp_dir().join(format!("hydra_fi_edit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_string_lossy().into_owned();
        let mut d = item(7, &dir_s, "archive_1.zip", None, DlState::Complete);
        std::fs::write(d.full_path(), b"bytes").unwrap();
        let fi = FileInfoState {
            dl: 7,
            file_name: "archive_1 کلم بروکلی.zip".into(),
            name_touched: true,
            save_dir: dir_s.clone(),
            ..FileInfoState::default()
        };
        let moved = adopt_dialog_edits(&mut d, &fi).unwrap();
        assert_eq!(d.file_name, "archive_1 کلم بروکلی.zip");
        assert!(d.name_locked, "a name the person typed is theirs to keep");
        let want = dir.join("archive_1 کلم بروکلی.zip");
        assert_eq!(moved.as_deref(), Some(want.as_path()));
        assert!(
            want.is_file(),
            "the finished file must follow the typed name"
        );
        assert!(!dir.join("archive_1.zip").exists());
        // Untouched fields are left alone, and nothing moves.
        let mut d2 = item(8, &dir_s, "keep.zip", None, DlState::Complete);
        std::fs::write(d2.full_path(), b"bytes").unwrap();
        let fi2 = FileInfoState {
            dl: 8,
            file_name: "ignored.zip".into(),
            name_touched: false,
            save_dir: dir_s.clone(),
            ..FileInfoState::default()
        };
        assert_eq!(adopt_dialog_edits(&mut d2, &fi2).unwrap(), None);
        assert_eq!(d2.file_name, "keep.zip");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The complete dialog's Move/Rename lands the file where the panel said
    /// and leaves the row pointing there, so the dialog's Open / Open folder
    /// and the list both follow it.
    #[test]
    fn a_file_moved_from_the_complete_dialog_is_followed_by_its_row() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-dlg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let d = item(
            4,
            &dir.to_string_lossy(),
            "setup.exe",
            None,
            DlState::Complete,
        );
        std::fs::write(d.full_path(), b"bytes").expect("finished file");
        let mut app = App::default();
        app.state.downloads.push(d);
        let to = dir.join("installers").join("app-setup.exe");

        let _ = app.update(Message::MoveRenameTo(4, to.clone()));

        assert!(to.is_file());
        assert!(
            !dir.join("setup.exe").exists(),
            "a move leaves no copy behind"
        );
        let d = app.item(4).expect("still listed");
        assert_eq!(d.full_path(), to);
        assert!(d.name_locked, "a later probe must not rename it back");
        assert!(app.state_dirty, "the new place has to outlive the session");
        assert!(app.confirm.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that went missing between the dialog and the panel is reported,
    /// and the row keeps the path it had rather than claiming the new one.
    #[test]
    fn a_failed_move_is_reported_and_the_row_keeps_its_path() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-fail-{}", std::process::id()));
        let d = item(
            5,
            &dir.to_string_lossy(),
            "gone.zip",
            None,
            DlState::Complete,
        );
        let before = d.full_path();
        let mut app = App::default();
        app.state.downloads.push(d);

        let _ = app.update(Message::MoveRenameTo(5, dir.join("elsewhere.zip")));

        assert!(matches!(app.confirm, Some(ConfirmKind::MoveFailed(_))));
        assert_eq!(app.item(5).expect("still listed").full_path(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Active transfers cannot be moved: the panel is not offered for a
    /// transfer that is actively running, or for a complete file that is
    /// no longer where the row says.
    #[test]
    fn move_rename_is_not_offered_for_active_or_missing_files() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let dir_s = dir.to_string_lossy().into_owned();
        let running = item(6, &dir_s, "live.iso", None, DlState::Receiving);
        std::fs::write(running.full_path(), b"bytes").expect("file on disk");
        let missing = item(7, &dir_s, "deleted.iso", None, DlState::Complete);
        let finished = item(8, &dir_s, "done.iso", None, DlState::Complete);
        std::fs::write(finished.full_path(), b"bytes").expect("file on disk");
        let paused = item(10, &dir_s, "stopped.iso", None, DlState::Paused);
        let mut app = App::default();
        app.state
            .downloads
            .extend([running, missing, finished, paused]);

        assert_eq!(app.update(Message::MoveRename(6)).units(), 0);
        assert_eq!(app.update(Message::MoveRename(7)).units(), 0);
        assert_eq!(app.update(Message::MoveRename(99)).units(), 0);
        assert!(
            app.update(Message::MoveRename(8)).units() > 0,
            "the panel opens"
        );
        assert!(
            app.update(Message::MoveRename(10)).units() > 0,
            "the panel opens for paused transfer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The list's menu entry moves the row the user picked, and with nothing
    /// picked it has nothing to ask about.
    #[test]
    fn the_menu_entry_moves_the_selected_row() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let d = item(
            9,
            &dir.to_string_lossy(),
            "notes.pdf",
            None,
            DlState::Complete,
        );
        std::fs::write(d.full_path(), b"bytes").expect("finished file");
        let mut app = App::default();
        app.state.downloads.push(d);
        let move_rename = |app: &mut App| app.update(Message::Menu(MenuAction::MoveRenameSel));

        assert_eq!(move_rename(&mut app).units(), 0);
        app.selected = vec![9];
        assert!(move_rename(&mut app).units() > 0, "the panel opens");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_rename_context_entry_enabled_for_inactive_downloads() {
        let mut app = App::default();
        let d = item(1, "/tmp", "test.bin", None, DlState::Receiving);
        app.state.downloads.push(d);
        app.selected = vec![1];

        let entries = crate::ui::menu::context_entries(&app);
        let mv = entries
            .iter()
            .find(|e| e.action == Some(MenuAction::MoveRenameSel))
            .expect("move/rename entry");
        assert!(!mv.enabled);

        app.state.downloads[0].state = DlState::Paused;
        let entries = crate::ui::menu::context_entries(&app);
        let mv = entries
            .iter()
            .find(|e| e.action == Some(MenuAction::MoveRenameSel))
            .expect("move/rename entry");
        assert!(mv.enabled);

        app.state.downloads[0].state = DlState::Complete;
        let entries = crate::ui::menu::context_entries(&app);
        let mv = entries
            .iter()
            .find(|e| e.action == Some(MenuAction::MoveRenameSel))
            .expect("move/rename entry");
        assert!(mv.enabled);
    }

    #[test]
    fn a_paused_download_with_part_file_is_moved_and_renamed() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-part-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let d = item(
            11,
            &dir.to_string_lossy(),
            "partial.bin",
            None,
            DlState::Paused,
        );
        let from_part = d.part_file();
        std::fs::write(&from_part, b"40percentbytes").expect("part file");
        let mut app = App::default();
        app.state.downloads.push(d);
        let to = dir.join("dest").join("renamed.bin");

        let _ = app.update(Message::MoveRenameTo(11, to.clone()));

        let to_part = dir.join("dest").join("renamed.bin.part");
        assert!(to_part.is_file());
        assert_eq!(std::fs::read(&to_part).unwrap(), b"40percentbytes");
        assert!(!from_part.exists());
        let d = app.item(11).expect("still listed");
        assert_eq!(d.full_path(), to);
        assert_eq!(d.part_file(), to_part);
        assert_eq!(
            d.part_path.as_deref(),
            Some(to_part.to_string_lossy().as_ref())
        );
        assert!(d.name_locked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_paused_stream_download_moves_all_companion_files() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let d = item(
            12,
            &dir.to_string_lossy(),
            "clip.mp4",
            None,
            DlState::Paused,
        );
        let from_part = d.part_file();
        std::fs::write(&from_part, b"stream-part").expect("part");
        let t0 = from_part.with_file_name("clip.mp4.part.t0");
        let t0_ck = from_part.with_file_name("clip.mp4.part.t0.ck");
        std::fs::write(&t0, b"track0").expect("t0");
        std::fs::write(&t0_ck, b"checkpoint0").expect("t0_ck");

        let mut app = App::default();
        app.state.downloads.push(d);
        let to = dir.join("streams").join("new-clip.mp4");

        let _ = app.update(Message::MoveRenameTo(12, to.clone()));

        let to_part = dir.join("streams").join("new-clip.mp4.part");
        let new_t0 = dir.join("streams").join("new-clip.mp4.part.t0");
        let new_t0_ck = dir.join("streams").join("new-clip.mp4.part.t0.ck");

        assert!(to_part.is_file());
        assert!(new_t0.is_file());
        assert!(new_t0_ck.is_file());
        assert!(!from_part.exists());
        assert!(!t0.exists());
        assert!(!t0_ck.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unstarted_download_without_part_file_updates_metadata_on_move() {
        let dir = std::env::temp_dir().join(format!("hydra-mv-unstarted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let d = item(
            13,
            &dir.to_string_lossy(),
            "pending.zip",
            None,
            DlState::Queued,
        );
        let mut app = App::default();
        app.state.downloads.push(d);
        let to = dir.join("queue").join("renamed-pending.zip");

        let _ = app.update(Message::MoveRenameTo(13, to.clone()));

        let d = app.item(13).expect("still listed");
        assert_eq!(d.full_path(), to);
        assert_eq!(d.file_name, "renamed-pending.zip");
        assert!(d.name_locked);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            cookie_source: None,
            referer: None,
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
            plugin_details: vec![],
            status_line: String::new(),
            shutdown_after: false,
            shutdown_action: PowerAction::default(),
            stream: None,
            plugin_plan: None,
            metalink: None,
            name_locked: false,
            proxy: ProxyChoice::default(),
        }
    }

    /// What a row may present as its ceiling is the tighter of the two caps.
    ///
    /// The Speed Limiter used to be handed to the transfer as its own cap,
    /// which made "the limit in force" a single number to look up. It is an
    /// aggregate now, so a download under both caps cannot exceed either, and
    /// a row claiming its own 500 KB/s under a 128 KB/s app-wide limit would
    /// be promising bandwidth no transfer can get.
    #[test]
    fn a_row_shows_the_tighter_of_the_download_and_app_wide_caps() {
        let mut app = App::default();
        let mut d = item(1, "/tmp", "x.zip", None, DlState::Receiving);

        assert_eq!(app.effective_limit(&d), None, "neither cap is set");

        app.cfg.settings.global_speed_limit = Some(128 * 1024);
        app.cfg.settings.speed_limiter_on = true;
        assert_eq!(app.effective_limit(&d), Some(128 * 1024));

        d.speed_limit = Some(500 * 1024);
        assert_eq!(
            app.effective_limit(&d),
            Some(128 * 1024),
            "the app-wide cap is the tighter one"
        );

        d.speed_limit = Some(64 * 1024);
        assert_eq!(
            app.effective_limit(&d),
            Some(64 * 1024),
            "the download's own cap is the tighter one"
        );

        app.cfg.settings.speed_limiter_on = false;
        assert_eq!(
            app.effective_limit(&d),
            Some(64 * 1024),
            "switching the Speed Limiter off leaves the download's own cap"
        );
    }

    /// "Start Download As New" over a finished file, and Redownload, both go
    /// through here: what is left of the first transfer has to be gone before
    /// the next start, or the resume check accepts the stale staging file and
    /// the new object is written into the old one's bytes.
    #[test]
    fn a_redownload_leaves_nothing_behind_for_the_next_start_to_resume_from() {
        let dir = std::env::temp_dir().join(format!("hydra-redl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut d = item(1, &dir.to_string_lossy(), "x.zip", None, DlState::Complete);
        let part = dir.join("x.zip.part");
        std::fs::write(&part, b"old bytes").expect("staging file");
        d.part_path = Some(part.to_string_lossy().into_owned());
        d.held = vec![(0, 9)];
        d.downloaded = 9;
        d.disp_progress = 1.0;
        d.error = Some("HEAD failed".into());

        reset_for_redownload(&mut d);

        assert!(!part.exists(), "the staging file must not survive");
        assert!(d.held.is_empty() && d.downloaded == 0 && d.part_path.is_none());
        assert_eq!(d.disp_progress, 0.0);
        assert_eq!(d.state, DlState::Paused);
        // The Result line described the transfer that is being replaced.
        assert_eq!(d.error, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_main_window_comes_back_the_size_it_was_left_at_whatever_the_scale() {
        use super::main_open_size;

        // What a resize stores (`WinResized`) is the window in OS points.
        // Reopening divides by the scale because iced multiplies the size
        // handed to `window::open` by it again — get that wrong and the
        // window grows by the ratio on every launch, until it is bigger
        // than the sanity range above and snaps back to the default size.
        let left_at = (1400.0, 900.0);
        for pct in crate::theme::SCALE_STEPS {
            let scale = crate::theme::ui_scale(pct);
            let (w, h) = main_open_size(Some(left_at), scale);
            assert!(
                (w * scale - left_at.0).abs() < 0.5 && (h * scale - left_at.1).abs() < 0.5,
                "{pct}% reopens a {left_at:?} window at {}x{} points",
                w * scale,
                h * scale
            );
        }

        // A saved size no screen could have produced is not restored — it
        // derives from the display instead, which is never this small.
        let (w, _) = main_open_size(Some((80.0, 40.0)), 1.0);
        assert!(
            w >= super::main_min_w(),
            "nonsense is replaced, not restored: {w}"
        );
    }

    #[test]
    fn add_url_reserves_bounded_space_for_extractor_diagnostics() {
        for source in ["plugin", "stream", "metalink"] {
            let mut app = App::default();
            let (_, initial_height) = app.window_size(WinKind::AddUrl);
            let error = "WARNING: No title found in player responses\nERROR: Sign in to confirm you're not a bot. Use browser cookies for authentication. ".repeat(4);
            match source {
                "plugin" => app.add_url.error = Some(error),
                "stream" => app.add_url.stream_error = Some(error),
                _ => app.add_url.metalink_error = Some(error),
            }
            let (_, error_height) = app.window_size(WinKind::AddUrl);
            assert!(error_height > initial_height + 40.0);
            assert!(error_height <= initial_height + 120.0);
        }
        let mut app = App::default();
        let (_, initial_height) = app.window_size(WinKind::AddUrl);
        app.add_url.error = Some("Invalid URL".into());
        assert_eq!(app.window_size(WinKind::AddUrl).1, initial_height + 40.0);
    }

    #[test]
    fn a_menu_is_placed_against_the_window_in_the_units_it_is_laid_out_in() {
        use super::main_viewport;

        // A resize reports OS points; the overlays, and the cursor position
        // a menu is placed at, are in interface units. Skip the conversion
        // and a 150% window reads as half again as tall as it is, so a menu
        // near the bottom is left running off it.
        let opened_at = (900.0, 600.0);
        let scale = crate::theme::ui_scale(150);
        let v = main_viewport(iced::Size::new(1400.0, 900.0), opened_at, scale);
        assert!((v.width - 1400.0 / scale).abs() < 0.5 && (v.height - 900.0 / scale).abs() < 0.5);

        // No resize has arrived yet: the size the window was opened at is
        // already in interface units and is used as it stands.
        assert_eq!(
            main_viewport(iced::Size::ZERO, opened_at, scale),
            iced::Size::new(900.0, 600.0)
        );
    }

    #[test]
    fn a_display_is_measured_in_points_whichever_unit_it_reports() {
        use super::display_normalized;
        let rect = |x, y, w, h| iced::Rectangle::new(Point::new(x, y), iced::Size::new(w, h));

        // A 1080p laptop at 125%, reported in physical pixels: 1536x864.
        let scaled = display_normalized(rect(0.0, 0.0, 1920.0, 1080.0), 1.25);
        assert_eq!(scaled, rect(0.0, 0.0, 1536.0, 864.0));

        // The same display reported in points already stays as it is: a
        // second division would leave 1229x691 and shrink every dialog.
        let points = display_normalized(rect(0.0, 0.0, 1536.0, 864.0), 1.0);
        assert_eq!(points, rect(0.0, 0.0, 1536.0, 864.0));

        // A small screen at 2x is a genuinely small screen, not a 2560 one
        // reported in pixels.
        let small = display_normalized(rect(0.0, 0.0, 1280.0, 800.0), 2.0);
        assert_eq!(small, rect(0.0, 0.0, 1280.0, 800.0));

        // A second monitor's origin is in the same unit as its size, so it
        // converts with it — or a window left on it reads as off every screen.
        let right = display_normalized(rect(1920.0, 0.0, 1920.0, 1080.0), 1.25);
        assert_eq!(right, rect(1536.0, 0.0, 1536.0, 864.0));
    }

    #[test]
    fn the_main_window_reopens_where_it_was_left_while_that_is_on_a_screen() {
        use super::main_open_position;
        let rect = |x, y, w, h| iced::Rectangle::new(Point::new(x, y), iced::Size::new(w, h));
        let laptop = rect(0.0, 0.0, 1512.0, 982.0);
        let external = rect(1512.0, -200.0, 2560.0, 1440.0);

        assert_eq!(
            main_open_position(Some((300.0, 120.0)), 1000.0, &[laptop]),
            Some(Point::new(300.0, 120.0))
        );
        assert_eq!(
            main_open_position(Some((2000.0, -150.0)), 1000.0, &[laptop, external]),
            Some(Point::new(2000.0, -150.0))
        );
        // Maximized on Windows: the outer frame starts just off the screen.
        assert_eq!(
            main_open_position(Some((-8.0, -8.0)), 1528.0, &[laptop]),
            Some(Point::new(-8.0, -8.0))
        );
        // Mostly off the left edge but the title bar still reachable.
        assert_eq!(
            main_open_position(Some((-400.0, 100.0)), 1000.0, &[laptop]),
            Some(Point::new(-400.0, 100.0))
        );

        // Left on the external monitor, which is gone now.
        assert_eq!(
            main_open_position(Some((2000.0, -150.0)), 1000.0, &[laptop]),
            None
        );
        // Title bar above the top edge: nothing left to drag it back by.
        assert_eq!(
            main_open_position(Some((300.0, -40.0)), 1000.0, &[laptop]),
            None
        );
        assert_eq!(main_open_position(None, 1000.0, &[laptop]), None);
        // The platform would not list its displays: nothing to check against.
        assert_eq!(main_open_position(Some((300.0, 120.0)), 1000.0, &[]), None);
    }

    #[test]
    fn moving_the_main_window_is_remembered_in_os_points() {
        let mut app = App::default();
        app.cfg.settings.ui_scale_pct = 150;
        let main = window::Id::unique();
        let dialog = window::Id::unique();
        app.main_id = Some(main);

        // iced reports the move in interface units, divided by the scale.
        let _ = app.update(Message::WinMoved(main, Point::new(200.0, 60.0)));
        assert_eq!(app.cfg.settings.window_pos, Some((300.0, 90.0)));
        assert!(app.cfg_dirty, "the position has to outlive the session");

        let _ = app.update(Message::WinMoved(dialog, Point::new(10.0, 10.0)));
        assert_eq!(app.cfg.settings.window_pos, Some((300.0, 90.0)));

        // Minimizing on Windows: the spot it was left at is what reopens.
        let _ = app.update(Message::WinMoved(main, Point::new(-21333.0, -21333.0)));
        assert_eq!(app.cfg.settings.window_pos, Some((300.0, 90.0)));
        assert_eq!(app.main_pos, Some(Point::new(300.0, 90.0)));
    }

    #[test]
    fn minimizing_on_windows_is_not_remembered_as_a_move() {
        use super::is_parked;

        // (-32000, -32000) pixels, at 100% and at the largest Windows scale.
        assert!(is_parked(Point::new(-32000.0, -32000.0)));
        assert!(is_parked(Point::new(-6400.0, -6400.0)));

        assert!(!is_parked(Point::new(0.0, 0.0)));
        assert!(!is_parked(Point::new(-8.0, -8.0)));
        // Three 2560-point monitors to the left of the primary one.
        assert!(!is_parked(Point::new(-7680.0, 40.0)));
    }

    #[test]
    fn a_dialog_never_opens_taller_than_the_screen_it_opens_on() {
        use super::fit_to_display;

        // The screen the cut-off OK button was reported on: 1920x1080 at
        // 125% display scaling is 1536x864 OS points.
        let laptop = iced::Size::new(1536.0, 864.0);
        // What the Configuration window asks for, at every scale the View
        // menu offers. The window reaches the screen multiplied by the
        // ratio, so that is what has to fit — with room to spare for the
        // taskbar and the title bar.
        for pct in crate::theme::SCALE_STEPS {
            let scale = crate::theme::ui_scale(pct);
            let (w, h) = fit_to_display((760.0, 700.0), laptop, scale);
            assert!(
                h * scale < laptop.height && w * scale < laptop.width,
                "{pct}% opens a {w}x{h} dialog as {}x{} points on a \
                 {}x{} screen",
                w * scale,
                h * scale,
                laptop.width,
                laptop.height
            );
        }

        // A dialog that already fits is not shrunk: the cap is a ceiling,
        // not a layout.
        let desk = iced::Size::new(2560.0, 1440.0);
        assert_eq!(fit_to_display((760.0, 700.0), desk, 1.0), (760.0, 700.0));

        // Neither is one whose screen the platform would not report.
        assert_eq!(
            fit_to_display((760.0, 700.0), iced::Size::ZERO, 1.0),
            (760.0, 700.0)
        );
    }

    #[test]
    fn progress_dialog_window_size_matches_detail_toggle() {
        let mut app = App::default();
        let dl = 1;
        assert_eq!(
            app.window_size(WinKind::Progress(dl)),
            crate::windows::progress::standard_size(app.cfg.settings.show_conn_details)
        );

        app.prog.entry(dl).or_default().details = true;
        assert_eq!(
            app.window_size(WinKind::Progress(dl)),
            (
                crate::windows::progress::WIDTH,
                crate::windows::progress::HEIGHT_DETAILS,
            )
        );

        app.prog.entry(dl).or_default().details = false;
        assert_eq!(
            app.window_size(WinKind::Progress(dl)),
            (
                crate::windows::progress::WIDTH,
                crate::windows::progress::HEIGHT_COLLAPSED,
            )
        );
    }

    #[test]
    fn the_mirror_list_panel_is_measured_by_what_it_actually_draws() {
        // The failure this guards against is silent: too short a window puts
        // the OK button below the bottom edge, and nothing at run time
        // notices. The panel draws at most three file rows and then one "+N"
        // row, so the height must stop growing at four rows however many files
        // the document describes.
        assert_eq!(
            metalink_panel_height(false, None),
            0.0,
            "no panel, no space"
        );
        assert!(metalink_panel_height(true, None) > 0.0, "the probing line");
        let one = metalink_panel_height(false, Some(1));
        let three = metalink_panel_height(false, Some(3));
        assert!(three > one, "each file adds a row");
        let four = metalink_panel_height(false, Some(4));
        assert!(four > three, "the \"+N\" row is drawn and must be reserved");
        assert_eq!(
            metalink_panel_height(false, Some(40)),
            four,
            "a forty-file document draws the same four rows as a four-file one"
        );
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

    /// The reported defect: a presigned link captured from the browser was
    /// parked behind the File Info dialog until its signature died, so every
    /// attempt ended in `403 Request has expired`. Ten seconds is what the
    /// origin in the report really issues.
    #[test]
    fn a_short_lived_signed_link_is_recognised_as_perishable() {
        let now = crate::fmt::now_unix().max(0) as u64;
        let ten_seconds = format!(
            "https://s3q.ait.dtu.dk:9000/figshare/26003087/TEP_Mode1.h5\
?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Date={}&X-Amz-Expires=10&X-Amz-Signature=ab",
            crate::app::tests::basic_iso8601(now)
        );
        assert!(expiring_soon(&ten_seconds));

        // A plain URL waits for the dialog like anything else.
        assert!(!expiring_soon("https://example.com/big.iso"));
        // So does a signature with a week of headroom: the dialog costs it
        // nothing, and suppressing it there would be a UI change with no
        // failure behind it.
        let a_week = format!(
            "https://ex.com/o?X-Amz-Date={}&X-Amz-Expires=604800&X-Amz-Signature=ab",
            crate::app::tests::basic_iso8601(now)
        );
        assert!(!expiring_soon(&a_week));
    }

    /// Render a unix timestamp the way SigV4 writes `X-Amz-Date`, so the test
    /// above can build a URL that is expiring RIGHT NOW rather than pinning a
    /// date that silently stops being short-lived as the clock passes it.
    pub(super) fn basic_iso8601(unix: u64) -> String {
        use chrono::{TimeZone, Utc};
        Utc.timestamp_opt(unix as i64, 0)
            .single()
            .expect("valid timestamp")
            .format("%Y%m%dT%H%M%SZ")
            .to_string()
    }

    /// The extension fills in what it knows and says nothing about the rest;
    /// "nothing" arrives as both `None` and `""`, and neither may reach the
    /// item — a blank name would replace the one derived from the URL.
    /// The leak the import must not have: paste address A, paste address B
    /// over it while A's import is still running, press OK. A's session was
    /// attached to B's download.
    #[test]
    fn an_import_that_lands_after_the_host_changed_is_asked_again_not_applied() {
        let mut app = App::default();
        app.cfg.settings.cookies_from_browser = "firefox".into();
        app.add_url.address = "https://b.test/x".into();
        app.add_url.cookies_of = "https://a.test/x".into();
        app.add_url.cookies_importing = true;
        let _ = app.update(Message::AddrCookiesImported(Box::new(Ok(
            imported_cookies("sid=for-a", "firefox — 1 cookie(s) for a.test"),
        ))));
        assert_eq!(
            app.add_url.capture.cookies, None,
            "a.test's session on b.test"
        );
        assert!(!app.add_url.cookies_imported);
        assert_eq!(
            app.add_url.cookies_of, "https://b.test/x",
            "the address in the box is the one asked about"
        );
        assert!(app.add_url.cookies_importing);
    }

    /// An earlier import's cookies go the moment the host changes — before
    /// any re-import answers — and a typed value, which is the user's, stays.
    /// Typing further into the SAME host's path is not a change of host.
    #[test]
    fn moving_to_another_host_drops_what_an_import_attached_but_not_what_was_typed() {
        let mut app = App::default();
        app.add_url.address = "https://a.test/x".into();
        app.add_url.cookies_of = "https://a.test/x".into();
        app.add_url.capture.cookies = Some("sid=for-a".into());
        app.add_url.capture.cookie_source = Some("firefox".into());
        app.add_url.cookies_imported = true;

        let _ = app.update(Message::AddrChanged("https://a.test/x/deeper".into()));
        assert_eq!(app.add_url.capture.cookies.as_deref(), Some("sid=for-a"));

        let _ = app.update(Message::AddrChanged("https://b.test/x".into()));
        assert_eq!(app.add_url.capture.cookies, None);
        assert_eq!(app.add_url.capture.cookie_source, None);
        assert!(!app.add_url.cookies_imported);

        let _ = app.update(Message::AddrCookies("typed=1".into()));
        let _ = app.update(Message::AddrChanged("https://c.test/x".into()));
        assert_eq!(app.add_url.capture.cookies.as_deref(), Some("typed=1"));
    }

    /// Checks finish in disk order, not in typing order; only the newest may
    /// say what the box will read.
    #[test]
    fn a_stale_cookie_check_does_not_answer_for_a_newer_one() {
        let mut app = App::default();
        app.options.draft.cookies_from_browser = "firefox".into();
        let _ = app.check_cookie_source();
        let old = app.options.cookie_check_gen;
        app.options.draft.cookies_from_browser = "chrome".into();
        let _ = app.check_cookie_source();
        assert_ne!(old, app.options.cookie_check_gen);

        let _ = app.update(Message::OptCookieChecked(old, Box::new(Ok("/old".into()))));
        assert!(
            app.options.cookie_checking,
            "the newer check is still running"
        );
        assert_eq!(app.options.cookie_check, None);

        let now = app.options.cookie_check_gen;
        let _ = app.update(Message::OptCookieChecked(now, Box::new(Ok("/new".into()))));
        assert!(!app.options.cookie_checking);
        assert_eq!(app.options.cookie_check, Some(Ok("/new".into())));
    }

    #[test]
    fn a_capture_says_nothing_rather_than_saying_nothing_twice() {
        let mut extras = CaptureExtras {
            cookies: Some("sid=abc".into()),
            cookie_source: None,
            name: Some(String::new()),
            referer: None,
            proxy: Some(String::new()),
        };
        let taken = extras.taken();
        assert_eq!(taken.cookies.as_deref(), Some("sid=abc"));
        assert_eq!(taken.name, None);
        assert_eq!(taken.referer, None);
        assert_eq!(taken.proxy, None);
        // Taken, not copied: the dialog state must not hand the same cookies
        // to a second download.
        assert!(extras.cookies.is_none());
    }

    /// What the browser knew lands on the item; what it did not say leaves
    /// the item's own value alone. A capture that arrives with the browser's
    /// proxy is reachable through that proxy and nowhere else, so it becomes
    /// this download's route — Options is never touched.
    #[test]
    fn a_captured_title_windows_cannot_store_is_made_writable() {
        let mut d = item(1, "/downloads", "index.html", None, DlState::Queued);
        write_capture_extras(
            &mut d,
            CaptureExtras {
                name: Some("Q:ماینکرفت از هیچ (3) | شوکه شدیم!!.mp4".into()),
                ..CaptureExtras::default()
            },
            None,
        );
        assert_eq!(d.file_name, "Q_ماینکرفت از هیچ (3) _ شوکه شدیم!!.mp4");
    }

    #[test]
    fn an_unstarted_download_saved_with_a_bad_name_is_repaired_on_start() {
        let mut d = item(1, "/dl", "Q:clip | part.mp4", None, DlState::Error);
        d.part_path = Some("/dl/Q:clip | part.mp4.part".into());

        repair_unstarted_name(&mut d);

        assert_eq!(d.file_name, "Q_clip _ part.mp4");
        assert_eq!(
            d.part_file(),
            std::path::Path::new("/dl/Q_clip _ part.mp4.part")
        );
    }

    #[test]
    fn a_download_with_bytes_on_disk_keeps_the_name_they_were_written_under() {
        let mut d = item(1, "/dl", "a:b.iso", None, DlState::Paused);
        d.part_path = Some("/dl/a:b.iso.part".into());
        d.held = vec![(0, 4096)];
        d.downloaded = 4096;

        repair_unstarted_name(&mut d);

        assert_eq!(d.file_name, "a:b.iso");
        assert_eq!(d.part_path.as_deref(), Some("/dl/a:b.iso.part"));
    }

    #[test]
    fn a_name_typed_in_the_dialog_is_made_writable() {
        let mut d = item(1, "/dl", "x.zip", None, DlState::Paused);
        let fi = FileInfoState {
            file_name: "report: final?.pdf".into(),
            save_dir: "/dl".into(),
            name_touched: true,
            ..FileInfoState::default()
        };
        let _ = adopt_dialog_edits(&mut d, &fi);
        assert_eq!(d.file_name, "report_ final_.pdf");
    }

    #[test]
    fn a_capture_writes_only_what_the_browser_actually_knew() {
        let mut d = item(1, "/downloads", "pack.zip", None, DlState::Queued);
        d.cookies = Some("stale=1".into());
        write_capture_extras(
            &mut d,
            CaptureExtras {
                cookies: Some("sid=abc".into()),
                cookie_source: None,
                name: Some("Setup.exe".into()),
                referer: Some("https://page.example/".into()),
                proxy: Some("socks5://127.0.0.1:10808".into()),
            },
            Some((Some("Programs".into()), Some("/downloads/programs".into()))),
        );
        assert_eq!(d.cookies.as_deref(), Some("sid=abc"));
        assert_eq!(d.referer.as_deref(), Some("https://page.example/"));
        assert_eq!(d.file_name, "Setup.exe");
        assert_eq!(d.category.as_deref(), Some("Programs"));
        assert_eq!(d.save_dir, "/downloads/programs");
        assert_eq!(
            d.proxy,
            ProxyChoice::Custom("socks5://127.0.0.1:10808".into())
        );

        // An extension that knew nothing — or one too old to send a proxy —
        // leaves every one of them as it found them.
        write_capture_extras(&mut d, CaptureExtras::default(), None);
        assert_eq!(d.cookies.as_deref(), Some("sid=abc"));
        assert_eq!(d.file_name, "Setup.exe");
        assert_eq!(d.save_dir, "/downloads/programs");
        assert_eq!(
            d.proxy,
            ProxyChoice::Custom("socks5://127.0.0.1:10808".into())
        );

        // Only a rename may move the download: cookies alone must not refile
        // it, which is what a `filing` resolved from the name means.
        write_capture_extras(
            &mut d,
            CaptureExtras {
                cookies: Some("sid=def".into()),
                ..CaptureExtras::default()
            },
            None,
        );
        assert_eq!(d.save_dir, "/downloads/programs");
        assert_eq!(d.category.as_deref(), Some("Programs"));
    }

    /// A proxy the browser reported is this download's own route. Anything
    /// the transport cannot speak leaves the choice untouched rather than
    /// failing the capture: the download is fine, and the setting was made
    /// in a different program.
    #[test]
    fn a_captured_proxy_becomes_this_downloads_own_route() {
        assert_eq!(
            captured_proxy(Some("socks5://127.0.0.1:10808".into())),
            Some(ProxyChoice::Custom("socks5://127.0.0.1:10808".into()))
        );
        assert_eq!(
            captured_proxy(Some("  http://proxy.example:8080  ".into())),
            Some(ProxyChoice::Custom("http://proxy.example:8080".into()))
        );
        assert_eq!(captured_proxy(None), None);
        assert_eq!(captured_proxy(Some(String::new())), None);
        assert_eq!(captured_proxy(Some("   ".into())), None);
        // A scheme the transport does not speak, and a port that is not one.
        assert_eq!(
            captured_proxy(Some("quic://proxy.example:443".into())),
            None
        );
        assert_eq!(
            captured_proxy(Some("http://proxy.example:ohno".into())),
            None
        );
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
        // Decoded for the reader, then made a leaf name: an escaped slash
        // must not become a directory.
        assert_eq!(stream_base_name("https://cdn.ex/My%20Show.m3u8"), "My Show");
        assert_eq!(stream_base_name("https://cdn.ex/a%2Fb.m3u8"), "a b");
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
        // 120 CHARACTERS of Hangul is 360 bytes, which no mainstream
        // filesystem will store — the cap has to bind in bytes too, on a
        // character boundary, with room left for the extension the stream
        // path appends.
        let korean = sanitize_file_name(&"설치프로그램".repeat(60));
        assert!(
            korean.len() <= 250,
            "{} bytes is past what a filesystem takes",
            korean.len()
        );
        assert!("설치프로그램".repeat(60).starts_with(&korean));
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
    fn a_probe_may_not_take_a_name_the_user_chose() {
        // Fresh item, nothing on disk, nobody has named it: the probe's
        // `Content-Disposition` name is exactly what should land.
        let mut d = item(1, "/dl", "index.html", None, DlState::Paused);
        assert!(may_adopt_name(&d));

        // The user typed a name in the File Info dialog and pressed Start
        // Download. The probe belongs to the transfer that press started, so
        // it answers here — with zero bytes in and nothing held, which is
        // what used to make it look like a fresh item.
        d.file_name = "MyName.zip".into();
        d.name_locked = true;
        assert!(!may_adopt_name(&d));

        // The lock survives the restart a Download Later / Start Queue round
        // trip performs, which issues a second probe against zero bytes.
        d.state = DlState::Connecting;
        assert!(!may_adopt_name(&d));

        // Bytes already on disk still pin the name on their own: renaming
        // under a running transfer would orphan them.
        let mut unlocked = item(2, "/dl", "setup.exe", None, DlState::Receiving);
        unlocked.held = vec![(0, 4096)];
        assert!(!may_adopt_name(&unlocked));
    }

    /// A probe that resolved no name must not hand back the placeholder.
    ///
    /// The stream URLs a player overlay captures carry the object in the
    /// query rather than the path (`.../video/tos/?a=1&br=2`) and are served
    /// with no `Content-Disposition`. `file_name_from_url` invents
    /// `index.html` for a path like that so the bytes have somewhere to go,
    /// and the probe used to report the invention as the object's name: it
    /// replaced the title the browser extension had captured and, through
    /// `SetFinalPath`, renamed the finished MP4 to `index.html` at the move
    /// out of the `.part`.
    #[test]
    fn a_probe_that_resolved_no_name_leaves_the_captured_one() {
        let mut app = App::default();
        let url = "https://v3-web.example.com/tok/video/tos/?a=1&br=2";
        let id = app.add_item(url.into(), None, None);
        app.item_mut(id).unwrap().file_name = "video.mp4".into();

        let meta = |name: Option<&str>| engine::LinkMeta {
            size: Some(9),
            file_name: name.map(str::to_string),
            is_metalink: false,
        };
        let _ = app.update(Message::InfoProbed(id, Some(meta(None))));
        assert_eq!(app.item(id).unwrap().file_name, "video.mp4");
        assert_eq!(app.item(id).unwrap().size, Some(9));

        // A name the probe really did resolve still wins: the server named
        // the object, and that beats a guess made from the page title.
        let _ = app.update(Message::InfoProbed(id, Some(meta(Some("clip.mp4")))));
        assert_eq!(app.item(id).unwrap().file_name, "clip.mp4");

        // The batch dialog reads its File Name column from the same probe,
        // and a row the probe could not name keeps the placeholder rather
        // than recording the invention as the resolved name.
        let _ = app.update(Message::BatchProbed(url.into(), Some(meta(None))));
        assert!(!app.batch.names.contains_key(url));
        let _ = app.update(Message::BatchProbed(
            url.into(),
            Some(meta(Some("clip.mp4"))),
        ));
        assert_eq!(
            app.batch.names.get(url).map(String::as_str),
            Some("clip.mp4")
        );
    }

    #[test]
    fn the_duplicate_warning_needs_a_real_file_under_the_real_name() {
        let dir = std::env::temp_dir().join(format!("hydra-dup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_string_lossy().into_owned();

        // Nothing there: no warning.
        assert_eq!(collision_file(&dir_s, "setup.exe", true), None);

        // A real file under the name the download will use: warn.
        std::fs::write(dir.join("setup.exe"), b"old").unwrap();
        assert!(collision_file(&dir_s, "setup.exe", true).is_some());

        // Same file, but the name is only the `index.html` placeholder
        // invented for a URL that named nothing — the probe has not answered
        // yet, so this download is not headed here at all.
        assert_eq!(collision_file(&dir_s, "setup.exe", false), None);

        // A DIRECTORY of that name is not a file to open. With category
        // folders switched off these sit in the same folder downloads land
        // in, and a page-title capture carries no extension to tell them
        // apart.
        std::fs::create_dir_all(dir.join("Programs")).unwrap();
        assert_eq!(collision_file(&dir_s, "Programs", true), None);

        // An EMPTY file is not one either: there is nothing in it to open,
        // and the download that replaces it replaces no bytes.
        std::fs::write(dir.join("empty.bin"), b"").unwrap();
        assert_eq!(collision_file(&dir_s, "empty.bin", false), None);
        assert_eq!(collision_file(&dir_s, "empty.bin", true), None);
        // One byte is a file someone can open, and warning is right again.
        std::fs::write(dir.join("empty.bin"), b"x").unwrap();
        assert!(collision_file(&dir_s, "empty.bin", true).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reported bug: every browser capture warned about a file the
    /// person could not find, because the warning was about the browser's
    /// own placeholder rather than about anything they had downloaded.
    ///
    /// Gecko cannot park a download while Hydra decides, so Firefox's
    /// transfer is still running at capture time — and Firefox reserves its
    /// target name by creating a zero-byte file under the FINAL name while
    /// the bytes go to a `name.<random>.ext.part` sibling. Both files are
    /// deleted the moment the extension cancels the browser's copy, which is
    /// why the folder was empty by the time the dialog was read. Verified
    /// against Firefox 155 on macOS before this test was written.
    #[test]
    fn a_browsers_name_reservation_is_not_a_download_to_open() {
        let dir = std::env::temp_dir().join(format!("hydra-gecko-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir_s = dir.to_string_lossy().into_owned();

        // Exactly what Firefox leaves in the download folder a second after
        // the click, for `libhydra-0.3.0-android.zip`.
        std::fs::write(dir.join("libhydra-0.3.0-android.zip"), b"").unwrap();
        std::fs::write(
            dir.join("libhydra-0.3.0-android.S7v5x6tB.zip.part"),
            vec![0xAB; 9_928_704],
        )
        .unwrap();

        assert_eq!(
            collision_file(&dir_s, "libhydra-0.3.0-android.zip", true),
            None,
            "the browser's own name reservation must not be reported as an existing download"
        );

        // Once the transfer really finishes, the placeholder has become the
        // file: the very next capture of the same asset must warn.
        std::fs::write(
            dir.join("libhydra-0.3.0-android.zip"),
            vec![0xCD; 9_928_704],
        )
        .unwrap();
        let _ = std::fs::remove_file(dir.join("libhydra-0.3.0-android.S7v5x6tB.zip.part"));
        assert_eq!(
            collision_file(&dir_s, "libhydra-0.3.0-android.zip", true).as_deref(),
            Some(
                dir.join("libhydra-0.3.0-android.zip")
                    .to_string_lossy()
                    .as_ref()
            )
        );

        // ...and once the person deletes that file, the warning is gone for
        // good — the case the issue was filed about.
        std::fs::remove_file(dir.join("libhydra-0.3.0-android.zip")).unwrap();
        assert_eq!(
            collision_file(&dir_s, "libhydra-0.3.0-android.zip", true),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
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

    /// The Result line carries the server's own sentence, and the dialog does
    /// not scroll: measuring a two-line message as one is what squeezed the
    /// row below it — the proxy row — out of shape.
    #[test]
    fn a_long_result_message_is_measured_by_the_lines_it_takes() {
        let one = "ftp: login failed";
        let two = "HEAD failed (Connection refused (os error 61)), ranged GET failed \
                   (Connection refused (os error 61))";
        assert_eq!(result_row_height(one), 32.0);
        assert!(
            result_row_height(two) > result_row_height(one),
            "a message that wraps must be given the room to wrap"
        );
        // However long the sentence, the dialog stops growing.
        assert_eq!(
            result_row_height(&"x".repeat(4000)),
            result_row_height(&"x".repeat(200))
        );
    }

    /// Changing a download's proxy is not a field edit like a description:
    /// the transfer has to be stopped and started again, so the dialog has
    /// to be able to tell that it changed at all.
    #[test]
    fn a_changed_proxy_is_what_tells_the_dialog_to_restart_the_transfer() {
        let mut d = item(1, "/tmp", "a.zip", None, DlState::Receiving);
        let fi = FileInfoState::default();
        assert_eq!(reroute(&d, &fi), (ProxyChoice::Default, false));

        let own = FileInfoState {
            proxy_pick: ProxyPick::Custom,
            proxy_spec: "  socks5://127.0.0.1:10808 ".into(),
            ..FileInfoState::default()
        };
        let (want, changed) = reroute(&d, &own);
        assert!(changed);
        assert_eq!(want, ProxyChoice::Custom("socks5://127.0.0.1:10808".into()));

        // The same address again, differently spaced, is not a change: it
        // must not restart a running transfer for nothing.
        d.proxy = want;
        assert!(!reroute(&d, &own).1);

        // The picker alone is enough: "no proxy" on a download that had one
        // is a reroute even though the address box still holds the text.
        let none = FileInfoState {
            proxy_pick: ProxyPick::Direct,
            ..own.clone()
        };
        assert_eq!(reroute(&d, &none), (ProxyChoice::Direct, true));
    }

    #[test]
    fn prog_state_seed_reflects_persisted_limit() {
        let p = prog_state_seed(Some(512 * 1024), &ProxyChoice::Default, true);
        assert!(p.limit_on);
        assert!(p.remember_limit);
        assert_eq!(p.limit_kb, "512");
        let p = prog_state_seed(None, &ProxyChoice::Default, true);
        assert!(!p.limit_on);
        assert!(!p.remember_limit);
        assert_eq!(p.limit_kb, "10");
    }

    /// Downloads > "Show connection details" decides the state a progress
    /// box opens in. Off means collapsed from the first frame — not a panel
    /// that appears and then folds away.
    #[test]
    fn the_connection_details_setting_seeds_a_new_progress_box() {
        for details in [true, false] {
            let p = prog_state_seed(None, &ProxyChoice::Default, details);
            assert_eq!(p.details, details);
        }
        assert!(
            crate::model::Settings::default().show_conn_details,
            "the panel stays on out of the box"
        );
    }

    /// Editing the category list must not strand the downloads already filed
    /// under it: a rename takes its items along, and a deleted category
    /// hands them back to "uncategorised" — either way the item stays
    /// reachable from a node the tree actually draws.
    #[test]
    fn downloads_follow_a_renamed_category_and_survive_a_deleted_one() {
        let mut cats = crate::model::default_categories();
        cats.iter_mut().find(|c| c.name == "Video").unwrap().name = "Movies".into();
        cats.retain(|c| c.name != "Music");
        let mut renames = HashMap::new();
        record_rename(&mut renames, "Video", "Movies");

        let mut dls = vec![
            item(1, "/d", "a.mkv", None, DlState::Complete),
            item(2, "/d", "b.mp3", None, DlState::Complete),
            item(3, "/d", "c.zip", None, DlState::Complete),
        ];
        dls[0].category = Some("Video".into());
        dls[1].category = Some("Music".into());
        dls[2].category = Some("Compressed".into());

        assert!(refile_downloads(&mut dls, &renames, &cats));
        assert_eq!(dls[0].category.as_deref(), Some("Movies"));
        assert_eq!(dls[1].category, None);
        assert_eq!(dls[2].category.as_deref(), Some("Compressed"));
        assert!(
            !refile_downloads(&mut dls, &renames, &cats),
            "a second pass has nothing left to move"
        );
    }

    /// Renaming twice before pressing OK still has to map from the name the
    /// downloads carry, not from the intermediate one nothing was filed under.
    #[test]
    fn a_category_renamed_twice_is_followed_from_its_original_name() {
        let mut renames = HashMap::new();
        record_rename(&mut renames, "Video", "Movies");
        record_rename(&mut renames, "Movies", "Films");
        assert_eq!(renames.get("Video").map(String::as_str), Some("Films"));
        assert_eq!(renames.len(), 1, "in {renames:?}");
    }

    #[test]
    fn the_tree_follows_a_rename_and_falls_back_from_a_deletion() {
        let mut cats = crate::model::default_categories();
        cats.iter_mut().find(|c| c.name == "Video").unwrap().name = "Movies".into();
        cats.retain(|c| c.name != "Music");
        let mut renames = HashMap::new();
        record_rename(&mut renames, "Video", "Movies");
        let follow = |sel| follow_tree_sel(&sel, &renames, &cats);

        assert_eq!(
            follow(TreeSel::Cat("Video".into())),
            TreeSel::Cat("Movies".into())
        );
        assert_eq!(
            follow(TreeSel::UnfCat("Video".into())),
            TreeSel::UnfCat("Movies".into())
        );
        assert_eq!(follow(TreeSel::FinCat("Music".into())), TreeSel::Finished);
        assert_eq!(follow(TreeSel::Cat("Music".into())), TreeSel::All);
        assert_eq!(
            follow(TreeSel::Queue("Main download queue".into())),
            TreeSel::Queue("Main download queue".into())
        );
    }

    /// The Save-to tab's whole category workflow: create one, give it file
    /// types another category already claimed, rename it, and delete it.
    #[test]
    fn a_category_can_be_created_typed_renamed_and_deleted() {
        let mut st = OptionsState {
            cat_name: "Pictures: PNG JPG".into(),
            ..Default::default()
        };
        st.add_category();
        assert_eq!(st.sel_category, "Pictures");
        assert_eq!(st.cat_name, "Pictures", "the box drops the types it used");
        assert_eq!(st.cat_exts_edit.text(), "PNG JPG");

        st.cat_exts_edit = text_editor::Content::with_text("PNG JPG .ISO");
        st.select_category("Programs".into());
        let pics = st.draft_cats.iter().find(|c| c.name == "Pictures").unwrap();
        assert_eq!(pics.exts, ["png", "jpg", "iso"]);
        assert!(
            !st.draft_cats
                .iter()
                .find(|c| c.name == "Programs")
                .unwrap()
                .exts
                .contains(&"iso".to_string()),
            "iso was claimed by Pictures"
        );
        // Selecting a category fills the boxes from it.
        assert_eq!(st.cat_name, "Programs");
        assert!(st.cat_exts_edit.text().contains("EXE"));

        st.select_category("Pictures".into());
        // Rename takes a name, not a list: a box carrying types is a New.
        st.cat_name = "Images: GIF".into();
        assert!(!st.can_rename_category());
        st.rename_category();
        assert_eq!(st.sel_category, "Pictures");
        st.cat_name = "Images".into();
        st.rename_category();
        assert_eq!(st.sel_category, "Images");
        assert_eq!(st.cat_renames.get("Pictures").unwrap(), "Images");
        assert!(st.draft_cats.iter().any(|c| c.name == "Images"));

        assert!(st.cat_is_removable(), "a category the user made is theirs");
        st.remove_category();
        assert!(!st.draft_cats.iter().any(|c| c.name == "Images"));
        assert_eq!(st.sel_category, crate::model::DEFAULT_CATEGORY);
    }

    /// The stock categories are named by the tree icons, by `categorize`'s
    /// extension-less fallback and by the AI seeding, and General is the
    /// folder every uncategorised download resolves through — so the tab
    /// offers neither rename nor remove on any of them, however the buttons
    /// are reached. Their file types stay editable: that is the whole point
    /// of adding MSIX to Programs.
    #[test]
    fn a_stock_category_can_be_retyped_but_not_renamed_or_removed() {
        let mut st = OptionsState::default();
        let before = st.draft_cats.len();

        for stock in ["General", "Programs", crate::model::AI_CATEGORY] {
            st.select_category(stock.into());
            assert!(!st.cat_is_removable(), "{stock} must keep its identity");
            st.cat_name = "Renamed".into();
            st.rename_category();
            st.remove_category();
            assert_eq!(st.sel_category, stock);
        }
        assert_eq!(st.draft_cats.len(), before);
        assert!(st.cat_renames.is_empty());

        st.select_category("Programs".into());
        st.cat_exts_edit = text_editor::Content::with_text("EXE MSI MSIX MSIXBUNDLE");
        st.commit_cat_exts();
        let programs = st.draft_cats.iter().find(|c| c.name == "Programs").unwrap();
        assert_eq!(programs.exts, ["exe", "msi", "msix", "msixbundle"]);
    }

    /// A name another category already has would make the by-name lookups
    /// ambiguous; a non-ASCII one would slip past the case-insensitive
    /// duplicate check and collide in the folder it creates.
    #[test]
    fn a_duplicate_or_unusable_name_is_refused() {
        let mut st = OptionsState::default();
        let before = st.draft_cats.len();

        for bad in [
            "Video",
            "video",
            "  Video  ",
            "",
            "../etc",
            "Bilder fur mich!\u{e9}",
        ] {
            st.cat_name = bad.into();
            assert!(!st.can_name_category(), "{bad:?} should be refused");
            st.add_category();
        }
        assert_eq!(st.draft_cats.len(), before, "nothing was added");
    }

    /// Typing a file type is not a commit: claiming an extension takes it off
    /// every other category, and doing that per keystroke would delete "z"
    /// from Compressed on the way to "zip".
    #[test]
    fn a_half_typed_file_type_does_not_raid_another_category() {
        let mut st = OptionsState {
            cat_name: "Pictures".into(),
            ..Default::default()
        };
        st.add_category();

        st.cat_exts_edit = text_editor::Content::with_text("Z");
        st.cat_exts_edit = text_editor::Content::with_text("ZIP");
        st.commit_cat_exts();

        let compressed = st
            .draft_cats
            .iter()
            .find(|c| c.name == "Compressed")
            .unwrap();
        assert!(compressed.exts.contains(&"z".to_string()));
        assert!(!compressed.exts.contains(&"zip".to_string()));
    }

    /// The reported bug: the native panel ran the platform's modal loop on
    /// the event-loop thread, so browsing for a save folder froze the app —
    /// and on Windows left the panel itself sharing its message pump with
    /// every running transfer. The answer now arrives as a message, which is
    /// what these three cover: the path still lands where it used to.
    #[test]
    fn a_save_as_answer_reaches_the_dialog_that_asked_for_it() {
        let mut app = App {
            file_info: FileInfoState {
                dl: 7,
                save_dir: "/tmp/old".into(),
                file_name: "x.zip".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = app.update(Message::FiPathPicked(7, "/tmp/new/y.zip".into()));
        assert_eq!(app.file_info.save_dir, "/tmp/new");
        assert_eq!(app.file_info.file_name, "y.zip");
    }

    /// A panel that no longer freezes the app is a panel the app can outlive:
    /// a browser capture arriving while it is up supersedes the File Info
    /// dialog, and the answer to the question the old dialog asked must not
    /// redirect the download that replaced it.
    #[test]
    fn a_save_as_answer_for_a_superseded_dialog_is_dropped() {
        let mut app = App {
            file_info: FileInfoState {
                dl: 7,
                save_dir: "/tmp/old".into(),
                file_name: "x.zip".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = app.update(Message::FiPathPicked(3, "/tmp/new/y.zip".into()));
        assert_eq!(app.file_info.save_dir, "/tmp/old");
        assert_eq!(app.file_info.file_name, "x.zip");
    }

    /// Browsing to a folder in the batch dialog is also the answer to where
    /// the batch goes: the mode has to follow the folder, or the list files
    /// itself by category anyway and the folder just chosen is ignored.
    #[test]
    fn a_browsed_batch_folder_is_where_the_batch_goes() {
        let mut app = App::default();
        app.batch.to_category = true;
        let _ = app.update(Message::BatchDirPicked("/tmp/iso".into()));
        assert_eq!(app.batch.dir, "/tmp/iso");
        assert!(app.batch.to_dir);
        assert!(!app.batch.to_category);
    }

    #[test]
    fn rejected_content_clears_metadata_and_resume_state_before_failure() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/setup.zip".into(), None, None);
        let d = app.item_mut(id).unwrap();
        d.held = vec![(0, 1024)];
        d.downloaded = 1024;
        d.size = Some(1024);
        d.disp_progress = 1.0;
        d.resume = Some(true);
        let _ = app.update(Message::Engine(engine::Event::Discarded { id }));
        let _ = app.update(Message::Engine(engine::Event::Failed {
            id,
            error: "server returned a web page".into(),
            done: 0,
            held: vec![],
            permission_denied: false,
        }));
        let d = app.item(id).unwrap();
        assert_eq!(d.state, DlState::Error);
        assert!(d.held.is_empty());
        assert_eq!(d.downloaded, 0);
        assert_eq!(d.size, None);
        assert_eq!(d.resume, None);
        assert_eq!(d.disp_progress, 0.0);
        let _ = app.adopt_probed(id, None, Some(1_000_000));
        assert_eq!(app.item(id).unwrap().size, Some(1_000_000));
    }

    /// A transfer that lost its folder shows the failure while the permission
    /// panel is up — the list is no longer frozen behind it — and picking a
    /// folder has to clear that failure along with the `.part` path, which
    /// named a file under the folder that could not be written. The bytes
    /// already on disk are not among the things that reset.
    #[test]
    fn a_re_granted_folder_clears_the_failure_and_keeps_the_bytes() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/x.iso".into(), None, None);
        if let Some(d) = app.item_mut(id) {
            d.save_dir = "/read-only".into();
            d.part_path = Some("/read-only/x.iso.part".into());
        }
        let _ = app.update(Message::Engine(engine::Event::Failed {
            id,
            error: "permission denied".into(),
            done: 0,
            held: vec![(0, 4095)],
            permission_denied: true,
        }));
        let d = app.item(id).expect("the row stays in the list");
        assert_eq!(d.state, DlState::Error);
        assert_eq!(d.held, vec![(0, 4095)]);

        app.save_dir_regranted(id, "/tmp/grantedsomewhere".into());
        let d = app.item(id).expect("the row stays in the list");
        assert_eq!(d.save_dir, "/tmp/grantedsomewhere");
        assert_eq!(d.state, DlState::Paused);
        assert_eq!(d.error, None);
        assert_eq!(d.part_path, None);
        assert_eq!(d.held, vec![(0, 4095)], "4 KiB already fetched is 4 KiB");
    }

    /// The spin arrows send their step through the same field message a
    /// typed digit takes, so the range has to be enforced where the value
    /// lands rather than beside the arrows: a held arrow, a pasted number and
    /// a hand-edited config all arrive here.
    #[test]
    fn a_queue_number_settles_inside_the_range_its_field_offers() {
        let mut app = App::default();
        app.cfg.queues = crate::model::default_queues();
        app.sch.queue = app.cfg.queues[0].name.clone();
        let files = crate::model::FILES_AT_ONCE;
        let retries = crate::model::QUEUE_RETRIES;

        let _ = app.update(Message::SchField(SchField::FilesAtOnce("99".into())));
        assert_eq!(app.cfg.queues[0].files_at_once, *files.end());
        let _ = app.update(Message::SchField(SchField::FilesAtOnce("0".into())));
        assert_eq!(app.cfg.queues[0].files_at_once, *files.start());

        // Turning retries off is the checkbox's job; a budget of zero here
        // would be a queue that retries, but never.
        let _ = app.update(Message::SchField(SchField::Retries("0".into())));
        assert_eq!(app.cfg.queues[0].schedule.retries, *retries.start());
        let _ = app.update(Message::SchField(SchField::Retries("500".into())));
        assert_eq!(app.cfg.queues[0].schedule.retries, *retries.end());
    }

    /// Save is the acknowledgement a dialog that writes through as you type
    /// otherwise never gives: the press puts the queue on disk and takes the
    /// window away. Nothing is pending on a freshly built app, so this asserts
    /// the dismissal without putting a config file anywhere.
    #[test]
    fn saving_the_scheduler_dismisses_it() {
        let mut app = App::default();
        app.windows
            .insert(iced::window::Id::unique(), WinKind::Scheduler);
        let _ = app.update(Message::SchSave);
        assert!(app.win_of(WinKind::Scheduler).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_linux_application_id() {
        std::env::remove_var("FLATPAK_ID");
        assert_eq!(super::linux_application_id(), "hydra");
        std::env::set_var("FLATPAK_ID", "io.github.ja7ad.hydra");
        assert_eq!(super::linux_application_id(), "io.github.ja7ad.hydra");
        std::env::remove_var("FLATPAK_ID");
    }

    fn properties_for(app: &App, id: DlId) -> FileInfoState {
        let d = app.item(id).expect("item");
        FileInfoState {
            dl: id,
            category: d
                .category
                .clone()
                .unwrap_or_else(|| model::DEFAULT_CATEGORY.into()),
            save_dir: d.save_dir.clone(),
            file_name: d.file_name.clone(),
            url: d.url.clone(),
            is_new: false,
            ..FileInfoState::default()
        }
    }

    /// The reported bug: Properties > OK was wired to "Download Later", so
    /// pressing OK on a running transfer stopped it and queued it. OK
    /// commits the fields and leaves the transfer exactly as it found it.
    #[test]
    fn properties_ok_leaves_a_running_download_running() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/x.iso".into(), None, None);
        app.item_mut(id).unwrap().state = DlState::Connecting;
        app.file_info = FileInfoState {
            description: "notes".into(),
            ..properties_for(&app, id)
        };

        let _ = app.update(Message::FiOk);

        let d = app.item(id).unwrap();
        assert!(
            d.state.is_active(),
            "OK stopped the transfer: {:?}",
            d.state
        );
        assert_eq!(d.description, "notes", "the fields were still committed");

        // Download Later on a new-download dialog still parks it.
        app.item_mut(id).unwrap().state = DlState::Paused;
        app.file_info = FileInfoState {
            is_new: true,
            ..properties_for(&app, id)
        };
        let _ = app.update(Message::FiDownloadLater);
        assert_eq!(app.item(id).unwrap().state, DlState::Queued);
    }

    /// Options > Save to > "Change folder for category on last selected"
    /// was stored and never read. On, a folder edited in the dialog becomes
    /// the category's; a folder left alone changes nothing.
    #[test]
    fn remember_last_dir_moves_the_category_folder_to_the_one_chosen() {
        let mut app = App::default();
        app.cfg.categories = model::default_categories();
        app.cfg.settings.remember_last_dir = true;
        let id = app.add_item("https://a.b/x.iso".into(), None, None);
        // The item's own category is the one the folder is remembered for.
        let cat = properties_for(&app, id).category;
        let dir_of = |app: &App| {
            app.cfg
                .categories
                .iter()
                .find(|c| c.name == cat)
                .map(|c| c.dir.clone())
                .expect("the item's category exists")
        };
        let before = dir_of(&app);

        app.file_info = properties_for(&app, id);
        let _ = app.update(Message::FiOk);
        assert_eq!(dir_of(&app), before, "untouched folder");

        app.file_info = FileInfoState {
            save_dir: "/tmp/elsewhere".into(),
            dir_touched: true,
            ..properties_for(&app, id)
        };
        let _ = app.update(Message::FiOk);
        assert_eq!(dir_of(&app), "/tmp/elsewhere");

        app.cfg.settings.remember_last_dir = false;
        app.file_info = FileInfoState {
            save_dir: "/tmp/third".into(),
            dir_touched: true,
            ..properties_for(&app, id)
        };
        let _ = app.update(Message::FiOk);
        assert_eq!(dir_of(&app), "/tmp/elsewhere", "off means off");
    }

    /// A draft the transfers could not act on is refused on the page that
    /// holds the problem, and the settings in force are not touched.
    #[test]
    fn options_ok_is_refused_on_the_page_that_holds_the_problem() {
        let mut app = App::default();
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Options);
        app.options.draft = app.cfg.settings.clone();
        app.options.base = app.cfg.settings.clone();
        let refused = |app: &mut App, tab: OptTab| {
            let _ = app.update(Message::OptOk);
            assert_eq!(app.options.tab, tab);
            assert!(app.options.error.is_some());
            assert!(
                app.win_of(WinKind::Options).is_some(),
                "the dialog stays up"
            );
        };

        let _ = app.update(Message::OptDraft(OptField::SpeedLimiter(true)));
        let _ = app.update(Message::OptDraft(OptField::SpeedLimitKb(String::new())));
        refused(&mut app, OptTab::SpeedLimit);
        assert!(!app.cfg.settings.speed_limiter_on);
        let _ = app.update(Message::OptDraft(OptField::SpeedLimiter(false)));

        let _ = app.update(Message::OptDraft(OptField::DlLimit(true)));
        let _ = app.update(Message::OptDraft(OptField::DlLimitMb("0".into())));
        refused(&mut app, OptTab::Quota);
        let _ = app.update(Message::OptDraft(OptField::DlLimitMb("200".into())));
        let _ = app.update(Message::OptDraft(OptField::DlLimitHours(String::new())));
        refused(&mut app, OptTab::Quota);
        let _ = app.update(Message::OptDraft(OptField::DlLimit(false)));

        let _ = app.update(Message::OptDraft(OptField::ProxyMode(ProxyMode::Script)));
        refused(&mut app, OptTab::Proxy);
        let _ = app.update(Message::OptDraft(OptField::ProxyMode(ProxyMode::Manual)));
        refused(&mut app, OptTab::Proxy);
        let _ = app.update(Message::OptDraft(OptField::ProxyHost("10.0.0.1".into())));
        let _ = app.update(Message::OptDraft(OptField::ProxyPort("99999".into())));
        refused(&mut app, OptTab::Proxy);
        let _ = app.update(Message::OptDraft(OptField::ProxyPort("1080".into())));
        assert_eq!(app.options_problem(), None);

        // Power save is a draft field like any other: the engine is only
        // told on OK, so Cancel leaves it as it was.
        let _ = app.update(Message::OptDraft(OptField::PowerSave(true)));
        assert!(!app.cfg.settings.power_save);
        let _ = app.update(Message::CloseThis(win));
        assert!(!app.cfg.settings.power_save);
    }

    /// The Options-on-completion tab's first checkbox did nothing. Both
    /// boxes write the same global setting the Options dialog does.
    #[test]
    fn the_completion_tab_toggles_the_complete_dialog() {
        let mut app = App::default();
        assert!(app.cfg.settings.show_complete_dialog);
        let _ = app.update(Message::ProgShowCompleteDialog(false));
        assert!(!app.cfg.settings.show_complete_dialog);
        assert!(app.cfg_dirty);
        let _ = app.update(Message::ProgShowCompleteDialog(true));
        assert!(app.cfg.settings.show_complete_dialog);
    }

    /// The reported bug: a queue set to start at `9:00` never fired, because
    /// the clock says `09:00`. The comparison is numeric.
    #[test]
    fn a_scheduled_minute_matches_however_it_was_typed() {
        let due = |typed: &str, now: &str| {
            let s = Schedule {
                start_enabled: true,
                start_at: typed.into(),
                once: true,
                ..Schedule::default()
            };
            schedule_start_due(&s, now, 0, false)
        };
        assert!(due("9:00", "09:00"));
        assert!(due("21:0", "21:00"));
        assert!(due("9.00", "09:00"));
        assert!(due("09:00", "09:00"));
        assert!(!due("9:00", "09:01"));
        assert!(!due("nine", "09:00"), "unreadable never fires");
    }

    /// A zero cap refused every start for good; the floor is one MB.
    #[test]
    fn a_zero_download_limit_is_read_as_one_megabyte() {
        let s = crate::model::Settings {
            dl_limit_enabled: true,
            dl_limit_mb: 0,
            ..Default::default()
        };
        assert_eq!(quota_cap(&s), Some(1024 * 1024));
    }

    /// Queues are addressed by name; a new one must never share a name with
    /// one already on the list, however the list was edited before.
    #[test]
    fn a_new_queue_never_takes_a_name_still_in_use() {
        let mut app = App::default();
        let _ = app.update(Message::SchNewQueue);
        let _ = app.update(Message::SchNewQueue);
        let _ = app.update(Message::SchNewQueue);
        let queue = |n: usize| format!("{} {n}", i18n::tr("Queue"));
        app.sch.queue = queue(2);
        let _ = app.update(Message::SchDeleteQueue);
        let _ = app.update(Message::SchNewQueue);
        let names: Vec<&str> = app.cfg.queues.iter().map(|q| q.name.as_str()).collect();
        let distinct: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(
            names.len(),
            distinct.len(),
            "a name is used twice: {names:?}"
        );
        assert_eq!(app.sch.queue, queue(2), "the smallest free number");
    }

    /// A batch sent to "one directory" with the directory blank would land
    /// in the process's working directory. OK refuses it and adds nothing.
    #[test]
    fn a_batch_to_a_blank_directory_is_refused() {
        let mut app = App::default();
        let _ = app.update(Message::BatchLoaded(Some("https://a.b/x.zip\n".into())));
        let _ = app.update(Message::BatchSaveMode(2));
        assert!(app.batch.to_dir);
        let _ = app.update(Message::BatchDir("   ".into()));
        let _ = app.update(Message::BatchOk);
        assert!(app.state.downloads.is_empty());
        assert_eq!(app.batch.checks.len(), 1, "the list is kept for the retry");

        let _ = app.update(Message::BatchDir("/tmp/batch".into()));
        let _ = app.update(Message::BatchOk);
        assert_eq!(app.state.downloads.len(), 1);
        assert_eq!(app.state.downloads[0].save_dir, "/tmp/batch");
    }

    fn pending(url: &str) -> Box<PendingAdd> {
        Box::new(PendingAdd {
            plugin_plan: None,
            url: url.into(),
            auth: None,
            capture: CaptureExtras::default(),
        })
    }

    /// The reported bug: a second question raised while one was on screen
    /// overwrote it — and with it the capture a duplicate dialog was holding.
    /// Questions wait their turn, and the capture arrives intact.
    #[test]
    fn a_confirmation_raised_over_another_waits_its_turn() {
        let mut app = App::default();
        let first = window::Id::unique();
        app.windows.insert(first, WinKind::Confirm);
        app.confirm = Some(ConfirmKind::DeleteCompleted);

        let _ = app.ask(ConfirmKind::Duplicate {
            existing: None,
            file: Some("/tmp/x.zip".into()),
            pending: pending("https://a.b/x.zip"),
        });
        assert!(matches!(app.confirm, Some(ConfirmKind::DeleteCompleted)));
        assert_eq!(app.confirm_queue.len(), 1);

        let _ = app.update(Message::CloseThis(first));
        assert!(matches!(app.confirm, Some(ConfirmKind::Duplicate { .. })));
        assert!(
            app.win_of(WinKind::Confirm).is_some(),
            "the next question opened"
        );
        assert!(app.confirm_queue.is_empty());

        let _ = app.update(Message::DupNew);
        assert_eq!(app.state.downloads.len(), 1);
        assert_eq!(app.state.downloads[0].url, "https://a.b/x.zip");
        assert!(app.confirm.is_none());
        assert!(app.win_of(WinKind::Confirm).is_none());
    }

    /// "Resume existing" over a finished entry used to start it again from
    /// zero over the finished file. A finished entry is shown, not restarted.
    #[test]
    fn resuming_a_finished_duplicate_does_not_restart_it() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/x.zip".into(), None, None);
        app.item_mut(id).unwrap().state = DlState::Complete;
        app.windows.insert(window::Id::unique(), WinKind::Confirm);
        app.confirm = Some(ConfirmKind::Duplicate {
            existing: Some(id),
            file: None,
            pending: pending("https://a.b/x.zip"),
        });
        let _ = app.update(Message::DupResume);
        assert_eq!(app.item(id).unwrap().state, DlState::Complete);
        assert_eq!(app.selected, vec![id]);
    }

    /// A site entered twice is one row retuned, not two rows of which only
    /// the first is ever read; Remove needs a row to act on.
    #[test]
    fn a_re_entered_site_login_replaces_its_row() {
        let mut app = App::default();
        let enter = |app: &mut App, site: &str, user: &str| {
            let _ = app.update(Message::OptDraft(OptField::LoginSite(site.into())));
            let _ = app.update(Message::OptDraft(OptField::LoginUser(user.into())));
            let _ = app.update(Message::OptDraft(OptField::LoginPass("pw".into())));
            let _ = app.update(Message::OptDraft(OptField::LoginAdd));
        };
        enter(&mut app, "ftp.example", "anna");
        enter(&mut app, "files.example", "bob");
        enter(&mut app, "ftp.example", "carl");
        let logins = &app.options.draft.logins;
        assert_eq!(logins.len(), 2);
        assert_eq!(logins[0].user, "carl");
        assert!(
            app.options.sel_login.is_none(),
            "nothing selected after New"
        );
        let _ = app.update(Message::OptDraft(OptField::LoginRemove));
        assert_eq!(
            app.options.draft.logins.len(),
            2,
            "no selection, no removal"
        );
    }

    /// Escape is a dialog's Cancel and Enter its default button, whichever
    /// dialog has the focus.
    #[test]
    fn escape_and_enter_drive_the_focused_dialog() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let key =
            |k: Named, win: window::Id| Message::RawKey(Key::Named(k), Modifiers::empty(), win);

        let mut app = App::default();
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Confirm);
        app.confirm = Some(ConfirmKind::UpToDate);
        let _ = app.update(key(Named::Escape, win));
        assert!(app.win_of(WinKind::Confirm).is_none());
        assert!(app.confirm.is_none());

        let id = app.add_item("https://a.b/x.zip".into(), None, None);
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Confirm);
        app.confirm = Some(ConfirmKind::DeleteItems(vec![id]));
        let _ = app.update(key(Named::Enter, win));
        assert!(app.item(id).is_none(), "Enter answered Yes");
        assert!(app.win_of(WinKind::Confirm).is_none());

        // A duplicate question has three equal answers: Enter picks none.
        let id = app.add_item("https://a.b/y.zip".into(), None, None);
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Confirm);
        app.confirm = Some(ConfirmKind::Duplicate {
            existing: Some(id),
            file: None,
            pending: pending("https://a.b/y.zip"),
        });
        let _ = app.update(key(Named::Enter, win));
        assert!(app.win_of(WinKind::Confirm).is_some());

        // The main window has no Cancel; Escape there is not a close.
        let main = window::Id::unique();
        app.windows.insert(main, WinKind::Main);
        let _ = app.update(key(Named::Escape, main));
        assert!(app.win_of(WinKind::Main).is_some());
    }

    /// A hostless extension's knock becomes a question; Enter does not
    /// answer it, No trusts nothing, and Yes admits that origin once.
    #[test]
    fn a_trust_request_is_asked_and_only_yes_trusts_the_origin() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let origin = "moz-extension://e88b5464-98e6-41c8-a457-f4887b0486e1";
        let request = || Message::Ext(crate::extbus::ExtEvent::TrustRequest(origin.into()));
        let mut app = App::default();

        let _ = app.update(request());
        assert!(matches!(&app.confirm, Some(ConfirmKind::TrustExtension(o)) if o == origin));
        let win = app
            .win_of(WinKind::Confirm)
            .expect("the question is on screen");
        let _ = app.update(Message::RawKey(
            Key::Named(Named::Enter),
            Modifiers::empty(),
            win,
        ));
        assert!(app.win_of(WinKind::Confirm).is_some(), "Enter answered");
        {
            let _question: El<'_> = crate::windows::confirm::view(&app);
        }
        let _ = app.update(Message::CloseThis(win));
        assert!(
            app.cfg.settings.allowed_extensions.is_empty(),
            "No trusted it"
        );

        for _ in 0..2 {
            let _ = app.update(request());
            let _ = app.update(Message::ConfirmYes);
        }
        assert_eq!(app.cfg.settings.allowed_extensions, [origin]);

        app.options.draft = app.cfg.settings.clone();
        app.options.apply(OptField::Untrust(0));
        app.options.apply(OptField::Untrust(0));
        assert!(app.options.draft.allowed_extensions.is_empty());
    }

    /// The Shortcuts dialog accepted anything. On close the table holds one
    /// spelling per combo, a combo no press can produce goes back to its
    /// default, and a conflict is settled for the earlier action.
    #[test]
    fn shortcuts_are_settled_when_the_dialog_closes() {
        let mut app = App::default();
        for (id, combo, _) in crate::model::SHORTCUT_ACTIONS {
            app.cfg
                .shortcuts
                .insert(id.to_string(), crate::model::platform_default(combo));
        }
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Shortcuts);
        let _ = app.update(Message::ShortcutEdit(
            "add_url".into(),
            "Shift+Cmd+N".into(),
        ));
        let _ = app.update(Message::ShortcutEdit("scheduler".into(), "e".into()));
        let _ = app.update(Message::ShortcutEdit(
            "options".into(),
            "cmd+shift+n".into(),
        ));
        let _ = app.update(Message::CloseThis(win));
        let p = crate::model::primary_modifier();
        assert_eq!(app.cfg.shortcuts["add_url"], format!("{p}+shift+n"));
        assert_eq!(
            app.cfg.shortcuts["scheduler"],
            format!("{p}+e"),
            "unusable goes back to default"
        );
        assert_eq!(
            app.cfg.shortcuts["options"],
            format!("{p}+,"),
            "the later action loses the conflict"
        );
    }

    /// A pressed combo matches the table through the same normalization,
    /// so a shortcut typed as `Shift+Cmd+V` fires before the dialog closes.
    #[test]
    fn a_pressed_combo_matches_a_differently_spelled_entry() {
        use iced::keyboard::{Key, Modifiers};
        let mut app = App::default();
        app.cfg
            .shortcuts
            .insert("select_all".into(), "Cmd+A".into());
        let id = app.add_item("https://a.b/x.zip".into(), None, None);
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Main);
        let command = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        let _ = app.update(Message::RawKey(Key::Character("a".into()), command, win));
        assert_eq!(app.selected, vec![id]);
    }

    fn exported_settings(cfg: ConfigFile) -> ConfigFile {
        let path = std::env::temp_dir().join(format!(
            "hydra-settings-{}-{:?}.hydata",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut from = App {
            cfg,
            ..App::default()
        };
        let _ = from.update(Message::ExportSettingsTo(Some(path.clone())));
        assert!(from.confirm.is_none(), "{:?}", from.confirm);
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        hydata::decode(&bytes, dirs::home_dir().as_deref()).unwrap()
    }

    #[test]
    fn exported_settings_are_adopted_by_another_install() {
        let mut theirs = model::normalize_config(ConfigFile::default());
        theirs.settings.default_conns = 12;
        theirs.settings.theme_mode = Some(ThemeMode::Dark);
        theirs.queues.retain(|q| q.builtin);
        theirs.language = Some("de".into());
        let mut app = App {
            cfg: model::normalize_config(ConfigFile::default()),
            ..App::default()
        };
        app.cfg.language = Some("fr".into());

        let (before, language) = app.adopt_config(exported_settings(theirs));
        assert_eq!(app.cfg.settings.default_conns, 12);
        assert_eq!(app.cfg.settings.theme_mode, Some(ThemeMode::Dark));
        assert!(app.cfg_dirty, "the imported settings have to reach disk");
        assert_eq!(before.language.as_deref(), Some("fr"));
        assert_eq!(language.as_deref(), Some("de"), "switched to afterwards");
        assert_eq!(app.cfg.language.as_deref(), Some("fr"), "until then");
    }

    #[test]
    fn importing_settings_refiles_downloads_off_a_missing_category() {
        let mut theirs = model::normalize_config(ConfigFile::default());
        theirs.categories.retain(|c| c.name != "Video");
        let mut app = App {
            cfg: model::normalize_config(ConfigFile::default()),
            ..App::default()
        };
        let id = app.add_item("https://a.b/x.mp4".into(), None, None);
        app.item_mut(id).unwrap().category = Some("Video".into());
        app.tree_sel = TreeSel::Cat("Video".into());

        let _ = app.adopt_config(exported_settings(theirs));
        assert_eq!(app.item(id).unwrap().category, None);
        assert_eq!(app.tree_sel, TreeSel::All);
        assert!(app.state_dirty);
    }

    #[test]
    fn a_file_that_is_not_settings_changes_nothing() {
        let path =
            std::env::temp_dir().join(format!("hydra-not-settings-{}.hydata", std::process::id()));
        std::fs::write(&path, "https://a.b/x.zip\n").unwrap();
        let mut app = App::default();
        app.cfg.settings.default_conns = 5;

        let _ = app.update(Message::ImportSettingsFrom(Some(path.clone())));
        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            app.confirm,
            Some(ConfirmKind::SettingsImportFailed(_))
        ));
        assert_eq!(app.cfg.settings.default_conns, 5);
        assert!(!app.cfg_dirty);

        let _ = app.update(Message::ImportSettingsFrom(None));
        assert_eq!(app.cfg.settings.default_conns, 5, "a cancelled picker");
    }

    #[test]
    fn a_settings_file_that_cannot_be_written_is_reported() {
        let mut app = App::default();
        let nowhere = std::env::temp_dir().join("no-such-dir").join("x.hydata");
        let _ = app.update(Message::ExportSettingsTo(Some(nowhere)));
        assert!(matches!(
            app.confirm,
            Some(ConfirmKind::SettingsExportFailed(_))
        ));
    }

    #[test]
    fn download_urls_export_from_tasks_and_settings_move_through_file() {
        let app = App::default();
        let actions = |kind| -> Vec<MenuAction> {
            crate::ui::menu::entries(kind, &app)
                .into_iter()
                .filter_map(|e| e.action)
                .collect()
        };
        let tasks = actions(MenuBarKind::Tasks);
        assert!(tasks.contains(&MenuAction::ExportUrls));
        assert!(!tasks.contains(&MenuAction::ExportSettings));
        assert!(!tasks.contains(&MenuAction::ImportSettings));
        let file = actions(MenuBarKind::File);
        assert!(file.contains(&MenuAction::ExportSettings));
        assert!(file.contains(&MenuAction::ImportSettings));
        for a in [
            MenuAction::ExportUrls,
            MenuAction::ExportSettings,
            MenuAction::ImportSettings,
        ] {
            assert_eq!(MenuAction::from_id(&a.id()), Some(a.clone()));
        }
    }

    /// A deleted row takes every window that describes it with it; a
    /// "Download complete" box left open would have dead buttons.
    #[test]
    fn a_dialog_closed_before_it_opened_is_closed_again_on_arrival() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/f.bin".into(), None, None);
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::FileInfo(id));
        let _ = app.close_file_info_windows();
        assert!(app.window_is_orphan(win));
        let _ = app.update(Message::WindowOpened(win));
        assert!(app.win_of(WinKind::FileInfo(id)).is_none());
    }

    #[test]
    fn deleting_a_download_closes_its_complete_and_preview_windows() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/x.zip".into(), None, None);
        app.item_mut(id).unwrap().state = DlState::Complete;
        app.windows
            .insert(window::Id::unique(), WinKind::Complete(id));
        app.windows
            .insert(window::Id::unique(), WinKind::ZipPreview(id));
        let _ = app.delete_item(id);
        assert!(app.item(id).is_none());
        assert!(app.windows.is_empty(), "{:?}", app.windows.values());
    }

    /// The Speed Limiter tab parsed a blank box as 10 KB/s while showing
    /// nothing, and "0" as no cap at all. The box shows the cap in force,
    /// and a box being retyped does not lift it.
    #[test]
    fn the_progress_limiter_shows_the_cap_it_applies() {
        let mut app = App::default();
        let id = app.add_item("https://a.b/x.zip".into(), None, None);
        app.prog.entry(id).or_default().limit_kb = String::new();
        let _ = app.update(Message::ProgLimitOn(id, true));
        assert_eq!(app.item(id).unwrap().speed_limit, Some(10 * 1024));
        assert_eq!(app.prog[&id].limit_kb, "10");

        let _ = app.update(Message::ProgLimitKb(id, "0".into()));
        assert_eq!(
            app.item(id).unwrap().speed_limit,
            Some(10 * 1024),
            "0 is not unlimited"
        );
        let _ = app.update(Message::ProgLimitKb(id, "250".into()));
        assert_eq!(app.item(id).unwrap().speed_limit, Some(250 * 1024));
    }

    /// While the address is still being read as a manifest, OK would add
    /// the manifest itself as a file. It waits.
    #[test]
    fn add_url_window_shrinks_when_media_controls_are_removed() {
        let mut app = App::default();
        let plain = app.window_size(WinKind::AddUrl);
        assert_eq!(plain, (760.0, 156.0));
        app.add_url.plugin_probing = true;
        let probing = app.window_size(WinKind::AddUrl);
        assert!(probing.1 > plain.1);
        app.add_url.plugin_probing = false;
        app.add_url.plugin_plan = Some(crate::plugins::tests::plan());
        let video = app.window_size(WinKind::AddUrl);
        assert!(video.1 > probing.1 && video.1 < 400.0);
        let _ = app.update(Message::PluginAudioOnly(true));
        let audio = app.window_size(WinKind::AddUrl);
        assert!(audio.1 < video.1);
        app.add_url
            .plugin_plan
            .as_mut()
            .unwrap()
            .plan
            .tracks
            .clear();
        let no_tracks = app.window_size(WinKind::AddUrl);
        assert!(no_tracks.1 < audio.1);
        app.add_url.plugin_plan = None;
        assert_eq!(app.window_size(WinKind::AddUrl), plain);
    }

    #[test]
    fn add_url_ok_waits_for_a_running_probe() {
        let mut app = App::default();
        app.add_url.address = "https://a.b/live.m3u8".into();
        app.add_url.stream_probing = true;
        let _ = app.update(Message::AddUrlOk);
        assert!(app.state.downloads.is_empty());
        app.add_url.stream_probing = false;
        app.add_url.metalink_probing = true;
        let _ = app.update(Message::AddUrlOk);
        assert!(app.state.downloads.is_empty());
    }

    #[test]
    fn opening_a_plugin_package_reviews_permissions_before_installing() {
        let mut app = App::default();
        let path = std::path::absolute("youtube.hyaplugin").unwrap();
        let _ = app.update(Message::InstallPluginFile(path.clone()));
        assert_eq!(app.options.tab, OptTab::Plugins);
        assert_eq!(app.options.plugins.path, path.to_string_lossy());
        assert!(app.options.plugins.busy);
        assert!(app.options.plugins.review.is_none());
        assert!(app.options.plugins.installed.is_empty());
        let queued = std::path::absolute("another.hyaplugin").unwrap();
        let _ = app.update(Message::InstallPluginFile(queued.clone()));
        assert_eq!(
            app.options.plugins.pending_source,
            Some(queued.to_string_lossy().into_owned())
        );
        assert_eq!(app.options.plugins.path, path.to_string_lossy());
        let _ = crate::plugins::update(
            &mut app,
            crate::plugins::Message::Reviewed(Box::new(Err("old package error".into()))),
        );
        assert!(app.options.plugins.pending_source.is_none());
        assert!(app.options.plugins.error.is_none());
        assert!(!app.options.plugins.busy);
    }

    #[test]
    fn browser_plugin_sources_open_review_and_queue_while_busy() {
        let mut app = App::default();
        let source = "https://example.com/youtube.hyaplugin".to_string();
        let _ = app.update(Message::InstallPluginSource(source.clone()));
        assert_eq!(app.options.tab, OptTab::Plugins);
        assert_eq!(app.options.plugins.path, source);
        assert!(app.options.plugins.busy);
        assert!(app.options.plugins.prepared.is_none());
        assert!(app.options.plugins.installed.is_empty());
        let next = "https://example.com/another.hyaplugin".to_string();
        let _ = app.update(Message::InstallPluginSource(next.clone()));
        assert_eq!(app.options.plugins.pending_source, Some(next));
        assert_eq!(app.options.plugins.path, source);
    }

    #[test]
    fn playlist_selection_queues_selected_entries_once_with_audio_preferences() {
        let mut app = App::default();
        app.cfg.queues = crate::model::default_queues();
        let mut info = crate::plugins::tests::plan();
        info.plan.tracks.clear();
        info.plan.entries = (1..=2)
            .map(|index| hya_plugin_api::PlaylistEntry {
                id: index.to_string(),
                url: format!("https://example.com/video{index}"),
                title: Some(format!("Video {index}")),
            })
            .collect();
        app.add_url.address = "https://example.com/playlist".into();
        app.add_url.plugin_of = app.add_url.address.clone();
        app.add_url.plugin_plan = Some(info.clone());
        let _ = app.update(Message::PluginAudioOnly(true));
        let _ = app.update(Message::PluginAudioFormat("mp3".into()));
        let _ = app.update(Message::PluginPlaylistEntry("2".into(), false));
        let _ = app.update(Message::AddUrlOk);
        assert_eq!(app.state.downloads.len(), 1);
        let item = &app.state.downloads[0];
        assert_eq!(item.url, "https://example.com/video1");
        let plan = item.plugin_plan.as_ref().unwrap();
        assert!(plan.preferences.audio_only);
        assert_eq!(plan.preferences.audio_format.as_deref(), Some("mp3"));
        assert!(plan.preferences.playlist_ids.is_none());
        assert!(item.queue.is_some());
        assert_eq!(item.state, DlState::Queued);
        assert!(!app.cfg.queues[0].running);
        app.add_url.address = "https://example.com/playlist".into();
        app.add_url.plugin_of = app.add_url.address.clone();
        app.add_url.plugin_plan = Some(info);
        let _ = app.update(Message::PluginPlaylistEntry("1".into(), false));
        let _ = app.update(Message::PluginPlaylistEntry("2".into(), false));
        let _ = app.update(Message::AddUrlOk);
        assert!(app.add_url.error.is_some());
        let _ = app.update(Message::PluginPlaylistEntry("1".into(), true));
        let _ = app.update(Message::AddUrlOk);
        assert_eq!(app.state.downloads.len(), 1);
    }

    #[test]
    fn address_loading_animates_only_during_inspection() {
        let mut app = App::default();
        assert!(!app.address_loading());
        let _ = app.update(Message::AnimTick);
        assert_eq!(app.add_url.loading_frame, 0);
        for kind in 0..4 {
            app.add_url = AddUrlState::default();
            match kind {
                0 => app.add_url.plugin_probing = true,
                1 => app.add_url.stream_probing = true,
                2 => app.add_url.metalink_probing = true,
                _ => app.add_url.cookies_importing = true,
            }
            assert!(app.address_loading());
            app.add_url.loading_frame = 11;
            let _ = app.update(Message::AnimTick);
            assert_eq!(app.add_url.loading_frame, 0);
            let _ = app.update(Message::AnimTick);
            assert_eq!(app.add_url.loading_frame, 1);
        }
        app.add_url = AddUrlState::default();
        assert!(!app.address_loading());
    }

    #[test]
    fn adding_playlist_preserves_queue_state_and_existing_items() {
        for running in [false, true] {
            let mut app = App::default();
            app.cfg.queues = crate::model::default_queues();
            app.cfg.queues[0].running = running;
            let existing = app.add_item("https://example.com/existing.zip".into(), None, None);
            let mut info = crate::plugins::tests::plan();
            info.plan.tracks.clear();
            info.plan.entries = (1..=2)
                .map(|index| hya_plugin_api::PlaylistEntry {
                    id: index.to_string(),
                    url: format!("https://example.com/video{index}"),
                    title: Some(format!("Video {index}")),
                })
                .collect();
            let _ = app.add_plugin_playlist(info.clone());
            assert_eq!(app.cfg.queues[0].running, running);
            assert_eq!(app.item(existing).unwrap().state, DlState::Paused);
            assert_eq!(app.state.downloads.len(), 3);
            assert!(app.state.downloads[1..]
                .iter()
                .all(|item| item.state == DlState::Queued));
            assert!(app.state.downloads[1].q_order < app.state.downloads[2].q_order);
            let _ = app.add_plugin_playlist(info);
            assert_eq!(app.state.downloads.len(), 3);
            assert_eq!(app.cfg.queues[0].running, running);
            if !running {
                let _ = app.queue_tick();
                assert!(app.state.downloads[1..]
                    .iter()
                    .all(|item| item.state == DlState::Queued));
            }
        }
    }

    fn imported_cookies(header: &str, source: &str) -> crate::engine::ImportedCookies {
        let mut jar = hya_net::CookieJar::new();
        jar.add_pairs(header, "www.youtube.com");
        crate::engine::ImportedCookies {
            header: header.into(),
            source: source.into(),
            jar,
        }
    }

    #[test]
    fn plugin_inspection_waits_for_chrome_cookie_import() {
        let mut app = App::default();
        app.cfg.settings.cookies_from_browser = "chrome".into();
        let url = "https://www.youtube.com/watch?v=example".to_string();
        let _ = app.update(Message::AddrChanged(url.clone()));
        assert!(app.add_url.cookies_importing);
        assert!(!app.add_url.plugin_probing);
        let _ = app.update(Message::AddrCookiesImported(Box::new(Ok(
            imported_cookies("session=chrome", "Chrome"),
        ))));
        assert!(app.add_url.plugin_probing);
        assert!(!app.add_url.plugin_probe_stale);
        assert_eq!(
            app.add_url.capture.cookies.as_deref(),
            Some("session=chrome")
        );
        assert!(app.add_url.browser_cookies.is_some());
        let _ = app.update(Message::PluginProbed(url, Box::new(Ok(None))));
        assert!(!app.add_url.plugin_probing);
    }

    #[test]
    fn cookie_edits_discard_inspection_replies_from_the_previous_session() {
        for cookies in ["session=new", ""] {
            let mut app = App::default();
            let url = "https://www.youtube.com/watch?v=example".to_string();
            app.add_url.capture.cookies = Some("session=old".into());
            let _ = app.update(Message::AddrChanged(url.clone()));
            let _ = app.update(Message::AddrCookies(cookies.into()));
            assert!(app.add_url.browser_cookies.is_none());
            assert!(app.add_url.plugin_probe_stale);
            let _ = app.update(Message::PluginProbed(
                url.clone(),
                Box::new(Err("Sign in to confirm you're not a bot".into())),
            ));
            assert!(app.add_url.error.is_none());
            assert!(app.add_url.plugin_probing);
            assert!(!app.add_url.plugin_probe_stale);
            let _ = app.update(Message::PluginProbed(
                url,
                Box::new(Ok(Some(crate::plugins::tests::plan()))),
            ));
            assert!(app.add_url.plugin_plan.is_some());
        }
    }

    #[test]
    fn typing_cookies_retries_a_failed_inspection_but_identical_cookies_do_not() {
        let mut app = App::default();
        let url = "https://www.youtube.com/watch?v=example".to_string();
        let _ = app.update(Message::AddrChanged(url.clone()));
        let _ = app.update(Message::PluginProbed(
            url.clone(),
            Box::new(Err("Sign in to confirm you're not a bot".into())),
        ));
        let _ = app.update(Message::AddrCookies("session=chrome".into()));
        assert!(app.add_url.plugin_probing);
        assert!(app.add_url.error.is_none());
        let _ = app.update(Message::PluginProbed(
            url,
            Box::new(Ok(Some(crate::plugins::tests::plan()))),
        ));
        let _ = app.update(Message::AddrCookies("session=chrome".into()));
        assert!(!app.add_url.plugin_probing);
        assert!(app.add_url.plugin_plan.is_some());
    }

    #[test]
    fn failed_cookie_import_still_allows_public_video_inspection() {
        for result in [
            Ok(imported_cookies("", "No cookies")),
            Err("Browser unavailable".into()),
        ] {
            let mut app = App::default();
            app.cfg.settings.cookies_from_browser = "chrome".into();
            let _ = app.update(Message::AddrChanged(
                "https://www.youtube.com/watch?v=example".into(),
            ));
            let _ = app.update(Message::AddrCookiesImported(Box::new(result)));
            assert!(!app.add_url.cookies_importing);
            assert!(app.add_url.plugin_probing);
            assert!(app.add_url.capture.cookies.is_none());
            assert!(app.add_url.cookie_note.is_some());
        }
    }

    #[test]
    fn an_inspection_reply_during_cookie_import_cannot_publish_a_stale_failure() {
        let mut app = App::default();
        let url = "https://www.youtube.com/watch?v=example".to_string();
        let _ = app.update(Message::AddrChanged(url.clone()));
        app.cfg.settings.cookies_from_browser = "chrome".into();
        let _ = app.update(Message::AddrImportCookies);
        let _ = app.update(Message::PluginProbed(
            url.clone(),
            Box::new(Err("Sign in to confirm you're not a bot".into())),
        ));
        assert!(app.add_url.cookies_importing);
        assert!(!app.add_url.plugin_probing);
        assert!(app.add_url.error.is_none());
        let _ = app.update(Message::AddrCookiesImported(Box::new(Ok(
            imported_cookies("session=chrome", "Chrome"),
        ))));
        assert!(app.add_url.plugin_probing);
        let _ = app.update(Message::PluginProbed(url, Box::new(Ok(None))));
        assert!(app.add_url.error.is_none());
    }

    #[test]
    fn entering_or_prefilling_a_url_starts_plugin_detection() {
        for message in [
            Message::AddrChanged("https://example.com/video.zip".into()),
            Message::AddrPrefill(Some("https://example.com/video.zip".into())),
        ] {
            let mut app = App::default();
            let _ = app.update(message);
            assert!(app.add_url.plugin_probing);
            assert_eq!(app.add_url.address, "https://example.com/video.zip");
            let _ = app.update(Message::AddUrlOk);
            assert!(app.state.downloads.is_empty());
            let _ = app.update(Message::PluginProbed(
                app.add_url.address.clone(),
                Box::new(Ok(None)),
            ));
            assert!(!app.add_url.plugin_probing);
            assert!(app.add_url.plugin_plan.is_none());
            assert_eq!(app.add_url.plugin_of, app.add_url.address);
        }
    }

    #[test]
    fn plugin_detection_discards_stale_results_and_handles_cleared_addresses() {
        let mut app = App::default();
        let first = "https://example.com/first".to_string();
        let second = "https://example.com/second".to_string();
        let _ = app.update(Message::AddrChanged(first.clone()));
        let _ = app.update(Message::AddrChanged(second.clone()));
        let _ = app.update(Message::PluginProbed(
            first,
            Box::new(Ok(Some(crate::plugins::tests::plan()))),
        ));
        assert!(app.add_url.plugin_probing);
        assert!(app.add_url.plugin_plan.is_none());
        let _ = app.update(Message::PluginProbed(
            second.clone(),
            Box::new(Ok(Some(crate::plugins::tests::plan()))),
        ));
        assert!(!app.add_url.plugin_probing);
        assert!(app.add_url.plugin_plan.is_some());
        let _ = app.update(Message::AddrChanged("https://example.com/third".into()));
        let _ = app.update(Message::AddrChanged(String::new()));
        let _ = app.update(Message::PluginProbed(
            "https://example.com/third".into(),
            Box::new(Err("stale failure".into())),
        ));
        assert!(!app.add_url.plugin_probing);
        assert!(app.add_url.plugin_plan.is_none());
        assert!(app.add_url.error.is_none());
    }

    #[test]
    fn clearing_and_reentering_a_url_repeats_detection() {
        let mut app = App::default();
        let url = "https://example.com/video".to_string();
        let _ = app.update(Message::AddrChanged(url.clone()));
        let _ = app.update(Message::PluginProbed(
            url.clone(),
            Box::new(Ok(Some(crate::plugins::tests::plan()))),
        ));
        let _ = app.update(Message::AddrChanged(String::new()));
        assert!(app.add_url.plugin_plan.is_none());
        assert!(app.add_url.plugin_of.is_empty());
        let _ = app.update(Message::AddrChanged(url));
        assert!(app.add_url.plugin_probing);
    }

    #[test]
    fn unclaimed_stream_and_metalink_urls_use_core_detection() {
        for (url, stream) in [
            ("https://example.com/video.m3u8", true),
            ("https://example.com/files.meta4", false),
        ] {
            let mut app = App::default();
            let _ = app.update(Message::AddrChanged(url.into()));
            let _ = app.update(Message::PluginProbed(url.into(), Box::new(Ok(None))));
            assert!(!app.add_url.plugin_probing);
            assert_eq!(app.add_url.stream_probing, stream);
            assert_eq!(app.add_url.metalink_probing, !stream);
            app.add_url.stream_probing = false;
            app.add_url.metalink_probing = false;
            app.add_url.stream_of = url.into();
            app.add_url.metalink_of = url.into();
            let _ = app.update(Message::AddrChanged(String::new()));
            let _ = app.update(Message::AddrChanged(url.into()));
            let _ = app.update(Message::PluginProbed(url.into(), Box::new(Ok(None))));
            assert_eq!(app.add_url.stream_probing, stream);
            assert_eq!(app.add_url.metalink_probing, !stream);
        }
    }

    /// The browser was told `ok: false` when the receipt timed out, so it
    /// kept its own download. A capture that arrives after that must not be
    /// kept here as well, or the file downloads twice.
    #[test]
    fn a_capture_the_browser_kept_is_not_kept_here_too() {
        let mut app = App::default();
        let (ack, receipt) = crate::extbus::Ack::pair();
        drop(receipt);
        let dl = crate::extbus::ExtDownload {
            url: "https://a.b/late.zip".into(),
            ..Default::default()
        };
        let _ = app.update(Message::Ext(crate::extbus::ExtEvent::Download(dl, ack)));
        // The capture auto-started behind its dialog, so the row waits for
        // the engine's stop before it leaves the list — but it is leaving.
        for d in &app.state.downloads {
            assert!(
                app.pending_delete.iter().any(|(id, _)| *id == d.id),
                "kept: {d:?}"
            );
        }
        assert!(app
            .windows
            .values()
            .all(|k| !matches!(k, WinKind::FileInfo(_))));

        let (ack, receipt) = crate::extbus::Ack::pair();
        let waiting = std::thread::spawn(move || receipt.recv().is_ok());
        let dl = crate::extbus::ExtDownload {
            url: "https://a.b/ontime.zip".into(),
            ..Default::default()
        };
        let _ = app.update(Message::Ext(crate::extbus::ExtEvent::Download(dl, ack)));
        assert!(waiting.join().unwrap());
        assert!(app
            .state
            .downloads
            .iter()
            .any(|d| d.url == "https://a.b/ontime.zip"
                && !app.pending_delete.iter().any(|(id, _)| *id == d.id)));
    }

    fn offer(has_bundle: bool) -> crate::update::UpdateInfo {
        crate::update::UpdateInfo {
            version: "9.9.9".into(),
            notes: String::new(),
            html_url: "https://example.invalid/release".into(),
            asset_name: "hydra.tar.gz".into(),
            asset_url: "https://example.invalid/hydra.tar.gz".into(),
            size: 1,
            sums_url: None,
            in_place: true,
            needs_auth: false,
            package: None,
            has_bundle,
            package_hint: None,
        }
    }

    /// Closing the dialog during Verifying/Preparing looked like a cancel,
    /// but the run's ReadyToRestart still quit the app and applied the
    /// update. A cancelled run's events are from a generation nobody is
    /// listening to any more.
    #[test]
    fn a_cancelled_update_cannot_restart_the_app() {
        let mut app = App::default();
        let win = window::Id::unique();
        app.windows.insert(win, WinKind::Update);
        app.updater.info = Some(offer(true));
        let _ = app.update(Message::UpdateNow);
        let started = app.updater.generation;
        assert!(matches!(app.updater.phase, UpdatePhase::Downloading { .. }));

        app.updater.phase = UpdatePhase::Verifying;
        let _ = app.update(Message::UpdateCancel);
        assert!(app.win_of(WinKind::Update).is_none());
        assert_eq!(app.updater.phase, UpdatePhase::Idle);

        let _ = app.update(Message::UpdateEvent(
            started,
            crate::update::UpdateEvent::ReadyToRestart,
        ));
        assert_ne!(app.updater.phase, UpdatePhase::Restarting);

        // The OS close button during Preparing is the same cancel.
        app.windows.insert(win, WinKind::Update);
        app.updater.info = Some(offer(true));
        let _ = app.update(Message::UpdateNow);
        let started = app.updater.generation;
        app.updater.phase = UpdatePhase::Preparing;
        let _ = app.update(Message::WindowClosed(win));
        let _ = app.update(Message::UpdateEvent(
            started,
            crate::update::UpdateEvent::ReadyToRestart,
        ));
        assert_ne!(app.updater.phase, UpdatePhase::Restarting);
    }

    /// A newer release with no build for this machine is still news: the
    /// dialog opens, names the version, and offers nothing to download.
    #[test]
    fn a_release_without_a_bundle_is_announced_but_not_downloadable() {
        let mut app = App::default();
        let _ = app.update(Message::UpdateChecked(Ok(Some(offer(false)))));
        let win = app.win_of(WinKind::Update).expect("the dialog opened");
        let _ = app.update(Message::UpdateNow);
        assert_eq!(app.updater.phase, UpdatePhase::Idle, "nothing to download");
        assert!(dialog_primary(&app, WinKind::Update, win).is_none());
        assert!(matches!(
            dialog_cancel(&app, WinKind::Update, win),
            Some(Message::UpdateCancel)
        ));
    }

    /// The Scheduler keeps its queue by name. A name renamed from the
    /// sidebar, or deleted, must not leave the window's buttons acting on
    /// nothing.
    #[test]
    fn the_scheduler_follows_a_renamed_or_deleted_queue() {
        let mut app = App::default();
        app.cfg.queues = model::default_queues();
        app.sch.queue = "no such queue".into();
        let _ = app.update(Message::SchField(SchField::FilesAtOnce("7".into())));
        assert_eq!(app.sch.queue, app.cfg.queues[0].name);
        assert_eq!(app.cfg.queues[0].files_at_once, 7);
    }

    #[test]
    // Renaming rebuilds the native menu and the tray, which muda builds on
    // the main thread only — a test thread is not it.
    #[cfg_attr(target_os = "macos", ignore = "muda needs the main thread")]
    fn renaming_a_queue_from_the_sidebar_renames_it_in_the_scheduler() {
        let mut app = App::default();
        let _ = app.update(Message::SchNewQueue);
        let old = app.sch.queue.clone();
        assert!(app.rename_queue(&old, "Night"));
        assert_eq!(app.sch.queue, "Night");
    }

    #[test]
    fn sort_by_order_of_addition_sorts_by_added_and_id() {
        let mut app = App::default();
        let mut d1 = item(1, "/d", "a.zip", None, DlState::Paused);
        d1.added = 100;
        let mut d2 = item(2, "/d", "b.zip", None, DlState::Paused);
        d2.added = 200;
        let mut d3 = item(3, "/d", "c.zip", None, DlState::Paused);
        d3.added = 200;
        let mut d4 = item(4, "/d", "d.zip", None, DlState::Paused);
        d4.added = 300;
        app.state.downloads = vec![d3, d1, d4, d2];

        app.sort = (SortKey::OrderOfAddition, true);
        assert_eq!(app.visible_ids(), vec![1, 2, 3, 4]);

        app.sort = (SortKey::OrderOfAddition, false);
        assert_eq!(app.visible_ids(), vec![4, 3, 2, 1]);
    }

    #[test]
    fn sort_by_last_try_date_handles_untried_and_reverses_cleanly() {
        let mut app = App::default();
        let mut d1 = item(1, "/d", "a.zip", None, DlState::Paused);
        d1.added = 100;
        d1.last_try = None;

        let mut d2 = item(2, "/d", "b.zip", None, DlState::Paused);
        d2.added = 150;
        d2.last_try = Some(300);

        let mut d3 = item(3, "/d", "c.zip", None, DlState::Paused);
        d3.added = 200;
        d3.last_try = None;

        let mut d4 = item(4, "/d", "d.zip", None, DlState::Paused);
        d4.added = 250;
        d4.last_try = Some(260);

        app.state.downloads = vec![d3, d1, d4, d2];

        // Descending (Z-A / newest first):
        // d2 (300) > d4 (260) > d3 (fallback added 200) > d1 (fallback added 100)
        app.sort = (SortKey::Column(Column::LastTry), false);
        assert_eq!(app.visible_ids(), vec![2, 4, 3, 1]);

        // Ascending (A-Z / oldest first):
        app.sort = (SortKey::Column(Column::LastTry), true);
        assert_eq!(app.visible_ids(), vec![1, 3, 4, 2]);
    }

    #[test]
    fn sort_key_and_menu_action_id_round_trip() {
        assert_eq!(
            SortKey::from_id(SortKey::OrderOfAddition.id()),
            Some(SortKey::OrderOfAddition)
        );
        assert_eq!(SortKey::from_id("Addition"), Some(SortKey::OrderOfAddition));
        assert_eq!(
            MenuAction::from_id(&MenuAction::ArrangeBy(SortKey::OrderOfAddition).id()),
            Some(MenuAction::ArrangeBy(SortKey::OrderOfAddition))
        );
        assert_eq!(
            MenuAction::from_id(&MenuAction::SortDirection(true).id()),
            Some(MenuAction::SortDirection(true))
        );
        assert_eq!(
            MenuAction::from_id(&MenuAction::SortDirection(false).id()),
            Some(MenuAction::SortDirection(false))
        );
    }
}

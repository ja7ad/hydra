// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent data model: downloads, categories, queues, settings.
//!
//! Everything here serializes to one JSON file in the platform config dir, so
//! the download list and options survive restarts. Live per-connection
//! state is `#[serde(skip)]` — a restart resumes from
//! the received spans (`held`), not from socket state.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type DlId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DlState {
    /// In a queue, waiting for a worker slot.
    Queued,
    /// Probing / sending the first requests.
    Connecting,
    /// Bytes are arriving.
    Receiving,
    Paused,
    Complete,
    Error,
}

impl DlState {
    pub fn is_active(self) -> bool {
        matches!(self, DlState::Connecting | DlState::Receiving)
    }
}

/// One row of the per-connection table in the progress dialog.
#[derive(Clone, Debug, Default)]
pub struct ConnRow {
    pub downloaded: u64,
    pub info: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadItem {
    pub id: DlId,
    pub url: String,
    pub file_name: String,
    /// Directory the finished file is moved to.
    pub save_dir: String,
    pub category: Option<String>,
    pub description: String,
    pub size: Option<u64>,
    pub downloaded: u64,
    pub state: DlState,
    pub error: Option<String>,
    /// Server honours byte ranges (the "Resume capability" line).
    pub resume: Option<bool>,
    pub added: i64,
    pub last_try: Option<i64>,
    /// Queue name when scheduled, `None` for direct downloads.
    pub queue: Option<String>,
    pub q_order: u32,
    /// HTTP basic credentials from "Use authorization".
    pub auth: Option<(String, String)>,
    /// Cookie header value for sites that gate downloads behind a session
    /// (filled manually, by the browser extension, or imported from a
    /// browser's own store).
    #[serde(default)]
    pub cookies: Option<String>,
    /// Where `cookies` came from, for Properties to show.
    ///
    /// A cookie is a bearer credential, and a user looking at a download that
    /// works when nothing else does is entitled to know whether it is carrying
    /// something they typed, something the extension captured, or something
    /// read out of their browser's profile. The DESCRIPTION is stored, never a
    /// second copy of the value.
    #[serde(default)]
    pub cookie_source: Option<String>,
    /// `Referer:` header value for a hotlink-protected origin: the page the
    /// browser was on when the extension captured this file. Sites that gate
    /// their CDN on it answer `403` to a request without it, however good the
    /// cookies are, so it travels with the item and is replayed on every
    /// start — the same way `StreamInfo::referer` already works for a
    /// manifest's segments.
    #[serde(default)]
    pub referer: Option<String>,
    /// Per-download cap, bytes/sec, when the Speed Limiter tab enables one.
    pub speed_limit: Option<u64>,
    /// Parked by the Connection tab's download limit, not by the user. The
    /// quota tick resumes exactly these when the window rolls over or the
    /// cap is raised; a hand-paused item carries `false` and stays put.
    #[serde(default)]
    pub limit_paused: bool,
    /// Byte spans confirmed on disk; drives resume and the chunk strip.
    pub held: Vec<(u64, u64)>,
    /// The `.part` staging file, pinned when the transfer first starts so a
    /// later rename in File Info cannot orphan the bytes being written.
    #[serde(default)]
    pub part_path: Option<String>,
    #[serde(skip)]
    pub rate: f64,
    /// Scheduler retries consumed in this session.
    #[serde(skip)]
    pub retries: u32,
    /// Animated progress fraction the widgets draw; glides toward
    /// `progress()` on the animation tick so the bar moves continuously
    /// between engine updates.
    #[serde(skip)]
    pub disp_progress: f32,
    #[serde(skip)]
    pub eta_secs: Option<u64>,
    /// Seconds of MEDIA captured, for a live recording. A live stream has no
    /// size, so this is what the progress dialog shows in place of one.
    #[serde(skip)]
    pub recorded_secs: Option<f64>,
    #[serde(skip)]
    pub conns: Vec<ConnRow>,
    #[serde(skip)]
    pub plugin_details: Vec<Vec<String>>,
    #[serde(skip)]
    pub status_line: String,
    /// Shut down, log off or sleep the computer once this download (and any
    /// virus scan) finishes — the per-download analogue of a queue's
    /// [`Schedule::shutdown_when_done`]. The action itself only runs after
    /// the cancellable countdown in `WinKind::Power`.
    #[serde(default)]
    pub shutdown_after: bool,
    #[serde(default)]
    pub shutdown_action: PowerAction,
    /// Set when `url` names an adaptive-stream MANIFEST rather than a file.
    /// Its presence is what routes the item to the stream-aware engine path
    /// instead of the range scheduler.
    #[serde(default)]
    pub stream: Option<StreamInfo>,
    #[serde(default)]
    pub plugin_plan: Option<crate::plugins::PlanInfo>,
    /// Set when this item came from a Metalink document. Its presence is what
    /// gives the transfer a mirror list, a size it can trust, a digest, and —
    /// where the document published `<pieces>` — per-chunk verification with
    /// targeted refetch instead of starting over.
    #[serde(default)]
    pub metalink: Option<MetalinkInfo>,
    /// The user named this file themselves — in the File Info / Properties
    /// dialog, or by taking the renamed copy the duplicate dialog offered.
    ///
    /// A probe answers AFTER the transfer it belongs to has started, so
    /// without this the `Content-Disposition` name arrived second and won:
    /// the edited name was written to the item, the download started, and
    /// the first `Probed` event put the server's name back and re-aimed the
    /// engine's final path at it. Every rename made from the dialog was lost
    /// that way, and the duplicate dialog's "Download as new file" copy was
    /// renamed back over the file it had just warned about.
    #[serde(default)]
    pub name_locked: bool,
    /// Which proxy this download takes: the app default, none, or its own.
    /// Editable in File Info and, while the transfer is stopped, in the
    /// progress dialog.
    #[serde(default)]
    pub proxy: ProxyChoice,
}

/// What a stream item needs beyond a URL: which rendition was chosen, and
/// what container the user asked the finished file to be. Persisted so a
/// restarted stream picks the same rendition instead of silently re-choosing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StreamInfo {
    /// "hls" or "dash".
    pub protocol: String,
    /// The variant playlist chosen in the browser, when one was.
    #[serde(default)]
    pub variant_url: Option<String>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub bandwidth: Option<u64>,
    /// "MP4" or "TS".
    #[serde(default)]
    pub container: String,
    /// Page the stream played on; sent as `Referer` with every segment.
    #[serde(default)]
    pub referer: Option<String>,
    /// The BROWSER's User-Agent, not Hydra's. Origins that gate on it hand
    /// a different playlist — or none — to anything else.
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub live: bool,
    /// Stop a live recording after this many seconds and finish the file.
    #[serde(default)]
    pub max_seconds: Option<u64>,
}

impl StreamInfo {
    /// The extension the finished file is named with, from the container
    /// asked for. The engine corrects it once the playlist has said what the
    /// segments are — a packed audio stream is neither `mp4` nor `ts`.
    pub fn ext(&self) -> &'static str {
        container_ext(&self.container)
    }
}

/// `ts` or `mp4`, whichever way the container was spelled.
pub fn container_ext(container: &str) -> &'static str {
    if container.eq_ignore_ascii_case("ts") {
        "ts"
    } else {
        "mp4"
    }
}

/// One mirror from a Metalink document, as the engine will use it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MirrorRef {
    pub url: String,
    /// Rank after the document's ordering has been normalised: 1 is best,
    /// always in the RFC 5854 direction whichever dialect it came from.
    ///
    /// Metalink 3.0 states preference on a scale that runs the OTHER way
    /// (0-100, higher better). Storing the document's own number would mean
    /// every consumer had to remember which dialect it came from, and the one
    /// that forgot would give most of the work to the mirror the publisher
    /// ranked last — a transfer that still succeeds, just slower, and
    /// indistinguishably from bad luck.
    #[serde(default)]
    pub priority: u32,
    /// A ceiling the MIRROR stated for itself. Narrows the user's per-host
    /// setting; never widens it.
    #[serde(default)]
    pub max_connections: Option<usize>,
}

/// What a Metalink document said about one file.
///
/// Persisted with the item for the same reason `held` is: a download resumed
/// after a restart that had lost its mirror list would fall back to one source
/// and lose its reserve bench — a difference invisible until the mirror that
/// failed before fails again.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MetalinkInfo {
    /// Every mirror this build can fetch from, best first.
    #[serde(default)]
    pub mirrors: Vec<MirrorRef>,
    /// The size the document stated.
    ///
    /// This is what admits a second mirror at all. Agreement between mirrors is
    /// otherwise established on a strong validator, and independent mirror
    /// operators running independent web servers cannot share an `ETag` — so
    /// without a stated size a nineteen-mirror list downloads from one host.
    #[serde(default)]
    pub size: Option<u64>,
    /// The strongest digest the document published, as `algorithm:hex`.
    #[serde(default)]
    pub digest: Option<String>,
    /// The document's `<pieces>` as a chunk manifest, in its on-disk JSON form.
    ///
    /// Stored as text rather than a parsed structure so the GUI's state file
    /// stays independent of `hya-net`'s manifest type: this file is written by
    /// one version of Hydra and read by the next.
    #[serde(default)]
    pub pieces: Option<String>,
    /// Where the document came from, for the file-info dialog.
    #[serde(default)]
    pub origin: String,
    /// The document carried an OpenPGP `<signature>` over this file.
    ///
    /// Recorded and NOT verified. Saying nothing would let a user assume there
    /// was nothing to check; reporting it as verified would be worse.
    #[serde(default)]
    pub signed: bool,
}

/// How far a live recording has got, as a fraction.
///
/// Only a recording with a time limit has one: seconds captured against
/// seconds asked for. Without a limit there is no end to be a fraction of,
/// and bytes cannot stand in — a live stream has no size.
pub fn live_progress(max_seconds: Option<u64>, recorded: Option<f64>) -> f32 {
    match (max_seconds, recorded) {
        (Some(limit), Some(done)) if limit > 0 => (done / limit as f64).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    }
}

impl DownloadItem {
    /// Name the file, made portable first: every name in the list is one
    /// Windows, macOS and Linux can all create, whoever supplied it. A name
    /// with nothing writable left in it is ignored. Returns whether the
    /// stored name changed.
    pub fn set_file_name(&mut self, name: &str) -> bool {
        let Some(name) = hya_net::filename::portable(name) else {
            return false;
        };
        let changed = self.file_name != name;
        self.file_name = name;
        changed
    }

    pub fn full_path(&self) -> PathBuf {
        PathBuf::from(&self.save_dir).join(&self.file_name)
    }

    /// The `.part` staging file: next to the destination, so completion is a
    /// same-filesystem rename. hydra writes bytes at their final offsets — no
    /// assembly pass exists, so a separate temp directory would only add a
    /// cross-drive copy at the end.
    pub fn part_file(&self) -> PathBuf {
        match &self.part_path {
            Some(p) => PathBuf::from(p),
            None => PathBuf::from(&self.save_dir).join(format!("{}.part", self.file_name)),
        }
    }

    pub fn progress(&self) -> f32 {
        // A live recording has no size, so bytes can say nothing about how
        // far along it is — but a recording with a time limit does have a
        // real fraction, and it is the one the person set. Seconds captured
        // against seconds asked for is the honest bar for that case.
        if let Some(si) = self.stream.as_ref().filter(|s| s.live) {
            return live_progress(si.max_seconds, self.recorded_secs);
        }
        match self.size {
            // Clamped: a size that is an estimate can lag the bytes it is
            // meant to bound, and a fraction over 1.0 draws a bar past its
            // own track.
            Some(s) if s > 0 => (self.downloaded as f64 / s as f64).min(1.0) as f32,
            _ => 0.0,
        }
    }

    /// The Status column text of the main list, in the active locale.
    pub fn status_text(&self) -> String {
        use crate::i18n::tr;
        match self.state {
            DlState::Complete => tr("Complete"),
            DlState::Paused => match self.size {
                Some(s) if s > 0 => {
                    format!("{} ({})", tr("Paused"), crate::fmt::pct(self.downloaded, s))
                }
                _ => tr("Paused"),
            },
            DlState::Queued => tr("Queued"),
            DlState::Error => tr("Error"),
            DlState::Connecting => tr("Connecting..."),
            DlState::Receiving => match self.size {
                Some(s) => crate::fmt::pct(self.downloaded, s),
                None => tr("Receiving..."),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CategoryDef {
    pub name: String,
    /// Extensions (lowercase, no dot) auto-filed into this category.
    pub exts: Vec<String>,
    /// Default download directory for the category.
    pub dir: String,
    /// The stock categories cannot be renamed or deleted — their file types
    /// and folder are the user's, their identity is not: the tree icons, the
    /// [`categorize`] fallback and the AI seeding all name them.
    #[serde(default)]
    pub builtin: bool,
}

impl CategoryDef {
    /// A user-made category: no file types yet, filed under `Downloads/<name>`.
    pub fn new(name: &str) -> Self {
        CategoryDef {
            name: name.to_string(),
            exts: vec![],
            dir: sub(name),
            builtin: false,
        }
    }
}

fn downloads_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("Downloads"))
        .to_string_lossy()
        .into_owned()
}

/// Where a category's files go on a fresh install.
pub fn default_category_dir(name: &str) -> String {
    if name == DEFAULT_CATEGORY {
        downloads_dir()
    } else {
        sub(name)
    }
}

fn sub(cat: &str) -> String {
    PathBuf::from(downloads_dir())
        .join(cat)
        .to_string_lossy()
        .into_owned()
}

/// The catch-all category: the file types no other list claims, and the
/// folder [`category_dir`] falls back to.
///
/// It is the first entry and stays first — new categories are appended, and
/// Options refuses to rename or remove this one — because "uncategorised"
/// and "category folders are switched off" both resolve through it.
pub const DEFAULT_CATEGORY: &str = "General";

/// The category model weights and datasets are filed under.
///
/// A constant rather than a literal because three places have to agree on it:
/// the default list, the tree's icon lookup, and the one-time migration that
/// adds it to an install that predates it.
pub const AI_CATEGORY: &str = "AI & Datasets";

/// Model weights, checkpoints and dataset containers.
///
/// Deliberately WITHOUT csv, tsv and json. They are dataset formats, but they
/// are also what a large part of the ordinary web serves, and a capture list
/// that swallows every one of them turns the browser into a nuisance — the
/// user can add them, but they must ask.
pub const AI_TYPES: &[&str] = &[
    "SAFETENSORS",
    "GGUF",
    "GGML",
    "ONNX",
    "PT",
    "PTH",
    "CKPT",
    "PB",
    "TFLITE",
    "H5",
    "HDF5",
    "KERAS",
    "NPY",
    "NPZ",
    "PKL",
    "MLMODEL",
    "MLPACKAGE",
    "CAFFEMODEL",
    "PARQUET",
    "PQ",
    "ARROW",
    "FEATHER",
    "AVRO",
    "ORC",
    "TFRECORD",
    "JSONL",
    "NDJSON",
];

/// The stock capture list: the classic media/archive set plus [`AI_TYPES`].
///
/// Composed rather than spelled out so the model formats have one definition
/// shared with the category and the migration.
pub fn default_auto_types() -> String {
    const BASE: &str = "3GP 7Z AAC ACE AIF APK ARJ ASF AVI BIN BZ2 DMG EXE GZ GZIP IMG ISO \
LZH M4A M4V MKV MOV MP3 MP4 MPA MPE MPEG MPG MSI MSU OGG OGV PDF PKG PPS PPT QT RA RAR RM \
RMVB SEA SIT SITX TAR TIF TIFF WAV WMA WMV Z ZIP";
    format!("{BASE} {}", AI_TYPES.join(" "))
}

pub fn default_categories() -> Vec<CategoryDef> {
    let e = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    vec![
        CategoryDef {
            name: DEFAULT_CATEGORY.into(),
            exts: vec![],
            dir: downloads_dir(),
            builtin: true,
        },
        CategoryDef {
            name: AI_CATEGORY.into(),
            exts: AI_TYPES.iter().map(|e| e.to_ascii_lowercase()).collect(),
            dir: sub(AI_CATEGORY),
            builtin: true,
        },
        CategoryDef {
            name: "Compressed".into(),
            exts: e("zip rar 7z gz gzip bz2 tar arj lzh sit sitx sea ace z xz zst"),
            dir: sub("Compressed"),
            builtin: true,
        },
        CategoryDef {
            name: "Documents".into(),
            exts: e("doc docx pdf ppt pptx pps txt rtf odt xls xlsx csv epub chm djvu"),
            dir: sub("Documents"),
            builtin: true,
        },
        CategoryDef {
            name: "Music".into(),
            exts: e("mp3 aac m4a wav wma ogg flac aif mpa ra"),
            dir: sub("Music"),
            builtin: true,
        },
        CategoryDef {
            name: "Programs".into(),
            exts: e("exe msi msu dmg pkg deb rpm appimage apk bin img iso"),
            dir: sub("Programs"),
            builtin: true,
        },
        CategoryDef {
            name: "Video".into(),
            exts: e("avi mp4 mkv mov mpg mpeg wmv flv m4v webm rm rmvb ogv 3gp asf qt ts"),
            dir: sub("Video"),
            builtin: true,
        },
    ]
}

/// Category for a file name, by extension, against the configured lists.
///
/// A name with no extension at all files under Programs: bare names like
/// `meilisearch-linux-aarch64` are almost always executables, and General
/// told the user nothing.
pub fn categorize(file: &str, cats: &[CategoryDef]) -> Option<String> {
    match file.rsplit_once('.') {
        None => cats
            .iter()
            .find(|c| c.name == "Programs")
            .map(|c| c.name.clone()),
        Some((_, ext)) => {
            let ext = ext.to_ascii_lowercase();
            cats.iter()
                .find(|c| c.exts.contains(&ext))
                .map(|c| c.name.clone())
        }
    }
}

/// Whether `name` is usable as a category name.
///
/// Printable ASCII only. The name is also a folder name and the key every
/// download is filed under, and duplicates are rejected case-insensitively
/// — which only ASCII case folding can do correctly here, so a non-ASCII
/// name would let two categories that a case-insensitive filesystem maps to
/// one folder both exist.
///
/// A separator, a drive letter or a `..` is refused rather than quietly
/// sanitised: a category must not be able to file downloads outside the
/// folder its name describes. The Windows-reserved characters are refused
/// everywhere, because a config written on one platform is opened on
/// another.
pub fn valid_category_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 64
        && n != "."
        && n != ".."
        && !n.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*'])
        && n.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
}

/// One category's file types as typed in Options: whitespace or commas
/// separate them, a leading dot is optional, case is not significant.
pub fn parse_exts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for token in text.split([' ', '\t', '\n', '\r', ',', ';']) {
        let ext = token.trim().trim_start_matches('.').to_ascii_lowercase();
        if !ext.is_empty() && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

/// Split the Save-to tab's name box into a category name and the file types
/// to create it with: `Pictures: .png .jpg`.
///
/// A colon can never be part of a category name — [`valid_category_name`]
/// refuses it, because the name is also a folder name — so the split is
/// unambiguous, and a name may still contain spaces ("My Games").
pub fn parse_category_entry(text: &str) -> (&str, Vec<String>) {
    match text.split_once(':') {
        Some((name, exts)) => (name.trim(), parse_exts(exts)),
        None => (text.trim(), vec![]),
    }
}

/// Give `exts` to the category called `name`, taking each one away from the
/// other categories.
///
/// An extension belongs to exactly one category: [`categorize`] stops at the
/// first list that matches, so the same extension in two lists files by
/// whichever category happens to come first — adding `msix` to Programs
/// would look like it did nothing at all if another list already had it.
pub fn set_category_exts(cats: &mut [CategoryDef], name: &str, exts: Vec<String>) {
    for c in cats.iter_mut().filter(|c| c.name != name) {
        c.exts.retain(|e| !exts.contains(e));
    }
    if let Some(c) = cats.iter_mut().find(|c| c.name == name) {
        c.exts = exts;
    }
}

/// Folder a download filed under `cat` should be saved in.
///
/// With `flat` on (Options > Save to > "Do not create category folders")
/// every category resolves to the first — General — folder, so no
/// per-category subdirectory is ever created. `cat` of `None` means "not
/// categorized", which also lands in General.
///
/// `None` comes back only when `cats` is empty or when `cat` names a
/// category that no longer exists; callers decide whether to fall back or to
/// leave the item's folder as it is.
pub fn category_dir(cats: &[CategoryDef], cat: Option<&str>, flat: bool) -> Option<String> {
    match cat.filter(|_| !flat) {
        Some(c) => cats.iter().find(|k| k.name == c).map(|k| k.dir.clone()),
        None => cats.first().map(|k| k.dir.clone()),
    }
}

/// Which palette the interface paints with (View > Theme).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    /// Follow the OS appearance, and keep following it: a desktop that
    /// switches to dark at sunset takes the interface with it, with no visit
    /// to the menu.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProxyMode {
    None,
    System,
    Script,
    Manual,
}

/// What the manually configured proxy speaks.
///
/// The distinction is not cosmetic: a SOCKS proxy carries a TCP stream and is
/// dialled by the connector, while an HTTP proxy parses the request line and
/// is addressed by the target. `crate::proxy` turns this into one or the
/// other; picking the wrong one produces a connection that fails with a
/// message about the wrong protocol.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ProxyType {
    /// Forward proxy: absolute-form requests, `CONNECT` for TLS. The default
    /// because it is what a bare `host:port` means everywhere else.
    #[default]
    Http,
    Socks4,
    Socks4a,
    Socks5,
}

impl ProxyType {
    /// The URL scheme this type is spelled with, which is also what
    /// `hya_net::Proxy::parse` reads.
    pub fn scheme(self) -> &'static str {
        match self {
            ProxyType::Http => "http",
            ProxyType::Socks4 => "socks4",
            ProxyType::Socks4a => "socks4a",
            ProxyType::Socks5 => "socks5",
        }
    }

    /// Every type, in the order the Options picker lists them.
    pub const ALL: [ProxyType; 4] = [
        ProxyType::Http,
        ProxyType::Socks4,
        ProxyType::Socks4a,
        ProxyType::Socks5,
    ];
}

impl std::fmt::Display for ProxyType {
    /// The picker's label. Not translated: these are protocol names.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProxyType::Http => "HTTP",
            ProxyType::Socks4 => "SOCKS4",
            ProxyType::Socks4a => "SOCKS4a",
            ProxyType::Socks5 => "SOCKS5",
        })
    }
}

/// Which proxy ONE download uses.
///
/// A download manager is often the reason a proxy exists on the machine at
/// all: one large file has to go through the tunnel while everything else
/// stays on the fast direct path, or the other way round. The choice travels
/// with the item and is persisted, so a download resumed tomorrow still takes
/// the route it was started on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyChoice {
    /// Whatever Options > Proxy/Socks says, including later changes to it.
    #[default]
    Default,
    /// Straight to the origin, whatever Options says.
    Direct,
    /// This download's own proxy, as a full specification —
    /// `socks5://user:pass@host:port`.
    Custom(String),
}

impl ProxyChoice {
    /// Which of the three this is, for the dialogs' picker.
    pub fn pick(&self) -> ProxyPick {
        match self {
            ProxyChoice::Default => ProxyPick::Default,
            ProxyChoice::Direct => ProxyPick::Direct,
            ProxyChoice::Custom(_) => ProxyPick::Custom,
        }
    }

    /// The specification the user typed, empty for the other two.
    pub fn spec(&self) -> &str {
        match self {
            ProxyChoice::Custom(s) => s,
            _ => "",
        }
    }

    /// Rebuild from what a dialog holds: a picker value and the address box
    /// beside it, which keeps its text while the picker is on something else
    /// so switching back does not retype it.
    pub fn from_parts(pick: ProxyPick, spec: &str) -> Self {
        match pick {
            ProxyPick::Default => ProxyChoice::Default,
            ProxyPick::Direct => ProxyChoice::Direct,
            ProxyPick::Custom => ProxyChoice::Custom(spec.trim().to_string()),
        }
    }
}

/// The three options a per-download proxy picker offers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ProxyPick {
    #[default]
    Default,
    Direct,
    Custom,
}

impl ProxyPick {
    pub const ALL: [ProxyPick; 3] = [ProxyPick::Default, ProxyPick::Direct, ProxyPick::Custom];
}

impl std::fmt::Display for ProxyPick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            ProxyPick::Default => crate::i18n::tr("Default (Options)"),
            ProxyPick::Direct => crate::i18n::tr("No proxy"),
            ProxyPick::Custom => crate::i18n::tr("This download only"),
        })
    }
}

/// What "when done" should do to the machine — a queue's
/// [`Schedule::shutdown_when_done`] or a download's
/// [`DownloadItem::shutdown_after`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum PowerAction {
    #[default]
    Shutdown,
    LogOff,
    Sleep,
}

impl PowerAction {
    /// Whether the action takes the login session with it. Shutting down and
    /// logging off end the process either way, so Hydra exits with them;
    /// sleeping only suspends the machine, and Hydra keeps running and is
    /// still there when it wakes.
    pub fn ends_session(self) -> bool {
        matches!(self, PowerAction::Shutdown | PowerAction::LogOff)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLogin {
    pub site: String,
    pub user: String,
    pub pass: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundRow {
    pub event: String,
    pub enabled: bool,
    pub file: String,
}

/// A named cap for the global Speed Limiter, switchable in one click from
/// the toolbar's Speed Limit button.
///
/// `limit` is bytes/sec, and `None` means the profile turns the limiter off
/// — the stock "Unlimited" entry is how the quick control clears a cap
/// without the user having to find the toggle again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeedProfile {
    pub name: String,
    pub limit: Option<u64>,
}

/// The profiles a fresh install starts with: off, a cap that leaves a video
/// call usable, and a looser overnight one. They are ordinary rows — the
/// Connection tab renames, retunes and deletes them like any other.
pub fn default_speed_profiles() -> Vec<SpeedProfile> {
    vec![
        SpeedProfile {
            name: "Unlimited".into(),
            limit: None,
        },
        SpeedProfile {
            name: "Background".into(),
            limit: Some(500 * 1024),
        },
        SpeedProfile {
            name: "Night".into(),
            limit: Some(5 * 1024 * 1024),
        },
    ]
}

/// A column of the download table, and with it the key the list sorts by:
/// every column orders the list by its own value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Column {
    Name,
    Queue,
    Size,
    Status,
    TimeLeft,
    Rate,
    LastTry,
    Description,
}

impl Column {
    /// Every column, in the order a fresh install shows them.
    pub const ALL: [Column; 8] = [
        Column::Name,
        Column::Queue,
        Column::Size,
        Column::Status,
        Column::TimeLeft,
        Column::Rate,
        Column::LastTry,
        Column::Description,
    ];

    /// The header label, which is also the `tr` key the catalogues carry.
    pub fn label(self) -> &'static str {
        match self {
            Column::Name => "File Name",
            Column::Queue => "Q",
            Column::Size => "Size",
            Column::Status => "Status",
            Column::TimeLeft => "Time left",
            Column::Rate => "Transfer rate",
            Column::LastTry => "Last Try Date",
            Column::Description => "Description",
        }
    }

    pub fn default_width(self) -> f32 {
        match self {
            Column::Name => 300.0,
            // Icon-only queue-membership strip: the resize minimum is enough.
            Column::Queue => 40.0,
            Column::Size => 110.0,
            Column::Status => 120.0,
            Column::TimeLeft => 130.0,
            Column::Rate => 150.0,
            Column::LastTry => 175.0,
            Column::Description => 200.0,
        }
    }

    /// Stable id for the native menu integrations, which address a menu
    /// entry by string ([`crate::app::MenuAction::id`]).
    pub fn id(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Queue => "Queue",
            Column::Size => "Size",
            Column::Status => "Status",
            Column::TimeLeft => "TimeLeft",
            Column::Rate => "Rate",
            Column::LastTry => "LastTry",
            Column::Description => "Description",
        }
    }

    pub fn from_id(id: &str) -> Option<Column> {
        Column::ALL.into_iter().find(|c| c.id() == id)
    }
}

/// The key the main download table is sorted by: either one of the columns,
/// or the order in which items were added.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Column(Column),
    OrderOfAddition,
}

impl SortKey {
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Column(c) => c.id(),
            SortKey::OrderOfAddition => "OrderOfAddition",
        }
    }

    pub fn from_id(id: &str) -> Option<SortKey> {
        if id == "OrderOfAddition" || id == "Addition" {
            Some(SortKey::OrderOfAddition)
        } else {
            Column::from_id(id).map(SortKey::Column)
        }
    }
}

impl From<Column> for SortKey {
    fn from(c: Column) -> Self {
        SortKey::Column(c)
    }
}

impl Default for SortKey {
    fn default() -> Self {
        SortKey::Column(Column::LastTry)
    }
}

/// How one column is presented: its place in [`Settings::columns`] is its
/// place in the header, left to right.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ColumnPref {
    pub id: Column,
    pub width: f32,
    pub visible: bool,
}

impl ColumnPref {
    pub fn new(id: Column) -> Self {
        ColumnPref {
            id,
            width: id.default_width(),
            visible: true,
        }
    }
}

/// Move `col` one place towards `left`, past the neighbour the caller can
/// see: the header skips hidden columns (`visible_only`), while the manage
/// dialog lists every column and moves within that list.
///
/// Returns whether the order actually changed — the outermost column on that
/// side has nowhere to go.
pub fn move_column(cols: &mut [ColumnPref], col: Column, left: bool, visible_only: bool) -> bool {
    let Some(i) = cols.iter().position(|p| p.id == col) else {
        return false;
    };
    let movable = |j: &usize| !visible_only || cols[*j].visible;
    let j = if left {
        (0..i).rev().find(movable)
    } else {
        (i + 1..cols.len()).find(movable)
    };
    match j {
        Some(j) => {
            cols.swap(i, j);
            true
        }
        None => false,
    }
}

/// Carry a drag of `col` to pointer x, reordering as it passes a neighbour,
/// and answer with the reference x the next step measures from.
///
/// The drop target is derived from the distance dragged, never from an
/// absolute position: the table's left edge moves with the sidebar and the
/// sideways scroll, and neither is known here. A column changes places once
/// the drag has covered MORE than half the neighbour it is passing, and the
/// reference then moves with it — so the swap-back threshold is that same
/// line. The comparison is strict on purpose: `>=` would both swap and swap
/// back at exactly half a width, and the loop would never end.
pub fn drag_column(cols: &mut [ColumnPref], col: Column, from_x: f32, x: f32) -> f32 {
    let mut from_x = from_x;
    loop {
        let Some(i) = cols.iter().position(|p| p.id == col) else {
            return from_x;
        };
        let dx = x - from_x;
        let next = if dx > 0.0 {
            (i + 1..cols.len()).find(|&j| cols[j].visible)
        } else {
            (0..i).rev().find(|&j| cols[j].visible)
        };
        let Some(j) = next.filter(|&j| dx.abs() > cols[j].width / 2.0) else {
            return from_x;
        };
        from_x += cols[j].width * dx.signum();
        cols.swap(i, j);
    }
}

/// Narrowest and widest a column may be dragged, and the bounds a stored
/// width is held to — a config claiming a 20 000 px column would push every
/// other one off the window with no way back.
pub const COL_MIN_W: f32 = 40.0;
pub const COL_MAX_W: f32 = 800.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // General tab
    pub launch_on_startup: bool,
    /// Ask the release API for a newer version when the app starts; a hit
    /// opens the update dialog. Only the check is automatic — downloading
    /// and installing always wait for the user's "Update Now".
    pub check_updates_on_startup: bool,
    /// Update checks also consider `-rc` pre-releases: a release candidate
    /// ahead of the stable release is offered, otherwise stable is.
    pub beta_channel: bool,
    /// Autostart launches come up in the tray without opening the window.
    pub start_in_tray: bool,
    /// Closing the main window leaves Hydra running in the system tray
    /// (default) instead of quitting, so queues and transfers carry on.
    /// Off: the close button ends the session the way File > Exit does.
    /// Ignored when no tray icon could be installed — without one there
    /// would be no way back into the app, so closing always quits then.
    pub close_to_tray: bool,
    /// Fewer wakeups everywhere: slower UI refresh, no glide animation,
    /// coarser engine ticks. Transfer speed is unaffected.
    pub power_save: bool,
    /// Hide the app from the Dock (macOS, activation policy Accessory) or
    /// the taskbar (Windows, skip_taskbar on each window). The tray icon
    /// remains the way back in. Not offered on Linux: neither Wayland nor
    /// iced/winit's X11 path exposes a skip-taskbar control.
    pub hide_from_taskbar: bool,
    /// GPU (wgpu) rendering instead of the default software raster; smoother
    /// on very large windows at the cost of a much larger memory baseline.
    pub gpu_render: bool,
    pub monitor_clipboard: bool,
    pub capture_browsers: Vec<(String, bool)>,
    // File types tab
    pub auto_types: String,
    /// Whether the model/dataset formats have already been offered to this
    /// install. See [`seed_ai_formats`] — it is a one-time additive migration,
    /// and this is what stops it from undoing a user's later edits.
    ///
    /// The field-level `default` is load-bearing and NOT redundant with the
    /// one on the struct: the struct's fills a missing field from
    /// `Settings::default()`, which says `true` because a fresh config already
    /// lists these formats. A config written before the flag existed would
    /// then read as "already seeded" and the migration would never run — the
    /// exact installs it exists for. Field-level wins, and gives `false`.
    #[serde(default)]
    pub ai_formats_seeded: bool,
    /// Options > Extensions: let a `--config DIR` (portable) copy register
    /// its own `hydra-host` with the browsers, so capture reaches THIS
    /// profile and can start it when nothing is running. Off by default —
    /// registration is machine-wide per user, so switching it on takes
    /// browser capture away from any ordinary install on the same account.
    /// Ignored without `--config`. See [`crate::nmhost::ensure_registered`].
    pub portable_capture: bool,
    /// WebSocket origins (`moz-extension://<uuid>`, ...) the user allowed to
    /// connect without the native host's token. Local to this profile: an
    /// origin names one browser install, so it neither exports nor imports.
    pub allowed_extensions: Vec<String>,
    pub dont_start_sites: String,
    // Save to tab
    pub remember_last_dir: bool,
    pub server_file_date: bool,
    /// Off (default): a download lands in its category's folder, so a fresh
    /// install fills `~/Downloads` with `Video/`, `Documents/`, ... On: the
    /// per-category folders are ignored and everything is saved straight
    /// into the General category's folder (`~/Downloads` out of the box).
    /// Only new downloads are affected; items already on the list keep the
    /// folder they were added with.
    pub no_category_dirs: bool,
    // Downloads tab
    pub show_file_info_dialog: bool,
    /// Start pulling bytes while the Download File Info dialog is open.
    pub bg_download: bool,
    pub start_minimized: bool,
    pub show_speed_tab: bool,
    pub show_completion_tab: bool,
    pub show_hide_buttons: bool,
    /// Whether a progress window opens with the per-connection panel already
    /// expanded. Only the state a window starts in — the Show/Hide details
    /// button still governs it once the window is up.
    pub show_conn_details: bool,
    pub show_complete_dialog: bool,
    /// Take a download off the list once it has finished — after the
    /// complete dialog is closed, when that dialog is enabled. Only the row
    /// goes; the downloaded file stays where it was saved.
    pub remove_completed: bool,
    pub user_agent: String,
    pub ffmpeg_path: String,
    pub virus_scanner: String,
    pub virus_args: String,
    // Connection tab
    /// `BROWSER[:PROFILE]` to read cookies from for downloads added by hand,
    /// empty for none. The same spelling the CLI's `--cookies-from-browser`
    /// takes, parsed by the same code, so the two cannot drift.
    ///
    /// Captures from the browser extension are unaffected: they already carry
    /// the exact cookies the page was using, which is better evidence than
    /// anything a profile scan can produce.
    #[serde(default)]
    pub cookies_from_browser: String,
    pub default_conns: usize,
    /// Measure-and-adapt connection count: the transfer starts at ONE
    /// connection and the in-band ramp (`hya_core::ramp`) admits more only
    /// while the aggregate rate says they pay for themselves, with
    /// `default_conns` as the ceiling. The project's own measurements found
    /// a fixed multi-connection setting slower than a single stream on four
    /// of five live objects (saturated links divide, they do not add), so
    /// this defaults ON.
    pub adaptive_conns: bool,
    pub conn_exceptions: Vec<(String, usize)>,
    pub dl_limit_enabled: bool,
    pub dl_limit_mb: u64,
    pub dl_limit_hours: u64,
    pub warn_before_stop: bool,
    // Proxy tab
    pub proxy_mode: ProxyMode,
    pub proxy_script: String,
    /// Address of the manual proxy: a host, or a full `socks5://host:port`
    /// spec pasted from whatever published it.
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_pass: String,
    /// What that address speaks. Configs written before the setting existed
    /// deserialize as `Http`, which is what they were treated as.
    pub proxy_type: ProxyType,
    // Sites logins
    pub logins: Vec<SiteLogin>,
    // Sounds
    pub sounds: Vec<SoundRow>,
    // View state
    /// Light, Dark, or System (follow the OS) — View > Theme. `None` marks a
    /// config written before the setting existed; [`load_config`] resolves it
    /// from the legacy `dark_mode` flag, so it is `Some` while running.
    pub theme_mode: Option<ThemeMode>,
    /// The old View > Dark Mode support checkbox. Read once at load, folded
    /// into `theme_mode`, and dropped from the file on the next save — an
    /// upgrade must not silently move a user off the palette they picked.
    pub dark_mode: Option<bool>,
    pub show_categories: bool,
    /// View > Hide toolbar text. Off, the toolbar draws icons alone and each
    /// one carries its label as a hover tooltip instead.
    pub show_toolbar_labels: bool,
    /// View > Scale, in percent of the size the layout was drawn at; see
    /// [`crate::theme::ui_scale`].
    pub ui_scale_pct: u16,
    /// The old View > Font point size (10..20 against a 13 pt layout). Read
    /// once at load, folded into `ui_scale_pct`, and dropped from the file on
    /// the next save — an upgrade must not move a reader off the size they
    /// had settled on.
    pub font_size: Option<u16>,
    /// Global cap from the toolbar's Speed Limit button, bytes/sec. Kept
    /// across a switch to an unlimited profile so turning the limiter back
    /// on restores the number that was last in force.
    pub global_speed_limit: Option<u64>,
    pub speed_limiter_on: bool,
    /// Named caps the quick control offers; see [`SpeedProfile`].
    pub speed_profiles: Vec<SpeedProfile>,
    /// Download-table columns, left to right: width, and whether the header
    /// shows them at all. [`load_config`] normalizes the list, so the rest of
    /// the program can rely on it naming every [`Column`] exactly once.
    pub columns: Vec<ColumnPref>,
    /// The old per-position width list, from before columns could be
    /// reordered or hidden. Read once at load, folded into `columns`, and
    /// dropped from the file on the next save.
    pub column_widths: Vec<f32>,
    /// Last main-window size; restored on start when still sensible.
    pub window_size: Option<(f32, f32)>,
    /// Last main-window top-left corner in OS points; restored on start
    /// while it still lands on a connected display.
    pub window_pos: Option<(f32, f32)>,
}

/// The User-Agent a download added by hand sends unless Options picks another.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 6.1; Trident/7.0; rv:11.0) like Gecko";

impl Default for Settings {
    fn default() -> Self {
        Settings {
            launch_on_startup: true,
            check_updates_on_startup: true,
            beta_channel: false,
            start_in_tray: true,
            close_to_tray: true,
            power_save: false,
            hide_from_taskbar: false,
            gpu_render: false,
            monitor_clipboard: false,
            capture_browsers: [
                "Apple Safari",
                "Google Chrome",
                "Microsoft Edge",
                "Mozilla Firefox",
                "Opera",
            ]
            .iter()
            .map(|b| (b.to_string(), true))
            .collect(),
            auto_types: default_auto_types(),
            // A fresh config already has them, so there is nothing to add.
            ai_formats_seeded: true,
            portable_capture: false,
            allowed_extensions: Vec::new(),
            dont_start_sites: "*.update.microsoft.com download.windowsupdate.com".into(),
            remember_last_dir: true,
            server_file_date: false,
            no_category_dirs: false,
            show_file_info_dialog: true,
            bg_download: true,
            start_minimized: false,
            show_speed_tab: true,
            show_completion_tab: true,
            show_hide_buttons: true,
            show_conn_details: true,
            show_complete_dialog: true,
            remove_completed: false,
            user_agent: DEFAULT_USER_AGENT.into(),
            cookies_from_browser: String::new(),
            ffmpeg_path: String::new(),
            virus_scanner: String::new(),
            virus_args: String::new(),
            // Default: 8 connections; the scheduler settles well at this
            // order on live origins.
            default_conns: 8,
            adaptive_conns: true,
            conn_exceptions: vec![],
            dl_limit_enabled: false,
            dl_limit_mb: 200,
            dl_limit_hours: 5,
            warn_before_stop: true,
            proxy_mode: ProxyMode::None,
            proxy_script: String::new(),
            proxy_host: String::new(),
            proxy_port: String::new(),
            proxy_user: String::new(),
            proxy_pass: String::new(),
            proxy_type: ProxyType::default(),
            logins: vec![],
            sounds: [
                "Download complete",
                "Download failed",
                "Queue processing started",
                "Queue processing stopped/finished",
            ]
            .iter()
            .map(|e| SoundRow {
                event: e.to_string(),
                enabled: false,
                file: String::new(),
            })
            .collect(),
            theme_mode: None,
            dark_mode: None,
            show_categories: true,
            show_toolbar_labels: true,
            ui_scale_pct: 100,
            font_size: None,
            global_speed_limit: None,
            speed_limiter_on: false,
            speed_profiles: default_speed_profiles(),
            columns: vec![],
            column_widths: vec![],
            window_size: None,
            window_pos: None,
        }
    }
}

impl Settings {
    /// The palette View > Theme is set to. A config that predates the
    /// setting reads as [`ThemeMode::System`] here only if it also carried no
    /// `dark_mode` flag — [`load_config`] folds that one in first.
    pub fn theme(&self) -> ThemeMode {
        self.theme_mode.unwrap_or_default()
    }

    /// The cap the Speed Limiter is imposing right now, `None` when it is
    /// off. The switched-off number is deliberately not readable here: every
    /// caller wants the limit in force, not the one that would be.
    pub fn global_limit(&self) -> Option<u64> {
        self.speed_limiter_on
            .then_some(self.global_speed_limit)
            .flatten()
    }

    /// Which profile the quick control shows as current, by index.
    ///
    /// Derived from the cap in force rather than stored alongside it: a limit
    /// typed straight into Options would otherwise leave a profile ticked
    /// that no longer describes what the limiter is doing.
    pub fn active_profile(&self) -> Option<usize> {
        let cap = self.global_limit();
        self.speed_profiles.iter().position(|p| p.limit == cap)
    }

    /// Fold an accepted Options draft into the live settings.
    ///
    /// Three-way, not a copy: `base` is what the dialog opened on, and only
    /// a field the dialog actually changed is taken from `draft`. Everything
    /// else keeps its current value, so a window moved, a column resized or
    /// a toolbar toggle flipped while the dialog was up is not undone by OK.
    pub fn apply_options_draft(&mut self, base: &Settings, draft: &Settings) {
        macro_rules! take_changed {
            ($($field:ident),* $(,)?) => {
                $( if draft.$field != base.$field {
                    self.$field = draft.$field.clone();
                } )*
            };
        }
        take_changed!(
            launch_on_startup,
            check_updates_on_startup,
            beta_channel,
            start_in_tray,
            close_to_tray,
            power_save,
            hide_from_taskbar,
            gpu_render,
            monitor_clipboard,
            capture_browsers,
            auto_types,
            portable_capture,
            allowed_extensions,
            dont_start_sites,
            remember_last_dir,
            server_file_date,
            no_category_dirs,
            show_file_info_dialog,
            bg_download,
            start_minimized,
            show_speed_tab,
            show_completion_tab,
            show_hide_buttons,
            show_conn_details,
            show_complete_dialog,
            remove_completed,
            user_agent,
            ffmpeg_path,
            virus_scanner,
            virus_args,
            cookies_from_browser,
            default_conns,
            adaptive_conns,
            conn_exceptions,
            dl_limit_enabled,
            dl_limit_mb,
            dl_limit_hours,
            warn_before_stop,
            proxy_mode,
            proxy_script,
            proxy_host,
            proxy_port,
            proxy_user,
            proxy_pass,
            proxy_type,
            logins,
            sounds,
            global_speed_limit,
            speed_limiter_on,
            speed_profiles,
        );
    }
}

/// A wall-clock minute typed into the Scheduler, as `(hour, minute)`.
///
/// `9:00`, `21:0` and `9.00` all name a minute; the box is not a form to
/// fill in to the digit. Anything that is not two numbers in range is `None`.
pub fn parse_hhmm(s: &str) -> Option<(u8, u8)> {
    let (h, m) = s.trim().split_once([':', '.'])?;
    let h: u8 = h.trim().parse().ok()?;
    let m: u8 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// The canonical `HH:MM` spelling of a typed minute, or the text as typed
/// when it does not parse — the field then shows what is wrong instead of
/// quietly holding a value nothing will ever match.
pub fn normalize_hhmm(s: &str) -> String {
    match parse_hhmm(s) {
        Some((h, m)) => format!("{h:02}:{m:02}"),
        None => s.to_string(),
    }
}

/// A shortcut as typed, in the one spelling a key press produces:
/// The platform's primary command modifier name: "cmd" on macOS, "ctrl" elsewhere.
pub fn primary_modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}

/// Convert a combo to the platform's default spelling (e.g. "cmd+n" -> "ctrl+n" on Windows/Linux).
pub fn platform_default(combo: &str) -> String {
    if cfg!(target_os = "macos") {
        combo.to_string()
    } else {
        combo.replace("cmd+", "ctrl+")
    }
}

/// Autocomplete modifier keywords as the user types in the shortcuts editor.
///
/// Converts typos like `atl` -> `alt+`, and modifier keywords (`ctrl`, `alt`,
/// `win`, `cmd`, `shift`, etc.) into their completed `+` forms while typing forward.
/// Deletions (backspacing) are preserved so the user can edit freely.
pub fn autocomplete_combo(typed: &str, prev: &str) -> String {
    let lower = typed.to_ascii_lowercase();
    if typed.len() <= prev.len() {
        return lower;
    }

    let cleaned = lower.replace([' ', '-'], "+");
    let parts: Vec<&str> = cleaned.split('+').collect();
    let trailing_plus = cleaned.ends_with('+');

    fn normalize_token(t: &str) -> &str {
        match t {
            "atl" | "option" | "opt" => "alt",
            "control" | "ctl" => "ctrl",
            "windows" => "win",
            "command" => "cmd",
            "shft" => "shift",
            other => other,
        }
    }

    let mut out = Vec::new();
    let num_parts = parts.len();

    for (idx, &part) in parts.iter().enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let norm = normalize_token(trimmed);
        out.push(norm);

        let is_last = idx == num_parts - 1;
        if is_last
            && !trailing_plus
            && matches!(
                norm,
                "alt" | "ctrl" | "win" | "cmd" | "shift" | "super" | "meta"
            )
        {
            out.push("");
        }
    }

    if trailing_plus && !out.is_empty() && !out.last().unwrap().is_empty() {
        out.push("");
    }

    out.join("+")
}

/// Normalize a combo into the platform's canonical representation:
/// primary modifier ("cmd" on macOS, "ctrl" elsewhere), then optional `shift`,
/// then optional `alt`, then a single character, lowercase.
pub fn normalize_combo(typed: &str) -> Option<String> {
    let mut parts: Vec<String> = typed
        .replace([' ', '-'], "+")
        .split('+')
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.trim().to_ascii_lowercase())
        .collect();
    let base = parts.pop()?;
    if base.chars().count() != 1 {
        return None;
    }
    let (mut cmd, mut shift, mut alt) = (false, false, false);
    for part in parts {
        match part.as_str() {
            "cmd" | "command" | "ctrl" | "control" | "meta" | "super" | "win" | "windows" => {
                cmd = true;
            }
            "shift" | "shft" => shift = true,
            "alt" | "atl" | "option" | "opt" => alt = true,
            _ => return None,
        }
    }
    if !cmd {
        return None;
    }
    let mut combo = format!("{}+", primary_modifier());
    if shift {
        combo.push_str("shift+");
    }
    if alt {
        combo.push_str("alt+");
    }
    combo.push_str(&base);
    Some(combo)
}

/// `"{base} N"` for the smallest N no queue already carries.
///
/// Queues are addressed by name everywhere, so a new one must never come out
/// with a name in use — which `len() + 1` did the moment a queue had been
/// deleted from the middle.
pub fn free_queue_name(queues: &[QueueDef], base: &str) -> String {
    (1..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !queues.iter().any(|q| q.name == *name))
        .expect("the naturals are never exhausted")
}

pub(crate) const MAX_CONNECTIONS: usize = 256;
pub(crate) const CONNECTION_OPTIONS: [usize; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

/// What the Scheduler's two number fields accept, whether they are typed
/// into or stepped with their arrows. The ceiling on simultaneous files is
/// the queue's own: past a handful the connections compete for the same
/// link instead of filling it. A retry budget of zero would read as "retry,
/// but never" — the checkbox beside it is what turns retries off.
pub const FILES_AT_ONCE: std::ops::RangeInclusive<u32> = 1..=16;
pub const QUEUE_RETRIES: std::ops::RangeInclusive<u32> = 1..=99;

/// A typed number pinned into the range its field accepts.
pub fn clamp_to(v: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
    v.clamp(*range.start(), *range.end())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    /// One-time downloading vs periodic synchronization.
    pub periodic: bool,
    pub start_on_startup: bool,
    pub start_enabled: bool,
    /// `23:00` wall-clock.
    pub start_at: String,
    pub once: bool,
    pub days: [bool; 7],
    pub stop_enabled: bool,
    pub stop_at: String,
    pub retries_enabled: bool,
    pub retries: u32,
    pub open_file_enabled: bool,
    pub open_file: String,
    pub exit_when_done: bool,
    /// Shut down, log off or sleep the computer once every file in the queue
    /// has downloaded — checked in the same "when done" pass as
    /// `exit_when_done`. The action itself only runs after the cancellable
    /// countdown in `WinKind::Power`.
    #[serde(default)]
    pub shutdown_when_done: bool,
    #[serde(default)]
    pub shutdown_action: PowerAction,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            periodic: false,
            start_on_startup: false,
            start_enabled: false,
            start_at: "23:00".into(),
            once: false,
            days: [true; 7],
            stop_enabled: false,
            stop_at: "07:30".into(),
            retries_enabled: false,
            retries: 10,
            open_file_enabled: false,
            open_file: String::new(),
            exit_when_done: false,
            shutdown_when_done: false,
            shutdown_action: PowerAction::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueDef {
    pub name: String,
    pub files_at_once: u32,
    pub schedule: Schedule,
    /// The two stock queues cannot be renamed or deleted.
    #[serde(default)]
    pub builtin: bool,
    /// Folder-icon colour, packed `0xRRGGBB`, chosen when the queue is
    /// created so each user queue is told apart at a glance. `None` is the
    /// stock yellow folder of the two built-in queues (and of queues saved
    /// before the field existed).
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(skip)]
    pub running: bool,
    /// Whether this run of the queue actually started at least one member.
    /// Gates the "queue finished" actions (sound, open file, exit): a queue
    /// started with nothing to do must not fire `exit_when_done`.
    #[serde(skip)]
    pub did_work: bool,
}

pub fn default_queues() -> Vec<QueueDef> {
    ["Main download queue", "Synchronization queue"]
        .iter()
        .map(|n| QueueDef {
            name: n.to_string(),
            files_at_once: 4,
            schedule: Schedule::default(),
            builtin: true,
            color: None,
            running: false,
            did_work: false,
        })
        .collect()
}

/// Folder colours handed to new queues: distinct hues that read on both the
/// light and the dark window, none of them the stock yellow.
pub const QUEUE_COLORS: [u32; 8] = [
    0x4C8DFF, // blue
    0x3DB56A, // green
    0xE05D5D, // red
    0xA66CFF, // violet
    0xFF8A3D, // orange
    0x2BB5C9, // teal
    0xE64FA0, // pink
    0x8C7A5B, // brown
];

/// A colour for a queue about to be created: random among the palette
/// entries no existing queue uses, so two new queues never come out alike
/// until the palette is exhausted; then random over the whole palette.
pub fn pick_queue_color(existing: &[QueueDef]) -> u32 {
    let free: Vec<u32> = QUEUE_COLORS
        .iter()
        .copied()
        .filter(|c| !existing.iter().any(|q| q.color == Some(*c)))
        .collect();
    let pool: &[u32] = if free.is_empty() {
        &QUEUE_COLORS
    } else {
        &free
    };
    // Enough randomness for a colour: the nanosecond clock, not a crypto
    // source, and no dependency for it.
    let seed = crate::fmt::since_epoch().subsec_nanos() as usize;
    pool[seed % pool.len()]
}

/// The `--config DIR` the app was started with, absolute, when one was
/// given. Written once in `main` before anything reads [`app_dir`].
static APP_DIR_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Run out of `dir` instead of the platform application directory. Called
/// from `main` for `--config DIR`, before the first [`app_dir`] read; a
/// second call is ignored, since half the process would already be pointing
/// at the first answer.
pub fn set_app_dir(dir: PathBuf) {
    let _ = APP_DIR_OVERRIDE.set(dir);
}

/// The `--config DIR` in force, if any.
///
/// Callers that hand this instance's identity to some OTHER process — the
/// login item that relaunches it, the native-messaging host the browser
/// spawns — have to know the directory is not the one that process would
/// otherwise assume.
pub fn app_dir_override() -> Option<&'static std::path::Path> {
    APP_DIR_OVERRIDE.get().map(|p| p.as_path())
}

/// The application directory holding `config.toml`, `state.redb`,
/// `locales/` and `logs/`.
///
/// `--config DIR` moves all of it, so a portable install (a USB stick, a
/// second profile) keeps its settings and download list beside itself.
/// Without the flag: deliberately NOT `dirs::config_dir()` everywhere — on
/// macOS that resolves to `~/Library/Application Support`, and hydra's
/// convention (shared with the CLI) is `~/.config/hydra` on both Linux and
/// macOS. Windows uses `%APPDATA%\hydra`
/// (`Users\{user}\AppData\Roaming\hydra`).
pub fn app_dir() -> PathBuf {
    if let Some(dir) = APP_DIR_OVERRIDE.get() {
        return dir.clone();
    }
    hya_plugin::hydra_dir()
}

/// User configuration: everything the Options/Scheduler dialogs edit.
/// Stored as `config.toml`; a missing or unparsable file yields defaults.
/// (action id, default combo, English label) — the shortcut table.
pub const SHORTCUT_ACTIONS: [(&str, &str, &str); 12] = [
    ("add_url", "cmd+n", "Add new download"),
    (
        "clipboard_add",
        "cmd+shift+v",
        "Add URL from clipboard and start",
    ),
    ("options", "cmd+,", "Options"),
    ("scheduler", "cmd+e", "Scheduler"),
    ("resume_last", "cmd+r", "Resume last unfinished download"),
    ("stop_last", "cmd+s", "Stop last active download"),
    ("start_main_queue", "cmd+shift+r", "Start main queue"),
    ("stop_main_queue", "cmd+shift+s", "Stop main queue"),
    ("select_all", "cmd+a", "Select all downloads"),
    (
        "remove_selected",
        "cmd+alt+r",
        "Remove selected downloads from the list",
    ),
    ("close_window", "cmd+w", "Close window"),
    ("quit", "cmd+q", "Exit Hydra"),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub language: Option<String>,
    /// action id -> combo ("cmd+shift+v"); `cmd` is ⌘ on macOS, Ctrl
    /// elsewhere. Editable in Help > Keyboard Shortcuts.
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// Log verbosity: debug | info | warn | error (default info).
    pub log_level: Option<String>,
    /// "software" (default; ~10x lighter in memory) or "gpu".
    pub renderer: Option<String>,
    pub settings: Settings,
    pub categories: Vec<CategoryDef>,
    pub queues: Vec<QueueDef>,
}

/// Rolling accounting for the Connection tab's "Download no more than N
/// MBytes every H hours".
///
/// Lives in the state file rather than in [`Settings`], deliberately: the
/// Options dialog edits a *clone* of the settings and writes the whole clone
/// back on OK, so a counter kept there would be rewound to whatever it read
/// when the dialog opened — silently refunding every byte transferred while
/// the dialog was on screen.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DlQuota {
    /// Bytes transferred inside the current window.
    pub used: u64,
    /// Unix seconds the window opened; 0 = no window running yet. The window
    /// starts at the first accounted byte, not at app start, so an idle
    /// Hydra does not burn through periods it never downloaded in.
    pub window_start: i64,
}

/// Runtime state: the download list. Kept separate from configuration so a
/// hand-edited (or broken) config never touches the user's download history.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StateFile {
    pub next_id: DlId,
    pub downloads: Vec<DownloadItem>,
    pub dl_quota: DlQuota,
}

impl Default for StateFile {
    fn default() -> Self {
        StateFile {
            next_id: 1,
            downloads: vec![],
            dl_quota: DlQuota::default(),
        }
    }
}

pub fn load_config() -> ConfigFile {
    let path = app_dir().join("config.toml");
    let cfg = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            crate::log::warn(&format!("config.toml unparsable ({e}); using defaults"));
            ConfigFile::default()
        }),
        Err(_) => ConfigFile::default(),
    };
    normalize_config(cfg)
}

/// Fill in defaults and migrate older keys — whatever version of Hydra, on
/// whatever machine, wrote `cfg`.
pub fn normalize_config(mut cfg: ConfigFile) -> ConfigFile {
    if cfg.categories.is_empty() {
        cfg.categories = default_categories();
    }
    if cfg.queues.is_empty() {
        cfg.queues = default_queues();
    }
    for (id, combo, _) in SHORTCUT_ACTIONS {
        let entry = cfg
            .shortcuts
            .entry(id.to_string())
            .or_insert_with(|| platform_default(combo));
        if let Some(norm) = normalize_combo(entry) {
            *entry = norm;
        }
    }
    // Configs written before the flag existed: stock names are stock queues.
    for q in &mut cfg.queues {
        if q.name == "Main download queue" || q.name == "Synchronization queue" {
            q.builtin = true;
        }
    }
    mark_builtin_categories(&mut cfg.categories);
    migrate_ui_scale(&mut cfg.settings);
    migrate_theme_mode(&mut cfg.settings);
    migrate_columns(&mut cfg.settings);
    migrate_user_agent(&mut cfg.settings);
    seed_ai_formats(&mut cfg);
    cfg
}

/// Flag the stock categories, for a config written before the flag existed.
///
/// The stock names are the stock categories, exactly as the two stock queues
/// are recognised by name above. Re-derived on every load rather than
/// trusted from the file, so a hand-edited config cannot make Programs
/// deletable — or lock a user's own category by claiming the flag.
fn mark_builtin_categories(cats: &mut [CategoryDef]) {
    let stock: Vec<String> = default_categories().into_iter().map(|c| c.name).collect();
    for c in cats {
        c.builtin = stock.contains(&c.name);
    }
}

/// Add the model and dataset formats to an install that predates them.
///
/// Changing the DEFAULTS only ever reaches a fresh install: `auto_types` and
/// the category list are both written to `config.toml` in full, so an existing
/// user would never see the new formats no matter what the defaults said.
///
/// Additive and one-time. It appends the types the list does not already have
/// and adds the category if it is absent, then records that it has run — so a
/// user who afterwards trims the list or deletes the category does not find
/// either restored on the next launch.
fn seed_ai_formats(cfg: &mut ConfigFile) {
    if cfg.settings.ai_formats_seeded {
        return;
    }
    cfg.settings.ai_formats_seeded = true;

    let listed: std::collections::HashSet<String> = cfg
        .settings
        .auto_types
        .split_whitespace()
        .map(|t| t.to_ascii_uppercase())
        .collect();
    let missing: Vec<&str> = AI_TYPES
        .iter()
        .copied()
        .filter(|t| !listed.contains(*t))
        .collect();
    if !missing.is_empty() {
        // Appended rather than merged and re-sorted: the field is a set, and
        // rewriting the order a user typed is not this migration's business.
        let sep = if cfg.settings.auto_types.trim().is_empty() {
            ""
        } else {
            " "
        };
        cfg.settings.auto_types = format!(
            "{}{sep}{}",
            cfg.settings.auto_types.trim_end(),
            missing.join(" ")
        );
    }
    if !cfg.categories.iter().any(|c| c.name == AI_CATEGORY) {
        cfg.categories.push(CategoryDef {
            name: AI_CATEGORY.into(),
            exts: AI_TYPES.iter().map(|e| e.to_ascii_lowercase()).collect(),
            dir: sub(AI_CATEGORY),
            builtin: true,
        });
    }
    crate::log::info(&format!(
        "config: seeded {} model/dataset formats",
        missing.len()
    ));
}

/// View > Scale replaced View > Font: the old setting was a point size
/// against a 13 pt layout and scaled the whole interface by the ratio, so a
/// saved size converts to the percentage it was already producing (10 pt ->
/// 77 % -> the 75 % step). Snapped onto an offered step, or the menu would
/// have no entry to tick. The legacy key is dropped either way, so the next
/// save writes just `ui_scale_pct`.
///
/// A nonsense value — 0 from a hand-edited file — leaves the setting at what
/// it already was rather than resetting the whole block, which is what the
/// same guard used to do.
fn migrate_ui_scale(s: &mut Settings) {
    if let Some(points) = s.font_size.take().filter(|p| *p > 0) {
        s.ui_scale_pct = crate::theme::nearest_scale(
            (u32::from(points) * 100 / crate::theme::FONT_SIZE as u32) as u16,
        );
    }
    if s.ui_scale_pct == 0 {
        s.ui_scale_pct = 100;
    }
}

/// View > Theme replaced the Dark Mode checkbox: a config written before it
/// carries only `dark_mode`, and that pick stands. Only a config with neither
/// key — a fresh install — starts out following the OS. The legacy key is
/// dropped either way, so the next save writes just `theme_mode`.
fn migrate_theme_mode(s: &mut Settings) {
    if s.theme_mode.is_none() {
        s.theme_mode = Some(match s.dark_mode {
            Some(true) => ThemeMode::Dark,
            Some(false) => ThemeMode::Light,
            None => ThemeMode::System,
        });
    }
    s.dark_mode = None;
}

/// Move a config off the legacy "Mozilla/5.0" default or an older
/// `hydra-gui/<version>` to the current version. Any other value is the user's
/// own choice.
fn migrate_user_agent(s: &mut Settings) {
    if s.user_agent == "Mozilla/5.0" || s.user_agent.starts_with("hydra-gui/") {
        s.user_agent = DEFAULT_USER_AGENT.into();
    }
}

/// Bring [`Settings::columns`] to the shape the table relies on: every
/// [`Column`] present exactly once, with a sane width.
///
/// A config written before columns could be reordered carries only
/// `column_widths`, one width per column in the stock order; those widths are
/// the user's and are kept. The legacy key is dropped either way, so the next
/// save writes just `columns`.
///
/// Unknown or repeated entries are dropped and missing ones appended, so a
/// config from another version — or a hand-edited one — cannot leave a column
/// unreachable: the manage dialog can only show what the list names.
fn migrate_columns(s: &mut Settings) {
    let legacy = std::mem::take(&mut s.column_widths);
    if s.columns.is_empty() {
        s.columns = Column::ALL.into_iter().map(ColumnPref::new).collect();
        for (pref, w) in s.columns.iter_mut().zip(legacy) {
            pref.width = w;
        }
        // The Q column shrank from a queue-name text column to an icon-only
        // strip; configs saved before that still carry the old default, which
        // would leave a wide empty band next to File Name.
        if let Some(q) = s.columns.iter_mut().find(|p| p.id == Column::Queue) {
            if q.width == 110.0 {
                q.width = Column::Queue.default_width();
            }
        }
    }
    let mut seen = Vec::with_capacity(Column::ALL.len());
    s.columns.retain(|p| {
        let first = !seen.contains(&p.id);
        seen.push(p.id);
        first
    });
    for c in Column::ALL {
        if !s.columns.iter().any(|p| p.id == c) {
            s.columns.push(ColumnPref::new(c));
        }
    }
    for p in &mut s.columns {
        p.width = if p.width.is_finite() {
            p.width.clamp(COL_MIN_W, COL_MAX_W)
        } else {
            p.id.default_width()
        };
    }
    // File Name is what identifies a row: hiding it would leave a table of
    // sizes and dates with nothing to read them against.
    if let Some(p) = s.columns.iter_mut().find(|p| p.id == Column::Name) {
        p.visible = true;
    }
}

pub fn save_config(cfg: &ConfigFile) {
    let dir = app_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(s) = toml::to_string_pretty(cfg) {
        let _ = std::fs::write(dir.join("config.toml"), s);
    }
}

// ---------------------------------------------------------------- state db
//
// The download list lives in an embedded redb database (`state.redb`):
// crash-safe transactions and room to grow to many thousands of entries.
// A `gui-state.json` from earlier builds is migrated in on first load.

const DOWNLOADS: redb::TableDefinition<u64, &[u8]> = redb::TableDefinition::new("downloads");
/// Small singleton records that are not download rows. Only the download
/// limit's window counter lives here today; it is kept out of `config.toml`
/// on purpose (it moves while transfers run, and that file is hand-editable)
/// and out of the downloads table so saving it does not rewrite every row.
const META: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("meta");
const QUOTA_KEY: &str = "dl_quota";

fn state_db() -> Option<&'static redb::Database> {
    use std::sync::OnceLock;
    static DB: OnceLock<Option<redb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let dir = app_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state.redb");
        match redb::Database::create(&path) {
            Ok(db) => Some(db),
            Err(first) => {
                // A database written by an older redb (file format 2) cannot
                // be opened by redb 4: set it aside and start fresh — the
                // JSON snapshot from the original migration re-seeds below.
                crate::log::warn(&format!("state db needs recovery: {first}"));
                let _ = std::fs::rename(&path, dir.join("state.redb.old"));
                match redb::Database::create(&path) {
                    Ok(db) => Some(db),
                    Err(e) => {
                        crate::log::error(&format!("state db unavailable: {e}"));
                        None
                    }
                }
            }
        }
    })
    .as_ref()
}

pub fn load_state() -> StateFile {
    let mut st = StateFile::default();
    let mut loaded = false;
    if let Some(db) = state_db() {
        use redb::ReadableDatabase;
        if let Ok(txn) = db.begin_read() {
            if let Ok(table) = txn.open_table(DOWNLOADS) {
                use redb::ReadableTable;
                if let Ok(iter) = table.iter() {
                    for row in iter.flatten() {
                        if let Ok(d) = serde_json::from_slice::<DownloadItem>(row.1.value()) {
                            st.next_id = st.next_id.max(d.id + 1);
                            st.downloads.push(d);
                        }
                    }
                    loaded = true;
                }
            }
        }
    }
    // One-time migration from the JSON era — the live file first, then the
    // snapshot kept from a previous migration (used again after db recovery).
    if !loaded || st.downloads.is_empty() {
        for name in ["gui-state.json", "gui-state.json.migrated"] {
            let json = app_dir().join(name);
            if !json.exists() {
                continue;
            }
            if let Some(old) = std::fs::read(&json)
                .ok()
                .and_then(|b| serde_json::from_slice::<StateFile>(&b).ok())
            {
                st = old;
                save_state(&st);
                if name == "gui-state.json" {
                    let _ = std::fs::rename(&json, app_dir().join("gui-state.json.migrated"));
                }
                crate::log::info(&format!("migrated {name} into state.redb"));
                break;
            }
        }
    }
    if let Some(db) = state_db() {
        use redb::ReadableDatabase;
        if let Ok(txn) = db.begin_read() {
            if let Ok(table) = txn.open_table(META) {
                if let Ok(Some(row)) = table.get(QUOTA_KEY) {
                    if let Ok(q) = serde_json::from_slice::<DlQuota>(row.value()) {
                        st.dl_quota = q;
                    }
                }
            }
        }
    }
    // A transfer that was live when the process died is not live now.
    for d in &mut st.downloads {
        if d.state.is_active() {
            d.state = DlState::Paused;
        }
        d.disp_progress = d.progress();
    }
    st.downloads.sort_by_key(|d| d.id);
    st
}

pub fn save_state(st: &StateFile) {
    let Some(db) = state_db() else { return };
    let Ok(txn) = db.begin_write() else { return };
    {
        let _ = txn.delete_table(DOWNLOADS);
        if let Ok(mut table) = txn.open_table(DOWNLOADS) {
            for d in &st.downloads {
                if let Ok(bytes) = serde_json::to_vec(d) {
                    let _ = table.insert(d.id, bytes.as_slice());
                }
            }
        }
    }
    if let Err(e) = txn.commit() {
        crate::log::error(&format!("state save failed: {e}"));
    }
    save_quota(&st.dl_quota);
}

/// Persist the download-limit window on its own. The counter moves while
/// transfers run, and [`save_state`] rewrites the whole downloads table —
/// this is the one-row write a per-second update can afford.
pub fn save_quota(q: &DlQuota) {
    let Some(db) = state_db() else { return };
    let Ok(txn) = db.begin_write() else { return };
    {
        if let (Ok(mut table), Ok(bytes)) = (txn.open_table(META), serde_json::to_vec(q)) {
            let _ = table.insert(QUOTA_KEY, bytes.as_slice());
        }
    }
    if let Err(e) = txn.commit() {
        crate::log::error(&format!("download-limit counter save failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ffmpeg_path_survives_config_roundtrip_and_draft_merge() {
        let mut settings: Settings = toml::from_str("").unwrap();
        assert!(settings.ffmpeg_path.is_empty());
        let base = settings.clone();
        let mut draft = base.clone();
        draft.ffmpeg_path = "F:/Downloads/yt-dlp/ffmpeg.exe".into();
        settings.apply_options_draft(&base, &draft);
        let loaded: Settings = toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(loaded.ffmpeg_path, draft.ffmpeg_path);
    }

    #[test]
    fn a_stream_is_named_for_the_container_however_it_was_spelled() {
        for (container, ext) in [("TS", "ts"), ("ts", "ts"), ("MP4", "mp4"), ("", "mp4")] {
            assert_eq!(super::container_ext(container), ext);
            let si = super::StreamInfo {
                container: container.into(),
                ..super::StreamInfo::default()
            };
            assert_eq!(si.ext(), ext);
        }
    }
    /// The load-bearing half of the migration flag.
    ///
    /// `Settings` carries `#[serde(default)]`, which fills a missing field
    /// from `Settings::default()` — and that says `true`, because a fresh
    /// config already lists the model formats. Without the field's OWN
    /// `#[serde(default)]` an upgraded install would read as "already seeded"
    /// and never receive them, which is the only case the migration exists for.
    #[test]
    fn a_config_written_before_the_flag_reads_as_unseeded() {
        let old: Settings = toml::from_str("font_size = 13").expect("parse");
        assert!(!old.ai_formats_seeded, "an old config must still be seeded");
        assert!(
            Settings::default().ai_formats_seeded,
            "a fresh one must not"
        );
    }

    /// View > Font (a point size) became View > Scale (a percentage). An
    /// install that had chosen a size must come back reading at the size it
    /// chose, not snapped to 100 %.
    #[test]
    fn a_font_size_becomes_the_percentage_it_was_already_producing() {
        let migrated = |toml_src: &str| {
            let mut s: Settings = toml::from_str(toml_src).expect("parse");
            migrate_ui_scale(&mut s);
            s
        };

        assert_eq!(migrated("font_size = 13").ui_scale_pct, 100);
        assert_eq!(migrated("font_size = 10").ui_scale_pct, 75);
        assert_eq!(migrated("font_size = 20").ui_scale_pct, 150);
        // The legacy key is dropped, so the next save writes only the new
        // one and a later load cannot undo a scale chosen since.
        assert_eq!(migrated("font_size = 10").font_size, None);

        // A config that has already migrated keeps its percentage.
        assert_eq!(migrated("ui_scale_pct = 125").ui_scale_pct, 125);
        // ...and a fresh one, and nonsense, read as 100 %.
        assert_eq!(migrated("").ui_scale_pct, 100);
        assert_eq!(migrated("ui_scale_pct = 0").ui_scale_pct, 100);
        assert_eq!(migrated("font_size = 0").ui_scale_pct, 100);
    }

    /// The limiter has two halves — a switch and a number — and only the
    /// pair says what is in force. Reading the number alone is how a
    /// switched-off cap gets applied to a transfer anyway.
    #[test]
    fn a_switched_off_limiter_imposes_no_cap_but_keeps_its_number() {
        let mut s = Settings {
            global_speed_limit: Some(500 * 1024),
            speed_limiter_on: false,
            ..Settings::default()
        };
        assert_eq!(s.global_limit(), None);
        s.speed_limiter_on = true;
        assert_eq!(s.global_limit(), Some(500 * 1024));
    }

    /// The tick in the quick control follows the cap, not a stored name:
    /// a number typed straight into Options must untick the profile it no
    /// longer matches instead of mislabelling the limiter.
    #[test]
    fn the_ticked_profile_is_whichever_one_describes_the_cap_in_force() {
        let stock = default_speed_profiles();
        let unlimited = stock
            .iter()
            .position(|p| p.limit.is_none())
            .expect("the stock list offers a way to clear the cap");
        let background = stock
            .iter()
            .position(|p| p.limit == Some(500 * 1024))
            .expect("the stock list offers a background cap");

        let mut s = Settings::default();
        assert_eq!(s.active_profile(), Some(unlimited));

        s.speed_limiter_on = true;
        s.global_speed_limit = Some(500 * 1024);
        assert_eq!(s.active_profile(), Some(background));

        s.global_speed_limit = Some(777 * 1024);
        assert_eq!(s.active_profile(), None, "a hand-typed cap is no profile");

        // Switching the limiter off is the unlimited profile again, whatever
        // number the switch left behind.
        s.speed_limiter_on = false;
        assert_eq!(s.active_profile(), Some(unlimited));
    }

    /// Profiles were added after the config format, so an install that
    /// predates them has to arrive at the stock list rather than an empty
    /// dropdown with no way to fill it.
    #[test]
    fn a_config_written_before_profiles_existed_gets_the_stock_ones() {
        let old: Settings = toml::from_str("font_size = 13").expect("parse");
        assert_eq!(old.speed_profiles, default_speed_profiles());
    }

    /// ...but a list the user has since edited is theirs, empty included.
    #[test]
    fn an_edited_profile_list_survives_a_reload() {
        let mine = Settings {
            speed_profiles: vec![SpeedProfile {
                name: "Calls".into(),
                limit: Some(64 * 1024),
            }],
            ..Settings::default()
        };
        let text = toml::to_string(&mine).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("parse");
        assert_eq!(back.speed_profiles, mine.speed_profiles);

        let emptied = Settings {
            speed_profiles: vec![],
            ..Settings::default()
        };
        let text = toml::to_string(&emptied).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("parse");
        assert!(back.speed_profiles.is_empty(), "deleting them must stick");
    }

    fn legacy_config() -> ConfigFile {
        let mut cfg = ConfigFile {
            settings: Settings {
                auto_types: "ZIP EXE MP4".into(),
                ai_formats_seeded: false,
                ..Settings::default()
            },
            categories: default_categories(),
            ..ConfigFile::default()
        };
        cfg.categories.retain(|c| c.name != AI_CATEGORY);
        cfg
    }

    #[test]
    fn the_model_formats_reach_an_install_that_predates_them() {
        let mut cfg = legacy_config();
        seed_ai_formats(&mut cfg);

        let listed: Vec<&str> = cfg.settings.auto_types.split_whitespace().collect();
        for want in ["SAFETENSORS", "GGUF", "H5", "PQ", "PARQUET", "ONNX"] {
            assert!(listed.contains(&want), "{want} missing from {listed:?}");
        }
        // What was already there survives.
        for kept in ["ZIP", "EXE", "MP4"] {
            assert!(listed.contains(&kept), "{kept} was dropped");
        }
        assert!(cfg.categories.iter().any(|c| c.name == AI_CATEGORY));
        assert!(cfg.settings.ai_formats_seeded);
    }

    #[test]
    fn seeding_twice_changes_nothing_the_second_time() {
        let mut cfg = legacy_config();
        seed_ai_formats(&mut cfg);
        let once = cfg.clone();
        seed_ai_formats(&mut cfg);
        assert_eq!(cfg.settings.auto_types, once.settings.auto_types);
        assert_eq!(cfg.categories.len(), once.categories.len());
    }

    /// The flag's real job: a user who trims the list afterwards keeps it
    /// trimmed. Restoring it on every launch would make the setting unusable.
    #[test]
    fn a_later_edit_is_not_undone_on_the_next_launch() {
        let mut cfg = legacy_config();
        seed_ai_formats(&mut cfg);
        cfg.settings.auto_types = "ZIP".into();
        cfg.categories.retain(|c| c.name != AI_CATEGORY);
        seed_ai_formats(&mut cfg);
        assert_eq!(cfg.settings.auto_types, "ZIP");
        assert!(!cfg.categories.iter().any(|c| c.name == AI_CATEGORY));
    }

    #[test]
    fn a_type_already_listed_is_not_added_twice() {
        let mut cfg = legacy_config();
        // Lower case, because the list is matched case-insensitively.
        cfg.settings.auto_types = "zip safetensors".into();
        seed_ai_formats(&mut cfg);
        let n = cfg
            .settings
            .auto_types
            .split_whitespace()
            .filter(|t| t.eq_ignore_ascii_case("safetensors"))
            .count();
        assert_eq!(n, 1, "in {:?}", cfg.settings.auto_types);
    }

    #[test]
    fn model_and_dataset_files_land_in_the_ai_category() {
        let cats = default_categories();
        for f in [
            "model.safetensors",
            "llama-3-8b.Q4_K_M.gguf",
            "TEP_Mode1.h5",
            "train.parquet",
            "shard.pq",
            "net.onnx",
            "weights.ckpt",
        ] {
            assert_eq!(
                categorize(f, &cats).as_deref(),
                Some(AI_CATEGORY),
                "{f} was filed wrong"
            );
        }
        // The formats deliberately left out stay where they were.
        assert_eq!(
            categorize("export.csv", &cats).as_deref(),
            Some("Documents")
        );
    }

    use super::*;

    /// The Options picker lists `ProxyType::ALL` and routing reads
    /// `scheme()`: a type missing from either is a protocol the user can
    /// never choose, or one that resolves as something else.
    #[test]
    fn every_proxy_type_is_offered_and_names_its_own_scheme() {
        assert_eq!(ProxyType::ALL.len(), 4);
        for t in ProxyType::ALL {
            let spec = format!("{}://127.0.0.1:1080", t.scheme());
            let px = hya_net::Proxy::parse(&spec).expect("a scheme the transport knows");
            assert_eq!(px.kind.as_str(), t.scheme());
            assert!(
                t.to_string().eq_ignore_ascii_case(t.scheme()),
                "the label and the scheme must name the same protocol: {t}"
            );
        }
    }

    /// The dialogs hold a picker and an address box; the item holds one
    /// value. Round-tripping between them must not lose the address when the
    /// picker moves off "this download only" and back.
    #[test]
    fn a_per_download_proxy_round_trips_between_the_dialog_and_the_item() {
        let own = ProxyChoice::Custom("socks5://127.0.0.1:10808".into());
        assert_eq!(own.pick(), ProxyPick::Custom);
        assert_eq!(own.spec(), "socks5://127.0.0.1:10808");
        assert_eq!(ProxyChoice::from_parts(own.pick(), own.spec()), own);

        // The other two carry no address, whatever is still in the box.
        assert_eq!(
            ProxyChoice::from_parts(ProxyPick::Default, "socks5://h:1"),
            ProxyChoice::Default
        );
        assert_eq!(
            ProxyChoice::from_parts(ProxyPick::Direct, "socks5://h:1"),
            ProxyChoice::Direct
        );
        assert_eq!(ProxyChoice::default().spec(), "");
        assert_eq!(ProxyChoice::Direct.pick(), ProxyPick::Direct);
    }

    /// A config written before the proxy settings worked must still load, and
    /// must not silently acquire a proxy: the keys it carries are gone, and
    /// the ones that replaced them default to "no proxy".
    #[test]
    fn a_config_from_before_the_proxy_settings_worked_still_loads() {
        let old = r#"
            proxy_mode = "Manual"
            proxy_host = "127.0.0.1"
            proxy_port = "10808"
            proxy_http = true
            proxy_https = false
            proxy_ftp = false
            font_size = 13
        "#;
        let s: Settings = toml::from_str(old).expect("an old config still parses");
        assert_eq!(s.proxy_mode, ProxyMode::Manual);
        assert_eq!(s.proxy_host, "127.0.0.1");
        assert_eq!(s.proxy_type, ProxyType::Http);
    }

    #[test]
    fn a_new_queue_gets_a_colour_no_other_queue_has() {
        let mut queues = default_queues();
        assert!(
            queues.iter().all(|q| q.color.is_none()),
            "stock queues stay yellow"
        );
        for _ in 0..QUEUE_COLORS.len() {
            let c = pick_queue_color(&queues);
            assert!(QUEUE_COLORS.contains(&c));
            assert!(
                !queues.iter().any(|q| q.color == Some(c)),
                "{c:06X} handed out twice"
            );
            let mut q = queues[0].clone();
            q.color = Some(c);
            queues.push(q);
        }
        // Palette exhausted: still a palette colour, never a crash.
        assert!(QUEUE_COLORS.contains(&pick_queue_color(&queues)));
    }

    fn cats() -> Vec<CategoryDef> {
        vec![
            CategoryDef {
                name: "General".into(),
                exts: vec![],
                dir: "/dl".into(),
                builtin: true,
            },
            CategoryDef {
                name: "Video".into(),
                exts: vec!["mp4".into()],
                dir: "/dl/Video".into(),
                builtin: true,
            },
        ]
    }

    #[test]
    fn a_saved_legacy_agent_moves_to_the_new_default() {
        for old in ["hydra-gui/0.9.3", "hydra-gui/1.0.3", "Mozilla/5.0"] {
            let mut s: Settings = toml::from_str(&format!(r#"user_agent = "{old}""#)).unwrap();
            migrate_user_agent(&mut s);
            assert_eq!(s.user_agent, DEFAULT_USER_AGENT);
        }
    }

    #[test]
    fn a_user_chosen_agent_survives_the_migration() {
        for mine in ["curl/8.7.1", "my-hydra-mirror/1", ""] {
            let mut s = Settings {
                user_agent: mine.into(),
                ..Settings::default()
            };
            migrate_user_agent(&mut s);
            assert_eq!(s.user_agent, mine);
        }
    }

    #[test]
    fn the_default_agent_does_not_name_hydra() {
        assert!(!Settings::default()
            .user_agent
            .to_ascii_lowercase()
            .contains("hydra"));
        assert_eq!(Settings::default().user_agent, DEFAULT_USER_AGENT);
    }

    #[test]
    fn theme_mode_takes_over_from_the_dark_mode_flag() {
        let mut old: Settings = toml::from_str("dark_mode = true").unwrap();
        migrate_theme_mode(&mut old);
        assert_eq!(old.theme(), ThemeMode::Dark);
        // The flag is gone from the file after the next save.
        assert_eq!(old.dark_mode, None);
        assert!(!toml::to_string(&old).unwrap().contains("dark_mode"));

        let mut old: Settings = toml::from_str("dark_mode = false").unwrap();
        migrate_theme_mode(&mut old);
        assert_eq!(old.theme(), ThemeMode::Light);

        // Neither key: a fresh install follows the OS.
        let mut fresh: Settings = toml::from_str("").unwrap();
        migrate_theme_mode(&mut fresh);
        assert_eq!(fresh.theme(), ThemeMode::System);

        // A hand-edited config carrying both: the explicit setting wins.
        let mut both: Settings =
            toml::from_str("theme_mode = \"System\"\ndark_mode = true").unwrap();
        migrate_theme_mode(&mut both);
        assert_eq!(both.theme(), ThemeMode::System);
    }

    #[test]
    fn category_dir_uses_subfolder_unless_flat() {
        let c = cats();
        assert_eq!(
            category_dir(&c, Some("Video"), false),
            Some("/dl/Video".into())
        );
        // Flat: the category still classifies the file, but the folder is
        // always General's.
        assert_eq!(category_dir(&c, Some("Video"), true), Some("/dl".into()));
        // Uncategorized, and a category that no longer exists.
        assert_eq!(category_dir(&c, None, false), Some("/dl".into()));
        assert_eq!(category_dir(&c, Some("Gone"), false), None);
        assert_eq!(category_dir(&c, Some("Gone"), true), Some("/dl".into()));
        assert_eq!(category_dir(&[], Some("Video"), false), None);
    }

    #[test]
    fn only_sleeping_leaves_the_session_up() {
        // Shutdown and log off take the process with them, so Hydra exits
        // alongside them; sleep suspends the machine and Hydra is still
        // there on wake.
        assert!(PowerAction::Shutdown.ends_session());
        assert!(PowerAction::LogOff.ends_session());
        assert!(!PowerAction::Sleep.ends_session());
    }

    #[test]
    fn a_config_written_before_the_toolbar_toggle_keeps_its_labels() {
        // The toolbar has always drawn its labels; an upgrade must not empty
        // it out because the new key is absent from the file.
        let old: Settings = toml::from_str("show_categories = true\n").unwrap();
        assert!(old.show_toolbar_labels);
        let hidden: Settings = toml::from_str("show_toolbar_labels = false\n").unwrap();
        assert!(!hidden.show_toolbar_labels);
    }

    #[test]
    fn a_config_from_0_6_1_with_the_file_manager_toggle_still_loads() {
        // 0.6.1 wrote `select_in_file_manager`; the key is gone, and a file
        // that still carries it must keep the rest of the user's settings.
        let old: Settings =
            toml::from_str("select_in_file_manager = false\nremove_completed = true\n").unwrap();
        assert!(old.remove_completed);
    }

    #[test]
    fn a_config_written_before_sleep_existed_still_loads() {
        // The stored variant name is the wire format: adding Sleep must not
        // move Shutdown or LogOff.
        let sc: Schedule = toml::from_str(
            "periodic = false\nstart_on_startup = false\nstart_enabled = false\n\
             start_at = \"23:00\"\nonce = false\ndays = [true, true, true, true, true, true, true]\n\
             stop_enabled = false\nstop_at = \"07:30\"\nretries_enabled = false\nretries = 10\n\
             open_file_enabled = false\nopen_file = \"\"\nexit_when_done = false\n\
             shutdown_when_done = true\nshutdown_action = \"LogOff\"\n",
        )
        .unwrap();
        assert_eq!(sc.shutdown_action, PowerAction::LogOff);
        // A schedule saved before the field existed at all defaults to
        // shutting down, as it always did.
        let bare: Schedule = toml::from_str("shutdown_when_done = true\n").unwrap();
        assert_eq!(bare.shutdown_action, PowerAction::Shutdown);
        assert!(toml::to_string(&Schedule {
            shutdown_action: PowerAction::Sleep,
            ..Schedule::default()
        })
        .unwrap()
        .contains("shutdown_action = \"Sleep\""));
    }

    #[test]
    fn a_category_name_may_not_reach_outside_its_folder() {
        for ok in ["Pictures", "My Games", "Serie TV", "Games (2026)"] {
            assert!(valid_category_name(ok), "{ok} should be allowed");
        }
        // Non-ASCII is refused: the duplicate check folds case the ASCII
        // way, so "Bücher" and "BÜCHER" would both be created and then
        // collide in one folder on macOS and Windows.
        for bad in [
            "",
            "   ",
            "..",
            ".",
            "a/b",
            "a\\b",
            "C:",
            "we*ird",
            "a\nb",
            "Bücher",
            "برنامه\u{200c}ها",
        ] {
            assert!(!valid_category_name(bad), "{bad:?} should be refused");
        }
    }

    #[test]
    fn file_types_are_read_however_they_are_typed() {
        assert_eq!(
            parse_exts("MSIX, .msixbundle\nappx  APPX ;exe"),
            ["msix", "msixbundle", "appx", "exe"]
        );
        assert!(parse_exts("  , ; \n").is_empty());
    }

    /// A category is created with its file types in one go
    /// (`Pictures: .png .jpg`); the colon is safe as the separator because a
    /// name may never contain one.
    /// A config written before the flag existed carries none, so the stock
    /// categories would come back renamable and deletable — and a user
    /// category that happens to be listed there must not be locked.
    #[test]
    fn a_config_written_before_the_builtin_flag_still_knows_its_stock_categories() {
        let toml = "[[categories]]\nname = \"Programs\"\nexts = [\"exe\"]\ndir = \"/dl/p\"\n\n\
             [[categories]]\nname = \"Games\"\nexts = [\"nsp\"]\ndir = \"/dl/g\"\n";
        let mut cfg: ConfigFile = toml::from_str(toml).unwrap();
        mark_builtin_categories(&mut cfg.categories);
        assert!(cfg.categories[0].builtin, "Programs is stock");
        assert!(!cfg.categories[1].builtin, "Games is the user's");
    }

    #[test]
    fn a_category_entry_carries_its_file_types() {
        assert_eq!(parse_category_entry("Pictures"), ("Pictures", vec![]));
        assert_eq!(
            parse_category_entry("  My Games : .exe, ISO "),
            ("My Games", vec!["exe".into(), "iso".into()])
        );
        assert_eq!(parse_category_entry("Pictures:").1, Vec::<String>::new());
        assert!(!valid_category_name(parse_category_entry(":png").0));
    }

    #[test]
    fn an_extension_moves_to_the_category_that_claims_it() {
        let mut cats = default_categories();
        cats.push(CategoryDef::new("Pictures"));
        set_category_exts(&mut cats, "Pictures", parse_exts("png jpg iso"));
        // Programs listed iso; it belongs to Pictures now, and the rest of
        // the Programs list is untouched.
        let programs = cats.iter().find(|c| c.name == "Programs").unwrap();
        assert!(!programs.exts.contains(&"iso".to_string()));
        assert!(programs.exts.contains(&"exe".to_string()));
        assert_eq!(categorize("disk.iso", &cats).as_deref(), Some("Pictures"));
        assert_eq!(categorize("setup.exe", &cats).as_deref(), Some("Programs"));
    }

    #[test]
    fn a_new_file_type_files_the_next_download_with_it() {
        let mut cats = default_categories();
        let mut exts = cats
            .iter()
            .find(|c| c.name == "Programs")
            .unwrap()
            .exts
            .clone();
        exts.extend(parse_exts("msix msixbundle"));
        set_category_exts(&mut cats, "Programs", exts);
        assert_eq!(
            categorize("VSCode.msixbundle", &cats).as_deref(),
            Some("Programs")
        );
    }

    /// Order of the columns, as their ids, for readable assertions.
    fn order(cols: &[ColumnPref]) -> Vec<&'static str> {
        cols.iter().map(|p| p.id.id()).collect()
    }

    fn stock() -> Vec<ColumnPref> {
        Column::ALL.into_iter().map(ColumnPref::new).collect()
    }

    #[test]
    fn the_header_moves_a_column_past_the_one_next_to_it() {
        let mut cols = stock();
        assert!(move_column(&mut cols, Column::Size, true, true));
        assert_eq!(order(&cols)[..3], ["Name", "Size", "Queue"]);
    }

    /// A hidden column is not on screen, so moving left has to land where the
    /// user can see it land — one place left of where it was drawn.
    #[test]
    fn the_header_skips_a_hidden_column_the_user_cannot_see() {
        let mut cols = stock();
        cols[1].visible = false;
        assert!(move_column(&mut cols, Column::Size, true, true));
        assert_eq!(order(&cols)[..3], ["Size", "Queue", "Name"]);
    }

    /// The manage dialog lists every column, hidden ones included, so there
    /// the neighbour is simply the row above.
    #[test]
    fn the_dialog_moves_a_column_past_the_row_above_it() {
        let mut cols = stock();
        cols[1].visible = false;
        assert!(move_column(&mut cols, Column::Size, true, false));
        assert_eq!(order(&cols)[..3], ["Name", "Size", "Queue"]);
    }

    #[test]
    fn the_outermost_column_has_nowhere_left_to_go() {
        let mut cols = stock();
        assert!(!move_column(&mut cols, Column::Name, true, false));
        assert!(!move_column(&mut cols, Column::Description, false, false));
        assert_eq!(order(&cols), order(&stock()));
    }

    #[test]
    fn a_drag_past_half_the_neighbour_swaps_and_a_shorter_one_does_not() {
        let mut cols = stock();
        let half = Column::Queue.default_width() / 2.0;
        // Exactly half is the line itself: the column that swapped there
        // would immediately meet the swap-back threshold as well.
        assert_eq!(drag_column(&mut cols, Column::Name, 0.0, half), 0.0);
        assert_eq!(order(&cols), order(&stock()));

        let from = drag_column(&mut cols, Column::Name, 0.0, half + 1.0);
        assert_eq!(order(&cols)[..2], ["Queue", "Name"]);
        assert_eq!(from, Column::Queue.default_width());
    }

    /// The reference x moves with the column, so dragging back to where the
    /// press started leaves the order exactly as it was found — a pointer
    /// resting on the threshold must not flip the column back and forth.
    #[test]
    fn a_drag_returned_to_its_start_leaves_the_order_alone() {
        let mut cols = stock();
        let mut from = 0.0;
        for x in [30.0, 60.0, 200.0, 120.0, 40.0, 0.0] {
            from = drag_column(&mut cols, Column::Name, from, x);
        }
        assert_eq!(order(&cols), order(&stock()));
        assert_eq!(from, 0.0);
    }

    #[test]
    fn a_drag_carries_a_column_past_every_neighbour_it_covers() {
        let mut cols = stock();
        let far: f32 = cols.iter().map(|p| p.width).sum();
        drag_column(&mut cols, Column::Name, 0.0, far);
        assert_eq!(*order(&cols).last().unwrap(), "Name");
    }

    /// A drag only reorders what the header draws: a hidden column keeps its
    /// slot and is stepped over, not landed on.
    #[test]
    fn a_drag_steps_over_a_hidden_column() {
        let mut cols = stock();
        cols[1].visible = false;
        let past = Column::Size.default_width();
        // Past Size, which is what the header draws next to File Name here.
        drag_column(&mut cols, Column::Name, 0.0, past);
        assert_eq!(order(&cols)[..3], ["Size", "Queue", "Name"]);
    }

    #[test]
    fn widths_saved_before_columns_could_be_reordered_are_kept() {
        let mut s = Settings {
            column_widths: vec![420.0, 110.0, 111.0, 120.0, 130.0, 150.0, 175.0, 200.0],
            ..Settings::default()
        };
        migrate_columns(&mut s);
        assert_eq!(order(&s.columns), order(&stock()));
        assert_eq!(s.columns[0].width, 420.0);
        assert_eq!(s.columns[2].width, 111.0);
        // The Q column's old text-column default is not a width the user
        // chose: it would leave a wide empty band next to File Name.
        assert_eq!(s.columns[1].width, Column::Queue.default_width());
        assert!(s.columns.iter().all(|p| p.visible));
        assert!(
            s.column_widths.is_empty(),
            "the legacy key is folded in and dropped"
        );
    }

    /// The table draws whatever the list names, so a config from another
    /// version — or a hand-edited one — must not be able to leave a column
    /// unreachable, twice over, or wide enough to push the rest off screen.
    #[test]
    fn a_damaged_column_list_is_repaired_at_load() {
        let mut s = Settings {
            columns: vec![
                ColumnPref {
                    id: Column::Size,
                    width: 90.0,
                    visible: false,
                },
                ColumnPref {
                    id: Column::Size,
                    width: 500.0,
                    visible: true,
                },
                ColumnPref {
                    id: Column::Name,
                    width: 9_000.0,
                    visible: false,
                },
            ],
            ..Settings::default()
        };
        migrate_columns(&mut s);
        assert_eq!(s.columns.len(), Column::ALL.len());
        assert_eq!(order(&s.columns)[..2], ["Size", "Name"]);
        assert_eq!(s.columns[0].width, 90.0, "the repeat is dropped, not kept");
        assert_eq!(s.columns[1].width, COL_MAX_W);
        assert!(
            s.columns[1].visible,
            "File Name names the row and cannot be hidden"
        );
        for c in Column::ALL {
            assert_eq!(s.columns.iter().filter(|p| p.id == c).count(), 1, "{c:?}");
        }
    }

    #[test]
    fn a_fresh_config_starts_with_every_column_in_the_stock_order() {
        let mut s = Settings::default();
        migrate_columns(&mut s);
        assert_eq!(order(&s.columns), order(&stock()));
        assert!(s.columns.iter().all(|p| p.visible));
    }

    /// The reported bug: Options was open while the window was moved and
    /// a column hidden, and OK put both back. Only what the dialog changed
    /// may be written; what changed elsewhere meanwhile stays.
    #[test]
    fn accepting_the_options_draft_keeps_what_changed_outside_the_dialog() {
        let base = Settings::default();
        let mut draft = base.clone();
        draft.user_agent = "curl/8".into();
        draft.show_speed_tab = false;

        let mut live = base.clone();
        // Changed elsewhere while the dialog was up.
        live.window_pos = Some((10.0, 20.0));
        live.show_toolbar_labels = false;
        live.speed_limiter_on = true;
        live.global_speed_limit = Some(128 * 1024);
        live.bg_download = false;

        live.apply_options_draft(&base, &draft);

        assert_eq!(live.user_agent, "curl/8");
        assert!(!live.show_speed_tab);
        assert_eq!(live.window_pos, Some((10.0, 20.0)));
        assert!(!live.show_toolbar_labels);
        assert!(live.speed_limiter_on, "the toolbar's toggle survives");
        assert_eq!(live.global_speed_limit, Some(128 * 1024));
        assert!(!live.bg_download, "File Info's tick survives");
    }

    /// A field the dialog DID change wins over one changed elsewhere: the
    /// draft is what the user pressed OK on.
    #[test]
    fn a_field_edited_in_the_dialog_wins_over_a_toolbar_change() {
        let base = Settings::default();
        let mut draft = base.clone();
        draft.speed_limiter_on = true;
        draft.global_speed_limit = Some(50 * 1024);
        let mut live = base.clone();
        live.global_speed_limit = Some(128 * 1024);
        live.apply_options_draft(&base, &draft);
        assert!(live.speed_limiter_on);
        assert_eq!(live.global_speed_limit, Some(50 * 1024));
    }

    /// The Scheduler used to match the typed text against `%H:%M` exactly,
    /// so `9:00` never fired. Every sensible spelling of a minute is one.
    #[test]
    fn a_typed_time_is_read_as_a_minute_however_it_is_spelled() {
        assert_eq!(parse_hhmm("9:00"), Some((9, 0)));
        assert_eq!(parse_hhmm("21:0"), Some((21, 0)));
        assert_eq!(parse_hhmm("9.00"), Some((9, 0)));
        assert_eq!(parse_hhmm(" 09:05 "), Some((9, 5)));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("9:60"), None);
        assert_eq!(parse_hhmm("900"), None);
        assert_eq!(parse_hhmm(""), None);
        assert_eq!(normalize_hhmm("9:00"), "09:00");
        assert_eq!(normalize_hhmm("21:0"), "21:00");
        assert_eq!(normalize_hhmm("9.00"), "09:00");
        assert_eq!(
            normalize_hhmm("nine"),
            "nine",
            "what cannot be read is left to be seen"
        );
    }

    /// A shortcut is stored in the one spelling a key press produces, or
    /// refused: modifier order, aliases, and the command modifier that
    /// every editable combo has to carry.
    #[test]
    fn a_typed_shortcut_normalizes_or_is_refused() {
        let p = primary_modifier();
        assert_eq!(
            normalize_combo("cmd+shift+v").as_deref(),
            Some(format!("{p}+shift+v").as_str())
        );
        assert_eq!(
            normalize_combo("Shift+Cmd+V").as_deref(),
            Some(format!("{p}+shift+v").as_str())
        );
        assert_eq!(
            normalize_combo("ctrl+alt+shift+r").as_deref(),
            Some(format!("{p}+shift+alt+r").as_str())
        );
        assert_eq!(
            normalize_combo("win+shift+v").as_deref(),
            Some(format!("{p}+shift+v").as_str())
        );
        assert_eq!(
            normalize_combo("atl+ctrl+r").as_deref(),
            Some(format!("{p}+alt+r").as_str())
        );
        assert_eq!(
            normalize_combo(" CMD + , ").as_deref(),
            Some(format!("{p}+,").as_str())
        );
        assert_eq!(normalize_combo("shift+v"), None, "no command modifier");
        assert_eq!(normalize_combo("cmd+F5"), None, "not a character");
        assert_eq!(normalize_combo("cmd+hyper+v"), None);
        assert_eq!(normalize_combo(""), None);
        for (_, combo, _) in SHORTCUT_ACTIONS {
            assert_eq!(
                normalize_combo(combo).as_deref(),
                Some(platform_default(combo).as_str())
            );
        }
    }

    #[test]
    fn shortcut_autocomplete_expands_modifiers_and_fixes_typos() {
        assert_eq!(autocomplete_combo("atl", "at"), "alt+");
        assert_eq!(autocomplete_combo("alt", "al"), "alt+");
        assert_eq!(autocomplete_combo("ctrl", "ctr"), "ctrl+");
        assert_eq!(autocomplete_combo("control", "contro"), "ctrl+");
        assert_eq!(autocomplete_combo("ctl", "ct"), "ctrl+");
        assert_eq!(autocomplete_combo("win", "wi"), "win+");
        assert_eq!(autocomplete_combo("windows", "window"), "win+");
        assert_eq!(autocomplete_combo("cmd", "cm"), "cmd+");
        assert_eq!(autocomplete_combo("command", "comman"), "cmd+");
        assert_eq!(autocomplete_combo("shift", "shif"), "shift+");
        assert_eq!(autocomplete_combo("shft", "shf"), "shift+");
        assert_eq!(autocomplete_combo("option", "optio"), "alt+");
        assert_eq!(autocomplete_combo("opt", "op"), "alt+");
        assert_eq!(autocomplete_combo("ctrl+atl", "ctrl+at"), "ctrl+alt+");
        assert_eq!(autocomplete_combo("ctrl a", "ctrl "), "ctrl+a");
        assert_eq!(
            autocomplete_combo("ctrl-shift-v", "ctrl-shift-"),
            "ctrl+shift+v"
        );
        assert_eq!(
            autocomplete_combo("ctrl", "ctrl+"),
            "ctrl",
            "backspace does not re-add plus"
        );
        assert_eq!(
            autocomplete_combo("ctr", "ctrl"),
            "ctr",
            "backspace preserves deletion"
        );
    }

    /// Queues are named, and a name must be free. `len() + 1` handed out
    /// "Queue 3" again after "Queue 2" was deleted from the middle.
    #[test]
    fn a_new_queue_takes_the_smallest_free_number() {
        let q = |name: &str| QueueDef {
            name: name.into(),
            files_at_once: 4,
            schedule: Schedule::default(),
            builtin: false,
            color: None,
            running: false,
            did_work: false,
        };
        assert_eq!(free_queue_name(&[], "Queue"), "Queue 1");
        let queues = [q("Main download queue"), q("Queue 1"), q("Queue 3")];
        assert_eq!(free_queue_name(&queues, "Queue"), "Queue 2");
        let queues = [q("Queue 1"), q("Queue 2")];
        assert_eq!(free_queue_name(&queues, "Queue"), "Queue 3");
    }

    fn test_download(state: DlState, size: Option<u64>, downloaded: u64) -> DownloadItem {
        DownloadItem {
            id: 1,
            url: "https://example.com/test.zip".into(),
            file_name: "test.zip".into(),
            save_dir: "/tmp".into(),
            category: None,
            description: String::new(),
            size,
            downloaded,
            state,
            error: None,
            resume: None,
            added: 0,
            last_try: None,
            queue: None,
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
        }
    }

    #[test]
    fn paused_status_text_shows_percentage_when_size_is_known() {
        let item = test_download(DlState::Paused, Some(10_000), 1034);
        assert_eq!(item.status_text(), "Paused (10.34%)");

        let zero_pct = test_download(DlState::Paused, Some(1000), 0);
        assert_eq!(zero_pct.status_text(), "Paused (0.00%)");

        let full_pct = test_download(DlState::Paused, Some(1000), 1000);
        assert_eq!(full_pct.status_text(), "Paused (100.00%)");
    }

    #[test]
    fn paused_status_text_omits_percentage_when_size_is_unknown_or_zero() {
        let no_size = test_download(DlState::Paused, None, 500);
        assert_eq!(no_size.status_text(), "Paused");

        let zero_size = test_download(DlState::Paused, Some(0), 0);
        assert_eq!(zero_size.status_text(), "Paused");
    }

    #[test]
    fn status_text_preserves_other_states() {
        assert_eq!(
            test_download(DlState::Complete, Some(1000), 1000).status_text(),
            "Complete"
        );
        assert_eq!(
            test_download(DlState::Queued, Some(1000), 0).status_text(),
            "Queued"
        );
        assert_eq!(
            test_download(DlState::Error, Some(1000), 500).status_text(),
            "Error"
        );
        assert_eq!(
            test_download(DlState::Connecting, Some(1000), 0).status_text(),
            "Connecting..."
        );
        assert_eq!(
            test_download(DlState::Receiving, Some(10_000), 354).status_text(),
            "3.54%"
        );
        assert_eq!(
            test_download(DlState::Receiving, None, 1000).status_text(),
            "Receiving..."
        );
    }
}

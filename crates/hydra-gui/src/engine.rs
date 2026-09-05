// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The download engine bridge: GUI on one side, `hya-core`/`hya-net` on the
//! other.
//!
//! One background thread runs a tokio runtime that owns every transfer. The
//! GUI talks to it through a command channel and hears back through an event
//! channel that an iced subscription drains, so the widget tree never blocks
//! on a socket and the engine never touches a widget.
//!
//! Transfer strategy follows what the CLI settled on:
//! range-capable origins get the `hya-core` scheduler with N connections
//! (default 8) and the stall-timeout clamp measured there; servers
//! without ranges fall back to one streaming GET. Pause/cancel go through
//! `run_transfer_cancellable`'s stop flag so sockets actually close, and the
//! received spans are re-`mark_done`d on resume — including across app restarts.

use crate::model::{ConnRow, DlId};
use hya_core::{Capability, Scheduler, Source};
use hya_net::polite::RateLimiter;
use hya_net::{probe_resilient, Probe, SparseSink, Target, TlsCapableConnector};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

// ------------------------------------------------------------------ protocol

#[derive(Clone, Debug)]
pub struct StartSpec {
    pub id: DlId,
    pub url: String,
    pub auth: Option<(String, String)>,
    pub conns: usize,
    pub user_agent: String,
    pub temp_path: String,
    pub final_path: String,
    /// Spans already on disk from a previous run; resumed via `mark_done`.
    pub held: Vec<(u64, u64)>,
    /// Size the previous source reported. A mirror serving a different size
    /// is a different object: held spans are dropped instead of spliced.
    pub expected_size: Option<u64>,
    /// `Cookie:` header value, verbatim.
    pub cookies: Option<String>,
    /// Aggregate cap in bytes/sec; `None` = unlimited (can be changed live).
    pub limit: Option<u64>,
    /// Start at one connection and let the in-band ramp admit more while
    /// they pay for themselves; `conns` becomes a ceiling, not a target.
    pub adaptive: bool,
    /// Stamp the finished file with the date the server reported for the
    /// object, instead of the time the bytes happened to land on disk.
    ///
    /// Options > "Set file creation date as provided by the server", and the
    /// CLI's `--remote-time`. Only `Last-Modified` can answer this: an ETag is
    /// opaque, so a server that sends no date leaves the file's own time
    /// alone rather than getting a fabricated one.
    pub remote_time: bool,
    /// Every mirror a Metalink document named, best first. Empty for an
    /// ordinary one-URL download, which is what every other caller passes.
    ///
    /// `url` stays the primary and is still probed first, so a failure to reach
    /// a mirror is reported against the same address the item shows.
    pub mirrors: Vec<crate::model::MirrorRef>,
    /// The size the document attested.
    ///
    /// Admission for a mirror is normally "same size AND the same strong
    /// validator as the first source", and that test is unsatisfiable across a
    /// real mirror list: independent operators cannot share an `ETag`. A size
    /// published by whoever built the object replaces it, and the digest below
    /// is what actually catches a mirror serving something else.
    pub attested_size: Option<u64>,
    /// The document's strongest digest, as `algorithm:hex`.
    pub attested_digest: Option<String>,
    /// The document's `<pieces>` chunk manifest, in its on-disk JSON form.
    pub pieces: Option<String>,
}

impl StartSpec {
    /// The mirror-list fields of an ORDINARY download: none of them.
    ///
    /// Spelled as a constructor rather than a `Default` on the whole struct
    /// because the rest of `StartSpec` has no sensible default — an id of 0 and
    /// an empty output path are not a download, and a `..Default::default()`
    /// that silently supplied them would turn a forgotten field into a runtime
    /// mystery instead of a compile error.
    ///
    /// Test-only for exactly that reason: the real call sites in `app.rs` spell
    /// every field out, so adding one to `StartSpec` fails to compile there
    /// instead of silently defaulting.
    #[cfg(test)]
    pub fn plain() -> Self {
        StartSpec {
            id: 0,
            url: String::new(),
            auth: None,
            conns: 1,
            user_agent: String::new(),
            temp_path: String::new(),
            final_path: String::new(),
            held: Vec::new(),
            expected_size: None,
            cookies: None,
            limit: None,
            adaptive: false,
            remote_time: false,
            mirrors: Vec::new(),
            attested_size: None,
            attested_digest: None,
            pieces: None,
        }
    }
}

/// An adaptive stream to assemble. Deliberately NOT a `StartSpec`: there is
/// no single object to range over, no size the server will state up front,
/// and no resume story yet — what it shares with a download is the id, the
/// destination and the event stream, which is all the GUI needs to show it
/// in the same list.
#[derive(Clone, Debug)]
pub struct StreamSpec {
    pub id: DlId,
    /// The manifest as the browser saw it.
    pub manifest: String,
    /// "hls" or "dash", as the extension read it. Only a tiebreak: what the
    /// manifest body actually is wins.
    pub protocol: String,
    /// The exact variant playlist the user picked, when they picked one.
    pub variant_url: Option<String>,
    /// Fallbacks for choosing a variant when `variant_url` is stale.
    pub height: Option<u32>,
    pub bandwidth: Option<u64>,
    /// "MP4" or "TS": what the user asked the finished file to be.
    pub container: String,
    pub cookies: Option<String>,
    pub referer: Option<String>,
    pub user_agent: String,
    pub temp_path: String,
    pub final_path: String,
    /// Segments in flight. 0 takes the library's default.
    ///
    /// Fixed, deliberately: the app-wide "adaptive connections" setting is
    /// for ranged file downloads and is NOT applied here. Admission was
    /// tried on the segment pipeline and measured slower — a stream's
    /// ceiling is already small, so ramping up to it can only arrive at the
    /// same place later. See `hya_stream::hls::Concurrency`.
    pub conns: usize,
    /// Stop a LIVE recording after this many seconds and finish the file.
    /// A live stream has no end of its own, so without this the only way to
    /// get a file is to sit and press Stop.
    pub max_seconds: Option<u64>,
    /// Aggregate cap in bytes/sec; `None` = unlimited. Carried on the spec
    /// for the same reason `StartSpec` carries it: a limit configured BEFORE
    /// the transfer starts has to apply from the first segment, not only
    /// once someone opens the Speed Limiter tab and triggers `SetLimit`.
    pub limit: Option<u64>,
}

/// What a manifest offers, read before anything is downloaded.
#[derive(Clone, Debug, Default)]
pub struct StreamProbe {
    /// "hls" or "dash".
    pub protocol: String,
    pub live: bool,
    /// Seconds, when the manifest states one.
    pub duration: Option<f64>,
    pub qualities: Vec<StreamQuality>,
    /// DASH keeps audio in its own Representation; HLS usually does not.
    pub separate_audio: bool,
    /// Set when the manifest names a DRM system, which is a refusal.
    pub drm: Option<String>,
    /// MPEG-TS segments can be saved as `.ts` without ffmpeg; fragmented
    /// MP4 cannot, so the container choice depends on this.
    pub is_ts: bool,
}

/// One rendition the user can pick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamQuality {
    pub label: String,
    pub height: Option<u32>,
    pub bandwidth: Option<u64>,
    /// The variant playlist, for HLS.
    pub url: Option<String>,
}

impl std::fmt::Display for StreamQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug)]
pub enum Cmd {
    Start(Box<StartSpec>),
    /// Assemble an HLS stream into one file (see [`StreamSpec`]).
    StartStream(Box<StreamSpec>),
    /// Pause and Cancel are one engine operation (stop the sockets, report the
    /// snapshot); the GUI decides whether the item is "Paused" or removed.
    Stop(DlId),
    SetLimit(DlId, Option<u64>),
    /// Re-aim where the finished file lands. The File Info dialog edits the
    /// name/directory while the transfer is already running in the
    /// background; the new destination takes effect at completion.
    SetFinalPath(DlId, String),
    /// Internal: a transfer task ended (any outcome); drop its live handles.
    /// Without this, entries for normally-finished transfers stayed in the
    /// live map for the process lifetime. Carries the task's own cancel flag
    /// so it can only evict ITS entry — after a quick pause/resume the map
    /// already holds the successor transfer under the same id.
    Done(DlId, Arc<AtomicBool>),
}

#[derive(Clone, Debug)]
pub enum Event {
    Probed {
        id: DlId,
        size: Option<u64>,
        ranges: bool,
        file_name: Option<String>,
    },
    Status {
        id: DlId,
        line: String,
    },
    Progress {
        id: DlId,
        done: u64,
        rate: f64,
        eta: Option<u64>,
        conns: Vec<ConnRow>,
        held: Vec<(u64, u64)>,
        /// Seconds of MEDIA captured so far. Only a live recording sets it:
        /// it has no size to show a percentage against, so how much time is
        /// on disk is the only honest measure of progress.
        recorded: Option<f64>,
    },
    Finished {
        id: DlId,
        elapsed: f64,
        size: u64,
    },
    /// Pause/cancel acknowledged; final byte snapshot for persistence.
    Stopped {
        id: DlId,
        done: u64,
        held: Vec<(u64, u64)>,
    },
    Failed {
        id: DlId,
        error: String,
        done: u64,
        held: Vec<(u64, u64)>,
        /// The OS refused filesystem access (TCC/permissions): the GUI offers
        /// the system folder picker, whose selection implicitly grants access.
        permission_denied: bool,
    },
}

// ------------------------------------------------------------------ handles

struct Live {
    cancel: Arc<AtomicBool>,
    limiter: Arc<RateLimiter>,
    final_path: Arc<Mutex<String>>,
}

static POWER_SAVE: AtomicBool = AtomicBool::new(false);

/// Coarser scheduler ticks and UI-event cadence; applies to transfers
/// started after the switch (running ones keep their tick, their emit rate
/// adapts live).
pub fn set_power_save(on: bool) {
    POWER_SAVE.store(on, Ordering::Relaxed);
}

/// What a connection row says while the transport keeps it idle on purpose.
///
/// A row that reads "waiting" or "Disconnect." after the adaptive search has
/// settled looks like a dropped connection and invites the fix that makes it
/// worse: raising the connection count. The label has to say that a decision
/// was made, and whose.
fn dormant_label(reason: hya_core::LimitReason) -> String {
    use hya_core::LimitReason as R;
    match reason {
        R::Measuring => crate::i18n::tr("Waiting (measuring)..."),
        R::Measured { .. } => crate::i18n::tr("Not used (measured slower)"),
        R::Refused { .. } | R::Starved { .. } => crate::i18n::tr("Not used (server limit)"),
        R::None => crate::i18n::tr("Waiting (connection limit)..."),
    }
}

/// One sentence for the status line and the log when the transport lowers the
/// connection count, or `None` when there is nothing to announce.
fn describe_limit(reason: hya_core::LimitReason, budget: usize) -> Option<String> {
    use hya_core::LimitReason as R;
    match reason {
        R::Measured {
            chosen,
            chosen_rate,
            tried,
            tried_rate,
            // A rate of zero means that level was never measured directly, which
            // a refusal can leave behind. Reporting "against 1 at 0 B/s" states a
            // measurement that was never taken.
        } if tried != chosen && tried_rate > 0.0 && chosen_rate > 0.0 => Some(
            crate::i18n::tr(
                "Measured {tried} connections at {tried_rate} against {chosen} at \
                 {chosen_rate}: using {n}",
            )
            .replace("{tried}", &tried.to_string())
            .replace("{tried_rate}", &crate::fmt::rate(tried_rate))
            .replace("{chosen}", &chosen.to_string())
            .replace("{chosen_rate}", &crate::fmt::rate(chosen_rate))
            .replace("{n}", &chosen.to_string()),
        ),
        R::Measured { chosen, .. } if chosen < budget => Some(
            crate::i18n::tr("Measured: {n} of {budget} connections pay for themselves")
                .replace("{n}", &chosen.to_string())
                .replace("{budget}", &budget.to_string()),
        ),
        R::Refused { serving } | R::Starved { serving } if serving < budget => Some(
            crate::i18n::tr("Server serves {n} connection(s) at once: using {n}")
                .replace("{n}", &serving.to_string()),
        ),
        _ => None,
    }
}

fn emit_interval_ms() -> u128 {
    if POWER_SAVE.load(Ordering::Relaxed) {
        500
    } else {
        100
    }
}

static CMDS: OnceLock<UnboundedSender<Cmd>> = OnceLock::new();
static EVENTS: Mutex<Option<UnboundedReceiver<Event>>> = Mutex::new(None);

/// Send a command to the engine (started lazily on first use).
pub fn send(cmd: Cmd) {
    let tx = CMDS.get_or_init(spawn_engine);
    let _ = tx.send(cmd);
}

/// Start the engine thread without sending anything, so the event receiver
/// exists before the first subscription poll.
pub fn ensure_started() {
    let _ = CMDS.get_or_init(spawn_engine);
}

/// The subscription side: take the event receiver (once).
pub fn take_events() -> Option<UnboundedReceiver<Event>> {
    EVENTS.lock().ok().and_then(|mut g| g.take())
}

fn spawn_engine() -> UnboundedSender<Cmd> {
    let (cmd_tx, mut cmd_rx) = unbounded_channel::<Cmd>();
    let (ev_tx, ev_rx) = unbounded_channel::<Event>();
    *EVENTS.lock().unwrap() = Some(ev_rx);

    // Transfer tasks report their own completion back through the command
    // channel so the live map sheds finished entries.
    let cmd_tx2 = cmd_tx.clone();
    std::thread::Builder::new()
        .name("hydra-engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("engine runtime");
            rt.block_on(async move {
                let mut live: HashMap<DlId, Live> = HashMap::new();
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        Cmd::Start(spec) => {
                            let cancel = Arc::new(AtomicBool::new(false));
                            // 0 = unlimited. The limiter is handed to the
                            // transfer either way: `Pace` reads its rate live,
                            // so Speed Limiter switched on mid-download binds
                            // this transfer without restarting it.
                            let limiter = Arc::new(RateLimiter::new(spec.limit.unwrap_or(0)));
                            let final_path = Arc::new(Mutex::new(spec.final_path.clone()));
                            live.insert(
                                spec.id,
                                Live {
                                    cancel: cancel.clone(),
                                    limiter: limiter.clone(),
                                    final_path: final_path.clone(),
                                },
                            );
                            let tx = ev_tx.clone();
                            let done_tx = cmd_tx2.clone();
                            crate::log::log(&format!(
                                "start #{} {}",
                                spec.id,
                                crate::log::redact(&spec.url)
                            ));
                            tokio::spawn(async move {
                                let id = spec.id;
                                let flag = cancel.clone();
                                run_download(*spec, cancel, limiter, final_path, tx).await;
                                let _ = done_tx.send(Cmd::Done(id, flag));
                            });
                        }
                        Cmd::StartStream(spec) => {
                            let cancel = Arc::new(AtomicBool::new(false));
                            let limiter = Arc::new(RateLimiter::new(spec.limit.unwrap_or(0)));
                            let final_path = Arc::new(Mutex::new(spec.final_path.clone()));
                            let pace_limiter = limiter.clone();
                            live.insert(
                                spec.id,
                                Live {
                                    cancel: cancel.clone(),
                                    limiter,
                                    final_path: final_path.clone(),
                                },
                            );
                            let tx = ev_tx.clone();
                            let done_tx = cmd_tx2.clone();
                            crate::log::log(&format!(
                                "start stream #{} {}",
                                spec.id, spec.manifest
                            ));
                            tokio::spawn(async move {
                                let id = spec.id;
                                let flag = cancel.clone();
                                run_stream(*spec, cancel, pace_limiter, final_path, tx).await;
                                let _ = done_tx.send(Cmd::Done(id, flag));
                            });
                        }
                        Cmd::Stop(id) => {
                            if let Some(l) = live.get(&id) {
                                l.cancel.store(true, Ordering::Relaxed);
                            }
                            crate::log::log(&format!("stop #{id}"));
                        }
                        Cmd::SetLimit(id, limit) => {
                            crate::log::debug(&format!("#{id} limit -> {limit:?}"));
                            if let Some(l) = live.get(&id) {
                                l.limiter.set_rate(limit.unwrap_or(0));
                            }
                        }
                        Cmd::SetFinalPath(id, path) => {
                            crate::log::debug(&format!("#{id} final path -> {path}"));
                            if let Some(l) = live.get(&id) {
                                if let Ok(mut g) = l.final_path.lock() {
                                    *g = path;
                                }
                            }
                        }
                        Cmd::Done(id, flag) => {
                            if live
                                .get(&id)
                                .map(|l| Arc::ptr_eq(&l.cancel, &flag))
                                .unwrap_or(false)
                            {
                                live.remove(&id);
                            }
                        }
                    }
                    live.retain(|_, l| !l.cancel.load(Ordering::Relaxed));
                }
            });
        })
        .expect("engine thread");
    cmd_tx
}

/// The one connector for the whole process. Its connection pool, TLS
/// session cache (`Resumption::in_memory_sessions`) and parsed root store
/// are designed to outlive a single transfer — `tls.rs` documents 1.6–2.0 s
/// of setup reused when the probe's handshake feeds the transfer. Building
/// a connector per transfer (as this file used to) discarded all three.
fn shared_connector() -> Result<Arc<TlsCapableConnector>, String> {
    static CONNECTOR: OnceLock<Result<Arc<TlsCapableConnector>, String>> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            TlsCapableConnector::new()
                .map(Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
}

/// What a link turns out to be, once its redirects have been resolved.
#[derive(Clone, Debug)]
pub struct LinkMeta {
    /// `None` when the server would not state one.
    pub size: Option<u64>,
    /// The name the object should be saved under: `Content-Disposition` if the
    /// server named one, else the last path segment of the FINAL URL.
    pub file_name: String,
    /// The URL serves a Metalink DOCUMENT rather than the object.
    ///
    /// Read from `Content-Type` on the probe that had to happen anyway, which
    /// is the only place the answer is free — and the only place it exists at
    /// all for a redirector like `.../metalink?repo=x`, whose name reveals
    /// nothing. A batch that misses this adds a few kilobytes of XML as a
    /// download and calls it done.
    pub is_metalink: bool,
}

/// Probe a link for the batch dialog's File Name and Size columns.
///
/// Resolves redirects first, including the ones expressed in HTML (see
/// [`hya_net::redirect`]): a referrer stripper such as `href.li/?<url>` has no
/// filename in its own path, and reading the column off the pasted URL listed
/// every such link as `index.html` with no size.
///
/// Bounded: a pasted 500-link batch must not open 500 concurrent
/// handshakes (fd limits, origin rate limiting). A small global gate keeps
/// the fan-out polite; the shared connector reuses connections per host.
pub async fn probe_link(url: String, user_agent: String) -> Option<LinkMeta> {
    static GATE: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let gate = GATE
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(6)))
        .clone();
    let _permit = gate.acquire_owned().await.ok()?;
    let connector = shared_connector().ok()?;
    let (url, p) = resolve_link(connector.as_ref(), url, &user_agent, &[]).await?;
    if p.status >= 300 {
        return None;
    }
    Some(LinkMeta {
        size: (p.size > 0).then_some(p.size),
        file_name: p
            .suggested_filename()
            .unwrap_or_else(|| file_name_from_url(&url)),
        is_metalink: p.serves_metalink(),
    })
}

/// Follow a link to the object it names: HTTP redirects and the HTML kind
/// alike (see [`hya_net::redirect`]), up to ten hops. Returns the final URL
/// with its probe, so the caller sees the object's own headers rather than
/// the redirector's. `None` when a hop cannot be parsed or answered.
async fn resolve_link(
    connector: &TlsCapableConnector,
    mut url: String,
    user_agent: &str,
    headers: &[String],
) -> Option<(String, Probe)> {
    for _ in 0..10 {
        let u = parse_url(&url).ok()?;
        let t = target_of(&u, headers.to_vec(), user_agent);
        let p = probe_resilient(connector, &t).await.ok()?;
        if p.is_redirect() {
            url = join_url(&u, p.location.as_deref().unwrap_or(""))?;
            continue;
        }
        if p.maybe_redirector() {
            if let Some(next) = hya_net::html_redirect(connector, &t)
                .await
                .and_then(|loc| join_url(&u, &loc))
            {
                url = next;
                continue;
            }
        }
        return Some((url, p));
    }
    None
}

/// A direct target for `u` carrying the request headers and user agent.
fn target_of(u: &ParsedUrl, headers: Vec<String>, user_agent: &str) -> Target {
    let base = if u.tls {
        Target::direct_tls(&u.host, u.port, &u.path)
    } else {
        Target::direct(&u.host, u.port, &u.path)
    };
    base.with_headers(headers, Some(user_agent.to_string()))
}

// ---------------------------------------------------------------- zip peek

/// List what is inside a remote ZIP archive without downloading it.
///
/// ZIP's index lives at the end of the file, so one ranged GET for the tail
/// (and, for an archive with thousands of entries, one more for the index
/// itself) is the whole cost — see [`hya_net::zipdir`]. `headers` are the
/// download's own login and cookies, so the peek sees the same object the
/// transfer would.
///
/// `known_size` is the size the dialog already has from its probe or from
/// the transfer running behind it. With it, no probe is made at all: the
/// ranged GET goes straight out and follows redirects itself. That is two
/// fewer round trips — two fewer TLS handshakes to a CDN — and on a slow
/// path the difference between the list appearing in one second or five.
///
/// Errors are sentences for the dialog, already translated.
pub async fn peek_zip(
    url: String,
    user_agent: String,
    headers: Vec<String>,
    known_size: Option<u64>,
) -> Result<Vec<hya_net::zipdir::Entry>, String> {
    use crate::i18n::tr;
    use hya_net::zipdir;

    let connector = shared_connector()?;
    let c = connector.as_ref();
    let (mut url, total) = match known_size {
        Some(n) if n > 0 => (url, n),
        _ => {
            let (url, p) = resolve_link(c, url, &user_agent, &headers)
                .await
                .ok_or_else(|| tr("The server did not answer."))?;
            if p.status >= 300 {
                return Err(hya_net::describe_status(p.status));
            }
            if p.size == 0 {
                return Err(tr("The server did not state the file's size."));
            }
            (url, p.size)
        }
    };

    // The listing itself is hya-net's. With a size in hand the first request
    // is the ranged GET, so a redirect surfaces there: follow it and retry.
    for _ in 0..10 {
        let u = parse_url(&url)?;
        let t = target_of(&u, headers.clone(), &user_agent);
        return match zipdir::fetch_listing(c, &t, total).await {
            Ok(entries) => Ok(entries),
            Err(zipdir::PeekError::Net(e)) => match hya_net::Redirect::of(&e) {
                Some(r) => match join_url(&u, &r.location) {
                    Some(next) => {
                        url = next;
                        continue;
                    }
                    None => Err(e.to_string()),
                },
                None => Err(e.to_string()),
            },
            Err(zipdir::PeekError::NoRanges) => Err(tr(
                "The server does not support partial downloads, so the archive cannot be previewed without downloading it.",
            )),
            Err(zipdir::PeekError::Zip(zipdir::Error::NotZip)) => {
                Err(tr("This file is not a ZIP archive."))
            }
            Err(zipdir::PeekError::Zip(zipdir::Error::Corrupt(_))) => {
                Err(tr("The archive's index is damaged."))
            }
            Err(zipdir::PeekError::IndexTooLarge) => {
                Err(tr("The archive's index is too large to preview."))
            }
        };
    }
    Err(tr("The server did not answer."))
}

// ------------------------------------------------------------------ metalink

/// One file a Metalink document describes, resolved into what an item needs.
#[derive(Clone, Debug)]
pub struct MetalinkChoice {
    /// The document's `name`, as a safe relative path.
    pub name: String,
    pub info: crate::model::MetalinkInfo,
    /// The mirror the item's own `url` should be, i.e. the publisher's first
    /// choice. The item shows one address; the rest are its mirrors.
    pub primary: String,
    /// Piece count, for the dialog. Zero when the document published none, or
    /// when the pieces do not tile the stated size and were therefore dropped.
    pub piece_count: usize,
    /// Mirrors listed against mirrors this build can fetch from. A list that
    /// silently loses two thirds of its entries to `rsync://` is worth seeing
    /// before a multi-gigabyte download rather than after.
    pub mirrors_listed: usize,
}

/// What an address turned out to be, when it is a Metalink document.
#[derive(Clone, Debug)]
pub struct MetalinkProbe {
    /// "3.0" or "4 (RFC 5854)".
    pub version: String,
    pub origin: String,
    pub files: Vec<MetalinkChoice>,
}

/// Does this address name a Metalink document by its NAME alone?
///
/// The cheap half of detection, and the only half available for a remote URL
/// before it is fetched. A URL with no usable extension
/// (`.../metalink?repo=fedora-40`) is settled by `Content-Type` inside
/// [`probe_metalink`], where the fetch has to happen anyway.
pub fn metalink_address(addr: &str) -> bool {
    let a = addr.trim();
    if a.is_empty() {
        return false;
    }
    if hya_net::metalink::is_metalink_filename(a.split(['?', '#']).next().unwrap_or(a)) {
        return true;
    }
    // A local file whose CONTENT says so. Free to check, and necessary: a
    // document saved by a browser is as likely to be called `metalink.xml` or
    // `download(1)` as anything else.
    if a.contains("://") {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(a) else {
        return false;
    };
    use std::io::Read as _;
    let mut head = [0u8; 4096];
    match f.read(&mut head) {
        Ok(n) if n > 0 => hya_net::metalink::looks_like_metalink(&head[..n]),
        _ => false,
    }
}

/// Read a Metalink document — a local path or a URL — and resolve every file
/// entry it describes into something the download list can hold.
///
/// Refuses the two cases where continuing would produce a wrong file quietly: a
/// `name` that escapes the destination directory (RFC 5854 §4.1.2.1, including
/// the Windows spellings a POSIX-only check misses), and an entry with no mirror
/// this build has a transport for.
pub async fn probe_metalink(source: String, user_agent: String) -> Result<MetalinkProbe, String> {
    let doc = if source.contains("://") {
        fetch_metalink(&source, &user_agent).await?
    } else {
        let text = std::fs::read_to_string(&source)
            .map_err(|e| format!("{}: {e}", crate::i18n::tr("Cannot read the mirror list")))?;
        hya_net::metalink::parse(&text).map_err(|e| e.to_string())?
    };
    let mut files = Vec::new();
    for f in &doc.files {
        let Ok(name) = f.safe_name() else {
            crate::log::warn(&format!(
                "metalink: refusing entry {:?}: the name escapes the destination directory",
                f.name
            ));
            continue;
        };
        let mut mirrors: Vec<&hya_net::MetaUrl> = f.fetchable_urls();
        // Transport first, the publisher's ranking within it — and then ONE
        // transport per item. An FTP mirror at rank 1 would not merely lead:
        // the engine routes on the item's own URL, so it would drop the whole
        // transfer to a single FTP stream and silently abandon the list. And a
        // trailing ftp mirror in a mixed list is no reserve either — the
        // multi-source engine probes and substitutes over HTTP targets, so it
        // would be a request sent to port 21. The leading tier carries the
        // item; an all-ftp document keeps its ftp mirrors and streams, as
        // before. See `UrlKind::transport_tier`.
        mirrors.sort_by_key(|u| (u.kind.transport_tier(), u.priority, u.url.clone()));
        if let Some(lead) = mirrors.first().map(|u| u.kind.transport_tier()) {
            mirrors.retain(|u| u.kind.transport_tier() == lead);
        }
        if mirrors.is_empty() {
            crate::log::warn(&format!(
                "metalink: {name} lists {} mirror(s), none on a scheme this build can fetch",
                f.urls.len()
            ));
            continue;
        }
        // The document's grid, not a convenient one: a digest is a function of
        // an exact byte span, so any other grid verifies nothing. Dropped
        // entirely when it does not tile the stated size, because applying it
        // anyway would report every chunk as corrupt.
        //
        // Capped by serialized size because this string is PERSISTED: it rides
        // in `MetalinkInfo` inside the GUI state, which is rewritten on every
        // state change for the life of the item. The parser admits up to a
        // million pieces, which serialize to ~65 MB — a document (a hostile
        // one, or merely a huge object on a tiny grid) could make every state
        // save write that. Four MiB covers every real distribution image (a
        // 4 GB ISO at Fedora's 256 KiB grid is ~1 MB); past it the piece list
        // is dropped with a log line and the whole-file digest still verifies
        // the object — coarser, never weaker.
        const PIECES_PERSIST_CAP: usize = 4 << 20;
        let pieces = hya_net::manifest::from_metalink(f).ok();
        let piece_count = pieces.as_ref().map(|m| m.chunks.digests.len()).unwrap_or(0);
        files.push(MetalinkChoice {
            name: name.to_string(),
            primary: mirrors[0].url.clone(),
            piece_count,
            mirrors_listed: f.urls.len(),
            info: crate::model::MetalinkInfo {
                // Dense ranks from 1: the document's own numbers run on two
                // different scales depending on dialect, and one field reaches
                // the allocator.
                mirrors: mirrors
                    .iter()
                    .enumerate()
                    .map(|(i, u)| crate::model::MirrorRef {
                        url: u.url.clone(),
                        priority: (i + 1) as u32,
                        max_connections: u.max_connections,
                    })
                    .collect(),
                size: f.size,
                digest: f.best_hash().map(|h| h.spec()),
                pieces: pieces.map(|m| m.to_json()).filter(|j| {
                    let keep = j.len() <= PIECES_PERSIST_CAP;
                    if !keep {
                        crate::log::warn(&format!(
                            "metalink: {name}: the piece list serializes to {} bytes; \
                             dropping it and verifying by whole-file digest instead",
                            j.len()
                        ));
                    }
                    keep
                }),
                origin: source.clone(),
                signed: f.signature.is_some(),
            },
        });
    }
    if files.is_empty() {
        return Err(crate::i18n::tr(
            "The mirror list describes no file this build can download.",
        ));
    }
    Ok(MetalinkProbe {
        version: doc
            .version
            .map(|v| v.as_str().to_string())
            .unwrap_or_else(|| "?".into()),
        origin: source,
        files,
    })
}

/// Point a running job at the sources a document turned out to describe.
///
/// A job fetches ONE object, so a document describing several is narrowed to a
/// single entry here. Which one is not arbitrary: the job's existing filename is
/// tried first, so a row the user already named — from the Add URL dialog, or
/// from a batch whose document resolved before the transfer started — keeps
/// pointing at the file they picked. Only a job that arrived with nothing but a
/// redirector URL falls back to the first entry, and that is logged, because the
/// user typed one address and is about to receive a differently named file.
fn adopt_metalink(
    doc: &hya_net::Metalink,
    spec: &mut StartSpec,
    id: DlId,
) -> Result<(String, Option<u64>), String> {
    let wanted = std::path::Path::new(&spec.final_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let usable: Vec<(&hya_net::MetalinkFile, &str)> = doc
        .files
        .iter()
        .filter_map(|f| f.safe_name().ok().map(|n| (f, n)))
        .filter(|(f, _)| !f.fetchable_urls().is_empty())
        .collect();
    let (file, name) = usable
        .iter()
        .find(|(_, n)| Some(*n) == wanted.as_deref())
        .or_else(|| usable.first())
        .copied()
        .ok_or_else(|| {
            crate::i18n::tr("The mirror list describes no file this build can download.")
        })?;
    if usable.len() > 1 && wanted.as_deref() != Some(name) {
        crate::log::warn(&format!(
            "#{id} the document describes {} files; fetching {name:?}",
            usable.len()
        ));
    }

    let mut mirrors: Vec<&hya_net::MetaUrl> = file.fetchable_urls();
    // Transport first, then one transport per item — same rule and same
    // reason as `probe_metalink`.
    mirrors.sort_by_key(|u| (u.kind.transport_tier(), u.priority, u.url.clone()));
    let lead = mirrors[0].kind.transport_tier();
    mirrors.retain(|u| u.kind.transport_tier() == lead);
    spec.url = mirrors[0].url.clone();
    spec.mirrors = mirrors
        .iter()
        .enumerate()
        .map(|(i, u)| crate::model::MirrorRef {
            url: u.url.clone(),
            priority: (i + 1) as u32,
            max_connections: u.max_connections,
        })
        .collect();
    spec.attested_size = file.size;
    spec.attested_digest = file.best_hash().map(|h| h.spec());
    spec.pieces = hya_net::manifest::from_metalink(file)
        .ok()
        .map(|m| m.to_json());
    // Held spans were recorded against whatever the redirector URL served,
    // which was the document. Splicing them into the object would be the same
    // corruption the size gate refuses.
    spec.held.clear();
    spec.expected_size = file.size;
    // The document names the file; a redirector URL names nothing worth using.
    if let Some(dir) = std::path::Path::new(&spec.final_path).parent() {
        spec.final_path = dir.join(name).to_string_lossy().into_owned();
    }
    crate::log::info(&format!(
        "#{id} mirror list: {name:?}, {} mirror(s), {} bytes, {} piece(s)",
        spec.mirrors.len(),
        spec.attested_size.unwrap_or(0),
        file.pieces.as_ref().map(|p| p.hashes.len()).unwrap_or(0)
    ));
    Ok((name.to_string(), file.size))
}

/// Fetch a document over HTTP, following redirects, with the body capped.
///
/// The cap is not defensiveness for its own sake: the body is fetched before
/// anything about it is known, and an unbounded read of a body chosen by
/// whoever answers is a memory-exhaustion primitive no care in the parser can
/// undo.
async fn fetch_metalink(url: &str, user_agent: &str) -> Result<hya_net::Metalink, String> {
    let connector = shared_connector()?;
    let mut url = url.to_string();
    for _ in 0..10 {
        let u = parse_url(&url)?;
        let base = if u.tls {
            Target::direct_tls(&u.host, u.port, &u.path)
        } else {
            Target::direct(&u.host, u.port, &u.path)
        };
        let t = base.with_headers(vec![], Some(user_agent.to_string()));
        if let Ok(p) = probe_resilient(connector.as_ref(), &t).await {
            if p.is_redirect() {
                match join_url(&u, p.location.as_deref().unwrap_or("")) {
                    Some(next) => {
                        url = next;
                        continue;
                    }
                    None => return Err(crate::i18n::tr("Unusable redirect")),
                }
            }
        }
        let body = hya_net::fetch_small(connector.as_ref(), &t, hya_net::metalink::MAX_DOCUMENT)
            .await
            .map_err(|e| format!("{}: {e}", crate::i18n::tr("Cannot read the mirror list")))?;
        let text = String::from_utf8(body)
            .map_err(|_| crate::i18n::tr("The mirror list is not valid UTF-8."))?;
        return hya_net::metalink::parse(&text).map_err(|e| e.to_string());
    }
    Err(crate::i18n::tr("Too many redirects"))
}

/// Make `dir` exist and be writable, healing one specific corruption seen in
/// the field: an EMPTY directory owned by another user (root) sitting in a
/// writable parent — deletable from the parent, so drop and recreate it.
fn ensure_writable_dir(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let probe = dir.join(".hydra-write-probe");
    match std::fs::write(&probe, b"x") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let empty = std::fs::read_dir(dir)
                .map(|mut i| i.next().is_none())
                .unwrap_or(false);
            if empty && std::fs::remove_dir(dir).is_ok() {
                let _ = std::fs::create_dir_all(dir);
                crate::log::warn(&format!(
                    "recreated unwritable empty directory {}",
                    dir.display()
                ));
            }
        }
        Err(_) => {}
    }
}
/// Users see io errors verbatim; "os error 13" deserves an explanation.
fn friendly_io(e: &std::io::Error, path: &str) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "{} ({path})",
            crate::i18n::tr(
                "Access to the download folder was denied. Allow Hydra under System Settings > Privacy & Security > Files and Folders, or choose another folder."
            )
        )
    } else {
        e.to_string()
    }
}

/// URL parsing lives in `hya-stream` so the CLI and the GUI resolve a
/// manifest's URIs identically. Re-exported here because every call site in
/// this crate already says `engine::parse_url`.
pub use hya_stream::{join_url, ParsedUrl};

/// `hya_stream::parse_url` with its two user-facing errors translated: the
/// library has no catalogue and no opinion about presentation.
pub fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    hya_stream::parse_url(url).map_err(|e| crate::i18n::tr(&e))
}

/// The name `url` itself states, or `None` when it states none — a bare
/// host, a directory address, a redirector whose own path carries nothing.
///
/// [`file_name_from_url`] invents `index.html` for those so a download always
/// has somewhere to go, but a caller deciding whether a name MEANS anything
/// must be able to tell the invented one apart: the duplicate check reported
/// "a file with this name already exists" about an `index.html` left by some
/// unrelated download, for a link whose real name the probe had not resolved
/// yet.
pub fn url_file_name(url: &str) -> Option<String> {
    let path = parse_url(url).map(|p| p.path).unwrap_or_default();
    let seg = path
        .split('?')
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let name = percent_decode(seg);
    (!name.is_empty()).then_some(name)
}

pub fn file_name_from_url(url: &str) -> String {
    url_file_name(url).unwrap_or_else(|| "index.html".into())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The request headers a download's credentials turn into: HTTP Basic for a
/// login, `Cookie:` for a cookie string. Shared by the transfer and by the
/// probes that must see the same object it will.
pub fn request_headers(auth: Option<(&str, &str)>, cookies: Option<&str>) -> Vec<String> {
    let mut headers = Vec::new();
    if let Some((user, pass)) = auth {
        headers.push(format!(
            "Authorization: Basic {}",
            base64(format!("{user}:{pass}").as_bytes())
        ));
    }
    if let Some(c) = cookies.map(str::trim).filter(|c| !c.is_empty()) {
        headers.push(format!("Cookie: {c}"));
    }
    headers
}

fn target_for(u: &ParsedUrl, spec: &StartSpec) -> Target {
    let headers = request_headers(
        spec.auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
        spec.cookies.as_deref(),
    );
    target_of(u, headers, &spec.user_agent)
}

/// Probe the mirror list and decide who fetches, who waits, and with how many
/// connections.
///
/// # Two admission tests, because there are two kinds of evidence
///
/// Without a document the only evidence is what the mirrors themselves say, so
/// agreement has to be pairwise: same size, same strong validator. That test is
/// deliberately strict — two mirrors serving different builds produce a file of
/// exactly the right length that is not either object — and it is also
/// unsatisfiable across a real mirror list, because independent operators
/// running independent web servers cannot share an `ETag`.
///
/// `attested_size` is different evidence: it comes from whoever built the
/// object, on a host that is usually not one of the mirrors, alongside a content
/// digest. A mirror is admitted if it agrees with the DOCUMENT, and the digest —
/// per chunk where `<pieces>` was published — is what catches one serving
/// something else.
///
/// Returns `(targets, connections per target, reserve bench, scheduler
/// sources)`. With no mirrors this is exactly the single-source tuple the
/// transfer used before mirror lists existed.
#[allow(clippy::too_many_arguments)]
async fn plan_sources(
    spec: &StartSpec,
    primary_url: &ParsedUrl,
    primary_target: &Target,
    primary_probe: &Probe,
    size: u64,
    delta: f64,
    budget: usize,
    connector: &Arc<TlsCapableConnector>,
    id: DlId,
) -> (Vec<Target>, Vec<usize>, hya_net::Bench, Vec<Source>) {
    let caps_for = |pr: &Probe| {
        if spec.attested_size.is_some() || !(pr.weak_validator || pr.validator.is_none()) {
            // A document that states the size and a content digest establishes
            // agreement more strongly than an `ETag` does, and from outside the
            // mirrors — so a source admitted on that evidence is a full one.
            Capability::Full
        } else {
            Capability::NoValidator
        }
    };
    // One seated source — and whatever reserves are still on their way.
    //
    // The late stream is threaded through here rather than dropped, because
    // this is the case that needs a bench MOST: with a single source there is
    // no second connection for repair to move work to, so a mirror that goes
    // silent has nothing behind it but the reserves. An earlier revision
    // returned `Bench::default()` on this path and discarded every straggler
    // the background prober was about to admit.
    let alone =
        |sources: Vec<Source>,
         late: Option<tokio::sync::mpsc::UnboundedReceiver<hya_net::Reserve>>| {
            (
                vec![primary_target.clone()],
                vec![budget],
                hya_net::Bench {
                    ready: Vec::new(),
                    late,
                },
                sources,
            )
        };
    if spec.mirrors.is_empty() {
        // Nothing was ever probed, so there is nothing on its way.
        return alone(
            vec![Source {
                caps: caps_for(primary_probe),
                delta_est: delta,
                ..Source::default()
            }],
            None,
        );
    }

    // The primary was already probed above; only the OTHERS cost a request
    // here, and they are probed concurrently because a mirror list is a list of
    // independent hosts and waiting for the slowest one in series is the whole
    // latency of the list.
    let host_of = |u: &ParsedUrl| format!("{}:{}", u.host, u.port);
    let mut kept: Vec<(String, Target, f64, Capability, hya_core::SourcePlan)> = Vec::new();
    let primary_plan = spec
        .mirrors
        .iter()
        .find(|m| m.url == spec.url)
        .map(|m| hya_core::SourcePlan {
            priority: m.priority.max(1),
            max_connections: m.max_connections,
        })
        .unwrap_or_default();
    kept.push((
        host_of(primary_url),
        primary_target.clone(),
        delta,
        caps_for(primary_probe),
        primary_plan,
    ));

    // Bounded fan-out, but not by politeness: every probe here goes to a
    // DIFFERENT host and one HEAD each is not something any of them feels — the
    // per-host ceilings elsewhere answer that question. The cap exists so a
    // forty-mirror document cannot open forty sockets at once and hit an fd
    // limit.
    //
    // The cost of a small cap is latency paid before the first byte: measured
    // on the CLI against a real twelve-mirror Fedora document, six in flight
    // took two waves and 4.1 s of setup against 0.5 s for a single URL. One
    // wave removes almost all of it.
    let gate = Arc::new(tokio::sync::Semaphore::new(16));
    let primary_validator = primary_probe.validator.clone();
    let mut set = tokio::task::JoinSet::new();
    for m in spec.mirrors.iter().filter(|m| m.url != spec.url).cloned() {
        let conn = connector.clone();
        let spec = spec.clone();
        let gate = gate.clone();
        let primary_validator = primary_validator.clone();
        set.spawn(async move {
            let _permit = gate.acquire_owned().await.ok()?;
            let u = parse_url(&m.url).ok()?;
            let t = target_for(&u, &spec);
            let hop = std::time::Instant::now();
            let pr = probe_resilient(conn.as_ref(), &t).await.ok()?;
            if pr.is_redirect() || pr.status >= 300 || !pr.ranges {
                return None;
            }
            // The document's size is the admission test when there is one.
            // Without a document, fall back to the pairwise rule: the same size
            // AND the same strong validator as the source already in hand.
            let ok = match spec.attested_size {
                Some(want) => pr.size == want,
                None => {
                    pr.size == size
                        && !pr.weak_validator
                        && pr.validator.is_some()
                        && pr.validator == primary_validator
                }
            };
            if !ok {
                return None;
            }
            let caps = if spec.attested_size.is_some() {
                Capability::Full
            } else {
                Capability::NoValidator
            };
            Some((
                format!("{}:{}", u.host, u.port),
                t,
                hop.elapsed().as_secs_f64().clamp(0.05, 45.0),
                caps,
                hya_core::SourcePlan {
                    priority: m.priority.max(1),
                    max_connections: m.max_connections,
                },
            ))
        });
    }
    // Collected in RANK order rather than in completion order: which mirror is
    // seated and which waits on the bench must not depend on which handshake
    // happened to finish first, or two runs against the same document choose
    // different mirrors and neither can be debugged from its log.
    //
    // And a mirror too slow to answer a HEAD is not one worth waiting for. The
    // probe phase is paid entirely before the first byte, so its cost is the
    // SLOWEST mirror in the list rather than the average: measured on the CLI
    // against a real Fedora document, one host taking eleven seconds to answer
    // held up a transfer that already had eleven other sources ready. Waiting
    // buys a source that, on that evidence, will be the first one repair takes
    // work away from.
    //
    // The window is relative rather than fixed, because "slow" is a property of
    // the path: three times the fastest mirror's own round trip, floored so a
    // fast first answer cannot make it unreasonably tight and capped so a
    // pathological one cannot reintroduce the wait. The primary is already
    // probed and admitted, so abandoning stragglers can never leave the
    // transfer with no source.
    // The window is short and it costs nothing to lose: a mirror that misses
    // it is not abandoned any more — it keeps probing in the background and
    // joins the reserve bench when it answers. All the window decides is how
    // long the transfer holds still hoping for one more SEAT, and the loop
    // stops on its own the moment the connection budget's worth of mirrors
    // have been admitted. (Unlike the CLI's loop, the clock here may arm
    // before any secondary has answered — the PRIMARY is already probed and
    // seated, so a bounded wait can never leave the transfer with nothing.)
    const GRACE_MULTIPLE: f64 = 3.0;
    const GRACE_MIN: std::time::Duration = std::time::Duration::from_millis(600);
    const GRACE_MAX: std::time::Duration = std::time::Duration::from_secs(10);
    let probe_start = std::time::Instant::now();
    let mut first_ok: Option<std::time::Duration> = None;
    let mut probed: Vec<(String, Target, f64, Capability, hya_core::SourcePlan)> = Vec::new();
    let mut streaming = false;
    while !set.is_empty() {
        // The primary holds one seat already; the rest of the budget is what
        // this loop is filling.
        if probed.len() + 1 >= budget.max(1) {
            streaming = true;
            break;
        }
        let window = std::time::Duration::from_secs_f64(
            first_ok.unwrap_or(GRACE_MIN).as_secs_f64() * GRACE_MULTIPLE,
        )
        .clamp(GRACE_MIN, GRACE_MAX);
        let left = window.saturating_sub(probe_start.elapsed());
        let joined = match tokio::time::timeout(left, set.join_next()).await {
            Ok(v) => v,
            Err(_) => {
                streaming = true;
                break;
            }
        };
        let Some(r) = joined else { break };
        if let Ok(Some(v)) = r {
            if first_ok.is_none() {
                first_ok = Some(probe_start.elapsed());
            }
            probed.push(v);
        }
    }
    // The stragglers keep probing. Each task is SELF-admitting — it returns
    // `Some` only for a mirror that passed the same size/validator/ranges test
    // the seated ones passed — so the forwarder's job is only to turn each
    // admission into a `Reserve` as it lands.
    let late = if streaming && !set.is_empty() {
        crate::log::info(&format!(
            "#{id} starting on {} source(s) after {:.2}s; {} mirror(s) still probing join the \
             reserve bench as they answer",
            probed.len() + 1,
            probe_start.elapsed().as_secs_f64(),
            set.len(),
        ));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(r) = set.join_next().await {
                let Ok(Some((host, target, _delta, _caps, plan))) = r else {
                    continue;
                };
                if tx.send(hya_net::Reserve { target, plan, host }).is_err() {
                    // The transfer ended; nothing left to probe for.
                    return;
                }
            }
        });
        Some(rx)
    } else {
        None
    };
    probed.sort_by(|a, b| (a.4.priority, &a.0).cmp(&(b.4.priority, &b.0)));
    let results = probed;
    kept.extend(results);

    if kept.len() == 1 {
        crate::log::info(&format!(
            "#{id} mirror list: one source so far; {} reserve(s) may still arrive",
            if late.is_some() { "more" } else { "no" }
        ));
        // `late` is handed on, not dropped: a lone source has no second
        // connection for repair to move work to, so the reserves still being
        // probed are the only thing between it and a stall.
        return alone(
            vec![Source {
                caps: kept[0].3,
                delta_est: delta,
                priority: kept[0].4.priority,
                ..Source::default()
            }],
            late,
        );
    }

    let plans: Vec<hya_core::SourcePlan> = kept.iter().map(|k| k.4).collect();
    // Per-host ceiling: the user's connection setting is what they chose for one
    // server, so it stays the per-host bound while `budget` is the aggregate.
    let split = hya_core::plan::allocate(&plans, budget, budget, budget);
    let seated: Vec<usize> = (0..kept.len()).filter(|&i| split[i] > 0).collect();
    let targets: Vec<Target> = seated.iter().map(|&i| kept[i].1.clone()).collect();
    let per: Vec<usize> = seated.iter().map(|&i| split[i]).collect();
    let sources: Vec<Source> = seated
        .iter()
        .map(|&i| Source {
            caps: kept[i].3,
            delta_est: kept[i].2.max(1e-3),
            // The publisher's ranking, consulted once — for the first split,
            // before anything has been measured. See `hya_core::sched::Source`.
            priority: kept[i].4.priority,
            ..Source::default()
        })
        .collect();
    // Everything the split did not seat is the bench, drawn on in place when a
    // source dies. This is what makes a nineteen-mirror list worth more than a
    // four-mirror one at four connections.
    let bench_ready: Vec<hya_net::Reserve> = {
        let mut idx: Vec<usize> = (0..kept.len()).filter(|&i| split[i] == 0).collect();
        idx.sort_by_key(|&i| (plans[i].priority, i));
        idx.into_iter()
            .map(|i| hya_net::Reserve {
                target: kept[i].1.clone(),
                plan: plans[i],
                host: kept[i].0.clone(),
            })
            .collect()
    };
    let bench = hya_net::Bench {
        ready: bench_ready,
        late,
    };
    crate::log::info(&format!(
        "#{id} mirror list: {} source(s) over {} connection(s), {} in reserve",
        targets.len(),
        per.iter().sum::<usize>(),
        bench.ready.len()
    ));
    (targets, per, bench, sources)
}

/// Check a finished transfer against what the Metalink document promised.
///
/// Two checks, in the order that costs least:
///
/// 1. **`<pieces>`**, when the document published them. A failing chunk is
///    refetched from a mirror that did not serve it and re-checked against the
///    same digest before being accepted, so a second corrupt copy is not taken
///    on faith merely because it was asked for twice. This is what makes a
///    mirror list worth more than a URL list at the moment something goes wrong:
///    the remedy is one chunk, not the whole object.
/// 2. **The whole-file digest**, which catches everything a piece grid cannot —
///    including a document whose pieces are absent.
///
/// Runs on the STAGING file, before the rename. A file that fails its digest
/// must never appear in the destination under the name the user asked for.
///
/// `Trust::Advertised`, always: nothing here has authenticated the document, and
/// its `<signature>` is recorded rather than checked. Detection and targeted
/// refetch are self-correcting and are allowed; naming erasure positions for a
/// parity decode is not.
async fn verify_attested(
    spec: &StartSpec,
    temp: &str,
    size: u64,
    connector: &Arc<TlsCapableConnector>,
    targets: &[Target],
) -> Result<(), String> {
    use hya_net::manifest::{ChunkVerifier, Manifest, Trust};

    if let Some(json) = spec.pieces.as_deref() {
        match Manifest::parse(json) {
            // A manifest that no longer parses is not a reason to fail a
            // byte-complete download: the whole-file digest below still checks
            // it, and the object itself is fine.
            Err(e) => crate::log::warn(&format!("#{} unusable piece list: {e}", spec.id)),
            Ok(m) => {
                let mut v = ChunkVerifier::new(m, Trust::Advertised);
                {
                    let mut f = std::fs::File::open(temp)
                        .map_err(|e| format!("cannot reopen the staging file to verify: {e}"))?;
                    v.write_reader(&mut f)
                        .map_err(|e| format!("read failed while verifying: {e}"))?;
                }
                if !v.all_verified() {
                    let bad = v.failed_indices().to_vec();
                    crate::log::warn(&format!(
                        "#{} {} chunk(s) failed their digest; refetching",
                        spec.id,
                        bad.len()
                    ));
                    let sink =
                        Arc::new(SparseSink::create(temp, size).map_err(|e| {
                            format!("cannot reopen the staging file to repair: {e}")
                        })?);
                    for (nth, idx) in bad.into_iter().enumerate() {
                        let (lo, hi) = v.manifest().span(idx);
                        // Rotate through the mirrors, starting past the primary.
                        // Which host served the corrupt chunk is unknowable from
                        // here, so a FIXED alternate is a coin-flip that repeats
                        // itself: if the alternate happens to be the bad mirror,
                        // every refetch fails and the whole repair dies on its
                        // first candidate. Rotation costs nothing and puts each
                        // retry somewhere new.
                        let t = targets[(1 + nth) % targets.len()].clone();
                        hya_net::fetch_range_retry(
                            connector.clone(),
                            t,
                            lo,
                            hi,
                            sink.clone(),
                            3,
                            30.0,
                        )
                        .await
                        .map_err(|e| format!("chunk {idx} refetch failed: {e}"))?;
                        let mut fresh = vec![0u8; (hi - lo) as usize];
                        {
                            use std::io::{Read as _, Seek as _, SeekFrom};
                            let mut f = std::fs::File::open(temp).map_err(|e| e.to_string())?;
                            f.seek(SeekFrom::Start(lo)).map_err(|e| e.to_string())?;
                            f.read_exact(&mut fresh).map_err(|e| e.to_string())?;
                        }
                        v.retry(idx);
                        if !v.write(lo, &fresh).is_empty() {
                            return Err(crate::i18n::tr(
                                "A chunk still fails its checksum after refetching: the mirrors are serving bytes the mirror list does not describe.",
                            ));
                        }
                    }
                }
                crate::log::info(&format!(
                    "#{} {} chunk(s) verified against the mirror list",
                    spec.id,
                    v.verified_count()
                ));
            }
        }
    }

    let Some(spec_digest) = spec.attested_digest.as_deref() else {
        return Ok(());
    };
    let Some((algo, want)) = spec_digest
        .split_once(':')
        .and_then(|(a, h)| hya_net::digest::Algo::parse(a).map(|al| (al, h.to_ascii_lowercase())))
    else {
        // An algorithm this build cannot compute is reported as unchecked
        // rather than as a pass: a verification that means nothing is worse
        // than an honest absence.
        crate::log::warn(&format!(
            "#{} cannot check {spec_digest}: unknown digest algorithm",
            spec.id
        ));
        return Ok(());
    };
    let path = temp.to_string();
    let got = tokio::task::spawn_blocking(move || digest_file(&path, algo))
        .await
        .map_err(|e| format!("digest task failed: {e}"))?;
    match got {
        Some(g) if g == want => {
            crate::log::info(&format!("#{} {} verified", spec.id, algo.as_str()));
            Ok(())
        }
        Some(g) => Err(format!(
            "{}: {} {} != {}",
            crate::i18n::tr("Checksum mismatch"),
            algo.as_str(),
            g,
            want
        )),
        None => Err(crate::i18n::tr(
            "The downloaded file could not be read to verify it.",
        )),
    }
}

/// Hash a file with any algorithm a mirror list may publish.
///
/// Streamed in 1 MiB chunks rather than read whole: peak memory must not scale
/// with the object, which is the one thing a downloader may never do — the
/// transfer itself writes at exact offsets and holds no reassembly buffer, so
/// this would otherwise be the only part of the program that could not handle a
/// file larger than RAM.
///
/// MD5 and SHA-1 are computed because they are what most Metalink 3.0 documents
/// actually publish. They are integrity checks here, not authentication: against
/// a transmission fault, a truncating proxy or a stale mirror — which is what a
/// published digest is for — they work, and against an adversary who chose the
/// bytes they do not, because the digest and the object came down the same wire.
fn digest_file(path: &str, algo: hya_net::digest::Algo) -> Option<String> {
    use hya_net::digest::Algo;
    use sha2::Digest as _;
    use std::io::Read;
    if matches!(algo, Algo::Crc32 | Algo::Crc32c) {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sha256 = sha2::Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut sha1 = sha1::Sha1::new();
    let mut md5 = md5::Md5::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => match algo {
                Algo::Sha256 => sha256.update(&buf[..n]),
                Algo::Sha512 => sha512.update(&buf[..n]),
                Algo::Sha1 => sha1.update(&buf[..n]),
                Algo::Md5 => md5.update(&buf[..n]),
                Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(hya_net::digest::to_lower_hex(&match algo {
        Algo::Sha256 => sha256.finalize().to_vec(),
        Algo::Sha512 => sha512.finalize().to_vec(),
        Algo::Sha1 => sha1.finalize().to_vec(),
        Algo::Md5 => md5.finalize().to_vec(),
        Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
    }))
}

// ------------------------------------------------------------------ transfer

async fn run_download(
    mut spec: StartSpec,
    cancel: Arc<AtomicBool>,
    limiter: Arc<RateLimiter>,
    final_path: Arc<Mutex<String>>,
    tx: UnboundedSender<Event>,
) {
    let id = spec.id;
    let ev = |e: Event| {
        let _ = tx.send(e);
    };

    // HYDRA_AB_FRESH=1: measurement escape hatch — build a throwaway
    // connector per transfer (the pre-pooling behaviour) to bisect
    // throughput differences. Not for production use.
    let fresh = std::env::var_os("HYDRA_AB_FRESH").is_some();
    let connector = match if fresh {
        TlsCapableConnector::new()
            .map(Arc::new)
            .map_err(|e| e.to_string())
    } else {
        shared_connector()
    } {
        Ok(c) => c,
        Err(e) => {
            ev(Event::Failed {
                id,
                error: e,
                done: 0,
                held: spec.held.clone(),
                permission_denied: false,
            });
            return;
        }
    };

    // ---- ftp://: single-connection fetch ---------------------------------
    //
    // Deliberately ONE connection: FTP range preemption costs
    // control-channel round trips that HTTP pays nothing for, so the
    // object streams sequentially from one source.
    if let Ok(u) = parse_url(&spec.url) {
        if u.ftp {
            run_ftp_download(&spec, &u, &cancel, &limiter, &tx).await;
            return;
        }
    }

    // ---- probe, following redirects --------------------------------------
    ev(Event::Status {
        id,
        line: crate::i18n::tr("Connecting..."),
    });
    let mut url = spec.url.clone();
    // The mirror the CURRENT attempt started from, as the document spells it.
    //
    // Distinct from `url`, which redirects rewrite: when mirror A answers 302
    // and the redirect target cannot be reached, the dead entry in
    // `spec.mirrors` is A — and removing by the post-redirect `url` removes
    // nothing, so `plan_sources` would probe the dead chain a second time.
    let mut attempt = url.clone();
    let mut probed: Option<(ParsedUrl, Probe)> = None;
    // Mirrors to fall forward to when the one being probed cannot be reached at
    // all, best-ranked first and excluding the one already being tried.
    //
    // Surviving a dead LEAD mirror is the whole point of a mirror list, and it
    // is the case a publisher's ranking is least able to help with: a document
    // says which mirrors it EXPECTS to serve well, and a host that no longer
    // resolves was expected to serve well right up until it stopped existing.
    // Without this the transfer fails at the first probe while holding a dozen
    // working URLs — which is exactly the failure the reserve bench was built
    // to remove, arriving one step before the bench exists.
    let mut fallback: std::collections::VecDeque<String> = spec
        .mirrors
        .iter()
        .map(|m| m.url.clone())
        .filter(|u| *u != spec.url)
        .collect();
    // Per-request setup estimate for the scheduler. Timed on the FINAL probe
    // hop only: the old whole-loop measurement folded every redirect hop in,
    // so the origins most in need of fast repair decisions (long redirect
    // chains) got the slowest ones. Floored at the CLI's 0.05 s prior so a
    // pooled-connection probe cannot make repairs look free.
    let mut delta = 0.05f64;
    // Redirect hops PLUS one attempt per mirror: a dead lead mirror must not
    // eat the budget a redirect chain needs, and a dozen dead mirrors must
    // still terminate.
    for _ in 0..(10 + fallback.len()) {
        let u = match parse_url(&url) {
            Ok(u) => u,
            Err(e) => {
                ev(Event::Failed {
                    id,
                    error: e,
                    done: 0,
                    held: spec.held.clone(),
                    permission_denied: false,
                });
                return;
            }
        };
        let t = target_for(&u, &spec);
        let t_hop = std::time::Instant::now();
        match probe_resilient(connector.as_ref(), &t).await {
            Ok(p) if p.is_redirect() => {
                let loc = p.location.clone().unwrap_or_default();
                crate::log::debug(&format!(
                    "#{id} redirect {} -> {}",
                    p.status,
                    crate::log::redact(&loc)
                ));
                match join_url(&u, &loc) {
                    Some(next) => url = next,
                    None => {
                        ev(Event::Failed {
                            id,
                            error: format!("{}: {loc}", crate::i18n::tr("Unusable redirect")),
                            done: 0,
                            held: spec.held.clone(),
                            permission_denied: false,
                        });
                        return;
                    }
                }
            }
            Ok(p) => {
                // A redirect the server expressed in HTML rather than in a
                // header: a referrer stripper or link filter answering `200`
                // with a page whose whole content is "go here instead".
                // Without this hop the saved file IS that page — the
                // one-kilobyte `index.html` this resolves. Charged to the same
                // hop budget as a `3xx`, since a pair of such pages pointing at
                // each other is a loop like any other.
                let hop_to = if p.maybe_redirector() {
                    hya_net::html_redirect(connector.as_ref(), &t)
                        .await
                        .and_then(|loc| join_url(&u, &loc))
                } else {
                    None
                };
                if let Some(next) = hop_to {
                    crate::log::debug(&format!(
                        "#{id} html redirect -> {}",
                        crate::log::redact(&next)
                    ));
                    url = next;
                } else {
                    crate::log::debug(&format!(
                        "#{id} probe: status={} size={} ranges={} type={:?}",
                        p.status, p.size, p.ranges, p.content_type
                    ));
                    delta = t_hop.elapsed().as_secs_f64().clamp(0.05, 45.0);
                    probed = Some((u, p));
                    break;
                }
            }
            Err(e) => match fallback.pop_front() {
                Some(next) => {
                    crate::log::warn(&format!(
                        "#{id} mirror {} could not be reached ({e}); trying the next one",
                        u.host
                    ));
                    // The failed mirror is out of this run entirely: it stays
                    // out of the source list AND out of the reserve bench, so
                    // `plan_sources` does not probe it again and a substitution
                    // cannot pick it later. Removed by the URL the ATTEMPT
                    // started from, not the one it died at — a mirror that
                    // redirects before failing dies at an address the document
                    // never listed.
                    spec.mirrors.retain(|m| m.url != attempt);
                    url = next;
                    attempt.clone_from(&url);
                    spec.url.clone_from(&url);
                }
                None => {
                    crate::log::error(&format!("#{id} probe failed: {e}"));
                    ev(Event::Failed {
                        id,
                        error: e.to_string(),
                        done: 0,
                        held: spec.held.clone(),
                        permission_denied: false,
                    });
                    return;
                }
            },
        }
        if cancel.load(Ordering::Relaxed) {
            ev(Event::Stopped {
                id,
                done: 0,
                held: spec.held.clone(),
            });
            return;
        }
    }
    let Some((u, p)) = probed else {
        ev(Event::Failed {
            id,
            error: crate::i18n::tr("Too many redirects"),
            done: 0,
            held: spec.held.clone(),
            permission_denied: false,
        });
        return;
    };
    crate::log::debug(&format!("#{id} probe delta {delta:.3}s"));

    // The date to stamp the finished file with, resolved once here while the
    // probe headers are still in hand.
    //
    // `Last-Modified` first and on its own terms: `validator` collapses to the
    // ETag whenever the server sent one, so reading THAT field would throw the
    // date away for GitHub, S3 and most CDNs — the common case, and the exact
    // defect the CLI's `--remote-time` was fixed for. The validator is still
    // consulted as a fallback, for the servers that send only a date: there it
    // IS the `Last-Modified` value.
    let stamp = spec.remote_time.then(|| remote_stamp(&p)).flatten();
    if spec.remote_time && stamp.is_none() {
        crate::log::info(&format!(
            "#{id} remote time: server sent no Last-Modified header, skipped"
        ));
    }

    // ---- the URL is a MIRROR LIST, not the object -------------------------
    //
    // `https://mirrors.fedoraproject.org/metalink?repo=fedora-41` has no
    // extension, so nothing the dialog could read told it what this was. The
    // probe has already happened and already carries the `Content-Type`, so
    // asking here costs nothing — and asking earlier would cost a round trip on
    // every download that is not a mirror list.
    //
    // Saving it instead would hand the user a few kilobytes of XML under the
    // name of the multi-gigabyte image they asked for: a file that passes every
    // check this program makes and is entirely the wrong one.
    //
    // One hop only, and only when the job does not already carry a list — a
    // document that names itself, or a mirror that answers with another mirror
    // list, then costs one wasted fetch rather than an unbounded chain.
    if spec.mirrors.is_empty() && p.serves_metalink() {
        crate::log::info(&format!("#{id} {} serves a Metalink document", u.host));
        match fetch_metalink(&url, &spec.user_agent).await {
            Ok(doc) => match adopt_metalink(&doc, &mut spec, id) {
                Ok((name, size)) => {
                    // The destination the finisher renames to is held behind a
                    // lock — File Info can retarget it while a transfer runs —
                    // so the document's name has to be written THERE and not
                    // only on the spec, or the object lands under the
                    // redirector's name after all.
                    if let Ok(mut g) = final_path.lock() {
                        g.clone_from(&spec.final_path);
                    }
                    // Tell the list what it is really about to receive. The
                    // user typed one address and is getting a differently named
                    // file of a very different size; a row that keeps showing
                    // "metalink" and no size leaves that as a mystery.
                    ev(Event::Probed {
                        id,
                        size,
                        ranges: true,
                        file_name: Some(name),
                    });
                    ev(Event::Status {
                        id,
                        line: crate::i18n::tr("Reading the mirror list..."),
                    });
                    // Re-enter with the document's sources in hand. Boxed
                    // because this is a recursive `async fn` and its future
                    // would otherwise have to contain itself.
                    return Box::pin(run_download(spec, cancel, limiter, final_path, tx)).await;
                }
                Err(e) => {
                    ev(Event::Failed {
                        id,
                        error: e,
                        done: 0,
                        held: spec.held.clone(),
                        permission_denied: false,
                    });
                    return;
                }
            },
            Err(e) => {
                ev(Event::Failed {
                    id,
                    error: e,
                    done: 0,
                    held: spec.held.clone(),
                    permission_denied: false,
                });
                return;
            }
        }
    }

    let file_name = p.suggested_filename().or_else(|| {
        let n = file_name_from_url(&url);
        (!n.is_empty()).then_some(n)
    });
    let known_size = (p.status < 300 && p.size > 0).then_some(p.size);
    ev(Event::Probed {
        id,
        size: known_size,
        ranges: p.ranges,
        file_name,
    });

    let target = target_for(&u, &spec);
    let temp = spec.temp_path.clone();
    if let Some(dir) = std::path::Path::new(&temp).parent() {
        ensure_writable_dir(dir);
    }

    // ---- no-range / unknown-size fallback: one streaming GET -------------
    let Some(size) = known_size.filter(|_| p.ranges) else {
        crate::log::info(&format!(
            "#{id} no range support / unknown size: single stream"
        ));
        ev(Event::Status {
            id,
            line: crate::i18n::tr("Receiving data..."),
        });
        let t0 = std::time::Instant::now();
        // Driven through a poll loop rather than a bare await. This path has no
        // scheduler to observe, so a plain await reported nothing until the last
        // byte and read the stop flag never: on a large object that is an hour of
        // "Receiving data..." at 0 B with Pause and Stop doing nothing at all.
        // The byte counter and the cancel flag are what the loop is for.
        let written = Arc::new(AtomicU64::new(0));
        let pace = hya_net::polite::Pace::shared(limiter);
        let fut = hya_net::fetch_streaming_observed(
            connector.as_ref(),
            &target,
            &temp,
            &written,
            Some(cancel.as_ref()),
            &pace,
        );
        tokio::pin!(fut);
        let mut sm_rate = 0.0f64;
        let mut rate_mark: Option<(u64, std::time::Instant)> = None;
        loop {
            tokio::select! {
                r = &mut fut => {
                    match r {
                        Ok(n) => {
                            finish_file(&spec, &final_path, &tx, n, t0.elapsed().as_secs_f64(), stamp);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            ev(Event::Stopped {
                                id,
                                done: written.load(Ordering::Relaxed),
                                held: vec![],
                            });
                        }
                        Err(e) => {
                            let denied = e.kind() == std::io::ErrorKind::PermissionDenied;
                            ev(Event::Failed {
                                id,
                                error: friendly_io(&e, &temp),
                                done: written.load(Ordering::Relaxed),
                                held: vec![],
                                permission_denied: denied,
                            });
                        }
                    }
                    return;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(emit_interval_ms() as u64)) => {
                    // Dropping the future closes the socket. A stalled read is
                    // exactly when the user reaches for Stop, so the flag is
                    // acted on here too and not only between reads.
                    if cancel.load(Ordering::Relaxed) {
                        ev(Event::Stopped {
                            id,
                            done: written.load(Ordering::Relaxed),
                            held: vec![],
                        });
                        return;
                    }
                    let done = written.load(Ordering::Relaxed);
                    let now = std::time::Instant::now();
                    if let Some((prev_done, prev_t)) = rate_mark {
                        let dt = now.duration_since(prev_t).as_secs_f64();
                        if dt > 0.0 {
                            let inst = done.saturating_sub(prev_done) as f64 / dt;
                            let alpha = (-dt / 1.5f64).exp();
                            sm_rate = sm_rate * alpha + inst * (1.0 - alpha);
                        }
                    }
                    rate_mark = Some((done, now));
                    ev(Event::Progress {
                        id,
                        done,
                        rate: sm_rate,
                        // No size to subtract from: this path exists because the
                        // server would not state one, so any ETA would be invented.
                        eta: None,
                        conns: vec![ConnRow {
                            downloaded: done,
                            info: crate::i18n::tr("Receiving data..."),
                        }],
                        held: vec![],
                        recorded: None,
                    });
                }
            }
        }
    };

    // ---- scheduler path ---------------------------------------------------
    let n = spec.conns.clamp(1, 32);
    // A mirror list turns this into a multi-source transfer. Everything below
    // degenerates to exactly the previous single-source behaviour when
    // `spec.mirrors` is empty, which is what every non-Metalink caller passes.
    let (targets, per, bench, sources) =
        plan_sources(&spec, &u, &target, &p, size, delta, n, &connector, id).await;
    let mut sched =
        Scheduler::new(size, sources, &per).with_stall_timeout((12.0 * delta).clamp(4.0, 45.0));
    // Adaptive: open the budget but start ONE connection active; the ramp
    // inside `run_transfer_cancellable` admits the rest only while the
    // aggregate rate says they pay. On a link the origin (or an ISP shaper)
    // saturates at one or two streams, a fixed 8 divides capacity and adds
    // setup cost — measured on this project as slower than a single stream
    // on most live objects.
    if spec.adaptive && n > 1 {
        sched.set_active_limit(1);
    }
    let mut held_spans = spec.held.clone();
    if let Some(exp) = spec.expected_size {
        if exp != size && !held_spans.is_empty() {
            crate::log::warn(&format!(
                "#{id} mirror size mismatch ({exp} -> {size}): restarting from zero"
            ));
            held_spans.clear();
        }
    }
    let mut already = 0u64;
    for &(lo, hi) in &held_spans {
        let hi = hi.min(size);
        if lo < hi {
            sched.mark_done(lo, hi);
            already += hi - lo;
        }
    }

    let sink = match SparseSink::create(&temp, size) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            crate::log::error(&format!("#{id} sink create failed at {temp}: {e}"));
            let denied = e.kind() == std::io::ErrorKind::PermissionDenied;
            ev(Event::Failed {
                id,
                error: friendly_io(&e, &temp),
                done: already,
                held: spec.held.clone(),
                permission_denied: denied,
            });
            return;
        }
    };

    ev(Event::Status {
        id,
        line: crate::i18n::tr("Receiving data..."),
    });

    // Observer: runs inside the transfer loop every tick; throttle the
    // channel to ~4 events/sec and accumulate per-connection byte counts
    // from range-cursor deltas (the scheduler reports position, not totals).
    let tx_obs = tx.clone();
    let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut per_conn: HashMap<usize, (u64, u64, u64)> = HashMap::new(); // conn -> (last_lo, last_pos, total)
    type Snapshot = (u64, Vec<(u64, u64)>);
    let snapshot: Arc<Mutex<Snapshot>> = Arc::new(Mutex::new((already, spec.held.clone())));
    let snap_obs = snapshot.clone();
    // Displayed rate: EWMA over MEASURED byte deltas, not a sum of the
    // scheduler's per-connection window rates. The per-connection figures are
    // an internal control signal and twitch by design; a readout built on
    // them made the speed and ETA jump every refresh. Bytes-over-wall-clock
    // through a ~1.5 s time constant reads as a steady counter.
    let mut sm_rate = 0.0f64;
    let mut rate_mark: Option<(u64, std::time::Instant)> = None;
    // The last concurrency decision reported, so each is announced once.
    let mut last_reason = hya_core::LimitReason::None;
    // While set, the status line is showing a verdict and is owed its normal
    // text back at this instant.
    let mut verdict_until: Option<std::time::Instant> = None;
    let mut observe = move |s: &Scheduler, done: u64| {
        // Say what the transport decided about concurrency, and why, the moment
        // it decides. The ramp's own trace goes to stderr, which a release GUI
        // built without a console cannot show; the session log under Help > Logs
        // is where a user can actually find it.
        let reason = s.limit_reason();
        if reason != last_reason {
            last_reason = reason;
            if let Some(line) = describe_limit(reason, s.n_conns()) {
                crate::log::info(&format!("#{id} {line}"));
                let _ = tx_obs.send(Event::Status { id, line });
                // Shown, then handed back. The status line's steady-state job is
                // to say the transfer is running; a one-off verdict that never
                // clears reads as a stuck transfer, and the decision itself stays
                // on the connection rows and in the log.
                verdict_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(8));
            }
        }
        if verdict_until.is_some_and(|t| std::time::Instant::now() >= t) {
            verdict_until = None;
            let _ = tx_obs.send(Event::Status {
                id,
                line: crate::i18n::tr("Receiving data..."),
            });
        }
        for j in 0..s.n_conns() {
            let e = per_conn.entry(j).or_insert((0, 0, 0));
            if let Some((lo, pos, _hi)) = s.conn_range(j) {
                if e.0 == lo && pos >= e.1 {
                    e.2 += pos - e.1;
                } else if pos > lo {
                    e.2 += pos - lo;
                }
                *e = (lo, pos, e.2);
            }
        }
        // 10 Hz to the GUI: frequent enough that the bar and counters read as
        // continuous, cheap enough to be invisible in profiles. The held-span
        // snapshot (kept for the cancel path) refreshes at the same cadence:
        // `held_ranges()` allocates an IntervalSet, so computing it at the
        // 50 Hz tick rate was pure waste, and a stop can only lose the last
        // <100 ms of spans — which resume then re-fetches, safely.
        if last_emit.elapsed().as_millis() >= emit_interval_ms() || done >= size {
            last_emit = std::time::Instant::now();
            let held = s.held_ranges();
            if let Ok(mut g) = snap_obs.lock() {
                *g = (done, held.clone());
            }
            let now = std::time::Instant::now();
            if let Some((prev_done, prev_t)) = rate_mark {
                let dt = now.duration_since(prev_t).as_secs_f64();
                if dt > 0.0 {
                    let inst = done.saturating_sub(prev_done) as f64 / dt;
                    // alpha from dt so the time constant is independent of
                    // emit cadence: tau = 1.5 s.
                    let alpha = (-dt / 1.5f64).exp();
                    sm_rate = sm_rate * alpha + inst * (1.0 - alpha);
                }
            }
            rate_mark = Some((done, now));
            let conns: Vec<ConnRow> = (0..s.n_conns())
                .map(|j| {
                    let r = s.conn_rate(j);
                    ConnRow {
                        downloaded: per_conn.get(&j).map(|e| e.2).unwrap_or(0),
                        // An idle connection under a concurrency cap is not a
                        // broken one: the origin refused the extra requests (a
                        // 429 — `ash-speed.hetzner.com` serves exactly two per
                        // address), or the adaptive ramp has not admitted this
                        // slot yet. Labelling that "Disconnect." reads as a
                        // failure and invites the fix that makes it worse:
                        // raising the connection count.
                        info: if s.conn_range(j).is_none() {
                            if s.active_limit() < s.n_conns() {
                                dormant_label(s.limit_reason())
                            } else {
                                crate::i18n::tr("Disconnect.")
                            }
                        } else if r > 1.0 {
                            crate::i18n::tr("Receiving data...")
                        } else {
                            crate::i18n::tr("Send GET...")
                        },
                    }
                })
                .collect();
            // ETA from the smoothed rate, and only once it has warmed past
            // 1 KB/s — an ETA computed from startup noise counts down from
            // nonsense values.
            let eta = if sm_rate > 1024.0 {
                Some(((size - done.min(size)) as f64 / sm_rate) as u64)
            } else {
                None
            };
            let _ = tx_obs.send(Event::Progress {
                id,
                done,
                rate: sm_rate,
                eta,
                conns,
                held,
                recorded: None,
            });
        }
    };

    let pace = hya_net::polite::Pace::shared(limiter);
    let t0 = std::time::Instant::now();
    let tick_ms = if POWER_SAVE.load(Ordering::Relaxed) {
        80
    } else {
        20
    };
    // Substitutions are logged rather than surfaced in a row: the connection
    // rows carry no host name, so the only place a user could learn that the
    // mirror changed under a running transfer is the log — and a download that
    // took twice as long as expected is exactly when they go looking.
    let sub_id = id;
    let mut on_sub = move |src: usize, r: &hya_net::Reserve| {
        crate::log::warn(&format!(
            "#{sub_id} source {src} failed; switched to reserve mirror {}",
            r.host
        ));
    };
    // Kept for the chunk-repair path, which needs somewhere to refetch a bad
    // chunk from after `targets` has been moved into the transfer.
    let conn_targets = targets.clone();
    let result = hya_net::run_transfer_with_reserves(
        connector.clone(),
        targets,
        &per,
        size,
        sink,
        sched,
        tick_ms,
        &mut observe,
        pace,
        Some(cancel.clone()),
        bench,
        Some(&mut on_sub),
    )
    .await;

    let (done, held) = snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or((already, vec![]));
    match result {
        Ok((elapsed, reqs)) => {
            // Requests far above the connection count means range churn
            // (repairs/steals) — the first thing to look at when a transfer
            // is slower than the pipe.
            crate::log::debug(&format!(
                "#{id} transfer {elapsed:.2}s, {reqs} requests over {n} conns"
            ));
            // Verify against what the document said BEFORE the staging file is
            // renamed into place. A file that fails its digest must never
            // appear in the destination directory under the name the user
            // asked for: at that point it looks, to them and to every other
            // program, exactly like a good one.
            if let Err(why) = verify_attested(&spec, &temp, size, &connector, &conn_targets).await {
                crate::log::error(&format!("#{id} verification failed: {why}"));
                let _ = std::fs::remove_file(&temp);
                ev(Event::Failed {
                    id,
                    error: why,
                    done,
                    held: vec![],
                    permission_denied: false,
                });
                return;
            }
            finish_file(&spec, &final_path, &tx, size, elapsed, stamp);
        }
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            ev(Event::Stopped { id, done, held });
        }
        Err(e) => {
            crate::log::error(&format!("#{id} failed at {done}/{size} bytes: {e}"));
            let denied = e.kind() == std::io::ErrorKind::PermissionDenied;
            let msg = friendly_io(&e, &spec.final_path);
            ev(Event::Failed {
                id,
                error: msg,
                done,
                held,
                permission_denied: denied,
            });
        }
    }
    let _ = t0;
}

/// Single-connection FTP transfer: probe SIZE, resume via REST from the
/// contiguous prefix already on disk, drive progress off the sink's byte
/// counter (there is no scheduler to observe).
async fn run_ftp_download(
    spec: &StartSpec,
    u: &ParsedUrl,
    cancel: &Arc<AtomicBool>,
    limiter: &Arc<RateLimiter>,
    tx: &UnboundedSender<Event>,
) {
    use hya_net::scheme::Fetcher;
    let id = spec.id;
    let ev = |e: Event| {
        let _ = tx.send(e);
    };
    ev(Event::Status {
        id,
        line: crate::i18n::tr("Connecting..."),
    });

    let ep = hya_net::scheme::Endpoint::new(&u.host, u.port, &u.path).with_credentials(
        spec.auth.as_ref().map(|(l, _)| l.as_str()),
        spec.auth.as_ref().map(|(_, p)| p.as_str()),
    );
    // The same limiter the HTTP path answers to, so Speed Limiter means the
    // same thing on an ftp:// download — including switched on mid-transfer.
    let fetcher = hya_net::ftp::FtpFetcher::new(Arc::new(hya_net::TcpConnector))
        .with_pace(hya_net::polite::Pace::shared(limiter.clone()));

    let probe = match fetcher.probe(&ep).await {
        Ok(p) => p,
        Err(e) => {
            crate::log::error(&format!("#{id} ftp probe failed: {e}"));
            ev(Event::Failed {
                id,
                error: format!("ftp: {e}"),
                done: 0,
                held: spec.held.clone(),
                permission_denied: false,
            });
            return;
        }
    };
    crate::log::debug(&format!(
        "#{id} ftp probe: size={} ranged={}",
        probe.size, probe.ranged
    ));
    if probe.size == 0 {
        ev(Event::Failed {
            id,
            error: crate::i18n::tr("The FTP server did not report a file size"),
            done: 0,
            held: vec![],
            permission_denied: false,
        });
        return;
    }
    let file_name = {
        let n = file_name_from_url(&spec.url);
        (!n.is_empty()).then_some(n)
    };
    ev(Event::Probed {
        id,
        size: Some(probe.size),
        ranges: probe.ranged,
        file_name,
    });

    // Resume from the contiguous prefix only — REST is a start offset, not a
    // span list.
    let start = if probe.ranged {
        spec.held
            .iter()
            .find(|(lo, _)| *lo == 0)
            .map(|(_, hi)| (*hi).min(probe.size))
            .unwrap_or(0)
    } else {
        0
    };

    let temp = spec.temp_path.clone();
    if let Some(dir) = std::path::Path::new(&temp).parent() {
        ensure_writable_dir(dir);
    }
    let sink = match SparseSink::create(&temp, probe.size) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let denied = e.kind() == std::io::ErrorKind::PermissionDenied;
            ev(Event::Failed {
                id,
                error: friendly_io(&e, &temp),
                done: start,
                held: spec.held.clone(),
                permission_denied: denied,
            });
            return;
        }
    };

    ev(Event::Status {
        id,
        line: crate::i18n::tr("Receiving data..."),
    });
    let t0 = std::time::Instant::now();
    let size = probe.size;
    let fut = fetcher.fetch_range(&ep, start, size, sink.clone());
    tokio::pin!(fut);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut sm_rate = 0.0f64;
    let mut last = (start, std::time::Instant::now());
    let result = loop {
        tokio::select! {
            res = &mut fut => break Some(res),
            _ = ticker.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    break None;
                }
                let done = start + sink.written.load(Ordering::Relaxed);
                let dt = last.1.elapsed().as_secs_f64();
                if dt > 0.0 {
                    let inst = done.saturating_sub(last.0) as f64 / dt;
                    let alpha = (-dt / 1.5f64).exp();
                    sm_rate = sm_rate * alpha + inst * (1.0 - alpha);
                }
                last = (done, std::time::Instant::now());
                let eta = (sm_rate > 1024.0)
                    .then(|| ((size - done.min(size)) as f64 / sm_rate) as u64);
                let _ = tx.send(Event::Progress {
                    id,
                    done,
                    rate: sm_rate,
                    eta,
                    conns: vec![ConnRow {
                        downloaded: done - start,
                        info: crate::i18n::tr("Receiving data..."),
                    }],
                    held: vec![(0, done)],
                    recorded: None,
                });
            }
        }
    };
    let done = start + sink.written.load(Ordering::Relaxed);
    match result {
        // Dropping the pinned future closes the single data connection.
        None => ev(Event::Stopped {
            id,
            done,
            held: vec![(0, done)],
        }),
        Some(Ok(())) => {
            let final_path = Arc::new(Mutex::new(spec.final_path.clone()));
            // No stamp over FTP: the date would have to come from `MDTM`, which
            // this transfer never issues. Silently leaving the file's own time
            // is the same no-op an HTTP server that sends no date gets.
            finish_file(
                spec,
                &final_path,
                tx,
                size,
                t0.elapsed().as_secs_f64(),
                None,
            );
        }
        Some(Err(e)) => {
            crate::log::error(&format!("#{id} ftp failed at {done}/{size}: {e}"));
            ev(Event::Failed {
                id,
                error: format!("ftp: {e}"),
                done,
                held: vec![(0, done)],
                permission_denied: false,
            });
        }
    }
}

/// The date to stamp a finished file with, read from a probe's headers.
///
/// `Last-Modified` first and on its own terms: [`Probe::validator`] collapses
/// to the ETag whenever the server sent one, so reading THAT field would throw
/// the date away for GitHub, S3 and most CDNs — the common case, and the exact
/// defect the CLI's `--remote-time` was fixed for. The validator is still
/// consulted as a fallback, for the servers that send only a date: there it IS
/// the `Last-Modified` value.
///
/// `None` when the server offered no date at all. An ETag is opaque and cannot
/// answer "when did this object last change?", so the finished file keeps its
/// own time rather than getting a fabricated one.
fn remote_stamp(p: &Probe) -> Option<u64> {
    p.last_modified
        .as_deref()
        .or(p.validator.as_deref())
        .and_then(hya_net::polite::parse_http_date)
}

/// Set a file's modification time from a Unix timestamp.
///
/// Modification time, not birth time: POSIX has no way to set the latter at
/// all, so "creation date" in the option's wording means the same thing here
/// that it means in every other download manager — the date the server said
/// the object last changed.
fn set_mtime(path: &std::path::Path, secs: u64) -> std::io::Result<()> {
    let f = std::fs::File::options().write(true).open(path)?;
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// Move the finished `.part` into place and report completion. The
/// destination is read at completion time so File-Info edits made while the
/// transfer ran land the file where the user finally said.
fn finish_file(
    spec: &StartSpec,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
    size: u64,
    elapsed: f64,
    stamp: Option<u64>,
) {
    let final_str = final_path
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| spec.final_path.clone());
    let final_path = std::path::Path::new(&final_str);
    if let Some(dir) = final_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let moved = std::fs::rename(&spec.temp_path, final_path).or_else(|_| {
        // Cross-device: copy then remove.
        std::fs::copy(&spec.temp_path, final_path)
            .map(|_| ())
            .map(|()| {
                let _ = std::fs::remove_file(&spec.temp_path);
            })
    });
    match moved {
        Ok(()) => {
            // After the rename, never before: the mtime has to be set on the
            // file the user keeps, and moving it is what fixes which file
            // that is. Best-effort — a filesystem that will not take the
            // timestamp (a read-only mount, an exotic FUSE target) is not a
            // reason to report a good download as failed.
            if let Some(secs) = stamp {
                if let Err(e) = set_mtime(final_path, secs) {
                    crate::log::warn(&format!("#{} cannot set remote time: {e}", spec.id));
                }
            }
            crate::log::log(&format!("done #{} -> {final_str}", spec.id));
            let _ = tx.send(Event::Finished {
                id: spec.id,
                elapsed,
                size,
            });
        }
        Err(e) => {
            let denied = e.kind() == std::io::ErrorKind::PermissionDenied;
            let _ = tx.send(Event::Failed {
                id: spec.id,
                error: friendly_io(&e, &final_str),
                done: size,
                held: vec![(0, size)],
                permission_denied: denied,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use hya_core::LimitReason as R;

    /// The whole point of the reason plumbing: an idle row must say WHY it is
    /// idle. A search that settled reads as a decision, a server limit reads as
    /// the server's, and only an unexplained cap falls back to the old wording.
    #[test]
    fn a_dormant_connection_row_names_the_decision_that_idled_it() {
        assert_eq!(super::dormant_label(R::Measuring), "Waiting (measuring)...");
        assert_eq!(
            super::dormant_label(R::Measured {
                chosen: 1,
                chosen_rate: 1.0,
                tried: 2,
                tried_rate: 1.0,
            }),
            "Not used (measured slower)"
        );
        assert_eq!(
            super::dormant_label(R::Refused { serving: 2 }),
            "Not used (server limit)"
        );
        assert_eq!(
            super::dormant_label(R::Starved { serving: 2 }),
            "Not used (server limit)"
        );
        // Nothing has explained this one, so the old wording is still the honest
        // description.
        assert_eq!(
            super::dormant_label(R::None),
            "Waiting (connection limit)..."
        );
    }

    /// The verdict sentence carries the numbers a user can check against a
    /// single-stream download of the same object, with every placeholder filled.
    #[test]
    fn a_measured_verdict_reports_both_levels_and_both_rates() {
        let line = super::describe_limit(
            R::Measured {
                chosen: 1,
                chosen_rate: 446.0 * 1024.0,
                tried: 2,
                tried_rate: 415.0 * 1024.0,
            },
            8,
        )
        .expect("a search that rejected a level must say so");
        for part in ["2", "1", "446", "415"] {
            assert!(line.contains(part), "{line:?} is missing {part:?}");
        }
        assert!(
            !line.contains('{'),
            "an unfilled placeholder reached the user: {line:?}"
        );
    }

    /// A rate of zero was never measured — a refusal can settle the search at a
    /// level no window ever covered. Reporting "against 1 at 0 B/s" would state a
    /// measurement that was never taken, so the simpler sentence is used instead.
    #[test]
    fn a_level_that_was_never_measured_is_not_quoted_as_a_rate() {
        let line = super::describe_limit(
            R::Measured {
                chosen: 1,
                chosen_rate: 0.0,
                tried: 2,
                tried_rate: 990.0,
            },
            8,
        )
        .expect("a settled search below the budget still explains itself");
        assert!(
            !line.contains("0 B") && !line.contains("against"),
            "a rate that was never measured was quoted: {line:?}"
        );
        assert!(line.contains('1') && line.contains('8'), "{line:?}");
    }

    /// Nothing to announce when nothing is holding connections back, and a
    /// server limit equal to the budget is not a limit.
    #[test]
    fn nothing_is_announced_when_no_decision_narrowed_the_transfer() {
        assert_eq!(super::describe_limit(R::None, 8), None);
        assert_eq!(super::describe_limit(R::Measuring, 8), None);
        assert_eq!(super::describe_limit(R::Refused { serving: 8 }, 8), None);
        assert!(super::describe_limit(R::Refused { serving: 2 }, 8).is_some());
    }
    use super::*;

    fn scratch(name: &str, body: &str) -> String {
        let p = std::env::temp_dir().join(format!(
            "hydra-gui-metalink-{name}-{}.meta4",
            std::process::id()
        ));
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metalink xmlns="urn:ietf:params:xml:ns:metalink">
  <file name="big.iso">
    <size>4194304</size>
    <hash type="md5">0123456789abcdef0123456789abcdef</hash>
    <hash type="sha-256">d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef</hash>
    <pieces length="1048576" type="sha-256">
      <hash>0000000000000000000000000000000000000000000000000000000000000001</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000002</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000003</hash>
      <hash>0000000000000000000000000000000000000000000000000000000000000004</hash>
    </pieces>
    <url priority="9">https://slow.example/big.iso</url>
    <url priority="1" maxconnections="2">https://fast.example/big.iso</url>
    <url priority="4">rsync://rs.example/big.iso</url>
  </file>
</metalink>"#;

    #[test]
    fn a_local_document_resolves_into_a_ranked_mirror_list_with_its_attestation() {
        let path = scratch("resolve", DOC);
        let doc = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(probe_metalink(path.clone(), "test".into()))
            .expect("a local document needs no network");
        assert_eq!(doc.files.len(), 1);
        let f = &doc.files[0];
        assert_eq!(f.name, "big.iso");
        // Best mirror first and the item's own address, so the row a user sees
        // is the source the transfer actually leads with.
        assert_eq!(f.primary, "https://fast.example/big.iso");
        // The rsync mirror is counted as listed and is not a source: a list
        // that silently shrank would be indistinguishable from one the
        // publisher wrote that way.
        assert_eq!(f.mirrors_listed, 3);
        assert_eq!(f.info.mirrors.len(), 2);
        assert_eq!(f.info.mirrors[0].priority, 1);
        assert_eq!(f.info.mirrors[0].max_connections, Some(2));
        assert_eq!(f.info.mirrors[1].priority, 2);
        assert_eq!(f.info.size, Some(4_194_304));
        // The STRONGEST digest, not the first listed: verifying against the md5
        // with a sha256 in hand is a choice, and it is the wrong one.
        assert_eq!(
            f.info.digest.as_deref(),
            Some("sha256:d201bd1eeb17086cd3aaf82b156810a5ba3f389e10b4472c9b2c7182f771a9ef")
        );
        assert_eq!(f.piece_count, 4);
        assert!(f.info.pieces.is_some());
        assert!(!f.info.signed);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_ftp_mirror_ranked_first_by_the_publisher_does_not_capture_the_item() {
        // The engine routes on the item's URL: an ftp primary is not "ftp
        // first", it is the whole transfer dropped to one sequential stream
        // with the mirror list abandoned. metalinker.org's own catix sample
        // ranks its ftp mirrors at preference 100 beside one http mirror.
        let src = r#"<metalink version="3.0" xmlns="http://www.metalinker.org/"><files>
          <file name="c.iso"><size>10</size><resources>
            <url type="ftp" preference="100">ftp://a.example/c.iso</url>
            <url type="http" preference="1">http://h.example/c.iso</url>
          </resources></file></files></metalink>"#;
        let path = scratch("ftp-tier", src);
        let doc = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(probe_metalink(path.clone(), "test".into()))
            .unwrap();
        let f = &doc.files[0];
        assert_eq!(f.primary, "http://h.example/c.iso");
        assert_eq!(f.info.mirrors[0].url, "http://h.example/c.iso");
        // The ftp mirror is not carried as a "reserve" either: the
        // multi-source engine probes and substitutes over HTTP targets, so a
        // mixed list would send its fallback requests to port 21. One
        // transport per item.
        assert_eq!(f.info.mirrors.len(), 1);
        assert_eq!(f.mirrors_listed, 2, "the document still shows both");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_document_is_recognised_by_its_name_or_its_content_but_never_guessed_at() {
        // The name, which is the only signal a remote URL offers before it is
        // fetched.
        assert!(metalink_address("https://example.org/f.iso.metalink"));
        assert!(metalink_address("https://example.org/f.meta4"));
        assert!(metalink_address("https://example.org/f.meta4?token=1"));
        // A redirector URL names nothing; it is settled by `Content-Type` at
        // probe time, not guessed at here.
        assert!(!metalink_address("https://mirrors.example/metalink?repo=x"));
        assert!(!metalink_address("https://example.org/big.iso"));
        assert!(!metalink_address(""));

        // A LOCAL file's content, because a document saved by a browser is as
        // likely to be called `download(1)` as anything else.
        let p = std::env::temp_dir().join(format!("hydra-gui-ml-content-{}", std::process::id()));
        std::fs::write(&p, DOC).unwrap();
        assert!(metalink_address(&p.to_string_lossy()));
        // ...and an unrelated XML file that merely mentions the word is not one:
        // a false positive here means treating a user's real download as a
        // mirror list.
        std::fs::write(&p, "<rss><title>metalink news</title></rss>").unwrap();
        assert!(!metalink_address(&p.to_string_lossy()));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_entry_with_no_fetchable_mirror_is_dropped_rather_than_added_unusable() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="only-rsync"><size>1</size><url>rsync://a/f</url></file>
            <file name="ok"><size>1</size><url>https://a/f</url></file>
          </metalink>"#;
        let path = scratch("norsync", src);
        let doc = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(probe_metalink(path.clone(), "test".into()))
            .unwrap();
        assert_eq!(doc.files.len(), 1);
        assert_eq!(doc.files[0].name, "ok");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_name_that_escapes_the_destination_is_refused_before_anything_is_added() {
        // RFC 5854 4.1.2.1. A downloader that honours it writes to a path
        // whoever served the document chose.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
            <file name="../../etc/cron.d/x"><size>1</size><url>https://a/f</url></file>
          </metalink>"#;
        let path = scratch("escape", src);
        let got = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(probe_metalink(path.clone(), "test".into()));
        assert!(got.is_err(), "an unusable document must not resolve");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pieces_that_do_not_tile_the_size_are_dropped_and_the_file_is_still_offered() {
        // The document contradicts itself. Refusing the file would be wrong —
        // it is perfectly fetchable — and applying the pieces anyway would
        // report every chunk as corrupt.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="f">
            <size>100</size>
            <pieces length="4" type="sha-1">
              <hash>1111111111111111111111111111111111111111</hash>
            </pieces>
            <url>https://a/f</url></file></metalink>"#;
        let path = scratch("mistile", src);
        let doc = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(probe_metalink(path.clone(), "test".into()))
            .unwrap();
        assert_eq!(doc.files[0].piece_count, 0);
        assert!(doc.files[0].info.pieces.is_none());
        assert_eq!(doc.files[0].info.size, Some(100));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn adopting_a_document_keeps_the_users_file_and_falls_back_to_the_first() {
        // The follow path lands on a document that may describe several files,
        // and one job fetches one object. A row the user already aimed at a
        // file must keep pointing at it; only a job that arrived with nothing
        // but a redirector URL takes the first entry.
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="a.iso"><size>10</size>
            <url priority="2">https://s.example/a.iso</url>
            <url priority="1">https://f.example/a.iso</url></file>
          <file name="b.iso"><size>20</size>
            <url>https://f.example/b.iso</url></file>
        </metalink>"#;
        let doc = hya_net::metalink::parse(src).unwrap();

        // The row was aimed at b.iso: it stays aimed there.
        let mut spec = StartSpec {
            final_path: "/tmp/dl/b.iso".into(),
            held: vec![(0, 5)],
            ..StartSpec::plain()
        };
        let (name, size) = adopt_metalink(&doc, &mut spec, 1).unwrap();
        assert_eq!(name, "b.iso");
        assert_eq!(size, Some(20));
        assert_eq!(spec.url, "https://f.example/b.iso");
        assert_eq!(spec.attested_size, Some(20));
        assert!(
            spec.held.is_empty(),
            "held spans described the redirector's own body, not the object"
        );

        // A redirector URL names nothing worth keeping: first entry, renamed,
        // best mirror leading and the ranking dense from 1.
        let mut spec = StartSpec {
            final_path: "/tmp/dl/metalink".into(),
            ..StartSpec::plain()
        };
        let (name, _) = adopt_metalink(&doc, &mut spec, 1).unwrap();
        assert_eq!(name, "a.iso");
        assert_eq!(
            spec.final_path, "/tmp/dl/a.iso",
            "the document names the file"
        );
        assert_eq!(spec.url, "https://f.example/a.iso", "priority 1 leads");
        assert_eq!(
            spec.mirrors.iter().map(|m| m.priority).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            spec.attested_digest.is_none(),
            "no digest published means none invented"
        );
    }

    #[test]
    fn adopting_a_document_with_nothing_fetchable_is_an_error_not_a_stall() {
        let src = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink">
          <file name="../evil"><size>1</size><url>https://a/x</url></file>
          <file name="ok"><size>1</size><url>rsync://a/x</url></file>
        </metalink>"#;
        let doc = hya_net::metalink::parse(src).unwrap();
        let mut spec = StartSpec {
            final_path: "/tmp/dl/x".into(),
            ..StartSpec::plain()
        };
        // One entry escapes the destination, the other has no transport: both
        // are refused, and the job fails with a sentence instead of fetching an
        // attacker-chosen path or nothing.
        assert!(adopt_metalink(&doc, &mut spec, 1).is_err());
    }

    #[test]
    fn a_url_that_names_no_file_says_so() {
        // A URL that names a file: the name means something, and the
        // duplicate check may warn on it.
        assert_eq!(
            url_file_name("https://host/dir/setup.exe"),
            Some("setup.exe".into())
        );
        assert_eq!(
            url_file_name("https://host/a%20b.zip?token=1"),
            Some("a b.zip".into())
        );
        // A URL that names none. `file_name_from_url` still has to answer
        // with somewhere to put the bytes, but callers deciding whether a
        // name is the download's must be able to see that it is invented.
        assert_eq!(url_file_name("https://host/"), None);
        assert_eq!(url_file_name("https://host"), None);
        assert_eq!(url_file_name("https://host/download/?id=7"), None);
        assert_eq!(file_name_from_url("https://host/"), "index.html");
        // ...and a page that really is called index.html is not invented.
        assert_eq!(
            url_file_name("https://host/index.html"),
            Some("index.html".into())
        );
    }

    /// The stamp must survive an ETag. A server sending BOTH headers — GitHub,
    /// S3, most CDNs — is the common case, and reading the collapsed
    /// `validator` field would discard the date for every one of them, which is
    /// what made the option look broken.
    #[test]
    fn remote_stamp_survives_an_etag() {
        let p = Probe {
            validator: Some("\"abc123\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            ..Default::default()
        };
        assert_eq!(remote_stamp(&p), Some(1_445_412_480));
    }

    /// Only a date: the collapsed validator IS the `Last-Modified`, so the
    /// fallback has to read it.
    #[test]
    fn remote_stamp_falls_back_to_a_date_form_validator() {
        let p = Probe {
            validator: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            ..Default::default()
        };
        assert_eq!(remote_stamp(&p), Some(1_445_412_480));
    }

    /// An ETag is opaque: no date, and none invented.
    #[test]
    fn remote_stamp_is_none_without_a_date() {
        let p = Probe {
            validator: Some("\"abc123\"".into()),
            ..Default::default()
        };
        assert_eq!(remote_stamp(&p), None);
        assert_eq!(remote_stamp(&Probe::default()), None);
    }

    #[test]
    fn set_mtime_stamps_the_file() {
        let path = std::env::temp_dir().join("hydra-gui-mtime-test.bin");
        std::fs::write(&path, b"x").expect("write");
        set_mtime(&path, 1_445_412_480).expect("set mtime");
        let got = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("modified")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after epoch")
            .as_secs();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, 1_445_412_480);
    }

    #[test]
    fn parse_url_forms() {
        let u = parse_url("https://example.com/a/b.zip?x=1").unwrap();
        assert!(u.tls);
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/a/b.zip?x=1");
        let u = parse_url("http://host:8080").unwrap();
        assert!(!u.tls);
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/");
        let f = parse_url("ftp://x/y").unwrap();
        assert!(f.ftp);
        assert_eq!(f.port, 21);
        assert!(parse_url("sftp://x/y").is_err());
        assert_eq!(file_name_from_url("https://h/a/b%20c.bin"), "b c.bin");
    }

    /// Every spelling a `Location` header or a redirector page may use.
    #[test]
    fn redirect_targets_resolve_against_the_page_that_served_them() {
        let base = parse_url("https://h.org/a/b/page?q=1").unwrap();
        let j = |loc: &str| join_url(&base, loc);
        assert_eq!(
            j("https://x.org/f.zip").as_deref(),
            Some("https://x.org/f.zip")
        );
        assert_eq!(j("//x.org/f.zip").as_deref(), Some("https://x.org/f.zip"));
        assert_eq!(
            j("/dl/f.zip").as_deref(),
            Some("https://h.org:443/dl/f.zip")
        );
        // Relative to the current DIRECTORY, with the query dropped: joining
        // against `b?q=1` would have produced `/a/b?q=1/f.zip`.
        assert_eq!(j("f.zip").as_deref(), Some("https://h.org:443/a/b/f.zip"));
        // Nothing fetchable, so the chain ends rather than guessing.
        assert_eq!(j("javascript:void(0)"), None);
        assert_eq!(j("  "), None);
    }

    #[test]
    fn base64_rfc() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    /// A/B throughput check: download the same URL twice in one process —
    /// run 1 cold, run 2 over the warm shared pool — and print both rates.
    /// Opt-in via HYDRA_GUI_LIVE_AB=<url>; read logs/gui.log (debug) for the
    /// probe delta and request counts of each run.
    #[test]
    fn live_ab_download() {
        let Some(url) = std::env::var("HYDRA_GUI_LIVE_AB")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        crate::log::set_level("debug");
        ensure_started();
        let mut rx = take_events().expect("events");
        let dir = std::env::temp_dir();
        for run in 1..=2u64 {
            let final_path = dir.join(format!("hydra-ab-{run}.bin"));
            let _ = std::fs::remove_file(&final_path);
            let conns: usize = std::env::var("HYDRA_AB_CONNS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8);
            send(Cmd::Start(Box::new(StartSpec {
                id: run,
                url: url.clone(),
                auth: None,
                conns,
                user_agent: "hydra-gui-ab".into(),
                temp_path: dir
                    .join(format!("hydra-ab-{run}.part"))
                    .to_string_lossy()
                    .into_owned(),
                final_path: final_path.to_string_lossy().into_owned(),
                held: vec![],
                expected_size: None,
                cookies: None,
                limit: None,
                adaptive: std::env::var_os("HYDRA_AB_ADAPTIVE").is_some(),
                remote_time: false,
                ..StartSpec::plain()
            })));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                assert!(std::time::Instant::now() < deadline, "run {run} timed out");
                match rx.blocking_recv().expect("engine alive") {
                    Event::Finished { id, elapsed, size } if id == run => {
                        eprintln!(
                            "run {run}: {size} bytes in {elapsed:.2}s = {:.2} MB/s",
                            size as f64 / elapsed / 1.048576e6
                        );
                        break;
                    }
                    Event::Failed { id, error, .. } if id == run => {
                        panic!("run {run} failed: {error}")
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_file(&final_path);
        }
    }

    /// Live network test of the full MIRROR-LIST path: read a real document,
    /// probe its mirrors, run the multi-source transfer, and verify the bytes
    /// against the digest the document published.
    ///
    /// Opt-in via `HYDRA_GUI_METALINK_TEST=<path or url>`, because it needs the
    /// network and a real mirror list. It is the only thing that exercises the
    /// GUI's own `plan_sources` and `verify_attested` end to end — the unit
    /// tests above stop at resolution, and a bug between resolution and the
    /// transfer would be invisible to them.
    #[test]
    fn live_metalink() {
        let Some(src) = std::env::var("HYDRA_GUI_METALINK_TEST")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let doc = rt
            .block_on(probe_metalink(src, "hydra-gui-test".into()))
            .expect("the document must resolve");
        let f = doc.files.first().expect("at least one file").clone();
        eprintln!(
            "metalink {}: {:?}, {} mirror(s), {} bytes, digest {:?}",
            doc.version,
            f.name,
            f.info.mirrors.len(),
            f.info.size.unwrap_or(0),
            f.info.digest
        );
        assert!(
            f.info.mirrors.len() > 1,
            "this test is about MULTI-source; the document offered {}",
            f.info.mirrors.len()
        );

        ensure_started();
        let mut rx = take_events().expect("events");
        let dir = std::env::temp_dir();
        let final_path = dir.join(format!("hydra-gui-metalink-{}", f.name));
        let _ = std::fs::remove_file(&final_path);
        send(Cmd::Start(Box::new(StartSpec {
            id: 1,
            url: f.primary.clone(),
            conns: 8,
            user_agent: "hydra-gui-test".into(),
            temp_path: dir
                .join("hydra-gui-metalink.part")
                .to_string_lossy()
                .into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
            mirrors: f.info.mirrors.clone(),
            attested_size: f.info.size,
            attested_digest: f.info.digest.clone(),
            pieces: f.info.pieces.clone(),
            ..StartSpec::plain()
        })));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out");
            match rx.blocking_recv().expect("engine alive") {
                Event::Finished {
                    id: 1,
                    elapsed,
                    size,
                } => {
                    eprintln!(
                        "{size} bytes in {elapsed:.2}s = {:.2} MB/s",
                        size as f64 / elapsed / 1.048576e6
                    );
                    // The engine verifies against the document BEFORE the
                    // rename, so a file that exists at the destination has
                    // already passed its digest. Checking the size here catches
                    // the one thing that check cannot: a rename that landed
                    // somewhere else.
                    let on_disk = std::fs::metadata(&final_path).expect("the file must exist");
                    if let Some(want) = f.info.size {
                        assert_eq!(on_disk.len(), want, "size must match the document");
                    }
                    break;
                }
                // `Failed` is the interesting outcome to report in full: a
                // digest mismatch and an unreachable mirror read very
                // differently, and a bare `panic!("failed")` would hide which.
                Event::Failed {
                    id: 1, error, done, ..
                } => {
                    panic!("failed after {done} bytes: {error}")
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&final_path);
    }

    /// Live test of the case a user actually hits: a REDIRECTOR URL pasted into
    /// Add URL, which reveals itself as a mirror list only through its
    /// `Content-Type`.
    ///
    /// Nothing the dialog can read tells it what
    /// `https://mirrors.fedoraproject.org/metalink?repo=...` is, so the engine
    /// has to notice on the probe it was going to make anyway. Getting this
    /// wrong is not a crash: it saves a few kilobytes of XML under the name of
    /// the object the user asked for, and reports success.
    ///
    /// Opt-in via `HYDRA_GUI_METALINK_URL_TEST=<url>`.
    #[test]
    fn live_metalink_redirector_url() {
        let Some(url) = std::env::var("HYDRA_GUI_METALINK_URL_TEST")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        ensure_started();
        let mut rx = take_events().expect("events");
        let dir = std::env::temp_dir().join(format!("hydra-gui-mlurl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Deliberately the name the URL implies, which is what the dialog would
        // have derived: the document must overwrite it.
        let guessed = dir.join("metalink");
        send(Cmd::Start(Box::new(StartSpec {
            id: 1,
            url,
            conns: 4,
            user_agent: "hydra-gui-test".into(),
            temp_path: dir.join("metalink.part").to_string_lossy().into_owned(),
            final_path: guessed.to_string_lossy().into_owned(),
            ..StartSpec::plain()
        })));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut named: Option<String> = None;
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out");
            match rx.blocking_recv().expect("engine alive") {
                Event::Probed {
                    id: 1,
                    file_name,
                    size,
                    ..
                } => {
                    eprintln!("probed: {file_name:?} {size:?}");
                    if let Some(n) = file_name {
                        named = Some(n);
                    }
                }
                Event::Finished { id: 1, size, .. } => {
                    let name = named.expect("the document must have named the file");
                    assert_ne!(
                        name, "metalink",
                        "the row must be renamed from the redirector's own name"
                    );
                    let landed = dir.join(&name);
                    assert!(
                        landed.exists(),
                        "the object must land under the document's name, not the URL's"
                    );
                    assert!(!guessed.exists(), "the XML must not be saved as the object");
                    eprintln!("{size} bytes as {name}");
                    break;
                }
                Event::Failed { id: 1, error, .. } => panic!("failed: {error}"),
                _ => {}
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live network test of the full engine path (probe -> scheduler ->
    /// rename): opt-in via HYDRA_GUI_LIVE_TEST=<url>.
    #[test]
    fn live_download() {
        let Some(url) = std::env::var("HYDRA_GUI_LIVE_TEST")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        ensure_started();
        let mut rx = take_events().expect("events");
        let dir = std::env::temp_dir();
        let final_path = dir.join("hydra-gui-live-test.bin");
        let _ = std::fs::remove_file(&final_path);
        send(Cmd::Start(Box::new(StartSpec {
            id: 1,
            url,
            auth: None,
            conns: 8,
            user_agent: "hydra-gui-test".into(),
            temp_path: dir
                .join("hydra-gui-live-test.part")
                .to_string_lossy()
                .into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
            held: vec![],
            expected_size: None,
            cookies: None,
            limit: None,
            adaptive: false,
            // On, so the harness exercises the stamping path too: it is part
            // of what "the file lands correctly on disk" means now.
            remote_time: true,
            ..StartSpec::plain()
        })));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut probed_size = None;
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out");
            match rx.blocking_recv().expect("engine alive") {
                Event::Probed { size, .. } => probed_size = size,
                Event::Finished { size, .. } => {
                    let meta = std::fs::metadata(&final_path).expect("final file");
                    assert_eq!(meta.len(), size);
                    if let Some(ps) = probed_size {
                        assert_eq!(ps, size);
                    }
                    let mtime = meta
                        .modified()
                        .expect("modified")
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("after epoch")
                        .as_secs();
                    eprintln!("live download ok: {size} bytes, mtime {mtime}");
                    break;
                }
                Event::Failed { error, .. } => panic!("download failed: {error}"),
                _ => {}
            }
        }
    }
}

// ------------------------------------------------------------ streams

/// Build a `Target` for one URL, carrying the browser's session with it.
///
/// Cookies and Referer are the whole point: a stream that plays only for a
/// signed-in browser must be fetched with the same credentials, and the
/// extension already collected them for the manifest's origin.
fn stream_target(
    seg: &hya_stream::Segment,
    cookies: Option<&str>,
    referer: Option<&str>,
    agent: &str,
) -> Result<Target, String> {
    let u = parse_url(&seg.url)?;
    let base = if u.tls {
        Target::direct_tls(&u.host, u.port, &u.path)
    } else {
        Target::direct(&u.host, u.port, &u.path)
    };
    let mut headers = Vec::new();
    if let Some(c) = cookies.filter(|c| !c.is_empty()) {
        headers.push(format!("Cookie: {c}"));
    }
    if let Some(r) = referer.filter(|r| !r.is_empty()) {
        headers.push(format!("Referer: {r}"));
    }
    // A playlist may carve every segment out of ONE file; without this the
    // whole file is fetched once per segment.
    if let Some(range) = seg.range_header() {
        headers.push(format!("Range: {range}"));
    }
    Ok(base.with_headers(headers, Some(agent.to_string())))
}

/// Fetch one bounded body with the stream's session headers.
/// A segment redirected elsewhere: the same segment (byte range included) at
/// the address the origin named, or `None` if this was not a redirect.
///
/// The range MUST travel with it. A playlist that carves segments out of one
/// file redirected to an edge would otherwise fetch the whole file.
fn redirect_target(seg: &hya_stream::Segment, e: &std::io::Error) -> Option<hya_stream::Segment> {
    let loc = &hya_net::Redirect::of(e)?.location;
    let url = hya_stream::join(&seg.url, loc)?;
    Some(hya_stream::Segment {
        url,
        range: seg.range,
        // The key travels too: a redirected segment is the same segment.
        key: seg.key.clone(),
    })
}

/// Hops a manifest fetch will follow. A CDN commonly answers the published
/// URL with a redirect to a regional edge, sometimes twice.
const MAX_REDIRECTS: usize = 5;

/// Fetch a manifest, following redirects, and report the URL it finally came
/// from — every relative URI inside resolves against THAT, so parsing
/// against the address originally asked for would aim every segment at the
/// wrong host.
async fn stream_get_at(
    connector: &Arc<TlsCapableConnector>,
    url: &str,
    cookies: Option<&str>,
    referer: Option<&str>,
    agent: &str,
    cap: usize,
) -> std::io::Result<(Vec<u8>, String)> {
    let mut at = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        let t = stream_target(&hya_stream::Segment::new(&at), cookies, referer, agent)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        match hya_net::fetch_small(connector.as_ref(), &t, cap).await {
            Ok(body) => return Ok((body, at)),
            Err(e) => {
                // `fetch_small` hands back the destination; a redirect is a
                // hop to take, not a failure to report.
                let Some(r) = hya_net::Redirect::of(&e) else {
                    return Err(e);
                };
                let Some(next) = hya_stream::join(&at, &r.location) else {
                    return Err(e);
                };
                at = next;
            }
        }
    }
    Err(std::io::Error::other("too many redirects"))
}

async fn stream_get(
    connector: &Arc<TlsCapableConnector>,
    url: &str,
    cookies: Option<&str>,
    referer: Option<&str>,
    agent: &str,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    stream_get_at(connector, url, cookies, referer, agent, cap)
        .await
        .map(|(body, _)| body)
}

/// The Progress/Status ticker both the VOD and the live paths report through.
///
/// `total_segments` is `None` for a recording: a live stream has no total, so
/// there is no percentage and no ETA to compute, and claiming one would be a
/// lie that only gets more wrong the longer it runs.
#[allow(clippy::too_many_arguments)]
fn spawn_ticker(
    id: DlId,
    meter: &Arc<hya_stream::hls::Meter>,
    tx: &UnboundedSender<Event>,
    cancel: &Arc<AtomicBool>,
    total_segments: Option<u64>,
    estimated: u64,
    // Milliseconds of media captured, fed by a live recorder. A recording
    // has no size to show a percentage against, so this is what the dialog
    // reports in its place.
    media_ms: Option<Arc<AtomicU64>>,
) -> tokio::task::JoinHandle<()> {
    let (meter, tx, cancel) = (meter.clone(), tx.clone(), cancel.clone());
    tokio::spawn(async move {
        let mut last_bytes = 0u64;
        let mut last_at = std::time::Instant::now();
        let mut last_line = String::new();
        let mut smoothed = 0.0f64;
        let mut announced = estimated;
        // A projection that is itself smoothed. The raw one moves in steps —
        // it only changes when a whole segment settles — and a bar drawn
        // against a stepping denominator stutters however smooth the
        // numerator is.
        let mut smooth_total = estimated as f64;
        // Matches what the range path emits at, so a stream's dialog is as
        // alive as a file's. The old 300 ms was three updates a second
        // against the range path's fifty, which is the whole reason streams
        // looked jerky next to ordinary downloads.
        let tick = if POWER_SAVE.load(Ordering::Relaxed) {
            std::time::Duration::from_millis(400)
        } else {
            std::time::Duration::from_millis(100)
        };
        loop {
            tokio::time::sleep(tick).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            // Totals only: the in-flight list is needed just for the
            // fallback rows below, and building it every tick cloned a label
            // per segment in flight, ten times a second, to discard it.
            let (bytes, segs) = meter.totals();
            let now = std::time::Instant::now();
            let dt = now.duration_since(last_at).as_secs_f64().max(0.001);
            let instant = bytes.saturating_sub(last_bytes) as f64 / dt;
            last_bytes = bytes;
            last_at = now;
            // A time-constant EMA (~1.5 s), not a per-tick one: the rate then
            // means the same thing whatever the tick rate is, and a segment
            // boundary no longer reads as a collapse to zero.
            let alpha = 1.0 - (-dt / 1.5).exp();
            smoothed += alpha * (instant - smoothed);

            // Once segments have landed, MEASURED bytes-per-segment beats the
            // manifest's bitrate estimate, so the size converges on the truth
            // instead of staying at a guess. A recording has no total to
            // converge on and reports what is on disk instead.
            let measured = match total_segments {
                Some(total) if segs > 0 => {
                    ((meter.settled() as f64 / segs as f64) * total as f64).max(bytes as f64)
                }
                Some(_) => estimated as f64,
                None => bytes as f64,
            };
            // Same time constant as the rate, so size and speed settle
            // together rather than one chasing the other.
            if smooth_total <= 0.0 {
                smooth_total = measured;
            } else {
                // Floored at what is actually on disk. `measured` already
                // is, but the EMA LAGS it: right after a jump the smoothed
                // total sits below `bytes`, and a size smaller than the
                // downloaded count draws a bar past 100%.
                smooth_total = (smooth_total + alpha * (measured - smooth_total)).max(bytes as f64);
            }
            let projected = smooth_total as u64;
            // A tighter threshold than before because the value no longer
            // jumps: this is what lets the bar glide instead of stepping.
            if projected > 0 && projected.abs_diff(announced) > announced.max(1) / 100 {
                announced = projected;
                let _ = tx.send(Event::Probed {
                    id,
                    size: total_segments.map(|_| projected),
                    ranges: false,
                    file_name: None,
                });
            }
            let eta = (total_segments.is_some() && smoothed > 512.0 && projected > bytes)
                .then(|| ((projected - bytes) as f64 / smoothed) as u64);
            // The status line changes at segment granularity; re-sending it
            // a hundred times a second would be a hundred redraws saying the
            // same thing.
            let line_now = match total_segments {
                Some(total) => format!("Segment {}/{total}", segs.min(total)),
                None => format!("{} {segs}", crate::i18n::tr("Recording, segments:")),
            };
            let recorded = media_ms
                .as_ref()
                .map(|m| m.load(Ordering::Relaxed) as f64 / 1000.0);
            // One row per CONNECTION, not per segment in flight.
            //
            // Rows built from the in-flight list — which is what this did —
            // were wrong in three ways at once: they SHUFFLED, because
            // settling a segment compacts the list under the reader; they
            // reset to zero every few seconds, because a row showed one
            // segment's bytes rather than the connection's running total;
            // and they outnumbered the configured connections three to one,
            // because the pipeline queues well ahead of what the gate admits.
            // Lanes are claimed against a held permit, so a row is a
            // connection that is really transferring, and it keeps its total
            // and its outcome once the work moves on.
            let lanes = meter.lanes();
            let conns: Vec<ConnRow> = if lanes.is_empty() {
                meter
                    .snapshot()
                    .2
                    .iter()
                    .map(|f| ConnRow {
                        downloaded: f.bytes,
                        info: crate::i18n::tr(if f.bytes > 0 {
                            "Receiving data..."
                        } else {
                            "Connecting..."
                        }),
                    })
                    .collect()
            } else {
                lanes
                    .iter()
                    .map(|l| ConnRow {
                        downloaded: l.bytes,
                        info: match l.state {
                            // Never used: left blank rather than labelled,
                            // so spare capacity reads as spare capacity.
                            hya_stream::hls::LaneState::Unused => String::new(),
                            hya_stream::hls::LaneState::Connecting => {
                                crate::i18n::tr("Connecting...")
                            }
                            hya_stream::hls::LaneState::Receiving => {
                                crate::i18n::tr("Receiving data...")
                            }
                            hya_stream::hls::LaneState::Completed => crate::i18n::tr("Completed."),
                            hya_stream::hls::LaneState::Failed => crate::i18n::tr("Disconnect."),
                        },
                    })
                    .collect()
            };
            if tx
                .send(Event::Progress {
                    id,
                    done: bytes,
                    rate: smoothed,
                    eta,
                    conns,
                    held: vec![(0, bytes)],
                    recorded,
                })
                .is_err()
            {
                return;
            }
            if line_now != last_line {
                last_line = line_now.clone();
                let _ = tx.send(Event::Status { id, line: line_now });
            }
        }
    })
}

/// The closure that fetches one segment, carrying the browser's session.
fn segment_fetcher(
    spec: &StreamSpec,
    connector: &Arc<TlsCapableConnector>,
    limiter: &Arc<RateLimiter>,
    cancel: &Arc<AtomicBool>,
) -> impl hya_stream::Fetcher {
    let (connector, ck, rf, ua, limiter, cancel) = (
        connector.clone(),
        spec.cookies.clone(),
        spec.referer.clone(),
        spec.user_agent.clone(),
        limiter.clone(),
        cancel.clone(),
    );
    move |seg: hya_stream::Segment, dest: String, counter: Arc<AtomicU64>| -> hya_stream::FetchSeg {
        let (connector, ck, rf, ua, limiter, cancel) = (
            connector.clone(),
            ck.clone(),
            rf.clone(),
            ua.clone(),
            limiter.clone(),
            cancel.clone(),
        );
        Box::pin(async move {
            // A CDN may bounce a segment to a regional edge; follow it.
            let mut seg = seg;
            for hop in 0..=MAX_REDIRECTS {
                let t = stream_target(&seg, ck.as_deref(), rf.as_deref(), &ua)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                // Streamed to a staging file rather than buffered: memory stays
                // flat whatever the segment size, and `counter` can be read while
                // the bytes are still arriving.
                //
                // `fetch_object`, not `fetch_streaming_observed`: a playlist is
                // hundreds of small objects on ONE origin, so the socket is kept
                // and reused. Otherwise every segment pays a fresh TCP and TLS
                // handshake, which on a distant origin is most of the wall clock.
                let pace = hya_net::polite::Pace::shared(limiter.clone());
                let pool = hya_net::Connector::pool(connector.as_ref());
                match hya_net::fetch_object(
                    connector.as_ref(),
                    &t,
                    &dest,
                    &counter,
                    Some(&cancel),
                    &pace,
                    pool.as_ref(),
                )
                .await
                {
                    Ok(n) => return Ok(n),
                    Err(e) if hop < MAX_REDIRECTS => {
                        let Some(next) = redirect_target(&seg, &e) else {
                            return Err(e);
                        };
                        seg = next;
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(std::io::Error::other("too many redirects"))
        })
    }
}

/// Why a protected stream is refused, in the user's language.
///
/// The DRM system is named separately from the explanation so the catalogue
/// key stays the same whichever system it is — and so a new system needs no
/// new translation.
pub fn drm_refusal(system: &str) -> String {
    format!(
        "{system} DRM: {}",
        crate::i18n::tr(
            "this stream is protected. Hydra does not bypass the technical measures that protect audio and video content, so it cannot be downloaded."
        )
    )
}

/// Where a recording re-reads its segment list from.
enum LiveSource {
    /// A media playlist, re-read for new `#EXT-X-MEDIA-SEQUENCE` entries.
    Hls { url: String },
    /// A dynamic MPD, re-read for new `$Number$` entries. The Representation
    /// ids are pinned so a refresh cannot silently switch rendition.
    Dash {
        url: String,
        video_id: String,
        audio_id: Option<String>,
    },
}

/// One track's current window.
struct Window {
    init: Option<hya_stream::Segment>,
    /// `(sequence number, segment, seconds of media)`.
    segments: Vec<(u64, hya_stream::Segment, f64)>,
    kind: hya_stream::hls::Segments,
}

/// Re-read the source and report each track's current window, whether the
/// stream has ended, and how long to wait before asking again.
async fn live_windows(
    source: &LiveSource,
    connector: &Arc<TlsCapableConnector>,
    spec: &StreamSpec,
) -> Result<(Vec<Window>, bool, std::time::Duration), String> {
    let (ck, rf, ua) = (
        spec.cookies.as_deref(),
        spec.referer.as_deref(),
        spec.user_agent.as_str(),
    );
    let cap = hya_stream::hls::playlist_cap();
    match source {
        LiveSource::Hls { url } => {
            let body = stream_get(connector, url, ck, rf, ua, cap)
                .await
                .map_err(|e| format!("could not re-read the playlist: {e}"))?;
            let pl = hya_stream::hls::parse(&String::from_utf8_lossy(&body), url);
            if let Some(d) = &pl.drm {
                return Err(drm_refusal(d));
            }
            if let Some(enc) = &pl.encryption {
                return Err(format!("{enc} encrypted streams are not supported yet"));
            }
            let refresh = pl.refresh_after();
            Ok((
                vec![Window {
                    init: pl.init.clone(),
                    segments: pl.timed_window(),
                    kind: pl.segments_kind.unwrap_or(hya_stream::hls::Segments::Ts),
                }],
                pl.ended,
                refresh,
            ))
        }
        LiveSource::Dash {
            url,
            video_id,
            audio_id,
        } => {
            let body = stream_get(connector, url, ck, rf, ua, cap)
                .await
                .map_err(|e| format!("could not re-read the manifest: {e}"))?;
            let mf = hya_stream::dash::parse(&String::from_utf8_lossy(&body), url);
            if let Some(d) = &mf.drm {
                return Err(drm_refusal(d));
            }
            let refresh = mf.refresh_after();
            // Pinned by id: a refresh must not switch rendition underneath a
            // half-written file.
            let Some(video) = mf.video.iter().find(|t| &t.id == video_id) else {
                return Err("the manifest no longer offers that video rendition".into());
            };
            let mut windows = vec![Window {
                init: video.init.clone(),
                segments: hya_stream::dash::Manifest::timed_window(video),
                kind: hya_stream::hls::Segments::Fmp4,
            }];
            if let Some(aid) = audio_id {
                let Some(audio) = mf.audio.iter().find(|t| &t.id == aid) else {
                    return Err("the manifest no longer offers that audio rendition".into());
                };
                windows.push(Window {
                    init: audio.init.clone(),
                    segments: hya_stream::dash::Manifest::timed_window(audio),
                    kind: hya_stream::hls::Segments::Fmp4,
                });
            }
            // A dynamic manifest never "ends"; only the user stops it.
            Ok((windows, false, refresh))
        }
    }
}

/// Record a live stream until it ends or the user stops it.
///
/// The shape is different from a VOD download in one way that matters: there
/// is no list to work through, so the loop is "re-read, take what is new,
/// wait". A live window slides, which is why segments are identified by
/// sequence number (`#EXT-X-MEDIA-SEQUENCE` + position, or `$Number$`) rather
/// than by position — position changes every refresh.
///
/// Stopping is a SUCCESS, not a cancellation. Someone recording a live stream
/// and pressing Stop wants the file they have, and there is nothing to resume
/// into later: the bytes they did not take are gone from the origin.
#[allow(clippy::too_many_arguments)]
async fn record_live(
    spec: &StreamSpec,
    source: LiveSource,
    primed: Option<(Vec<Window>, bool, std::time::Duration)>,
    connector: &Arc<TlsCapableConnector>,
    limiter: &Arc<RateLimiter>,
    cancel: &Arc<AtomicBool>,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
) {
    let id = spec.id;
    let ev = |e: Event| {
        let _ = tx.send(e);
    };
    let fail = |msg: String| {
        crate::log::error(&format!("#{id} recording failed: {msg}"));
        let _ = tx.send(Event::Failed {
            id,
            error: msg,
            done: 0,
            held: vec![],
            permission_denied: false,
        });
    };

    let tracks = match &source {
        LiveSource::Hls { .. } => 1,
        LiveSource::Dash { audio_id, .. } => 1 + usize::from(audio_id.is_some()),
    };
    if let Some(dir) = std::path::Path::new(&spec.temp_path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut files = Vec::with_capacity(tracks);
    let mut parts = Vec::with_capacity(tracks);
    for i in 0..tracks {
        let part = format!("{}.t{i}", spec.temp_path);
        match std::fs::File::create(&part) {
            Ok(f) => {
                files.push(f);
                parts.push(part);
            }
            Err(e) => return fail(friendly_io(&e, &part)),
        }
    }

    let meter = Arc::new(hya_stream::hls::Meter::default());
    // The recorder's media clock, in milliseconds so the ticker can read it
    // from an atomic rather than behind a lock.
    let media_ms = Arc::new(AtomicU64::new(0));
    let ticker = spawn_ticker(id, &meter, tx, cancel, None, 0, Some(media_ms.clone()));
    let fetch = segment_fetcher(spec, connector, limiter, cancel);
    let t0 = std::time::Instant::now();

    // Seconds of media PLANNED per track. Kept per track because video and
    // audio cover the same span: a single shared counter would be filled by
    // the video track and then leave nothing for the audio one, silently
    // recording a video-only file.
    let mut planned_secs = vec![0f64; tracks];
    let mut last: Vec<Option<u64>> = vec![None; tracks];
    let mut init_done = vec![false; tracks];
    let mut kinds = vec![hya_stream::hls::Segments::Ts; tracks];
    let mut taken = 0u64;
    let mut skipped = 0u64;
    let mut error: Option<String> = None;
    // A manifest that never yields a segment would otherwise loop forever
    // waiting for one. Give it a few refreshes, then say so.
    let mut barren = 0u32;
    const BARREN_LIMIT: u32 = 3;
    // How far BELOW the last taken number counts as a restarted
    // sequence rather than a stale republish. A live window is a
    // handful of segments; anything this far back is a new sequence.
    const RESET_GAP: u64 = 64;
    // A live stream never ends on its own. When the user asked for a fixed
    // length, that is what stops it — and stopping is a success: the file is
    // finished and playable, exactly as if the broadcast had ended.
    let deadline = spec
        .max_seconds
        .map(|s| t0 + std::time::Duration::from_secs(s));

    // Segments in flight, and so the rows in the connection table. Resolved
    // exactly as the VOD path resolves it: 0 means "no preference", which is
    // the DEFAULT, not one connection.
    let conns = hya_stream::hls::Concurrency::fixed(spec.conns).ceiling();
    let gate = Arc::new(tokio::sync::Semaphore::new(conns));
    meter.open_lanes(conns);
    // Said out loud because the alternative is a support thread: a recording
    // that looks slow and a connection table with one busy row is either a
    // stream that publishes slowly or a client that is not asking for more,
    // and only this line tells the two apart.
    crate::log::info(&format!(
        "#{id} recording with {conns} connection(s), {} track(s)",
        tracks
    ));

    // The caller already read the manifest to work out that this IS live.
    // Starting from that read rather than asking again saves a request and,
    // more importantly, closes the gap in which the window could slide and
    // take a segment with it.
    let mut primed = primed;
    'recording: loop {
        let refreshed = match primed.take() {
            Some(w) => Ok(w),
            None => live_windows(&source, connector, spec).await,
        };
        let (windows, ended, refresh) = match refreshed {
            Ok(w) => w,
            Err(e) => {
                // A refresh that fails after something was recorded is not
                // worth throwing the recording away for.
                if taken > 0 {
                    crate::log::warn(&format!("#{id} recording stopped: {e}"));
                    break 'recording;
                }
                error = Some(e);
                break 'recording;
            }
        };

        for (i, w) in windows.iter().enumerate().take(tracks) {
            kinds[i] = w.kind;
            crate::log::debug(&format!(
                "#{id} track {i}: window of {} segments, seq {}..{}, refresh {:?}",
                w.segments.len(),
                w.segments.first().map(|(s, _, _)| *s).unwrap_or(0),
                w.segments.last().map(|(s, _, _)| *s).unwrap_or(0),
                refresh
            ));
            // The init map has to be the first thing in the file.
            if !init_done[i] {
                if let Some(init) = &w.init {
                    let dest = format!("{}.i{i}", spec.temp_path);
                    let counter = Arc::new(AtomicU64::new(0));
                    meter.begin(format!("init {i}"), counter.clone());
                    // An init map is load-bearing: it carries ftyp+moov, and
                    // fragments written without it in front are not a file
                    // any player will open. A failure to WRITE it is as
                    // fatal as a failure to fetch it.
                    let placed = match fetch(init.clone(), dest.clone(), counter.clone()).await {
                        Ok(n) => append_file(&dest, &mut files[i]).map(|()| n),
                        Err(e) => Err(e),
                    };
                    match placed {
                        Ok(n) => meter.settle(&counter, n),
                        Err(e) => {
                            meter.drop_inflight(&counter);
                            let _ = std::fs::remove_file(&dest);
                            error = Some(format!("could not place the init segment: {e}"));
                            break 'recording;
                        }
                    }
                }
                init_done[i] = true;
            }

            // The window is PLANNED in order, then FETCHED concurrently.
            //
            // The checks below are stateful: each one's answer depends on
            // what the previous segment did to `last[i]`, so they must run
            // in sequence. The FETCHING need not, and used not to be
            // separated from them — which made a recording run at one
            // connection no matter how many were configured. A refresh
            // commonly reveals several segments at once, and the first one
            // hands over the whole window as a backlog, so there is real
            // work to overlap here.
            let mut wanted: Vec<(u64, hya_stream::Segment, f64)> = Vec::new();
            for (seq, url, secs) in &w.segments {
                // A restarted encoder renumbers from a low value. Without
                // this, every segment after a restart looks "already seen"
                // and the recording silently freezes until the numbering
                // climbs back past the old high-water mark — which for a
                // fresh sequence never happens.
                if let Some(l) = last[i] {
                    if seq.saturating_add(RESET_GAP) < l {
                        crate::log::info(&format!(
                            "#{id} sequence restarted at {seq} (was {l}); continuing"
                        ));
                        last[i] = None;
                    }
                }
                if last[i].is_some_and(|l| *seq <= l) {
                    continue;
                }
                // A window that slid past what we last took means the origin
                // dropped segments before we asked: a real gap, worth saying.
                if let Some(l) = last[i] {
                    if *seq > l + 1 {
                        // The origin dropped segments before we asked: a
                        // real gap in the recording, and one worth naming
                        // rather than leaving to be noticed on playback.
                        crate::log::warn(&format!(
                            "#{id} track {i}: window slid past {} segment(s) ({}..{})",
                            *seq - l - 1,
                            l + 1,
                            *seq - 1
                        ));
                        skipped += *seq - l - 1;
                    }
                }
                // Marked taken at PLANNING time, as it was before: a segment
                // that then fails to fetch is a gap the recording moves past,
                // not one it retries into a stall.
                last[i] = Some(*seq);
                wanted.push((*seq, url.clone(), *secs));
            }

            if cancel.load(Ordering::Relaxed)
                || deadline.is_some_and(|d| std::time::Instant::now() >= d)
            {
                break 'recording;
            }

            // A length ask is a promise about how much MEDIA comes back, so
            // it cuts the plan rather than the loop. The rule itself lives in
            // hya-stream, shared with the CLI recorder.
            if let Some(max) = spec.max_seconds {
                hya_stream::hls::trim_to_budget(
                    &mut wanted,
                    &mut planned_secs[i],
                    max as f64,
                    |(_, _, secs)| *secs,
                );
            }

            // Every segment in the window is queued at once; the gate decides
            // how many are actually in flight.
            let mut queued: std::collections::VecDeque<_> = std::collections::VecDeque::new();
            for (n, (seq, url, secs)) in wanted.into_iter().enumerate() {
                // Distinct per segment: one shared staging name would have
                // concurrent fetches writing over each other.
                let dest = format!("{}.s{i}.{n}", spec.temp_path);
                let counter = Arc::new(AtomicU64::new(0));
                meter.begin(format!("Segment {seq}"), counter.clone());
                let (f, g, d2, c2, m2) = (
                    fetch.clone(),
                    gate.clone(),
                    dest.clone(),
                    counter.clone(),
                    meter.clone(),
                );
                let task = tokio::spawn(async move {
                    let _permit = match g.acquire().await {
                        Ok(p) => p,
                        Err(_) => return Err(std::io::Error::other("fetch gate closed")),
                    };
                    // The permit is the connection, so the row is taken here.
                    let lane = m2.occupy(&c2);
                    // Bounded exactly as the VOD path bounds an attempt. An
                    // origin that accepts and then goes SILENT never completes
                    // a read, and `fetch_object` only tests the cancel flag
                    // between reads — so without this the append loop blocks
                    // forever on `task.await`: the recording freezes, the
                    // deadline never fires, and Cancel does nothing.
                    let r = match tokio::time::timeout(
                        hya_stream::hls::ATTEMPT_TIMEOUT,
                        f(url, d2, c2.clone()),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "live segment stalled",
                        )),
                    };
                    if let Some(l) = lane {
                        l.finish(r.is_ok());
                    }
                    r
                });
                queued.push_back((seq, secs, dest, counter, task));
            }

            // Appended in PLAYLIST order however they land, which is what
            // keeps the output playable. A cancel drains the rest rather
            // than breaking out, so no staging file is left behind.
            let mut interrupted = false;
            while let Some((seq, secs, dest, counter, task)) = queued.pop_front() {
                let outcome = match task.await {
                    Ok(r) => r,
                    Err(e) => Err(std::io::Error::other(format!("segment task: {e}"))),
                };
                if interrupted {
                    meter.drop_inflight(&counter);
                    let _ = std::fs::remove_file(&dest);
                    continue;
                }
                match outcome {
                    Ok(n) => {
                        if append_file(&dest, &mut files[i]).is_ok() {
                            meter.settle(&counter, n);
                            taken += 1;
                            // The media clock advances on the FIRST track
                            // only: video and audio cover the same span, and
                            // counting both would double it.
                            if i == 0 {
                                media_ms.fetch_add((secs * 1000.0) as u64, Ordering::Relaxed);
                            }
                        } else {
                            meter.drop_inflight(&counter);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        meter.drop_inflight(&counter);
                        let _ = std::fs::remove_file(&dest);
                        interrupted = true;
                    }
                    Err(e) => {
                        // One bad segment in a live stream is a gap, not the
                        // end of the recording.
                        meter.drop_inflight(&counter);
                        let _ = std::fs::remove_file(&dest);
                        crate::log::warn(&format!("#{id} segment {seq} lost: {e}"));
                        skipped += 1;
                    }
                }
            }
            if interrupted {
                break 'recording;
            }
        }

        if taken == 0 {
            barren += 1;
            if barren >= BARREN_LIMIT {
                error = Some("the manifest published no segments; nothing to record".into());
                break;
            }
        }
        // The budget is spent: stop, rather than refresh into windows that
        // the cut above will now empty every time — which reads to the
        // barren-window guard as an origin publishing nothing.
        if spec
            .max_seconds
            .is_some_and(|m| planned_secs[0] >= m as f64)
        {
            crate::log::info(&format!(
                "#{id} recorded the {} s asked for",
                spec.max_seconds.unwrap_or(0)
            ));
            break 'recording;
        }

        if ended {
            crate::log::info(&format!("#{id} the stream ended"));
            break;
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            crate::log::info(&format!(
                "#{id} recorded the requested {}s",
                spec.max_seconds.unwrap_or(0)
            ));
            break;
        }
        // Sleep in slices so Stop is responsive rather than up to a full
        // refresh period away.
        let until = std::time::Instant::now() + refresh;
        while std::time::Instant::now() < until {
            if cancel.load(Ordering::Relaxed) {
                break 'recording;
            }
            if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                // `break 'recording`, not `break`: a plain break leaves only
                // the SLEEP, and the outer loop would then re-read the
                // manifest and fetch a whole further window before noticing
                // the deadline — overrunning a "record 60 s" ask by most of
                // a refresh period.
                crate::log::info(&format!(
                    "#{id} recorded the requested {}s",
                    spec.max_seconds.unwrap_or(0)
                ));
                break 'recording;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    ticker.abort();
    for i in 0..tracks {
        let _ = std::fs::remove_file(format!("{}.s{i}", spec.temp_path));
        let _ = std::fs::remove_file(format!("{}.i{i}", spec.temp_path));
    }
    drop(files);

    if let Some(e) = error {
        for p in &parts {
            let _ = std::fs::remove_file(p);
        }
        return fail(e);
    }
    if taken == 0 {
        for p in &parts {
            let _ = std::fs::remove_file(p);
        }
        return fail("the stream produced no segments".into());
    }

    // Finalise exactly as a VOD download does.
    let want_ext = if spec.container.eq_ignore_ascii_case("ts") {
        "ts"
    } else {
        "mp4"
    };
    let final_str = final_path
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| spec.final_path.clone());
    let final_p = std::path::Path::new(&final_str);
    if let Some(dir) = final_p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    ev(Event::Status {
        id,
        line: crate::i18n::tr("Finishing the recording..."),
    });

    let result = if parts.len() == 2 {
        hya_stream::hls::mux(
            std::path::Path::new(&parts[0]),
            std::path::Path::new(&parts[1]),
            final_p,
        )
        .map_err(|e| {
            let v = final_p.with_extension("video.mp4");
            let a = final_p.with_extension("audio.m4a");
            let _ = std::fs::rename(&parts[0], &v);
            let _ = std::fs::rename(&parts[1], &a);
            format!(
                "{e} (video saved as {}, audio as {})",
                v.display(),
                a.display()
            )
        })
        .map(|()| hya_stream::hls::Finished::Remuxed)
    } else {
        hya_stream::hls::finish(kinds[0], want_ext, std::path::Path::new(&parts[0]), final_p)
            .map_err(|e| {
                // Whatever was recorded still plays as assembled, so it is kept
                // and named rather than thrown away with the error.
                let kept = final_p.with_extension(match kinds[0] {
                    hya_stream::hls::Segments::Ts => "ts",
                    hya_stream::hls::Segments::Fmp4 => "mp4",
                });
                let _ = std::fs::rename(&parts[0], &kept);
                format!("{e} (the recording was saved as {})", kept.display())
            })
    };
    for p in &parts {
        let _ = std::fs::remove_file(p);
    }
    let finished = match result {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    if let hya_stream::hls::Finished::RemuxSkipped(why) = &finished {
        // The file plays, but it is the fragmented assembly rather than the
        // faststart MP4 that was asked for. Saying so beats leaving it to be
        // discovered by a player that dislikes fMP4.
        crate::log::warn(&format!("#{id} kept the assembled file: {why}"));
    }

    let size = std::fs::metadata(final_p).map(|m| m.len()).unwrap_or(0);
    let elapsed = t0.elapsed().as_secs_f64();
    if skipped > 0 {
        crate::log::warn(&format!(
            "#{id} recording has {skipped} missing segment(s); playback will jump"
        ));
    }
    crate::log::log(&format!(
        "recorded #{id} -> {final_str} ({taken} segments, {skipped} lost, {size} bytes, {elapsed:.1}s)"
    ));
    ev(Event::Probed {
        id,
        size: Some(size),
        ranges: false,
        file_name: None,
    });
    ev(Event::Finished { id, elapsed, size });
}

/// Append a whole file to an open output and delete it.
fn append_file(src: &str, out: &mut std::fs::File) -> std::io::Result<()> {
    let mut f = std::fs::File::open(src)?;
    std::io::copy(&mut f, out)?;
    drop(f);
    let _ = std::fs::remove_file(src);
    Ok(())
}

/// What one stream download has to assemble. DASH keeps video and audio in
/// separate Representations far more often than HLS does, and combining them
/// is a real muxing step — so the two shapes are distinguished here rather
/// than pretended away.
enum Assembly {
    /// One track that is already a playable file once concatenated.
    Single(hya_stream::hls::Plan),
    /// Video and audio fetched separately, then muxed into one file.
    VideoAudio(hya_stream::hls::Plan, hya_stream::hls::Plan),
}

impl Assembly {
    fn plans(&self) -> Vec<&hya_stream::hls::Plan> {
        match self {
            Assembly::Single(p) => vec![p],
            Assembly::VideoAudio(v, a) => vec![v, a],
        }
    }

    fn segments(&self) -> u64 {
        self.plans()
            .iter()
            .map(|p| p.segments.len() as u64)
            .sum::<u64>()
    }

    fn estimated(&self) -> u64 {
        self.plans()
            .iter()
            .filter_map(|p| p.estimated_size)
            .sum::<u64>()
    }
}

/// Resolve a manifest, download its segments, and produce one playable file.
///
/// Progress is reported against the size the manifest *implies*
/// (bitrate x duration) only until the first segments land; after that it is
/// projected from what actually arrived, because no server states the total
/// for a stream and the manifest's own number is routinely out by a third.
async fn run_stream(
    spec: StreamSpec,
    cancel: Arc<AtomicBool>,
    limiter: Arc<RateLimiter>,
    final_path: Arc<Mutex<String>>,
    tx: UnboundedSender<Event>,
) {
    let id = spec.id;
    let ev = |e: Event| {
        let _ = tx.send(e);
    };
    let fail = |msg: String| {
        crate::log::error(&format!("#{id} stream failed: {msg}"));
        let _ = tx.send(Event::Failed {
            id,
            error: msg,
            done: 0,
            held: vec![],
            permission_denied: false,
        });
    };

    let connector = match shared_connector() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let (ck, rf, ua) = (
        spec.cookies.as_deref(),
        spec.referer.as_deref(),
        spec.user_agent.as_str(),
    );

    ev(Event::Status {
        id,
        line: crate::i18n::tr("Reading playlist..."),
    });

    // 1. The manifest.
    let cap = hya_stream::hls::playlist_cap();
    // `base` is where the manifest ACTUALLY came from after redirects; every
    // relative URI inside resolves against it.
    let (body, base) = match stream_get_at(&connector, &spec.manifest, ck, rf, ua, cap).await {
        Ok(b) => b,
        Err(e) => return fail(format!("could not read the manifest: {e}")),
    };
    let text = String::from_utf8_lossy(&body).into_owned();
    if base != spec.manifest {
        crate::log::info(&format!(
            "#{id} manifest redirected to {}",
            crate::log::redact(&base)
        ));
    }
    crate::log::debug(&format!(
        "#{id} manifest {} bytes from {}",
        body.len(),
        crate::log::redact(&base)
    ));

    // What the manifest IS beats what the extension said it was: a URL can
    // be renamed, a body cannot. Neither shape means this is not a manifest
    // at all — a login page or an error page served under a `.m3u8` URL is
    // the usual cause, and saying so beats "lists no segments".
    let is_dash = if text.trim_start().starts_with("#EXTM3U") {
        false
    } else if text.contains("<MPD") {
        true
    } else {
        return fail(format!(
            "{} is not an HLS or DASH manifest (the server returned {} bytes of something else)",
            spec.manifest,
            body.len()
        ));
    };

    let assembly = if is_dash {
        let mf = hya_stream::dash::parse(&text, &base);
        let Some(video) = mf.choose_video(spec.height) else {
            return fail("the manifest lists no video renditions".into());
        };
        if mf.live {
            // The refusals below live in `mf.plan()`, which only the VOD path
            // reaches — so they have to be repeated HERE, before the primed
            // window fetches a byte. The recorder cannot decrypt: keys rotate
            // mid-live and nothing fetches them, so what the VOD path refuses
            // the live path must refuse too, rather than appending ciphertext
            // and finishing successfully over it.
            if let Some(d) = &mf.drm {
                return fail(drm_refusal(d));
            }
            // A dynamic manifest publishes a sliding window, not a list to
            // work through: record it until it ends or is stopped.
            crate::log::info(&format!(
                "#{id} recording live dash: video {} + audio {}",
                video.id,
                mf.choose_audio().map(|a| a.id.as_str()).unwrap_or("-")
            ));
            let audio = mf.choose_audio();
            let source = LiveSource::Dash {
                url: base.clone(),
                video_id: video.id.clone(),
                audio_id: audio.map(|a| a.id.clone()),
            };
            let mut windows = vec![Window {
                init: video.init.clone(),
                segments: hya_stream::dash::Manifest::timed_window(video),
                kind: hya_stream::hls::Segments::Fmp4,
            }];
            if let Some(a) = audio {
                windows.push(Window {
                    init: a.init.clone(),
                    segments: hya_stream::dash::Manifest::timed_window(a),
                    kind: hya_stream::hls::Segments::Fmp4,
                });
            }
            let primed = Some((windows, false, mf.refresh_after()));
            return record_live(
                &spec,
                source,
                primed,
                &connector,
                &limiter,
                &cancel,
                &final_path,
                &tx,
            )
            .await;
        }
        crate::log::info(&format!(
            "#{id} dash video {}p @ {} kbps ({} segments)",
            video.height.unwrap_or(0),
            video.bandwidth.unwrap_or(0) / 1000,
            video.segments.len()
        ));
        let vplan = match mf.plan(video) {
            Ok(p) => p,
            Err(refusal) => return fail(refusal.to_string()),
        };
        // Audio is a separate Representation in most DASH; without it the
        // finished file would be silent, so it is fetched and muxed rather
        // than quietly dropped.
        match mf.choose_audio() {
            Some(audio) => {
                crate::log::info(&format!(
                    "#{id} dash audio {} @ {} kbps ({} segments)",
                    audio.id,
                    audio.bandwidth.unwrap_or(0) / 1000,
                    audio.segments.len()
                ));
                match mf.plan(audio) {
                    Ok(aplan) => Assembly::VideoAudio(vplan, aplan),
                    // Audio that will not resolve is not a reason to lose
                    // the video.
                    Err(e) => {
                        crate::log::warn(&format!("#{id} dash audio unusable: {e}"));
                        Assembly::Single(vplan)
                    }
                }
            }
            None => Assembly::Single(vplan),
        }
    } else {
        // HLS: a master needs one more round trip to reach the variant that
        // actually lists segments.
        let mut playlist = hya_stream::hls::parse(&text, &base);
        let mut bandwidth = spec.bandwidth;
        let mut variant_url = base.clone();
        if playlist.is_master() {
            let Some(chosen) = hya_stream::hls::choose(
                &playlist.variants,
                spec.variant_url.as_deref(),
                spec.height,
            )
            .cloned() else {
                return fail("the master playlist lists no variants".into());
            };
            bandwidth = bandwidth.or(chosen.bandwidth);
            for v in &playlist.variants {
                crate::log::debug(&format!(
                    "#{id} candidate {}p @ {} kbps {}",
                    v.height.unwrap_or(0),
                    v.bandwidth.unwrap_or(0) / 1000,
                    v.codecs.as_deref().unwrap_or("-")
                ));
            }
            crate::log::info(&format!(
                "#{id} variant {}p @ {} kbps -> {}",
                chosen.height.unwrap_or(0),
                chosen.bandwidth.unwrap_or(0) / 1000,
                chosen.url
            ));
            let body = match stream_get(&connector, &chosen.url, ck, rf, ua, cap).await {
                Ok(b) => b,
                Err(e) => return fail(format!("could not read the variant playlist: {e}")),
            };
            playlist = hya_stream::hls::parse(&String::from_utf8_lossy(&body), &chosen.url);
            variant_url = chosen.url;
        }
        if playlist.live {
            // As above: `Plan::build` is never reached on this path, so its
            // refusals are restated before the primed window is used.
            if let Some(d) = &playlist.drm {
                return fail(drm_refusal(d));
            }
            if let Some(enc) = &playlist.encryption {
                return fail(format!("recording {enc} live streams is not supported yet"));
            }
            crate::log::info(&format!(
                "#{id} recording live hls from {}",
                crate::log::redact(&variant_url)
            ));
            let primed = Some((
                vec![Window {
                    init: playlist.init.clone(),
                    segments: playlist.timed_window(),
                    kind: playlist
                        .segments_kind
                        .unwrap_or(hya_stream::hls::Segments::Ts),
                }],
                playlist.ended,
                playlist.refresh_after(),
            ));
            let source = LiveSource::Hls { url: variant_url };
            return record_live(
                &spec,
                source,
                primed,
                &connector,
                &limiter,
                &cancel,
                &final_path,
                &tx,
            )
            .await;
        }
        match hya_stream::hls::Plan::build(&playlist, bandwidth) {
            Ok(p) => Assembly::Single(p),
            Err(refusal) => return fail(refusal.to_string()),
        }
    };

    let total_segments = assembly.segments();
    let estimated = assembly.estimated();
    for (n, plan) in assembly.plans().iter().enumerate() {
        crate::log::debug(&format!(
            "#{id} track {n}: {} segments, {:?}, init {}, byte-ranged {}, ~{} bytes",
            plan.segments.len(),
            plan.kind,
            plan.init.is_some(),
            plan.segments.iter().any(|s| s.range.is_some()),
            plan.estimated_size.unwrap_or(0)
        ));
    }
    let conc = hya_stream::hls::Concurrency::fixed(spec.conns);
    crate::log::info(&format!(
        "#{id} {} ({} declared): {total_segments} segments, ~{estimated} bytes, \
         {} connection(s), ffmpeg {}",
        if is_dash { "dash" } else { "hls" },
        spec.protocol,
        conc.ceiling(),
        // Which branch the finish takes hangs on this, so a file that came
        // out as TS when MP4 was asked for is explained by this one word.
        if hya_stream::hls::ffmpeg().is_some() {
            "present"
        } else {
            "absent"
        },
    ));
    ev(Event::Probed {
        id,
        size: (estimated > 0).then_some(estimated),
        ranges: false,
        file_name: None,
    });

    // 2. Assemble. One staging file per track.
    if let Some(dir) = std::path::Path::new(&spec.temp_path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    // One job per track. Whether each can carry on from an earlier run is
    // decided here, before anything is measured.
    struct Job {
        part: String,
        staging: String,
        checkpoint: String,
        resumed: hya_stream::hls::Checkpoint,
    }
    let jobs: Vec<Job> = assembly
        .plans()
        .iter()
        .enumerate()
        .map(|(i, plan)| {
            let part = format!("{}.t{i}", spec.temp_path);
            let checkpoint = format!("{}.t{i}.ck", spec.temp_path);
            let saved = hya_stream::hls::Checkpoint::read(&checkpoint);
            let resumed = if saved.usable(&part, plan) {
                crate::log::debug(&format!(
                    "#{id} track {i}: resuming at {} segments / {} bytes",
                    saved.segments, saved.bytes
                ));
                saved
            } else {
                if saved.segments > 0 {
                    crate::log::debug(&format!(
                        "#{id} track {i}: checkpoint discarded (plan or file no longer matches)"
                    ));
                }
                // Not the file this checkpoint describes: start clean rather
                // than splice onto something unknown.
                let _ = std::fs::remove_file(&part);
                let _ = std::fs::remove_file(&checkpoint);
                hya_stream::hls::Checkpoint::default()
            };
            Job {
                part,
                staging: format!("{}.s{i}", spec.temp_path),
                checkpoint,
                resumed,
            }
        })
        .collect();

    // AES-128 keys, fetched with the SAME session as the manifest: a key
    // gated behind the browser's cookies is the ordinary case, and fetching
    // it any other way just gets a 403.
    let mut keys = hya_stream::hls::Keys::new();
    for plan in assembly.plans() {
        for uri in plan.key_uris() {
            if keys.contains_key(&uri) {
                continue;
            }
            // The URI only — never the 16 bytes it answers with.
            crate::log::debug(&format!(
                "#{id} fetching AES-128 key {}",
                crate::log::redact(&uri)
            ));
            match stream_get(&connector, &uri, ck, rf, ua, hya_stream::hls::KEY_FETCH_CAP).await {
                Ok(bytes) if bytes.len() == 16 => {
                    let mut k = [0u8; 16];
                    k.copy_from_slice(&bytes);
                    keys.insert(uri, k);
                }
                Ok(bytes) => {
                    return fail(format!(
                        "the AES-128 key at {uri} is {} bytes, not 16",
                        bytes.len()
                    ))
                }
                Err(e) => return fail(format!("could not fetch the AES-128 key {uri}: {e}")),
            }
        }
    }
    if !keys.is_empty() {
        crate::log::info(&format!("#{id} AES-128: {} key(s)", keys.len()));
    }

    let meter = Arc::new(hya_stream::hls::Meter::default());
    let carried: u64 = jobs.iter().map(|j| j.resumed.bytes).sum();
    let carried_segments: u64 = jobs.iter().map(|j| j.resumed.segments).sum();
    if carried > 0 {
        crate::log::info(&format!(
            "#{id} resuming stream at {carried_segments}/{total_segments} segments ({carried} bytes)"
        ));
        meter.preload(carried, carried_segments);
    }
    let t0 = std::time::Instant::now();

    // A recording has no total, so the ticker is told so rather than
    // inventing a percentage.
    let ticker = spawn_ticker(
        id,
        &meter,
        &tx,
        &cancel,
        Some(total_segments),
        estimated,
        None,
    );

    let fetch = segment_fetcher(&spec, &connector, &limiter, &cancel);

    let mut parts: Vec<String> = Vec::new();
    let mut outcome = Ok(());
    for (plan, job) in assembly.plans().into_iter().zip(&jobs) {
        // Append when carrying on, truncate when starting over.
        let opened = if job.resumed.segments > 0 {
            std::fs::OpenOptions::new().append(true).open(&job.part)
        } else {
            std::fs::File::create(&job.part)
        };
        let mut out = match opened {
            Ok(f) => f,
            Err(e) => {
                outcome = Err(friendly_io(&e, &job.part));
                break;
            }
        };
        let resume = hya_stream::hls::Resume {
            skip: job.resumed.segments as usize,
            bytes: job.resumed.bytes,
            checkpoint: Some(job.checkpoint.as_str()),
        };
        match hya_stream::hls::fetch_all(
            plan,
            &mut out,
            &job.staging,
            fetch.clone(),
            &meter,
            &cancel,
            resume,
            conc,
            &keys,
        )
        .await
        {
            Ok(_) => parts.push(job.part.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                ticker.abort();
                // The staging files and their checkpoints are KEPT: that is
                // what makes the next Start carry on rather than refetch
                // everything. `held` still reports one contiguous span so the
                // row and the bar show where it stopped.
                let done = meter.settled();
                return ev(Event::Stopped {
                    id,
                    done,
                    held: vec![(0, done)],
                });
            }
            Err(e) => {
                outcome = Err(format!("segment download failed: {e}"));
                break;
            }
        }
    }
    ticker.abort();
    if let Err(e) = outcome {
        // A failure leaves the parts and checkpoints alone too — a flaky
        // segment is the commonest failure and retrying should not throw
        // away the gigabyte that did arrive.
        return fail(e);
    }

    // 3. Container. What was assembled is already playable for every case
    //    except MPEG-TS asked to become MP4, and DASH's two tracks.
    let want_ext = if spec.container.eq_ignore_ascii_case("ts") {
        "ts"
    } else {
        "mp4"
    };
    let final_str = final_path
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| spec.final_path.clone());
    let final_p = std::path::Path::new(&final_str);
    if let Some(dir) = final_p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let result = match (&assembly, parts.as_slice()) {
        (Assembly::VideoAudio(..), [video, audio]) => {
            ev(Event::Status {
                id,
                line: crate::i18n::tr("Combining video and audio..."),
            });
            hya_stream::hls::mux(
                std::path::Path::new(video),
                std::path::Path::new(audio),
                final_p,
            )
            .map_err(|e| {
                // Both tracks are playable on their own, so they are kept and
                // named rather than thrown away with the error.
                let v = final_p.with_extension("video.mp4");
                let a = final_p.with_extension("audio.m4a");
                let _ = std::fs::rename(video, &v);
                let _ = std::fs::rename(audio, &a);
                format!(
                    "{e} (video saved as {}, audio as {})",
                    v.display(),
                    a.display()
                )
            })
            .map(|()| hya_stream::hls::Finished::Remuxed)
        }
        (Assembly::Single(plan), [only]) => {
            if hya_stream::hls::plan_finish(
                plan.kind,
                want_ext,
                hya_stream::hls::ffmpeg().is_some(),
            ) == hya_stream::hls::Finish::Remux
            {
                ev(Event::Status {
                    id,
                    line: crate::i18n::tr("Remuxing..."),
                });
            }
            hya_stream::hls::finish(plan.kind, want_ext, std::path::Path::new(only), final_p)
                .map_err(|e| {
                    let kept = final_p.with_extension(plan.native_ext());
                    let _ = std::fs::rename(only, &kept);
                    format!("{e} (the stream was saved as {})", kept.display())
                })
        }
        _ => Err("nothing was assembled".into()),
    };
    for j in &jobs {
        let _ = std::fs::remove_file(&j.part);
        let _ = std::fs::remove_file(&j.checkpoint);
    }
    let finished = match result {
        Ok(f) => f,
        Err(e) => return fail(e),
    };
    match &finished {
        // Playable, but the fragmented assembly rather than the faststart
        // MP4 that was asked for.
        hya_stream::hls::Finished::RemuxSkipped(why) => {
            crate::log::warn(&format!("#{id} kept the assembled file: {why}"));
        }
        f => crate::log::debug(&format!("#{id} finished as {f:?}")),
    }

    let size = std::fs::metadata(final_p).map(|m| m.len()).unwrap_or(0);
    let elapsed = t0.elapsed().as_secs_f64();
    crate::log::log(&format!(
        "done stream #{id} -> {final_str} ({size} bytes, {elapsed:.1}s)"
    ));
    // The row's size must end up being what is actually on disk, not the
    // projection it was tracking against.
    ev(Event::Probed {
        id,
        size: Some(size),
        ranges: false,
        file_name: None,
    });
    ev(Event::Finished { id, elapsed, size });
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// An HTTP/1.1 origin serving a route table, recording the headers it was
    /// asked with. Small enough to read, real enough that the transfer under
    /// test does actual sockets, actual parsing, actual IO.
    ///
    /// A route holds a LIST of bodies: the Nth request for that path gets the
    /// Nth body and the last one repeats, which is how a growing live
    /// playlist is modelled. Everything is owned by the server thread, so
    /// tests running in parallel cannot see each other's routes.
    fn serve_core(
        routes: Vec<(String, Vec<Vec<u8>>)>,
        slow: &'static str,
        ms: u64,
    ) -> (
        String,
        Arc<Mutex<Vec<String>>>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // High-water mark of requests in the air AT ONCE. A client that
        // claims to fetch in parallel can be held to it.
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = seen.clone();
        let routes = Arc::new(routes);
        let hits: Arc<Mutex<std::collections::HashMap<String, usize>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let (peak2, live2) = (peak.clone(), live.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { continue };
                let (routes, hits, log) = (routes.clone(), hits.clone(), log.clone());
                let (peak, live) = (peak2.clone(), live2.clone());
                // A thread per connection. Serving each request to completion
                // inside the accept loop SERIALISES every client, which both
                // hides whether the client fetches in parallel and lets one
                // delayed path stall the whole origin.
                std::thread::spawn(move || {
                    let n = live.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(n, Ordering::Relaxed);
                    let finish = |live: &std::sync::atomic::AtomicUsize| {
                        live.fetch_sub(1, Ordering::Relaxed);
                    };
                    let Ok(peek) = s.try_clone() else {
                        return finish(&live);
                    };
                    let mut r = BufReader::new(peek);
                    let mut line = String::new();
                    if r.read_line(&mut line).is_err() || line.is_empty() {
                        return finish(&live);
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let mut headers = String::new();
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                            break;
                        }
                        headers.push_str(&h);
                    }
                    if let Ok(mut g) = log.lock() {
                        g.push(format!("{path}\n{headers}"));
                    }
                    if ms > 0 && path.contains(slow) {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                    }
                    let nth = {
                        let mut g = hits.lock().unwrap();
                        let c = g.entry(path.clone()).or_insert(0);
                        let n = *c;
                        *c += 1;
                        n
                    };
                    let body = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, bodies)| bodies[nth.min(bodies.len() - 1)].clone());
                    // A body of `>>>dest` means "302 to dest", which is how the
                    // redirect tests describe a CDN bouncing to an edge.
                    if let Some(b) = &body {
                        if let Some(dest) = b.strip_prefix(b">>>".as_ref()) {
                            let dest = String::from_utf8_lossy(dest).to_string();
                            let resp = format!(
                                "HTTP/1.1 302 Found\r\nLocation: {dest}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                            let _ = s.write_all(resp.as_bytes());
                            let _ = s.flush();
                            return finish(&live);
                        }
                    }
                    let resp = match body {
                        Some(b) => {
                            let mut out = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                b.len()
                            )
                            .into_bytes();
                            out.extend_from_slice(&b);
                            out
                        }
                        None => {
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                                .to_vec()
                        }
                    };
                    let _ = s.write_all(&resp);
                    let _ = s.flush();
                    finish(&live)
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), seen, peak)
    }

    fn one_each(routes: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<Vec<u8>>)> {
        routes.into_iter().map(|(p, b)| (p, vec![b])).collect()
    }

    fn serve(routes: Vec<(String, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let (base, seen, _) = serve_core(one_each(routes), "\u{0}never", 0);
        (base, seen)
    }

    /// The same origin, but paths containing `slow` answer after `ms` — which
    /// is how a test gets a deterministic window in which to pause.
    fn serve_delayed(
        routes: Vec<(String, Vec<u8>)>,
        slow: &'static str,
        ms: u64,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let (base, seen, _) = serve_core(one_each(routes), slow, ms);
        (base, seen)
    }

    /// An origin whose answer to a path changes with each request.
    fn serve_sequence(routes: Vec<(String, Vec<Vec<u8>>)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let (base, seen, _) = serve_core(routes, "\u{0}never", 0);
        (base, seen)
    }

    /// An origin that also reports how many requests it ever had in the air
    /// at once, for the tests that care whether fetching overlapped.
    fn serve_peak(
        routes: Vec<(String, Vec<Vec<u8>>)>,
        slow: &'static str,
        ms: u64,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let (base, _, peak) = serve_core(routes, slow, ms);
        (base, peak)
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hydra-stream-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn a_master_playlist_is_resolved_and_its_segments_assembled_in_order() {
        let dir = tmp("vod");
        let (base, seen) = serve(vec![
            (
                "/master.m3u8".into(),
                concat!(
                    "#EXTM3U\n",
                    "#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n",
                    "low/index.m3u8\n",
                    "#EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1920x1080\n",
                    "high/index.m3u8\n"
                )
                .into(),
            ),
            (
                "/high/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:2\n",
                    "#EXTINF:2.0,\nseg0.ts\n#EXTINF:2.0,\nseg1.ts\n#EXTINF:2.0,\nseg2.ts\n",
                    "#EXT-X-ENDLIST\n"
                )
                .into(),
            ),
            ("/high/seg0.ts".into(), b"AAAA".to_vec()),
            ("/high/seg1.ts".into(), b"BBBB".to_vec()),
            ("/high/seg2.ts".into(), b"CCCC".to_vec()),
        ]);

        let final_path = dir.join("out.ts");
        let spec = StreamSpec {
            id: 1,
            manifest: format!("{base}/master.m3u8"),
            protocol: "hls".into(),
            conns: 0,
            max_seconds: None,
            limit: None,
            variant_url: None,
            height: Some(1080),
            bandwidth: None,
            container: "TS".into(),
            cookies: Some("sid=s3cr3t".into()),
            referer: Some("https://page.example/watch".into()),
            user_agent: "hydra-test/1".into(),
            temp_path: dir.join("out.part").to_string_lossy().into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
        };
        let (tx, mut rx) = unbounded_channel();
        run_stream(
            spec.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RateLimiter::new(0)),
            Arc::new(Mutex::new(spec.final_path.clone())),
            tx,
        )
        .await;

        let mut finished = None;
        let mut failure = None;
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Finished { size, .. } => finished = Some(size),
                Event::Failed { error, .. } => failure = Some(error),
                _ => {}
            }
        }
        assert_eq!(failure, None, "stream failed");
        assert_eq!(finished, Some(12));
        assert_eq!(std::fs::read(&final_path).unwrap(), b"AAAABBBBCCCC");
        // The staging file is not left behind.
        assert!(!std::path::Path::new(&spec.temp_path).exists());

        let reqs = seen.lock().unwrap().clone();
        // The 1080p variant was chosen; the 360p one was never fetched.
        assert!(reqs.iter().any(|r| r.starts_with("/high/index.m3u8")));
        assert!(!reqs.iter().any(|r| r.starts_with("/low/")));
        // Every request carried the browser's session, segments included.
        assert_eq!(reqs.len(), 5);
        for r in &reqs {
            assert!(r.contains("Cookie: sid=s3cr3t"), "no cookie on {r}");
            assert!(
                r.contains("Referer: https://page.example/watch"),
                "no referer on {r}"
            );
            assert!(r.contains("hydra-test/1"), "no user-agent on {r}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fragmented_mp4_puts_the_init_map_first() {
        let dir = tmp("fmp4");
        let (base, _) = serve(vec![
            (
                "/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n",
                    "#EXT-X-MAP:URI=\"init.mp4\"\n",
                    "#EXTINF:2.0,\nseg0.m4s\n#EXTINF:2.0,\nseg1.m4s\n#EXT-X-ENDLIST\n"
                )
                .into(),
            ),
            ("/init.mp4".into(), b"INIT".to_vec()),
            ("/seg0.m4s".into(), b"0000".to_vec()),
            ("/seg1.m4s".into(), b"1111".to_vec()),
        ]);
        let final_path = dir.join("out.mp4");
        let spec = StreamSpec {
            id: 2,
            manifest: format!("{base}/index.m3u8"),
            protocol: "hls".into(),
            conns: 0,
            max_seconds: None,
            limit: None,
            variant_url: None,
            height: None,
            bandwidth: None,
            // fMP4 concatenated behind its init map already IS an mp4, so
            // asking for MP4 must NOT invoke a remux.
            container: "MP4".into(),
            cookies: None,
            referer: None,
            user_agent: "hydra-test/1".into(),
            temp_path: dir.join("out.part").to_string_lossy().into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
        };
        let (tx, mut rx) = unbounded_channel();
        run_stream(
            spec.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RateLimiter::new(0)),
            Arc::new(Mutex::new(spec.final_path.clone())),
            tx,
        )
        .await;
        let mut failure = None;
        while let Ok(e) = rx.try_recv() {
            if let Event::Failed { error, .. } = e {
                failure = Some(error);
            }
        }
        assert_eq!(failure, None);
        assert_eq!(std::fs::read(&final_path).unwrap(), b"INIT00001111");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_drm_playlist_is_refused_and_nothing_is_written() {
        let dir = tmp("drm");
        let (base, seen) = serve(vec![(
            "/index.m3u8".into(),
            concat!(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n",
                "#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\",URI=\"skd://x\"\n",
                "#EXTINF:2.0,\nseg0.ts\n#EXT-X-ENDLIST\n"
            )
            .into(),
        )]);
        let final_path = dir.join("out.ts");
        let spec = StreamSpec {
            id: 3,
            manifest: format!("{base}/index.m3u8"),
            protocol: "hls".into(),
            conns: 0,
            max_seconds: None,
            limit: None,
            variant_url: None,
            height: None,
            bandwidth: None,
            container: "TS".into(),
            cookies: None,
            referer: None,
            user_agent: "hydra-test/1".into(),
            temp_path: dir.join("out.part").to_string_lossy().into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
        };
        let (tx, mut rx) = unbounded_channel();
        run_stream(
            spec.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RateLimiter::new(0)),
            Arc::new(Mutex::new(spec.final_path.clone())),
            tx,
        )
        .await;
        let mut failure = None;
        while let Ok(e) = rx.try_recv() {
            if let Event::Failed { error, .. } = e {
                failure = Some(error);
            }
        }
        // The wording is tuned; what must hold is that it NAMES the system
        // and says the download cannot happen — never "not supported yet",
        // which reads as a feature still to come.
        let msg = failure.clone().expect("a DRM playlist must be refused");
        assert!(msg.contains("Widevine"), "the system must be named: {msg}");
        assert!(
            msg.contains("cannot be downloaded"),
            "the refusal must be plain: {msg}"
        );
        assert!(!final_path.exists());
        // The key was never requested — only the playlist itself was read.
        assert_eq!(seen.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Run one stream to completion (or failure) and report what came back.
    async fn drive(spec: StreamSpec, cancel: Arc<AtomicBool>) -> (Option<u64>, Option<String>) {
        let (tx, mut rx) = unbounded_channel();
        run_stream(
            spec.clone(),
            cancel,
            Arc::new(RateLimiter::new(0)),
            Arc::new(Mutex::new(spec.final_path.clone())),
            tx,
        )
        .await;
        let (mut finished, mut failure) = (None, None);
        while let Ok(e) = rx.try_recv() {
            match e {
                Event::Finished { size, .. } => finished = Some(size),
                Event::Failed { error, .. } => failure = Some(error),
                Event::Stopped { done, .. } => finished = finished.or(Some(done)),
                _ => {}
            }
        }
        (finished, failure)
    }

    fn spec_for(id: DlId, manifest: String, dir: &std::path::Path, name: &str) -> StreamSpec {
        StreamSpec {
            id,
            manifest,
            protocol: "hls".into(),
            conns: 0,
            max_seconds: None,
            limit: None,
            variant_url: None,
            height: None,
            bandwidth: None,
            container: "MP4".into(),
            cookies: None,
            referer: None,
            user_agent: "hydra-test/1".into(),
            temp_path: dir
                .join(format!("{name}.part"))
                .to_string_lossy()
                .into_owned(),
            final_path: dir.join(name).to_string_lossy().into_owned(),
        }
    }

    #[tokio::test]
    async fn a_redirected_manifest_resolves_its_segments_against_where_it_landed() {
        let dir = tmp("redirect");
        // The published URL bounces to a regional edge, and so does a
        // segment. Relative URIs inside the playlist belong to the edge, not
        // to the address originally asked for.
        let (base, seen) = serve(vec![
            ("/live/index.m3u8".into(), b">>>/edge7/index.m3u8".to_vec()),
            (
                "/edge7/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:2\n",
                    "#EXTINF:2.0,\nseg0.ts\n#EXTINF:2.0,\nseg1.ts\n#EXT-X-ENDLIST\n"
                )
                .into(),
            ),
            // Resolved against /edge7/, not /live/.
            ("/edge7/seg0.ts".into(), b">>>/cdn/seg0.ts".to_vec()),
            ("/cdn/seg0.ts".into(), b"AAAA".to_vec()),
            ("/edge7/seg1.ts".into(), b"BBBB".to_vec()),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(30, format!("{base}/live/index.m3u8"), &dir, "out.ts")
        };
        let (finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);
        assert_eq!(finished, Some(8));
        assert_eq!(std::fs::read(&spec.final_path).unwrap(), b"AAAABBBB");
        let reqs = seen.lock().unwrap().clone();
        // The playlist was read from the edge it was sent to...
        assert!(reqs.iter().any(|r| r.starts_with("/edge7/index.m3u8")));
        // ...and the redirected segment was followed to its final home.
        assert!(reqs.iter().any(|r| r.starts_with("/cdn/seg0.ts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// AES-128-CBC with PKCS#7, the way an HLS packager writes a segment.
    /// A fresh AES-128 key for one test run. Seeded from the process's
    /// `RandomState`, so the bytes exist only while the test does.
    fn test_key() -> [u8; 16] {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut key = [0u8; 16];
        for (i, half) in key.chunks_mut(8).enumerate() {
            let mut h = RandomState::new().build_hasher();
            h.write_usize(i);
            half.copy_from_slice(&h.finish().to_ne_bytes());
        }
        key
    }

    fn encrypt_segment(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        use aes::cipher::{block_padding::Pkcs7, BlockModeEncrypt, KeyIvInit};
        let mut buf = vec![0u8; plain.len() + 16];
        buf[..plain.len()].copy_from_slice(plain);
        let n = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
            .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();
        buf.truncate(n);
        buf
    }

    #[tokio::test]
    async fn an_encrypted_live_playlist_is_refused_not_recorded_as_ciphertext() {
        // The recorder cannot decrypt, and the VOD refusal lives in
        // `Plan::build`, which the live route never reaches. Without an
        // explicit guard the FIRST window — already fetched to detect that
        // the stream is live — is appended raw and the recording finishes
        // reporting success over a file of noise.
        let dir = tmp("live-enc");
        let (base, seen) = serve(vec![
            (
                "/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:1\n",
                    "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n",
                    "#EXTINF:4.0,\ns0.ts\n"
                )
                .into(),
            ),
            ("/s0.ts".into(), vec![0u8; 32]),
            ("/k".into(), vec![0u8; 16]),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(42, format!("{base}/index.m3u8"), &dir, "out.ts")
        };
        let (_, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        let msg = failure.expect("an encrypted live stream must not report success");
        assert!(msg.contains("AES-128"), "unhelpful message: {msg}");
        assert!(!std::path::Path::new(&spec.final_path).exists());
        // Refused before a single segment was pulled.
        let reqs = seen.lock().unwrap().clone();
        assert!(!reqs.iter().any(|r| r.starts_with("/s0.ts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_drm_live_playlist_is_refused_before_the_primed_window_is_used() {
        let dir = tmp("live-drm");
        let (base, seen) = serve(vec![
            (
                "/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:4\n",
                    "#EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"com.apple.streamingkeydelivery\",URI=\"skd://x\"\n",
                    "#EXTINF:4.0,\ns0.ts\n"
                )
                .into(),
            ),
            ("/s0.ts".into(), vec![0u8; 32]),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(43, format!("{base}/index.m3u8"), &dir, "out.ts")
        };
        let (_, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        let msg = failure
            .clone()
            .expect("a DRM live playlist must be refused");
        assert!(msg.contains("FairPlay"), "the system must be named: {msg}");
        assert!(
            msg.contains("cannot be downloaded"),
            "the refusal must be plain: {msg}"
        );
        assert!(!seen.lock().unwrap().iter().any(|r| r.starts_with("/s0.ts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn an_aes_128_playlist_is_fetched_keyed_and_decrypted() {
        let dir = tmp("aes128");
        // Generated per run, not baked into the source: a literal here is
        // key material committed to the repository, however short its life.
        let key = test_key();
        // No explicit IV, so each segment's IV is its media sequence number
        // as a 128-bit big-endian number.
        let iv = |seq: u64| u128::from(seq).to_be_bytes();
        let (base, seen) = serve(vec![
            (
                "/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:4\n",
                    "#EXT-X-MEDIA-SEQUENCE:5\n",
                    "#EXT-X-KEY:METHOD=AES-128,URI=\"enc.key\"\n",
                    "#EXTINF:4.0,\ns0.ts\n#EXTINF:4.0,\ns1.ts\n#EXT-X-ENDLIST\n"
                )
                .into(),
            ),
            ("/enc.key".into(), key.to_vec()),
            (
                "/s0.ts".into(),
                encrypt_segment(b"FIRST-PLAINTEXT!", &key, &iv(5)),
            ),
            (
                "/s1.ts".into(),
                encrypt_segment(b"second plaintext", &key, &iv(6)),
            ),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(40, format!("{base}/index.m3u8"), &dir, "out.ts")
        };
        let (_, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None, "AES-128 should download, not be refused");
        // The file on disk is PLAINTEXT: decryption happened before append,
        // so a resumed run never has to know which bytes were encrypted.
        assert_eq!(
            std::fs::read(&spec.final_path).unwrap(),
            b"FIRST-PLAINTEXT!second plaintext"
        );
        // The key was fetched once and reused for both segments.
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.iter().filter(|r| r.starts_with("/enc.key")).count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_key_that_cannot_be_fetched_fails_before_anything_is_written() {
        let dir = tmp("aes-nokey");
        let (base, seen) = serve(vec![(
            "/index.m3u8".into(),
            concat!(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n",
                "#EXT-X-KEY:METHOD=AES-128,URI=\"gone.key\"\n",
                "#EXTINF:4.0,\ns0.ts\n#EXT-X-ENDLIST\n"
            )
            .into(),
        )]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(41, format!("{base}/index.m3u8"), &dir, "out.ts")
        };
        let (_, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        let msg = failure.expect("a missing key is a failure, not a silent noise file");
        assert!(msg.contains("AES-128 key"), "unhelpful message: {msg}");
        assert!(!std::path::Path::new(&spec.final_path).exists());
        // Refused before a single segment was requested.
        let reqs = seen.lock().unwrap().clone();
        assert!(!reqs.iter().any(|r| r.starts_with("/s0.ts")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_dash_manifest_is_parsed_and_its_video_segments_assembled() {
        let dir = tmp("dash-video");
        let (base, seen) = serve(vec![
            (
                "/m.mpd".into(),
                concat!(
                    r#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>"#,
                    r#"<AdaptationSet contentType="video">"#,
                    r#"<SegmentTemplate media="v/$Number$.m4s" initialization="v/init.mp4" duration="4" timescale="1" startNumber="1"/>"#,
                    r#"<Representation id="v0" bandwidth="800000" width="640" height="360"/>"#,
                    r#"<Representation id="v1" bandwidth="4000000" width="1920" height="1080"/>"#,
                    "</AdaptationSet></Period></MPD>"
                )
                .into(),
            ),
            ("/v/init.mp4".into(), b"INIT".to_vec()),
            ("/v/1.m4s".into(), b"AAAA".to_vec()),
            ("/v/2.m4s".into(), b"BBBB".to_vec()),
        ]);
        let mut spec = spec_for(10, format!("{base}/m.mpd"), &dir, "out.mp4");
        spec.protocol = "dash".into();
        let (finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);
        assert_eq!(finished, Some(12));
        // Init map first, then the fragments in order: a playable fMP4.
        assert_eq!(std::fs::read(&spec.final_path).unwrap(), b"INITAAAABBBB");
        // No remux was attempted; fragmented MP4 already is MP4.
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(reqs.len(), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn dash_audio_is_fetched_alongside_video_rather_than_dropped() {
        let dir = tmp("dash-av");
        let (base, seen) = serve(vec![
            (
                "/m.mpd".into(),
                concat!(
                    r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period>"#,
                    r#"<AdaptationSet contentType="video">"#,
                    r#"<SegmentTemplate media="v/$Number$.m4s" initialization="v/i.mp4" duration="4" timescale="1" startNumber="1"/>"#,
                    r#"<Representation id="v0" bandwidth="800000" width="640" height="360"/>"#,
                    "</AdaptationSet>",
                    r#"<AdaptationSet contentType="audio">"#,
                    r#"<SegmentTemplate media="a/$Number$.m4a" initialization="a/i.mp4" duration="4" timescale="1" startNumber="1"/>"#,
                    r#"<Representation id="a0" bandwidth="64000"/>"#,
                    "</AdaptationSet></Period></MPD>"
                )
                .into(),
            ),
            ("/v/i.mp4".into(), b"VI".to_vec()),
            ("/v/1.m4s".into(), b"VVVV".to_vec()),
            ("/a/i.mp4".into(), b"AI".to_vec()),
            ("/a/1.m4a".into(), b"AAAA".to_vec()),
        ]);
        let mut spec = spec_for(11, format!("{base}/m.mpd"), &dir, "out.mp4");
        spec.protocol = "dash".into();
        let (_, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;

        // Both tracks were fetched, whatever happened at the muxing step.
        let reqs = seen.lock().unwrap().clone();
        for want in ["/v/i.mp4", "/v/1.m4s", "/a/i.mp4", "/a/1.m4a"] {
            assert!(
                reqs.iter().any(|r| r.starts_with(want)),
                "audio or video track was skipped: {reqs:?}"
            );
        }
        match hya_stream::hls::ffmpeg() {
            // With no ffmpeg the outcome is a plain explanation, and BOTH
            // tracks are kept and named rather than deleted.
            None => {
                let msg = failure.expect("combining without ffmpeg should not claim success");
                assert!(msg.contains("ffmpeg"), "unhelpful message: {msg}");
                assert!(dir.join("out.video.mp4").exists(), "video was thrown away");
                assert!(dir.join("out.audio.m4a").exists(), "audio was thrown away");
            }
            // With ffmpeg present it is invoked; these four-byte fixtures are
            // not real media, so it may still refuse them — what matters is
            // that it was tried and the tracks survived either way.
            Some(_) => {
                if let Some(msg) = failure {
                    assert!(msg.contains("ffmpeg"), "unexpected failure: {msg}");
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A live media playlist as it looks at one moment: a sliding window.
    /// A six-segment window, two seconds each, for the tests that care about
    /// how a window is fetched rather than what is in it.
    const SIX: &[&str] = &["s0.ts", "s1.ts", "s2.ts", "s3.ts", "s4.ts", "s5.ts"];

    fn live_playlist(first_seq: u64, names: &[&str], ended: bool) -> Vec<u8> {
        let mut p =
            format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:{first_seq}\n");
        for n in names {
            p.push_str(&format!("#EXTINF:2.000,\n{n}\n"));
        }
        if ended {
            p.push_str("#EXT-X-ENDLIST\n");
        }
        p.into_bytes()
    }

    /// A live window is a BACKLOG: the first refresh hands over everything
    /// the origin is currently holding, and a long-running recording keeps
    /// being handed several at a time. Taking them one at a time is what
    /// made a recording crawl on a single connection however many the user
    /// had configured — the connection table showed row 1 working and rows
    /// 2..8 blank, which is exactly what it was doing.
    #[tokio::test]
    async fn a_live_window_is_fetched_concurrently_and_still_written_in_order() {
        let dir = tmp("live-conc");
        // Six segments offered at once, each answered slowly enough that
        // serial fetching could not overlap by accident.
        // NO end tag on the first window: `#EXT-X-ENDLIST` makes the
        // playlist a VOD one, which routes past `record_live` entirely and
        // would leave this asserting about the wrong code path. The second
        // read repeats the window and ends it, so nothing new is taken.
        let (base, peak) = serve_peak(
            vec![
                (
                    "/index.m3u8".into(),
                    vec![
                        live_playlist(100, SIX, false),
                        live_playlist(100, SIX, true),
                    ],
                ),
                ("/s0.ts".into(), vec![b"AAAA".to_vec()]),
                ("/s1.ts".into(), vec![b"BBBB".to_vec()]),
                ("/s2.ts".into(), vec![b"CCCC".to_vec()]),
                ("/s3.ts".into(), vec![b"DDDD".to_vec()]),
                ("/s4.ts".into(), vec![b"EEEE".to_vec()]),
                ("/s5.ts".into(), vec![b"FFFF".to_vec()]),
            ],
            ".ts",
            80,
        );
        let spec = StreamSpec {
            container: "TS".into(),
            conns: 4,
            ..spec_for(30, format!("{base}/index.m3u8"), &dir, "live.ts")
        };
        let (finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);

        // Concurrency must not cost ordering: segments answer out of order
        // and are still appended in playlist order, because the append end
        // stays serial while the fetching end does not.
        assert_eq!(
            std::fs::read_to_string(&spec.final_path).unwrap(),
            "AAAABBBBCCCCDDDDEEEEFFFF"
        );
        assert_eq!(finished, Some(24));

        // The point of the change. One at a time would peak at 1.
        let seen = peak.load(Ordering::Relaxed);
        assert!(
            seen > 1,
            "a live window must be fetched concurrently, but the origin \
             never had more than {seen} request in the air"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--record-seconds` is a promise about how much MEDIA comes back, so
    /// a window bigger than the ask must be cut, not rounded up to.
    ///
    /// This is the regression that batching the window introduced: fetching
    /// a whole window at a time can only check the budget once per window,
    /// which overruns by however much the window held — and a DVR playlist
    /// whose first window carries hours would hand back all of it.
    #[tokio::test]
    async fn a_recording_deadline_cuts_the_window_it_lands_inside() {
        let dir = tmp("live-deadline");
        // One window, six segments, two seconds each: twelve seconds offered
        // against a four-second ask.
        let (base, _seen) = serve_sequence(vec![
            (
                "/index.m3u8".into(),
                // Live, so this reaches `record_live`; the repeat lets the
                // recording end if the budget somehow fails to stop it.
                vec![
                    live_playlist(100, SIX, false),
                    live_playlist(100, SIX, true),
                ],
            ),
            ("/s0.ts".into(), vec![b"AAAA".to_vec()]),
            ("/s1.ts".into(), vec![b"BBBB".to_vec()]),
            ("/s2.ts".into(), vec![b"CCCC".to_vec()]),
            ("/s3.ts".into(), vec![b"DDDD".to_vec()]),
            ("/s4.ts".into(), vec![b"EEEE".to_vec()]),
            ("/s5.ts".into(), vec![b"FFFF".to_vec()]),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            max_seconds: Some(4),
            ..spec_for(31, format!("{base}/index.m3u8"), &dir, "cut.ts")
        };
        let (_finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);

        // Two segments cover the four seconds asked for. The whole window
        // would be "AAAABBBBCCCCDDDDEEEEFFFF" — three times the ask.
        assert_eq!(
            std::fs::read_to_string(&spec.final_path).unwrap(),
            "AAAABBBB"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_live_hls_playlist_is_recorded_until_it_ends() {
        let dir = tmp("live-hls");
        // The window slides: each refresh drops one segment and adds one.
        // Position means nothing across refreshes — only the sequence number.
        let (base, seen) = serve_sequence(vec![
            (
                "/index.m3u8".into(),
                vec![
                    live_playlist(100, &["s0.ts", "s1.ts"], false),
                    live_playlist(101, &["s1.ts", "s2.ts"], false),
                    live_playlist(102, &["s2.ts", "s3.ts"], true),
                ],
            ),
            ("/s0.ts".into(), vec![b"AAAA".to_vec()]),
            ("/s1.ts".into(), vec![b"BBBB".to_vec()]),
            ("/s2.ts".into(), vec![b"CCCC".to_vec()]),
            ("/s3.ts".into(), vec![b"DDDD".to_vec()]),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(20, format!("{base}/index.m3u8"), &dir, "live.ts")
        };
        let (finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);
        // Every segment exactly once, in order, across three windows.
        assert_eq!(
            std::fs::read_to_string(&spec.final_path).unwrap(),
            "AAAABBBBCCCCDDDD"
        );
        assert_eq!(finished, Some(16));
        // Overlapping entries were recognised as already taken, not refetched.
        let reqs = seen.lock().unwrap().clone();
        for seg in ["/s0.ts", "/s1.ts", "/s2.ts", "/s3.ts"] {
            assert_eq!(
                reqs.iter().filter(|r| r.starts_with(seg)).count(),
                1,
                "{seg} was fetched more than once: {reqs:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stopping_a_recording_keeps_what_was_recorded() {
        let dir = tmp("live-stop");
        // Never ends: only the user stops it.
        let (base, _) = serve_sequence(vec![
            (
                "/index.m3u8".into(),
                vec![live_playlist(1, &["s0.ts"], false)],
            ),
            ("/s0.ts".into(), vec![b"KEEPME".to_vec()]),
        ]);
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec_for(21, format!("{base}/index.m3u8"), &dir, "rec.ts")
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = {
            let cancel = cancel.clone();
            let path = spec.final_path.clone();
            let part = format!("{}.t0", spec.temp_path);
            tokio::spawn(async move {
                let _ = path;
                for _ in 0..200 {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    // Stop once the first segment is on disk.
                    if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) >= 6 {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let (finished, failure) = drive(spec.clone(), cancel).await;
        stopper.abort();
        // Stopping a recording is a SUCCESS: there is nothing to resume into
        // later, and the user wants the file they have.
        assert_eq!(failure, None, "stopping a recording must not be a failure");
        assert_eq!(finished, Some(6));
        assert_eq!(std::fs::read_to_string(&spec.final_path).unwrap(), "KEEPME");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_live_dash_manifest_is_recorded_from_its_sliding_timeline() {
        let dir = tmp("live-dash");
        let mpd = |start: u64, t: u64| {
            format!(
                concat!(
                    r#"<MPD type="dynamic" minimumUpdatePeriod="PT1S" availabilityStartTime="2026-01-01T00:00:00Z"><Period>"#,
                    r#"<AdaptationSet contentType="video">"#,
                    r#"<Representation id="v0" bandwidth="800000" width="640" height="360">"#,
                    r#"<SegmentTemplate initialization="v/init.m4v" media="v/seg-$Number$.m4v" timescale="1000" startNumber="{start}">"#,
                    r#"<SegmentTimeline><S t="{t}" d="2000" r="1"/></SegmentTimeline>"#,
                    "</SegmentTemplate></Representation>",
                    "</AdaptationSet></Period></MPD>"
                ),
                start = start,
                t = t
            )
            .into_bytes()
        };
        let (base, seen) = serve_sequence(vec![
            // Window slides by one each refresh: 10,11 -> 11,12 -> 12,13.
            (
                "/m.mpd".into(),
                vec![mpd(10, 0), mpd(11, 2000), mpd(12, 4000)],
            ),
            ("/v/init.m4v".into(), vec![b"IN".to_vec()]),
            ("/v/seg-10.m4v".into(), vec![b"AA".to_vec()]),
            ("/v/seg-11.m4v".into(), vec![b"BB".to_vec()]),
            ("/v/seg-12.m4v".into(), vec![b"CC".to_vec()]),
            ("/v/seg-13.m4v".into(), vec![b"DD".to_vec()]),
        ]);
        let mut spec = spec_for(22, format!("{base}/m.mpd"), &dir, "live.mp4");
        spec.protocol = "dash".into();
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = {
            let (cancel, part) = (cancel.clone(), format!("{}.t0", spec.temp_path));
            tokio::spawn(async move {
                for _ in 0..300 {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    // init + 4 segments = 10 bytes.
                    if std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) >= 10 {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let (_, failure) = drive(spec.clone(), cancel).await;
        stopper.abort();
        assert_eq!(failure, None);
        // Init map first, then each $Number$ exactly once despite the
        // overlapping windows.
        assert_eq!(
            std::fs::read_to_string(&spec.final_path).unwrap(),
            "INAABBCCDD"
        );
        let reqs = seen.lock().unwrap().clone();
        assert_eq!(
            reqs.iter().filter(|r| r.starts_with("/v/init")).count(),
            1,
            "the init map was fetched more than once"
        );
        for n in 10..=13 {
            assert_eq!(
                reqs.iter()
                    .filter(|r| r.starts_with(&format!("/v/seg-{n}.m4v")))
                    .count(),
                1,
                "segment {n} was refetched"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_url_that_is_not_a_manifest_says_so_instead_of_guessing() {
        let dir = tmp("notmanifest");
        // The MDN sample the browser would sniff as a DIRECT file: it must
        // never reach the stream path, and if it does the message has to be
        // about that, not about "no segments".
        let (base, _) = serve(vec![(
            "/flower.mp4".into(),
            b"\x00\x00\x00\x20ftypisom".to_vec(),
        )]);
        let spec = spec_for(12, format!("{base}/flower.mp4"), &dir, "flower.mp4");
        let (_, failure) = drive(spec, Arc::new(AtomicBool::new(false))).await;
        let msg = failure.expect("a plain MP4 is not a manifest");
        assert!(
            msg.contains("not an HLS or DASH manifest"),
            "misleading message: {msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Mirrors `hls::WINDOW`; the resume test needs to know where one window
    /// ends in order to pause inside the next.
    const WINDOW_FOR_TEST: usize = 6;

    #[tokio::test]
    async fn a_paused_stream_carries_on_instead_of_refetching() {
        let dir = tmp("resume");
        // Two windows: the first completes and is checkpointed, the second
        // is interrupted.
        let count = 8usize;
        let mut playlist = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        let mut routes: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..count {
            let name = if i >= WINDOW_FOR_TEST {
                format!("seg{i}.slow.ts")
            } else {
                format!("seg{i}.ts")
            };
            playlist.push_str(&format!("#EXTINF:2.0,\n{name}\n"));
            routes.push((format!("/{name}"), format!("<{i}>").into_bytes()));
        }
        playlist.push_str("#EXT-X-ENDLIST\n");
        routes.push(("/index.m3u8".into(), playlist.into_bytes()));
        // Segments in the second window are slow, which gives the pause
        // somewhere deterministic to land.
        let (base, seen) = serve_delayed(routes, "slow", 400);

        let spec = spec_for(13, format!("{base}/index.m3u8"), &dir, "out.ts");
        let spec = StreamSpec {
            container: "TS".into(),
            ..spec
        };

        // Cancel once the first window has been served.
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (seen, cancel) = (seen.clone(), cancel.clone());
            tokio::spawn(async move {
                for _ in 0..300 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let served = seen
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r.starts_with("/seg"))
                        .count();
                    if served > WINDOW_FOR_TEST {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let (_, failure) = drive(spec.clone(), cancel).await;
        watcher.abort();
        assert_eq!(failure, None, "pausing should not be a failure");
        assert!(
            !std::path::Path::new(&spec.final_path).exists(),
            "a paused stream must not produce a finished file"
        );
        // The staging file and its checkpoint are what make the resume work.
        let ckpt = hya_stream::hls::Checkpoint::read(&format!("{}.t0.ck", spec.temp_path));
        assert!(ckpt.segments > 0, "nothing was checkpointed");
        let already = ckpt.segments;

        // Second run: only the segments that are still missing are asked for.
        seen.lock().unwrap().clear();
        let (finished, failure) = drive(spec.clone(), Arc::new(AtomicBool::new(false))).await;
        assert_eq!(failure, None);
        let second: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("/seg"))
            .map(|r| r.lines().next().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            second.len(),
            count - already as usize,
            "resumed run refetched settled segments: {second:?}"
        );

        // And the result is byte-identical to a clean run.
        let want: String = (0..count).map(|i| format!("<{i}>")).collect();
        assert_eq!(std::fs::read_to_string(&spec.final_path).unwrap(), want);
        assert_eq!(finished, Some(want.len() as u64));
        // Staging files and checkpoints are cleaned up on success.
        assert!(!std::path::Path::new(&format!("{}.t0", spec.temp_path)).exists());
        assert!(!std::path::Path::new(&format!("{}.t0.ck", spec.temp_path)).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_missing_segment_fails_the_transfer_rather_than_leaving_a_hole() {
        let dir = tmp("hole");
        let (base, _) = serve(vec![
            (
                "/index.m3u8".into(),
                concat!(
                    "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n",
                    "#EXTINF:2.0,\nseg0.ts\n#EXTINF:2.0,\ngone.ts\n#EXT-X-ENDLIST\n"
                )
                .into(),
            ),
            ("/seg0.ts".into(), b"AAAA".to_vec()),
        ]);
        let final_path = dir.join("out.ts");
        let spec = StreamSpec {
            id: 4,
            manifest: format!("{base}/index.m3u8"),
            protocol: "hls".into(),
            conns: 0,
            max_seconds: None,
            limit: None,
            variant_url: None,
            height: None,
            bandwidth: None,
            container: "TS".into(),
            cookies: None,
            referer: None,
            user_agent: "hydra-test/1".into(),
            temp_path: dir.join("out.part").to_string_lossy().into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
        };
        let (tx, mut rx) = unbounded_channel();
        run_stream(
            spec.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(RateLimiter::new(0)),
            Arc::new(Mutex::new(spec.final_path.clone())),
            tx,
        )
        .await;
        let mut failure = None;
        while let Ok(e) = rx.try_recv() {
            if let Event::Failed { error, .. } = e {
                failure = Some(error);
            }
        }
        assert!(
            failure
                .as_deref()
                .unwrap_or("")
                .contains("segment download failed"),
            "unexpected outcome: {failure:?}"
        );
        // A partial assembly must never be presented as the finished file.
        assert!(!final_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Read a manifest and report what it offers, without downloading anything.
///
/// This is what lets the Add URL dialog show a quality list before the user
/// commits: a manifest is a few kilobytes, so asking it what it contains
/// costs one request and answers the only question that matters — which
/// rendition, and is this live.
pub async fn probe_stream(
    url: String,
    user_agent: String,
    cookies: Option<String>,
) -> Result<StreamProbe, String> {
    let connector = shared_connector()?;
    let cap = hya_stream::hls::playlist_cap();
    let body = stream_get(&connector, &url, cookies.as_deref(), None, &user_agent, cap)
        .await
        .map_err(|e| format!("could not read the manifest: {e}"))?;
    let text = String::from_utf8_lossy(&body).into_owned();

    if text.contains("<MPD") {
        let mf = hya_stream::dash::parse(&text, &url);
        if mf.video.is_empty() {
            return Err("the manifest lists no video renditions".into());
        }
        return Ok(StreamProbe {
            protocol: "dash".into(),
            live: mf.live,
            duration: (mf.duration > 0.0).then_some(mf.duration),
            qualities: mf
                .video
                .iter()
                .map(|t| StreamQuality {
                    label: quality_label(t.height, t.bandwidth),
                    height: t.height,
                    bandwidth: t.bandwidth,
                    url: None,
                })
                .collect(),
            separate_audio: mf.choose_audio().is_some(),
            drm: mf.drm.clone(),
            // DASH is fragmented MP4; `.ts` would need a real remux.
            is_ts: false,
        });
    }
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("this URL is not an HLS or DASH manifest".into());
    }

    let pl = hya_stream::hls::parse(&text, &url);
    // A master lists renditions; a media playlist is the single rendition,
    // and only IT knows whether the stream is live and what it is made of.
    let (live, is_ts) = if pl.is_master() {
        match pl.variants.last() {
            Some(cheapest) => {
                let probe = stream_get(
                    &connector,
                    &cheapest.url,
                    cookies.as_deref(),
                    None,
                    &user_agent,
                    cap,
                )
                .await
                .ok()
                .map(|b| hya_stream::hls::parse(&String::from_utf8_lossy(&b), &cheapest.url));
                match probe {
                    Some(p) => (
                        p.live,
                        p.segments_kind == Some(hya_stream::hls::Segments::Ts),
                    ),
                    None => (false, true),
                }
            }
            None => (pl.live, true),
        }
    } else {
        (
            pl.live,
            pl.segments_kind != Some(hya_stream::hls::Segments::Fmp4),
        )
    };

    let qualities = if pl.is_master() {
        pl.variants
            .iter()
            .map(|v| StreamQuality {
                label: quality_label(v.height, v.bandwidth),
                height: v.height,
                bandwidth: v.bandwidth,
                url: Some(v.url.clone()),
            })
            .collect()
    } else {
        vec![StreamQuality {
            label: crate::i18n::tr("Only one quality"),
            height: None,
            bandwidth: None,
            url: None,
        }]
    };
    Ok(StreamProbe {
        protocol: "hls".into(),
        live,
        duration: (pl.duration > 0.0).then_some(pl.duration),
        qualities,
        separate_audio: false,
        drm: pl.drm.clone(),
        is_ts,
    })
}

fn quality_label(height: Option<u32>, bandwidth: Option<u64>) -> String {
    let q = match height {
        Some(h) if h >= 720 => format!("{h}p HD"),
        Some(h) => format!("{h}p"),
        None => crate::i18n::tr("auto"),
    };
    match bandwidth {
        Some(b) if b > 0 => format!("{q}  ({} kbps)", b / 1000),
        _ => q,
    }
}

#[cfg(test)]
mod peek_zip_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    /// An origin that answers HEAD with the size and `Accept-Ranges`, and a
    /// ranged GET with exactly that span — the two requests a peek makes.
    /// Counts the body bytes it sends, which is the number the feature is
    /// about: the archive must not be downloaded to be listed.
    fn serve(object: Vec<u8>) -> (u16, Arc<AtomicU64>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let sent = Arc::new(AtomicU64::new(0));
        let counter = sent.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Range: bytes=") {
                        let (lo, hi) = v.trim().split_once('-').unwrap();
                        range = Some((lo.parse::<usize>().unwrap(), hi.parse::<usize>().unwrap()));
                    }
                }
                let total = object.len();
                let (head, body): (String, &[u8]) = if line.starts_with("HEAD") {
                    (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nContent-Type: application/zip\r\nConnection: close\r\n\r\n"),
                        &[],
                    )
                } else if let Some((lo, hi)) = range {
                    let hi = hi.min(total - 1);
                    (
                        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {lo}-{hi}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", hi - lo + 1),
                        &object[lo..=hi],
                    )
                } else {
                    (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"),
                        &object[..],
                    )
                };
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(body);
                counter.fetch_add(body.len() as u64, Ordering::SeqCst);
                let _ = sock.flush();
            }
        });
        (port, sent)
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn a_small_archive_is_read_whole() {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file("a.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"hello").unwrap();
        let file = w.finish().unwrap().into_inner();
        let (port, _) = serve(file);
        let url = format!("http://127.0.0.1:{port}/small.zip");
        let entries = block_on(peek_zip(url, "test".into(), vec![], None)).expect("peek");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 5);
    }

    /// With the size in hand the peek makes no HEAD at all, and a redirect
    /// on the ranged GET is followed rather than reported.
    #[test]
    fn a_known_size_skips_the_probe_and_follows_redirects() {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file("b.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"world").unwrap();
        let file = w.finish().unwrap().into_inner();
        let total = file.len() as u64;
        let (port, _) = serve(file);
        // A redirector in front, answering everything with a 302 to the origin.
        let hop = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let hop_port = hop.local_addr().unwrap().port();
        let heads = Arc::new(AtomicU64::new(0));
        let counted = heads.clone();
        std::thread::spawn(move || {
            for conn in hop.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                if line.starts_with("HEAD") {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let _ = sock.write_all(
                    format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/b.zip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
                );
                let _ = sock.flush();
            }
        });
        let url = format!("http://127.0.0.1:{hop_port}/go");
        let entries = block_on(peek_zip(url, "test".into(), vec![], Some(total))).expect("peek");
        assert_eq!(entries[0].name, "b.txt");
        assert_eq!(heads.load(Ordering::SeqCst), 0, "no probe was made");
    }

    #[test]
    fn a_non_archive_is_named_as_such() {
        let junk = vec![b'x'; 200_000];
        let (port, _) = serve(junk);
        let url = format!("http://127.0.0.1:{port}/not.zip");
        let err = block_on(peek_zip(url, "test".into(), vec![], None)).expect_err("not a zip");
        assert_eq!(err, crate::i18n::tr("This file is not a ZIP archive."));
    }

    /// Manual timing run: `HYDRA_PEEK_URL=<zip url> cargo test -p hya-gui \
    /// peek_phases -- --ignored --nocapture`. Prints how long each of the
    /// peek's network phases takes against a real origin.
    #[test]
    #[ignore]
    fn peek_phases_timing() {
        let Ok(url) = std::env::var("HYDRA_PEEK_URL") else {
            return;
        };
        let original = url.clone();
        block_on(async {
            let t0 = std::time::Instant::now();
            let connector = shared_connector().unwrap();
            let c = connector.as_ref();
            let (url, p) = resolve_link(c, url, "hydra-test", &[])
                .await
                .expect("resolve");
            eprintln!(
                "resolve: {:?}  size={} status={} ranges={}",
                t0.elapsed(),
                p.size,
                p.status,
                p.ranges
            );
            let u = parse_url(&url).unwrap();
            let t = target_of(&u, vec![], "hydra-test");
            let t1 = std::time::Instant::now();
            let total = p.size;
            let tail = hya_net::fetch_small_range(
                c,
                &t,
                total - hya_net::zipdir::TAIL_LEN,
                total - 1,
                hya_net::zipdir::TAIL_LEN as usize,
            )
            .await
            .expect("tail");
            eprintln!("tail: {:?}  {} bytes", t1.elapsed(), tail.len());
            let dir = hya_net::zipdir::locate(&tail, total).unwrap();
            eprintln!(
                "dir: offset={} len={} in_tail={}",
                dir.offset,
                dir.len,
                dir.offset >= total - tail.len() as u64
            );
            let t2 = std::time::Instant::now();
            let all = peek_zip(url, "hydra-test".into(), vec![], None)
                .await
                .expect("peek");
            eprintln!(
                "peek_zip, probing: {:?}  {} entries",
                t2.elapsed(),
                all.len()
            );
            let t3 = std::time::Instant::now();
            let all = peek_zip(original, "hydra-test".into(), vec![], Some(total))
                .await
                .expect("peek");
            eprintln!(
                "peek_zip, size known, from the original url: {:?}  {} entries",
                t3.elapsed(),
                all.len()
            );
        });
    }

    #[test]
    fn credentials_become_headers() {
        let h = request_headers(Some(("me", "pw")), Some("  sid=1  "));
        assert_eq!(h, ["Authorization: Basic bWU6cHc=", "Cookie: sid=1"]);
        assert!(request_headers(None, Some("   ")).is_empty());
    }
}

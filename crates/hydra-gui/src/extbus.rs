// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Browser-extension bridge: a loopback TCP listener the native-messaging
//! host (`hydra-host`) connects to on behalf of the Chrome/Firefox add-ons.
//!
//! Why TCP and not a unix socket / named pipe: one code path on every OS,
//! plain `std::net` (no extra runtime plumbing), and the browser can never
//! reach it directly anyway — only `hydra-host` does, authenticated by a
//! random token. Port and token are published to `<app_dir>/ipc.json`,
//! readable only by the owning user, which is the same trust boundary the
//! config itself lives behind.
//!
//! Wire format: one JSON object per line, one reply line per request.
//! Every reply carries the current capture settings so the extension's
//! cached filter list converges without a dedicated poll.
//!
//! There are two front doors to the same request handler:
//!  - the ephemeral line-protocol port, used by `hydra-host`, authenticated
//!    by the ipc.json token (browsers cannot read files, hosts can);
//!  - a WebSocket listener on a KNOWN port (6799, fallback 16799), spoken
//!    directly by the browser extension, authenticated by the `Origin`
//!    header (browsers always send `chrome-extension://...` and never let a
//!    page forge it). A live WS connection doubles as the extension's
//!    "hydra is running" indicator.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// A download handed over by the browser. Everything except `url` is
/// best-effort: the dialog shows what arrived and probes the rest.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtDownload {
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
    /// Verbatim `Cookie:` header assembled by the extension for this URL.
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub mime: Option<String>,
    /// Page the download started from (shown nowhere yet; logged).
    #[serde(default)]
    pub tab_url: Option<String>,
}

/// One rendition of a stream, as the extension read it out of the manifest.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtVariant {
    /// The variant playlist itself, when the manifest named one. Used as the
    /// fallback target when `ExtStream::variant_url` is absent.
    #[serde(default)]
    pub url: Option<String>,
    /// Height is what a person picks by, and what the engine matches on.
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub bandwidth: Option<u64>,
    #[serde(default)]
    pub codecs: Option<String>,
}

/// An adaptive stream handed over by the browser. Distinct from
/// [`ExtDownload`] on purpose: the URL names a MANIFEST, not a file, so the
/// ordinary capture path would save a few kilobytes of playlist text.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ExtStream {
    /// The manifest, as the browser requested it.
    pub url: String,
    /// "hls" or "dash".
    #[serde(default)]
    pub protocol: Option<String>,
    /// The exact variant playlist the user chose in the popup or the
    /// in-page panel.
    #[serde(default)]
    pub variant_url: Option<String>,
    #[serde(default)]
    pub variant: Option<ExtVariant>,
    /// "MP4" or "TS": the container the user picked.
    #[serde(default)]
    pub container: Option<String>,
    /// Suggested name, taken from the page title.
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub tab_url: Option<String>,
    #[serde(default)]
    pub live: bool,
    /// Seconds, when the manifest stated one.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Bitrate x duration, as the extension estimated it.
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub enum ExtEvent {
    /// Single captured download -> Download File Info dialog.
    Download(ExtDownload),
    /// A manifest -> the stream-aware download path.
    Stream(Box<ExtStream>),
    /// "Download all links": many URLs -> the batch window.
    Links(Vec<String>),
    /// Popup's "Open Hydra": surface the main window.
    Open,
    /// A newer build launched and asked this instance to step aside so the
    /// surviving process is the new version (see `signal_existing`).
    Shutdown,
}

/// Capture-policy snapshot the UI thread publishes for the socket threads.
/// Mirrors the Options fields the extension needs for filtering.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ExtConfig {
    /// Capture flag for the browser that asked. Resolved per request from
    /// `browsers` — an extension only ever sees its own row.
    pub capture: bool,
    /// Space-separated extension list (Options > File Types).
    pub auto_types: String,
    /// Sites whose downloads are never captured.
    pub dont_start_sites: String,
    /// Every row of Options > General > "Capture downloads from the following
    /// browsers", in order. Not serialized: the extension is told about
    /// itself, not about the user's other browsers.
    #[serde(skip)]
    pub browsers: Vec<(String, bool)>,
}

static TX: OnceLock<UnboundedSender<ExtEvent>> = OnceLock::new();
static RX: Mutex<Option<UnboundedReceiver<ExtEvent>>> = Mutex::new(None);
static CFG: Mutex<Option<ExtConfig>> = Mutex::new(None);

fn sender() -> UnboundedSender<ExtEvent> {
    TX.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        if let Ok(mut g) = RX.lock() {
            *g = Some(rx);
        }
        tx
    })
    .clone()
}

/// Take the receiving end (once), for the iced subscription.
pub fn take_events() -> Option<UnboundedReceiver<ExtEvent>> {
    let _ = sender();
    RX.lock().ok().and_then(|mut g| g.take())
}

/// UI thread pushes the fields the extension mirrors; called at boot and on
/// every config save, so the socket side never touches `App`.
pub fn publish_config(cfg: &crate::model::ConfigFile) {
    let browsers = cfg.settings.capture_browsers.clone();
    // Default for a request that does not say which browser it is (an
    // extension older than the `browser` field): the Chromium-family row,
    // which is what the previous single-flag behaviour meant.
    let capture = browsers
        .iter()
        .find(|(name, _)| name.contains("Chrome"))
        .map(|(_, on)| *on)
        .unwrap_or(true);
    let snap = ExtConfig {
        capture,
        auto_types: cfg.settings.auto_types.clone(),
        dont_start_sites: cfg.settings.dont_start_sites.clone(),
        browsers,
    };
    if let Ok(mut g) = CFG.lock() {
        *g = Some(snap);
    }
}

/// Browsers that have spoken to us recently, by the label Options shows.
/// The extension heartbeats every 20 s, so anything inside a minute is a
/// live connection; the window is generous so one dropped beat does not
/// make the row flicker.
static SEEN: Mutex<Option<Vec<(String, std::time::Instant)>>> = Mutex::new(None);
const SEEN_TTL: std::time::Duration = std::time::Duration::from_secs(60);

fn note_browser(name: &str) {
    let Ok(mut g) = SEEN.lock() else { return };
    let list = g.get_or_insert_with(Vec::new);
    let now = std::time::Instant::now();
    match list.iter_mut().find(|(n, _)| n == name) {
        Some(e) => e.1 = now,
        None => list.push((name.to_string(), now)),
    }
}

/// Labels of the browsers whose extension is connected right now, for the
/// status column in Options > General.
pub fn live_browsers() -> Vec<String> {
    let Ok(g) = SEEN.lock() else { return vec![] };
    let now = std::time::Instant::now();
    g.as_ref()
        .map(|l| {
            l.iter()
                .filter(|(_, t)| now.duration_since(*t) < SEEN_TTL)
                .map(|(n, _)| n.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Resolve the row for the browser that sent this request. Matching is by
/// the exact label the extension reports, then by keyword, so a rename in
/// the settings list does not silently disable capture.
fn capture_for(cfg: &ExtConfig, browser: Option<&str>) -> bool {
    let Some(want) = browser else {
        return cfg.capture;
    };
    let want_l = want.to_ascii_lowercase();
    if let Some((_, on)) = cfg
        .browsers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(want))
    {
        return *on;
    }
    for key in ["edge", "opera", "firefox", "safari", "chrome"] {
        if want_l.contains(key) {
            if let Some((_, on)) = cfg
                .browsers
                .iter()
                .find(|(n, _)| n.to_ascii_lowercase().contains(key))
            {
                return *on;
            }
        }
    }
    cfg.capture
}

fn config_snapshot() -> ExtConfig {
    CFG.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

/// 128 bits from two independently OS-seeded SipHash keys. Not a general
/// CSPRNG, but ample for a loopback token whose alternative reader would
/// already own the user account (and could read ipc.json directly).
fn make_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for salt in 0u64..2 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64 ^ salt);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Probe for an already-running instance via its ipc.json. When one
/// answers, ask it to surface its main window (unless we were an autostart
/// `--minimized` launch — then a bare ping) and report `true` so the caller
/// can exit: one instance owns the state db, the tray, and this socket.
/// A stale ipc.json (dead port, wrong token) reads as "no instance".
///
/// Exception: when the answering instance reports a DIFFERENT version, an
/// old build is still resident after an upgrade — handing over would keep
/// the old code (and its About-window version) on screen forever. Instead
/// it is asked to quit, and once its socket goes dark this returns `false`
/// so the caller boots as the new instance. Builds that predate "shutdown"
/// refuse it as unknown; they get the classic hand-over.
pub fn signal_existing(minimized: bool) -> bool {
    let Ok(text) = std::fs::read_to_string(crate::model::app_dir().join("ipc.json")) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let (Some(port), Some(token)) = (
        v.get("port").and_then(|p| p.as_u64()),
        v.get("token").and_then(|t| t.as_str()),
    ) else {
        return false;
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port as u16));
    let Ok(s) = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let Ok(mut out) = s.try_clone() else {
        return false;
    };
    let mut reader = BufReader::new(s);
    let mut request = move |kind: &str| -> Option<serde_json::Value> {
        writeln!(out, "{{\"type\":\"{kind}\",\"token\":\"{token}\"}}").ok()?;
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    };
    let acked = |r: &serde_json::Value| r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);

    let Some(pong) = request("ping") else {
        return false;
    };
    if !acked(&pong) {
        return false;
    }

    let theirs = pong.get("version").and_then(|t| t.as_str()).unwrap_or("");
    if theirs != env!("CARGO_PKG_VERSION") && request("shutdown").is_some_and(|r| acked(&r)) {
        // It saves state and exits; the socket dying is the all-clear.
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_err() {
                return false;
            }
        }
        // Still alive after 5 s (wedged exit path?): treat it as the owner.
    }

    if !minimized {
        return request("open").is_some_and(|r| acked(&r));
    }
    true
}

/// Bind the listener and publish `<app_dir>/ipc.json`. Failure to bind is
/// logged and otherwise ignored: the GUI works fine without the bridge.
pub fn start() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => {
            crate::log::warn(&format!("extbus: bind failed: {e}"));
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            crate::log::warn(&format!("extbus: local_addr failed: {e}"));
            return;
        }
    };
    let token = make_token();

    // The extension-facing WebSocket port must be knowable without reading
    // a file, so it is fixed with one fallback.
    let ws = WS_PORTS
        .iter()
        .find_map(|p| TcpListener::bind(("127.0.0.1", *p)).ok());
    let ws_port = ws
        .as_ref()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port());
    match ws_port {
        Some(p) => crate::log::info(&format!("extbus: websocket on 127.0.0.1:{p}")),
        None => crate::log::warn(
            "extbus: websocket ports busy; extension will use the native host path",
        ),
    }

    let dir = crate::model::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ipc.json");
    let body = format!(
        "{{\"port\":{port},\"token\":\"{token}\",\"ws_port\":{},\"pid\":{}}}\n",
        ws_port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "null".into()),
        std::process::id()
    );
    if let Err(e) = std::fs::write(&path, &body) {
        crate::log::warn(&format!("extbus: cannot write {}: {e}", path.display()));
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    crate::log::info(&format!("extbus: listening on 127.0.0.1:{port}"));

    std::thread::Builder::new()
        .name("extbus-accept".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let tok = token.clone();
                let _ = std::thread::Builder::new()
                    .name("extbus-conn".into())
                    .spawn(move || serve(stream, &tok));
            }
        })
        .ok();

    if let Some(ws) = ws {
        std::thread::Builder::new()
            .name("extbus-ws-accept".into())
            .spawn(move || {
                for conn in ws.incoming() {
                    let Ok(stream) = conn else { continue };
                    let _ = std::thread::Builder::new()
                        .name("extbus-ws".into())
                        .spawn(move || serve_ws(stream));
                }
            })
            .ok();
    }
}

/// Extension-facing WebSocket ports, fixed so the browser needs no file
/// access to find us. One fallback for the unlucky case of the first being
/// taken by something else.
const WS_PORTS: &[u16] = &[6799, 16799];

/// Handle one authenticated request; the reply always carries the capture
/// settings and echoes any `id` (the WebSocket path multiplexes on it).
/// `trusted` is true for the token-bearing line protocol only — the
/// WebSocket path is origin-authenticated, which any extension context
/// satisfies, so takeover commands are refused there.
fn dispatch(req: &serde_json::Value, trusted: bool) -> serde_json::Value {
    let (ok, err): (bool, Option<&str>) = match req.get("type").and_then(|t| t.as_str()) {
        Some("ping") | Some("config") => (true, None),
        Some("open") => {
            let _ = sender().send(ExtEvent::Open);
            (true, None)
        }
        // Version-mismatch takeover from signal_existing: quit gracefully
        // so the newly launched build can become the instance.
        Some("shutdown") if trusted => {
            let _ = sender().send(ExtEvent::Shutdown);
            (true, None)
        }
        Some("download") => match serde_json::from_value::<ExtDownload>(req.clone()) {
            Ok(dl) if !dl.url.is_empty() => {
                // referer/user_agent/size arrive for the log today; the
                // engine's StartSpec grows per-download headers later.
                crate::log::info(&format!(
                    "extbus: capture {} (mime={} size={} referer={} ua={} from={})",
                    dl.url,
                    dl.mime.as_deref().unwrap_or("-"),
                    dl.size.map(|s| s.to_string()).as_deref().unwrap_or("-"),
                    dl.referer.as_deref().unwrap_or("-"),
                    dl.user_agent.as_deref().unwrap_or("-"),
                    dl.tab_url.as_deref().unwrap_or("-"),
                ));
                let _ = sender().send(ExtEvent::Download(dl));
                (true, None)
            }
            _ => (false, Some("bad download request")),
        },
        Some("stream") => match serde_json::from_value::<ExtStream>(req.clone()) {
            Ok(s) if !s.url.is_empty() => {
                crate::log::info(&format!(
                    "extbus: stream {} ({} {}, container={}, live={}, from={})",
                    s.url,
                    s.protocol.as_deref().unwrap_or("?"),
                    s.variant
                        .as_ref()
                        .and_then(|v| v.height)
                        .map(|h| format!("{h}p"))
                        .unwrap_or_else(|| "auto".into()),
                    s.container.as_deref().unwrap_or("MP4"),
                    s.live,
                    s.tab_url.as_deref().unwrap_or("-"),
                ));
                crate::log::debug(&format!(
                    "extbus: stream codecs={} duration={} ua={}",
                    s.variant
                        .as_ref()
                        .and_then(|v| v.codecs.as_deref())
                        .unwrap_or("-"),
                    s.duration
                        .map(|d| format!("{d:.0}s"))
                        .as_deref()
                        .unwrap_or("-"),
                    s.user_agent.as_deref().unwrap_or("-"),
                ));
                let _ = sender().send(ExtEvent::Stream(Box::new(s)));
                (true, None)
            }
            _ => (false, Some("bad stream request")),
        },
        Some("links") => {
            let urls: Vec<String> = req
                .get("urls")
                .and_then(|u| u.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if urls.is_empty() {
                (false, Some("no urls"))
            } else {
                crate::log::info(&format!("extbus: {} links", urls.len()));
                let _ = sender().send(ExtEvent::Links(urls));
                (true, None)
            }
        }
        _ => (false, Some("unknown type")),
    };

    let cfg = config_snapshot();
    // Every reply carries the settings the extension mirrors — but `capture`
    // is that BROWSER's checkbox, not one flag shared by all of them.
    let browser = req.get("browser").and_then(|b| b.as_str());
    if let Some(b) = browser {
        note_browser(b);
    }
    let mut v = serde_json::json!({
        "ok": ok,
        "capture": capture_for(&cfg, browser),
        "auto_types": cfg.auto_types,
        "dont_start_sites": cfg.dont_start_sites,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Some(e) = err {
        v["error"] = serde_json::Value::String(e.into());
    }
    if let Some(id) = req.get("id") {
        v["id"] = id.clone();
    }
    v
}

// ------------------------------------------------------- line protocol

fn serve(stream: TcpStream, token: &str) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // Hard cap: nothing legitimate sends megabytes per request.
        if line.len() > 4 * 1024 * 1024 {
            break;
        }
        let reply = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) if req.get("token").and_then(|t| t.as_str()) == Some(token) => {
                dispatch(&req, true)
            }
            Ok(_) => {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::json!({"ok": false, "error": "bad token"})
                );
                break;
            }
            Err(_) => serde_json::json!({"ok": false, "error": "bad json"}),
        };
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}

// ----------------------------------------------------------- websocket

/// Minimal RFC 6455 server side: enough for one browser extension speaking
/// small text frames. No fragmentation, no extensions, no TLS (loopback).
fn serve_ws(stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(300)));
    let _ = stream.set_nodelay(true);
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);

    // ---- handshake -----------------------------------------------------
    let mut head = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
        if head.len() > 16 * 1024 {
            return;
        }
    }
    let header = |name: &str| -> Option<String> {
        head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };

    // A browser always stamps extension contexts with their real origin and
    // pages cannot forge it — this is the authentication. (Native processes
    // could connect, but a same-user process already owns ~/.config/hydra.)
    let origin = header("Origin").unwrap_or_default();
    let origin_ok = [
        "chrome-extension://",
        "moz-extension://",
        "safari-web-extension://",
    ]
    .iter()
    .any(|p| origin.starts_with(p));
    let key = header("Sec-WebSocket-Key");
    let upgrade_ok = header("Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let (Some(key), true, true) = (key, upgrade_ok, origin_ok) else {
        let _ = out.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
        crate::log::warn(&format!(
            "extbus: rejected ws handshake (origin: {origin:?})"
        ));
        return;
    };

    let accept = {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        h.update(key.as_bytes());
        h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        b64(&h.finalize())
    };
    if out
        .write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )
        .is_err()
    {
        return;
    }
    crate::log::info(&format!("extbus: ws connected ({origin})"));

    // ---- frames --------------------------------------------------------
    while let Some((opcode, payload)) = ws_read_frame(&mut reader) {
        match opcode {
            0x9 => {
                // Ping: the extension's keep-alive heartbeat.
                if ws_write_frame(&mut out, 0xA, &payload).is_err() {
                    break;
                }
            }
            0x8 => {
                let _ = ws_write_frame(&mut out, 0x8, &[]);
                break;
            }
            0x1 => {
                let reply = match std::str::from_utf8(&payload)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                {
                    Some(req) => dispatch(&req, false),
                    None => serde_json::json!({"ok": false, "error": "bad json"}),
                };
                if ws_write_frame(&mut out, 0x1, reply.to_string().as_bytes()).is_err() {
                    break;
                }
            }
            // Binary/continuation: nothing legitimate sends these here.
            _ => break,
        }
    }
}

/// One frame; None on error/EOF. Client frames must be masked (RFC 6455).
fn ws_read_frame(reader: &mut BufReader<TcpStream>) -> Option<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    reader.read_exact(&mut hdr).ok()?;
    let opcode = hdr[0] & 0x0F;
    let masked = hdr[1] & 0x80 != 0;
    let mut len = (hdr[1] & 0x7F) as u64;
    if len == 126 {
        let mut b = [0u8; 2];
        reader.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as u64;
    } else if len == 127 {
        let mut b = [0u8; 8];
        reader.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b);
    }
    if !masked || len > 4 * 1024 * 1024 {
        return None;
    }
    let mut mask = [0u8; 4];
    reader.read_exact(&mut mask).ok()?;
    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).ok()?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Some((opcode, payload))
}

fn ws_write_frame(out: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => frame.push(n as u8),
        n if n <= u16::MAX as usize => {
            frame.push(126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            frame.push(127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    out.write_all(&frame)?;
    out.flush()
}

/// Standard base64, only needed for the handshake accept key — not worth a
/// crate dependency.
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

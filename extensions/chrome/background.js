// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hydra browser integration, service worker.
//
// Architecture:
//  - Primary transport: a persistent WebSocket to the app's fixed loopback
//    port (hydra: 6799/16799), reconnecting with backoff forever. The open
//    socket doubles as the "app is running" indicator (toolbar shows a gray
//    X when it is down).
//  - Fallback transport: native messaging through `hydra-host`, which can
//    LAUNCH the app when it is not running, after which the WebSocket
//    takes over again.
//  - Capture: pause the browser download the moment it is created (pausing
//    is reversible where cancelling is not), decide when the filename is
//    known, then either hand it to Hydra and erase, or resume the browser
//    download untouched.

// One source of truth for Chrome/Edge/Brave AND Safari. Safari exposes the
// promise-based `browser` namespace (and `chrome` only since 16.4), so bind
// whichever exists and feature-detect the rest: Safari has no `downloads`
// API at all, which is why auto-capture is Chromium-only. The WebSocket
// transport below is identical everywhere.
globalThis.chrome ??= globalThis.browser;

const HOST = "com.hydra.host";
const WS_PORTS = [6799, 16799];

/// Which browser this copy is running in, spelled the way Hydra's
/// Options > General list spells it. Every request carries it so the app can
/// answer with THAT browser's capture checkbox — without it, one flag stood
/// for all of them and the Firefox/Safari rows did nothing.
///
/// Chromium forks that do not identify themselves (Brave, Vivaldi, Arc) look
/// like Chrome to the UA and are governed by the Chrome row, which is also
/// the row their extension install is closest to.
const BROWSER = (() => {
  const ua = navigator.userAgent || "";
  if (/\bEdg[A-Z]?\//.test(ua)) return "Microsoft Edge";
  if (/\bOPR\/|\bOpera\//.test(ua)) return "Opera";
  if (/\bFirefox\/|\bGecko\//.test(ua) && !/\bChrome\//.test(ua)) return "Mozilla Firefox";
  if (/\bChrome\/|\bChromium\//.test(ua)) return "Google Chrome";
  if (/\bSafari\//.test(ua)) return "Apple Safari";
  return "Google Chrome";
})();

// Mirrors hydra-gui's Options > File Types default; overwritten by the GUI's
// list on every round-trip.
const DEFAULT_TYPES =
  "3GP 7Z AAC ACE AIF APK ARJ ASF AVI BIN BZ2 DMG EXE GZ GZIP IMG ISO LZH " +
  "M4A M4V MKV MOV MP3 MP4 MPA MPE MPEG MPG MSI MSU OGG OGV PDF PKG PPS PPT " +
  "QT RA RAR RM RMVB SEA SIT SITX TAR TIF TIFF WAV WMA WMV Z ZIP";
const DEFAULT_SKIP = "*.update.microsoft.com download.windowsupdate.com";

// Content-type → canonical extension. Used when the filename itself is
// inconclusive.
const MIME_EXT = {
  "application/zip": "ZIP", "application/x-zip": "ZIP", "application/x-zip-compressed": "ZIP",
  "application/gzip": "GZ", "application/x-gzip": "GZ", "application/x-gzip-compressed": "GZ",
  "application/x-7z-compressed": "7Z", "application/x-compress-7z": "7Z",
  "application/x-rar": "RAR", "application/x-rar-compressed": "RAR",
  "application/x-tar": "TAR", "application/x-gtar": "TAR",
  "application/x-bzip2": "BZ2", "application/x-compress": "Z",
  "application/pdf": "PDF", "application/x-msi": "MSI",
  "application/x-dosexec": "EXE", "application/x-msdos-program": "EXE",
  "application/x-apple-diskimage": "DMG",
  "application/vnd.android.package-archive": "APK",
  "application/epub+zip": "EPUB",
  "video/mp4": "MP4", "video/x-mp4": "MP4", "video/mpg4": "MP4",
  "video/webm": "WEBM", "video/x-matroska": "MKV", "video/quicktime": "MOV",
  "video/x-msvideo": "AVI", "video/msvideo": "AVI", "video/avi": "AVI",
  "video/x-flv": "FLV", "video/x-flash-video": "FLV",
  "video/mpeg": "MPG", "video/x-ms-wmv": "WMV", "video/x-ms-asf": "ASF",
  "video/3gpp": "3GP", "video/3gpp2": "3GP",
  // Was in the old gate regex but never in this table, so it was sniffed and
  // then labelled `null` — the exact drift deriving the gate from here ends.
  "video/ogg": "OGV", "video/x-theora": "OGV",
  "audio/mpeg": "MP3", "audio/mp3": "MP3", "audio/x-mpeg": "MP3",
  "audio/mp4": "M4A", "audio/mp4a-latm": "M4A", "audio/x-m4a": "M4A",
  "audio/m4a": "M4A", "audio/aac": "AAC", "audio/aacp": "AAC",
  "audio/wav": "WAV", "audio/x-wav": "WAV", "audio/wave": "WAV",
  "audio/vnd.wave": "WAV", "audio/x-pn-wav": "WAV",
  "audio/x-ms-wma": "WMA",
  "audio/ogg": "OGG", "application/ogg": "OGG", "audio/vorbis": "OGG",
  "audio/opus": "OPUS", "audio/x-opus+ogg": "OPUS",
  "audio/webm": "WEBM", "audio/flac": "FLAC", "audio/x-flac": "FLAC",
  "audio/aiff": "AIFF", "audio/x-aiff": "AIFF",
  "audio/basic": "AU", "audio/midi": "MIDI", "audio/x-midi": "MIDI",
  "audio/3gpp": "3GP", "audio/3gpp2": "3GP", "audio/amr": "AMR",
};

// Direct media worth listing in the popup sniffer. Segmented HLS/DASH is
// handled separately, further down, as a stream rather than a file.
//
// DERIVED from MIME_EXT rather than restating it. The two were maintained by
// hand and had already drifted — `video/ogg` was in the gate with no entry in
// the table — and either half of a drift is a silent bug: a type in the table
// but not the gate is never sniffed, one in the gate but not the table is
// sniffed and then labelled `null`.
//
// An exact-match Set, not a prefix regex: the caller has already split the
// parameters off the Content-Type, so there is nothing to prefix-match, and a
// Set cannot accidentally match `audio/mpegurl` (an HLS playlist) the way
// `/^audio\/mpeg/` did.
const MEDIA_MIME_SET = new Set(
  Object.keys(MIME_EXT).filter((m) => /^(?:audio|video)\//.test(m) || m === "application/ogg")
);
const isMediaMime = (mime) => MEDIA_MIME_SET.has((mime || "").toLowerCase());

// A file server that does not know the type says `application/octet-stream`,
// or says nothing at all. That is not a reason to ignore a song: for those
// two answers ONLY, the extension decides. Anything with a real, non-media
// Content-Type is still rejected on the type, so a .zip cannot sneak in by
// being named .mp3.
const VAGUE_MIME = /^(?:application\/(?:octet-stream|binary)|binary\/octet-stream)?$/i;
const MEDIA_PATH =
  /\.(?:mp3|m4a|m4b|aac|wav|wave|ogg|oga|opus|flac|aif|aiff|wma|amr|au|mid|midi|mp4|m4v|webm|mkv|mov|avi|flv|mpg|mpeg|wmv|3gp|3g2)(?:$|[?#])/i;
const MEDIA_MIN_BYTES = 256 * 1024;
// Audio carries an order of magnitude fewer bytes than video per second, so
// the video floor silently hid whole songs, podcast chapters and voice
// notes. Still high enough to keep UI blips and notification sounds out.
const AUDIO_MIN_BYTES = 32 * 1024;
const MEDIA_PER_TAB = 25;

// ---------------------------------------------------------------- settings

async function getState() {
  return chrome.storage.local.get({
    enabled: true, // extension-side master switch (popup toggle)
    guiCapture: true, // "Google Chrome" checkbox in hydra's Options
    autoTypes: DEFAULT_TYPES,
    skipSites: DEFAULT_SKIP,
    videoPanel: true, // the floating Download button over <video> elements
    hydraSeen: false, // ever completed a round-trip
  });
}

function absorbReply(reply) {
  if (!reply || typeof reply !== "object" || reply.unreachable) return;
  const patch = { hydraSeen: true };
  if (typeof reply.capture === "boolean") patch.guiCapture = reply.capture;
  if (typeof reply.auto_types === "string" && reply.auto_types) patch.autoTypes = reply.auto_types;
  if (typeof reply.dont_start_sites === "string") patch.skipSites = reply.dont_start_sites;
  chrome.storage.local.set(patch);
}

// --------------------------------------------------------------- transport

let ws = null;
let wsReady = false;
let wsPortIdx = 0;
let reconnectTimer = null;
let heartbeatTimer = null;
let reqSeq = 1;
const pendingReqs = new Map(); // id -> {resolve, timer}

function setConnected(on) {
  // Gray X on the toolbar icon while the app is down.
  chrome.action.setBadgeBackgroundColor({ color: "#606060" }).catch(() => {});
  chrome.action.setBadgeText({ text: on ? "" : "X" }).catch(() => {});
  chrome.action
    .setTitle({ title: on ? "Hydra Download Manager" : "Hydra is not running" })
    .catch(() => {});
}

function wsTeardown() {
  wsReady = false;
  if (heartbeatTimer) {
    clearInterval(heartbeatTimer);
    heartbeatTimer = null;
  }
  for (const [, p] of pendingReqs) {
    clearTimeout(p.timer);
    p.resolve(null);
  }
  pendingReqs.clear();
  if (ws) {
    try {
      ws.close();
    } catch {}
    ws = null;
  }
}

function wsScheduleReconnect(delayMs = 4000) {
  if (reconnectTimer) return;
  wsPortIdx++; // next attempt tries the fallback port
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    wsConnect();
  }, delayMs);
}

function wsConnect() {
  if (ws) return;
  try {
    ws = new WebSocket(`ws://127.0.0.1:${WS_PORTS[wsPortIdx % WS_PORTS.length]}/`);
  } catch {
    ws = null;
    wsScheduleReconnect();
    return;
  }
  ws.onopen = () => {
    wsReady = true;
    setConnected(true);
    // Heartbeat: keeps the service worker alive (Chrome extends SW life on
    // WS traffic) and notices a dead app quickly.
    heartbeatTimer = setInterval(() => wsRequest({ type: "ping" }, 5000), 20000);
    wsRequest({ type: "config" }, 5000); // sync capture settings on connect
  };
  ws.onmessage = (ev) => {
    let reply;
    try {
      reply = JSON.parse(ev.data);
    } catch {
      return;
    }
    absorbReply(reply);
    const p = pendingReqs.get(reply.id);
    if (p) {
      clearTimeout(p.timer);
      pendingReqs.delete(reply.id);
      p.resolve(reply);
    }
  };
  ws.onclose = ws.onerror = () => {
    wsTeardown();
    setConnected(false);
    wsScheduleReconnect();
  };
}

/// Returns the reply, or null when the socket is down / timed out — the
/// caller then falls back to the native host.
function wsRequest(msg, timeoutMs = 8000) {
  return new Promise((resolve) => {
    if (!ws || !wsReady) return resolve(null);
    const id = reqSeq++;
    const timer = setTimeout(() => {
      pendingReqs.delete(id);
      resolve(null);
    }, timeoutMs);
    pendingReqs.set(id, { resolve, timer });
    try {
      ws.send(JSON.stringify({ browser: BROWSER, ...msg, id }));
    } catch {
      clearTimeout(timer);
      pendingReqs.delete(id);
      resolve(null);
    }
  });
}

/// Native messaging, both dialects: Chrome takes a callback and reports
/// failures via lastError; Safari ignores the host name (the message goes
/// to the wrapper app's SFSafariWebExtensionHandler) and returns a promise.
function native(msg) {
  return new Promise((resolve) => {
    const done = (reply) => {
      absorbReply(reply);
      resolve(reply || { ok: false, error: "no reply", unreachable: true });
    };
    const failed = (e) => resolve({ ok: false, error: String(e), unreachable: true });
    try {
      const ret = chrome.runtime.sendNativeMessage(HOST, { browser: BROWSER, ...msg }, (reply) => {
        if (chrome.runtime.lastError) failed(chrome.runtime.lastError.message);
        else done(reply);
      });
      if (ret && typeof ret.then === "function") ret.then(done, failed);
    } catch (e) {
      failed(e);
    }
  });
}

/// One request, best transport: WebSocket when live; otherwise the native
/// host (which launches the app), after which the WebSocket reattaches.
async function request(msg) {
  const viaWs = await wsRequest(msg);
  if (viaWs) return viaWs;
  const viaHost = await native(msg);
  if (viaHost && viaHost.ok) {
    setConnected(true);
    if (!reconnectTimer) wsScheduleReconnect(700); // app is up: reattach fast
  }
  return viaHost;
}

// ----------------------------------------------------------------- filters

function extOf(nameOrPath) {
  const base = String(nameOrPath || "").split(/[/\\]/).pop().split(/[?#]/)[0];
  const dot = base.lastIndexOf(".");
  return dot > 0 ? base.slice(dot + 1).toUpperCase() : "";
}

function typeMatches(autoTypes, filename, url, mime) {
  const set = new Set(autoTypes.toUpperCase().split(/[\s,]+/).filter(Boolean));
  const fromName = extOf(filename);
  if (fromName && set.has(fromName)) return true;
  const mapped = MIME_EXT[(mime || "").split(";")[0].trim().toLowerCase()];
  if (mapped && set.has(mapped)) return true;
  try {
    const fromUrl = extOf(new URL(url).pathname);
    return !!fromUrl && set.has(fromUrl);
  } catch {
    return false;
  }
}

// Site patterns support leading-* wildcard ("*.update.microsoft.com").
function siteSkipped(skipSites, url) {
  let host;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  return skipSites
    .toLowerCase()
    .split(/[\s,]+/)
    .filter(Boolean)
    .some((pat) => {
      if (pat.startsWith("*.")) {
        const dom = pat.slice(2);
        return host === dom || host.endsWith("." + dom);
      }
      return host === pat || host.endsWith("." + pat);
    });
}

// `storage.session` is MV3/Safari 16.4+; fall back to local where absent so
// the media list and the Alt stamp still work (they are short-lived keys).
const sessionStore = () => chrome.storage.session ?? chrome.storage.local;

async function altBypassed() {
  const { altTs = 0 } = await sessionStore().get("altTs");
  return Date.now() - altTs < 3000;
}

// ----------------------------------------------------------------- cookies

async function cookieHeader(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (!cookies.length) return null;
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------- capture

async function sendToHydra(url, extras = {}) {
  return request({
    type: "download",
    url,
    cookies: await cookieHeader(url),
    user_agent: navigator.userAgent,
    ...extras,
  });
}

function captureEligible(item, state) {
  const url = item.finalUrl || item.url;
  return (
    state.enabled &&
    state.guiCapture &&
    /^(https?|ftp):/i.test(url) &&
    item.byExtensionId !== chrome.runtime.id
  );
}

// Freeze the download the moment it exists (downloads.pause in onCreated).
// Pausing is reversible where cancelling is not: small files
// cannot slip through by finishing early, and signed one-shot URLs keep
// working because we never have to re-request them.
async function parkDownload(item) {
  const state = await getState();
  if (!captureEligible(item, state)) return;
  try {
    await chrome.downloads.pause(item.id);
  } catch {
    // Finished already or not pausable; the decision step handles it.
  }
}

async function decideCapture(item) {
  const state = await getState();
  const url = item.finalUrl || item.url;

  const giveBack = async () => {
    try {
      await chrome.downloads.resume(item.id);
    } catch {}
  };

  if (!captureEligible(item, state)) return giveBack();
  if (siteSkipped(state.skipSites, url)) return giveBack();
  if (await altBypassed()) return giveBack();
  if (!typeMatches(state.autoTypes, item.filename, url, item.mime)) return giveBack();

  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : null;
  const reply = await sendToHydra(url, {
    filename: item.filename ? item.filename.split(/[/\\]/).pop() : null,
    referer: item.referrer || null,
    size,
    mime: item.mime || null,
  });

  if (reply && reply.ok) {
    // Hydra owns it now; drop the browser's paused copy.
    try {
      await chrome.downloads.cancel(item.id);
    } catch {}
    try {
      await chrome.downloads.erase({ id: item.id });
    } catch {}
  } else {
    // Hydra unreachable: the browser download continues untouched.
    await giveBack();
  }
}

// Looked up by name rather than written as a property access: the event is
// Chromium-only, and a literal `chrome.downloads.onDeterminingFilename`
// makes Firefox's add-on linter report an unsupported API even though this
// branch never runs there.
const onDeterminingFilename = chrome.downloads?.["onDeterminingFilename"];

if (onDeterminingFilename) {
  // Chromium: park on creation, decide once onDeterminingFilename resolves
  // the real name (Content-Disposition applied).
  chrome.downloads.onCreated.addListener((item) => parkDownload(item));
  onDeterminingFilename.addListener((item, suggest) => {
    suggest();
    decideCapture(item);
  });
} else if (chrome.downloads?.onCreated) {
  // Firefox: no onDeterminingFilename, but `filename` is already resolved on
  // the create event. Park and decide in ONE sequential listener — two
  // listeners on the same event run concurrently, and the decision would
  // race the pause it depends on.
  chrome.downloads.onCreated.addListener(async (item) => {
    await parkDownload(item);
    await decideCapture(item);
  });
}
// Safari has no downloads API at all; there the extension is menus +
// selection pill + sniffing only.

// ------------------------------------------------------------ context menus

// Promise form (not the callback): it is the only one both Chrome MV3 and
// Safari's promise-based namespace implement.
async function installMenus() {
  const menus = chrome.contextMenus ?? chrome.menus;
  if (!menus) return;
  try {
    await menus.removeAll();
  } catch {}
  for (const [id, title, contexts] of [
    ["hydra-link", "Download with Hydra", ["link"]],
    ["hydra-media", "Download with Hydra", ["image", "video", "audio"]],
    ["hydra-all-links", "Download all links with Hydra", ["page"]],
  ]) {
    try {
      menus.create({ id, title, contexts });
    } catch {}
  }
}

chrome.runtime.onInstalled.addListener((details) => {
  installMenus();
  wsConnect();
  if (details?.reason === "install" && chrome.tabs?.create) {
    chrome.tabs.create({ url: chrome.runtime.getURL("welcome.html") });
  }
});
chrome.runtime.onStartup.addListener(() => {
  installMenus();
  wsConnect();
});
// Any service-worker wake-up re-establishes the socket.
wsConnect();

(chrome.contextMenus ?? chrome.menus)?.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === "hydra-link" || info.menuItemId === "hydra-media") {
    const url = info.menuItemId === "hydra-link" ? info.linkUrl : info.srcUrl;
    if (url) await sendToHydra(url, { referer: tab?.url || null, tab_url: tab?.url || null });
  } else if (info.menuItemId === "hydra-all-links" && tab?.id != null) {
    try {
      const resp = await chrome.tabs.sendMessage(tab.id, { type: "collect-links" });
      const urls = [...new Set((resp?.urls || []).filter((u) => /^https?:/i.test(u)))];
      if (urls.length) await request({ type: "links", urls });
    } catch {
      // No content script on this page (chrome://, PDF viewer, ...).
    }
  }
});

// ------------------------------------------------------------ stream sniffer
//
// Adaptive streams arrive as a MANIFEST plus hundreds of short segments.
// What belongs in the popup is the manifest — one logical stream — so the
// segments are filtered out of the direct-media list instead of crowding it
// out, and a master playlist swallows the variant playlists it points at.
//
// Everything here is read-only reconnaissance: manifests are parsed for
// their variant table, and a stream whose manifest declares a DRM system is
// listed as unsupported rather than handed to Hydra.

const HLS_MIME =
  /^(?:application\/(?:vnd\.apple\.mpegurl|x-mpegurl|mpegurl|octet-stream-m3u8)|(?:audio|video)\/(?:x-)?mpegurl)$/i;
const DASH_MIME = /^(?:application\/dash\+xml|video\/vnd\.mpeg\.dash\.mpd)$/i;
const HLS_PATH = /\.m3u8(?:$|[?#])/i;
const DASH_PATH = /\.mpd(?:$|[?#])/i;

// Segment shapes, kept out of the direct-media list. fMP4 segments are
// served as video/mp4 and are big enough to clear MEDIA_MIN_BYTES, so the
// URL is what distinguishes them from a real file.
// NOT `.m4a` or `.m4v`: those are ordinary Apple container extensions for a
// finished audio or video file, and treating them as segment shapes hid every
// one of them from the sniffer. Real fMP4 segments are `.m4s`/`.cmfa`/`.cmfv`,
// and anything under a detected manifest's directory is excluded separately
// by the stream-directory check, which is the reliable filter.
const SEGMENT_PATH =
  /\.(?:ts|m4s|cmfv|cmfa|cmft|fmp4|mp4f|dash|init|key)(?:$|[?#])/i;
const SEGMENT_MIME = /^(?:video|audio)\/mp2t$|^application\/x-mpegts$/i;

const STREAMS_PER_TAB = 12;
const MANIFEST_MAX_BYTES = 4 * 1024 * 1024;
const MANIFEST_TIMEOUT_MS = 8000;

/// Name the DRM system behind an HLS KEYFORMAT or a DASH schemeIdUri.
/// Recognising one is how a stream gets marked unsupported — nothing here
/// touches keys or licences.
function drmName(id) {
  const s = String(id || "").toLowerCase();
  if (s.includes("edef8ba9-79d6-4ace-a3c8-27dcd51d21ed") || s.includes("widevine"))
    return "Widevine";
  if (s.includes("9a04f079-9840-4286-ab92-e65be0885f95") || s.includes("playready"))
    return "PlayReady";
  if (
    s.includes("94ce86fb-07ff-4f43-adb8-93d2fa968ca2") ||
    s.includes("fairplay") ||
    s.includes("com.apple.streamingkeydelivery")
  )
    return "FairPlay";
  if (s.includes("e2719d58-a985-b3c9-781a-b030af78d30e") || s.includes("clearkey"))
    return "ClearKey";
  return null;
}

function absUrl(u, base) {
  try {
    return new URL(u, base).href;
  } catch {
    return u;
  }
}

/// Manifests are re-requested with a rotating auth token in the query, so
/// identity is origin+path — the same key the direct-media sniffer uses.
function streamKey(url) {
  try {
    const u = new URL(url);
    return u.origin + u.pathname;
  } catch {
    return String(url).split("?")[0];
  }
}

/// A tab title made into a leaf filename. The in-page panel already sends
/// one; the popup has no page context of its own, so the same name is
/// derived here rather than letting the two paths disagree.
function titleName(title) {
  return (
    String(title || "")
      .replace(/[\\/:*?"<>|]/g, " ")
      .replace(/\s+/g, " ")
      .trim()
      .replace(/^[.\s]+|[.\s]+$/g, "")
      .slice(0, 120) || ""
  );
}

/// Stable identity for one variant, so the popup can name the row's choice
/// back to the service worker. Duplicated verbatim in popup.js — the two run
/// in different contexts and must agree.
function streamVariantId(v) {
  return [v?.id ?? "", v?.url ?? "", v?.bandwidth ?? ""].join("|");
}

/// `KEY=value` / `KEY="quoted,value"` attribute lists, RFC 8216 §4.2.
function hlsAttrs(line) {
  const out = {};
  for (const m of line.matchAll(/([A-Za-z0-9-]+)=("[^"]*"|[^,]*)/g)) {
    out[m[1].toUpperCase()] = m[2].replace(/^"|"$/g, "");
  }
  return out;
}

function tagValue(line) {
  const i = line.indexOf(":");
  return i < 0 ? "" : line.slice(i + 1);
}

/// Parse a playlist. A master lists variants; a media playlist lists
/// segments, and only it knows the duration and whether the stream is live.
function parseHls(text, baseUrl) {
  const info = {
    protocol: "hls",
    kind: "media",
    live: false,
    duration: null,
    segments: 0,
    drm: null,
    encryption: null,
    // "ts" (MPEG-TS segments, concatenable and remuxable) or "fmp4"
    // (fragmented MP4 with an init map). It decides which output containers
    // the in-page panel may offer.
    segmentType: null,
    variants: [],
    audioTracks: 0,
    children: [],
  };
  let ended = false;
  let vod = false;
  let pending = null;
  let wantSegment = false;
  let duration = 0;

  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;

    if (line.startsWith("#EXT-X-STREAM-INF:")) {
      info.kind = "master";
      pending = hlsAttrs(tagValue(line));
    } else if (line.startsWith("#EXT-X-MEDIA:")) {
      const a = hlsAttrs(tagValue(line));
      if (a.URI) info.children.push(absUrl(a.URI, baseUrl));
      if ((a.TYPE || "").toUpperCase() === "AUDIO") info.audioTracks++;
    } else if (line.startsWith("#EXT-X-KEY:") || line.startsWith("#EXT-X-SESSION-KEY:")) {
      const a = hlsAttrs(tagValue(line));
      const method = (a.METHOD || "").toUpperCase();
      if (method === "NONE") continue;
      const named = drmName(a.KEYFORMAT || "");
      if (named) info.drm ||= named;
      else if (method.startsWith("SAMPLE-AES")) info.drm ||= "SAMPLE-AES";
      else if (method === "AES-128") info.encryption ||= "AES-128";
    } else if (line.startsWith("#EXT-X-MAP:")) {
      info.segmentType = "fmp4"; // an init map means fragmented MP4
    } else if (line.startsWith("#EXTINF:")) {
      info.segments++;
      duration += parseFloat(tagValue(line)) || 0;
      wantSegment = true;
    } else if (line === "#EXT-X-ENDLIST") {
      ended = true;
    } else if (line.startsWith("#EXT-X-PLAYLIST-TYPE:")) {
      vod = /VOD/i.test(tagValue(line));
    } else if (!line.startsWith("#") && wantSegment) {
      // The URI after an EXTINF is a segment; its extension names the
      // container when no EXT-X-MAP said so.
      wantSegment = false;
      if (!info.segmentType) {
        info.segmentType = /\.ts(?:$|[?#])/i.test(line) ? "ts" : "fmp4";
      }
    } else if (!line.startsWith("#") && pending) {
      // The URI line that follows an EXT-X-STREAM-INF is that variant.
      const res = /^(\d+)x(\d+)$/.exec(pending.RESOLUTION || "");
      const url = absUrl(line, baseUrl);
      info.variants.push({
        url,
        id: pending["NAME"] || null,
        bandwidth:
          parseInt(pending["AVERAGE-BANDWIDTH"] || pending.BANDWIDTH || "0", 10) || null,
        width: res ? +res[1] : null,
        height: res ? +res[2] : null,
        codecs: pending.CODECS || null,
        frameRate: parseFloat(pending["FRAME-RATE"]) || null,
      });
      info.children.push(url);
      pending = null;
    }
  }

  if (info.kind === "media") {
    info.duration = duration || null;
    // No ENDLIST and no VOD marker is the only signal a playlist gives that
    // it is still growing.
    info.live = !ended && !vod;
  }
  info.variants.sort((a, b) => (b.bandwidth || 0) - (a.bandwidth || 0));
  return info;
}

/// ISO 8601 duration ("PT1H2M3.5S") in seconds. Years/months are not
/// meaningful for a presentation and are ignored.
function isoDuration(s) {
  const m =
    /^P(?:[\d.]+Y)?(?:[\d.]+M)?(?:([\d.]+)D)?(?:T(?:([\d.]+)H)?(?:([\d.]+)M)?(?:([\d.]+)S)?)?$/.exec(
      String(s).trim()
    );
  if (!m) return null;
  const n = (x) => parseFloat(x || "0") || 0;
  return n(m[1]) * 86400 + n(m[2]) * 3600 + n(m[3]) * 60 + n(m[4]) || null;
}

/// Parse an MPD with regexes rather than DOMParser: a Chromium MV3 service
/// worker has no DOM, and the handful of attributes needed here do not
/// justify shipping an XML parser.
function parseDash(text, baseUrl) {
  const info = {
    protocol: "dash",
    kind: "mpd",
    live: /\btype\s*=\s*"dynamic"/i.test(text),
    segmentType: "fmp4", // DASH is fragmented MP4 (or WebM, also unmuxed)
    duration: isoDuration(/mediaPresentationDuration\s*=\s*"([^"]+)"/i.exec(text)?.[1] || ""),
    segments: 0,
    drm: null,
    encryption: null,
    variants: [],
    audioTracks: 0,
    children: [],
  };

  let cencOnly = false;
  for (const m of text.matchAll(/<ContentProtection\b([^>]*)>/gi)) {
    const scheme = /schemeIdUri\s*=\s*"([^"]+)"/i.exec(m[1])?.[1] || "";
    const named = drmName(scheme);
    if (named) info.drm ||= named;
    else if (/mp4protection/i.test(scheme)) cencOnly = true;
  }
  // Common encryption with no recognised system is still encrypted, and the
  // key would have to come from a licence server either way.
  if (!info.drm && cencOnly) info.drm = "CENC";

  for (const block of text.split(/<AdaptationSet\b/i).slice(1)) {
    const head = block.slice(0, block.indexOf(">") + 1);
    const body = block.split(/<\/AdaptationSet>/i)[0];
    const setType = (
      /contentType\s*=\s*"([^"]+)"/i.exec(head)?.[1] ||
      /mimeType\s*=\s*"([^"/]+)\//i.exec(head)?.[1] ||
      ""
    ).toLowerCase();

    for (const r of body.matchAll(/<Representation\b([^>]*?)\/?>/gi)) {
      const at = r[1];
      const attr = (n) => new RegExp(`\\b${n}\\s*=\\s*"([^"]*)"`, "i").exec(at)?.[1] || null;
      const type = setType || (attr("mimeType") || "").split("/")[0].toLowerCase();
      if (type === "audio") {
        info.audioTracks++;
        continue;
      }
      if (type && type !== "video") continue; // subtitles, thumbnails
      info.variants.push({
        url: baseUrl,
        id: attr("id"),
        bandwidth: parseInt(attr("bandwidth") || "0", 10) || null,
        width: parseInt(attr("width") || "0", 10) || null,
        height: parseInt(attr("height") || "0", 10) || null,
        codecs: attr("codecs"),
        frameRate: parseFloat(attr("frameRate")) || null,
      });
    }
  }
  info.variants.sort((a, b) => (b.bandwidth || 0) - (a.bandwidth || 0));
  return info;
}

function classifyManifest(text) {
  if (/^﻿?\s*#EXTM3U/.test(text)) return "hls";
  if (/<MPD[\s>]/i.test(text)) return "dash";
  return null;
}

/// Fetch a manifest with the browser session's cookies. Bounded in time and
/// size: a manifest is kilobytes, and anything claiming otherwise is not one.
async function fetchManifest(url) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), MANIFEST_TIMEOUT_MS);
  try {
    const r = await fetch(url, { credentials: "include", cache: "no-store", signal: ctl.signal });
    if (!r.ok) return null;
    if (parseInt(r.headers.get("content-length") || "0", 10) > MANIFEST_MAX_BYTES) return null;
    const text = await r.text();
    return text.length > MANIFEST_MAX_BYTES ? null : text;
  } catch {
    return null; // CORS-blocked, offline, aborted: the entry stays thin.
  } finally {
    clearTimeout(timer);
  }
}

// Per-tab serialization. The sniffer is a read-modify-write over session
// storage and webRequest fires concurrently; without this, two responses
// arriving together lose one of the entries.
const tabLocks = new Map();
function withTab(tabId, fn) {
  const next = (tabLocks.get(tabId) || Promise.resolve()).then(fn, fn);
  tabLocks.set(
    tabId,
    next.then(
      () => {},
      () => {}
    )
  );
  return next;
}

async function tabStreams(tabId) {
  const key = `streams_${tabId}`;
  const got = await sessionStore().get(key);
  return got[key] || [];
}

/// Keys of playlists a listed master already covers, so a variant seen
/// afterwards is not listed as a stream of its own.
async function tabStreamKids(tabId) {
  const key = `streamkids_${tabId}`;
  const got = await sessionStore().get(key);
  return got[key] || [];
}

/// One HLS master, enriched with the duration and encryption its variants
/// know about and it does not. The cheapest variant is fetched because the
/// timeline is identical across them.
async function enrichHlsMaster(info, pageUrl) {
  const cheapest = info.variants[info.variants.length - 1];
  if (!cheapest?.url) return info;
  const text = await fetchManifest(cheapest.url);
  if (!text) return info;
  const media = parseHls(text, cheapest.url);
  info.duration = media.duration;
  info.segments = media.segments;
  info.segmentType = media.segmentType;
  info.live = media.live;
  info.drm ||= media.drm;
  info.encryption ||= media.encryption;
  return info;
}

/// Record a manifest against its tab, replacing any variant entries the
/// manifest turns out to own.
async function noteStream(tabId, url, mime, tabUrl) {
  const key = streamKey(url);
  const list = await tabStreams(tabId);
  const known = list.find((s) => s.key === key);
  if (known) {
    // A live player re-reads its playlist every few seconds, so this path is
    // hit constantly. Only write when the URL actually changed — otherwise a
    // single live tab rewrites session storage at the playlist's refresh
    // rate for as long as it is open.
    if (known.url !== url) {
      known.url = url; // a rotated auth token
      await sessionStore().set({ [`streams_${tabId}`]: list });
    }
    return;
  }
  if ((await tabStreamKids(tabId)).includes(key)) return; // a listed master owns it

  const text = await fetchManifest(url);
  const protocol =
    (text && classifyManifest(text)) ||
    (HLS_MIME.test(mime) || HLS_PATH.test(url) ? "hls" : DASH_PATH.test(url) ? "dash" : null);
  if (!protocol) return;

  let info;
  if (!text) {
    // Unreadable manifest: still worth listing, Hydra will fetch it itself.
    info = {
      protocol,
      kind: protocol === "hls" ? "media" : "mpd",
      live: false,
      duration: null,
      segments: 0,
      drm: null,
      encryption: null,
      variants: [],
      audioTracks: 0,
      children: [],
    };
  } else if (protocol === "hls") {
    info = parseHls(text, url);
    if (info.kind === "master" && info.variants.length) info = await enrichHlsMaster(info, tabUrl);
  } else {
    info = parseDash(text, url);
  }

  const entry = {
    key,
    url,
    mime: mime || null,
    protocol: info.protocol,
    kind: info.kind,
    live: info.live,
    duration: info.duration,
    segments: info.segments,
    drm: info.drm,
    encryption: info.encryption,
    segmentType: info.segmentType,
    audioTracks: info.audioTracks,
    variants: info.variants,
    pageUrl: tabUrl || null,
  };

  const kidKeys = info.children.map(streamKey);
  // A variant may have been listed before its master showed up; the master
  // is the better entry, so the variants collapse into it.
  const kept = list.filter((s) => !kidKeys.includes(s.key));
  kept.push(entry);
  while (kept.length > STREAMS_PER_TAB) kept.shift();

  const kids = [...new Set([...(await tabStreamKids(tabId)), ...kidKeys])];
  await sessionStore().set({
    [`streams_${tabId}`]: kept,
    [`streamkids_${tabId}`]: kids.slice(-STREAMS_PER_TAB * 20),
  });
}

/// Ask Hydra for a stream download. `stream` is a distinct request type:
/// a manifest is not a file, and handing it to the ordinary download path
/// would save a few kilobytes of playlist text.
async function sendStreamToHydra(entry, variant, opts = {}) {
  // Defence in depth: the panel lists a protected stream as an unclickable
  // note, so this should be unreachable from the UI.
  if (entry.drm) {
    return { ok: false, error: `${entry.drm} DRM: this stream is protected and cannot be downloaded` };
  }
  const target = variant?.url || entry.url;
  const reply = await request({
    type: "stream",
    url: entry.url,
    protocol: entry.protocol,
    variant_url: target !== entry.url ? target : null,
    variant: variant
      ? {
          id: variant.id,
          url: variant.url,
          width: variant.width,
          height: variant.height,
          bandwidth: variant.bandwidth,
          codecs: variant.codecs,
          frame_rate: variant.frameRate,
        }
      : null,
    variants: entry.variants,
    container: opts.container || (entry.segmentType === "ts" ? "TS" : "MP4"),
    filename: opts.filename || null,
    segment_type: entry.segmentType,
    live: entry.live,
    duration: entry.duration,
    encryption: entry.encryption,
    audio_tracks: entry.audioTracks,
    // Only a finite presentation has a size to estimate; a live playlist's
    // duration is its sliding window.
    size:
      !entry.live && variant?.bandwidth && entry.duration
        ? Math.round((variant.bandwidth * entry.duration) / 8)
        : null,
    cookies: await cookieHeader(entry.url),
    user_agent: navigator.userAgent,
    referer: entry.pageUrl || null,
    tab_url: entry.pageUrl || null,
    mime: entry.mime || null,
  });
  // Builds without a stream-aware path answer "unknown type"; say so rather
  // than quietly downloading the playlist as a text file.
  if (reply && !reply.ok && /unknown type/i.test(reply.error || "")) {
    return { ok: false, error: "this Hydra version cannot download streams — update Hydra" };
  }
  return reply;
}

// ------------------------------------------------------------ media sniffer

async function tabMedia(tabId) {
  const key = `media_${tabId}`;
  const got = await sessionStore().get(key);
  return got[key] || [];
}

/// One badge for everything grabbable on the tab: direct files and streams.
async function refreshBadge(tabId) {
  const n = (await tabMedia(tabId)).length + (await tabStreams(tabId)).length;
  try {
    await chrome.action.setBadgeBackgroundColor({ color: "#2c6e31", tabId });
    await chrome.action.setBadgeText({ tabId, text: n ? String(n) : "" });
  } catch {
    // Tab may already be gone.
  }
  // Nudge the in-page panel: a manifest usually lands while the pointer is
  // already resting on the player.
  try {
    Promise.resolve(chrome.tabs.sendMessage(tabId, { type: "hydra-media-changed" })).catch(
      () => {}
    );
  } catch {
    // No content script here (chrome://, the PDF viewer, a closed tab).
  }
}

chrome.webRequest?.onResponseStarted.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const headers = Object.fromEntries(
      (details.responseHeaders || []).map((h) => [h.name.toLowerCase(), h.value])
    );
    const mime = (headers["content-type"] || "").split(";")[0].trim();
    const url = details.url.split("#")[0];

    // A manifest first: its MIME is often a generic text type, so the path
    // gets a vote too.
    if (HLS_MIME.test(mime) || DASH_MIME.test(mime) || HLS_PATH.test(url) || DASH_PATH.test(url)) {
      withTab(details.tabId, async () => {
        const tab = await chrome.tabs.get(details.tabId).catch(() => null);
        await noteStream(details.tabId, url, mime, tab?.url || details.initiator || null);
        await refreshBadge(details.tabId);
      });
      return;
    }

    // The type decides, except when the server declined to give one.
    if (!isMediaMime(mime) && !(VAGUE_MIME.test(mime) && MEDIA_PATH.test(url))) return;
    // Stream segments are ordinary-looking media (fMP4 is video/mp4) and
    // there are hundreds of them; the manifest already stands for the lot.
    if (SEGMENT_MIME.test(mime) || SEGMENT_PATH.test(url)) return;

    const size = parseInt(headers["content-length"] || "0", 10) || 0;
    // Range replies of streaming players still reveal the full size here.
    const total = /\/(\d+)$/.exec(headers["content-range"] || "");
    const fullSize = total ? parseInt(total[1], 10) : size;
    const floor = /^audio\//i.test(mime) ? AUDIO_MIN_BYTES : MEDIA_MIN_BYTES;
    if (fullSize && fullSize < floor) return;

    withTab(details.tabId, async () => {
      const key = `media_${details.tabId}`;
      const list = await tabMedia(details.tabId);
      // Same resource re-requested with shifting Range offsets: keep one entry.
      const bare = url.split("?")[0];
      if (list.some((m) => m.url.split("?")[0] === bare)) return;
      // Segments sit next to their playlist; anything under a detected
      // manifest's directory belongs to that stream, whatever it is called.
      const dirs = (await tabStreams(details.tabId)).map((s) =>
        s.key.slice(0, s.key.lastIndexOf("/") + 1)
      );
      if (dirs.some((d) => d && streamKey(url).startsWith(d))) return;

      // The label comes from the MIME here, where MIME_EXT already lives.
      // Deriving it from the URL alone in the panel guesses "MP4" whenever a
      // path carries no extension, which is most streaming endpoints — so an
      // MP3 offered itself as "MP4 file".
      const kind = MIME_EXT[mime.split(";")[0].trim().toLowerCase()] || null;
      list.push({ url, mime, kind, size: fullSize || null });
      if (list.length > MEDIA_PER_TAB) list.shift();
      await sessionStore().set({ [key]: list });
      await refreshBadge(details.tabId);
    });
  },
  // `main_frame` and `sub_frame` matter as much as `media` here. An <audio>
  // or <video> element loads as `media`, but a LINK to an .mp3 — the browser
  // opening its built-in player, which is what "played in the browser" means
  // most of the time — is a top-level navigation, and a player in an iframe
  // is a sub-frame. Without these two, those responses never reached this
  // listener at all, whatever their Content-Type said.
  //
  // The cost is a few regex tests on each navigation before returning; a
  // manifest opened directly in a tab now gets sniffed too.
  {
    urls: ["<all_urls>"],
    types: ["main_frame", "sub_frame", "media", "xmlhttprequest", "other"],
  },
  ["responseHeaders"]
);

async function clearTab(tabId) {
  tabLocks.delete(tabId);
  await sessionStore().remove([
    `media_${tabId}`,
    `streams_${tabId}`,
    `streamkids_${tabId}`,
  ]);
  refreshBadge(tabId);
}
chrome.tabs.onRemoved.addListener((tabId) => clearTab(tabId));
chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (changeInfo.url) clearTab(tabId); // navigated away: stale list
});

// ------------------------------------------------------------ popup/content

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  (async () => {
    // A content script speaks for the tab it runs in; the popup has no tab
    // of its own and means the active one.
    const senderTab = sender?.tab?.id ?? null;
    const whichTab = async () => {
      if (senderTab != null) return senderTab;
      const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
      return tab?.id ?? null;
    };
    switch (msg?.type) {
      case "alt-click":
        await sessionStore().set({ altTs: Date.now() });
        sendResponse({ ok: true });
        break;
      case "get-state": {
        const state = await getState();
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        const media = tab?.id != null ? await tabMedia(tab.id) : [];
        const streams = tab?.id != null ? await tabStreams(tab.id) : [];
        sendResponse({
          state,
          media,
          streams,
          connected: wsReady,
          tabId: tab?.id ?? null,
          tabUrl: tab?.url ?? null,
        });
        break;
      }
      case "set-enabled":
        await chrome.storage.local.set({ enabled: !!msg.enabled });
        sendResponse({ ok: true });
        break;
      case "set-video-panel":
        await chrome.storage.local.set({ videoPanel: !!msg.on });
        sendResponse({ ok: true });
        break;
      case "page-media": {
        // What the in-page panel may offer, for the asking tab only.
        const id = await whichTab();
        const state = await getState();
        sendResponse({
          streams: id != null ? await tabStreams(id) : [],
          media: id != null ? await tabMedia(id) : [],
          videoPanel: state.enabled && state.videoPanel,
        });
        break;
      }
      case "ping":
        // Probe only: never boots the app, so the popup can report status.
        sendResponse((await wsRequest({ type: "ping" }, 2500)) || { ok: false, unreachable: true });
        break;
      case "status": {
        // For the welcome page. Probe only: `wsRequest` cannot start
        // anything, and `hydra-host` answers a "ping" WITHOUT launching the
        // app — which is exactly what separates "host is not installed"
        // (unreachable) from "host is fine, Hydra is simply closed".
        const viaWs = wsReady || !!(await wsRequest({ type: "ping" }, 2500));
        if (viaWs) {
          sendResponse({ app: true, host: true });
          break;
        }
        const host = await native({ type: "ping" });
        sendResponse({ app: !!host?.ok, host: !host?.unreachable });
        break;
      }
      case "open-hydra":
        sendResponse(await request({ type: "open" }));
        break;
      case "download-url":
        sendResponse(await sendToHydra(msg.url, { referer: msg.referer || null }));
        break;
      case "download-stream": {
        const id = await whichTab();
        const entry = (id != null ? await tabStreams(id) : []).find((s) => s.key === msg.key);
        if (!entry) return sendResponse({ ok: false, error: "stream is gone" });
        const variant =
          entry.variants.find((v) => streamVariantId(v) === msg.variant) ||
          entry.variants[0] ||
          null;
        // The popup sends no name; the page title is the best one there is.
        const tab = await chrome.tabs.get(id).catch(() => null);
        sendResponse(
          await sendStreamToHydra(entry, variant, {
            container: msg.container,
            filename: msg.filename || titleName(tab?.title),
          })
        );
        break;
      }
      case "selection-links": {
        // The floating pill over highlighted links: one link behaves like a
        // click on it, several open the batch box.
        const urls = [...new Set((msg.urls || []).filter((u) => /^https?:/i.test(u)))];
        if (!urls.length) return sendResponse({ ok: false, error: "no links" });
        if (urls.length === 1) {
          sendResponse(await sendToHydra(urls[0], { referer: msg.referer || null }));
        } else {
          sendResponse(await request({ type: "links", urls }));
        }
        break;
      }
      case "download-all-links": {
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        if (tab?.id == null) return sendResponse({ ok: false, error: "no tab" });
        try {
          const resp = await chrome.tabs.sendMessage(tab.id, { type: "collect-links" });
          const urls = [...new Set((resp?.urls || []).filter((u) => /^https?:/i.test(u)))];
          if (!urls.length) return sendResponse({ ok: false, error: "no links" });
          sendResponse(await request({ type: "links", urls }));
        } catch {
          sendResponse({ ok: false, error: "page not accessible" });
        }
        break;
      }
      default:
        sendResponse({ ok: false, error: "unknown message" });
    }
  })();
  return true; // async sendResponse
});

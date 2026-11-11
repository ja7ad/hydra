// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hydra browser integration: the part that is the same in every browser.
//
// Architecture:
//  - Primary transport: a persistent WebSocket to the app's fixed loopback
//    port (hydra: 6799/16799), reconnecting with backoff forever. Its first
//    frame is `auth` with the app's per-run token; the socket is usable
//    once the app has agreed, and doubles as the "app is running" indicator
//    (toolbar shows a gray X when it is down).
//  - Fallback transport: native messaging through `hydra-host`, which can
//    LAUNCH the app when it is not running, after which the WebSocket
//    takes over again — and which is the only party that can read the
//    token (`ws-token`), so reaching the host is what proves this extension
//    is Hydra's own.
//  - Capture: the gates a download passes and the hand-off itself live
//    here (`offerToHydra`, `decideCapture`). WHEN to ask, and what to do
//    with the browser's own copy, is the one thing the browsers do not
//    agree on, so that part is each browser's own `background.js`, loaded
//    after this file into the same global scope:
//      chrome/background.js   Chromium parks the download and hands it back
//      firefox/background.js  Gecko decides off the response headers
//    Safari has no downloads API; its background is this file alone.
//
// Everything in here is a top-level declaration on purpose: a classic script
// shares its globals with the scripts loaded beside it, which is how the
// browser files reach `offerToHydra` without any export plumbing.

// Safari exposes the promise-based `browser` namespace (and `chrome` only
// since 16.4), so bind whichever exists and feature-detect the rest.
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

// Seconds the in-page video bar stays up after it has shown itself. Zero
// leaves it up until it is dismissed; the ceiling only keeps a typo out of
// setTimeout.
const PANEL_TIMEOUT_DEFAULT = 10;
const PANEL_TIMEOUT_MAX = 600;

// When the selection pill is offered. "multi" is the middle answer the
// reports asked for: on a release page a highlighted column of files is
// exactly the batch the pill is for, while a single link is already served by
// the right-click menu and by Alt+click.
const PILL_MODES = ["always", "multi", "never"];
const PILL_MODE_DEFAULT = "always";

function clampPillMode(v) {
  return PILL_MODES.includes(v) ? v : PILL_MODE_DEFAULT;
}

function clampPanelTimeout(v) {
  // An empty field is "I have not chosen", not "never hide it" — and
  // Number("") is 0, which would quietly mean the latter.
  if (v === "" || v == null) return PANEL_TIMEOUT_DEFAULT;
  const n = Math.round(Number(v));
  if (!Number.isFinite(n)) return PANEL_TIMEOUT_DEFAULT;
  return Math.min(Math.max(n, 0), PANEL_TIMEOUT_MAX);
}

async function getState() {
  return chrome.storage.local.get({
    enabled: true, // extension-side master switch (popup toggle)
    guiCapture: true, // "Google Chrome" checkbox in hydra's Options
    autoTypes: DEFAULT_TYPES,
    skipSites: DEFAULT_SKIP,
    videoPanel: true, // the floating Download button over <video> elements
    selectionPill: PILL_MODE_DEFAULT, // when the pill is offered beside selected links
    panelTimeout: PANEL_TIMEOUT_DEFAULT, // seconds it lingers; 0 = until dismissed
    hydraSeen: false, // ever completed a round-trip
    plugins: [], // enabled Hydra plugins and the addresses they claim
  });
}

function absorbReply(reply) {
  if (!reply || typeof reply !== "object" || reply.unreachable) return;
  const patch = { hydraSeen: true };
  if (typeof reply.capture === "boolean") patch.guiCapture = reply.capture;
  if (typeof reply.auto_types === "string" && reply.auto_types) patch.autoTypes = reply.auto_types;
  if (typeof reply.dont_start_sites === "string") patch.skipSites = reply.dont_start_sites;
  if (Array.isArray(reply.plugins)) patch.plugins = reply.plugins;
  chrome.storage.local.set(patch);
}

// --------------------------------------------------------------- transport

let ws = null;
let wsReady = false; // authenticated: requests may go over it
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

/// `keepPort`: the port answered (it refused the token, or dropped a live
/// socket), so it is the one to dial again rather than the fallback.
function wsScheduleReconnect(delayMs = 4000, keepPort = false) {
  if (reconnectTimer) return;
  if (!keepPort) wsPortIdx++; // next attempt tries the fallback port
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    wsConnect();
  }, delayMs);
}

function wsConnect() {
  if (ws) return;
  let sock;
  try {
    sock = new WebSocket(`ws://127.0.0.1:${WS_PORTS[wsPortIdx % WS_PORTS.length]}/`);
  } catch {
    wsScheduleReconnect();
    return;
  }
  ws = sock;
  sock.onopen = () => wsAuthenticate(sock);
  sock.onmessage = (ev) => {
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
  sock.onclose = sock.onerror = () => {
    if (ws !== sock) return; // already torn down, or replaced meanwhile
    wsTeardown();
    setConnected(false);
    wsScheduleReconnect();
  };
}

const HEARTBEAT_MS = 20000;
const HEARTBEAT_REPLY_MS = 5000;
// How long to leave the socket alone when the host could not be asked for
// a token: without the host there is nothing new to say to the app.
const WS_TOKEN_RETRY_MS = 30000;

/// The first frame on a fresh socket: `auth` with the cached token, or with
/// none — the extension ids Hydra ships under are admitted without one, and
/// an older app that predates the handshake answers it like any request. A
/// refusal means the token is missing or stale (the app restarted), so the
/// native host is asked for the current one and the socket redialled;
/// without a host to ask, captures fall back to native messaging as before.
async function wsAuthenticate(sock) {
  const { ws_token: cached = null } = await sessionStore().get("ws_token");
  if (ws !== sock) return;
  const reply = await wsRequest({ type: "auth", token: cached ?? undefined }, HEARTBEAT_REPLY_MS, sock);
  if (ws !== sock) return;
  if (reply && reply.error !== "unauthorized") {
    wsReady = true;
    setConnected(true);
    // Heartbeat: keeps the service worker alive (Chrome extends SW life on
    // WS traffic) and notices a dead app quickly.
    heartbeatTimer = setInterval(heartbeat, HEARTBEAT_MS);
    wsRequest({ type: "config" }, 5000); // sync capture settings on connect
    retryDeferredCaptures();
    return;
  }
  // The app closes a socket it refused; a silent one is as good as gone.
  wsTeardown();
  setConnected(false);
  if (!reply) return wsScheduleReconnect();
  await sessionStore().remove("ws_token");
  const token = await fetchWsToken();
  wsScheduleReconnect(token ? 200 : WS_TOKEN_RETRY_MS, true);
}

let wsTokenFetch = null;

/// The app's current WebSocket token, by way of the native host (a process
/// spawn, so concurrent askers share one). Null when there is no host, or
/// no app for it to ask.
function fetchWsToken() {
  wsTokenFetch ??= native({ type: "ws-token" })
    .then(async (reply) => {
      if (!reply?.ok || typeof reply.token !== "string") return null;
      await sessionStore().set({ ws_token: reply.token });
      return reply.token;
    })
    .finally(() => {
      wsTokenFetch = null;
    });
  return wsTokenFetch;
}

/// One heartbeat. A socket the OS silently dropped — the machine slept, the
/// app was killed — looks open from here and reports nothing: every request
/// on it then times out, and each capture paid the full 8 s before falling
/// back to the native host. A ping that goes unanswered is that socket, so
/// it is torn down on the spot and the reconnect loop takes over.
async function heartbeat() {
  const sock = ws;
  const pong = await wsRequest({ type: "ping" }, HEARTBEAT_REPLY_MS);
  if (pong || ws !== sock) return; // answered, or already replaced meanwhile
  wsTeardown();
  setConnected(false);
  wsScheduleReconnect(1000);
}

/// Returns the reply, or null when the socket is down / timed out — the
/// caller then falls back to the native host. `sock` is only given by the
/// handshake, which speaks before the socket is ready for anyone else.
function wsRequest(msg, timeoutMs = 8000, sock = wsReady ? ws : null) {
  return new Promise((resolve) => {
    if (!sock) return resolve(null);
    const id = reqSeq++;
    const timer = setTimeout(() => {
      pendingReqs.delete(id);
      resolve(null);
    }, timeoutMs);
    pendingReqs.set(id, { resolve, timer });
    try {
      sock.send(JSON.stringify({ browser: BROWSER, ...msg, id }));
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

// ------------------------------------------------------------------- proxy
//
// The proxy the BROWSER is using, handed over with the download so Hydra
// leaves by the same door. Without it a file that only the tunnel can reach
// is fetched by an app that has never heard of the tunnel, and the address
// has to be retyped into Hydra's Options by hand.
//
// It travels as ONE download's proxy, not as a change to Options: it is the
// browser's setting, it can change without telling us, and a user who has
// deliberately pointed Hydra somewhere else must keep that.
//
// The `proxy` permission is OPTIONAL and doubles as the feature switch —
// granted means on, and the API itself is absent until it is. An extension
// that already asks for `downloads`, `cookies` and every host must not also
// be disabled at its next update over a setting most people never turn on,
// and a second stored flag beside the permission could only ever disagree
// with it.
//
// A browser following the machine's proxy ("system" / "autoDetect") will not
// say what that is, and resolves to "no proxy known", which leaves the
// download on whatever Options says, exactly as if this were switched off.
// A proxy switcher (ZeroOmega, SwitchyOmega) is the other case people have:
// in Chromium it installs a PAC script, which is run here for the one URL;
// in Firefox it routes each request from a listener the settings never show,
// so the route a response actually took is remembered instead.

// A local settings read answers in microseconds. The ceiling is not about
// slowness: a capture AWAITS this, and a browser whose namespace neither
// calls the callback nor returns a promise would park the download forever
// on a question about a proxy. Answering "no proxy known" is the same thing
// the feature does when it is switched off.
const PROXY_READ_TIMEOUT_MS = 1500;

/// The browser's proxy configuration, or null when the permission has not
/// been granted (or the API does not exist at all, as in Safari).
function proxyConfig() {
  return new Promise((resolve) => {
    const api = chrome.proxy?.settings;
    if (!api?.get) return resolve(null);
    const timer = setTimeout(() => resolve(null), PROXY_READ_TIMEOUT_MS);
    const settle = (v) => {
      clearTimeout(timer);
      resolve(v);
    };
    const done = (got) => settle(chrome.runtime.lastError ? null : (got?.value ?? null));
    const failed = () => settle(null);
    try {
      // Both dialects, like `native()` above: Chromium takes a callback and
      // reports failure through lastError, a promise-only namespace returns
      // one. Reading it twice would be harmless — it is a read — but only
      // one of the two ever answers.
      const ret = api.get({}, done);
      if (ret && typeof ret.then === "function") ret.then(done, failed);
    } catch {
      // A namespace strict enough to reject the trailing callback as an
      // unexpected argument: ask again the way it wants to be asked, rather
      // than reporting "no proxy" for what is really a calling convention.
      try {
        Promise.resolve(api.get({})).then(done, failed);
      } catch {
        failed();
      }
    }
  });
}

// What Hydra's `Proxy::parse` accepts. QUIC — which Chromium can name — is
// not among them, and a proxy it cannot speak is better left unsaid than
// sent as something it is not.
const PROXY_PORTS = { http: 80, https: 443, socks4: 1080, socks4a: 1080, socks5: 1080 };

/// `scheme://host:port`, or null when there is nothing usable to say.
///
/// Credentials are never part of it, and neither browser offers any:
/// Chromium's `ProxyServer` has no such fields and Firefox keeps proxy
/// logins in the password manager. A proxy that needs a login is finished
/// in Hydra's own Options.
function proxySpec(scheme, hostport) {
  const raw = String(hostport ?? "").trim();
  if (!raw) return null;
  // Firefox stores a bare `host:port`, but its own documented example stores
  // `http://proxy.org:8080`: a scheme the value states wins over the field
  // it was read from.
  const at = raw.indexOf("://");
  let kind = String(at < 0 ? scheme || "http" : raw.slice(0, at)).toLowerCase();
  if (kind === "socks") kind = "socks5"; // the bare word means 5 everywhere
  if (!(kind in PROXY_PORTS)) return null;
  const rest = (at < 0 ? raw : raw.slice(at + 3)).replace(/\/+$/, "");
  if (!rest) return null;
  // A port is optional in Chromium's ProxyServer and defaults PER SCHEME
  // there, while Hydra's parser defaults 8080 for HTTP. State it rather than
  // let the two disagree about what `http://proxy` means.
  const ported = /:\d+$/.test(rest) || (rest.startsWith("[") && !rest.endsWith("]"));
  return ported ? `${kind}://${rest}` : `${kind}://${rest}:${PROXY_PORTS[kind]}`;
}

/// A host the browser would NOT send through its proxy. Chromium publishes
/// the list as `bypassList`, Firefox as `passthrough`, and the two pattern
/// languages agree on everything that turns up in practice: a host, a
/// `.suffix` or `*.suffix`, and `<local>` for the dotless names an intranet
/// uses. A port or scheme attached to a pattern is ignored — narrowing a
/// bypass to one port is vanishingly rare, and reading it wrong sends
/// intranet traffic to a proxy that will refuse it.
function proxyBypassed(list, url) {
  let host;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  return []
    .concat(list ?? [])
    .join(",")
    .toLowerCase()
    .split(/[\s,;]+/)
    .filter(Boolean)
    .some((pat) => {
      // `<-loopback>` REMOVES the implicit loopback bypass; it never adds one.
      if (pat === "<-loopback>") return false;
      if (pat === "<local>") return !host.includes(".");
      const bare = pat.replace(/^[a-z0-9+.-]+:\/\//, "").replace(/:\d+$/, "");
      if (!bare) return false;
      if (bare.startsWith(".")) return host.endsWith(bare);
      if (!bare.includes("*")) return host === bare;
      const rx = new RegExp(`^${bare.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`);
      return rx.test(host);
    });
}

/// The scheme the origin is reached by, as the proxy settings spell it.
function urlScheme(url) {
  try {
    return new URL(url).protocol.replace(":", "").toLowerCase();
  } catch {
    return "http";
  }
}

/// Chromium's `ProxyConfig`. Only `fixed_servers` names an address we can
/// read; every other mode is either direct or something the browser keeps
/// to itself.
function chromiumProxy(cfg, url) {
  if (cfg.mode !== "fixed_servers") return null;
  const rules = cfg.rules || {};
  if (proxyBypassed(rules.bypassList, url)) return null;
  const scheme = urlScheme(url);
  const pick =
    rules.singleProxy ||
    (scheme === "https"
      ? rules.proxyForHttps
      : scheme === "ftp"
        ? rules.proxyForFtp
        : rules.proxyForHttp) ||
    rules.fallbackProxy;
  if (!pick?.host) return null;
  return proxySpec(pick.scheme, pick.port ? `${pick.host}:${pick.port}` : pick.host);
}

/// Firefox's proxy settings. The per-protocol fields are HTTP proxies;
/// `socks` is the fallback for everything they do not cover, which is how
/// Firefox itself routes a request.
function geckoProxy(cfg, url) {
  if (cfg.proxyType !== "manual") return null;
  if (proxyBypassed(cfg.passthrough, url)) return null;
  const scheme = urlScheme(url);
  const perScheme = cfg.httpProxyAll
    ? cfg.http
    : scheme === "https"
      ? cfg.ssl
      : scheme === "ftp"
        ? cfg.ftp
        : cfg.http;
  return (
    proxySpec("http", perScheme) ||
    proxySpec(cfg.socksVersion === 4 ? "socks4" : "socks5", cfg.socks)
  );
}

// What a PAC entry's keyword means to Chromium, the one running the script.
// A bare SOCKS there is version 4, unlike the bare word in a settings field.
const PAC_KINDS = {
  PROXY: "http",
  HTTPS: "https",
  SOCKS: "socks4",
  SOCKS4: "socks4",
  SOCKS5: "socks5",
};

/// The first usable entry of a `FindProxyForURL` answer such as
/// `PROXY 127.0.0.1:8080; DIRECT`. DIRECT ahead of any proxy means the
/// browser goes direct, and so does the download.
function pacAnswerProxy(answer) {
  for (const entry of String(answer ?? "").split(";")) {
    const [kind, where] = entry.trim().split(/\s+/);
    if (!kind) continue;
    const upper = kind.toUpperCase();
    if (upper === "DIRECT") return null;
    const spec = upper in PAC_KINDS ? proxySpec(PAC_KINDS[upper], where) : null;
    if (spec) return spec;
  }
  return null;
}

// A rule list can run to thousands of entries and the offscreen document may
// have to be created first; still, a capture awaits this, so a script that
// loops or a PAC server that never answers costs a bounded wait and then
// "no proxy known".
const PAC_TIMEOUT_MS = 3000;
const PAC_DOCUMENT = "offscreen.html";
let pacDocumentCreating = null;

async function ensurePacDocument() {
  const open = await chrome.runtime.getContexts({ contextTypes: ["OFFSCREEN_DOCUMENT"] });
  if (open.length) return;
  pacDocumentCreating ??= chrome.offscreen
    .createDocument({
      url: PAC_DOCUMENT,
      reasons: ["IFRAME_SCRIPTING"],
      justification: "Evaluate the browser's proxy auto-config script for a captured download.",
    })
    // Losing a race with a service worker instance that created it first
    // still leaves a document to talk to.
    .catch(() => {})
    .finally(() => {
      pacDocumentCreating = null;
    });
  await pacDocumentCreating;
}

async function pacScriptText(pac) {
  if (typeof pac?.data === "string" && pac.data.trim()) return pac.data;
  if (!pac?.url) return null;
  const res = await fetch(pac.url, { cache: "no-store" });
  return res.ok ? res.text() : null;
}

/// Run Chromium's PAC script for this URL in the sandbox, as the browser
/// would: an https URL is reduced to its origin before the script sees it.
async function evaluatePac(pac, url) {
  const script = await pacScriptText(pac);
  if (!script) return null;
  await ensurePacDocument();
  const u = new URL(url);
  const reply = await chrome.runtime.sendMessage({
    target: "hydra-pac",
    script,
    url: u.protocol === "https:" ? `${u.origin}/` : url,
    host: u.hostname.replace(/^\[|\]$/g, ""),
  });
  return reply && !reply.error ? pacAnswerProxy(reply.result) : null;
}

async function pacProxy(pac, url) {
  if (!chrome.offscreen?.createDocument || !chrome.runtime.getContexts) return null;
  const expired = Symbol("expired");
  let timer;
  const deadline = new Promise((resolve) => {
    timer = setTimeout(() => resolve(expired), PAC_TIMEOUT_MS);
  });
  const answer = evaluatePac(pac, url).catch(() => null);
  const got = await Promise.race([answer, deadline]);
  clearTimeout(timer);
  if (got !== expired) return got;
  // A script stuck in a loop holds the sandbox for good; closing the document
  // discards it, and the next capture starts a fresh one.
  chrome.offscreen.closeDocument().catch(() => {});
  return null;
}

// Firefox: the proxy each recent response really came through, by origin.
// Bounded, oldest first out; an origin seen again moves to the back.
const OBSERVED_PROXY_ORIGINS = 256;
const observedProxies = new Map();

function proxyInfoSpec(info) {
  if (!info?.host) return null;
  const host = info.host.includes(":") && !info.host.startsWith("[") ? `[${info.host}]` : info.host;
  const kinds = { http: "http", https: "https", socks: "socks5" };
  const kind = info.type === "socks4" ? (info.proxyDNS ? "socks4a" : "socks4") : kinds[info.type];
  return kind ? proxySpec(kind, `${host}:${info.port}`) : null;
}

/// Note the route a Firefox webRequest event reports. Credentials are left
/// out, as in `proxySpec`.
function noteProxyInfo(details) {
  let origin;
  try {
    origin = new URL(details.url).origin;
  } catch {
    return;
  }
  observedProxies.delete(origin);
  observedProxies.set(origin, proxyInfoSpec(details.proxyInfo));
  if (observedProxies.size > OBSERVED_PROXY_ORIGINS) {
    observedProxies.delete(observedProxies.keys().next().value);
  }
}

function observedProxy(url) {
  try {
    return observedProxies.get(new URL(url).origin) ?? null;
  } catch {
    return null;
  }
}

if (BROWSER === "Mozilla Firefox") {
  chrome.webRequest?.onHeadersReceived.addListener(noteProxyInfo, { urls: ["<all_urls>"] });
}

/// The proxy to hand over with this URL, or null for "Hydra decides".
async function captureProxy(url) {
  const cfg = await proxyConfig();
  if (!cfg) return null;
  // The two browsers answer with different objects; each names itself.
  if (typeof cfg.proxyType === "string") {
    return cfg.proxyType === "manual" ? geckoProxy(cfg, url) : observedProxy(url);
  }
  if (cfg.mode === "pac_script") return pacProxy(cfg.pacScript, url);
  if (typeof cfg.mode === "string") return chromiumProxy(cfg, url);
  return null;
}

// ----------------------------------------------------------------- cookies

/// The site (scheme + registrable host) a partitioned cookie is keyed on:
/// the top-level page the request was made under, as the `topLevelSite` of
/// a cookie partition key spells it. Null when the page is unknown, in
/// which case every partition is asked.
function partitionSite(pageUrl) {
  try {
    const u = new URL(pageUrl);
    if (!/^https?:$/.test(u.protocol)) return null;
    return `${u.protocol}//${u.hostname}`;
  } catch {
    return null;
  }
}

/// Cookies the plain `getAll({url})` leaves out: those set with the
/// `Partitioned` attribute (CHIPS) are keyed on the top-level site they
/// were set under and only come back when a partition is named. A player
/// embedded on one site and a CDN that answers only with such a cookie is
/// exactly the case that used to reach Hydra with no cookie at all.
///
/// A browser that predates the attribute rejects the unknown property
/// outright; that is the empty list, never a failed capture.
async function partitionedCookies(url, pageUrl) {
  const site = partitionSite(pageUrl);
  const partitionKey = site ? { topLevelSite: site } : {};
  try {
    return (await chrome.cookies.getAll({ url, partitionKey })) || [];
  } catch {
    return [];
  }
}

async function cookieHeader(url, pageUrl = null) {
  try {
    const plain = (await chrome.cookies.getAll({ url })) || [];
    const seen = new Set();
    const cookies = [];
    for (const c of plain.concat(await partitionedCookies(url, pageUrl))) {
      const pair = `${c.name}=${c.value}`;
      if (seen.has(pair)) continue;
      seen.add(pair);
      cookies.push(pair);
    }
    if (!cookies.length) return null;
    return cookies.join("; ");
  } catch {
    return null;
  }
}

// ----------------------------------------------------------------- capture

// A signed URL good for longer than this is in no danger from a redirect or a
// retry, so it is handed over as-is. Mirrors `expiring_soon` in the GUI.
const EXPIRY_HORIZON = 3600;

// The deadline a signed URL carries, in unix seconds, or null for none.
// Mirrors `hya_net::signed`: SigV4 (S3 and every dialect of it) states an
// issue time and a window, CloudFront and GCS sign a bare `Expires` epoch.
function signedDeadline(url) {
  let params;
  try {
    params = new URL(url).searchParams;
  } catch {
    return null;
  }
  const get = (name) => {
    for (const [k, v] of params) if (k.toLowerCase() === name) return v;
    return null;
  };
  const issued = get("x-amz-date");
  const window = Number(get("x-amz-expires"));
  if (issued && Number.isFinite(window)) {
    const t = Date.parse(
      issued.replace(
        /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})Z$/,
        "$1-$2-$3T$4:$5:$6Z",
      ),
    );
    if (!Number.isNaN(t)) return Math.floor(t / 1000) + window;
  }
  const expires = Number(get("expires"));
  // `Expires=0` is how a cache header spells "stale", not a 1970 deadline.
  return Number.isFinite(expires) && expires > 1e9 ? expires : null;
}

// Which of a download's two URLs to hand Hydra.
//
// Normally the resolved one: it is where the bytes are, and reaching them
// costs Hydra no redirect. But a resolved URL that is a SHORT-LIVED SIGNATURE
// is the wrong thing to hand over — figshare's store signs for ten seconds —
// because our copy of it is already dead by the first retry, and a paused
// download can never be resumed from it. The original link does not have that
// problem: replayed with the cookies we send beside it, it mints a fresh
// signature on every attempt.
function captureUrl(item) {
  const resolved = item.finalUrl || item.url;
  if (!item.url || item.url === resolved) return resolved;
  const deadline = signedDeadline(resolved);
  if (deadline === null) return resolved;
  return deadline <= Date.now() / 1000 + EXPIRY_HORIZON ? item.url : resolved;
}

// WeTransfer's page downloads through its own service worker, at
// `/download/local-bartender?id=<base64 JSON>`. That URL exists only inside the
// browser that registered the worker; anywhere else it is a 404. The `id` holds
// what the worker would send to the API, so we ask for the real link instead.
async function resolveLocalBartender(url) {
  let page;
  try {
    page = new URL(url);
  } catch {
    return url;
  }
  if (!/(^|\.)wetransfer\.com$/i.test(page.hostname) || page.pathname !== "/download/local-bartender") {
    return url;
  }
  try {
    const job = JSON.parse(atob(page.searchParams.get("id")));
    const body = { security_hash: job.secret, intent: job.intent };
    if (job.password) body.password = job.password;
    if (job.recipientId) body.recipient_id = job.recipientId;
    if (job.fileIds) body.file_ids = String(job.fileIds).split(",");
    const reply = await fetch(`${page.origin}/api/v4/transfers/${encodeURIComponent(job.transferId)}/download`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      credentials: "include",
    });
    if (!reply.ok) return url;
    const { direct_link: link } = await reply.json();
    return /^https:/i.test(link) ? link : url;
  } catch {
    return url;
  }
}

async function sendToHydra(url, extras = {}) {
  return request({
    type: "download",
    url,
    cookies: await cookieHeader(url, extras.referer || extras.tab_url || null),
    user_agent: navigator.userAgent,
    proxy: await captureProxy(url),
    ...extras,
  });
}

// The gates every capture passes, and the handoff itself. Returns null when
// Hydra took the transfer, otherwise the reason it was left to the browser.
//
// Two callers ask these questions, in this order, about the same transfer at
// different moments: the Chromium path from a parked DownloadItem, the Gecko
// path from response headers that have not become a download yet. Only what
// they do with the answer differs.
async function offerToHydra({ url, filename, mime, size, referer }) {
  const state = await getState();
  if (!state.enabled || !state.guiCapture) return "capture is switched off";
  if (!/^(https?|ftp):/i.test(url)) return "not an http(s)/ftp download";
  if (siteSkipped(state.skipSites, url)) return "site is on the skip list";
  if (await altBypassed()) return "Alt was held down";
  if (!typeMatches(state.autoTypes, filename, url, mime)) {
    return (
      `"${extOf(filename) || extOf(url) || "no extension"}" is not in the ` +
      `capture list (Options > Downloaded file types)`
    );
  }
  const reply = await sendToHydra(await resolveLocalBartender(url), {
    filename: filename ? String(filename).split(/[/\\]/).pop() : null,
    referer: referer || null,
    size: size > 0 ? size : null,
    mime: mime || null,
  });
  if (reply && reply.ok) return null;
  return /^hydra is starting$/i.test(reply?.error || "") ? HYDRA_STARTING : "Hydra did not take it";
}

// ----------------------------------------------------------------- plugins

// A plugin claim is a Chrome-style match pattern, read the way hydra-plugin's
// matcher.rs reads it: `magnet:*` claims a whole scheme, `*://` means http or
// https, `*.host` means strictly a subdomain, and `*` in the path is a glob
// over path and query.
function claimMatches(pattern, address) {
  let url;
  try {
    url = new URL(address);
  } catch {
    return false;
  }
  const scheme = url.protocol.slice(0, -1);
  const whole = /^([a-z]+):\*$/.exec(pattern);
  if (whole) return scheme === whole[1];
  const parts = /^([^:/]+):\/\/([^/]+)(\/.*)$/s.exec(pattern);
  if (!parts) return false;
  const [, want, hostPattern, pathPattern] = parts;
  const schemeOk = want === "*" ? scheme === "http" || scheme === "https" : scheme === want.toLowerCase();
  return schemeOk && claimHostMatches(hostPattern, url) && claimGlob(pathPattern, url.pathname + url.search);
}

const DEFAULT_PORTS = { http: "80", https: "443", ftp: "21" };

function claimHostMatches(pattern, url) {
  const host = url.hostname.toLowerCase().replace(/\.$/, "");
  if (!host || host.includes(":") || host.startsWith("[")) return false;
  const colon = pattern.lastIndexOf(":");
  const [hostPart, port] = colon < 0 ? [pattern, "*"] : [pattern.slice(0, colon), pattern.slice(colon + 1)];
  const urlPort = url.port || DEFAULT_PORTS[url.protocol.slice(0, -1)] || "";
  if (port !== "*" && port !== urlPort) return false;
  const want = hostPart.toLowerCase();
  if (want === "*") return true;
  if (want.startsWith("*.")) return host.endsWith(want.slice(1)) && host.length > want.length - 1;
  return host === want;
}

function claimGlob(pattern, text) {
  const body = pattern.split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*");
  return new RegExp(`^${body}$`, "s").test(text);
}

// Hosts this build must never offer a plugin on — a store policy, set by the
// browser's own background.js (see chrome/background.js). Empty elsewhere.
function pluginWithheld(address) {
  const hosts = globalThis.PLUGIN_HOSTS_WITHHELD || [];
  let host;
  try {
    host = new URL(address).hostname.toLowerCase();
  } catch {
    return false;
  }
  return hosts.some((h) => host === h || host.endsWith(`.${h}`));
}

// Schemes a browser navigates or downloads itself. A plugin claiming one of
// these whole claims no page in particular, and must not take over the web.
const BROWSER_SCHEMES = new Set(["http", "https", "ftp", "file", "data", "blob", "javascript", "about"]);

const claimsWholeBrowserScheme = (claim) => BROWSER_SCHEMES.has(/^([a-z]+):\*$/.exec(claim)?.[1]);

/// The first enabled plugin, in Hydra's resolver order, that claims `address`.
function pluginFor(plugins, address) {
  if (pluginWithheld(address)) return null;
  const claims = (p) => (p.claims || []).some((c) => !claimsWholeBrowserScheme(c) && claimMatches(c, address));
  return (plugins || []).find(claims) || null;
}

/// Link schemes the content script hands to Hydra on click (`magnet`): the
/// ones a whole-scheme claim names and no browser handles on its own.
function pluginSchemes(plugins) {
  const schemes = (plugins || []).flatMap((p) =>
    (p.claims || []).map((c) => /^([a-z]+):\*$/.exec(c)?.[1]).filter((s) => s && !BROWSER_SCHEMES.has(s))
  );
  return [...new Set(schemes)];
}

async function sendPluginUrl(url, { referer = null, tabUrl = null } = {}) {
  const web = /^https?:/i.test(url);
  return request({
    type: "plugin-url",
    url,
    referer,
    tab_url: tabUrl,
    cookies: web ? await cookieHeader(url, referer) : null,
    user_agent: navigator.userAgent,
    proxy: web ? await captureProxy(url) : null,
  });
}

// The native host's answer when it launched the app but the app had not
// published its socket within the host's budget. Not a refusal: the app is
// coming up, and the download can be offered again the moment the
// WebSocket attaches.
const HYDRA_STARTING = "Hydra is still starting";

// How long a download stays parked for an app that is starting before it is
// handed back to the browser. Generous: a cold start on a slow disk, with
// the host's own 20 s already spent, can take this long.
const DEFERRED_CAPTURE_MS = 60000;

// Parked downloads waiting for the app to come up, id -> when they were
// parked. In session storage rather than a timer: a service worker that is
// put to sleep in the meantime takes its timers with it, and a download it
// parked would then stay paused with nobody left to hand it back.
async function deferredCaptures() {
  const { deferred = {} } = await sessionStore().get("deferred");
  return deferred;
}

/// Keep `item` parked until the WebSocket attaches (then offer it again) or
/// the wait runs out (then the browser gets it back).
async function deferCapture(item) {
  const deferred = await deferredCaptures();
  if (item.id in deferred) return;
  deferred[item.id] = Date.now();
  await sessionStore().set({ deferred });
  setTimeout(sweepDeferredCaptures, DEFERRED_CAPTURE_MS + 50);
}

/// Hand back every parked download whose wait has run out. Runs on the
/// timer, and on every wake-up of the worker in case the timer did not.
async function sweepDeferredCaptures() {
  const deferred = await deferredCaptures();
  const now = Date.now();
  let changed = false;
  for (const [id, since] of Object.entries(deferred)) {
    if (now - since < DEFERRED_CAPTURE_MS) continue;
    delete deferred[id];
    changed = true;
    console.debug(`hydra: app did not come up in time — download ${id} handed back`);
    chrome.downloads.resume(Number(id)).catch(() => {});
  }
  if (changed) await sessionStore().set({ deferred });
}

/// Offer every parked download again; called when the socket attaches. One
/// the user resumed or cancelled in the meantime is theirs and is skipped.
async function retryDeferredCaptures() {
  const deferred = await deferredCaptures();
  const ids = Object.keys(deferred).map(Number);
  if (!ids.length) return;
  await sessionStore().set({ deferred: {} });
  for (const id of ids) {
    let item;
    try {
      [item] = await chrome.downloads.search({ id });
    } catch {
      continue;
    }
    if (!item || item.state !== "in_progress" || !item.paused) continue;
    decideCapture(item, true).catch((e) => console.debug(`hydra: retry failed — ${e}`));
  }
}

// `parked` says whether the browser's own download is being held for us, and
// therefore whether there is anything to hand back. Resuming one that was
// never parked is not harmless: on Gecko it throws, and the throw used to be
// swallowed next to a download that had already been cancelled.
// ------------------------------------------------------------ form posts

// A download that answered a form POST — SharePoint and OneDrive zip a folder
// by posting its file list and tokens to `*.svc.ms/transform/zip` — has its
// request in the body. Hydra is handed a URL and asks with a bodiless GET,
// which the service refuses, so such a download stays with the browser.
const POSTED_TTL_MS = 120_000;
const POSTED_MAX = 64;
const postedUrls = new Map();

function notePosted(details) {
  if (details.method !== "POST") return;
  const url = details.url.split("#")[0];
  postedUrls.delete(url);
  postedUrls.set(url, Date.now());
  if (postedUrls.size > POSTED_MAX) postedUrls.delete(postedUrls.keys().next().value);
}

function wasPosted(...urls) {
  return urls.some((url) => {
    const at = url && postedUrls.get(url.split("#")[0]);
    return !!at && Date.now() - at < POSTED_TTL_MS;
  });
}

chrome.webRequest?.onBeforeRequest?.addListener(notePosted, {
  urls: ["http://*/*", "https://*/*"],
  types: ["main_frame", "sub_frame"],
});

async function decideCapture(item, parked) {
  const url = captureUrl(item);

  // Declining used to be silent, and a download Hydra never sees is then
  // indistinguishable from a broken install: the browser just downloads it,
  // exactly as it would with no extension at all. Say which gate turned it
  // away — chrome://extensions -> the service worker's console is where a
  // report of "the extension does nothing" gets answered in one line.
  const giveBack = async (why) => {
    console.debug(`hydra: left to the browser (${why}) — ${url}`);
    if (!parked) return;
    try {
      await chrome.downloads.resume(item.id);
    } catch {}
  };

  if (item.byExtensionId === chrome.runtime.id) return giveBack("started by this extension");
  if (wasPosted(url, item.url, item.finalUrl)) return giveBack("answered a form POST");

  const why = await offerToHydra({
    url,
    filename: item.filename,
    mime: item.mime,
    size: item.totalBytes > 0 ? item.totalBytes : item.fileSize,
    referer: item.referrer,
  });

  if (!why) {
    // Hydra owns it now; drop the browser's copy. Erasing only after the
    // cancel really took: a download that finished before the answer came
    // back is already a file on disk, and erasing its record would leave
    // that file with nothing pointing at it.
    try {
      await chrome.downloads.cancel(item.id);
      await chrome.downloads.erase({ id: item.id });
    } catch {}
  } else if (why === HYDRA_STARTING && parked) {
    // The host launched the app but it has not published its socket yet.
    // The download stays parked and is offered again when the socket
    // attaches; the browser gets it back only if that never happens.
    console.debug(`hydra: app is starting, holding the download — ${url}`);
    await deferCapture(item);
  } else {
    // A gate turned it away, or Hydra is unreachable: the browser download
    // continues untouched.
    await giveBack(why);
  }
}

// Small key->value notes with a lifetime. The capture paths hand transfers to
// one another by URL (and redirect chains by request id) across events that
// fire milliseconds apart; nothing here is worth persisting, and an entry that
// outlives its transfer must not decide the next one.
function expiringNotes(ttlMs) {
  const notes = new Map();
  return {
    set(key, value = true) {
      const now = Date.now();
      for (const [k, n] of notes) if (n.until <= now) notes.delete(k);
      notes.set(key, { until: now + ttlMs, value });
    },
    get(key) {
      const n = notes.get(key);
      return n && n.until > Date.now() ? n.value : undefined;
    },
    take(key) {
      const value = this.get(key);
      notes.delete(key);
      return value;
    },
  };
}

// Downloads the browser is to keep, keyed by URL: the Alt bypass's own
// `downloads.download()` below, and on Gecko a download that existed before
// its response came back. Whichever capture path sees one of these must
// stand aside, or the bypass cancels the very download it just promised.
const browserOwned = expiringNotes(30000);

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
// Any service-worker wake-up re-establishes the socket, and hands back any
// download a previous incarnation parked for an app that never came up.
wsConnect();
sweepDeferredCaptures();

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
const HLS_PATH = /\.m3u8?(?:$|[?#])/i;
const DASH_PATH = /\.mpd(?:$|[?#])/i;

// A response that says nothing about being a manifest and may be one
// anyway: a playlist served as `text/plain` or `application/octet-stream`
// from a path with no extension (`/playlist?id=…`), an MPD served as
// `application/xml`. Only a SMALL one is worth a look — a manifest is
// kilobytes — and only from a script's own request (`xmlhttprequest`, which
// is also how a `fetch()` is reported, or `other`), which is how every
// player asks for one; a navigation to a text file is a page the user is
// reading.
const SNIFF_MIME = /^(?:text\/(?:plain|xml)|application\/(?:xml|octet-stream|binary))?$/i;
const SNIFF_TYPES = new Set(["xmlhttprequest", "other"]);
const SNIFF_MAX_BYTES = 64 * 1024;

function sniffable(type, mime, size) {
  return SNIFF_TYPES.has(type) && SNIFF_MIME.test(mime) && size > 0 && size < SNIFF_MAX_BYTES;
}

// Sniff candidates that were fetched and turned out not to be manifests, so
// a player polling the same endpoint does not have it fetched on every poll.
const notManifests = expiringNotes(60000);

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

/// What to call a captured media file.
///
/// Normally nothing: the URL's own basename is the better name, and Hydra
/// reads it off the URL by itself — `big_buck_bunny_1080p.mp4` beats any page
/// title, and on a page of samples every row would otherwise take the same
/// one. But an object stored as `fc1eced1-6d50-4375-a125-ef65c887d7d5.mp4` was
/// named by the CDN for the CDN, and lands as a row of gibberish the user then
/// renames by hand. There the page title is the answer, which is what IDM
/// gives them.
function mediaName(url, title, kind) {
  const base = decodeURIComponent(url.split(/[?#]/)[0].split("/").pop() || "");
  const stem = base.replace(/\.[^.]+$/, "");
  const opaque =
    !stem ||
    /^[0-9a-f]{8,}$/i.test(stem) ||
    /^[0-9a-f][0-9a-f-]{15,}$/i.test(stem) ||
    /^\d{6,}$/.test(stem);
  if (!opaque) return null;
  const named = titleName(title);
  if (!named) return null;
  // The extension the file really has, else what the server called its type.
  const ext = (/\.([a-z0-9]{2,5})$/i.exec(base)?.[1] || kind || "").toLowerCase();
  return ext ? `${named}.${ext}` : named;
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
async function noteStream(tabId, url, mime, tabUrl, sniff = false) {
  const key = streamKey(url);
  if (sniff && notManifests.get(key)) return;
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
  // A sniff candidate said nothing about being a manifest, so only its
  // body may say so; anything announced by type or path is listed even
  // when it could not be read.
  const announced = sniff
    ? null
    : HLS_MIME.test(mime) || HLS_PATH.test(url)
      ? "hls"
      : DASH_MIME.test(mime) || DASH_PATH.test(url)
        ? "dash"
        : null;
  const protocol = (text && classifyManifest(text)) || announced;
  if (!protocol) {
    if (sniff) notManifests.set(key);
    return;
  }

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
    cookies: await cookieHeader(entry.url, entry.pageUrl),
    user_agent: navigator.userAgent,
    // The manifest's proxy, not the variant's: segments are served from the
    // same origin, and one route has to carry the whole recording.
    proxy: await captureProxy(entry.url),
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

/// The page a response was made for, which is what goes to the origin as
/// the Referer when the file is handed to Hydra. For a request out of an
/// iframe that is the FRAME's document, not the tab's: a player embedded on
/// a blog is served by a CDN that checks for the player's own host, and the
/// blog's address is a Referer it refuses. `webNavigation.getFrame` knows
/// the frame's URL when that permission is held; without it, Chromium's
/// `initiator` (the frame's origin) or Gecko's `documentUrl` still beat the
/// tab. A top-level request keeps the tab's URL, as before.
async function pageUrlOf(details) {
  const http = (u) => (/^https?:/i.test(u || "") ? u : null);
  if (details.frameId > 0) {
    if (chrome.webNavigation?.getFrame) {
      try {
        const frame = await chrome.webNavigation.getFrame({
          tabId: details.tabId,
          frameId: details.frameId,
        });
        if (http(frame?.url)) return frame.url;
      } catch {
        // The frame is gone, or the permission is not held after all.
      }
    }
    const own = http(details.documentUrl) || http(details.initiator);
    if (own) return own;
  }
  const tab = await chrome.tabs.get(details.tabId).catch(() => null);
  return http(tab?.url) || http(details.initiator) || null;
}

chrome.webRequest?.onResponseStarted.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const headers = Object.fromEntries(
      (details.responseHeaders || []).map((h) => [h.name.toLowerCase(), h.value])
    );
    const mime = (headers["content-type"] || "").split(";")[0].trim();
    const url = details.url.split("#")[0];
    const size = parseInt(headers["content-length"] || "0", 10) || 0;

    // A manifest first: its MIME is often a generic text type, so the path
    // gets a vote too — and a small text or octet-stream body a script
    // asked for gets read, because a playlist at `/playlist?id=…` served
    // as text/plain announces itself in no other way.
    const announced =
      HLS_MIME.test(mime) || DASH_MIME.test(mime) || HLS_PATH.test(url) || DASH_PATH.test(url);
    if (announced || sniffable(details.type, mime, size)) {
      withTab(details.tabId, async () => {
        const page = await pageUrlOf(details);
        await noteStream(details.tabId, url, mime, page, !announced);
        await refreshBadge(details.tabId);
      });
      return;
    }

    // The type decides, except when the server declined to give one.
    if (!isMediaMime(mime) && !(VAGUE_MIME.test(mime) && MEDIA_PATH.test(url))) return;
    // Stream segments are ordinary-looking media (fMP4 is video/mp4) and
    // there are hundreds of them; the manifest already stands for the lot.
    if (SEGMENT_MIME.test(mime) || SEGMENT_PATH.test(url)) return;

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
      list.push({ url, mime, kind, size: fullSize || null, pageUrl: await pageUrlOf(details) });
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
  if (!changeInfo.url) return;
  clearTab(tabId); // navigated away: stale list
  // A same-document navigation (YouTube moving to the next video) runs no
  // new content script, so the page is told its address changed.
  try {
    Promise.resolve(chrome.tabs.sendMessage(tabId, { type: "location-changed" })).catch(() => {});
  } catch {
    // No content script in that tab (a browser page); nothing to tell.
  }
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
      case "alt-download": {
        // Gecko's stand-in save (see the content script): the Alt bypass
        // promises the browser keeps this one, and on Firefox only the
        // extension can make that happen. Both capture paths recognise our
        // own downloads, so this cannot fall back into capture.
        if (!/^https?:/i.test(msg.url || "")) {
          return sendResponse({ ok: false, error: "not an http(s) link" });
        }
        try {
          // The header-stage capture does not see who asked; say so here, or
          // the bypass cancels the very download it just promised.
          browserOwned.set(msg.url);
          await chrome.downloads.download({ url: msg.url });
          sendResponse({ ok: true });
        } catch (e) {
          sendResponse({ ok: false, error: String(e) });
        }
        break;
      }
      case "get-state": {
        const state = await getState();
        state.selectionPill = clampPillMode(state.selectionPill);
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
      case "set-selection-pill": {
        const mode = clampPillMode(msg.mode);
        await chrome.storage.local.set({ selectionPill: mode });
        sendResponse({ ok: true, mode });
        break;
      }
      case "set-panel-timeout": {
        const seconds = clampPanelTimeout(msg.seconds);
        await chrome.storage.local.set({ panelTimeout: seconds });
        sendResponse({ ok: true, seconds });
        break;
      }
      case "page-media": {
        // What the in-page panel may offer, for the asking tab only.
        const id = await whichTab();
        const state = await getState();
        sendResponse({
          streams: id != null ? await tabStreams(id) : [],
          media: id != null ? await tabMedia(id) : [],
          videoPanel: state.enabled && state.videoPanel,
          selectionPill: state.enabled ? clampPillMode(state.selectionPill) : "never",
          panelTimeout: clampPanelTimeout(state.panelTimeout),
        });
        break;
      }
      case "proxy-status": {
        // Resolved against the page in front: a proxy can be named per
        // scheme and bypassed per host, so an answer for some other URL
        // would not be the one a download from here gets.
        const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
        const url = /^https?:/i.test(tab?.url || "") ? tab.url : "https://example.com/";
        sendResponse({ spec: await captureProxy(url) });
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
      case "download-url": {
        // Only a file the media sniffer saw is named after the page: an
        // ordinary link — a `.zip` on a downloads page — means its own name,
        // not the article's.
        const id = await whichTab();
        const bare = msg.url.split(/[?#]/)[0];
        const hit = (id != null ? await tabMedia(id) : []).find(
          (m) => m.url.split(/[?#]/)[0] === bare
        );
        const tab = id != null ? await chrome.tabs.get(id).catch(() => null) : null;
        sendResponse(
          await sendToHydra(msg.url, {
            // The frame the file was loaded from outranks the popup's tab
            // URL: for an embedded player that is the page the CDN expects.
            referer: hit?.pageUrl || msg.referer || null,
            filename: hit ? mediaName(msg.url, tab?.title, hit.kind) : null,
          })
        );
        break;
      }
      case "page-plugin": {
        // What the content script may offer: a button when a plugin claims
        // the page itself, and the link schemes it should hand over on click.
        const state = await getState();
        const plugins = state.enabled ? state.plugins : [];
        const url = msg.url || "";
        const page = /^https?:/i.test(url) ? pluginFor(plugins, url) : null;
        sendResponse({
          page: page ? { id: page.id, name: page.name } : null,
          schemes: pluginSchemes(plugins),
        });
        break;
      }
      case "plugin-url": {
        const state = await getState();
        const url = msg.url || "";
        if (!state.enabled || !pluginFor(state.plugins, url)) {
          return sendResponse({ ok: false, error: "no plugin for this address" });
        }
        sendResponse(
          await sendPluginUrl(url, { referer: msg.referer || null, tabUrl: sender?.tab?.url || null })
        );
        break;
      }
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

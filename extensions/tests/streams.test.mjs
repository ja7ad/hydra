// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The stream sniffer end to end: detection, one entry per manifest, segment
// suppression, quality choice, session context, DRM refusal.
// Run from the repository root:  node extensions/tests/streams.test.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";

const SRC = readFileSync("extensions/chrome/background.js", "utf8");
let fails = 0;
const check = (l, c, x = "") => { if (c) console.log(`ok   ${l}`); else { fails++; console.log(`FAIL ${l} ${x}`); } };
const tick = (n = 6) => new Promise((r) => { let i = 0; const f = () => (++i >= n ? r() : setImmediate(f)); setImmediate(f); });

const MASTER = `#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="a1",NAME="English",URI="audio/en.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=5200000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2",FRAME-RATE=30
v8/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS="avc1.64001f,mp4a.40.2"
v5/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=902000,RESOLUTION=640x360,CODECS="avc1.64001e,mp4a.40.2"
v2/index.m3u8
`;
const VARIANT = `#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:6\n` +
  Array.from({ length: 100 }, (_, i) => `#EXTINF:6.0,\nseg${i}.m4s`).join("\n") + `\n#EXT-X-ENDLIST\n`;
const MPD = `<?xml version="1.0"?><MPD type="static" mediaPresentationDuration="PT10M">
 <Period><AdaptationSet contentType="video">
  <Representation id="v0" bandwidth="6000000" width="2560" height="1440" codecs="avc1.640033"/>
  <Representation id="v1" bandwidth="2000000" width="1280" height="720" codecs="avc1.64001f"/>
 </AdaptationSet><AdaptationSet contentType="audio"><Representation id="a0" bandwidth="128000"/></AdaptationSet></Period></MPD>`;
const DRM_MASTER = `#EXTM3U
#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES-CTR,KEYFORMAT="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",URI="data:x"
#EXT-X-STREAM-INF:BANDWIDTH=6000000,RESOLUTION=1920x1080
p/index.m3u8
`;
const BODIES = {
  "https://cdn.ex/hls/master.m3u8?tok=1": MASTER,
  "https://cdn.ex/hls/v8/index.m3u8": VARIANT,
  "https://cdn.ex/hls/v5/index.m3u8": VARIANT,
  "https://cdn.ex/hls/v2/index.m3u8": VARIANT,
  "https://cdn.ex/dash/m.mpd": MPD,
  "https://drm.ex/premium/master.m3u8": DRM_MASTER,
};

function build({ hydraReply = { ok: true } } = {}) {
  const store = {}, sent = [];
  const ev = () => { const l = []; return { addListener: (f) => l.push(f), fire: (...a) => l.map((f) => f(...a)), l }; };
  const mk = () => ({
    async get(d) { if (typeof d === "string") return { [d]: store[d] }; const o = {}; for (const [k, v] of Object.entries(d)) o[k] = k in store ? store[k] : v; return o; },
    async set(p) { Object.assign(store, p); },
    async remove(k) { for (const x of [].concat(k)) delete store[x]; },
  });
  const msgs = ev(), tabsUpdated = ev();
  let badge = "";
  const wr = { addListener: (f) => (wr.fn = f) };
  const chrome = {
    runtime: { id: "x", lastError: null, onInstalled: ev(), onStartup: ev(), onMessage: msgs, getURL: (p) => p, sendNativeMessage: (a, b, c) => c(undefined) },
    storage: { local: mk(), session: mk() },
    action: { setBadgeBackgroundColor: async () => {}, setBadgeText: async ({ text }) => { badge = text; }, setTitle: async () => {} },
    downloads: { onCreated: ev(), onDeterminingFilename: ev(), pause: async () => {}, resume: async () => {}, cancel: async () => {}, erase: async () => {} },
    contextMenus: { removeAll: async () => {}, create: () => {}, onClicked: ev() },
    tabs: { query: async () => [{ id: 7, url: "https://page.ex/watch" }], get: async () => ({ id: 7, url: "https://page.ex/watch" }), sendMessage: async () => ({ urls: [] }), create: () => {}, onRemoved: ev(), onUpdated: tabsUpdated },
    cookies: { getAll: async () => [{ name: "sid", value: "s3cr3t" }] },
    webRequest: { onResponseStarted: wr },
  };
  class WS {
    constructor() { setImmediate(() => this.onopen && this.onopen()); }
    send(r) { const m = JSON.parse(r); sent.push(m); const rep = m.type === "ping" || m.type === "config" ? { ok: true } : hydraReply;
      setImmediate(() => this.onmessage && this.onmessage({ data: JSON.stringify({ ...rep, id: m.id, capture: true, auto_types: "ZIP MP4", dont_start_sites: "" }) })); }
    close() {}
  }
  const fetch = async (url) => {
    const body = BODIES[url];
    if (body === undefined) throw new Error("404 " + url);
    return { ok: true, headers: { get: () => null }, text: async () => body };
  };
  const ctx = vm.createContext({ chrome, navigator: { userAgent: "Chrome/120" }, WebSocket: WS, console,
    setTimeout, clearTimeout, setInterval, clearInterval, setImmediate, fetch, AbortController, URL, URLSearchParams });
  vm.runInContext(SRC, ctx);
  const hdr = (ct, len) => [{ name: "Content-Type", value: ct }].concat(len ? [{ name: "Content-Length", value: String(len) }] : []);
  return {
    sent, tabsUpdated,
    resp: (url, ct, len) => wr.fn({ tabId: 7, url, responseHeaders: hdr(ct, len) }),
    state: () => new Promise((res) => msgs.l[0]({ type: "get-state" }, {}, res)),
    send: (m) => new Promise((res) => msgs.l[0](m, {}, res)),
    badge: () => badge,
  };
}

// ------------------------------------------------------------------ HLS
{
  const h = build(); await tick(6);
  h.resp("https://cdn.ex/hls/master.m3u8?tok=1", "application/vnd.apple.mpegurl");
  await tick(20);
  let st = await h.state();
  check("a played HLS master is detected", st.streams.length === 1, JSON.stringify(st.streams.map((s) => s.url)));
  const s = st.streams[0];
  check("it is listed as one stream, not per segment", s.protocol === "hls" && s.kind === "master");
  check("its variants are read from the manifest", s.variants.length === 3 && s.variants[0].height === 1080 && s.variants[0].bandwidth === 5200000, JSON.stringify(s.variants));
  check("duration/liveness come from a variant playlist", s.duration === 600 && s.live === false, `${s.duration}/${s.live}`);
  check("audio renditions are counted", s.audioTracks === 1);
  check("no DRM on a plain stream", s.drm === null);

  // The player now fetches the variant playlists and the segments.
  h.resp("https://cdn.ex/hls/v8/index.m3u8", "application/x-mpegurl");
  h.resp("https://cdn.ex/hls/v5/index.m3u8", "application/x-mpegurl");
  await tick(16);
  for (let i = 0; i < 40; i++) h.resp(`https://cdn.ex/hls/v8/seg${i}.m4s`, "video/mp4", 4_000_000);
  h.resp("https://cdn.ex/hls/v8/init.mp4", "video/mp4", 900_000);
  await tick(16);
  st = await h.state();
  check("variant playlists fold into their master", st.streams.length === 1, JSON.stringify(st.streams.map((x) => x.key)));
  check("segments never reach the media list", st.media.length === 0, JSON.stringify(st.media));

  // Re-request with a rotated token: still one entry, refreshed URL.
  h.resp("https://cdn.ex/hls/master.m3u8?tok=2", "application/vnd.apple.mpegurl");
  await tick(10);
  st = await h.state();
  check("a rotated auth token does not duplicate the entry", st.streams.length === 1 && st.streams[0].url.endsWith("tok=2"), st.streams[0]?.url);

  // A genuine progressive file on the same page still shows up.
  h.resp("https://files.ex/trailer.mp4", "video/mp4", 700_000_000);
  await tick(10);
  st = await h.state();
  check("direct MP4 sniffing is unchanged", st.media.length === 1 && st.media[0].size === 700_000_000, JSON.stringify(st.media));
  check("the badge counts streams and files together", h.badge() === "2", h.badge());

  // Hand it to Hydra.
  const v720 = s.variants.find((v) => v.height === 720);
  const vid = [v720.id ?? "", v720.url ?? "", v720.bandwidth ?? ""].join("|");
  const r = await h.send({ type: "download-stream", key: s.key, variant: vid });
  await tick(6);
  const req = h.sent.find((m) => m.type === "stream");
  check("Download sends a `stream` task, not a file download", !!req && r.ok, JSON.stringify(h.sent.map((m) => m.type)));
  check("the chosen quality travels with it", req?.variant?.height === 720 && req?.variant_url === "https://cdn.ex/hls/v5/index.m3u8", JSON.stringify(req?.variant));
  check("browser session context is preserved", req?.cookies === "sid=s3cr3t" && req?.referer === "https://page.ex/watch" && req?.tab_url === "https://page.ex/watch" && !!req?.user_agent);
  check("an estimated size is included", req?.size === Math.round((2800000 * 600) / 8), String(req?.size));

  // Navigating away drops the tab's entries.
  h.tabsUpdated.fire(7, { url: "https://page.ex/other" });
  await tick(8);
  st = await h.state();
  check("navigating away clears stale entries", st.streams.length === 0 && st.media.length === 0);
}

// ----------------------------------------------------------------- DASH
{
  const h = build(); await tick(6);
  h.resp("https://cdn.ex/dash/m.mpd", "application/dash+xml");
  await tick(14);
  const st = await h.state();
  const s = st.streams[0];
  check("a DASH manifest is detected", s?.protocol === "dash", JSON.stringify(st.streams));
  check("DASH representations are read", s?.variants.length === 2 && s.variants[0].height === 1440);
  check("DASH audio is counted separately", s?.audioTracks === 1);
  check("DASH duration is parsed from PT10M", s?.duration === 600);
}

// ------------------------------------------------------------------ DRM
{
  const h = build(); await tick(6);
  h.resp("https://drm.ex/premium/master.m3u8", "application/vnd.apple.mpegurl");
  await tick(14);
  const st = await h.state();
  check("a Widevine manifest is flagged, not fetched", st.streams[0]?.drm === "Widevine", JSON.stringify(st.streams[0]));
  const r = await h.send({ type: "download-stream", key: st.streams[0].key, variant: "" });
  check("DRM streams are refused with a clear message", r.ok === false && /Widevine DRM: this stream is protected/.test(r.error), JSON.stringify(r));
  check("nothing was sent to Hydra for it", !h.sent.some((m) => m.type === "stream"));
}

// ------------------------------------------- old Hydra without stream support
{
  const h = build({ hydraReply: { ok: false, error: "unknown type" } }); await tick(6);
  h.resp("https://cdn.ex/hls/master.m3u8?tok=1", "application/vnd.apple.mpegurl");
  await tick(20);
  const st = await h.state();
  const r = await h.send({ type: "download-stream", key: st.streams[0].key, variant: "" });
  check("an older Hydra gets a plain explanation, not a playlist file", r.ok === false && /update Hydra/.test(r.error), JSON.stringify(r));
}

// ------------------------------------------------------------- audio files
// Audio played in a browser was not offered at all. Three separate causes,
// each enough on its own, so each is pinned separately.
{
  const h = build(); await tick(6);

  // 1. The gate regex was far narrower than the label table beside it, so
  //    most audio types were sniffed, named, and then dropped.
  const types = [
    ["https://a.ex/song.mp3", "audio/mpeg", "MP3"],
    ["https://a.ex/track.wav", "audio/wav", "WAV"],
    ["https://a.ex/clip.ogg", "audio/ogg", "OGG"],
    ["https://a.ex/hifi.flac", "audio/flac", "FLAC"],
    ["https://a.ex/voice.opus", "audio/opus", "OPUS"],
    ["https://a.ex/pod.m4a", "audio/mp4", "M4A"],
    ["https://a.ex/old.wma", "audio/x-ms-wma", "WMA"],
    ["https://a.ex/raw.aiff", "audio/x-aiff", "AIFF"],
  ];
  for (const [url, ct] of types) h.resp(url, ct, 5_000_000);
  await tick(12);
  let st = await h.state();
  for (const [url, ct, label] of types) {
    const hit = st.media.find((m) => m.url === url);
    check(`${ct} is offered as a download`, !!hit, JSON.stringify(st.media.map((m) => m.url)));
    check(`${ct} is labelled ${label}`, hit?.kind === label, hit?.kind);
  }
}

{
  const h = build(); await tick(6);
  // 2. `.m4a` is the ordinary AAC file extension, but it was listed as a
  //    stream SEGMENT shape, so every .m4a on the web was discarded.
  h.resp("https://a.ex/audio/episode.m4a", "audio/mp4", 40_000_000);
  await tick(10);
  let st = await h.state();
  check("a .m4a file is a file, not a discarded segment", st.media.length === 1, JSON.stringify(st.media));

  // ...but a real fMP4 segment shape still is one.
  h.resp("https://a.ex/audio/chunk12.m4s", "audio/mp4", 400_000);
  await tick(10);
  st = await h.state();
  check("a .m4s segment is still kept out of the file list", st.media.length === 1, JSON.stringify(st.media));
}

{
  const h = build(); await tick(6);
  // 3. The 256 KB floor is sized for video. A three-minute song clears it,
  //    but a voice note or a short clip does not, and vanished silently.
  h.resp("https://a.ex/voicenote.mp3", "audio/mpeg", 60_000);
  await tick(10);
  let st = await h.state();
  check("a small audio file is still offered", st.media.length === 1, JSON.stringify(st.media));

  // A UI blip is still noise, not a download.
  h.resp("https://a.ex/click.mp3", "audio/mpeg", 4_000);
  await tick(10);
  st = await h.state();
  check("a tiny interface sound is not", st.media.length === 1, JSON.stringify(st.media));

  // Video keeps the higher floor: a 60 KB mp4 is a thumbnail preview.
  h.resp("https://a.ex/preview.mp4", "video/mp4", 60_000);
  await tick(10);
  st = await h.state();
  check("the video floor is unchanged", st.media.length === 1, JSON.stringify(st.media));
}

console.log(fails ? `\n${fails} FAILED` : "\nall passed");
process.exit(fails ? 1 : 0);

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The in-page panel's row list: the wording, order and container choices are
// the contract with the reader, so they are pinned here.
// Run from the repository root:  node extensions/tests/panel.test.mjs
import { readFileSync } from "node:fs";

const src = readFileSync("extensions/chrome/content.js", "utf8");

// Pull the pure list-building functions out of the content script.
const names = ["fmtBytes", "variantId", "pageTitle", "playingSrc", "buildRows"];
let code = "let pageItems = { streams: [], media: [] };\n";
for (const n of names) {
  const i = src.indexOf(`function ${n}(`);
  if (i < 0) throw new Error("missing " + n);
  code += src.slice(i, src.indexOf("\n}\n", i) + 3) + "\n";
}
const api = new Function(
  "document",
  "location",
  code +
    "return { buildRows, pageTitle, playingSrc, set: (p) => (pageItems = p) };"
)({ title: "Free HLS Player - Online M3U8 Player & Stream Tester | Castr" }, { hostname: "castr.com" });

let fails = 0;
const eq = (label, got, want) => {
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g === w) console.log(`ok   ${label}`);
  else {
    fails++;
    console.log(`FAIL ${label}\n  got  ${g}\n  want ${w}`);
  }
};

const TITLE = "Free HLS Player - Online M3U8 Player & Stream Tester Castr";
eq("the page title becomes a filesystem-safe name", api.pageTitle(), TITLE);

const hlsTs = {
  key: "https://cdn.ex/hls/master.m3u8",
  protocol: "hls",
  live: false,
  drm: null,
  segmentType: "ts",
  variants: [
    { id: null, url: "v3.m3u8", width: 1920, height: 1080, bandwidth: 4794000 },
    { id: null, url: "v2.m3u8", width: 854, height: 480, bandwidth: 2060000 },
    { id: null, url: "v1.m3u8", width: 640, height: 360, bandwidth: 1093000 },
  ],
};

api.set({ streams: [hlsTs], media: [] });
eq(
  "TS segments offer both containers, cheapest quality first",
  api.buildRows().map((r) => r.label),
  [
    `${TITLE}, TS file, quality 360p, 1093 kbps`,
    `${TITLE}, MP4 file, quality 360p, 1093 kbps`,
    `${TITLE}, TS file, quality 480p, 2060 kbps`,
    `${TITLE}, MP4 file, quality 480p, 2060 kbps`,
    `${TITLE}, TS file, quality 1080p HD, 4794 kbps`,
    `${TITLE}, MP4 file, quality 1080p HD, 4794 kbps`,
  ]
);
eq(
  "each row carries the variant, container and name to Hydra",
  api.buildRows()[5],
  {
    kind: "stream",
    key: "https://cdn.ex/hls/master.m3u8",
    variant: "|v3.m3u8|4794000",
    container: "MP4",
    filename: TITLE,
    label: `${TITLE}, MP4 file, quality 1080p HD, 4794 kbps`,
  }
);

api.set({ streams: [{ ...hlsTs, segmentType: "fmp4" }], media: [] });
eq(
  "fragmented MP4 is never offered as TS",
  api.buildRows().map((r) => r.container),
  ["MP4", "MP4", "MP4"]
);

api.set({
  streams: [{ ...hlsTs, protocol: "dash", segmentType: "fmp4", variants: hlsTs.variants.slice(0, 1) }],
  media: [],
});
eq("DASH offers MP4 only", api.buildRows().map((r) => r.label), [
  `${TITLE}, MP4 file, quality 1080p HD, 4794 kbps`,
]);

api.set({ streams: [{ ...hlsTs, live: true }], media: [] });
eq("a live stream says so on every row", api.buildRows()[0].label.endsWith(", live"), true);

api.set({ streams: [{ ...hlsTs, drm: "Widevine" }], media: [] });
eq("a DRM stream produces no rows at all", api.buildRows(), []);

api.set({
  streams: [],
  media: [
    { url: "https://cdn.ex/b.mp4", size: 91234496 },
    { url: "https://cdn.ex/a.mp4", size: 48234496 },
  ],
});
eq(
  "direct files are listed too, smallest first",
  api.buildRows().map((r) => r.label),
  [`${TITLE}, MP4 file, 46 MB`, `${TITLE}, MP4 file, 87 MB`]
);
eq("a direct file downloads by URL", api.buildRows()[0].kind, "media");

// ---- a player that is playing offers ITS file, not the page's -----------
{
  const media = [
    { url: "https://a.ex/s/sample-10s.mp3", kind: "MP3", size: 157_000 },
    { url: "https://a.ex/s/sample-12s.mp3", kind: "MP3", size: 201_000 },
    { url: "https://a.ex/s/sample-35s.mp3", kind: "MP3", size: 547_000 },
  ];
  api.set({ streams: [], media });

  // Without a player, the panel still lists what the page holds.
  eq("with nothing playing, every sniffed file is offered", api.buildRows().length, 3);

  // Playing one of them narrows it to that one. A page of samples used to
  // put all twenty-five under the one being played.
  const rows = api.buildRows("https://a.ex/s/sample-12s.mp3");
  eq("a playing file is the only row", rows.length, 1);
  eq("and it is the one being played", rows[0].url, "https://a.ex/s/sample-12s.mp3");
  eq(
    "named by the FILE, since the page title names them all identically",
    rows[0].label,
    "sample-12s.mp3, MP3 file, 196 KB"
  );

  // A query string is not part of identity: signed URLs rotate theirs, and
  // the sniffed entry is the one worth sending.
  const q = api.buildRows("https://a.ex/s/sample-35s.mp3?token=abc");
  eq("a signed URL still matches its sniffed entry", q[0].url, "https://a.ex/s/sample-35s.mp3");

  // Something playing that was never sniffed is still downloadable.
  const un = api.buildRows("https://a.ex/other/voice.wav");
  eq("an unsniffed file is still offered", un.length, 1);
  eq("with its type read off the name", un[0].label, "voice.wav, WAV file");
}

// ---- what counts as "playing a file" ------------------------------------
{
  const el = (src) => ({ currentSrc: src, src: "" });
  api.set({ streams: [{ key: "https://a.ex/hls/master.m3u8", drm: null, variants: [] }], media: [] });

  eq("a plain media URL counts", api.playingSrc(el("https://a.ex/a.mp3")), "https://a.ex/a.mp3");
  // MSE playback — every JS HLS/DASH player — hands back a blob: URL, which
  // is not fetchable; the stream rows are the right answer there.
  eq("a Media-Source blob does not", api.playingSrc(el("blob:https://a.ex/9f2c")), null);
  // Safari plays HLS natively and reports the MANIFEST as currentSrc. That
  // is a stream, not a file, and offering it would save a text playlist.
  eq("a manifest URL does not", api.playingSrc(el("https://a.ex/hls/master.m3u8")), null);
  eq("nor an .mpd", api.playingSrc(el("https://a.ex/d/manifest.mpd")), null);
  eq("nor nothing at all", api.playingSrc(el("")), null);
}

// ---- how the panel presents itself, pinned at the source ---------------

// An autoplaying player is the ordinary case on a live page: the manifest is
// sniffed AFTER playback starts, so by the time the sniffer reports, the
// `play` handler has already run and found nothing to offer. A handler that
// only refreshes an ALREADY-open panel therefore does nothing, and the bar
// stays hidden until a pause and play re-fires `play`.
const changed = src.slice(src.indexOf('msg?.type === "hydra-media-changed"'));
eq(
  "a fresh sniff can open the panel with none already showing",
  /panelTarget \|\| panelMedia\(\)/.test(changed.slice(0, changed.indexOf("\n  }"))),
  true
);

// The video panel and the selection pill are the same product; they share
// one mark and one palette rather than drifting into two house styles.
eq(
  "the brand mark is defined once, not once per panel",
  (src.match(/iVBORw0KGgo/g) || []).length,
  1
);
eq(
  "both the pill and the video panel draw that one mark",
  (src.match(/brandIcon\(\)/g) || []).length,
  3 // the definition, plus one call from each panel
);
for (const shade of ["#1c3a1e", "linear-gradient(#fdfdfd, #e8efe8)", "#9db89f"]) {
  eq(
    `the panel uses the pill's ${shade}`,
    (src.match(new RegExp(shade.replace(/[()#]/g, "\\$&"), "g")) || []).length >= 2,
    true
  );
}

// A single row means the bar ACTS; a dropdown holding one item is a menu
// the user has to open to find the thing they already asked for.
eq(
  "one row makes the bar download instead of opening a list",
  /if \(panelSingle && panelRows\.length === 1\) \{\s*\n\s*sendSingle\(\);/.test(src),
  true
);
eq(
  "and \"Download all\" is not offered for a single file",
  /if \(panelRows\.length > 1\) \{/.test(src),
  true
);

// ---- the bar can be pushed off the play button -------------------------

// A small <audio> element is barely taller than the bar, so anchoring at the
// player's top-left corner puts the bar squarely over the play button. The
// bar must therefore be movable — and moving it must not also download.
{
  const drag = src.slice(src.indexOf("bar.addEventListener(\"pointerdown\""));
  const body = drag.slice(0, drag.indexOf("close.addEventListener"));

  eq(
    "the bar handles a drag at all",
    /pointerdown/.test(body) && /pointermove/.test(body) && /pointerup/.test(body),
    true
  );
  eq(
    "a drag that ends is not also a click",
    /panelDragged = true/.test(body),
    true
  );
  eq(
    "a press below the slop threshold stays a click",
    /DRAG_SLOP/.test(body),
    true
  );
  eq(
    "the pointer is captured, so a fast drag does not escape the bar",
    /setPointerCapture/.test(body),
    true
  );
  eq(
    "the bar does not hide itself out from under a drag",
    /if \(!panelDrag\) schedulePanelHide\(\)/.test(src),
    true
  );

  // The click handler must consume that flag, or the first press AFTER a
  // drag would be swallowed instead of downloading.
  const click = src.slice(src.indexOf("bar.addEventListener(\"click\""));
  eq(
    "the flag is consumed by the next click, not left set",
    /panelDragged = false/.test(click.slice(0, click.indexOf("});"))),
    true
  );
}

// Where it was put is where it stays: a site puts its player in the same
// place on every page, so moving the bar once should be enough.
{
  eq(
    "the position is remembered per origin",
    /panelpos_\$\{location\.origin\}/.test(src),
    true
  );
  eq(
    "and restored before the bar can first appear",
    src.indexOf("loadPanelOffset()") > src.indexOf("function loadPanelOffset"),
    true
  );
}

// A bar dragged off-screen could never be grabbed back.
{
  const place = src.slice(src.indexOf("function placePanel"));
  const body = place.slice(0, place.indexOf("\n}"));
  eq(
    "placement is clamped to the viewport",
    /innerWidth/.test(body) && /innerHeight/.test(body),
    true
  );
  eq(
    "the offset is applied, not the old fixed inset",
    /panelOffset/.test(body),
    true
  );
}

console.log(fails ? `\n${fails} FAILED` : "\nall passed");
process.exit(fails ? 1 : 0);

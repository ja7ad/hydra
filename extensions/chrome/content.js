// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Content script, three jobs:
//  1. Stamp Alt+clicks so the background honors the "hold Alt to let the
//     browser download normally" convention.
//  2. Collect page links for "Download all links with Hydra".
//  3. The selection pill: highlight one or more links and a floating
//     "Download with Hydra" button appears next to the selection.

window.addEventListener(
  "mousedown",
  (e) => {
    if (e.altKey) {
      try {
        chrome.runtime.sendMessage({ type: "alt-click" });
      } catch {
        // Extension reloaded underneath us; harmless.
      }
    }
  },
  true
);

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type === "collect-links") {
    const urls = Array.from(document.querySelectorAll("a[href]"), (a) => a.href).filter(
      (h) => /^https?:/i.test(h)
    );
    sendResponse({ urls });
  }
  return false;
});

// ------------------------------------------------------- selection pill

const URL_RE = /https?:\/\/[^\s<>"')\]]+/g;

function linksInSelection() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || sel.rangeCount === 0) return [];
  const urls = new Set();
  for (let i = 0; i < sel.rangeCount; i++) {
    const range = sel.getRangeAt(i);
    const root = range.commonAncestorContainer;
    const scope = root.nodeType === Node.ELEMENT_NODE ? root : root.parentElement;
    if (!scope) continue;
    for (const a of scope.querySelectorAll("a[href]")) {
      try {
        if (range.intersectsNode(a) && /^https?:/i.test(a.href)) urls.add(a.href);
      } catch {
        // Detached node mid-mutation; skip it.
      }
    }
  }
  // Bare URLs pasted as text count too.
  for (const m of sel.toString().matchAll(URL_RE)) urls.add(m[0]);
  return [...urls];
}

let pillHost = null;
let pillLabel = null;
let pillUrls = [];

// Why a protected stream cannot be fetched.
//
// Phrased as the legal boundary it is rather than a missing feature: "not
// supported" reads as "not supported YET" and leaves people waiting for a
// release that is never coming.
const DRM_WHY =
  "This stream is protected by DRM. Hydra does not bypass the technical measures that protect audio and video content, so it cannot be downloaded.";

// The brand mark, for both the selection pill and the video panel.
//
// An <img> with a data: URI is subject to the PAGE's CSP, so a strict
// img-src blanks it; on failure we swap in an inline SVG arrow, which no
// CSP can block because nothing is fetched.
const BRAND_PNG =
  "data:image/png;base64,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";

function brandIcon() {
  const icon = document.createElement("img");
  icon.className = "icon";
  icon.alt = "";
  icon.src = BRAND_PNG;
  icon.addEventListener("error", () => {
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("class", "icon");
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", "M12 3v12m0 0l-5-5m5 5l5-5M4 20h16");
    path.setAttribute("fill", "none");
    path.setAttribute("stroke", "#1c7a6e");
    path.setAttribute("stroke-width", "2.4");
    path.setAttribute("stroke-linecap", "round");
    path.setAttribute("stroke-linejoin", "round");
    svg.append(path);
    icon.replaceWith(svg);
  });
  return icon;
}

function buildPill() {
  pillHost = document.createElement("div");
  // The page must not style or see into this; a closed shadow root and
  // max z-index keep the pill above and apart.
  const shadow = pillHost.attachShadow({ mode: "closed" });
  pillHost.style.cssText =
    "position:absolute;z-index:2147483647;top:0;left:0;width:0;height:0;overflow:visible;";
  const style = document.createElement("style");
  style.textContent = `
    .pill {
      position: absolute;
      display: flex;
      align-items: center;
      gap: 6px;
      white-space: nowrap;
      font: 12px/1 system-ui, sans-serif;
      color: #1c3a1e;
      background: linear-gradient(#fdfdfd, #e8efe8);
      border: 1px solid #9db89f;
      border-radius: 14px;
      box-shadow: 0 2px 8px rgba(0,0,0,.25);
      padding: 6px 10px;
      cursor: pointer;
      user-select: none;
    }
    .pill:hover { background: linear-gradient(#ffffff, #dcebdd); }
    .icon {
      width: 16px;
      height: 16px;
      flex: 0 0 16px;
      display: block;
      /* The mark is a tall glyph; nudge it onto the text baseline. */
      margin-top: -1px;
    }
    .close {
      font-size: 11px;
      color: #777;
      padding: 2px 4px;
      cursor: pointer;
      border-radius: 8px;
    }
    .close:hover { background: #d6d6d6; color: #333; }
  `;
  const pill = document.createElement("div");
  pill.className = "pill";

  const icon = brandIcon();

  pillLabel = document.createElement("span");
  const close = document.createElement("span");
  close.className = "close";
  close.textContent = "✕";
  pill.append(icon, pillLabel, close);
  shadow.append(style, pill);

  pill.addEventListener("mousedown", (e) => {
    // Keep the selection alive: no default mousedown, no bubbling into the
    // page's own selection handling.
    e.preventDefault();
    e.stopPropagation();
  });
  close.addEventListener("click", (e) => {
    e.stopPropagation();
    hidePill();
  });
  pill.addEventListener("click", (e) => {
    e.stopPropagation();
    if (!pillUrls.length) return hidePill();
    pillLabel.textContent = "Sending…";
    try {
      chrome.runtime.sendMessage(
        { type: "selection-links", urls: pillUrls, referer: location.href },
        (r) => {
          pillLabel.textContent = r && r.ok ? "Sent to Hydra ✓" : "Hydra not reachable";
          setTimeout(hidePill, 1200);
        }
      );
    } catch {
      hidePill();
    }
  });
  document.documentElement.append(pillHost);
  return pill;
}

let pillEl = null;

function hidePill() {
  if (pillEl) pillEl.style.display = "none";
}

function showPill() {
  const urls = linksInSelection();
  if (!urls.length) return hidePill();
  const sel = window.getSelection();
  const rect = sel.getRangeAt(sel.rangeCount - 1).getBoundingClientRect();
  if (!rect || (rect.width === 0 && rect.height === 0)) return hidePill();

  if (!pillEl) pillEl = buildPill();
  pillUrls = urls;
  pillLabel.textContent =
    urls.length === 1 ? "Download with Hydra" : `Download ${urls.length} links with Hydra`;
  pillEl.style.display = "flex";
  // Below the selection's end, clamped to the viewport; above when there is
  // no room underneath.
  const margin = 6;
  let top = window.scrollY + rect.bottom + margin;
  if (rect.bottom + 34 > window.innerHeight) top = window.scrollY + rect.top - 34;
  let left = window.scrollX + Math.min(Math.max(rect.left, 4), window.innerWidth - 240);
  pillEl.style.top = `${top}px`;
  pillEl.style.left = `${left}px`;
}

document.addEventListener("mouseup", (e) => {
  if (pillHost && e.composedPath().includes(pillHost)) return;
  // Let the browser finalize the selection first.
  setTimeout(showPill, 10);
});

document.addEventListener("selectionchange", () => {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed) hidePill();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") hidePill();
});

window.addEventListener("scroll", hidePill, { passive: true });

// ------------------------------------------------- in-page video panel
//
// The IDM habit, and deliberately the same shape: a "Download this video"
// bar rides the top-left of a player while the pointer is over it, and
// hovering it drops a list of everything this page offers — one row per
// variant per container, named from the page title, cheapest quality first.
//
// It lists only what the background sniffer already saw. Nothing here probes
// the page or the network, and a manifest carrying DRM is listed as
// unsupported rather than offered.
//
// The whole thing lives in a closed shadow root at maximum z-index: the page
// can neither style it nor read it, and player chrome cannot cover it.

const PANEL_MIN_W = 200; // ignore decorative and hidden players
const PANEL_MIN_H = 80;
// A default <audio> player is about 300x54 — under the video floor, which is
// why audio pages offered no button at all. Audio has no picture to be big,
// so it only has to be visible and not a one-pixel beacon.
const AUDIO_MIN_W = 100;
const AUDIO_MIN_H = 20;
const PANEL_LINGER_MS = 2200;

let panelHost = null;
let panelEl = null;
let panelTitle = null;
let panelCaret = null;
/// One obvious file to fetch, so the bar acts instead of opening a menu.
let panelSingle = false;
let panelList = null;
let panelTarget = null; // the <video> it is anchored to
let panelRows = [];
let panelTimer = null;
let panelRaf = 0;
let panelEnabled = true;
let panelDismissed = false; // closed by hand: gone until the page reloads
// Where the bar sits relative to the player's top-left corner. The default
// inset puts it over the transport controls of a SMALL player — an <audio>
// element is barely taller than the bar itself — so it has to be movable,
// and once moved it has to stay moved.
let panelOffset = { x: 10, y: 10 };
let panelDrag = null;
// Set when a drag actually moved, so the `click` that follows `pointerup`
// does not also fire a download.
let panelDragged = false;
// Pointer travel, in px, below which a press is still a click. Without it a
// shaky hand turns an ordinary click into a one-pixel drag.
const DRAG_SLOP = 4;
let pageItems = { streams: [], media: [] };

function fmtBytes(n) {
  if (!n) return "";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${u[i]}`;
}

// Must match streamVariantId() in background.js.
function variantId(v) {
  return [v?.id ?? "", v?.url ?? "", v?.bandwidth ?? ""].join("|");
}

/// The page title is the filename, as it is in IDM. Strip what no
/// filesystem accepts and keep it to a sane length.
function pageTitle() {
  const raw = document.title || location.hostname || "video";
  return raw.replace(/[\\/:*?"<>|]/g, " ").replace(/\s+/g, " ").trim().slice(0, 120) || "video";
}

/// One row per variant per offered container, cheapest quality first — the
/// order and wording IDM uses, so the list reads the same way.
/// The file a player is actually playing, if that is a plain download.
///
/// `currentSrc` is what the element resolved to after picking among its
/// <source> children. Media-Source playback — HLS and DASH through a JS
/// player — reports a `blob:` URL, which is not fetchable and means the
/// stream rows are the right answer instead. Safari plays HLS natively and
/// reports the MANIFEST here, which is also a stream, not a file.
function playingSrc(el) {
  if (!el) return null;
  const src = el.currentSrc || el.src || "";
  if (!/^https?:/i.test(src)) return null;
  if (/\.(?:m3u8|mpd)(?:$|[?#])/i.test(src)) return null;
  const bare = (u) => u.split(/[?#]/)[0];
  // A sniffed manifest under this URL means the same thing.
  if (pageItems.streams.some((x) => bare(x.key) === bare(src))) return null;
  return src;
}

function buildRows(playing) {
  const title = pageTitle();
  const rows = [];

  // A player that is playing ONE file has already answered the question, so
  // the panel offers THAT file rather than everything the page holds. On a
  // page of samples the old behaviour listed all twenty-five over the one
  // being played: a menu to search through instead of an answer, and every
  // entry titled identically because they share the page title.
  if (playing) {
    const bare = (u) => u.split(/[?#]/)[0];
    const hit = pageItems.media.find((m) => bare(m.url) === bare(playing));
    const ext = (bare(playing).split(".").pop() || "").toUpperCase();
    const what = hit?.kind || (/^[A-Z0-9]{2,5}$/.test(ext) ? ext : "MP4");
    // The file's own name, not the page title: on a page of samples the
    // page title is the same for all of them and names none of them.
    const named = decodeURIComponent(bare(playing).split("/").pop() || "").trim();
    return [
      {
        kind: "media",
        // The sniffed URL when there is one: it carries whatever query the
        // origin wanted, which a signed URL needs to stay fetchable.
        url: hit?.url || playing,
        label: [named || title, `${what} file`, fmtBytes(hit?.size)]
          .filter(Boolean)
          .join(", "),
      },
    ];
  }

  for (const s of pageItems.streams) {
    if (s.drm) continue;
    // TS segments can be concatenated as-is or remuxed; fragmented MP4 and
    // DASH only make sense as MP4, so offering TS there would be a lie.
    const containers = s.segmentType === "ts" ? ["TS", "MP4"] : ["MP4"];
    const variants = s.variants.length ? s.variants.slice().reverse() : [null];
    for (const v of variants) {
      for (const container of containers) {
        const quality = v?.height ? `quality ${v.height}p${v.height >= 720 ? " HD" : ""}` : null;
        const rate = v?.bandwidth ? `${Math.round(v.bandwidth / 1000)} kbps` : null;
        rows.push({
          kind: "stream",
          key: s.key,
          variant: variantId(v),
          container,
          filename: title,
          label: [title, `${container} file`, quality, rate, s.live ? "live" : null]
            .filter(Boolean)
            .join(", "),
        });
      }
    }
  }

  for (const m of pageItems.media.slice().sort((a, b) => (a.size || 0) - (b.size || 0))) {
    const ext = (m.url.split(/[?#]/)[0].split(".").pop() || "").toUpperCase();
    // What the SERVER said it is beats what the path looks like, and both
    // beat assuming video.
    const what = m.kind || (/^[A-Z0-9]{2,5}$/.test(ext) ? ext : "MP4");
    rows.push({
      kind: "media",
      url: m.url,
      label: [title, `${what} file`, fmtBytes(m.size)].filter(Boolean).join(", "),
    });
  }
  return rows;
}

/// Is this element a player big enough to hang the bar on?
///
/// Audio and video are measured against different floors: the video floor
/// exists to skip decorative and hidden players, and applying it to an
/// <audio> tag — which is a control strip, not a picture — rejected every
/// audio player on the web.
function playerBigEnough(el) {
  const r = el.getBoundingClientRect();
  if (r.bottom < 0 || r.top > window.innerHeight) return false;
  const audio = el.tagName === "AUDIO";
  const w = audio ? AUDIO_MIN_W : PANEL_MIN_W;
  const h = audio ? AUDIO_MIN_H : PANEL_MIN_H;
  return r.width >= w && r.height >= h;
}

/// Where this site's bar was last dragged to.
///
/// Per ORIGIN, because a site puts its player in the same place on every
/// page: having moved the bar off the play button once, the reader should
/// not have to do it again for the next track.
const panelPosKey = () => `panelpos_${location.origin}`;

async function loadPanelOffset() {
  try {
    const k = panelPosKey();
    const got = await chrome.storage.local.get({ [k]: null });
    const v = got?.[k];
    if (v && Number.isFinite(v.x) && Number.isFinite(v.y)) panelOffset = v;
  } catch {
    // Storage unavailable; the default inset is a fine answer.
  }
}

function savePanelOffset() {
  try {
    chrome.storage.local.set({ [panelPosKey()]: panelOffset });
  } catch {
    // Not worth failing a drag over.
  }
}

/// The player the bar should sit on: the biggest one actually on screen.
///
/// Video wins ties against audio regardless of area, because a page with
/// both is a video page with a sound element on it, not the other way round.
function panelMedia() {
  let best = null;
  let bestRank = -1;
  for (const el of document.querySelectorAll("video, audio")) {
    if (!playerBigEnough(el)) continue;
    const r = el.getBoundingClientRect();
    const area = r.width * r.height;
    const rank = (el.tagName === "VIDEO" ? 1e12 : 0) + area;
    if (rank > bestRank) {
      bestRank = rank;
      best = el;
    }
  }
  return best;
}

function sendRow(row, done) {
  const msg =
    row.kind === "stream"
      ? {
          type: "download-stream",
          key: row.key,
          variant: row.variant,
          container: row.container,
          filename: row.filename,
        }
      : { type: "download-url", url: row.url, referer: location.href };
  try {
    chrome.runtime.sendMessage(msg, (r) => done && done(r));
  } catch {
    if (done) done(null);
  }
}

function buildPanel() {
  panelHost = document.createElement("div");
  panelHost.style.cssText =
    "position:absolute;z-index:2147483647;top:0;left:0;width:0;height:0;overflow:visible;";
  const shadow = panelHost.attachShadow({ mode: "closed" });
  const style = document.createElement("style");
  style.textContent = `
    .wrap {
      position: absolute;
      font: 12px/1.35 system-ui, sans-serif;
      opacity: 0;
      transform: translateY(-4px);
      transition: opacity .14s ease, transform .14s ease;
      pointer-events: none;
    }
    .wrap.on { opacity: 1; transform: none; pointer-events: auto; }
    .bar {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      white-space: nowrap;
      color: #1c3a1e;
      background: linear-gradient(#fdfdfd, #e8efe8);
      border: 1px solid #9db89f;
      border-radius: 14px;
      box-shadow: 0 2px 8px rgba(0,0,0,.25);
      padding: 6px 10px;
      cursor: pointer;
      user-select: none;
    }
    .bar:hover { background: linear-gradient(#ffffff, #dcebdd); }
    /* A press downloads, a drag moves: the pointer cursor keeps the primary
       action advertised, and the grab cursor only appears once a drag is
       really under way. touch-action stops a touch-drag from scrolling the
       page instead of moving the bar. */
    .bar { touch-action: none; }
    .wrap.dragging .bar { cursor: grabbing; }
    .wrap.dragging { user-select: none; }
    .title { font-weight: 600; white-space: nowrap; }
    .caret { color: #5c7a5e; font-size: 10px; }
    .btn {
      font-size: 11px;
      color: #777;
      padding: 2px 4px;
      cursor: pointer;
      border-radius: 8px;
    }
    .btn:hover { background: #d6d6d6; color: #333; }
    .icon {
      width: 16px;
      height: 16px;
      flex: 0 0 16px;
      display: block;
      /* The mark is a tall glyph; nudge it onto the text baseline. */
      margin-top: -1px;
    }
    .menu {
      display: none;
      margin-top: 6px;
      min-width: 300px;
      max-width: min(560px, 80vw);
      max-height: 320px;
      overflow-y: auto;
      padding: 4px 0;
      border-radius: 10px;
      color: #1c3a1e;
      background: linear-gradient(#fdfdfd, #f2f6f2);
      border: 1px solid #9db89f;
      box-shadow: 0 6px 18px rgba(0,0,0,.28);
    }
    .wrap.open .menu { display: block; }
    .row {
      display: flex;
      gap: 8px;
      padding: 6px 12px;
      cursor: pointer;
      white-space: nowrap;
    }
    .row:hover { background: #dcebdd; }
    .row .n { color: #5c7a5e; min-width: 16px; text-align: right; }
    .row .t { overflow: hidden; text-overflow: ellipsis; }
    .row.all {
      font-weight: 600;
      border-bottom: 1px solid #c4d6c5;
      margin-bottom: 2px;
      padding-bottom: 8px;
    }
    .row.note { color: #5c7a5e; cursor: default; }
    .row.note:hover { background: transparent; }
  `;

  const wrap = document.createElement("div");
  wrap.className = "wrap";
  const bar = document.createElement("div");
  bar.className = "bar";

  const svg = brandIcon();

  const title = document.createElement("span");
  title.className = "title";
  // Set per show: the same bar serves an <audio> tag, and "Download this
  // video" over a song reads as a bug.
  title.textContent = "Download this video";
  panelTitle = title;
  const caret = document.createElement("span");
  caret.className = "caret";
  caret.textContent = "▾";
  panelCaret = caret;
  const close = document.createElement("span");
  close.className = "btn";
  close.textContent = "✕";
  close.title = "Hide until this page reloads";

  bar.append(svg, title, caret, close);
  panelList = document.createElement("div");
  panelList.className = "menu";
  wrap.append(bar, panelList);
  shadow.append(style, wrap);
  panelEl = wrap;

  // Never let a click reach the player underneath — but in the BUBBLE
  // phase: stopping propagation while capturing would cut the event off
  // before it ever reached the row that was clicked.
  for (const ev of ["mousedown", "click", "dblclick", "pointerdown"]) {
    wrap.addEventListener(ev, (e) => e.stopPropagation());
  }
  wrap.addEventListener("pointerover", () => clearTimeout(panelTimer));
  wrap.addEventListener("pointerleave", () => {
    // Mid-drag the pointer routinely leaves the bar. Hiding then would pull
    // the thing out from under the hand that is holding it.
    if (!panelDrag) schedulePanelHide();
  });
  bar.addEventListener("pointerover", () => {
    if (!panelSingle && !panelDrag) wrap.classList.add("open");
  });

  // Drag to move.
  //
  // The bar is anchored over the top-left of the player, which on a small
  // <audio> element is exactly where the play button is — so it has to be
  // possible to push it aside without dismissing it and losing the download.
  bar.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    panelDrag = {
      id: e.pointerId,
      // The GRAB point, not the bar's corner: anything else makes the bar
      // jump under the cursor on the first movement.
      fromX: e.clientX,
      fromY: e.clientY,
      baseX: panelOffset.x,
      baseY: panelOffset.y,
      moved: false,
    };
    clearTimeout(panelTimer);
    try {
      // Keeps the move and up events coming even when the pointer outruns
      // the bar, which during a fast drag it always does.
      bar.setPointerCapture(e.pointerId);
    } catch {
      // Capture is a nicety, not a requirement.
    }
  });

  bar.addEventListener("pointermove", (e) => {
    if (!panelDrag || e.pointerId !== panelDrag.id) return;
    const dx = e.clientX - panelDrag.fromX;
    const dy = e.clientY - panelDrag.fromY;
    if (!panelDrag.moved && Math.abs(dx) + Math.abs(dy) < DRAG_SLOP) return;
    panelDrag.moved = true;
    // A list unfurling under a bar that is being moved is just noise.
    wrap.classList.remove("open");
    wrap.classList.add("dragging");
    panelOffset = { x: panelDrag.baseX + dx, y: panelDrag.baseY + dy };
    placePanel();
    e.preventDefault();
  });

  const endDrag = (e) => {
    if (!panelDrag || (e && e.pointerId !== panelDrag.id)) return;
    const moved = panelDrag.moved;
    panelDrag = null;
    wrap.classList.remove("dragging");
    if (moved) {
      // Consumed by the `click` that follows a `pointerup`, so putting the
      // bar down somewhere does not also start a download.
      panelDragged = true;
      savePanelOffset();
      schedulePanelHide();
    }
  };
  bar.addEventListener("pointerup", endDrag);
  bar.addEventListener("pointercancel", endDrag);

  bar.addEventListener("click", (e) => {
    e.preventDefault();
    // The tail of a drag, not a press.
    if (panelDragged) {
      panelDragged = false;
      return;
    }
    // Pressing Download on a playing file should download that file, not
    // open a list for the user to find it in again.
    if (panelSingle && panelRows.length === 1) {
      sendSingle();
      return;
    }
    wrap.classList.toggle("open");
  });
  close.addEventListener("click", (e) => {
    e.stopPropagation();
    panelDismissed = true;
    hidePanel();
  });

  document.documentElement.append(panelHost);
}

/// `el` is the player the bar is about to sit on — passed in rather than
/// read from `panelTarget`, which showPanel only assigns AFTER this returns.
function renderRows(el) {
  panelList.textContent = "";
  panelRows = buildRows(playingSrc(el));
  const drm = pageItems.streams.filter((s) => s.drm);
  if (!panelRows.length && !drm.length) return false;
  // One row and nothing to explain: the bar downloads it on click, so the
  // dropdown and its caret would be a menu with a single item in it.
  panelSingle = panelRows.length === 1 && !drm.length;
  if (panelCaret) {
    panelCaret.style.display = panelSingle ? "none" : "";
  }

  const mkRow = (text, n, onClick, cls = "") => {
    const row = document.createElement("div");
    row.className = `row ${cls}`.trim();
    const num = document.createElement("span");
    num.className = "n";
    num.textContent = n == null ? "" : `${n}.`;
    const t = document.createElement("span");
    t.className = "t";
    t.textContent = text;
    row.append(num, t);
    if (onClick) {
      row.addEventListener("click", (e) => {
        e.stopPropagation();
        onClick(t);
      });
    }
    panelList.append(row);
    return t;
  };

  if (panelRows.length > 1) {
    mkRow(
      "Download all",
      null,
      (t) => {
        const total = panelRows.length;
        t.textContent = `Sending ${total}...`;
        let left = total;
        let bad = 0;
        for (const r of panelRows) {
          sendRow(r, (rep) => {
            if (!(rep && rep.ok)) bad++;
            if (--left === 0) t.textContent = bad ? `${bad} of ${total} failed` : "Sent";
          });
        }
      },
      "all"
    );
  }

  panelRows.forEach((r, i) => {
    mkRow(r.label, i + 1, (t) => {
      const was = t.textContent;
      t.textContent = "Sending...";
      sendRow(r, (rep) => {
        if (rep && rep.ok) t.textContent = `${was}  - sent`;
        else t.textContent = rep && rep.error ? `${was}  - ${rep.error}` : was;
      });
    });
  });

  for (const s of drm) {
    const t = mkRow(
      `${s.protocol.toUpperCase()} stream - protected by ${s.drm} DRM`,
      null,
      null,
      "note"
    );
    // The row says WHAT; hovering says why, so the short line stays short.
    t.title = DRM_WHY;
  }
  return true;
}

function placePanel() {
  if (!panelTarget || !panelEl) return;
  const r = panelTarget.getBoundingClientRect();
  // The offset is deliberately NOT clamped to the player. An <audio> element
  // is about one bar tall, so getting clear of its controls usually means
  // sitting just outside it. Only the viewport is enforced, so a bar can
  // never be dragged somewhere it cannot be grabbed back from.
  const w = panelEl.offsetWidth || 220;
  const h = panelEl.offsetHeight || 32;
  const clamp = (v, lo, hi) => Math.min(Math.max(v, lo), Math.max(lo, hi));
  const x = clamp(r.left + panelOffset.x, 0, window.innerWidth - w);
  const y = clamp(r.top + panelOffset.y, 0, window.innerHeight - h);
  panelEl.style.left = `${window.scrollX + x}px`;
  panelEl.style.top = `${window.scrollY + y}px`;
}

function showPanel(video) {
  if (!panelEnabled || panelDismissed || !video) return;
  if (!panelHost) buildPanel();
  if (!renderRows(video)) return hidePanel();
  clearTimeout(panelTimer);
  panelTarget = video;
  if (panelTitle) {
    panelTitle.textContent =
      video.tagName === "AUDIO" ? "Download this audio" : "Download this video";
  }
  panelEl.classList.add("on");
  placePanel();
}

function hidePanel() {
  clearTimeout(panelTimer);
  if (panelEl) {
    panelEl.classList.remove("on");
    panelEl.classList.remove("open");
  }
  panelTarget = null;
}

/// Send the one row the bar stands for, reporting on the bar's own label
/// because in single-file mode there is no menu row to report in.
function sendSingle() {
  const r = panelRows[0];
  if (!r || !panelTitle) return;
  const was = panelTitle.textContent;
  panelTitle.textContent = "Sending...";
  clearTimeout(panelTimer);
  sendRow(r, (rep) => {
    if (rep && rep.ok) {
      panelTitle.textContent = "Sent to Hydra";
      schedulePanelHide();
    } else {
      panelTitle.textContent = (rep && rep.error) || was;
    }
  });
}

function schedulePanelHide() {
  clearTimeout(panelTimer);
  panelTimer = setTimeout(hidePanel, PANEL_LINGER_MS);
}

async function refreshPageItems() {
  try {
    const r = await chrome.runtime.sendMessage({ type: "page-media" });
    if (r) {
      pageItems = { streams: r.streams || [], media: r.media || [] };
      panelEnabled = r.videoPanel !== false;
      if (!panelEnabled) hidePanel();
    }
  } catch {
    // Background asleep or extension reloaded; keep what we had.
  }
}

// Hovering a player is the trigger, the way a selection is the pill's.
// Delegated, so players created later are covered without a rescan.
document.addEventListener(
  "pointerover",
  (e) => {
    const v =
      e.target instanceof Element ? e.target.closest("video, audio") : null;
    if (v) {
      if (playerBigEnough(v)) showPanel(v);
    } else if (panelTarget && e.target !== panelHost) {
      schedulePanelHide();
    }
  },
  true
);

// Starting playback deserves a glance even without a hover.
document.addEventListener(
  "play",
  (e) => {
    // HTMLMediaElement, not HTMLVideoElement: an <audio> tag starting to
    // play is exactly as much a download opportunity as a <video> one.
    if (!(e.target instanceof HTMLMediaElement)) return;
    refreshPageItems().then(() => {
      const v = panelMedia();
      if (v) {
        showPanel(v);
        schedulePanelHide();
      }
    });
  },
  true
);

// The sniffer says when this tab's list changed — a manifest usually lands
// while the pointer is already resting on the player.
chrome.runtime.onMessage.addListener((msg) => {
  if (msg?.type === "hydra-media-changed") {
    refreshPageItems().then(() => {
      // An autoplaying player starts BEFORE its manifest is sniffed, so the
      // `play` handler above runs while there is still nothing to offer and
      // leaves `panelTarget` null. Refreshing only an already-open panel
      // therefore did nothing, and the bar stayed hidden until a pause and
      // play re-fired `play` — which is exactly when it appeared. Falling
      // back to the biggest visible player closes that window.
      const fresh = !panelTarget;
      const v = panelTarget || panelMedia();
      if (v) {
        showPanel(v);
        // Shown on its own initiative rather than from a hover, so it
        // fades like the play-triggered one instead of sitting there.
        if (fresh) schedulePanelHide();
      }
    });
  }
  return false;
});

// Keep it glued to the player without a listener storm.
for (const ev of ["scroll", "resize"]) {
  window.addEventListener(
    ev,
    () => {
      if (!panelTarget || panelRaf) return;
      panelRaf = requestAnimationFrame(() => {
        panelRaf = 0;
        placePanel();
      });
    },
    { passive: true, capture: true }
  );
}

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") hidePanel();
});

// A full-screen player owns the screen; the bar is anchored in page
// coordinates and would be stranded behind it.
document.addEventListener("fullscreenchange", () => {
  if (document.fullscreenElement) hidePanel();
});

// Both at load: the sniffed list, and where this site's bar was last put.
// The offset is read BEFORE anything can show the bar, so it never appears
// at the default inset and then jumps to the remembered spot.
loadPanelOffset();
refreshPageItems();

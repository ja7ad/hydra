// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

// Why a protected stream cannot be fetched. The legal boundary, not a gap
// in the feature set — see the same text in content.js and the Rust side.
const DRM_WHY =
  "This stream is protected by DRM. Hydra does not bypass the technical measures that protect audio and video content, so it cannot be downloaded.";

const $ = (id) => document.getElementById(id);

function fmtSize(n) {
  if (!n) return "";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

function nameOf(url) {
  try {
    const p = new URL(url).pathname.split("/").filter(Boolean).pop();
    return decodeURIComponent(p || url);
  } catch {
    return url;
  }
}

function fmtBitrate(bps) {
  if (!bps) return "";
  return bps >= 1e6 ? `${(bps / 1e6).toFixed(1)} Mbps` : `${Math.round(bps / 1e3)} kbps`;
}

function fmtDuration(sec) {
  if (!sec) return "";
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const parts = [Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, "0"));
  return h ? `${h}:${parts.join(":")}` : `${+parts[0]}:${parts[1]}`;
}

// Must match streamVariantId() in background.js: the two contexts name the
// same variant to each other by this string.
function variantId(v) {
  return [v?.id ?? "", v?.url ?? "", v?.bandwidth ?? ""].join("|");
}

/// RFC 6381 codec strings are unreadable; name the ones that actually turn
/// up and pass anything else through rather than hiding it.
function codecName(codecs) {
  if (!codecs) return "";
  const names = [];
  for (const raw of String(codecs).split(",")) {
    const c = raw.trim().toLowerCase();
    if (/^(avc1|avc3)/.test(c)) names.push("H.264");
    else if (/^(hvc1|hev1)/.test(c)) names.push("H.265");
    else if (/^av01/.test(c)) names.push("AV1");
    else if (/^(vp09|vp9)/.test(c)) names.push("VP9");
    else if (/^vp8/.test(c)) names.push("VP8");
    else if (/^mp4a\.40/.test(c)) names.push("AAC");
    else if (/^(ec-3|ac-3)/.test(c)) names.push("Dolby");
    else if (/^opus/.test(c)) names.push("Opus");
    else if (/^flac/.test(c)) names.push("FLAC");
    else if (c) names.push(raw.trim());
  }
  return [...new Set(names)].join(" + ");
}

/// Whether the chosen rendition carries both halves of the programme. DASH
/// keeps them apart, so "video + audio" is a real distinction there.
function trackShape(s, v) {
  const hasVideo = !!(v && (v.height || v.width));
  const hasAudio = s.audioTracks > 0 || (hasVideo && s.protocol === "hls" && !s.audioTracks);
  if (hasVideo && s.audioTracks > 0) return "video + audio";
  if (hasVideo) return "video";
  if (hasAudio) return "audio only";
  return "";
}

function variantLabel(v) {
  const q = v.height ? `${v.height}p` : v.width ? `${v.width}w` : null;
  const rate = fmtBitrate(v.bandwidth);
  return [q, rate].filter(Boolean).join(" · ") || v.id || "default";
}

/// Video bytes at the variant's bitrate. Audio rides along in an HLS
/// muxed variant but is a separate track in DASH, so this reads low there —
/// hence the tilde.
function variantSize(v, duration) {
  return v?.bandwidth && duration ? (v.bandwidth * duration) / 8 : 0;
}

function renderStreams(streams) {
  const list = $("stream-list");
  list.textContent = "";
  $("stream-section").hidden = !streams.length;

  for (const s of streams) {
    const li = document.createElement("li");

    const head = document.createElement("div");
    head.className = "s-head";
    const tag = document.createElement("span");
    tag.className = "tag";
    tag.textContent = s.protocol.toUpperCase();
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = nameOf(s.url);
    name.title = s.url;
    head.append(tag, name);
    if (s.live) {
      const live = document.createElement("span");
      live.className = "tag live";
      live.textContent = "LIVE";
      head.append(live);
    }

    const meta = document.createElement("div");
    meta.className = "s-meta";

    const select = document.createElement("select");
    for (const v of s.variants) {
      const opt = document.createElement("option");
      opt.value = variantId(v);
      opt.textContent = variantLabel(v);
      select.append(opt);
    }
    select.hidden = s.variants.length < 2;

    const btn = document.createElement("button");
    btn.textContent = "Download";

    const describe = () => {
      if (s.drm) {
        meta.className = "s-meta drm";
        meta.textContent = `${s.drm} DRM — protected`;
        meta.title = DRM_WHY;
        return;
      }
      const v = s.variants.find((x) => variantId(x) === select.value) || s.variants[0];
      const bits = [];
      if (v) bits.push(variantLabel(v));
      const codec = codecName(v?.codecs);
      if (codec) bits.push(codec);
      const shape = trackShape(s, v);
      if (shape) bits.push(shape);
      // A live playlist's duration is the sliding window it publishes, not a
      // length anyone can download, so neither it nor a size derived from it
      // means anything here.
      if (s.live) {
        bits.push("live");
      } else {
        if (s.duration) bits.push(fmtDuration(s.duration));
        const size = variantSize(v, s.duration);
        if (size) bits.push(`~${fmtSize(size)}`);
      }
      if (s.encryption) bits.push(s.encryption);
      meta.className = "s-meta";
      meta.textContent = bits.join(" · ") || new URL(s.url).hostname;
    };
    describe();
    select.addEventListener("change", describe);

    if (s.drm) {
      btn.disabled = true;
      btn.title = DRM_WHY;
    } else {
      btn.addEventListener("click", async () => {
        btn.disabled = true;
        btn.textContent = "Sending…";
        const r = await chrome.runtime.sendMessage({
          type: "download-stream",
          key: s.key,
          variant: select.value,
        });
        btn.textContent = r && r.ok ? "Sent" : "Failed";
        if (!(r && r.ok)) {
          $("hint").textContent = r?.error ? `Stream: ${r.error}.` : "Hydra is not reachable.";
          btn.disabled = false;
          setTimeout(() => (btn.textContent = "Download"), 1500);
        }
      });
    }

    const pick = document.createElement("div");
    pick.className = "s-pick";
    pick.append(select, btn);
    li.append(head, meta, pick);
    list.append(li);
  }
}

async function refresh() {
  const { state, media, streams, tabUrl, connected } = await chrome.runtime.sendMessage({
    type: "get-state",
  });
  $("enabled").checked = state.enabled;
  $("video-panel").checked = state.videoPanel !== false;

  // Liveness: the live WebSocket is the truth; probe only if it looks down
  // (never launches the app).
  const ping = connected ? { ok: true } : await chrome.runtime.sendMessage({ type: "ping" });
  const st = $("status");
  if (ping && ping.ok) {
    st.className = "status on";
    st.title = "Hydra is running";
  } else {
    st.className = "status off";
    st.title = state.hydraSeen
      ? "Hydra is not running (it starts automatically on capture)"
      : "Hydra native host not reachable — run the install script";
  }

  const hint = $("hint");
  if (!state.guiCapture) {
    hint.textContent = "Capture is off in Hydra's Options (Google Chrome unchecked).";
  } else if (!(ping && ping.ok) && !state.hydraSeen) {
    hint.textContent = "Install the native host: scripts/install-native-host.sh";
  } else {
    hint.textContent = "Alt+click a link to bypass capture once.";
  }

  renderStreams(streams || []);

  const list = $("media-list");
  list.textContent = "";
  $("media-section").hidden = !media.length;
  for (const m of media) {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = nameOf(m.url);
    name.title = m.url;
    const size = document.createElement("span");
    size.className = "size";
    size.textContent = fmtSize(m.size);
    const btn = document.createElement("button");
    btn.textContent = "Download";
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      btn.textContent = "Sent";
      await chrome.runtime.sendMessage({ type: "download-url", url: m.url, referer: tabUrl });
    });
    li.append(name, size, btn);
    list.append(li);
  }
}

$("enabled").addEventListener("change", async (e) => {
  await chrome.runtime.sendMessage({ type: "set-enabled", enabled: e.target.checked });
});

$("video-panel").addEventListener("change", async (e) => {
  await chrome.runtime.sendMessage({ type: "set-video-panel", on: e.target.checked });
});

$("open-hydra").addEventListener("click", async () => {
  await chrome.runtime.sendMessage({ type: "open-hydra" });
  window.close();
});

$("all-links").addEventListener("click", async () => {
  const r = await chrome.runtime.sendMessage({ type: "download-all-links" });
  $("hint").textContent = r && r.ok ? "Links sent to Hydra." : `Could not send links${r?.error ? `: ${r.error}` : ""}.`;
});

refresh();

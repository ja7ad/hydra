// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The welcome page's live connection check. Three outcomes, because they
// need three different answers from the reader:
//
//   app  — the WebSocket (or the host) reached a running Hydra: nothing to do.
//   host — the native messaging host answered but Hydra is closed: also
//          nothing to do, the first capture starts it.
//   none — the browser cannot find the host at all: the install step was
//          skipped or the browser has not restarted since, so the page shows
//          how to fix it.
//
// Safari exposes `browser`; Chromium and Firefox expose `chrome`.
globalThis.chrome ??= globalThis.browser;

const $ = (id) => document.getElementById(id);

const CMD = {
  windows: "powershell -ExecutionPolicy Bypass -File scripts\\install-native-host.ps1",
  unix: "scripts/install-native-host.sh",
};

function isWindows() {
  const p = navigator.userAgentData?.platform || navigator.platform || navigator.userAgent;
  return /win/i.test(p);
}

function setChip(cls, text, title) {
  const chip = $("conn");
  chip.className = `eyebrow ${cls}`;
  chip.title = title || "";
  $("conn-text").textContent = text;
}

async function probe() {
  setChip("pending", "Checking Hydra…");
  $("trouble").hidden = true;

  let status;
  try {
    status = await chrome.runtime.sendMessage({ type: "status" });
  } catch {
    status = null; // service worker gone; treat as unreachable
  }

  if (status?.app) {
    setChip("ok", "Connected to Hydra", "The extension is talking to the running app.");
    return;
  }

  if (status?.host) {
    setChip(
      "warn",
      "Hydra is not running",
      "The browser can reach Hydra; it starts on the first capture."
    );
    return;
  }

  setChip("bad", "Native host not found", "The browser cannot reach the Hydra app.");
  $("fix-cmd").textContent = isWindows() ? CMD.windows : CMD.unix;
  // A snap or Flatpak browser keeps its profile in its own private tree, so
  // "start Hydra once" is the fix rather than anything the user types — but
  // only if the running Hydra is new enough to know those paths.
  const note = $("fix-sandboxed");
  if (note) {
    note.hidden = !/Linux/i.test(navigator.userAgent);
  }
  $("trouble").hidden = false;
}

$("recheck").addEventListener("click", probe);
probe();

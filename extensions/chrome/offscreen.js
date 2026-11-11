// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// The offscreen document that holds the PAC sandbox (pac-sandbox.html). The
// service worker has no DOM to frame it in, and the sandbox has no extension
// APIs to be messaged through, so this page only relays: a runtime message in,
// a postMessage to the frame, the frame's answer back.

const frame = document.getElementById("pac");
// The document's own load waits for the frame's, and unlike the frame's it
// cannot have fired before this script was parsed.
const loaded =
  document.readyState === "complete"
    ? Promise.resolve()
    : new Promise((resolve) => window.addEventListener("load", resolve, { once: true }));
const pending = new Map();
let nextId = 0;

window.addEventListener("message", (event) => {
  if (event.source !== frame.contentWindow) return;
  const answer = pending.get(event.data?.id);
  pending.delete(event.data?.id);
  answer?.(event.data);
});

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.target !== "hydra-pac") return false;
  loaded.then(() => {
    const id = nextId++;
    pending.set(id, sendResponse);
    frame.contentWindow.postMessage({ id, script: msg.script, url: msg.url, host: msg.host }, "*");
  });
  return true;
});

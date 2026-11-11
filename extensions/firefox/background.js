// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Hydra browser integration, Firefox event page. Everything browser-neutral
// is core.js (a copy of extensions/chrome/core.js, loaded before this file
// by the manifest); this file is only the Gecko way of capturing a download.
//
// Gecko cannot park a download. `downloads.pause()` there is
// `download.cancel()`, and `downloads.resume()` is gated on `canResume`,
// which needs `hasPartialData` — false for a download still at byte 0. So a
// download touched on creation and handed back would stay "Canceled" for
// good, and one cancelled after Hydra took it leaves a "Canceled" row in the
// Library that `downloads.erase()` does not remove: erase drops the session
// entry, not the history one.
//
// So the decision is made one step earlier, in a blocking
// `webRequest.onHeadersReceived`, before a download exists at all. A
// response cancelled there leaves nothing behind: no bytes past the headers,
// no row. Firefox holds the response for as long as the answer takes, so a
// closed app can be launched by the native host and still take the file
// before the browser has written a byte of it. `downloads.onCreated` stays
// registered underneath as the net for a transfer that never passed through
// a response we were shown, and pays the cancelled row for it.

// The two capture paths, each standing the other down, in the one direction it
// can arrive from.
//
// A response judged at the header stage and declined, waiting for the download
// the browser is about to make of it. Read once, then forgotten: clicking the
// same link again is a new question. (`browserOwned`, the other direction, is
// core.js's: the Alt bypass sets it from there.)
const headerVerdicts = expiringNotes(15000);
// requestId -> the URL the chain started at, so `captureUrl` still has both.
const redirectOrigins = expiringNotes(60000);

// The request types a browser download can arrive as. An `xmlhttprequest` or
// an `image` carrying a zip belongs to the page that asked for it — cancelling
// those breaks the page and creates no download row to begin with.
const DOWNLOAD_TYPES = ["main_frame", "sub_frame", "object", "other"];

// Content types the browser SHOWS. Cancelling one of these would take a page
// away from the user, so only what is left — octet-stream, application/zip, a
// type with no viewer behind it — counts as a download in the making.
const VIEWABLE_MIME = new Set([
  "application/pdf",
  "application/json",
  "application/xml",
  "application/xhtml+xml",
  "application/javascript",
  "application/x-javascript",
  "application/ecmascript",
  "application/wasm",
  "application/manifest+json",
]);

function willDownload(headers) {
  // `attachment` is the server saying so outright, whatever the type is.
  if (/^\s*attachment\s*(?:;|$)/i.test(headers["content-disposition"] || "")) return true;
  const mime = (headers["content-type"] || "").split(";")[0].trim().toLowerCase();
  // No type at all means the browser sniffs for one, and we would be guessing
  // against it. Let it through; `downloads.onCreated` still sees the result.
  if (!mime) return false;
  return !/^(?:text|image|audio|video|font)\//i.test(mime) && !VIEWABLE_MIME.has(mime);
}

// Just enough of RFC 6266 to get a name — and through it an extension — out of
// a Content-Disposition. The encoded form wins where a server sends both,
// which is the order browsers read them in.
function dispositionName(disp) {
  const encoded = /filename\*\s*=\s*[^']*'[^']*'([^;]+)/i.exec(disp || "");
  if (encoded) {
    try {
      return decodeURIComponent(encoded[1].trim());
    } catch {
      // Malformed percent-escapes: fall through to the plain parameter.
    }
  }
  const plain = /filename\s*=\s*(?:"([^"]*)"|([^;]+))/i.exec(disp || "");
  const name = plain && (plain[1] ?? plain[2]).trim();
  return name || null;
}

// A download link with target="_blank" opens a tab first, and the browser
// closes that tab itself once the response turns out to be a download. A
// response we cancelled never reaches that point, so the blank tab is ours to
// clean up. An opener and a URL that never committed are what separate it from
// the tab a plain link click navigates, which must be left alone.
async function closeBlankTab(tabId) {
  if (tabId == null || tabId < 0) return;
  try {
    const tab = await chrome.tabs.get(tabId);
    if (tab.openerTabId != null && (!tab.url || tab.url === "about:blank")) {
      await chrome.tabs.remove(tabId);
    }
  } catch {
    // Already gone, or not ours to close.
  }
}

/// The capture point: a response that is ABOUT to become a download.
///
/// Blocking a response to decide costs the round-trip to the app, paid only
/// on transfers that were going to be downloads anyway.
function interceptResponse(details) {
  // core.js observes every response too, but nothing promises its listener
  // runs before this one, and the capture below asks for this very route.
  noteProxyInfo(details);
  const headers = Object.fromEntries(
    (details.responseHeaders || []).map((h) => [h.name.toLowerCase(), h.value]),
  );
  if (!willDownload(headers)) return;
  if (browserOwned.get(details.url) !== undefined) return;
  if (details.method === "POST") {
    // Its request is in a body Hydra is never handed; see `notePosted`.
    headerVerdicts.set(details.url);
    return;
  }

  // The same choice `decideCapture` makes, given the same two URLs: a resolved
  // URL that is a short-lived signature is dead by Hydra's first retry, and
  // the link that minted it is not.
  const url = captureUrl({ url: redirectOrigins.get(details.requestId), finalUrl: details.url });

  return offerToHydra({
    url,
    filename: dispositionName(headers["content-disposition"]),
    mime: (headers["content-type"] || "").split(";")[0].trim(),
    size: parseInt(headers["content-length"] || "", 10),
    referer: details.originUrl || details.documentUrl || null,
  }).then((why) => {
    if (!why) {
      if (details.type === "main_frame") closeBlankTab(details.tabId);
      return { cancel: true };
    }
    console.debug(`hydra: left to the browser (${why}) — ${url}`);
    // Keyed by the RESOLVED url, which is what a DownloadItem reports as its
    // own, so the download this response becomes can find the answer.
    headerVerdicts.set(details.url);
    return {};
  });
}

// The net under the header stage: a transfer that never passed through a
// response we were shown — a retry from the downloads panel, a
// `downloads.download()` from another add-on — still gets offered.
chrome.downloads.onCreated.addListener((item) => {
  // Gecko reports the resolved URL as `url` and has no `finalUrl`; reading
  // both keeps this right should that ever change.
  const url = item.finalUrl || item.url;
  // Answered already at the header stage — asking again could reach an app
  // that the first ask has just LAUNCHED, and that second answer is the
  // cancelled row this whole path exists to avoid.
  if (headerVerdicts.take(url) !== undefined) return;
  browserOwned.set(url);
  decideCapture(item, false);
});

// Registered at the top level, synchronously: that is what lets Firefox
// persist a blocking listener across event-page shutdowns and wake this
// script for the response instead of letting it through.
const filter = { urls: ["http://*/*", "https://*/*"], types: DOWNLOAD_TYPES };
chrome.webRequest.onBeforeRedirect.addListener((d) => {
  // First hop wins: what we want is the link the user actually followed.
  if (redirectOrigins.get(d.requestId) === undefined) redirectOrigins.set(d.requestId, d.url);
}, filter);
chrome.webRequest.onHeadersReceived.addListener(interceptResponse, filter, [
  "blocking",
  "responseHeaders",
]);

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Automatic capture, the context menu and the selection pill, driven through
// the real service worker with a mocked browser and a fake Hydra.
// Run from the repository root:  node extensions/tests/capture.test.mjs
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { loadBackground } from "./load.mjs";

const UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

let fails = 0;
const check = (label, cond, extra = "") => {
  if (cond) console.log(`ok   ${label}`);
  else { fails++; console.log(`FAIL ${label} ${extra}`); }
};
const tick = (n = 3) => new Promise((r) => { let i = 0; const f = () => (++i >= n ? r() : setImmediate(f)); setImmediate(f); });

// The PAC evaluator the Chromium sandbox page runs, loaded the same way: a
// classic script whose declarations land on its global.
function pacSandbox() {
  const ctx = vm.createContext({});
  vm.runInContext(readFileSync("extensions/chrome/pac.js", "utf8"), ctx, { filename: "pac.js" });
  return ctx;
}

function build({ hydraReply = { ok: true }, store = {}, gecko = false, ua = UA, tab = null, proxy, proxyStyle = "callback", cookieJar = null, frames = null, fetchImpl = null, offscreen = null } = {}) {
  const sent = [];          // messages that reached "Hydra"
  const calls = [];         // downloads API calls
  const cookieQueries = []; // every cookies.getAll details object
  const listeners = { headerObservers: [] };
  const pacDoc = { open: false, created: [], closed: 0, asked: [], sandbox: null };
  const ev = () => { const l = []; return { addListener: (f) => l.push(f), fire: (...a) => l.map((f) => f(...a)), l }; };

  const storage = () => ({
    async get(def) {
      if (typeof def === "string") return { [def]: store[def] };
      const out = {};
      for (const [k, v] of Object.entries(def)) out[k] = k in store ? store[k] : v;
      return out;
    },
    async set(patch) { Object.assign(store, patch); },
    async remove(keys) { for (const k of [].concat(keys)) delete store[k]; },
  });

  const onCreated = ev(), onDetermining = ev(), onMenu = ev();
  const chrome = {
    runtime: {
      id: "hydra-ext",
      lastError: null,
      onInstalled: ev(), onStartup: ev(),
      onMessage: (listeners.msg = ev()),
      getURL: (p) => `chrome-extension://x/${p}`,
      sendNativeMessage: (_h, _m, cb) => cb(undefined),
      // The offscreen document, as the service worker reaches it: "pac" runs
      // the real sandbox evaluator, "hang" is a script that never returns.
      ...(offscreen
        ? {
            getContexts: async () => (pacDoc.open ? [{ contextType: "OFFSCREEN_DOCUMENT" }] : []),
            sendMessage: async (msg) => {
              pacDoc.asked.push(msg);
              if (!pacDoc.open || msg.target !== "hydra-pac") throw new Error("Receiving end does not exist.");
              if (offscreen === "hang") return new Promise(() => {});
              try {
                return { result: (pacDoc.sandbox ??= pacSandbox()).pacFindProxy(msg.script, msg.url, msg.host) };
              } catch (e) {
                return { error: String(e) };
              }
            },
          }
        : {}),
    },
    ...(offscreen
      ? {
          offscreen: {
            createDocument: async (opts) => {
              if (pacDoc.open) throw new Error("Only a single offscreen document may be created.");
              pacDoc.created.push(opts);
              pacDoc.open = true;
            },
            closeDocument: async () => {
              pacDoc.open = false;
              pacDoc.closed++;
            },
          },
        }
      : {}),
    storage: { local: storage(), session: storage() },
    action: {
      setBadgeBackgroundColor: async () => {}, setBadgeText: async () => {}, setTitle: async () => {},
    },
    downloads: {
      onCreated,
      // Gecko has no onDeterminingFilename at all; its background.js never
      // touches it, and a mock that offered it anyway would hide a Firefox
      // file that had started to.
      ...(gecko ? {} : { onDeterminingFilename: onDetermining }),
      download: async (opts) => calls.push(["download", opts.url]),
      pause: async (id) => calls.push(["pause", id]),
      resume: async (id) => calls.push(["resume", id]),
      cancel: async (id) => calls.push(["cancel", id]),
      erase: async (q) => calls.push(["erase", q.id]),
    },
    contextMenus: { removeAll: async () => {}, create: () => {}, onClicked: onMenu },
    tabs: {
      query: async () => [{ id: 7, url: "https://page.example/watch" }],
      get: async () => tab ?? { id: 7, url: "https://page.example/watch", title: "Sunset Timelapse 4K" },
      remove: async (id) => calls.push(["tabRemove", id]),
      sendMessage: async () => ({ urls: ["https://a.example/1.zip", "https://b.example/2.zip"] }),
      create: () => {}, onRemoved: ev(), onUpdated: ev(),
    },
    cookies: {
      getAll: async (details) => {
        cookieQueries.push(details);
        if (cookieJar) return cookieJar(details);
        // A browser that predates CHIPS rejects the property outright.
        if ("partitionKey" in details) throw new Error("Unexpected property: 'partitionKey'.");
        return [{ name: "sid", value: "abc" }];
      },
    },
    // `webNavigation` is only there when its permission is held.
    ...(frames ? { webNavigation: { getFrame: async ({ frameId }) => frames[frameId] ?? null } } : {}),
    // `proxy` is an OPTIONAL permission, and an ungranted one leaves the API
    // undefined — which is also every Safari build. `undefined` here is that
    // state, and the tests below depend on the difference.
    ...(proxy === undefined
      ? {}
      : {
          proxy: {
            settings: {
              get: (_d, cb) => {
                const got = { value: proxy, levelOfControl: "controlled_by_this_extension" };
                // A promise-only namespace refuses the trailing callback as
                // an unexpected argument rather than ignoring it.
                if (proxyStyle === "promise") {
                  if (cb) throw new Error("Incorrect argument types");
                  return Promise.resolve(got);
                }
                if (proxyStyle === "silent") return undefined; // answers neither way
                return cb(got);
              },
            },
          },
        }),
    webRequest: {
      onResponseStarted: {
        addListener: (fn, filter) => {
          listeners.sniff = fn;
          listeners.sniffFilter = filter;
        },
      },
      onHeadersReceived: {
        addListener: (fn, filter, spec) => {
          if (!spec?.includes("blocking")) return listeners.headerObservers.push(fn);
          listeners.headers = fn;
          listeners.headersFilter = filter;
          listeners.headersSpec = spec;
        },
      },
      onBeforeRedirect: {
        addListener: (fn) => { listeners.redirect = fn; },
      },
      onBeforeRequest: {
        addListener: (fn, filter) => { listeners.request = fn; listeners.requestFilter = filter; },
      },
    },
  };

  class FakeWS {
    constructor() { setImmediate(() => this.onopen && this.onopen()); }
    send(raw) {
      const msg = JSON.parse(raw);
      sent.push(msg);
      const reply = msg.type === "ping" || msg.type === "config"
        ? { ok: true } : hydraReply;
      setImmediate(() => this.onmessage && this.onmessage({
        data: JSON.stringify({ ...reply, id: msg.id, capture: true, auto_types: "ZIP MP4 EXE", dont_start_sites: "*.blocked.example" }),
      }));
    }
    close() {}
  }

  const ctx = vm.createContext({
    chrome, navigator: { userAgent: ua }, WebSocket: FakeWS, console,
    setTimeout, clearTimeout, setInterval, clearInterval, setImmediate, fetch: fetchImpl ?? (async () => { throw new Error("no net"); }),
    // A fresh vm context has the ECMAScript built-ins only; URL and friends
    // are web globals the service worker relies on, so hand them in.
    AbortController, URL, URLSearchParams, TextDecoder, atob,
  });
  loadBackground(ctx, gecko ? "firefox" : "chrome");
  const send = (msg) => new Promise((res) => { listeners.msg.l[0](msg, {}, res); });
  // One response, shaped as chrome.webRequest delivers it — and gated by the
  // registered `types` filter, exactly as the browser gates it. Calling the
  // listener directly would let a test pass against a filter that never
  // delivers the response in the first place.
  const respond = ({ url, mime, size = 4e6, type = "media", tabId = 7, frameId = 0, initiator, documentUrl }) =>
    (listeners.sniffFilter?.types || []).includes(type) &&
    listeners.sniff?.({
      tabId,
      url,
      type,
      frameId,
      initiator,
      documentUrl,
      responseHeaders: [
        { name: "Content-Type", value: mime },
        { name: "Content-Length", value: String(size) },
      ],
    });
  // One response as the browser hands it to a BLOCKING listener, gated by the
  // registered `types` filter the way the browser gates it: a test that called
  // the listener directly would pass against a filter the response never
  // reaches. Returns whatever the listener returns — the blocking verdict.
  const headResponse = ({ url, headers = {}, type = "main_frame", tabId = 7, requestId = "r1", proxyInfo, method = "GET" }) =>
    (listeners.headersFilter?.types || []).includes(type)
      ? listeners.headers?.({
          url, type, tabId, requestId, proxyInfo, method,
          responseHeaders: Object.entries(headers).map(([name, value]) => ({ name, value })),
        })
      : undefined;
  // Any response at all, as Firefox shows it to a non-blocking observer.
  const observed = ({ url, proxyInfo = null, type = "xmlhttprequest" }) =>
    listeners.headerObservers.forEach((fn) => fn({ url, type, tabId: 7, requestId: "o1", proxyInfo }));
  // A request as the browser announces it before sending, gated like the rest.
  const requested = ({ url, method = "GET", type = "sub_frame" }) =>
    (listeners.requestFilter?.types || []).includes(type) &&
    listeners.request?.({ url, method, type, tabId: 7, requestId: "q1" });
  const redirected = ({ url, redirectUrl, requestId = "r1" }) =>
    listeners.redirect?.({ url, redirectUrl, requestId, type: "main_frame", tabId: 7 });

  return {
    sent, calls, onCreated, onDetermining, onMenu, send, store, respond,
    headResponse, redirected, requested, cookieQueries, observed, pacDoc,
    sniffFilter: () => listeners.sniffFilter,
    headersSpec: () => listeners.headersSpec,
  };
}

// ---------------------------------------------------- 1. automatic capture
{
  const h = build();
  await tick(4);
  const item = { id: 1, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip", totalBytes: 5e6, referrer: "https://page.example/" };
  h.onCreated.fire(item);
  await tick(4);
  check("capture: browser download is paused on creation", h.calls.some(([c, id]) => c === "pause" && id === 1), JSON.stringify(h.calls));

  const determined = h.onDetermining.fire(item, () => h.calls.push(["suggest"]));
  // Everything the browser does with a download after this listener answers —
  // reserving the path, and the "Save as" dialog above all — must not happen
  // while Hydra is still being asked. Suggesting here and deciding later put
  // the browser's save dialog on screen next to Hydra's own.
  check("capture: the suggestion is deferred, not answered on the spot", !h.calls.some(([c]) => c === "suggest"), JSON.stringify(h.calls));
  check("capture: the listener returns true to claim the deferral", determined.every((r) => r === true), JSON.stringify(determined));

  await tick(8);
  const dl = h.sent.find((m) => m.type === "download");
  check("capture: suggest() is still called once the decision is in", h.calls.filter(([c]) => c === "suggest").length === 1, JSON.stringify(h.calls));
  check("capture: the browser is released only after its copy is cancelled",
    h.calls.findIndex(([c]) => c === "suggest") > h.calls.findIndex(([c]) => c === "cancel"), JSON.stringify(h.calls));
  check("capture: matching type is handed to Hydra", !!dl, JSON.stringify(h.sent));
  check("capture: the URL is forwarded", dl?.url === "https://cdn.example/pack.zip");
  check("capture: cookies ride along", dl?.cookies === "sid=abc");
  check("capture: referer + size + mime ride along", dl?.referer === "https://page.example/" && dl?.size === 5e6 && dl?.mime === "application/zip");
  check("capture: the request names its browser", dl?.browser === "Google Chrome", JSON.stringify(dl));
  check("capture: browser's copy is cancelled and erased", h.calls.some(([c]) => c === "cancel") && h.calls.some(([c]) => c === "erase"));
  check("capture: it is NOT resumed once Hydra took it", !h.calls.some(([c]) => c === "resume"));
}

// ------------------------------------------- 1b. a download a form POSTed
//
// SharePoint zips a shared folder by posting the file list and its tokens to
// the zip service from a hidden frame. Hydra would replay the URL as a GET
// with no body, and the service answers that with an error.
{
  const h = build();
  await tick(4);
  const zip = "https://northeurope1-mediap.svc.ms/transform/zip?cs=abc";
  h.requested({ url: zip, method: "POST" });
  const item = { id: 11, url: zip, filename: "share.zip", mime: "application/zip", totalBytes: 0 };
  h.onCreated.fire(item);
  await tick(3);
  h.onDetermining.fire(item, () => h.calls.push(["suggest"]));
  await tick(6);
  check("form post: the zip is not sent to Hydra", !h.sent.some((m) => m.type === "download"), JSON.stringify(h.sent));
  check("form post: the browser keeps it", h.calls.some(([c]) => c === "resume"));

  const plain = "https://cdn.example/after.zip";
  h.requested({ url: plain, method: "GET" });
  const next = { id: 12, url: plain, filename: "after.zip", mime: "application/zip", totalBytes: 5e6 };
  h.onCreated.fire(next);
  await tick(3);
  h.onDetermining.fire(next, () => h.calls.push(["suggest"]));
  await tick(6);
  check("form post: a GET download is still captured", h.sent.some((m) => m.type === "download" && m.url === plain));
}
{
  const h = build({ gecko: true });
  await tick(4);
  const zip = "https://northeurope1-mediap.svc.ms/transform/zip?cs=abc";
  const verdict = await h.headResponse({
    url: zip,
    type: "sub_frame",
    method: "POST",
    headers: { "Content-Type": "application/zip", "Content-Disposition": "attachment; filename=share.zip" },
  });
  check("form post (gecko): the response is let through", verdict === undefined, JSON.stringify(verdict));
  h.onCreated.fire({ id: 13, url: zip, filename: "share.zip", mime: "application/zip" });
  await tick(6);
  check("form post (gecko): nothing reaches Hydra", !h.sent.some((m) => m.type === "download"), JSON.stringify(h.sent));
}

// ------------------------------------- 2. a type outside the capture list
{
  const h = build();
  await tick(4);
  const item = { id: 2, url: "https://cdn.example/page.html", filename: "page.html", mime: "text/html" };
  h.onCreated.fire(item); await tick(3);
  h.onDetermining.fire(item, () => h.calls.push(["suggest"])); await tick(6);
  check("passthrough: unlisted type is not sent to Hydra", !h.sent.some((m) => m.type === "download"));
  check("passthrough: the browser download resumes", h.calls.some(([c]) => c === "resume"));
  // A deferred suggestion that never arrives is a download wedged in target
  // determination: no bytes, no dialog, no error.
  check("passthrough: the browser is released to save it itself", h.calls.filter(([c]) => c === "suggest").length === 1, JSON.stringify(h.calls));
}

// ------------------------------------------------------ 3. Alt-click bypass
{
  const h = build({ store: { altTs: Date.now() } });
  await tick(4);
  const item = { id: 3, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" };
  h.onDetermining.fire(item, () => h.calls.push(["suggest"])); await tick(6);
  check("alt bypass: nothing is sent to Hydra", !h.sent.some((m) => m.type === "download"));
  check("alt bypass: the browser keeps the download", h.calls.some(([c]) => c === "resume"));
  check("alt bypass: the browser is released to save it itself", h.calls.filter(([c]) => c === "suggest").length === 1, JSON.stringify(h.calls));
}

// ------------------------------- 3b. the Gecko stand-in for an Alt+click save
//
// `browser.altClickSave` has defaulted to false since Firefox 13, so standing
// aside on Gecko left the user with no download anywhere. The extension saves
// the link itself there; `captureEligible` skips its own downloads, so the
// file cannot bounce straight back into Hydra.
{
  const h = build({ gecko: true });
  await tick(4);
  await h.send({ type: "alt-download", url: "https://cdn.example/pack.zip" });
  check("alt stand-in: the browser is asked to download the link",
    h.calls.some(([c, u]) => c === "download" && u === "https://cdn.example/pack.zip"),
    JSON.stringify(h.calls));
  check("alt stand-in: nothing is sent to Hydra", !h.sent.some((m) => m.type === "download"));

  const refused = await h.send({ type: "alt-download", url: "javascript:alert(1)" });
  check("alt stand-in: a non-http link is refused", refused?.ok === false);
  check("alt stand-in: and never reaches the downloads API",
    h.calls.filter(([c]) => c === "download").length === 1, JSON.stringify(h.calls));
}

// ------------------------- 3c. WeTransfer's service-worker download URL
//
// The page downloads through `/download/local-bartender`, a URL only its own
// service worker answers. Replayed by Hydra it is a 404, so the real link is
// asked of the API with the job the URL carries.
{
  const job = { transferId: "t1", secret: "h4sh", intent: "entire_transfer" };
  const virtual = `https://team.wetransfer.com/download/local-bartender?id=${encodeURIComponent(btoa(JSON.stringify(job)))}`;
  const fetched = [];
  const answer = (status, json) => async (url, init) => {
    fetched.push({ url, init });
    return { ok: status === 200, status, json: async () => json };
  };
  const capture = async (opts) => {
    const h = build(opts);
    await tick(4);
    const item = { id: 9, url: virtual, filename: "a.zip", mime: "application/zip", referrer: "https://team.wetransfer.com/" };
    h.onCreated.fire(item); await tick(3);
    h.onDetermining.fire(item, () => {}); await tick(10);
    return h;
  };

  let h = await capture({ fetchImpl: answer(200, { direct_link: "https://download.wetransfer.com/eu/t1/a.zip?token=x" }) });
  const dl = h.sent.find((m) => m.type === "download");
  check("wetransfer: Hydra gets the API's direct link", dl?.url === "https://download.wetransfer.com/eu/t1/a.zip?token=x", JSON.stringify(dl));
  check("wetransfer: the API is asked for that transfer with its security hash",
    fetched[0]?.url === "https://team.wetransfer.com/api/v4/transfers/t1/download" &&
      JSON.parse(fetched[0]?.init.body).security_hash === "h4sh" &&
      JSON.parse(fetched[0]?.init.body).intent === "entire_transfer", JSON.stringify(fetched));

  h = await capture({ fetchImpl: answer(404, {}) });
  check("wetransfer: a refused API call falls back to the captured URL",
    h.sent.find((m) => m.type === "download")?.url === virtual);

  h = await capture({ fetchImpl: answer(200, { direct_link: "http://evil.example/a.zip" }) });
  check("wetransfer: a non-https direct link is not trusted",
    h.sent.find((m) => m.type === "download")?.url === virtual);

  fetched.length = 0;
  h = build({ fetchImpl: answer(200, { direct_link: "https://x.example/" }) });
  await tick(4);
  const plain = { id: 10, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" };
  h.onCreated.fire(plain); await tick(3);
  h.onDetermining.fire(plain, () => {}); await tick(10);
  check("wetransfer: other downloads never touch the API", fetched.length === 0, JSON.stringify(fetched));
}

// ------------------------------------------------------- 4. excluded site
{
  const h = build();
  await tick(12); // let the config round-trip land the site list first
  const item = { id: 4, url: "https://dl.blocked.example/pack.zip", filename: "pack.zip", mime: "application/zip" };
  h.onDetermining.fire(item, () => {}); await tick(8);
  check("skip list: an excluded site is left to the browser", !h.sent.some((m) => m.type === "download") && h.calls.some(([c]) => c === "resume"), JSON.stringify({sent:h.sent.map(m=>m.type), calls:h.calls}));
}

// ------------------------------------------------- 5. Hydra says no / down
{
  const h = build({ hydraReply: { ok: false, error: "nope" } });
  await tick(4);
  const item = { id: 5, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" };
  h.onDetermining.fire(item, () => {}); await tick(8);
  check("fallback: a refused hand-off resumes the browser download", h.calls.some(([c]) => c === "resume"));
  check("fallback: the paused copy is not cancelled", !h.calls.some(([c]) => c === "cancel"));
}

// ------------------------------------------------ 5b. the Gecko capture path
//
// `downloads.pause()` on Gecko is `download.cancel()`, and `resume()` needs
// partial data a download at byte 0 has none of. So nothing there may be
// parked speculatively: a download the extension touches and hands back is
// stuck "Canceled" for good.
{
  const h = build({ gecko: true });
  await tick(12);
  const item = { id: 51, url: "https://cdn.example/photo.png", filename: "photo.png", mime: "image/png" };
  h.onCreated.fire(item); await tick(8);
  check("gecko: an unlisted type is never touched",
    h.calls.length === 0 && !h.sent.some((m) => m.type === "download"), JSON.stringify(h.calls));
}
{
  const h = build({ gecko: true });
  await tick(12);
  const item = { id: 52, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip", totalBytes: 5e6 };
  h.onCreated.fire(item); await tick(10);
  check("gecko: a matching type reaches Hydra without being paused",
    h.sent.some((m) => m.type === "download") && !h.calls.some(([c]) => c === "pause"), JSON.stringify(h.calls));
  check("gecko: the browser's copy is cancelled once Hydra has it",
    h.calls.some(([c]) => c === "cancel") && h.calls.some(([c]) => c === "erase"));
}
{
  const h = build({ gecko: true, hydraReply: { ok: false, error: "nope" } });
  await tick(12);
  const item = { id: 53, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" };
  h.onCreated.fire(item); await tick(10);
  check("gecko: a refused hand-off leaves the browser download running",
    !h.calls.some(([c]) => c === "cancel") && !h.calls.some(([c]) => c === "resume"), JSON.stringify(h.calls));
}

// ------------------------------- 5c. the Gecko capture point: before a download
//
// Cancelling a download on Gecko leaves a "Canceled" row in the browser's own
// list for every file Hydra takes, and nothing can erase the browser's memory
// of it. So the decision is made one event earlier, off the response headers:
// a response cancelled there never becomes a download at all.
{
  const h = build({ gecko: true });
  await tick(12);
  check("gecko headers: the listener is registered as blocking",
    (h.headersSpec() || []).includes("blocking"), JSON.stringify(h.headersSpec()));

  const verdict = await h.headResponse({
    url: "https://cdn.example/pack.zip",
    headers: {
      "Content-Type": "application/zip",
      "Content-Length": "5000000",
      "Content-Disposition": 'attachment; filename="pack.zip"',
    },
  });
  check("gecko headers: a matching response is cancelled before it downloads",
    verdict?.cancel === true, JSON.stringify(verdict));
  const dl = h.sent.find((m) => m.type === "download");
  check("gecko headers: it reaches Hydra with name, size and mime",
    dl?.url === "https://cdn.example/pack.zip" && dl?.filename === "pack.zip" &&
      dl?.size === 5e6 && dl?.mime === "application/zip",
    JSON.stringify(dl));
  check("gecko headers: no download record is created, so none is cancelled",
    h.calls.length === 0, JSON.stringify(h.calls));
}

// What the browser SHOWS is not a download, and cancelling it would take the
// page away from the user.
{
  const h = build({ gecko: true });
  await tick(12);
  const page = await h.headResponse({
    url: "https://page.example/index.html",
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });
  const pdf = await h.headResponse({
    url: "https://cdn.example/manual.pdf",
    headers: { "Content-Type": "application/pdf" },
  });
  check("gecko headers: a page the browser renders is left alone", page === undefined);
  check("gecko headers: a PDF the built-in viewer opens is left to the viewer", pdf === undefined);
  check("gecko headers: neither is offered to Hydra", !h.sent.some((m) => m.type === "download"));
}

// A response Hydra refuses becomes a download — and that download must not
// ask the same question again: between the two asks the native host may have
// LAUNCHED the app, and the second answer is the cancelled row we avoided.
{
  const h = build({ gecko: true, hydraReply: { ok: false, error: "nope" } });
  await tick(12);
  const verdict = await h.headResponse({
    url: "https://cdn.example/pack.zip",
    headers: { "Content-Type": "application/zip" },
  });
  check("gecko headers: a refused response is not cancelled", !verdict?.cancel, JSON.stringify(verdict));
  const asked = h.sent.filter((m) => m.type === "download").length;
  h.onCreated.fire({ id: 61, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" });
  await tick(10);
  check("gecko headers: the download it becomes does not ask a second time",
    h.sent.filter((m) => m.type === "download").length === asked,
    JSON.stringify(h.sent.map((m) => m.type)));
  check("gecko headers: and is never cancelled",
    !h.calls.some(([c]) => c === "cancel"), JSON.stringify(h.calls));
}

// The name lives in Content-Disposition when the URL carries no extension —
// which is most "download.php?id=" links.
{
  const h = build({ gecko: true });
  await tick(12);
  const verdict = await h.headResponse({
    url: "https://cdn.example/download?id=884",
    headers: {
      "Content-Type": "application/octet-stream",
      "Content-Disposition": "attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.zip",
    },
  });
  check("gecko headers: an RFC 5987 filename supplies the extension the URL lacks",
    verdict?.cancel === true &&
      h.sent.find((m) => m.type === "download")?.filename === "résumé.zip",
    JSON.stringify(h.sent));
}

// The Alt bypass saves the link through `downloads.download()` on Gecko. The
// header stage cannot see who asked, so cancelling here would defeat the very
// bypass the download exists to honour.
{
  const h = build({ gecko: true });
  await tick(12);
  await h.send({ type: "alt-download", url: "https://cdn.example/pack.zip" });
  const verdict = await h.headResponse({
    url: "https://cdn.example/pack.zip",
    headers: { "Content-Type": "application/zip" },
  });
  check("gecko headers: the Alt bypass's own download survives",
    !verdict?.cancel && !h.sent.some((m) => m.type === "download"), JSON.stringify(verdict));
}

// The net runs the other way round too. "Save Link As" and a retry from the
// downloads panel create the download BEFORE the response comes back, so the
// header stage would otherwise offer the same transfer a second time and Hydra
// would queue it twice.
{
  const h = build({ gecko: true });
  await tick(12);
  h.onCreated.fire({ id: 71, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip" });
  await tick(10);
  check("gecko: a download the browser started first still reaches Hydra",
    h.sent.filter((m) => m.type === "download").length === 1, JSON.stringify(h.sent.map((m) => m.type)));
  const verdict = await h.headResponse({
    url: "https://cdn.example/pack.zip",
    headers: { "Content-Type": "application/zip" },
  });
  check("gecko: and its response is not offered a second time",
    verdict === undefined && h.sent.filter((m) => m.type === "download").length === 1,
    JSON.stringify(h.sent.map((m) => m.type)));
}

// A redirect to a signature good for ten seconds: our copy of it is dead by
// Hydra's first retry, and only the link that minted it can be replayed.
{
  const h = build({ gecko: true });
  await tick(12);
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\.\d+/, "");
  const signed = `https://store.example/blob?X-Amz-Date=${stamp}&X-Amz-Expires=10&X-Amz-Signature=dead`;
  h.redirected({ url: "https://files.example/pack.zip", redirectUrl: signed, requestId: "r9" });
  const verdict = await h.headResponse({
    url: signed,
    requestId: "r9",
    headers: { "Content-Type": "application/zip" },
  });
  check("gecko headers: a short-lived signature is taken, not handed over",
    verdict?.cancel === true &&
      h.sent.find((m) => m.type === "download")?.url === "https://files.example/pack.zip",
    JSON.stringify(h.sent.find((m) => m.type === "download")));
}

// A download link with target="_blank" opens a tab the browser would have
// closed itself once the response turned into a download.
{
  const h = build({ gecko: true, tab: { id: 9, url: "about:blank", openerTabId: 7 } });
  await tick(12);
  await h.headResponse({
    url: "https://cdn.example/pack.zip",
    tabId: 9,
    headers: { "Content-Type": "application/zip" },
  });
  await tick(6);
  check("gecko headers: the blank tab the link opened is closed after all",
    h.calls.some(([c, id]) => c === "tabRemove" && id === 9), JSON.stringify(h.calls));
}

// ------------------------------------------- 6. right-click a link / media
{
  const h = build();
  await tick(4);
  await h.onMenu.fire({ menuItemId: "hydra-link", linkUrl: "https://files.example/a.mp4" }, { url: "https://page.example/" })[0];
  await tick(6);
  const dl = h.sent.find((m) => m.type === "download");
  check("context menu: 'Download with Hydra' sends the link", dl?.url === "https://files.example/a.mp4", JSON.stringify(h.sent));
  check("context menu: page URL travels as referer/tab_url", dl?.referer === "https://page.example/" && dl?.tab_url === "https://page.example/");

  const h2 = build(); await tick(4);
  await h2.onMenu.fire({ menuItemId: "hydra-media", srcUrl: "https://files.example/v.webm" }, { url: "https://page.example/" })[0];
  await tick(6);
  check("context menu: image/video/audio uses srcUrl", h2.sent.find((m) => m.type === "download")?.url === "https://files.example/v.webm");
}

// --------------------------------------------------- 7. the selection pill
{
  const h = build(); await tick(4);
  const one = await h.send({ type: "selection-links", urls: ["https://files.example/only.zip"], referer: "https://page.example/" });
  await tick(2);
  check("selection: a single highlighted link downloads directly", h.sent.some((m) => m.type === "download" && m.url === "https://files.example/only.zip"), JSON.stringify(one));

  const h2 = build(); await tick(4);
  await h2.send({ type: "selection-links", urls: ["https://a.example/1.zip", "https://b.example/2.zip"], referer: "https://page.example/" });
  await tick(2);
  const batch = h2.sent.find((m) => m.type === "links");
  check("selection: several links open the batch box", batch?.urls?.length === 2, JSON.stringify(h2.sent));

  const h3 = build(); await tick(4);
  await h3.send({ type: "download-all-links" });
  await tick(2);
  check("'Download all links' collects the page and batches them", h3.sent.find((m) => m.type === "links")?.urls?.length === 2);
}

// ------------------------------------------------ 8. welcome page's status
{
  const h = build(); await tick(4);
  const st = await h.send({ type: "status" });
  check("welcome: status reports a live app", st?.app === true && st?.host === true, JSON.stringify(st));
}

// ------------------------------------------- N. audio played in the browser
{
  const h = build();
  await tick(4);

  // An <audio> element loads as `media`, but a LINK to an .mp3 — the browser
  // opening its own player, which is what "played in the browser" usually
  // means — is a top-level navigation. Watching only `media` meant those
  // responses never reached the sniffer at all.
  const types = h.sniffFilter()?.types || [];
  check(
    "sniffer: navigations are watched, not just media elements",
    types.includes("main_frame") && types.includes("sub_frame"),
    JSON.stringify(types)
  );

  for (const [mime, url] of [
    ["audio/mpeg", "https://cdn.example/song.mp3"],
    ["audio/wav", "https://cdn.example/take.wav"],
    ["audio/ogg", "https://cdn.example/clip.ogg"],
    ["audio/flac", "https://cdn.example/master.flac"],
    ["audio/x-m4a", "https://cdn.example/voice.m4a"],
    ["audio/opus", "https://cdn.example/note.opus"],
  ]) {
    const t = build();
    await tick(2);
    t.respond({ url, mime, type: "main_frame" });
    await tick(6);
    const list = t.store.media_7 || [];
    check(
      `sniffer: ${mime} played in the tab is caught`,
      list.some((m) => m.url === url),
      JSON.stringify(list)
    );
  }

  // A file server that does not know the type says octet-stream; that is not
  // a reason to lose a song.
  {
    const t = build();
    await tick(2);
    t.respond({ url: "https://files.example/album/track.mp3", mime: "application/octet-stream", type: "main_frame" });
    await tick(6);
    check(
      "sniffer: octet-stream with an audio extension is caught",
      (t.store.media_7 || []).some((m) => m.url.endsWith("track.mp3")),
      JSON.stringify(t.store.media_7)
    );
  }

  // ...but the extension only decides when the TYPE was vague. A real
  // non-media type still wins, so a .zip cannot get in by being named .mp3.
  {
    const t = build();
    await tick(2);
    t.respond({ url: "https://files.example/archive.mp3", mime: "application/zip", type: "main_frame" });
    await tick(6);
    check(
      "sniffer: a real non-media type still wins over the extension",
      (t.store.media_7 || []).length === 0,
      JSON.stringify(t.store.media_7)
    );
  }

  // Stream segments must still be kept out, or a playlist floods the list.
  {
    const t = build();
    await tick(2);
    t.respond({ url: "https://cdn.example/hls/seg9.ts", mime: "video/mp2t" });
    await tick(6);
    check(
      "sniffer: stream segments are still excluded",
      (t.store.media_7 || []).length === 0,
      JSON.stringify(t.store.media_7)
    );
  }

  // A short notification blip is not a download worth offering.
  {
    const t = build();
    await tick(2);
    t.respond({ url: "https://cdn.example/blip.mp3", mime: "audio/mpeg", size: 4096 });
    await tick(6);
    check(
      "sniffer: a tiny UI sound stays out",
      (t.store.media_7 || []).length === 0,
      JSON.stringify(t.store.media_7)
    );
  }
}

// ------------------------------------- 9. naming a captured media file
//
// A CDN that stores its objects under a uuid hands the user a row of
// gibberish to rename by hand; the page title is what IDM gives them and
// what they asked for.
{
  const uuid = "https://cdn.example/1b364999/fc1eced1-6d50-4375-a125-ef65c887d7d5.mp4";
  const h = build();
  await tick(4);
  h.respond({ url: uuid, mime: "video/mp4", size: 109_566_922 });
  await tick(6);
  await h.send({ type: "download-url", url: `${uuid}?e=1&s=abc`, referer: "https://page.example/watch" });
  await tick(2);
  const dl = h.sent.find((m) => m.type === "download");
  check(
    "naming: an opaque CDN name becomes the page title",
    dl?.filename === "Sunset Timelapse 4K.mp4",
    JSON.stringify(dl)
  );
  check("naming: the referer travels with it", dl?.referer === "https://page.example/watch");

  // A file that names itself keeps its name: a page of samples would
  // otherwise put the same title on every one of them.
  const h2 = build();
  await tick(4);
  h2.respond({ url: "https://cdn.example/big_buck_bunny_1080p.mp4", mime: "video/mp4" });
  await tick(6);
  await h2.send({ type: "download-url", url: "https://cdn.example/big_buck_bunny_1080p.mp4" });
  await tick(2);
  check(
    "naming: a file that names itself is left alone",
    !h2.sent.find((m) => m.type === "download")?.filename,
    JSON.stringify(h2.sent)
  );

  // An ordinary link is not media the page played, so the article's title
  // has nothing to do with it.
  const h3 = build();
  await tick(4);
  await h3.send({ type: "download-url", url: "https://files.example/8675309.zip" });
  await tick(2);
  check(
    "naming: an unsniffed link keeps its own name",
    !h3.sent.find((m) => m.type === "download")?.filename,
    JSON.stringify(h3.sent)
  );
}

// ------------------------------- 6. how long the in-page video bar stays up
//
// The bar is put up by the page, but the setting lives with the rest of the
// extension's state, so the content script is told the value along with the
// list it is about to offer.
{
  const h = build();
  await tick(4);
  const fresh = await h.send({ type: "page-media" });
  check("panel timeout: ten seconds until told otherwise", fresh.panelTimeout === 10,
    JSON.stringify(fresh));

  check("panel timeout: a chosen value is stored",
    (await h.send({ type: "set-panel-timeout", seconds: "45" }))?.seconds === 45);
  check("panel timeout: and reaches the page",
    (await h.send({ type: "page-media" })).panelTimeout === 45);

  // Zero is a real answer — leave the bar up until it is dismissed — so it
  // must survive the clamp rather than being read as "unset".
  check("panel timeout: zero means until dismissed",
    (await h.send({ type: "set-panel-timeout", seconds: 0 }))?.seconds === 0);
  check("panel timeout: and survives the round trip",
    (await h.send({ type: "page-media" })).panelTimeout === 0);

  check("panel timeout: a value past the ceiling is clamped",
    (await h.send({ type: "set-panel-timeout", seconds: 99999 }))?.seconds === 600);
  check("panel timeout: a negative one cannot get through",
    (await h.send({ type: "set-panel-timeout", seconds: -5 }))?.seconds === 0);
  // Number("") is 0, which would silently turn an empty field into "never
  // hide it" rather than "I have not chosen".
  check("panel timeout: an empty field falls back to the default",
    (await h.send({ type: "set-panel-timeout", seconds: "" }))?.seconds === 10);
  check("panel timeout: so does nonsense",
    (await h.send({ type: "set-panel-timeout", seconds: "soon" }))?.seconds === 10);
}

// ------------------------- 7. when the two in-page buttons are offered
//
// Both ride the same reply as the list they offer, and both are answers about
// what the PAGE shows, so the master capture switch takes them with it.
{
  const h = build();
  await tick(4);
  const fresh = await h.send({ type: "page-media" });
  check("pill: offered on any selection until told otherwise",
    fresh.selectionPill === "always", JSON.stringify(fresh));

  check("pill: the popup's choice is stored and echoed",
    (await h.send({ type: "set-selection-pill", mode: "multi" }))?.mode === "multi");
  check("pill: and reaches the page",
    (await h.send({ type: "page-media" })).selectionPill === "multi");
  check("pill: choosing when it shows leaves the video bar alone",
    (await h.send({ type: "page-media" })).videoPanel === true);

  check("pill: it can be switched off outright",
    (await h.send({ type: "set-selection-pill", mode: "never" }))?.mode === "never" &&
      (await h.send({ type: "page-media" })).selectionPill === "never");

  // The popup renders the stored value straight into a <select>; a value no
  // option carries would show as no choice at all.
  check("pill: a mode nobody offers falls back to the default",
    (await h.send({ type: "set-selection-pill", mode: "sometimes" }))?.mode === "always");
  check("pill: so does a missing one",
    (await h.send({ type: "set-selection-pill" }))?.mode === "always");

  // Capture off means the extension stands aside; a button inviting it back
  // onto every selection would be arguing with that.
  await h.send({ type: "set-enabled", enabled: false });
  const off = await h.send({ type: "page-media" });
  check("pill: capture off takes both in-page buttons with it",
    off.selectionPill === "never" && off.videoPanel === false, JSON.stringify(off));
  check("pill: and the popup is still shown the mode it will return to",
    (await h.send({ type: "get-state" })).state.selectionPill === "always");
}

// ------------------------------------------- 10. the browser's own proxy
//
// A file behind a tunnel is unreachable by an app that has never heard of the
// tunnel. The proxy travels with the download so Hydra leaves by the same
// door — as THAT download's route, never as a change to Hydra's Options.
{
  // Chromium, one proxy for everything.
  const h = build({
    proxy: { mode: "fixed_servers", rules: { singleProxy: { scheme: "socks5", host: "127.0.0.1", port: 10808 } } },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  const dl = h.sent.find((m) => m.type === "download");
  check("proxy: a fixed proxy rides along with the capture",
    dl?.proxy === "socks5://127.0.0.1:10808", JSON.stringify(dl?.proxy));
}
{
  // Per-protocol rules: an https download must take the https entry, not
  // whatever the http one happens to be.
  const h = build({
    proxy: {
      mode: "fixed_servers",
      rules: {
        proxyForHttp: { scheme: "http", host: "plain.example", port: 3128 },
        proxyForHttps: { scheme: "http", host: "secure.example", port: 8888 },
        bypassList: ["<local>", "*.intranet.example"],
      },
    },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: the entry for the URL's own scheme is the one that is sent",
    h.sent.at(-1)?.proxy === "http://secure.example:8888", JSON.stringify(h.sent.at(-1)?.proxy));

  await h.send({ type: "download-url", url: "http://cdn.example/pack.zip" });
  check("proxy: and http takes the http entry",
    h.sent.at(-1)?.proxy === "http://plain.example:3128", JSON.stringify(h.sent.at(-1)?.proxy));

  // A bypassed host is one the BROWSER fetches directly; sending it through
  // the proxy anyway routes intranet traffic to a proxy that will refuse it.
  await h.send({ type: "download-url", url: "https://files.intranet.example/pack.zip" });
  check("proxy: a bypassed host is handed over with no proxy at all",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://fileserver/pack.zip" });
  check("proxy: <local> covers the dotless names an intranet uses",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // Chromium states no port for a proxy on its scheme's default. Hydra's
  // parser defaults 8080 for HTTP, so leaving it unsaid makes the two
  // disagree about what `http://proxy` means.
  const h = build({
    proxy: { mode: "fixed_servers", rules: { singleProxy: { scheme: "http", host: "proxy.example" } } },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: a port the browser left implicit is spelled out",
    h.sent.at(-1)?.proxy === "http://proxy.example:80", JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // "system" is the browser saying it follows the machine and will not say
  // what that is. That, and going direct, leave the download on whatever
  // Hydra's Options say.
  for (const mode of ["system", "direct", "auto_detect"]) {
    const h = build({ proxy: { mode, rules: { singleProxy: { scheme: "socks5", host: "h", port: 1 } } } });
    await tick(4);
    await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
    check(`proxy: "${mode}" names nothing we can pass on`,
      h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  }
}
// ZeroOmega's Auto Switch profile, as Chromium holds it: a generated PAC
// script that picks a profile per host and ends in a FindProxyForURL answer.
const SWITCHER_PAC = `
var FindProxyForURL = function(init, profiles) {
  return function(url, host) {
    "use strict";
    var result = init, scheme = url.substr(0, url.indexOf(":"));
    do {
      if (!profiles[result]) return result;
      result = profiles[result];
      if (typeof result === "function") result = result(url, host, scheme);
    } while (typeof result !== "string" || result.charCodeAt(0) === 43);
    return result;
  };
}("+auto switch", {
  "+auto switch": function(url, host, scheme) {
    "use strict";
    if (/(?:^|\\.)blocked\\.example$/.test(host)) return "+proxy";
    if (shExpMatch(host, "*.tunnel.example")) return "+socks";
    return "DIRECT";
  },
  "+proxy": function(url, host, scheme) {
    "use strict";
    return "PROXY 127.0.0.1:8080; DIRECT";
  },
  "+socks": function(url, host, scheme) {
    "use strict";
    return "SOCKS5 127.0.0.1:1080; SOCKS 127.0.0.1:1080";
  }
});`;
{
  // ZeroOmega's Auto Switch in Chromium: the browser's setting is a PAC
  // script, and what it resolves to depends on the URL.
  const h = build({ offscreen: "pac", proxy: { mode: "pac_script", pacScript: { data: SWITCHER_PAC, mandatory: true } } });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.blocked.example/pack.zip" });
  check("pac: a host the switcher routes through its proxy takes that proxy",
    h.sent.at(-1)?.proxy === "http://127.0.0.1:8080", JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://files.tunnel.example/pack.zip" });
  check("pac: a SOCKS5 profile is handed over as socks5",
    h.sent.at(-1)?.proxy === "socks5://127.0.0.1:1080", JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://open.example/pack.zip" });
  check("pac: a host the script sends DIRECT gets no proxy",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  check("pac: one offscreen document serves every capture",
    h.pacDoc.created.length === 1 && h.pacDoc.created[0].reasons.includes("IFRAME_SCRIPTING"),
    JSON.stringify(h.pacDoc.created));
  // Chromium hands the script only the origin of an https URL; a rule keyed
  // on a path must not see one here that the browser would not show it.
  check("pac: an https URL reaches the script as its origin only",
    h.pacDoc.asked[0]?.url === "https://cdn.blocked.example/" && h.pacDoc.asked[0]?.host === "cdn.blocked.example",
    JSON.stringify(h.pacDoc.asked[0]));

  check("pac: the popup is told the route for the page in front",
    (await h.send({ type: "proxy-status" }))?.spec === null);
}
{
  // ZeroOmega's PAC profile points at a URL rather than inlining the script.
  const fetchImpl = async (url) => ({ ok: url === "http://pac.example/proxy.pac", text: async () => SWITCHER_PAC });
  const h = build({ offscreen: "pac", fetchImpl, proxy: { mode: "pac_script", pacScript: { url: "http://pac.example/proxy.pac" } } });
  await tick(4);
  await h.send({ type: "download-url", url: "http://www.blocked.example/pack.zip" });
  check("pac: a script fetched by URL is run the same way",
    h.sent.at(-1)?.proxy === "http://127.0.0.1:8080", JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // A script that throws, or one with no FindProxyForURL, is not a proxy.
  for (const data of ["throw new Error('boom')", "var nothing = 1;"]) {
    const h = build({ offscreen: "pac", proxy: { mode: "pac_script", pacScript: { data } } });
    await tick(4);
    await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
    check(`pac: a broken script leaves Hydra to decide (${data})`,
      h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  }
}
{
  // A script that never returns: the capture still goes out, without a
  // proxy, and the stuck document is discarded for the next one.
  const h = build({ offscreen: "hang", proxy: { mode: "pac_script", pacScript: { data: SWITCHER_PAC } } });
  await tick(4);
  const sent = h.send({ type: "download-url", url: "https://cdn.blocked.example/pack.zip" });
  const raced = await Promise.race([sent.then(() => "answered"), new Promise((r) => setTimeout(() => r("hung"), 6000))]);
  check("pac: a script that never answers does not park the download",
    raced === "answered" && h.sent.at(-1)?.proxy === null, JSON.stringify([raced, h.sent.at(-1)?.proxy]));
  check("pac: and the stuck document is closed", h.pacDoc.closed === 1, String(h.pacDoc.closed));
}
{
  // A browser with no offscreen API (Firefox reports PAC as autoConfig and
  // never reaches this) has nowhere to run the script: no proxy known.
  const h = build({ proxy: { mode: "pac_script", pacScript: { data: SWITCHER_PAC } } });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.blocked.example/pack.zip" });
  check("pac: without an offscreen document the script is not run",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
}

const FIREFOX_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:140.0) Gecko/20100101 Firefox/140.0";
{
  // ZeroOmega in Firefox routes each request from `proxy.onRequest`; the
  // settings still say "system", and only the response says where it went.
  const h = build({ gecko: true, ua: FIREFOX_UA, proxy: { proxyType: "system" } });
  await tick(4);
  h.observed({ url: "https://cdn.example/page.js", proxyInfo: { type: "socks", host: "127.0.0.1", port: 1080, proxyDNS: true, username: "u" } });
  h.observed({ url: "https://open.example/page.js", proxyInfo: { type: "direct", host: "", port: -1 } });
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("firefox route: the proxy a response from that origin took is sent",
    h.sent.at(-1)?.proxy === "socks5://127.0.0.1:1080", JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://open.example/pack.zip" });
  check("firefox route: an origin reached directly gets no proxy",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://never-seen.example/pack.zip" });
  check("firefox route: an origin never seen is left to Hydra",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "http://cdn.example/pack.zip" });
  check("firefox route: another scheme is another origin",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));

  // The route changes when the switcher's rules do; the latest one wins.
  h.observed({ url: "https://cdn.example/other.js", proxyInfo: { type: "http", host: "::1", port: 3128 } });
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("firefox route: the newest observation wins, IPv6 bracketed",
    h.sent.at(-1)?.proxy === "http://[::1]:3128", JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // The download's own response, at the blocking header stage, is the first
  // the extension sees from that origin — the capture it triggers must use it.
  const h = build({ gecko: true, ua: FIREFOX_UA, proxy: { proxyType: "none" } });
  await tick(4);
  const verdict = h.headResponse({
    url: "https://dl.example/pack.zip",
    headers: { "content-type": "application/zip", "content-length": "5000000" },
    proxyInfo: { type: "socks4", host: "10.0.0.1", port: 1081, proxyDNS: false },
  });
  await verdict;
  const dl = h.sent.find((m) => m.type === "download");
  check("firefox route: the download's own response names its proxy",
    dl?.proxy === "socks4://10.0.0.1:1081", JSON.stringify(dl?.proxy));
}
{
  // Off means off: without the permission the observation is never used.
  const h = build({ gecko: true, ua: FIREFOX_UA });
  await tick(4);
  h.observed({ url: "https://cdn.example/page.js", proxyInfo: { type: "http", host: "proxy.example", port: 8080 } });
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("firefox route: without the permission nothing is passed on",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // Manual settings still decide on their own, bypass list included.
  const h = build({ gecko: true, ua: FIREFOX_UA, proxy: { proxyType: "manual", http: "proxy.example:3128", passthrough: ".corp.example" } });
  await tick(4);
  h.observed({ url: "https://files.corp.example/x", proxyInfo: { type: "http", host: "other.example", port: 1 } });
  await h.send({ type: "download-url", url: "https://files.corp.example/pack.zip" });
  check("firefox route: manual settings with a bypass are not second-guessed",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // Firefox's own shape, and the SOCKS fallback for a scheme its per-protocol
  // fields do not cover.
  const h = build({
    gecko: true,
    proxy: { proxyType: "manual", socks: "127.0.0.1:9050", socksVersion: 5, passthrough: "localhost, .corp.example" },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: Firefox's SOCKS entry carries a scheme it has no field for",
    h.sent.at(-1)?.proxy === "socks5://127.0.0.1:9050", JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "https://files.corp.example/pack.zip" });
  check("proxy: Firefox's passthrough list is a bypass list",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // Firefox stores a bare `host:port`, but its own documentation stores
  // `http://proxy.org:8080`. A scheme the value states wins over the field.
  const h = build({
    gecko: true,
    proxy: { proxyType: "manual", ssl: "http://secure.example:8080", socks: "127.0.0.1:9050", socksVersion: 4 },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: an https download prefers the ssl entry over SOCKS",
    h.sent.at(-1)?.proxy === "http://secure.example:8080", JSON.stringify(h.sent.at(-1)?.proxy));
  await h.send({ type: "download-url", url: "ftp://cdn.example/pack.zip" });
  check("proxy: a scheme with no entry of its own falls back to SOCKS, at its version",
    h.sent.at(-1)?.proxy === "socks4://127.0.0.1:9050", JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // A namespace that answers with a promise and REFUSES the callback: the
  // proxy is still read, rather than a calling convention being reported as
  // "this browser has no proxy".
  const h = build({
    proxyStyle: "promise",
    proxy: { mode: "fixed_servers", rules: { singleProxy: { scheme: "socks5", host: "127.0.0.1", port: 10808 } } },
  });
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: a promise-only settings API is read too",
    h.sent.at(-1)?.proxy === "socks5://127.0.0.1:10808", JSON.stringify(h.sent.at(-1)?.proxy));
}
{
  // A namespace that answers neither way. A capture AWAITS the proxy read,
  // so without a ceiling the download would be parked forever on a question
  // about a proxy — the browser's copy is paused and nothing ever resumes
  // it. The answer after the ceiling is the same one the feature gives when
  // it is switched off.
  const h = build({
    proxyStyle: "silent",
    proxy: { mode: "fixed_servers", rules: { singleProxy: { scheme: "socks5", host: "127.0.0.1", port: 10808 } } },
  });
  await tick(4);
  const sent = h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  const raced = await Promise.race([sent.then(() => "answered"), new Promise((r) => setTimeout(() => r("hung"), 4000))]);
  check("proxy: a settings API that never answers does not park the download",
    raced === "answered" && h.sent.at(-1)?.proxy === null,
    JSON.stringify([raced, h.sent.at(-1)?.proxy]));
}
{
  // The permission IS the switch: ungranted, the API is not there, and the
  // capture is exactly what it was before this feature existed.
  const h = build();
  await tick(4);
  await h.send({ type: "download-url", url: "https://cdn.example/pack.zip" });
  check("proxy: without the optional permission nothing is passed on",
    h.sent.at(-1)?.proxy === null, JSON.stringify(h.sent.at(-1)?.proxy));

  const manifest = JSON.parse(readFileSync("extensions/chrome/manifest.json", "utf8"));
  check("proxy: the permission is optional, so no install is disabled for it",
    (manifest.optional_permissions || []).includes("proxy") &&
      !(manifest.permissions || []).includes("proxy"),
    JSON.stringify(manifest.optional_permissions));
  const safari = JSON.parse(readFileSync("extensions/safari/manifest.json", "utf8"));
  check("proxy: Safari has no proxy API, so it does not ask for one",
    !(safari.optional_permissions || []).includes("proxy"));
  check("pac: the sandbox page is declared, so it may compile the script",
    (manifest.sandbox?.pages || []).includes("pac-sandbox.html") && manifest.permissions.includes("offscreen"),
    JSON.stringify([manifest.sandbox, manifest.permissions]));
  const firefox = JSON.parse(readFileSync("extensions/firefox/manifest.json", "utf8"));
  check("pac: Firefox, with no offscreen documents, is not asked for one",
    !firefox.sandbox && !firefox.permissions.includes("offscreen"));
}

// ------------------------------------------- 7. what the Firefox manifest pins
//
// The capture path above only runs if the manifest loads it AFTER the core it
// reaches into, and the loopback socket only connects if the manifest drops
// Firefox's MV3 default CSP: `upgrade-insecure-requests` there rewrites
// `ws://127.0.0.1` to `wss://`, which the app does not speak, so every
// request fell through to spawning the native host and the toolbar never
// showed the app as running.
{
  const manifest = JSON.parse(readFileSync("extensions/firefox/manifest.json", "utf8"));
  check("firefox manifest: core.js is loaded before background.js",
    JSON.stringify(manifest.background?.scripts) === JSON.stringify(["core.js", "background.js"]),
    JSON.stringify(manifest.background));
  const csp = manifest.content_security_policy?.extension_pages || "";
  check("firefox manifest: the extension CSP does not upgrade the loopback socket",
    /\bscript-src 'self'/.test(csp) && !/upgrade-insecure-requests/.test(csp), JSON.stringify(csp));
  check("firefox manifest: the header stage may block",
    manifest.permissions.includes("webRequestBlocking"));
}

// ------------------------------------ 8. the Referer of an embedded player
//
// A player in an iframe is served by a CDN that checks the Referer against
// the PLAYER's host. The tab's URL is the blog around it, which the CDN
// refuses — so what the file was loaded from has to be the frame, and the
// frame's URL comes from webNavigation when that permission is held.
{
  const frames = { 3: { url: "https://player.example/embed/42", frameId: 3 } };
  const h = build({ frames });
  await tick(4);
  h.respond({ url: "https://cdn.example/v/42.mp4", mime: "video/mp4", size: 9e7, frameId: 3, initiator: "https://player.example" });
  await tick(8);
  await h.send({ type: "download-url", url: "https://cdn.example/v/42.mp4", referer: "https://page.example/watch" });
  const dl = h.sent.filter((m) => m.type === "download").at(-1);
  check("referer: an iframed player's file names the frame, not the tab", dl?.referer === "https://player.example/embed/42", JSON.stringify(dl));

  // Without the permission there is no getFrame; the frame's origin (what
  // Chromium reports as `initiator`) still beats the tab.
  const h2 = build();
  await tick(4);
  h2.respond({ url: "https://cdn.example/v/43.mp4", mime: "video/mp4", size: 9e7, frameId: 3, initiator: "https://player.example" });
  await tick(8);
  await h2.send({ type: "download-url", url: "https://cdn.example/v/43.mp4", referer: "https://page.example/watch" });
  check("referer: without webNavigation the frame's origin is used", h2.sent.filter((m) => m.type === "download").at(-1)?.referer === "https://player.example");

  // A top-level request keeps the tab as before.
  const h3 = build({ frames });
  await tick(4);
  h3.respond({ url: "https://cdn.example/v/44.mp4", mime: "video/mp4", size: 9e7, frameId: 0 });
  await tick(8);
  await h3.send({ type: "download-url", url: "https://cdn.example/v/44.mp4", referer: "https://page.example/watch" });
  check("referer: a top-level file keeps the tab's URL", h3.sent.filter((m) => m.type === "download").at(-1)?.referer === "https://page.example/watch");
}

// ------------------------------------------- 9. partitioned (CHIPS) cookies
//
// A cookie set with `Partitioned` is keyed on the top-level site and never
// comes back from a plain `getAll({url})`. A CDN that answers only with such
// a cookie used to reach Hydra with no cookie at all.
{
  const jar = ({ url, partitionKey }) => {
    if (!partitionKey) return [{ name: "sid", value: "abc" }];
    if (partitionKey.topLevelSite === "https://page.example") return [{ name: "__Host-cdn", value: "p4rt" }];
    return [];
  };
  const h = build({ cookieJar: jar });
  await tick(4);
  const item = { id: 90, url: "https://cdn.example/pack.zip", filename: "pack.zip", mime: "application/zip", totalBytes: 5e6, referrer: "https://page.example/dl" };
  h.onDetermining.fire(item, () => {}); await tick(8);
  const dl = h.sent.find((m) => m.type === "download" && m.url.endsWith("pack.zip"));
  check("chips: the partition of the page the download came from is asked", h.cookieQueries.some((q) => q.partitionKey?.topLevelSite === "https://page.example"), JSON.stringify(h.cookieQueries));
  check("chips: partitioned cookies ride along with the plain ones", dl?.cookies === "sid=abc; __Host-cdn=p4rt", JSON.stringify(dl?.cookies));

  // A browser without the attribute rejects the property; the plain
  // cookies still go, and nothing fails.
  const h2 = build();
  await tick(4);
  h2.onDetermining.fire({ ...item, id: 91 }, () => {}); await tick(8);
  check("chips: an older browser still sends its plain cookies", h2.sent.find((m) => m.type === "download" && m.url.endsWith("pack.zip"))?.cookies === "sid=abc");
}

console.log(fails ? `\n${fails} FAILED` : "\nall passed");
process.exit(fails ? 1 : 0);

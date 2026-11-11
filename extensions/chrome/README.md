# Hydra Extension for Chromium Browsers

**Edge, Brave, Vivaldi, Opera, Arc and Chromium all load this same
directory** — they are Chromium, they use the `chrome-extension://` origin
scheme, and the pinned manifest `key` gives the add-on the identical id in
every one of them, so the native host's allow-list matches without any
per-browser build. Load it from `edge://extensions`, `brave://extensions`
and so on exactly as described below for Chrome. Both native-host installers
already register Edge, Brave, Vivaldi and Chromium alongside Chrome.



Browser integration for [Hydra](../../README.md): automatic
download capture, right-click "Download with Hydra", "Download all links",
a floating "Download with Hydra" button when you highlight links on a page
(single link downloads directly, several open the batch box), per-tab media
sniffing with a badge counter, HLS/DASH stream detection with quality
selection, a floating "Download this video" bar over players, optional hand-over of the
browser's own proxy, and a welcome page on first install.

## How it works

```
                    ┌── WebSocket 127.0.0.1:6799 ─────────────┐   (primary)
extension ──────────┤                                          ├──> hydra-gui
 (this dir)         └── native messaging ──> hydra-host ───────┘   (extbus)
                         (stdio frames)      launches the app     (fallback)
```

- The extension watches `chrome.downloads`. When a download's file type
  matches the capture list, it pauses it, collects the cookies for that URL,
  hands it to Hydra, and only then cancels the browser's copy — if Hydra is
  unreachable the paused download simply resumes in the browser. That
  parking step is Chromium's alone and lives in `background.js`; everything
  browser-neutral (transport, gates, sniffing, menus, the popup's messages)
  is `core.js`, which `background.js` imports. Firefox has its own
  `background.js` with a different capture path — see
  [../firefox/README.md](../firefox/README.md).
- The **WebSocket is the primary transport**: no process spawn per request,
  and the open socket is itself the "app is running" signal.
- `hydra-host` is the fallback, spawned by the browser per request. It
  forwards JSON to the running GUI (authenticated with the token from
  `ipc.json`) and **launches the GUI minimized** when it is not running —
  the only path that can start the app.
- Every GUI reply carries the current capture settings, so the file-type
  list, excluded sites, and **this browser's** checkbox under Hydra's
  Options > General propagate to the extension automatically. Each request
  names the browser it came from, so the rows in that list govern their own
  browser rather than sharing one flag.
- **The in-page bar** (`content.js`) mirrors: hovering a player shows
  "Download this video" or "Download this audio". Click the bar to open a
  numbered list of every variant in every container Hydra can actually
  produce — TS *and* MP4 for MPEG-TS segments, MP4 only for fragmented MP4
  and DASH — cheapest quality
  first, with "Download all" at the top. Each row leads with what tells it
  from its neighbours — `1080p HD · MP4 · 4.8 Mbps` for a stream variant,
  its own name for a direct file — and the page title the download is saved
  under heads the list once instead of opening every row. It lists only what
  the background already sniffed; it never probes the page or the network.
  Click the bar again, press Escape, or click outside to close the list.
  A single detected file downloads directly when the bar is clicked.
- The bar follows the reader: it clears itself once the player it belongs to
  scrolls out of view and moves to the next one on screen, which is what a
  feed of clips needs. How long it stays otherwise is the **Hide it after**
  box in the popup — 10 seconds by default, `0` to leave it up until it is
  dismissed. Turn the bar off entirely from the popup as well.
- Adaptive streams are sniffed as one entry per manifest, never per segment:
  a `.m3u8` or `.mpd` response is fetched once, parsed for its variants
  (resolution, bitrate, codecs, duration), and listed under **Streams** in
  the popup. Variant playlists a master already covers are folded into it,
  and segments are kept out of the direct-media list. A manifest that
  declares Widevine, PlayReady, FairPlay or common encryption is shown as
  unsupported and is never sent — Hydra does not circumvent DRM.
- The selection pill is the same bargain as the video bar: handy on a release
  page where the files are a highlighted column, in the way on a page of prose
  that happens to be linked. **Download button on selected links** in the popup
  chooses when it shows — *Always*, *Only several links* (a lone one is already
  served by the right-click menu, so the batch is what is left), or *Never*.
  The page obeys the new choice at once rather than on the next reload, and a
  pill the choice forbids is taken back rather than left on screen.
- Hold **Alt** while clicking a link to bypass capture once.

## Install

1. Register the native host. **The Hydra app does this itself** on every
   start, for every browser installed for your user, so normally there is
   nothing to do here. From a source checkout, where the app may not have
   been launched yet, the script does the same thing:

   ```bash
   scripts/install-native-host.sh
   ```

2. Load the extension: `chrome://extensions` → enable **Developer mode** →
   **Load unpacked** → pick `extensions/chrome/`.

3. Restart the browser once so it sees the native-host manifest.

The extension ID is pinned by the `key` field in `manifest.json`
(`jpnonmbbkjdpeebdhkjoliklfhkdcomj`), so the native-host manifest written in
step 1 stays valid no matter where the unpacked directory lives. The install
script re-derives the ID from `manifest.json`, so the two can never drift.

### Packaged .zip

```bash
scripts/build-extensions.sh
```

writes `target/extensions/hydra-chrome-<version>.zip` — the same files as
this directory, minus the repository-only ones, with `manifest.json` at the
archive root — plus `target/extensions/chrome/`, the unpacked copy it was
made from, the Firefox `.xpi`, and an `INSTALL.txt`.

With a signing key it also writes `hydra-chrome-<version>.crx` (CRX3: the
zip behind an RSA-SHA256-signed header):

```bash
scripts/build-extensions.sh --crx-key path/to/key.pem
scripts/build-extensions.sh --crx        # generate a throwaway key instead
```

Each shape has one job. The **unpacked directory** is the only one installable
by hand — Chromium has not accepted a dragged-in `.crx` from outside the Web
Store for years. The **`.zip`** is what the Web Store consumes (strip `key`
first; it is only needed for the fixed id of a local install). The **`.crx`**
is for enterprise policy deployment against a self-hosted update manifest.

The signing key IS the identity: Chromium requires the manifest `key` to be
the signer's public key, so signing with anything other than the key behind
the pinned one moves the extension off `jpnonmbbkjdpeebdhkjoliklfhkdcomj`.
The script rewrites the manifest key to match (a mismatched pair is rejected
outright by the browser) and warns with the id it produced. Such a build
still reaches Hydra over the WebSocket — `extbus` accepts any extension
origin that presents the `ipc.json` token, which only the native host hands
out — so it needs the native-messaging allow-list to name its id, and that
list is also what gates launching the app.

`--out DIR` packs into DIR instead, which is how every installer ships the
extension inside the installed application.

## Protocol (extension → GUI)

Two front doors to the same handler:

| Transport | Port | Auth | Used by |
|---|---|---|---|
| WebSocket | `6799`, fallback `16799` (fixed) | extension `Origin`, then `auth` with the `ipc.json` token | the extension, directly |
| Line protocol | ephemeral, published in `ipc.json` | random token from `ipc.json` | `hydra-host` |

The WebSocket is the primary path: no process spawn per request, and the
live socket *is* the "Hydra is running" indicator (the toolbar shows a gray
**X** while it is down). The native host remains the fallback and
is the only path that can **launch** the app.

Any extension origin gets past the WebSocket handshake, but the socket's
first frame must be `{"type":"auth","token":…}` carrying the token from
`ipc.json` — which the extension gets by asking the native host
`{"type":"ws-token"}` (answered `{"ok":true,"ws_port":N,"token":…}` while
the app is running) and caches in `storage.session`. Any other frame on an
unauthenticated socket is answered `{"ok":false,"error":"unauthorized"}`
and the socket closed (as is one idle for 5 s), so a stale token after an
app restart simply triggers a refresh and a redial; the extension ids
Hydra ships under (`nmhost::CHROMIUM_EXT_IDS`) are admitted without a token.

Requests are JSON objects: `ping`, `config`, `open`,
`download {url, filename?, cookies?, referer?, user_agent?, size?, mime?,
tab_url?, proxy?}`, `links {urls}`. An `id` is echoed back so replies can be
matched. Replies: `{ok, capture, auto_types, dont_start_sites, version,
id?, error?}`.

## Behavior details

- **Capture order**:
  the browser download is **paused** the instant it is created, the decision
  is made once `onDeterminingFilename` resolves the real filename, and only
  then is it cancelled and erased — or resumed untouched if Hydra did not
  take it. Pausing is reversible where cancelling is not: small files cannot
  finish before the round-trip, and signed one-shot URLs never need
  re-requesting.
- **The filename suggestion is deferred**: the `onDeterminingFilename`
  listener returns `true` and calls `suggest()` only after the decision.
  Chromium reserves the path and puts up its own **"Save as" dialog** in the
  steps straight after this listener answers, so answering it immediately
  raced that dialog onto the screen next to Hydra's New Download window for
  everyone with *Ask where to save each file* enabled. `suggest()` is still
  called exactly once on every path — releasing a declined download, and
  harmlessly landing on a cancelled one.
- **The browser's proxy** (popup: *Use this browser's proxy*): a file behind
  a tunnel is unreachable by an app that has never heard of the tunnel, so
  the proxy the browser is using for that URL travels with the capture as
  `proxy` and becomes **that download's** route — Hydra's own
  Options > Proxy/Socks is never touched, because the browser's setting can
  change tomorrow and the user's answer in Hydra must not have been
  overwritten today. The entry for the URL's scheme is the one that is sent,
  the browser's bypass list (`bypassList` / `passthrough`, including
  `<local>`) is honoured, and no credentials are involved: neither browser's
  API exposes them. Following the **machine's** proxy (`system` /
  `autoDetect`) is deliberately *not* passed on, because the browser will not
  say what it resolves to; it leaves the download on whatever Hydra's
  Options say. A proxy switcher (ZeroOmega, SwitchyOmega) is followed: in
  Chromium its **PAC script** (inline or by URL) is run for the download's
  URL in a sandbox page hosted by an offscreen document — MV3 service
  workers cannot compile code — with the PAC helpers but no DNS, capped at
  3 s, and its first `PROXY`/`HTTPS`/`SOCKS*` entry is sent (`DIRECT` sends
  none). In Firefox the switcher routes requests from `proxy.onRequest`,
  which the settings never show, so the `proxyInfo` of the last response
  seen from the download's origin is sent instead. The read is capped at 1.5 s, because a capture awaits
  it and a browser that answers neither way would leave the download parked
  on a question about a proxy. `proxy` is an **optional permission** and *is* the
  switch — the API does not exist until the checkbox grants it, so there is
  no second flag to fall out of step with it, and no existing install is
  disabled at its next update. Safari has no proxy API; the row stays hidden
  there.
- **Single instance**: launching hydra-gui while another instance runs now
  just surfaces the running instance's window and exits — the extension
  always talks to the instance that owns `ipc.json`.
- After changing the extension files, reload it on `chrome://extensions`;
  after re-running the install script, restart the browser once.

## Safari

`core.js`, `content.js` and the popup in this directory are the **single
source of truth for Safari and Firefox too** — they bind `browser` or
`chrome`, handle both native-messaging dialects, and feature-detect every
API. `scripts/sync-extension-resources.sh safari` copies them next to
Safari's manifest (Safari has nothing to capture, so `core.js` is its whole
background script); see [../safari/README.md](../safari/README.md) for the
build.

## Safari (details)

Safari has no native-messaging-host registry and **no `downloads` API**, so
automatic capture is impossible there (Apple does not expose download
events to extensions). Everything else — context menus, the selection
button, media sniffing, the popup — works via a wrapper app whose Swift
handler ([../safari/SafariWebExtensionHandler.swift](../safari/SafariWebExtensionHandler.swift))
forwards to the same loopback socket. Building it requires full Xcode:

```bash
scripts/build-safari-extension.sh
```

Then open the built app once, enable the extension in Safari → Settings →
Extensions, and (for unsigned dev builds) Develop → Allow Unsigned
Extensions.

## Notes / current limits

- `referer` and per-download `user_agent` are transmitted and logged, but
  the engine does not yet send them (StartSpec has no header support);
  cookies **are** applied.
- Streams whose manifest declares DRM (Widevine, PlayReady, FairPlay,
  common encryption) are listed as protected and are never sent — Hydra
  does not circumvent DRM. Live HLS/DASH under any encryption is refused by
  the engine for the same reason keys make it impossible to record honestly.

## Do I need `hydra-host` on every OS?

Only for one job: **starting Hydra when it is not already running.** Every
other request goes over the WebSocket, which behaves identically on macOS,
Linux and Windows and needs no registration at all. So if Hydra is already
running (it installs a login item by default), the extension never invokes
the host.

Ship it per-OS anyway, since the cold-start click is the common case:

| OS | Install | Registration |
|---|---|---|
| macOS / Linux | `scripts/install-native-host.sh` | JSON manifest in each browser's `NativeMessagingHosts` directory |
| Windows | `powershell -ExecutionPolicy Bypass -File scripts\install-native-host.ps1` | `HKCU` registry key per browser pointing at a manifest in `%LOCALAPPDATA%\Hydra` |

Both installers derive the Chromium extension id from the pinned manifest
key and the Firefox id from `browser_specific_settings.gecko.id`, so the
two platforms can never disagree about which extension is allowed.

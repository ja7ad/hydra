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
selection, a floating "Download this video" bar over players, and a welcome
page on first install.

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
  unreachable the paused download simply resumes in the browser.
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
- **The in-page bar** (`content.js`) mirrors IDM's: hovering a player shows
  "Download this video", and hovering that drops a numbered list of every
  variant in every container Hydra can actually produce — TS *and* MP4 for
  MPEG-TS segments, MP4 only for fragmented MP4 and DASH — named after the
  page title, cheapest quality first, with "Download all" at the top. It
  lists only what the background already sniffed; it never probes the page
  or the network. Turn it off from the popup.
- Adaptive streams are sniffed as one entry per manifest, never per segment:
  a `.m3u8` or `.mpd` response is fetched once, parsed for its variants
  (resolution, bitrate, codecs, duration), and listed under **Streams** in
  the popup. Variant playlists a master already covers are folded into it,
  and segments are kept out of the direct-media list. A manifest that
  declares Widevine, PlayReady, FairPlay or common encryption is shown as
  unsupported and is never sent — Hydra does not circumvent DRM.
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
origin — but not over native messaging, which is allow-listed by id and is
the only path that can launch the app.

`--out DIR` packs into DIR instead, which is how every installer ships the
extension inside the installed application.

## Protocol (extension → GUI)

Two front doors to the same handler:

| Transport | Port | Auth | Used by |
|---|---|---|---|
| WebSocket | `6799`, fallback `16799` (fixed) | `Origin: chrome-extension://…` | the extension, directly |
| Line protocol | ephemeral, published in `ipc.json` | random token from `ipc.json` | `hydra-host` |

The WebSocket is the primary path: no process spawn per request, and the
live socket *is* the "Hydra is running" indicator (the toolbar shows a gray
**X** while it is down). The native host remains the fallback and
is the only path that can **launch** the app.

Requests are JSON objects: `ping`, `config`, `open`,
`download {url, filename?, cookies?, referer?, user_agent?, size?, mime?,
tab_url?}`, `links {urls}`. An `id` is echoed back so replies can be
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
- **Single instance**: launching hydra-gui while another instance runs now
  just surfaces the running instance's window and exits — the extension
  always talks to the instance that owns `ipc.json`.
- After changing the extension files, reload it on `chrome://extensions`;
  after re-running the install script, restart the browser once.

## Safari

The files in this directory are the **single source of truth for Safari
too** — they bind `browser` or `chrome`, handle both native-messaging
dialects, and feature-detect every API. `scripts/sync-extension-resources.sh safari`
copies them next to Safari's manifest; see
[../safari/README.md](../safari/README.md) for the build.

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

# Hydra Firefox Extension

Same code, same transport, same behaviour as the
[Chrome extension](../chrome/README.md) — including **automatic download
capture**, which Firefox fully supports, and **HLS/DASH stream sniffing**.

## Why there is no separate source tree

The extension code in `extensions/chrome` is browser-neutral: it binds
whichever namespace exists (`browser` on Firefox, `chrome` on Chromium),
handles both native-messaging dialects, and feature-detects every API. Only
the manifest differs, so:

```bash
scripts/sync-extension-resources.sh firefox
```

copies the shared files next to Firefox's manifest into `Resources/`.

**Edit `extensions/chrome/*`, never `extensions/firefox/Resources/*`** — the
latter is regenerated on every sync.

## Firefox-specific details

- **Manifest V3 with an event page** (`background.scripts`) instead of a
  service worker — Firefox's MV3 background is a non-persistent event page,
  and the shared code runs unchanged on both.
- **Capture uses a single `onCreated` listener.** Firefox has no
  `onDeterminingFilename` (a Chrome-only event), but it resolves `filename`
  on the create event itself, so the extension parks and decides in one
  sequential step. Splitting them across two listeners would race, because
  listeners on the same event run concurrently.
- **Stable add-on id** (`hydra@ja7ad.github.io`, in
  `browser_specific_settings.gecko`) — native messaging allow-lists Firefox
  add-ons by id (`allowed_extensions`), not by an extension-origin URL the
  way Chromium does.
- **Host permissions may need granting.** Firefox MV3 treats
  `host_permissions` as optional; if cookies are not being attached, grant
  site access from `about:addons` → Hydra → Permissions.

## Install

1. Register the native host. **The Hydra app does this itself** on every
   start — it writes Firefox's manifest into
   `~/Library/Application Support/Mozilla/NativeMessagingHosts` on macOS,
   `~/.mozilla/native-messaging-hosts` on Linux, and points
   `HKCU\Software\Mozilla\NativeMessagingHosts` at it on Windows — so
   normally there is nothing to do here. From a source checkout, where the
   app may not have been launched yet, the script does the same thing:

   ```bash
   scripts/install-native-host.sh
   ```

2. Assemble the extension directory:

   ```bash
   scripts/sync-extension-resources.sh firefox
   ```

3. Load it: `about:debugging#/runtime/this-firefox` → **Load Temporary
   Add-on** → pick `extensions/firefox/manifest.json`.

### Packaged .xpi

```bash
scripts/build-firefox-xpi.sh      # Firefox only
scripts/build-extensions.sh       # Firefox .xpi + Chromium .zip + INSTALL.txt
```

Both write `target/extensions/hydra-firefox-<version>.xpi` (and verify that
`manifest.json` sits at the archive root, which Firefox requires) alongside
`target/extensions/firefox/`, the unpacked copy the archive was made from.
Load either — **Load Temporary Add-on** accepts an `.xpi` directly, or the
directory's `manifest.json`.

`build-extensions.sh --out DIR` packs into DIR instead; that is how the DMG,
PKG, deb, rpm and Windows installers put the extension inside the installed
application, next to an `INSTALL.txt` written with that machine's paths.

Temporary add-ons are cleared when Firefox restarts. For a **permanent**
install the package must be signed by Mozilla:

```bash
export WEB_EXT_API_KEY=user:12345678:123
export WEB_EXT_API_SECRET=...
scripts/build-firefox-xpi.sh --sign
```

(keys from <https://addons.mozilla.org/developers/addon/api/key/>; the
`unlisted` channel means self-distribution, not a public listing). The
alternative is Developer Edition / Nightly / ESR with
`xpinstall.signatures.required=false`.

## Transport

Identical to Chrome: a persistent WebSocket to `ws://127.0.0.1:6799`
(fallback 16799), authenticated by the `moz-extension://…` origin, with the
native host as the fallback that can launch the app. See the
[protocol table](../chrome/README.md#protocol-extension--gui).

# Changelog

All notable changes to the Hydra project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [1.1.0] - 2026-10-08

### Added

- **Extensible Plugin Runtime & Authoring SDK (`hydra-plugin`, `hydra-plugin-api`, `hydra-plugin-sdk`, `hydra-plugin-cli`, `hydra-gui`, `hydra-cli`, packaging)**:
  - Added a sandboxed plugin runtime executing WebAssembly guests via Wasmtime with fuel metering, controlled child command execution, and host HTTP access.
  - Added guest plugin SDK bindings and test harnesses for Rust, C, Go, Python, and Node.js.
  - Added the `hydra-plugin-cli` developer tool with commands to scaffold (`new`), build, package (`pack`), and sign (`sign`) `.hyaplugin` bundles using Ed25519 and Minisign signatures.
  - Added the Plugins tab under Settings in the desktop app and interactive terminal management in the CLI, supporting plugin installation, configuration, inspection, and removal.
  - Added system file associations and MIME type registrations (`application/x-hydra-plugin`) for `.hyaplugin` packages across Windows, macOS, Linux, and Flatpak, enabling direct opening and installation from file managers and browsers.
  - Added shared online plugin catalog discovery with one-click installation, update checks, and direct web installation links.
- **BitTorrent Downloads via Official Plugin (`hydra-torrent`, `hydra-plugin`, `hydra-gui`, `hydra-cli`)**:
  - Added native BitTorrent downloading through the official `hydra-torrent` plugin, supporting `.torrent` files and `magnet:` links.
  - Added native component support in the plugin architecture, bundling supervised platform binaries for Windows (x86_64, ARM64), macOS (Apple Silicon, Intel), and Linux (x86_64, ARM64).
  - Added torrent metadata inspection, multi-file and track selection in the Add URL dialog and CLI.
  - Added live peer counts, seed counts, upload and download rates, and piece completion progress to progress windows and terminal output.
- **YouTube & Media Stream Extraction (`hydra-youtube`, `hydra-gui`, `hydra-cli`)**:
  - Added bundled official YouTube resolver plugin for extracting video streams, adaptive audio/video tracks, and playlist entries.
  - Added stream and track selection choices in the Add URL dialog and interactive CLI prompts.
  - Added batch queuing support for YouTube playlists directly into the download list.
- **Settings Export and Import (`hydra-gui`)**:
  - Added *File ▸ Export settings...* and *Import settings...*, saving and restoring application configuration as a portable, compressed `.hydata` file.
  - Preserved machine privacy and environment portability by stripping local secrets, passwords, window geometry, and browser extension authorizations, while remapping home directory paths to the importing user's home (`~/...`).
- **High-Concurrency Connection Limits up to 256 (`hydra-gui`)**:
  - Added support for up to 256 connections per download and in per-server connection exceptions in the desktop app, expanding beyond the previous 32-connection limit.
- **Move and Rename Incomplete Transfers (`hydra-gui`)**:
  - Allowed moving and renaming paused, stopped, and incomplete downloads from the context menu and shortcut, relocating `.part` staging files and pinning chosen names.
- **Order by Addition in Download List (`hydra-gui`)**:
  - Added "Order by addition" sorting to the downloads table and View menu, preserving the order items were added.
- **Windows Package Manager Distribution (packaging)**:
  - Added official Windows Package Manager (`winget install ja7ad.hydra`) installation support and automated package submissions.

### Changed

- **Default User-Agent (`hydra-gui`)**:
  - Changed the default User-Agent for manually added downloads from generic `Mozilla/5.0` to Internet Explorer 11 Trident (`Mozilla/5.0 (Windows NT 6.1; Trident/7.0; rv:11.0) like Gecko`), preventing generic web scraper blocks, and migrated saved configurations holding legacy defaults.
- **In-Page Media Dropdown Activation (browser extensions)**:
  - Changed the browser extension media badge dropdown to open only on click instead of hover, preventing accidental popup triggers when moving the mouse or repositioning the badge.
- **Desktop Shortcut Created by Default on Windows (packaging)**:
  - Changed Windows GUI installations and the PowerShell install script (`install.ps1`) to create a desktop shortcut by default (`-Desktop:$false` skips it).

### Fixed

- **HTML Block Pages Intercepting Binary Downloads (`hydra-gui`, `hydra-net`)**:
  - Fixed captive portals, firewalls, and login pages returning HTML responses in place of binary files: Hydra now detects HTML block pages, rejects overwriting destination files, clears invalid staging data, and reports an error instead of saving HTML as the target binary.
- **Cross-Platform Filename Portability (`hydra-net`, `hydra-gui`, `hydra-cli`)**:
  - Fixed invalid characters (`<`, `>`, `:`, `"`, `|`, `?`, `*`), control codes, trailing spaces/dots, and Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`) across all platforms so downloaded files can be safely written to or moved across NTFS and FAT drives without OS errors.
- **Windows Drive Roots in Properties (`hydra-gui`)**:
  - Fixed editing save locations to Windows drive roots (such as `Q:\file.zip`) in the Properties dialog, preserving the root slash (`Q:\`) instead of stripping it to an invalid path (`Q:`), and kept drive letters out of display names.
- **Console Window Flashes on Windows (`hydra-gui`, `hydra-stream`, `hydra-plugin`)**:
  - Fixed ffmpeg remuxing and plugin commands briefly flashing visible console windows on Windows by launching subprocesses with `CREATE_NO_WINDOW`.
- **Startup Toggle in Portable Mode (`hydra-gui`)**:
  - Fixed the "Launch Hydra on startup" setting being toggled in portable mode, disabling the option with an explanation to prevent portable instances from overwriting or hijacking the startup entry of an installed copy.
- **Progress Dialog Minimum Dimensions (`hydra-gui`)**:
  - Fixed the progress dialog collapsing below readable proportions by enforcing strict minimum window dimensions matching collapsed and detailed view states.
- **Download Percentage in Paused State (`hydra-gui`)**:
  - Fixed the status column omitting progress while paused, displaying the percentage completed alongside the paused state (e.g. `Paused (45.20%)`) whenever file size is known.
- **macOS Window Restore from Dock (`hydra-gui`)**:
  - Fixed clicking the Dock icon on macOS failing to reopen the main window when all windows were closed or minimized.
- **Keyboard Layout and Platform Shortcut Handling (`hydra-gui`)**:
  - Fixed keyboard shortcut matching on non-Latin keyboard layouts by mapping physical key positions, added modifier autocompletion in shortcut settings, and adapted primary modifiers (`cmd` vs `ctrl`) per platform.
- **Dark Theme Text Selection Contrast (`hydra-gui`)**:
  - Fixed text input selection highlight in dark mode using light theme selection background, ensuring high contrast and readable text when selecting input fields.
- **Linux AppImage Portability on Older Distributions (packaging)**:
  - Fixed AppImage builds crashing on older Linux systems by building against Ubuntu 20.04 (glibc 2.31) and bundling missing `libxkbcommon` libraries.
- **Windows CLI Startup Stack Overflow (`hydra-cli`)**:
  - Fixed command line interface crashing on Windows debug builds by reserving an 8 MiB stack size to accommodate deeply nested CLI command structures.
- **Hostless Extension Authorization (`hydra-gui`)**:
  - Fixed WebSocket connections from browser extensions lacking a native host being refused without prompting, allowing users to approve and remember trusted extension origins.
- **Download List Sorting by Last Try Date (`hydra-gui`)**:
  - Fixed table sorting by last try date failing to sort timestamps accurately.

---

## [1.0.1] - 2026-09-26

### Fixed

- **Store-Installed Chromium Extensions Can't Reach Hydra (`hydra-gui`, packaging)**:
  - Fixed extensions installed from the Chrome Web Store, Edge Add-ons or Opera add-ons failing to connect to the desktop app, because Hydra only trusted the development extension id and not the ids the stores sign with.
  - Added the store ids to the native-host manifests written by the app, the Windows installer, the macOS `.dmg` and `.pkg`, the `.deb`, `.rpm`, AUR and AppImage packages and the install scripts, so the extension connects even before the app has been started once.
- **Launch at Startup on macOS 13 and Later (`hydra-gui`)**:
  - Fixed *Launch Hydra on startup* by registering the app through `SMAppService`, so it appears under System Settings ▸ General ▸ Login Items ▸ *Open at Login* and starts at login again. The LaunchAgent that earlier versions wrote is removed, so it can't start a second copy.
  - Left the item alone once it is switched off in System Settings instead of re-registering it on every launch. A login launch on macOS 13+ opens the window even when *minimized* is set, because Open at Login items can't take arguments; macOS 11–12 still use the LaunchAgent and still start minimized.
- **Full Disk Access Shown as Missing on macOS 27 (`hydra-gui`)**:
  - Fixed the permissions guide reporting Full Disk Access as not granted on macOS 27, which no longer has the per-user TCC database it checked. It now checks the system database first, and shows the status as unknown rather than denied when neither database exists.

---

## [1.0.0] - 2026-09-25

### Added

- **Move/Rename From the Download Complete Dialog (`hydra-gui`)**:
  - Added a *Move/Rename...* button between *Open folder* and *Close* that opens the native save panel, moves the file and keeps the list row pointing at it, so a finished download can be filed away without going back to the main window.
  - Locked the new name so a later probe cannot rename it back, and made the button work while the main window is hidden in the tray.
- **Main Window Position Remembered & Daily Log Rotation (`hydra-gui`)**:
  - Added `window_pos` to `config.toml`: the main window reopens where it was last placed, or centred when the middle of its title bar would land on a display that is no longer connected.
  - Added daily rotation of `gui.log`: on the first write of a new day the old log becomes `gui-YYYY-MM-DD.log`, and three days are kept instead of one file growing without limit.
- **User-Agent Presets (`hydra-gui`)**:
  - Added a dropdown under *Options → Downloads → User-Agent for manually added downloads* with *Default (Mozilla/5.0)*, *Chrome*, *Firefox*, *Safari* and *Edge*; editing the text switches it to *Custom*.
- **Authentication Flags & URL Credentials (`hydra-cli`, `hydra-net`)**:
  - Added `-u/--user USER[:PASSWORD]` and `--password` for HTTP Basic auth, with the wget spellings `--user`, `--password`, `--http-user`, `--http-password` and curl's `-u` mapped onto them.
  - Started sending the credentials in `http://user:pass@host/`, which used to be parsed and dropped; they are withheld on a redirect to another host and restored if the chain comes back.
- **Stream Format Coverage (`hydra-stream`)**:
  - Added HLS `EXT-X-DEFINE` variables (`NAME`/`VALUE`, `IMPORT` from the master playlist, `QUERYPARAM`), `EXT-X-GAP` segments, mid-stream `EXT-X-MAP` changes and inline `data:` AES-128 keys.
  - Added packed-audio HLS (`.aac`, `.mp3`, `.ac3`, `.eac3`), saved under its own extension instead of as an unplayable `.mp4`/`.ts`.
  - Added DASH `SegmentList`, `SegmentBase` and single-file `BaseURL` Representations, and multi-Period manifests joined into one track per Representation.
- **Update Dialog Knows About Package Managers (`hydra-gui`, `hydra-cli`, `hydra-updater`)**:
  - Added a Homebrew hint (`brew upgrade ja7ad/tap/hydra`, or `--cask`) in place of the in-app installer on Homebrew installs, in the GUI and in `hydra update` (with a `hint` field under `--json`).
  - Added a notice for a newer release that has no build for this system, with a link to its release page, instead of ignoring it.
- **Browser Extensions 0.3.7 (browser extensions)**:
  - Added body sniffing for small XHR/fetch responses with a generic content type, so HLS/DASH manifests behind extensionless URLs or served as `application/xml` are detected; `.m3u` counts as HLS too.
  - Added CHIPS partitioned cookies to captures, and media inside an iframe now sends the frame's own URL as the Referer.
  - Added a 5-second heartbeat so a dead socket falls back to native messaging at once instead of after an 8-second capture wait, and a capture made while Hydra is still starting is held for up to 60 seconds and handed over when it comes up.
  - Added Opera to the native-host install scripts.
- **libhydra 1.0.0 (`hydra-ffi`)**:
  - Bumped the C library to 1.0.0 (`HYDRA_FFI_VERSION "1.0.0"`, `HYDRA_FFI_VERSION_NUMBER 1000000`). The ABI stays `HYDRA_FFI_ABI_VERSION 1`: no symbol, struct layout or constant changed, so existing bindings keep linking.
  - Documented the ABI 1 behavior guarantees in `include/hydra.h` and `docs/ffi/ABI.md`: init zeroing up to `struct_size`, `hydra_event_wake` semantics, which calls a callback may make, and that a job's terminal event is its last.

### Changed

- **Stricter Command-Line Validation (`hydra-cli`)**:
  - Changed zero, negative or out-of-range values for `-x`, `-s`, `--max-total-connections` (above 4096), `--parallel-max`, `--chunk-size`, `--tries`, `--limit-rate`, `--timeout`, `--wait` and `--connect-timeout` into usage errors (exit 2) instead of silent fallbacks, along with an unknown `--container` or `--compat`, `--json` with `--stdout`, and `-O` with several URLs unless `--mirrors` is given.
  - Removed `--retry-delay` and `hydra checksum --sidecars`; sidecar lookups stay on by default and `--no-sidecars` turns them off.
  - Made a repeated valued flag take its last value, as curl and wget do, instead of failing to parse.
  - Made Ctrl-C save the resume record from the ranges actually held, print how to continue with `-c`, and exit 130; a second Ctrl-C exits at once.
- **No Overwrites Without a Terminal (`hydra-cli`)**:
  - Changed runs with no terminal to write beside an existing file (`name.1`) instead of overwriting it or reading an answer from a pipe; multi-URL runs skip the file and say so. `--force` still replaces and `-c` still continues, and streams now honour `--no-clobber` and `--force` too.
- **Proxy Environment Read Per Scheme (`hydra-cli`, `hydra-net`)**:
  - Changed proxy discovery to read `https_proxy`, `http_proxy` and `ftp_proxy` by scheme and then `all_proxy`, lower case first, and to honour `no_proxy`; only `http_proxy` used to be read, for every scheme. An unparsable proxy variable is now an error.
- **Default User-Agent (`hydra-gui`, `hydra-cli`, `hydra-ffi`)**:
  - Changed the desktop app's default for manually added downloads from `hydra-gui/<version>` to `Mozilla/5.0`, because some web firewalls and NCBI refuse the `hydra` token; saved configs still holding the old default are migrated.
  - Changed the CLI and libhydra default from `hydra/0.1` to `hydra/<version>`.
- **IEC Size Units in Stream Output (`hydra-cli`)**:
  - Changed stream, `--inspect` and `--preview` sizes to IEC units (`1.50 KiB`, `5.00 MiB`), matching the rest of the output. The progress rate now shows the real rate from the first frame and counts only this run's bytes after a resume.
- **Open Folder Goes Through the Windows Shell (`hydra-gui`)**:
  - Changed *Open folder* on Windows to `SHOpenFolderAndSelectItems`, so a replacement file manager such as Directory Opus or XYplorer gets the window with the file highlighted. The 0.6.1 workaround checkbox *Select the downloaded file in the file manager* is gone; an old `select_in_file_manager` key is ignored.
  - Moved *Open folder* off the UI thread on every platform, so a slow network share no longer freezes the window, and made it open the folder when the file itself is missing.
- **Keyboard Handling in Dialogs (`hydra-gui`)**:
  - Bound Escape to a dialog's Cancel and Enter to its default button; the duplicate prompt, whose three choices are equal, ignores Enter.
  - Queued a second confirmation raised while one is showing, instead of letting it replace the first and lose the capture that one held.
- **Extension WebSocket Requires the IPC Token (`hydra-gui`, `hydra-host`, browser extensions)**:
  - Changed the local WebSocket to require the token from `ipc.json` as the first frame from any extension other than the two pinned Chromium ids; any web extension could previously connect by origin alone. hydra-host hands the token to a running app through a new `ws-token` request.
  - Moved Firefox extensions older than 0.3.7 onto native messaging, which works but is slower, because the WebSocket now refuses them; updating the extension restores the fast path. Extension 0.3.7 still works with older Hydra releases.
  - Allow-listed the Chrome Web Store id in the native-host manifest, which the app rewrites on every launch.
- **Leaner Idle Redraws (`hydra-gui`)**:
  - Changed the 80 ms animation tick to run only while a progress window is open on an active download, and progress polling to every 250 ms when no progress window is open, cutting idle CPU use.
- **Removed Settings That Did Nothing (`hydra-gui`)**:
  - Removed *Use FTP in PASV mode* and *Show the dialog to add an address to the list of exceptions for a twice-cancelled download*; their `config.toml` keys are ignored.

### Fixed

- **Command-Line Flags Ignored on Some Paths (`hydra-cli`)**:
  - Fixed `--content-disposition`, `--connect-timeout`, `--show-progress`, `--no-verbose`, `--parallel-max` and `--wait` being parsed and never applied, and `-P` failing on a directory that did not exist yet.
  - Fixed `-T/--timeout` not covering the probe, so a server or proxy that accepted the connection and never answered hung forever.
  - Fixed downloads of unknown size (chunked, no `Content-Length`) and FTP skipping every post-transfer flag: a wrong `--checksum` now fails the run instead of exiting 0, and `--no-save`, `--stdout`, `--max-filesize`, `--remote-time`, `--etag-save`, `--sort-by-type` and `--limit-rate` apply.
  - Fixed `--proxy` not being used for redirect hops, FTP and `hydra checksum` sidecar fetches, and `hydra interactive` ignoring the command line's headers, rate cap, proxy, cookies and connection count.
  - Fixed cookie flags (`-b`, `--cookie-jar`, `--load-cookies`, `--cookies-from-browser`) not reaching HLS/DASH manifests, segments and keys, `--list-streams`, `--inspect` or `--preview`.
  - Fixed `--stdout` staging under the URL's name in the working directory, which overwrote and then deleted any file already there, and a failed write to stdout exiting 0.
  - Fixed the resume record storing one prefix instead of the exact ranges held, which could make `-c` treat missing bytes as downloaded.
  - Fixed a corrupt `queue.json` being replaced by an empty queue; it is now kept as `queue.json.corrupt-<time>` and the run stops with an error.
  - Fixed IPv6 literal URLs (`http://[::1]:8080/`) and `hydra interactive --headless` exiting 0 when an item failed.
- **Proxy Logins Over HTTPS (`hydra-net`, `hydra-ffi`)**:
  - Fixed HTTPS through an authenticated HTTP proxy failing with 407: the `CONNECT` now carries `Proxy-Authorization`, and a refusal says the proxy wants a login.
  - Fixed the proxy login being forwarded inside the tunnel to the HTTPS origin.
- **Truncated or Mismatched Stream Segments (`hydra-net`, `hydra-stream`)**:
  - Fixed a chunked segment or playlist cut off before its final chunk being written as complete; it is now retried.
  - Fixed a server answering a byte-range segment (HLS `EXT-X-BYTERANGE`, DASH `mediaRange`) with `200` or the wrong `Content-Range` having the whole object written in place of the slice.
- **HLS and DASH Parsing (`hydra-stream`, `hydra-gui`, `hydra-cli`)**:
  - Fixed an unknown encryption method or an unfetchable key URI (`skd://`) writing ciphertext as the output; both are now refused with the reason, and a malformed IV is no longer reported as DRM.
  - Fixed live recordings dropping a slow segment that was still downloading: a segment is now abandoned only after 60 seconds without a byte, or after 4 hours in total.
  - Fixed DASH `startNumber="0"` producing wrong segment URLs, a negative `SegmentTimeline` repeat producing a single segment, `presentationTimeOffset` being ignored in `$Time$`, and XML entities in `BaseURL`.
  - Fixed playlists starting with a byte-order mark being rejected, remuxing MP3 or AC-3 audio to MP4 failing, and `--list-streams` reporting "not an HLS or DASH manifest" for what was really a 404.
  - Fixed signed or rotating segment URLs defeating a stream resume. A partial stream download started by an earlier release restarts rather than resumes.
  - Fixed the desktop app sending a stream's session cookies to every host it touched; segments, keys and variants on a CDN now get only that host's cookies.
- **libhydra Behavior (`hydra-ffi`)**:
  - Fixed `hydra_event_wake` not releasing a waiter, so `hydra_event_wait(HYDRA_WAIT_FOREVER)` blocked forever; released calls now return `HYDRA_ERR_AGAIN`.
  - Fixed a stale `PROGRESS` event arriving after a job's `COMPLETED`, `FAILED`, `PAUSED` or `CANCELLED`.
  - Fixed `hydra_job_create` reading past a smaller struct declared by an older header, init functions leaving bytes past this build's struct uninitialised, and `hydra_job_create_from_metalink` accepting NULL.
  - Fixed `network_policy` and `power_mode` in `hydra_engine_config_t` being validated and then ignored.
  - Fixed a resume splicing bytes from two versions of an object: the strong `ETag` is now persisted and a changed ETag or size discards the held ranges.
  - Fixed `hydra_job_cancel` on a job that never ran deleting whatever file was already at `output_path`.
  - Fixed the job's login being re-sent on a redirect to another host or from https to http, and protocol-relative, query-only and IPv6 redirect targets resolving wrongly.
  - Fixed a percent-encoded `/`, `\` or NUL in a URL naming a file outside the output directory.
  - Fixed `stall_count` in `hydra_engine_get_metrics` being overwritten per job instead of summed, and restored jobs being stuck in a running state they could not leave.
- **Updater Safety (`hydra-updater`, `hydra-gui`)**:
  - Fixed a stalled update download hanging forever: connects now time out after 15 seconds, a body read after 30 seconds without a byte, and Cancel is honoured within 250 ms.
  - Fixed a failure part-way through the swap leaving some files new and some old; every file is renamed aside first and all are restored on failure.
  - Fixed an archive missing from the release's `SHA256SUMS.txt` being installed unverified; it is now refused and deleted.
  - Fixed cancelling during *Verifying* or *Preparing* still restarting into the update, and staging moved out of the shared `/tmp` into an owner-only directory.
  - Made the updater honour `HTTPS_PROXY`/`HTTP_PROXY`, say when a GitHub rate limit clears, and strip the macOS quarantine flag from the updated bundle.
- **Dialogs and Options (`hydra-gui`)**:
  - Fixed *Options → OK* overwriting settings changed elsewhere while the dialog was open (window position, hidden columns, the toolbar speed limiter); only the fields edited in the dialog are saved now.
  - Fixed a download limit of 0 MB blocking every download for good, and made *Options* refuse OK on the tab with the problem (no speed entered, an unevaluated PAC script, a proxy with no address or a bad port) with the reason beside the buttons.
  - Fixed *Properties → OK* stopping and queuing a running transfer.
  - Fixed a Scheduler start time typed as `9:00` never firing, and the Scheduler losing track of a queue renamed or deleted from the sidebar.
  - Fixed *Resume existing* on a finished duplicate restarting it from zero; the prompt now offers *Open existing*.
  - Fixed *Show download complete dialog* in the progress window's *Options on completion* tab doing nothing, and *Change folder for "<category>" category on last selected* being saved but never used.
  - Fixed *Add URL* adding a stream manifest or mirror list as a plain file when OK was pressed while it was still being checked.
  - Fixed a browser capture that arrived after the browser had kept its own download also being downloaded by Hydra.
  - Fixed *Tasks → Import* adding the same address twice, the progress window's speed limit box hiding the limit in force, and a dialog closed before it finished opening being left orphaned.
  - Fixed the startup update check being skipped when Hydra started in the tray.
- **Window Scaling and Layout (`hydra-gui`)**:
  - Fixed *View → Scale* applying the scale twice to the main window's minimum size, which above 100% forced the window larger than the screen.
  - Fixed the progress window's connections table and scanner log staying 8 rows tall when the window was enlarged.
  - Fixed the *Download complete* dialog pushing *Close* off its edge in longer translations such as Russian.
- **Small Files Split Too Finely (`hydra-cli`, `hydra-gui`, `hydra-ffi`)**:
  - Fixed small objects being split across every connection; each connection now gets at least 256 KiB, so a 50 KB file is one request.
- **Portable Windows Launcher (`scripts/windows/portable`)**:
  - Fixed the 0.6.1 portable bundle failing to start through `HydraPortable.exe` by dropping `RunAsAdmin=none` from the launcher configuration.

---

## [0.6.1] - 2026-09-22

### Added

- **Cookie Jar & Browser Cookie Import for Authenticated Downloads (`hydra-net`, `hydra-cli`, `hydra-gui`)**:
  - Added `--cookie "session=abc; csrf=def"` (curl's spelling) and `--cookie-jar <FILE>`, a Netscape `cookies.txt` read before the first request and written back after it, alongside wget's one-direction `--load-cookies`, `--save-cookies` and `--keep-session-cookies`.
  - Added `--cookies-from-browser <BROWSER[:PROFILE]>`, reading the browser's own store: Firefox, LibreWolf and Zen from `cookies.sqlite`; Chrome, Chromium, Edge, Brave, Vivaldi and Opera from their encrypted `Cookies` database, unlocked through the platform the way the browser does it (macOS Keychain, Windows DPAPI-wrapped `Local State` key, Linux libsecret with the documented `peanuts` fallback); Safari from `Cookies.binarycookies`. A running browser does not have to be closed — the store and its WAL are copied into an owner-only (`0700`) directory and read from the copy.
  - Implemented the SQLite page/overflow reader, the Chromium AES-CBC/AES-GCM unwrapping, the Safari binary format and the Netscape `cookies.txt` parser inside `hydra-net::cookies`, with fixtures and a generator script (`scripts/make-cookie-fixtures.py`) rather than a browser-database dependency.
  - Scoped narrowly and deliberately: only cookies for the host being downloaded from survive the import, `Domain=` may widen a cookie only to a domain the setting host is under and never to a public suffix, a redirect to another site carries that site's cookies and nothing else, values are never logged at any `-v` level, and a jar written to disk is `0600`.
  - Every run names the exact store it read and the host it read it for; nothing is written to disk unless `--cookie-jar` or `--save-cookies` asked for it.
  - A `Set-Cookie` issued on a redirect is now held for the rest of that chain with no flag at all, which is what a login-gated CDN expects. The jar dies with the chain unless a flag asked for it to be kept.
  - In the desktop app: **Add URL** gained a Cookies field that imports the session for the address being typed, **Options → Connection → Cookies** picks the browser and profile used for downloads added by hand and reports live whether that store can be read, and **Properties** shows where a download's cookies came from. Captures from the Hydra browser extension already carry the page's own cookies and are unaffected.
  - On macOS a browser profile lives behind the system privacy control, so the first attempt names the path it could not read and points at **Full Disk Access** in System Settings ▸ Privacy & Security instead of failing as a download error.
- **Options Dialog Grouped Into Four Sections (`hydra-gui`)**:
  - Replaced nine tabs over two rows with four — *Application*, *Files*, *Connection*, *Extensions* — each opening a row of sub-tabs, leaving room for pages to be added without a tenth top-level tab.
  - Split the former Connection tab into *Connections*, *Cookies*, *Speed limiter*, *Download limits*, *Proxy / Socks* and *Sites Logins*, and gave ffmpeg its own *Media tools* page under Extensions.
  - Translated the new tab, section and cookie strings across all 30 supported languages.
- **Portable Windows Bundle in the Release (`scripts/package-windows-portable.sh`, CI)**:
  - Added a PortableApps-format bundle (`HydraPortable.exe` launcher, `App/AppInfo` metadata and icons, a `DefaultData` profile) built for `amd64` and `arm64` and attached to the release, so Hydra can run from an external drive with its configuration, state and logs beside it.
- **Browser Extensions as Release Assets (CI)**:
  - Added a release job that packs the Chromium `.zip` and Firefox `.xpi` from source and uploads them with the rest of the assets, and wired the Safari wrapper build into the release workflow.
- **Safari Extension Sync, Validation & macOS Build Workflow (CI)** — *contributed by [@VedantMadane](https://github.com/VedantMadane) ([#1](https://github.com/ja7ad/hydra/issues/1))*:
  - Added `.github/workflows/safari-extension.yml`, which regenerates the Safari `Resources` from the Chrome sources and fails if the result is dirty, validates the MV3 manifest and required assets, builds the wrapper app with full Xcode on macOS, and uploads the `.app` as a CI artifact.

### Fixed

- **Self-Redirect Reported as a Spent Hop Budget (`hydra-net`, `hydra-cli`, `hydra-gui`)**:
  - Fixed a `Location` pointing back at an address already requested being followed until the redirect budget ran out and then reported as "too many redirects", which sent the reader looking for a chain that was too long instead of one that never moved. `polite::RedirectChain` now detects the repeat on the hop that makes it and fails with `redirect loop: <url> was already requested`; the GUI names it *Redirect loop*, or *Redirect loop (the server expects a cookie)* when that is what the chain is asking for.
  - Fixed `-H` headers and the user agent being dropped on any absolute `Location`, on the theory that an absolute hop is cross-origin. A mirror that answers with its own address in full form is still the origin that was asked, and dropping the headers there discarded the one thing the user had supplied to get in. The test is now scheme, host and port, judged on the origin endpoint rather than the socket peer, so credentials still stop at the origin that issued them while the agent and ordinary headers travel the whole chain.
  - Fixed a proxied `https` chain losing its scheme at the first absolute hop and continuing in cleartext.
- **Non-ASCII and Unescaped Paths Rejected as `400 Bad Request` (`hydra-net`)**:
  - Fixed a URL pasted as the user reads it — `/d/guest/あけあけ/packs/x.rar`, or any address with a space — going onto the wire as raw UTF-8, which nginx-fronted origins are entitled to reject even though every browser fetches the same link. The request-target is now percent-encoded per RFC 3986 at the transport, one rule for the CLI, the GUI, the FFI and the stream front ends, while the URL stays as typed wherever it is displayed, written to a resume sidecar or compared across a redirect chain. `%` passes through, so an already-escaped path is not encoded twice.
- **Probe Renaming a File It Could Not Name (`hydra-gui`)**:
  - Fixed a link probe that learned nothing about the filename falling back to a `file_name_from_url` placeholder — typically `index.html` — and the app adopting it over the name a browser capture or the File Info dialog already carried. `LinkMeta::file_name` is now `Option<String>`, so "the object is called this" is distinguishable from "nobody said".
- **Named Firefox Profile Swapped for a Sibling (`hydra-net`)**:
  - Fixed `--cookies-from-browser firefox:default` reading `default-release` on a machine where the profile actually called `default` has no store, because the substring match fell through to the first profile that did. The label after the salt is now matched exactly before any substring, and a named profile without a store is reported as such instead of quietly replaced.
- **Cookie Import Hardening (`hydra-net`, `hydra-cli`, `hydra-gui`)**:
  - Fixed an imported session surviving a change of host in **Add URL**: typing an address on another host now clears what an earlier import attached before any re-import answers, so pressing OK in between cannot send the previous site's cookies, and an import that finishes after the address has moved is discarded and re-asked.
  - Fixed the Options readability check answering out of order — every keystroke in the profile box starts another, and they finish in whatever order the disk allows — by tagging each check and letting only the newest report.
  - Fixed two ways a corrupt or hostile store could be walked further than it should: an overflow page naming itself as its own continuation is now cut off by the same cycle guard as the b-tree walk, and a WAL is subject to the same size cap as the database it belongs to.
  - Corrected the warning printed when both a cookie flag and `-H 'Cookie: ...'` are given: the jar replaces the header per host rather than winning outright.
  - Dropped the unused `hmac` dependency and kept browser store paths out of test failure messages, closing a CodeQL clear-text-logging report.
- **Open Folder Forcing Explorer (`hydra-gui`)**:
  - Fixed *Open folder* always going through the platform's file manager with the file selected, which on Windows means Explorer even when a replacement is installed. Added *Select the downloaded file in the file manager* under Options; with it off the folder opens through whatever the system opens folders with, and the replacement gets the window.

---

## [0.6.0] - 2026-09-19

### Added

- **File Actions, Percentage UI Scale & Portable Browser-Capture Handoff (`hydra-gui`)**:
  - Added *Show in Folder*, *Open With...* and *Move file* actions to the download row context menu, each with a native, platform-specific implementation (Explorer `/select,`, macOS `open -R` / AppleScript chooser, Linux `dbus-send`/`gio`).
  - Replaced the point-size *View → Font* setting with a percentage-based *View → Scale* (50%, 67%, 75%, 80%, 90%, 100%, 110%, 125%, 150%, 175%, 200%), scaling the entire interface — text, rows, buttons and dialogs — together instead of just the type size.
  - Added *Options → Extensions → "Let this copy handle browser capture"* for portable installs (`hydra-gui --config <dir>`), registering that copy's native-messaging host so browser downloads reach the running portable profile instead of any ordinary install on the same account.
- **Flatpak Packaging & Flathub Release (`packaging/flatpak`, CI)**:
  - Added a Flatpak manifest, desktop entry and AppStream metadata, with a GitHub Actions job that builds and validates the bundle and publishes it to Flathub as `io.github.ja7ad.hydra`.
  - Installable with `flatpak install flathub io.github.ja7ad.hydra`.
- **Video Panel Quality-Led Rows & Auto-Hide Timeout (browser extensions)**:
  - Each row in the "Download this video" bar now leads with what distinguishes it from its neighbors (e.g. `1080p HD · MP4 · 4.8 Mbps`), with the page title heading the list once instead of repeating on every row.
  - The bar now clears itself once its player scrolls out of view and reappears over the next one on screen — useful for a feed of clips.
  - Added a *Hide it after* setting in the popup (10 seconds by default, `0` to leave it up until dismissed).
- **Browser's Own Proxy Hand-off for Captured Downloads (browser extensions, `hydra-gui`)**:
  - Added a *Use this browser's proxy* option in the popup: the proxy the browser is using for a captured URL now travels with that single download as its own route, without touching Hydra's own Options → Proxy/Socks settings.
  - Honors the browser's proxy bypass list; system-wide/auto-detect proxies and PAC scripts are intentionally left alone (neither browser exposes what they resolve to), so those downloads fall back to Hydra's own settings.
  - Requires no credentials and no extra permission grant beyond the existing capture optional permission; Safari has no proxy API, so the option stays hidden there.
- **Toolbar Text Visibility Toggle (`hydra-gui`)**:
  - Added *View → Hide toolbar text*, collapsing the toolbar to icons only, with the label available as a hover tooltip. The Speed Limiter button keeps its active cap visible in the tooltip (e.g. "Speed Limit — 5 MB/s") so a forgotten limit stays discoverable.
- **Selection-Pill Visibility Control (browser extensions)**:
  - Added *Download button on selected links* in the popup, choosing when the floating batch-download pill appears over a text selection: *Always*, *Only several links*, or *Never*. Takes effect immediately on the page, without a reload.
- **Multi-Row Selection & Bulk Actions in the Batch Download Table (`hydra-gui`)**:
  - Added click, Shift-click and Cmd/Ctrl-click multi-row selection to the batch download dialog's link table.
  - Added *Check Selected* and *Uncheck Selected* buttons alongside the existing *Check All*/*Uncheck All*, enabled only while a selection is active.
- **Scheduler Queue Field Spin Arrows & Save (`hydra-gui`)**:
  - Added up/down spin arrows next to the Scheduler's numeric fields (retries, files-at-once) so values can be nudged without retyping them, and values below the field's floor can be stepped back into range.
  - Added a *Save* button to the Scheduler dialog to persist queue changes without starting or stopping it.

### Fixed

- **Firefox Capture Split & Blocked WebSocket (`extensions/firefox`)**:
  - Split the shared extension code into a browser-neutral `core.js` and a per-browser `background.js`, giving Firefox its own capture path (Firefox has no `onDeterminingFilename`, so capture happens at the response instead of after the download is already created, which is what previously left orphaned "Canceled" rows in Firefox's native download list).
  - Fixed Firefox's default Manifest V3 content-security-policy silently upgrading the extension's `ws://127.0.0.1:6799` connection to `wss://`, which the app does not speak — the socket never connected, every capture fell through to slower `hydra-host` spawning, and the toolbar reported the app as down even while it was running.
- **Chromium Save Dialog Race (`extensions/chrome`)**:
  - Fixed Chromium's native *"Save as"* dialog popping up alongside Hydra's own *New Download* window for users with *Ask where to save each file* enabled, by deferring the filename `suggest()` callback until after Hydra has decided whether to capture the download.
- **HLS Alternate Audio Rendition Fetch (`hydra-stream`, `hydra-cli`, `hydra-gui`)**:
  - Fixed downloaded HLS streams coming out silent when a variant's audio is a separate `#EXT-X-MEDIA` rendition rather than muxed into the video segments (a pattern several streaming CDNs use). Hydra now parses alternate audio renditions, picks the one a player would (`DEFAULT`, then `AUTOSELECT`, then the first listed), and fetches it alongside the video.
- **CLI Percent-Escape Decoding for Suggested Filenames (`hydra-cli`)**:
  - Fixed downloaded filenames derived from a URL (when `-O` is not given) keeping raw percent-escapes, e.g. saving `Elementor%20Pro.zip` instead of `Elementor Pro.zip`.
  - Query strings are now stripped before the filename is derived, so a redirect parameter containing its own `/` (`?redirect=/a/b.zip`) no longer hijacks the suggested name.
- **Batch Dialog URL List Scrolling (`hydra-gui`)**:
  - Fixed the batch download dialog's URL text box clipping pasted lists instead of scrolling, which could hide the first or last links in a large paste with no visible indication that more content existed.
- **Batch Clipboard Link Recognition & Probing (`hydra-gui`)**:
  - Fixed links pasted in "titled list" formats (e.g. IDM's `title|url`, numbered lists, HTML anchor markup) being dropped entirely instead of added to the batch table.
  - Fixed the batch dialog re-probing every link on each keystroke after a large paste; links are now probed once each, debounced until typing pauses.
- **App-Wide Speed Limiter Cap (`hydra-gui`)**:
  - Fixed the Speed Limiter applying its cap per download instead of as a single shared aggregate, which let overall bandwidth usage multiply with the number of simultaneously active downloads.
- **Non-Blocking File & Folder Pickers (`hydra-gui`)**:
  - Fixed native *Save As* / folder-picker dialogs freezing the whole application (including progress repaints for other active downloads) while open, by moving them onto the platform's own async panel APIs instead of `rfd`'s blocking call.
- **Windows Task Manager Blank Icon (`hydra-gui`)**:
  - Fixed the app icon appearing blank in Windows Task Manager and other small-icon contexts by storing icon images below 256×256 as uncompressed DIB entries instead of PNG-compressed ones, which Windows' small-icon GDI path cannot decode.
- **macOS Tray Icon Left-Click (`hydra-gui`)**:
  - Fixed left-clicking the macOS menu-bar tray icon doing nothing on some systems, by updating the `tray-icon` dependency to 0.25, which stops AppKit from swallowing the click before it reaches Hydra.
- **File Properties Start Button Caption (`hydra-gui`)**:
  - The *Start Download* button in the *File Properties* dialog is now captioned by state: *Resume Download* when bytes are already held, *Start Download As New* for a completed file, or *Show Progress* for one already running.
  - Fixed re-downloading a completed file (or redownloading from the list) potentially writing the new data into a stale `.part` file left over from the previous transfer.

---

## [0.5.0] - 2026-09-13

### Added

- **Toolbar Speed Limiter Quick Control & Named Profiles (`hydra-gui`)**:
  - Added a dedicated Speed Limit button with a profile dropdown menu directly to the main toolbar for instantaneous bandwidth throttling.
  - The toolbar button dynamically reflects the active rate cap (e.g. `500 KB/s`, `5 MB/s`) or displays *Speed Limit* when unconstrained.
  - Introduced named speed limiter profiles (*Unlimited*, *Background* at 500 KB/s, *Night* at 5 MB/s) selectable from the toolbar dropdown, native macOS menu, and in-window application menu.
  - Added speed profile management in *Options → Connection*, enabling users to create, rename, adjust rate limits for, and delete custom profiles.
- **Download Table Column Customization & Reordering (`hydra-gui`)**:
  - Added an interactive column manager dialog (*View → Columns* or header context menu) allowing users to hide, show, and reorder download table columns (*File Name*, *Size*, *Status*, *Time Left*, *Speed*, *Last Try Date*, *Description*, *Queue*).
  - Enforced *File Name* as a locked, mandatory column to ensure row identity while all other columns can be toggled freely.
  - Added a *Reset* button to immediately restore default column ordering and visibility.
  - Column visibility preferences and custom widths persist across sessions in `config.toml` (`settings.columns`).
- **Custom Download Category Management (`hydra-gui`)**:
  - Added full category customization in *Options → Save to* and the sidebar categories context menu, allowing users to create, rename, delete, and retype download categories.
  - Supports defining custom file extension lists per category, automatically transferring claimed extensions between categories to avoid classification conflicts.
  - Protected stock built-in categories (*General*, *Programs*, *Video*, *Music*, *Documents*, *Archives*, *Compressed*, *AI Models*) against accidental deletion or renaming.
  - Added strict category name sanitization, enforcing 64-character limits and rejecting cross-platform reserved characters (`/`, `\`, `:`, `<`, `>`, `"`, `|`, `?`, `*`) and directory traversal.
- **Point Font Sizing & Locale-Aware Font Resolution (`hydra-gui`)**:
  - Replaced coarse Small/Medium/Large presets with fine-grained point font sizes from 10 pt to 20 pt in *View → Font*.
  - Added locale-aware typography resolution: pairs Arabic and Persian scripts (`ar`, `fa`) with the bundled `Vazirmatn` font for complete shaping coverage, while selecting the highest-priority installed system font for other locales (`Segoe UI Variable Text`/`Segoe UI`/`Tahoma` on Windows, `SF Pro Text`/`SF Pro`/`Helvetica Neue` on macOS, `Cantarell`/`Ubuntu`/`Noto Sans` on Linux).
  - Added a restart confirmation prompt when switching between locales requiring different font faces.
- **Configurable Progress Connection Details Visibility (`hydra-gui`)**:
  - Added a *"Show connection details"* setting in *Options → Downloads* (`show_conn_details`).
  - Allows download progress dialogs to open in a compact, collapsed view without expanding per-connection transfer segments by default, while retaining the in-dialog button to expand details on demand.
- **Connection Limit Exceptions Management (`hydra-gui`)**:
  - Added an interactive scrollable list in *Options → Connection* allowing users to select and remove per-host connection limit exceptions.
  - Added support for wildcard subdomain matching (`*.example.com`) and domain suffixes (e.g. `uplod.ir`) in connection limit exception rules.

### Fixed

- **Unresponsive Origin Hang & Immediate Stop Cancellation (`hya-net::http`, `hydra-gui::engine`)**:
  - Added a 10-second patience timeout (`HEAD_PATIENCE`) in `probe_resilient` for origins that accept TCP connections but never return an HTTP response or terminate the connection (e.g. `s7.uplod.ir:182`).
  - Automatically falls back to a ranged GET request (`bytes=0-0` / HTTP 206) when HEAD requests time out or fail without headers.
  - Wrapped connection probes, FTP greeting/SIZE commands, and Metalink document retrieval in `cancellable` async wrappers, allowing *Stop* and *Stop All* to immediately terminate downloads stalled in the initial connecting phase instead of hanging indefinitely.
- **Windows Taskbar Theme Detection & Tray Icon Recolor (`hydra-gui::tray`)**:
  - Fixed invisible system tray icons on Windows when Windows mode (taskbar) is set to Dark while application mode is set to Light.
  - Directly queries `SystemUsesLightTheme` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` rather than relying on application-level theme detection.
  - Added asynchronous registry monitoring via `RegNotifyChangeKeyValue` to dynamically re-tint the tray icon between light and dark monochrome glyphs in real-time as the Windows taskbar theme changes.
- **Gecko / Firefox Leftover "Canceled" Downloads (`extensions/`, Firefox)**:
  - Fixed captured browser downloads leaving orphaned "Canceled" entries in Firefox's native download list.
  - Intercepts download responses during `webRequest.onHeadersReceived` (using `webRequestBlocking`) to cancel captured transfers before Firefox creates a download object, keeping the native browser download list clean while allowing uncaptured downloads to proceed untouched.
- **Gecko / Firefox Alt+Click Capture Bypass (`extensions/`, Firefox)**:
  - Fixed Alt+click capture bypass failing on Firefox, where `browser.altClickSave` defaults to false since Firefox 13.
  - Content script now captures Alt+click on Firefox and triggers a native browser download (`chrome.downloads.download`) while skipping Hydra capture.
- **Context Menu Window Boundary Clamping (`hydra-gui::ui::menu`)**:
  - Fixed context menus clipping outside the application window when right-clicking download rows near the bottom or right edges.
  - Implemented dynamic panel height calculation and boundary clamping (`anchor`), opening the menu above the cursor when approaching the bottom edge and shifting left when approaching the right edge, accounting for UI font scaling.

---

## [0.4.4] - 2026-09-11

### Added

- **Per-Download & Global Proxy Routing Engine (`hydra-gui`, `hya-net`)**:
  - Connected the application-wide proxy configuration (*Options → Proxy/Socks*) to actual network transfers across single-file downloads, segmented multi-connection transfers, media stream sniffing, and link probes.
  - Implemented a dedicated `Route` manager in `hydra-gui::proxy`: routes SOCKS proxies (`socks4`, `socks5`, `socks5h`) at the transport connector level via `TlsCapableConnector::with_socks` and routes HTTP forward proxies per-request using absolute URI targets or TLS `CONNECT` tunnels.
  - Added per-download proxy configuration in the *Download File Info* dialog, allowing users to choose between *Default (from Options)*, *None (direct connection)*, or *Custom proxy...* (`socks5://...`, `http://...`) for individual downloads without altering global settings.
  - Added live inline validation in the *Download File Info* dialog to report syntax and configuration errors for custom proxy addresses prior to starting downloads.
  - Implemented live reload: updating proxy settings in the Options dialog immediately takes effect for all subsequent network transfers without requiring an application restart.
  - Synchronized proxy UI settings and validation messages across all 30 supported languages.
- **Expiring Signed URL Detection & Automatic Re-Resolution (`hya-net::signed`, `extensions/`, Chrome, Firefox, Safari)**:
  - Added short-lived signed URL detection in `hya_net::signed` supporting AWS SigV4 (`X-Amz-Date` and `X-Amz-Expires`), CloudFront, and Google Cloud Storage signed URLs (`Expires` epoch timestamp).
  - Updated browser extensions (`v0.3.4`) to inspect resolved URLs: if a signed URL has a short expiration window (within 1 hour, or as brief as 10 seconds), the extension forwards the original redirecting page URL alongside session cookies to Hydra instead of the ephemeral signed link.
  - Enables downloads to reliably refresh and mint fresh signatures on retries, pause/resume, or reconnection rather than permanently failing with HTTP 403 Forbidden or 410 Gone when credentials expire.
- **WAF & Bot Challenge Detection and Retry Handling (`hya-net::http`, `hydra-cli`)**:
  - Enhanced probe and transfer validation (`Probe::refusal`) to detect bot challenges (such as AWS WAF returning HTTP 202 Accepted with empty content lengths) and prevent them from being mistaken for valid 0-byte file downloads.
  - Added automatic server error detail extraction: parses and extracts human-readable failure explanations directly from XML and short response bodies (such as S3 `<Message>Request has expired</Message>`) instead of presenting generic HTTP status codes.
  - Added automatic challenge retry handling in `hydra-net` prior to refusing links.
- **Multi-Distribution Ubuntu PPA Support (`packaging/debian/`, `.github/workflows/release.yml`)**:
  - Expanded Launchpad PPA packaging and release workflows to build and publish source packages for multiple Ubuntu distributions (including Ubuntu 24.04 Noble, 22.04 Jammy, and 20.04 Focal).

### Fixed

- **NSIS Installer & Uninstaller User PATH Protection (`scripts/windows/hydra-installer.nsi`, `scripts/windows/set-user-path.ps1`)**:
  - Fixed a critical regression on Windows where the NSIS installer or uninstaller wiped the per-user `PATH` environment variable if the existing `PATH` exceeded the 1024-character `NSIS_MAX_STRLEN` limit.
  - Replaced native NSIS string operations with a companion PowerShell helper script (`set-user-path.ps1`) utilizing .NET registry APIs without string length constraints.
  - Preserves unexpanded `%VAR%` references (`REG_EXPAND_SZ`), retains existing entry casing and ordering, performs idempotent directory addition and removal, and broadcasts `WM_SETTINGCHANGE` so running shells and Windows Explorer immediately update their environment without requiring user logoff.
- **Native Messaging Host Breakaway & GUI Capture Lifecycles (`crates/hydra-host`, `crates/hydra-gui`, `extensions/`)**:
  - Fixed captured browser downloads aborting when Hydra GUI is launched on demand by the native messaging host on Windows. Browsers run native messaging hosts inside Windows Job Objects and terminate all processes in the job when the host exits after answering `sendNativeMessage`.
  - Configured `hydra-host` to launch `hydra-gui` using `CREATE_BREAKAWAY_FROM_JOB` (falling back gracefully if breakaway is restricted) so the desktop GUI process outlives the host.
  - Updated `extbus` so download capture is acknowledged only after the UI thread has successfully placed the item into the download list; if unreachable, the download is handed back to the browser to resume untouched.
  - Configured panic hooks in GUI builds to write complete backtraces to `gui.log` when standard I/O streams are detached.
  - Added automated test suites covering native messaging framing, IPC lock handling, and GUI process launch.
- **Gecko / Firefox Irreversible Download Pause (`extensions/`, Chrome, Firefox, Safari)**:
  - Stopped parking downloads on Gecko (Firefox), where `downloads.pause()` cannot be undone programmatically, allowing downloads to remain active during capture evaluation and gracefully resume in the browser if Hydra does not accept the transfer.
  - Added service worker console debugging in browser extensions detailing why downloads are bypassed or handed back to the browser.
- **False-Positive Existing File Warnings on Browser Captures (`crates/hydra-gui`)**:
  - Fixed the Add Download dialog erroneously reporting "A file with this name already exists" when capturing downloads from Firefox.
  - Firefox creates a 0-byte file placeholder under the final filename while actively writing data to an adjacent `.part` file; updated `collision_file` to ignore empty files, preventing false collision warnings for browser reservations that are deleted once Hydra assumes the transfer.
- **UI Font Ratio Double-Scaling & Display Boundary Clamping (`crates/hydra-gui`)**:
  - Fixed an issue where saved window dimensions in OS points were scaled by the UI font ratio twice upon relaunch, causing windows to expand on every start until exceeding bounds and resetting.
  - Added `fit_to_display` logic to clamp fixed-size secondary dialogs (Configuration, Scheduler, Batch Download, Progress) to usable display bounds across high-DPI screens and custom font scales, preventing action buttons (OK/Cancel) from being pushed off-screen.
- **Start Progress Dialog Minimized Setting (`crates/hydra-gui`)**:
  - Fixed the *"Start download progress dialog minimized"* setting being ignored when starting downloads manually.
  - Deferred window minimization to the `WindowOpened` event when the native window handle is valid, and prevented subsequent focus acquisition from unminimizing the progress window.
- **Download File Info & File Properties Layout Refinements (`crates/hydra-gui`)**:
  - Reorganized the *Download File Info* and *File Properties* dialog layout:
    - Grouped immutable properties (*Status*, *Size*, *Last try date*, and *Result* error message) into a clean read-only block at the top of the dialog for existing downloads.
    - Centered the category file-type icon and archive ZIP preview button in the side column alongside input fields.
    - Moved dialog action buttons to a dedicated footer bar spanning the dialog width, keeping buttons centered relative to the window and eliminating empty margins.
    - Dynamically computed dialog heights based on rendered rows to prevent control clipping.
    - Standardized field label colon punctuation across all language localizations.
- **Windows Installer Extension Version Documentation (`scripts/build-windows-installer.sh`, `scripts/windows/hydra-installer.nsi`)**:
  - Corrected Windows installer packaging to dynamically populate the actual browser extension version in `INSTALL.txt` during build time.

---

## [0.4.3] - 2026-09-08

### Added

- **Configurable Application Directory & Portable GUI Profile (`hydra-gui`, `README.md`)**:
  - Added `--config <DIR>` (and `--config=<DIR>`) command-line flag to run the desktop application from a custom directory.
  - Relocates `config.toml`, download state database (`state.redb`), logs (`logs/`), and localization catalogs (`locales/`) into the specified directory instead of the platform default (`~/.config/hydra` on Linux/macOS or `%APPDATA%\hydra` on Windows).
  - Automatically resolves relative paths against the launch working directory and creates target directories if they do not exist.
  - Enables portable installations (e.g. on external drives) and multiple isolated profiles that run concurrently alongside standard installations with separate single-instance locks (`ipc.json`).
  - Automatically passes `--relaunch-arg --config --relaunch-arg <DIR>` to the update finisher to preserve the custom configuration directory across self-updates.
  - Protects machine-wide user registrations by leaving startup login items (`autostart.rs`) and native messaging host manifests (`nmhost.rs`) tied to the default installation, while allowing browser extensions to communicate with running portable profiles via WebSocket.
- **Configurable Keyboard Shortcuts (`hydra-gui`)**:
  - Expanded the shortcut manager (`SHORTCUT_ACTIONS`) with configurable keybindings:
    - **Select All Downloads** (`Cmd+A` / `Ctrl+A`): Selects all items in the active download list.
    - **Remove Selected Downloads** (`Cmd+Alt+R` / `Ctrl+Alt+R`): Prompts for confirmation before removing selected downloads from the list, matching toolbar and menu delete actions.
    - **Close Window** (`Cmd+W` / `Ctrl+W`): Closes the focused secondary dialog or active window.
    - **Exit Hydra** (`Cmd+Q` / `Ctrl+Q`): Flushes the download list and configuration to disk before quitting.
  - Added platform quit conventions: built-in `Alt+F4` support on Windows and custom AppKit `Cmd+Q` handling on macOS ensuring clean state persistence prior to termination.
  - Made the *Shortcuts* configuration dialog scrollable (`scrollable`) and increased its default window dimensions (`520x520`) to comfortably accommodate the expanded action list and UI font scaling.

### Fixed

- **HTTP Referer Header Propagation & Hotlink CDN Protection (`hydra-gui`, `extensions/`, Chrome, Firefox, Safari)**:
  - Fixed download failures (HTTP 403 Forbidden) on hotlink-protected CDNs that require an originating `Referer` header.
  - Updated browser extensions (bumped to version `0.3.2`) to capture the source page URL and pass it across the extension bus.
  - Propagated referers through the GUI and download engine: `ExtDownload` → `DownloadItem::referer` → `StartSpec::referer` → `request_headers()`.
  - Supplied captured referer and session request headers to `probe_link`, ensuring the *Download File Info* dialog queries protected endpoints correctly and resolves exact file sizes instead of reporting unknown sizes (`?`).
- **Sticky Download Table Header & Grid Scrolling (`hydra-gui`)**:
  - Pinned the download table header to the top of the viewport using a layered stack layout (`stack![rows, head]`), keeping column titles visible while scrolling through long download lists.
  - Decoupled the ruled empty grid (filler rows) from the scrollable content container, eliminating artificial scroll height and ensuring vertical scrollbars only display when the download list actually overflows the window.
  - Synchronized horizontal scroll offsets (`table_scroll_x`) with background grid hairlines to maintain perfect vertical line alignment during horizontal panning.
- **System Tray Icon Left-Click Activation (`hydra-gui`)**:
  - Fixed an issue on Windows and macOS where left-clicking the tray icon opened the context menu over the main window.
  - Disabled `with_menu_on_left_click(false)` in `muda` / `tray-icon` so that left-clicking restores and focuses the main window, while right-clicking reveals the tray context menu, matching platform conventions and Linux behavior.
- **Auto-Dismiss Complete Dialog on "Open Folder" (`hydra-gui`)**:
  - Fixed the download completion dialog remaining open in the background after clicking "Open Folder".
  - Dismisses the dialog automatically upon opening the destination folder (`WinKind::Complete(id)`), matching the behavior of the "Open" file action.
- **Options Layout & Background Download Toggle (`hydra-gui`)**:
  - Surfaced the *"Download in background while choosing options"* toggle in *Options → Downloads* with contextual tooltips, allowing users to choose whether downloads start immediately upon link addition or wait for dialog confirmation.
  - Reordered General settings in the Options dialog to place the Dock/taskbar visibility setting directly alongside *"Close to system tray"*.
  - Localized the background download setting across all 30 languages.

---

## [0.4.2] - 2026-09-05

### Added

- **Remote ZIP Archive Inspection & Central Directory Preview (`hya-net`, `hydra-cli`, `hydra-gui`, `docs/man/hydra.1`)**: Added instant inspection and previewing of remote ZIP archives without downloading the archive body:
  - **Tail-Byte Central Directory Parser (`hya-net::zipdir`)**: Implemented `fetch_small_range` and `fetch_listing` in `hydra-net` to safely fetch bounded byte ranges from the tail of remote archives using HTTP range requests (`Range: bytes=...`). Parses End of Central Directory (EOCD) records, ZIP64 EOCD locators and records, and central directory headers with support for archives larger than 4 GB (ZIP64), UTF-8 / CP437 character encodings, variable-length archive comments, and self-extracting (SFX) executables in 1 or 2 small requests.
  - **CLI Archive Preview (`hydra-cli`)**: Added `--preview <url>` flag to probe and display remote ZIP contents directly in the terminal, rendering formatted tables with uncompressed file sizes, compressed packed sizes, modification timestamps, and relative file paths. Added documentation and command-line examples to the `hydra.1` man page.
  - **GUI ZIP Preview Dialog (`hydra-gui`)**: Added an interactive *Preview* action button to the *Download File Info* dialog when adding ZIP files. Opens an inspection modal (`WinKind::ZipPreview`) listing archive files with dedicated type icons, uncompressed and packed sizes, and modification dates, parented to the active file info window.
- **Queue Folder Color Badges (`hydra-gui`)**: Added colored folder badges across the scheduler queue list, categories sidebar, and download table to visually distinguish queues and categories at a glance.

### Fixed

- **Dialog Window Parenting & Window Layering (`hydra-gui`)**: Fixed secondary dialogs (Add URL, Options, Download File Info, ZIP Preview, Permissions) slipping behind the main window or opening in duplicate:
  - Implemented platform-native window parenting via raw window tokens: child `NSWindow` on macOS (`addChildWindow_ordered`), `GWLP_HWNDPARENT` on Windows (`SetWindowLongPtrW`), and `WM_TRANSIENT_FOR` via `x11rb` on Linux/X11.
  - Fixed Linux/X11 build compilation by importing `x11rb::wrapper::ConnectionExt` for `change_property32`.
- **GUI Dialog Layout, Styling & Sizing Enhancements (`hydra-gui`)**:
  - **Download File Info Dialog**: Overhauled layout with unified "Save As" path input and browse button, indented category path labels and greyed-out default directory display, cleaner spacing, and centered dialog action buttons. Removed redundant in-window header banner in favor of the OS title bar.
  - **Save As Path Retention Guard**: Disabled automatic global path retention by default (`remember: false`) in Download File Info dialogs; user-modified paths now apply only to the specific download unless explicitly checked. Correctly labels the destination category as General when category subdirectories are disabled.
  - **Add URL Dialog**: Removed redundant duplicate header, aligned form labels, and kept authentication credential inputs rendered and dimmed until toggled to avoid disruptive dialog layout shifts.
  - **Permissions & Scheduler Dialogs**: Moved Permissions *Refresh* action to the bottom button bar and compacted permission entries. Balanced queue button widths, refined row and day column spacing, and aligned action buttons in the Scheduler dialog.
  - **Segment Stream Progress State**: Displayed *Stop* instead of *Pause* for segment and media stream downloads in the progress window.
  - **Dialog Dimensions**: Adjusted default window dimensions across Add URL, Complete, About, Permissions, and Power dialogs.
- **Sub-Menu Overflow Scrolling (`hydra-gui`)**: Wrapped GUI sub-menus containing more than 10 items (notably the *Language* menu with 30+ locales) in a scrollable container (`scrollable`), preventing long flyouts from extending past window boundaries.
- **Browser Extension Floating Panel Dismissal & Scoping (`extensions/`, Chrome, Firefox, Safari)**:
  - Fixed floating download panel close button clicks being intercepted as pointer drag events, allowing single clicks to reliably dismiss the panel.
  - Scoped video player dismissal tracking to individual DOM elements using a `WeakSet` instead of a page-wide boolean, preventing closing a download bar on one video from hiding download bars on subsequent videos in infinite-scroll feeds (e.g., X/Twitter).
  - Updated close button tooltip to *"Hide for this player"* and bumped extension manifest versions to `0.3.1`.

### Changed & Refactored

- **Multi-Language Localization Synchronization (`hydra-gui`)**: Synchronized new translation catalogs for remote ZIP archive previewing (column headers, entry counts, preview buttons, and status verdicts) across all 30 supported locales (`ar`, `cs`, `da`, `de`, `el`, `en`, `es`, `fa`, `fi`, `fr`, `he`, `hi`, `hu`, `id`, `it`, `ja`, `ko`, `nl`, `pl`, `pt-BR`, `pt`, `ro`, `ru`, `sv`, `th`, `tr`, `uk`, `vi`, `zh-hant`, `zh`).

---

## [0.4.1] - 2026-09-04

### Added

- **Metalink 3.0 & RFC 5854 / RFC 6249 Multi-Source Mirror Engine (`hya-net`, `hya-core`, `hydra-cli`, `hydra-gui`, `hydra-ffi`)**: Introduced native Metalink support enabling robust multi-mirror range assembly, dynamic reserve benches, and localised chunk repair:
  - **Dual-Dialect Parser & RFC 6249 Discovery**: Added a zero-heavy-dependency XML pull parser supporting Metalink 3.0 (`.metalink`) and Metalink 4.0 / RFC 5854 (`.meta4`) documents from disk, direct URLs, or dynamic mirror redirectors. Automatically discovers duplicate mirror endpoints via HTTP `Link: <...>; rel=duplicate` headers during connection probing.
  - **Multi-Source Range Splicing & Exterior Verification**: Assembles byte ranges across disparate mirror hosts without requiring shared `ETag` validators by anchoring verification to exterior document digests and sizes.
  - **Dynamic Mirror Reserve Bench**: Enforces politeness socket budgets while maintaining secondary mirrors on a reserve bench; when an active mirror fails or throttles mid-transfer, connections hot-swap to reserves in place without stranding assigned ranges.
  - **Localised Piece Verification & Repair**: Validates per-piece checksums (`<pieces>`) on the fly as data lands; detects corrupted segments and repairs them individually from alternate mirrors without re-downloading the entire file.
  - **CLI `metalink` Subcommand & Filtering Flags**: Inspect Metalink documents (`hydra metalink <target>` or `--json`) and filter candidate mirrors by geography (`--metalink-location`), operating system (`--metalink-os`), protocol (`--metalink-preferred-protocol`, `--metalink-enable-unique-protocol`), or target filename (`--metalink-file`).
  - **GUI Metalink Inspection Modal**: Automatically detects Metalink documents in the *Add URL* dialog, presenting an inspection view showing contained files, sizes, usable mirror counts, and piece hash availability before starting downloads.
  - **C ABI Metalink API (`libhydra`)**: Exposed `hydra_metalink_parse`, `hydra_metalink_doc_t`, and multi-mirror job creation functions in the frozen C ABI.
- **GUI Batch Download Dialog Enhancements (`hydra-gui`)**: Overhauled the batch download window with advanced management features:
  - **Sortable Multi-Column Table**: Added clickable column headers with directional sorting indicators for *File Name*, *File Type*, *Size*, *Download from*, and *Save to*.
  - **Content Filters**: Added quick-filter toggles to *"Hide HTML files"* and *"Hide duplicate links"*.
  - **Bulk Selection Controls**: Added master header checkbox to select or deselect all visible items in one click.
  - **Inline Output Folder Selector**: Added a directory browse button directly in the dialog for assigning custom destination paths to selected items.
  - **Selection Summary**: Displays total count of selected items and aggregate download size in the dialog footer.
- **Power Management: System Sleep & Safety Countdown Prompt (`hydra-gui`)**:
  - Added system **Sleep** option alongside Shutdown and Log Off upon download completion in the progress window and queue scheduler.
  - Added an interactive 30-second cancellable countdown prompt before executing power actions (sleep, shutdown, log off), preventing accidental interruptions.
- **Remove Completed Downloads Option (`hydra-gui`)**: Added an option under *Options → Downloads* (*"Remove completed downloads from download list"*) and a per-download checkbox in the Progress completion tab to automatically prune finished items from the download list upon completion or dialog dismissal.
- **Expanded Internationalization to 30 Locales (`hydra-gui`)**:
  - Added 15 new language translations: Czech (`cs`), Danish (`da`), Greek (`el`), Finnish (`fi`), Hindi (`hi`), Hungarian (`hu`), Indonesian (`id`), Italian (`it`), Polish (`pl`), Portuguese (`pt`), Romanian (`ro`), Swedish (`sv`), Thai (`th`), Ukrainian (`uk`), and Vietnamese (`vi`).
  - Completely synchronized translations for Metalink, batch filters, power sleep actions, dormant connection reasons, and concurrency measurement strings across all 30 supported locales.
- **Chocolatey Windows Package Distribution (`packaging/choco`)**: Added official Chocolatey packaging files (`hydra-download-manager.nuspec`, automated install/uninstall scripts) for Windows package management via `choco install hydra`.
- **Transfer Benchmarks & Automated Testbed Suite (`docs/`, `scripts/benchmark/`)**: Added comprehensive benchmark documentation and testbed tooling measuring HYDRA against `aria2c`, `wget`, and `curl` over 100 ms high-latency paths and public mirror endpoints, demonstrating higher average speed (414 MB/s with `-x 8`), flat 6.9 MiB peak memory, and minimal CPU time (1.48 s). Added reproducible testbed scripts (`bed.sh`, `server-benchmark.sh`, `plot_bed.py`, `plot_srv.py`).

### Fixed

- **International Script & RFC 6266 Content-Disposition Resolution (`hya-net`, `hydra-gui`)**:
  - Replaced naive substring matching with a full RFC 6266 parameter parser that prioritizes RFC 5987 / RFC 6266 §4.3 encoded `filename*` parameters over plain `filename=`.
  - Added UTF-8 percent-decoding for non-ASCII filenames across CJK, Arabic, Persian, Cyrillic, and Hebrew scripts, eliminating issues where cloud storage IDs were assigned as fallback names.
  - Stripped malicious bidirectional Unicode overrides (`U+202E` RLO, isolates, marks, BOM) to prevent file extension spoofing while preserving legitimate script characters such as Persian zero-width non-joiners (`U+200C`).
  - Replaced lossy Latin-1 fallback with proper charset rejection to prevent filename corruption on legacy encodings.
  - Clamped filenames by byte boundary (<= 255 bytes) while preserving the file extension to prevent `ENAMETOOLONG` filesystem errors on multi-byte UTF-8 scripts.
- **User-Chosen Filename Preservation & Conflict Guarding (`hydra-gui`)**:
  - Added `name_locked` tracking to ensure user-edited names in File Info or Add URL dialogs are preserved and never overwritten by subsequent probe `Content-Disposition` headers.
  - Eliminated false "file already exists" warnings on non-file URLs or flat-folder downloads, and ensured duplicate confirmation dialogs display the full destination file path.
- **File Info Dialog Edits on Fast Background Completion (`hydra-gui`)**: Fixed a race condition where files that finished downloading before the File Info dialog was dismissed lost user edits; edits are now adopted upon confirmation and output files are renamed accordingly.
- **Rate Limiter Quantization Bottleneck & High-Bandwidth Throughput (`hya-net`)**: Fixed throughput capping at ~78–89 MiB/s when configuring high rate limits (200–400 MiB/s). Introduced a 10 ms burst window (`BURST_WINDOW`) crediting timer overshoots back to avoid over-throttling on OS timer tick quantization, and deferred sub-tick debts (< 2 ms) to subsequent reservations in `Pace::wait`.
- **Silent Origin Starvation Detection & Cap Step-Down (`hya-net`, `hya-core`)**: Detected origins that accept TCP handshakes and HTTP request headers but stream data to only a subset of connections while starving the rest; dynamically steps down connection concurrency (similar to HTTP 429 backoff) rather than hanging or stalling the transfer.
- **HTTP Status Reason Phrases & Probe Error Rejection (`hydra-cli`, `hya-net`, `hydra-ffi`)**: Added `describe_status` helper to format standard HTTP reason phrases. Rejects probe responses with HTTP status >= 400 immediately with human-readable error messages rather than attempting multi-range downloads on HTTP error pages.
- **macOS GUI Auto-Start Service Registration (`hydra-gui`)**: Fixed launch agent path and service registration to ensure reliable automatic startup on macOS.
- **Silent NSIS Desktop Shortcut Creation (`packaging/windows`)**: Enabled desktop shortcut creation by default during silent Windows NSIS installer runs.

### Changed & Refactored

- **Connection Performance & Core Scheduler Rework (`hya-core`, `hya-net`, `hydra-cli`)**: Comprehensively overhauled the connection scheduler, rate limiter, and async I/O loops to achieve **high average download speed**, **low peak memory**, and **low CPU time**:
  - **High Average Speed**:
    - *TCP Slow-Start Aware Collapse Detector*: Introduced short/long EWMA rising ratio detection in `CollapseDetector` (`is_slow_start`) to differentiate initial connection ramp-up from throughput stalls. Connections must be warm and settled before being evaluated as repair victims or takers, enforcing persistent divergence before stealing ranges and ensuring connections issue exactly one request per chunk with zero redundant repairs.
    - *Rate Limiter Pacing Overhaul*: Carries sub-tick debts and credits timer overshoots back in `Pace`, preventing quantization stalls on 1 ms OS timers and unlocking full gigabit line saturation (reaching 414 MB/s over 100 ms paths).
    - *Initial RTT Probe Seeding*: Captures probe latency (`first_rtt`) to seed realistic connection setup cost deltas into the scheduler.
    - *Silent Starvation Recovery*: Dynamically steps down concurrency ceilings when servers stall surplus streams, maintaining uninterrupted throughput.
  - **Low Memory Peak**:
    - *Strict Resident Memory Ceiling*: Streaming architecture and connection pooling maintain a flat **6.9–8.0 MiB peak memory footprint** across multi-gigabyte transfers, consuming less than a third of the memory of `aria2c` (21.1–25.3 MiB) and `curl` (10.9–23.1 MiB).
    - *Gated Stream Checksum Hashing*: Made SHA-256 calculation opt-in via `--print-checksum`, avoiding streaming buffer allocations and memory bloat on multi-stream jobs.
  - **Low CPU Time**:
    - *Decoupled Ticker-Based Transfer Loop*: Decoupled `run_transfer_with_reserves` from packet arrival wakeups, replacing per-arrival scheduler iterations with a dedicated ticker loop. Eliminates tens of thousands of redundant scheduler passes and thread context switches per transfer.
    - *Single-Worker Tokio Runtime*: Defaulted CLI async runtime to a single worker thread (`HYDRA_WORKERS=1` by default), eliminating cross-core thread bouncing, cache contention, and synchronization overhead.
    - *Measured CPU Efficiency*: Achieved **1.48 s total CPU time** on a 1 GB download at 414 MB/s (compared to **2.52 s** for `aria2c` and **3.21 s** for `curl`).
- **Connection Measurement Diagnostics & Dormant States (`hydra-gui`, `hya-core`)**: Introduced `LimitReason` enum (`Measuring`, `Settled`, `ServerLimit`, `SilentStarvation`) to track why connection counts sit below politeness budgets. The GUI connection list now displays explicit dormant connection labels (*"Waiting (measuring)..."*, *"Not used (measured slower)"*, *"Not used (server limit)"*) and temporary status notifications detailing concurrency search verdicts.

---

## [0.4.0] - 2026-08-31

### Added

- **HLS and MPEG-DASH Streaming Engine (`hya-stream`, `crates/hydra-stream`)**: Introduced `hya-stream`, a standalone media streaming engine and manifest parser designed for high-performance chunk assembly and live stream capture:
  - **HLS Protocol Support**: Full parser for Master and Media playlists (`#EXTM3U`, `#EXT-X-STREAM-INF`, `#EXT-X-MEDIA`, `#EXT-X-MAP`, `#EXT-X-KEY`, `#EXT-X-BYTERANGE`, `#EXT-X-DISCONTINUITY`), audio/video/subtitle rendition grouping, live sliding windows, and AES-128-CBC encrypted segment decryption.
  - **MPEG-DASH Support**: Complete MPD XML parser supporting multi-period manifests, AdaptationSets, Representations, SegmentTemplates (dynamic variable substitution for `$Number$`, `$Time$`, `$RepresentationID$`, SegmentTimeline, SegmentList, SegmentBase), initialization segments, and multi-track demuxing.
  - **Segment Assembly Pipeline**: High-throughput asynchronous segment fetching with connection pooling, in-order segment sequencing, live stream recording with duration limits, and automated container packaging/muxing (MP4, MKV, TS, AAC, MP3) using FFmpeg.
- **CLI Media Streaming & Inspection Commands (`hydra-cli`)**: Added stream downloading and inspection capabilities to the CLI:
  - `--stream` / `--stream-quality <QUALITY>`: Select target stream resolution and quality (e.g. `best`, `1080p`, `720p`, `480p`, `360p`, `worst`, `audio_only`, or custom bitrate).
  - `--stream-container <CONTAINER>`: Specify output media container format (`mp4`, `mkv`, `ts`, `aac`, `mp3`).
  - `--stream-inspect` / `-i`: Probe remote HLS/DASH streams and display formatted tables of available video/audio streams, bitrates, resolutions, codecs, and durations without downloading.
  - `--stream-duration <DURATION>`: Set maximum recording duration for live streams (e.g., `30m`, `2h`).
  - `--stream-live`: Force live stream capture mode.
  - `--stream-audio-track <TRACK>` / `--stream-video-track <TRACK>`: Explicitly select target audio and video track IDs.
- **`hya` Command Shorthand & Unified Symlinks (`hydra-cli`, packaging targets)**: Added `hya` as an official first-class binary command and symlink alias across all installation methods and package formats (Arch Linux AUR, Debian/Ubuntu `.deb`, Fedora `.rpm`, Linux AppImage, macOS `.dmg`/`.pkg`, Homebrew formula, Windows NSIS installer, and shell/PowerShell install scripts). Generated native shell completion scripts for both `hydra` and `hya` aliases (Bash, Zsh, Fish, PowerShell, Elvish) and added the `hya.1` man page.
- **GUI Stream Probing & Media Track Selection (`hydra-gui`)**:
  - Integrated automated stream probing into the *Add URL* dialog when pasting HLS (`.m3u8`) or DASH (`.mpd`) links, presenting stream variant details (resolutions, bitrates, codecs), audio track selection dropdowns, and recording duration limit inputs.
  - Added stream quality and container format preference controls directly in the *Batch Download* window.
  - Added stream progress tracking in the *Download Progress* window, displaying live stream indicators, elapsed recording duration, segment counts, and active FFmpeg muxing status.
- **Browser Extension Stream Sniffing & Floating Video Bar (`extensions/`, Chrome, Firefox, Safari, Edge, Brave)**:
  - Bumped browser extensions to v0.3.0 with live HLS (`.m3u8`) and MPEG-DASH (`.mpd`) request sniffing and media stream capture.
  - Added an in-page floating video download bar overlaid on HTML5 video elements for instant one-click downloading.
  - Upgraded popup UI with media filter tabs (All, Videos, Audio, Documents, Images), live stream badges, quality selector dropdowns, and copy link actions.
  - Redesigned onboarding welcome page (`welcome.html`) with real-time native messaging host connection testing and troubleshooting guides.
  - Added support for Flatpak and Snap browser profile locations on Linux.
- **Linux AppImage Packaging & In-App Self-Updates (`scripts/package-appimage.sh`, `hydra-updater`, CI)**:
  - Added official multi-architecture Linux AppImage releases (`x86_64` and `aarch64`) bundling all required dependencies, desktop integration, icons, and native messaging host manifests.
  - Added in-app AppImage self-updating via `hydra-updater` with `zsync` differential binary updating, atomic single-file replacement, and privilege elevation support (`pkexec` / `sudo`) for system installations.
  - Integrated AppImage autostart registration and desktop launcher creation.
- **Structured GUI Logging & In-App Log Viewer (`hydra-gui`)**:
  - Added high-performance asynchronous structured logging with diagnostic startup banners (OS version, architecture, Hydra build, graphics/desktop environment).
  - Added automatic real-time sensitive data redaction to sanitize authentication tokens, authorization headers, passwords, and private query parameters.
  - Added an interactive in-app Log Viewer modal accessible via *Help → View Logs* and the native macOS menu bar, featuring log level filtering (Trace, Debug, Info, Warn, Error), text search, and clipboard export.
  - Added a direct *Help → Report Issue* action to streamline bug reporting with environment context.
- **Close to System Tray Option (`hydra-gui`)**: Added a *Close to system tray instead of exiting* option under *Options → General*, enabling users to minimize the application to the system tray upon closing the main window without interrupting active downloads.
- **Automatic Native Messaging Host Installer (`hydra-gui`)**: Added automated browser native messaging host manifest registration on GUI startup for Chrome, Chromium, Firefox, Microsoft Edge, Brave, Vivaldi, Opera, and Tor, with connectivity status in *Options → Extensions*.
- **Turkish Localization (`hydra-gui`)**: Added full Turkish (`tr`) language translation catalog across all menus, dialogs, settings, and notifications, expanding total supported GUI locales to 15.
- **Documentation & Man Pages**: Added `hydra-host.1` man page for the native messaging host protocol, `crates/hydra-stream/README.md` for stream engine architecture, and updated Linux AppImage guides.

### Fixed

- **Remote File Modification Timestamp Preservation (`hydra-gui`)**: Fixed timestamp loss on completed downloads by extracting the `Last-Modified` HTTP header independently of cache validators and applying the exact remote modification timestamp to downloaded files (`set_mtime`) after final renaming when the *Preserve file date/time* setting is enabled.
- **Fedora Copr Dynamic Version Rendering (`scripts/render-rpm-spec.sh`, CI)**: Fixed Fedora Copr SRPM build failures by dynamically rendering the version macro from Cargo manifests into `hydra.spec` before generating source RPMs.
- **Cryptographic Key Safety in Test Suites (`hydra-stream`)**: Eliminated hard-coded cryptographic test keys and constant initialization vectors in unit tests.
- **Crates.io Publishing Pipeline for Stream Engine (`.github/workflows/publish-crates.yml`, CI)**: Added `hya-stream` to the crates.io publishing workflow, dependency verification, and index synchronizers.

### Changed & Refactored

- **Pooled Object Fetching & Typed Redirects (`hya-net`)**: Added `fetch_object` to `hydra-net` for efficient pooled HTTP connection reuse during single-object and playlist fetches, along with typed `RedirectError` for explicit hop limit and redirect loop handling.
- **Multi-Language Localization Updates (`hydra-gui`, `extensions/`)**: Comprehensively updated and synchronized translation strings across all 15 supported locales (`ar`, `de`, `en`, `es`, `fa`, `fr`, `he`, `ja`, `ko`, `nl`, `pt-BR`, `ru`, `tr`, `zh`, `zh-Hant`), including a French translation overhaul and new strings for stream downloads, FFmpeg status, log viewer, close-to-tray, and extension diagnostics.

---

## [0.3.14] - 2026-08-29

### Added

- **Post-Download Antivirus Scanning (`hydra-gui`)**: Added integrated antivirus scanning for completed downloads. When configured under *Options → Downloads* with a virus scanner executable and arguments (supporting `%1` file path substitution), finished files are scanned prior to final completion. The progress window displays live scanner console output, an indeterminate marquee progress bar during scans, explicit status verdicts (*Clean*, *Infected*, or *Scan Error*), and interactive decision buttons (*Skip*, *Keep file*, or *Delete file*).
- **Automated Computer Shutdown and Logout (`hydra-gui`)**: Added support for automatically shutting down or logging off the computer when downloads finish. Can be enabled on individual downloads in the *Progress → Completion* tab, or scheduled for entire download queues in the *Scheduler* window upon completing all queued tasks.
- **Traditional Chinese Localization (`hydra-gui`)**: Added full Traditional Chinese (`zh-Hant`) language catalog, bringing total supported GUI locales to 14.
- **Site Block List Auto-Start Warning & Guarding (`hydra-gui`)**: Added domain and subdomain pattern matching against the *Don't start downloading automatically from following sites* block list. Displays an inline warning banner in the *Add URL* dialog for matching sites, and ensures URLs from blocked sites are added in a paused/queued state rather than auto-starting across single, batch, and browser extension captures.
- **Inline Double-Click Queue Renaming (`hydra-gui`)**: Added ability to rename custom download queues by double-clicking them directly in either the Categories sidebar or the Scheduler queue list, with protection for built-in queues (*Main download queue* and *Synchronization queue*).

### Fixed

- **Connection Admission by Live Count under Throttling (`hya-core`, `hya-net`)**: Fixed scheduler concurrency management under server rate-limiting and HTTP 429/503 errors. Replaced index-based slot gating with live admitted connection counts (`admitted()`), ensuring active high-index connections are not prematurely retired while lower-index connections cool down. Prioritized proven connections over unproven ones when reallocating freed slots to eliminate connection thrashing.
- **Work-Conserving Assignment Reserve Preservation (`hya-core`)**: Restored the unassigned work reserve during concurrency ramping so idle connections draining their quotas do not prematurely consume the remaining file chunks, preventing costly range stealing and repair cycles for subsequent connections while preserving maximal-range assignments for settled fixed-concurrency transfers.
- **Throttling Cooldown Synchronization (`hya-net`)**: Synchronized the scheduler's settling gate duration (`cap_settled_until`) with connection backoff timers, ensuring upward concurrency probing and retry bursts are properly evaluated rather than dropped.
- **FFI Config Struct Bounds Safety (`hydra-ffi`)**: Fixed struct size offset validation in `engine_cfg` conversion to prevent invalid unaligned pointer reads when inspecting header flags.
- **GUI Double-Click Event Handling (`hydra-gui`)**: Replaced intercepted mouse area events on button rows with internal double-click interval tracking, resolving issues where double-clicks were not detected on queue items.

### Changed & Refactored

- **Path-Scoped Site Login Credential Resolution (`hydra-gui`)**: Improved site credential lookup under *Options → Sites Logins* to support host, subdomain wildcard (`*.example.com`), and longest-path prefix matching (e.g. `example.com/private` taking precedence over `example.com`).
- **Multi-Language Localization Updates (`hydra-gui`)**: Comprehensive updates and refinements to German (`de`), Korean (`ko`), and Spanish (`es`) translations, along with synchronized missing status and column strings across all 14 supported locales.

---

## [0.3.13] - 2026-08-25

### Added

- **System Theme Support & Theme Selector (`hydra-gui`)**: Added full OS system theme detection and dynamic appearance switching. Replaced the binary *View → Dark Mode* toggle with a *View → Theme* submenu offering *System Default*, *Light*, and *Dark* modes across both in-window and native macOS menu bars, with automatic migration from legacy dark mode settings.
- **Configurable Category Subdirectory Creation (`hydra-gui`)**: Added an option under *Options → Save To* (*"Do not create category folders — save everything in the default folder"*) to allow users to disable automatic creation of per-category subdirectories (e.g., `Downloads/Video`, `Downloads/Documents`) and save all new downloads directly into the general download directory.
- **Hebrew & Portuguese (Brazil) Localizations (`hydra-gui`)**: Added full language catalogs for Hebrew (`he`) and Portuguese (Brazil) (`pt-BR`), bringing total supported GUI locales to 13.

### Fixed

- **Adaptive Connection Handling & Recovery under Origin Throttling (`hya-core`, `hya-net`)**: Prevented catastrophic slowdowns and excessive round-trips when servers enforce strict connection limits or return HTTP 429 (Too Many Requests) / 503 errors. Added `conn_ceiling` in `Scheduler` to size range shares against reachable concurrency rather than total budget; added `clamp_max` to `ConcurrencyRamp` to halt upward doubling toward refused levels; implemented exponential backoff on upward recovery probes; and floored concurrency ceilings at the windowed peak of active streaming connections.
- **File Type Filtering for Background Downloads in File Info Dialog (`hydra-gui`)**: Fixed the *Download File Info* dialog prefetching behavior to respect the *Options → File types* configuration. Files with unlisted extensions will no longer trigger automatic background data fetching while the dialog is open unless explicitly started or manually enabled, falling back to lightweight link probing.

### Changed & Refactored

- **GUI Language Selection Menu Ordering (`hydra-gui`)**: Alphabetized language choices in the GUI settings and View menus by their English language names.
- **Localization Updates (`hydra-gui`)**: Updated and completed translation strings for German (`de`), Korean (`ko`), and Hebrew (`he`), and synchronized new theme mode and category directory settings across all supported locales.

---

## [0.3.12] - 2026-08-24

### Added

- **HTML Redirect & Meta Refresh Resolution (`hya-net`, `hydra-cli`, `hydra-gui`, `hydra-ffi`)**: Added automated HTML redirector page and `<meta http-equiv="refresh">` / JavaScript redirection detection (`hya_net::redirect`, `Probe::maybe_redirector`). CLI automatically follows redirectors to adopt target destination file names unless overridden by `-O`/`--output-dir`. GUI integrates asynchronous redirect probing in the File Info dialog and Batch download window to show resolved file names, and FFI drivers resolve HTML redirects within hop limits.
- **Resilient Probe Fallback & Observable Single-Stream Fetching (`hya-net`, `hydra-cli`, `hydra-gui`, `hydra-ffi`)**: Added `probe_resilient` to seamlessly fallback to a 1-byte ranged GET (`Range: bytes=0-0`) when origins refuse HTTP HEAD requests or drop connections without `Content-Length`. Added `fetch_streaming_observed` for single-stream fallback downloads supporting atomic write progress tracking, real-time download rate calculations, rate limiter pacing (`Pace`), and responsive cancellation/pause handling.
- **Adaptive Concurrency Throttling on 429/503 Limits (`hya-net`)**: Implemented dynamic concurrency backoff upon encountering HTTP 429 (Too Many Requests) or 503 (Service Unavailable) status codes. Retains active streaming connections while lowering concurrency ceilings down to the streaming floor during refusal bursts, periodically probing upward after sustained progress to safely restore concurrency.
- **Official Arch Linux AUR Packaging (`packaging/aur`, `packaging/aur-bin`, CI)**: Added official Arch Linux User Repository (AUR) package specifications for both source (`hydra-download-manager`) and pre-built binary (`hydra-download-manager-bin`) distributions, complete with automated `.SRCINFO` generation, release publishing workflows, and documentation.
- **German Localization (`hydra-gui`)**: Added full German (`de`) language translation catalog across all GUI menus, settings tabs, dialogs, and notifications.
- **Connection Limit Status Indicator (`hydra-gui`)**: Added dedicated "Waiting (connection limit)..." connection state status when connections are throttled or held back by concurrency limits or server rate limiting.

### Fixed

- **GUI Rendering & Resource Usage Optimization (`hydra-gui`)**: Virtualized the download table viewport to render only visible rows plus overscan, drastically reducing memory usage and CPU rendering time on large queues. Replaced per-row linear searches with `HashSet<DlId>` lookups for selection styling.
- **Mouse Movement Overhead & Selection Scroll Stability (`hydra-gui`)**: Replaced high-frequency mouse motion event redraws with zero-sized cursor probing and cadence-sampled drag ticks (`Message::DragTick`). Stabilized the root widget tree with an unconditional overlay stack to prevent scroll position resets when clicking or dragging rows.
- **macOS Window Color Space Pinning (`hydra-gui`)**: Pinned AppKit window color space to the softbuffer drawing surface on macOS (`macos_surface`), eliminating full-window ColorSync conversions on Retina/P3 displays and cutting main-thread redraw CPU usage by ~35%.
- **GUI Menu Bar Hover Tracking (`hydra-gui`)**: Fixed menu bar behavior on Windows and Linux so hovering across menu headers (`File`, `Edit`, `View`, etc.) automatically switches the active dropdown menu once a menu is open.
- **Empty File Downloads with `Content-Length: 0` (`hya-net`, `hydra-cli`)**: Fixed zero-byte download handling by accurately distinguishing explicit `Content-Length: 0` headers from missing length metadata.
- **Launchpad PPA Upload & Release Automation (`.github/workflows/release.yml`)**: Fixed PPA upload loop indentation, repaired FTP upload errors, and removed duplicate workflow steps.
- **Release Notes Deduplication (`.github/workflows/release.yml`, `.github/release.yml`)**: Integrated GitHub API release notes generation into release workflows to prevent duplicate release note bodies.

### Changed & Refactored

- **Korean & Multi-Language Localization**: Updated Korean (`ko`) translation strings and added new status strings across all 11 supported locales (`ar`, `de`, `en`, `es`, `fa`, `fr`, `ja`, `ko`, `nl`, `ru`, `zh`).
- **CI Security & Documentation Assets**: Added explicit top-level workflow permissions in CI workflows, optimized documentation and extension images via lossless compression, and updated web documentation SEO and sitemap metadata.

---

## [0.3.11] - 2026-08-23

### Added

- **Microsoft Edge Add-ons Store Publication (`hydra-gui`, `README.md`)**: Added direct store link to the official Microsoft Edge Add-ons listing (`microsoftedge.microsoft.com/addons/detail/hydra-download-manager-in/obemipfpeenmhkdpkobdkeedhdakaoai`) in *Options → Extensions*, with dedicated Microsoft Edge brand artwork and separated rows for other Chromium-based browsers (Brave, Vivaldi, Opera, Arc, Chromium).
- **GitHub Issue Templates (`.github/ISSUE_TEMPLATE`)**: Added standardized issue form templates for bug reports and feature requests with platform/component selectors, reproduction guidance, and security disclosure links.

### Fixed

- **UI Scaling by Font Size Setting (`hydra-gui`)**: Fixed font size setting by scaling the entire GUI window interface proportionately (`theme::ui_scale`), keeping layouts, buttons, and dialogs balanced rather than resizing text within fixed-size containers.
- **macOS Native Menu State Synchronization (`hydra-gui`)**: Added in-place menu synchronization (`macos_menu::sync`) to prevent radio and checkbox menu items (Dark Mode, Speed Limiter, Font Size, Language) from becoming desynchronized or dual-ticked on click.
- **Release Notes Deduplication (`hydra-updater`)**: Fixed duplicate release note sections appearing in in-app update dialogs when release workflows or notes generators run multiple times over the same tag.
- **Launchpad PPA Build Toolchain & Vendoring (`packaging/debian`, CI)**: Fixed Launchpad PPA source builds on Ubuntu 24.04 (noble) and 22.04 (jammy) by targeting versioned `rustc-1.91` / `cargo-1.91` packages, configuring `CARGO_HOME` for sbuild sandboxes, and clearing per-crate `.cargo-checksum.json` paths during packaging.
- **Release CI GPG Key Validation (`.github/workflows/release.yml`)**: Added fail-fast validation in release automation to verify secret signing keys are present in `LAUNCHPAD_GPG_KEY` before attempting PPA uploads.

### Changed & Refactored

- **Korean Localization**: Updated and completed Korean (`ko`) translation strings for dialogs, settings, updater prompts, and extension options.
- **GUI Asset Organization (`hydra-gui`)**: Reorganized built-in localization catalogues into `crates/hydra-gui/assets/locale/` alongside visual brand assets.
- **Release Automation Prerelease Filtering (`.github/workflows/release.yml`)**: Prevented automated publishing of pre-release builds to Homebrew tap, Fedora Copr, and Launchpad PPA repositories.

---

## [0.3.10] - 2026-08-23

### Added

- **Firefox Add-ons Store Publication (`hydra-gui`, `extensions/firefox`)**: Added direct store link to the official Firefox Add-ons listing (`addons.mozilla.org/en-US/firefox/addon/hdm-integration/`) in *Options → Extensions*, replacing manual installation guides with one-click store installation.
- **Multi-Language Localization**: Updated Firefox extension translation strings across all 10 supported locales (`ar`, `en`, `es`, `fa`, `fr`, `ja`, `ko`, `nl`, `ru`, `zh`).

---

## [0.3.9] - 2026-08-23

### Added

- **Linux StatusNotifierItem (SNI) Tray Integration (`hydra-gui`)**: Added native Linux D-Bus `StatusNotifierItem` (SNI) system tray support (`ksni` / `zbus`) with automatic desktop environment detection and runtime fallback between D-Bus SNI and X11/muda. Delivers seamless tray icon support across modern Wayland and X11 desktop environments (GNOME via AppIndicator, KDE Plasma, COSMIC, Sway, Hyprland). Added selectable tray backend options (*Auto*, *StatusNotifierItem (D-Bus)*, *Muda (X11/Legacy)*) under *Options → General*.
- **Linux Taskbar & Launcher Progress (`hydra-gui`)**: Added Unity and KDE launcher D-Bus progress bar and urgent transfer count badges via `com.canonical.Unity.LauncherEntry`, displaying aggregate download progress and active transfer counts directly on the Linux dock and taskbar launcher.
- **In-App Browser Extension Manager (`hydra-gui`)**: Added a dedicated **Browser Extensions** tab in the Options window (*Options → Extensions*) and an **Extensions** action button on the main toolbar. Provides interactive extension installation guides for Chrome, Firefox, Edge, and Brave with status badges, installation directories, packed files (`.zip`/`.xpi`), and one-click actions to open extension folders or browser extension management pages (`chrome://extensions`, `about:debugging`, `edge://extensions`, `brave://extensions`).
- **Official Debian / Ubuntu Launchpad PPA Packaging (`packaging/debian`, CI)**: Added official Debian packaging infrastructure (`control`, `rules`, `copyright`, `postinst`, `postrm`, native messaging host manifests, `.desktop` autostart) with automated Source Package (DSC) generation and Launchpad PPA release triggers (`ppa:ja7ad/hydra`).
- **Official Fedora Copr Packaging (`packaging/rpm`, CI)**: Added Fedora RPM package specifications (`packaging/rpm/hydra.spec`) and automated Copr repository build and release triggers (`copr enable ja7ad/hydra`).
- **Post-Install Extension Setup Guidance (`install.sh` / `install.ps1`)**: Added interactive post-installation setup instructions in both Unix shell and Windows PowerShell installation scripts, outlining browser extension loading steps for Chrome, Edge, Brave, and Firefox.
- **Interactive Web Changelog & Documentation Portal (`docs/`)**: Added a searchable and filterable web changelog portal (`docs/changelog.html`) with category tagging, release links, and improved landing page download modals and engine architecture cards.
- **Multi-Language Localization**: Added localized strings for the new Extensions settings tab, toolbar extension buttons, and Linux tray backend options across all 10 supported locales (`ar`, `en`, `es`, `fa`, `fr`, `ja`, `ko`, `nl`, `ru`, `zh`).

### Fixed

- **Debian Packaging Native Messaging Manifest Generation**: Replaced inline heredoc JSON manifest creation in `debian/rules` with standalone validated JSON manifest files for Chrome/Chromium and Mozilla Firefox.
- **Debian Changelog Formatting in Launchpad Release Automation**: Fixed Debian release versioning and distribution targeting (`noble`, `jammy`) in automated Launchpad source package generation.

### Changed & Refactored

- **Linux Package Dependencies**: Updated Debian and RPM package specifications to recommend `gnome-shell-extension-appindicator` instead of legacy shared library appindicator dependencies.
- **GitHub Release Categorization**: Added the `enhancement` label to the Features category in `.github/release.yml`.

---

## [0.3.7] - 2026-08-22

### Added

- **Browser Extension Bundling & Packaging (`scripts/build-extensions.sh`, packaging targets)**: Added a unified extension build system (`make extensions`) producing both packed (`.zip` for Chrome/Chromium, `.xpi` for Firefox) and unpacked directory trees with path-tailored `INSTALL.txt` guides. Bundled extensions across all packaging targets: Windows setup (`%LOCALAPPDATA%\Programs\Hydra\extensions`), macOS `.app` bundle (`Contents/Resources/extensions`), macOS `.pkg` (`/Library/Application Support/Hydra/extensions`), Linux `.deb`/`.rpm` (`/usr/share/hydra-download-manager/extensions`), and release archives (`<prefix>/share/hydra/extensions`).
- **CRX Signing & Enterprise Policy Packaging**: Added Chromium `.crx` generation and signing (`--crx-key`, `HYDRA_CRX_KEY`, `--crx`) with pinned extension ID (`jpnonmbbkjdpeebdhkjoliklfhkdcomj`) for enterprise policy deployments (`ExtensionSettings` / `ExtensionInstallForcelist`).
- **Rubber-Band Drag Selection (`hydra-gui`)**: Added interactive marquee drag selection to the download table, allowing users to click and drag across rows to select multiple downloads simultaneously, with accompanying theme and bounding-box styling.
- **ABI Specification (`docs/ffi/ABI.md`)**: Added the canonical human-readable specification for the `libhydra` C ABI — FFI design principles, the formal ABI 1 stability policy (what is frozen, what may be appended, what forces ABI 2), the full ownership/encoding/error/threading contract, the event-queue rationale and its ordering and drop guarantees, and the compatibility-testing matrix. `include/hydra.h` now carries a concise summary and points at it.
- **Frozen ABI Baseline (`crates/hydra-ffi/abi/abi-1.manifest`)**: Added a machine-checkable record of ABI 1 — every enumerator value, field offset and width, struct size and exported symbol.
- **ABI Stability Gate (`scripts/ffi-abi-compat.sh`, `make ffi-compat`)**: Added a CI gate that derives the current layout from the header with a generated C probe and enforces the ABI 1 rules against the frozen baseline: fields may not move or change width, enumerators may not be renumbered, symbols may not disappear, and only the two size-prefixed configuration structs may grow. Additions the contract permits pass.
- **Forward-Compatibility Probe (`examples/ffi-c/compat_probe.c`)**: Added an old-header/new-library conformance program. `scripts/ffi-c-example.sh` now builds it against `include/hydra.h` as published by *every* release tag and links each against the library from the current branch, with a guard wall after each caller-allocated struct so a byte written past an older header's extent is caught at test time.
- **Wider ABI CI Matrix**: The FFI conformance job now also runs under Clang on Linux (in addition to GCC), and the stability gate runs on Linux, macOS and Windows.

### Fixed

- **PowerShell Execution Policy in Windows Installer (`install.ps1`)**: Fixed native messaging host registration failure on Windows machines with Restricted PowerShell execution policies by invoking `install-native-host.ps1` in its own PowerShell process with `-ExecutionPolicy Bypass` and non-fatal warning recovery.
- **MSVC Guard Index Cast & MSYS2 Legacy Header Checkout (`scripts/ffi-c-example.sh` / `compat_probe.c`)**: Fixed unsigned pointer cast in compatibility probe for MSVC and resolved legacy release header checkouts when running under MSYS2 environments.

### Changed & Refactored

- **`libhydra` as a First-Class Product**: Documentation now draws an explicit line between `hydra` (the GPL application — CLI, GUI, host) and `libhydra` (the permissively licensed embeddable engine, with its own version, release archives and compatibility promise). Language bindings are documented as independent downstream projects that need only the published header and a release archive.

---

## [0.3.6] - 2026-08-22

### Added

- **`hydra compat-link` CLI Subcommand (`hydra-cli`)**: Added a dedicated subcommand to plan, verify, and install `wget` and `curl` dialect symlinks/shims into `$PATH` or custom directories, checking against `$PATH` shadowing so users know if another tool takes precedence. *(e.g. `hydra compat-link --dry-run`, `hydra compat-link`, `hydra compat-link --dir ~/.local/bin`)*
- **macOS Application Bundle In-Place Updates (`hydra-updater` / `hydra-gui`)**: Added full updater support for macOS `.app` bundles (`Hydra Download Manager.app`), maintaining proper bundle directory structures, re-stamping `Contents/Info.plist` bundle versions, and applying ad-hoc code signatures.
- **Elevated Self-Updates (`hydra-updater` / `hydra-gui`)**: Added native authentication prompts (`osascript` on macOS, `pkexec`/`sudo` on Linux) to allow in-place updating of system-wide / root-owned installations (e.g. in `/usr/local/bin` or `/Applications`) without requiring full re-installation.
- **Windows Apps & Features Integration (`install.ps1` / `uninstall.ps1`)**: Registered Hydra in Windows *Installed apps / Apps & features* with display icons, publisher info, and direct uninstaller registration for native Windows Settings integration.
- **Desktop & Start Menu Shortcuts (`install.ps1`)**: Added Start Menu and optional Desktop (`-Desktop`) shortcut generation with embedded high-resolution icons and application metadata.
- **Offline Windows Uninstaller Packaging**: Bundled `uninstall.ps1` into Windows release archives to allow complete offline uninstallation of files, shortcuts, PATH modifications, and registry entries.

### Fixed

- **Immediate Transport Failure Range Reclaim (`hya-net` / `hya-core`)**: Fixed long stalls on closed pooled connections, truncated responses, or refused socket requests by immediately detecting transport failures and re-assigning pending byte ranges instead of waiting out the full stall timeout.
- **Stale In-Flight Range Discard (`hya-core`)**: Added request start timestamping to prevent discarded or superseded chunks from being erroneously credited after a stall or range preemption.
- **Repeated Boolean Flags in CLI Dialects (`hydra-cli`)**: Fixed dialect canonicalizer rejecting repeated flags like `curl -s -sS` or `wget -q -q`, while correctly preserving counting flags like `-vv` for verbosity levels.
- **Sudo User Directory Resolution (`install.sh` / `uninstall.sh`)**: Fixed script installation when executed via `sudo` by resolving `$SUDO_USER` to properly place and clean desktop icons, `.desktop` files, and browser native messaging manifests in the user's home directory instead of `/root`.
- **macOS Application Unregistration & Graceful Quit (`uninstall.sh`)**: Added Launch Services unregistration (`lsregister -u`) and graceful AppleScript quit messaging before removing the `.app` bundle to prevent orphaned Spotlight entries.
- **Windows Process Locking During Upgrades (`install.ps1` / `uninstall.ps1`)**: Added graceful window close requests (`CloseMainWindow`) and process termination for running instances (`hydra-gui`, `hydra-host`, `hydra-updater`) before attempting file replacement or uninstallation.

### Changed & Refactored

- **FFI Metric Definition Clarity (`hydra-ffi` / `include/hydra.h`)**: Clarified `stall_count` documentation in `hydra_progress_t` and `hydra_metrics_t` to specify that it accounts for ranges reclaimed from both no-progress timeouts and transport/socket connection failures.
- **Update Dialog Elevation Guidance (`hydra-gui`)**: Added notices in the in-app update dialog when an update will prompt for administrator authentication, localized across 10 supported languages.

---

## [0.3.5] - 2026-08-21

### Added

- **C/C++ Foreign Function Interface (`libhydra` / `hydra-ffi`)**: Added stable C ABI bindings and header (`include/hydra.h`) to embed Hydra's engine in C, C++, Python, Swift, Go, etc. *(e.g. `hydra_engine_new()`, `hydra_engine_add_job()`, `hydra_job_start()`, `hydra_engine_poll_event()`)*
- **Self-Update Engine & CLI Command (`hydra-updater` / `hydra-cli`)**: Added `hydra update` command with automatic release discovery from GitHub releases and direct asset downloads. *(e.g. `hydra update`, `hydra update --beta`, `hydra update --download-only`, `hydra update --json`)*
- **GUI In-App Updater (`hydra-gui`)**: Added automated update checking on launch, interactive update modal dialog with rendered markdown release notes, and single-click restart finisher.
- **Beta / Pre-Release Update Channel**: Added beta channel support to opt in to pre-release builds in both CLI (`hydra update --beta`) and GUI (*Options → General → Include pre-release versions*).
- **Package Installer & Bundle Detection**: Automatically detects package-managed or read-only installations (macOS `.app` bundle, system `/usr/bin`), offering direct installer downloads (`.deb`, `.rpm`, `.dmg`, `.pkg`, `.exe`) when in-place updating is not possible.
- **Periodic Download Quotas & Rollover (`hydra-gui`)**: Added configurable data usage limits over rolling periods (hourly, daily, weekly, monthly) with auto-pausing when exceeded and auto-resuming upon quota reset. *(Configurable in Options → Connection)*
- **Live Quota Dashboard (`hydra-gui`)**: Added visual quota consumption gauges, percentage indicators, and real-time countdown timers until the next quota rollover in the Connection settings tab.
- **Homebrew Formula Support**: Added official Homebrew tap distribution for macOS and Linux CLI installations. *(e.g. `brew install ja7ad/tap/hydra`)*
- **Dynamic Speed Limit Badge & Status (`hydra-gui`)**: Added `(Limited)` indicator badge in transfer tables and progress detail views when a transfer or engine bandwidth is actively throttled.
- **Multi-language Localization**: Added localization for update workflows, package notifications, periodic quota tracking, and rate limiter status across 10 locales (`ar`, `en`, `es`, `fa`, `fr`, `ja`, `ko`, `nl`, `ru`, `zh`).
- **C/C++ Integration Examples & ABI Tests**: Added standalone C and C++ integration examples and ABI verification test suites under `examples/ffi-c/`.

### Fixed

- **Dynamic Live Speed Limiter (`hya-net` / `hydra-gui` / `hydra-ffi`)**: Fixed speed limiter freezing at transfer start; rate limit adjustments now take effect immediately mid-flight without connection teardown or accumulated sleep debt.
- **Paired Aggregate & Per-Job Rate Limiting (`hya-net`)**: Fixed multi-cap pacing using `Pace::pair` to enforce both global engine bandwidth ceilings and individual download rate caps simultaneously.
- **FTP Download Speed Limiting (`hya-net` / `hydra-cli`)**: Fixed unshaped FTP transfers by integrating `Pace` rate limiting into `FtpFetcher`. *(e.g. `hydra --limit-rate 500k ftp://example.com/file.iso`)*
- **Linux Application ID & Dock Icon (`hydra-gui`)**: Fixed missing window icon and improper taskbar grouping on GNOME/KDE/Wayland desktops by setting `application_id` to `dev.ja7ad.hydra` and installing hicolor icons.
- **Release Asset Version Resolution (`hydra-updater`)**: Fixed asset lookup failures when matching pre-release tags against clean version asset spellings (e.g. resolving `v0.3.2-rc` to base version `0.3.2`).
- **Release Notes Comment Filtering (`hydra-updater`)**: Fixed raw HTML comments and auto-generated GitHub template noise leaking into in-app release notes dialogs.
- **Windows MSVC FFI Linker Compatibility**: Fixed static CRT linkage, path spacing issues, and linker symbol collisions on MSVC by utilizing response files and `/NODEFAULTLIB` filtering.
- **C99 / Pre-C11 FFI Compatibility (`hydra-ffi`)**: Fixed header compilation errors on older C compilers by adding static assertion fallbacks and dynamic linker probes in `hydra.h`.
- **NSIS Windows Installer Version Parsing**: Fixed automated Windows installer builds by extracting clean numeric version strings directly from `Cargo.toml`.

### Changed & Refactored

- **General Settings Layout (`hydra-gui`)**: Reorganized Options → General settings to group startup behaviors together (*Launch at system startup*, *Start minimized to system tray*, *Check for updates at launch*).
- **Concurrency Warm-Up Gating (`hya-core` / `hya-net`)**: Enhanced the adaptive ramp search to gate connection scaling until existing connections complete initial TCP slow-start and achieve stable throughput delivery.
- **Pre-release Package Version Formatting**: Standardized pre-release version strings across macOS `.app` Info.plist, macOS `.pkg`, Debian `.deb`, and RedHat `.rpm` packages.
- **Workspace Dependency Stamping**: Automated workspace version inheritance and dependency stamping during crates.io publishing workflows.

---

## [0.2.3] - 2026-08-19

- Baseline release featuring core adaptive concurrency retrieval, segmented HTTP/HTTPS/FTP downloads, CLI interactive TUI manager, desktop GUI, and browser extension integrations.

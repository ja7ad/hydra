# Changelog

All notable changes to the Hydra project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

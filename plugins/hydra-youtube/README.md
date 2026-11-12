# YouTube resolver

This official Hydra plugin uses [yt-dlp](https://github.com/yt-dlp/yt-dlp)
to extract metadata. Hydra downloads direct HTTP media tracks and combines
video and audio through your installed ffmpeg. Playlist links expand into ordered
video jobs (up to 512 available entries). Audio can be saved in its original format
or converted to MP3, M4A, Opus, FLAC or WAV using ffmpeg. Live streams, DRM formats
and formats available only as segmented manifests are not supported.

Install yt-dlp for your platform and put `yt-dlp` (`yt-dlp.exe` on Windows)
on PATH, or enable **Download yt-dlp if it is missing** in the plugin settings.
That opt-in downloads the official 2026.10.26 release for the host platform,
verifies its published SHA256 and pins it in the plugin data directory.
Existing pinned programs are never replaced automatically. No helpers are bundled. The Wasm plugin is identical on Linux, macOS and
Windows, including ARM64; the external tools must match your OS and CPU.

YouTube changes faster than any pinned release. `hydra plugin check hydra.youtube`
logs a warning when the approved yt-dlp is older than that release; update it
with your package manager and approve the new binary (see below).

## JavaScript runtime

yt-dlp needs a JavaScript runtime to solve YouTube's player challenges: Deno
2 or newer, Node.js 22 or newer, QuickJS or Bun. The **JavaScript runtime for
yt-dlp** setting controls which one it may use:

- `default` lets yt-dlp use Deno when it is installed and Node otherwise.
- A named runtime makes yt-dlp use only that one. yt-dlp always prefers an
  enabled Deno, so this is how to keep it on Node while a broken or outdated
  Deno is also installed.
- **JavaScript runtime folder** points the chosen runtime at an install that is
  not on PATH, such as an nvm, fnm or portable one. It needs a named runtime.

Hydra runs yt-dlp with PATH plus `/usr/local/bin`, `/opt/homebrew/bin`,
`~/.deno/bin`, `~/.local/bin` and `~/.bun/bin`, so the common installers are
found even when Hydra is started from a desktop launcher.

## When YouTube refuses the connection

"Sign in to confirm you're not a bot" is YouTube rejecting the network or the
session, not a plugin fault. In order of effort:

1. Enable **Use granted browser cookies** and add cookies from a browser that
   plays the video (see Cookies below).
2. Sign in to youtube.com in that browser. A signed-in session reaches player
   clients that anonymous sessions on a flagged network cannot.
3. Set **YouTube extractor arguments**. The text is passed to yt-dlp as
   `--extractor-args youtube:…`, for example `player_client=tv,web_safari`, or
   `po_token=web.gvs+TOKEN` on networks where YouTube answers media requests
   with 403 until a [PO token](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)
   is supplied.

When YouTube limits a session to its SABR protocol, only the pre-merged 360p
format has a direct URL and that is all the plugin can list; yt-dlp's warning
about it is kept in the plugin log.

```sh
rustup target add wasm32-wasip1
cargo build --manifest-path plugins/hydra-youtube/Cargo.toml --release --target wasm32-wasip1
```

Copy `target/wasm32-wasip1/release/hydra_youtube.wasm` from this folder to
`plugin.wasm`, then install this folder in developer mode, or use the packaging
script in the parent README.

```sh
hydra plugin install plugins/hydra-youtube --accept-permissions
hydra --list-tracks 'https://www.youtube.com/watch?v=VIDEO_ID'
hydra --quality 1080 --container mkv 'https://www.youtube.com/watch?v=VIDEO_ID'
hydra --no-input --extract-audio --audio-format mp3 'https://www.youtube.com/playlist?list=PLAYLIST_ID'
hydra --audio none --track FORMAT_ID 'https://www.youtube.com/watch?v=VIDEO_ID'
```

GUI: Options → Extensions & Plugins → Plugins → enter this folder/package → Review permissions →
Accept permissions and install. Add URL automatically inspects matching links. Choose
video, audio, subtitle tracks and the output container before starting.
Playlist entries can be selected before they enter the queue; private and deleted
entries are left out. On the command line a playlist keeps going when one video
fails and reports the failed items at the end with a nonzero exit status. The Audio only option
shows extraction formats. On videos with dubbed audio, the best-audio choice stays on
the original language; pick another language by its track ID. Subtitles, including
auto-generated and auto-translated captions, are saved as sidecar files.

Cookies are optional and disabled by default. To enable them, set `use_cookies`
to true and supply browser cookies using Hydra's existing cookie controls.
Only granted cookie domains enter a temporary Netscape file, removed after the
tool exits. Download only content you are authorized to save.

The helper runs with `--ignore-config` and `--no-plugin-dirs`, and receives
`--skip-download`; local yt-dlp configuration cannot add execution or download
options, and every command line the plugin can build is declared in its manifest.
yt-dlp is pinned by file hash. After updating it, explicitly approve
its new binary with `hydra plugin grant hydra.youtube exec:yt-dlp`.
Use `hydra plugin set hydra.youtube yt_dlp_path /path/to/program/folder`
followed by that grant command when it is outside PATH.

A terminal download without explicit selection flags offers numbered quality and audio
choices. Use `--no-input` for unattended downloads; it also refuses plugin forms.
Use `hydra plugin logs hydra.youtube` or View plugin logs in GUI settings for
resolver diagnostics. Installation shows optional setup hints, which remain in plugin info.

Development tests:

```sh
cargo test --manifest-path plugins/hydra-youtube/Cargo.toml
python3 scripts/plugins/plugin-media-e2e.py
python3 scripts/plugins/plugin-youtube-live-e2e.py --cookies-from-browser firefox --rounds 3
```

The second script talks to YouTube. It fails, rather than skips, when YouTube
refuses the network; see [the E2E notes](../../docs/testing/youtube-live-e2e.md).

The final package is `youtube-download_{version}.hyaplugin`. Automatic backend installation
supports macOS x64/ARM64, Windows x64/ARM64 and Linux x64/ARM64. Other platforms
can provide their own yt-dlp executable. Hydra's configured proxy is supplied to
metadata requests and the backend through its otherwise scrubbed environment.
The fixed backend release is reproducible; updating it is an explicit plugin
maintenance change, since extractor compatibility can change at YouTube.

# YouTube plugin live E2E

Two scripts exercise the YouTube plugin against YouTube itself. Both drive the
real Hydra CLI and the real Wasm plugin in an isolated `HYDRA_CONFIG_DIR`; a
stage YouTube refuses counts as a failure, never as a pass.

## Live E2E

```sh
python3 scripts/plugins/plugin-youtube-live-e2e.py --rounds 3
python3 scripts/plugins/plugin-youtube-live-e2e.py --cookies-from-browser firefox --rounds 3
```

Each round lists a playlist, resolves two videos, and for each video downloads
the best quality, the lowest quality when more than one is offered, an MP3
extraction when a separate audio track exists, and one subtitle sidecar. Every
downloaded file is checked with ffprobe. Useful options: `--yt-dlp-dir FOLDER`
tests a specific yt-dlp build, `--runtime node` and `--extractor-args …` set
the matching plugin settings, `--video URL` (repeatable) and `--playlist URL`
replace the defaults. The JSON report and every stage's stdout and stderr stay
in the printed artifact folder.

Requires yt-dlp (unless `--yt-dlp-dir` is given), a JavaScript runtime, ffmpeg
and ffprobe. Without `--cookies-from-browser` no browser profile is read.

## Moving to a new yt-dlp release

```sh
python3 scripts/plugins/update-yt-dlp.py            # add --cookies-from-browser firefox on a refused network
python3 scripts/plugins/update-yt-dlp.py --dry-run  # run every check, change nothing
```

The script compares the release pinned in `plugins/hydra-youtube/src/backend.rs`
with yt-dlp's latest release. For a newer one it downloads this platform's
official build into a temporary folder, verifies it against the release's
`SHA2-256SUMS`, and then requires two things of it:

1. yt-dlp's option parser accepts every command line declared in the plugin
   manifest (all video and playlist shapes, with and without an explicit
   runtime, extractor arguments and cookies). A refused option means the
   plugin needs a code change before the release can be adopted.
2. The live E2E above passes with that build.

Only then does it rewrite the pinned release in `src/backend.rs` and the plugin
README and raise the plugin's patch version in `hydra-plugin.toml`,
`Cargo.toml` and `Cargo.lock`. It never commits. Its pure parts are covered by
`python3 scripts/plugins/test_update_yt_dlp.py`.

## Observed result: 2026-11-12

Network: a VPN exit in a datacenter range. yt-dlp 2026.08.19 (installed) and
2026.10.26 (official macOS build), Deno 2.9.8 and Node 26.13.0.

| Stage | Anonymous | Firefox cookies, 3 rounds |
|---|---|---|
| Playlist listing (182 entries) | pass | pass, 3 of 3 |
| Video resolution | refused: "Sign in to confirm you're not a bot" on all 12 player clients | pass, 6 of 6: one 240p or 360p pre-merged track, plus subtitles |
| Media download | not reached | fail, 9 of 9: googlevideo answers 403 |

yt-dlp alone behaves identically on this network: `-f 18` ends in HTTP 403 and
an HLS variant from the `web_safari` client stalls on its first segment, which
also returns 403. Its log names the cause: YouTube binds a GVS PO token to the
video for this session, and subtitle URLs return an empty body for the same
reason. The browser session held no first-party YouTube sign-in cookies
(`LOGIN_INFO`, `SID`), so the TV clients that need no PO token stayed
unavailable. A complete download therefore needs one of: a browser signed in to
youtube.com, a PO token passed through the plugin's "YouTube extractor
arguments" setting, or a network YouTube does not flag. None of the three was
available for this run, so no real media download has been verified with the
current plugin. The full download path is verified hermetically by
`scripts/plugins/plugin-media-e2e.py`.

### After signing in to youtube.com in Firefox

Same network and yt-dlp 2026.08.19, with the Firefox session signed in:
`--cookies-from-browser firefox --rounds 3` passed 31 of 31 stages.

| Stage, each round | Result |
|---|---|
| Playlist listing | 182 entries |
| Video 1 (`jNQXAC9IVRw`) tracks | 144p and 240p, 8 audio tracks, 2 subtitle languages |
| Video 1 best / 144p / MP3 / `de` subtitle sidecar | 240p h264+aac / 144p / mp3 / written |
| Video 2 (`aqz-KE-bpKQ`) tracks | 144p to 2160p, 12 audio tracks |
| Video 2 best / 144p / MP3 | 2160p av1+aac, 743 MB in 161 to 237 s / 144p / mp3 |

A signed-in session is therefore enough on a network YouTube refuses
anonymously; no PO token was needed.

### GUI, driven over its IPC socket

A debug `hydra-gui` in an isolated `HOME`, with this plugin installed, was
driven through the line protocol the browser extension uses. Nothing was
clicked, so dialogs were opened but not confirmed.

| Command | Result |
|---|---|
| `ping`, `config` | answered |
| `download` of a 20 MB file from a local range origin | completed; SHA-256 matches |
| `stream` with a local HLS playlist | stream capture dialog opened |
| `plugin-url` with a playlist | plugin resolved 182 entries |
| `plugin-url` and `download` with a video URL | plugin invoked; refused by YouTube's bot verification |
| `shutdown` | clean exit |

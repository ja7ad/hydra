# hya-stream

HLS and MPEG-DASH: manifest parsing, segment planning, and assembly.

A manifest is not a file. It names a few hundred short objects that have to
arrive, be ordered, and be concatenated — which is a different problem from
the byte-range scheduling in [`hya-core`](https://crates.io/crates/hya-core),
and is what this crate solves. It performs **no IO of its own**: the caller
supplies a fetcher, so the same code serves the desktop app, the CLI, and
anything else built on [HYDRA](https://github.com/ja7ad/hydra).

## What it does

- **HLS** (`hls`) — master/media playlist parsing, variant selection by
  height or explicit URL, bounded-concurrency segment fetch with retry and a
  per-attempt timeout, ordered append to the output file, and a checkpoint
  sidecar so a paused download resumes without re-fetching what already
  landed.
- **MPEG-DASH** (`dash`) — MPD parsing, `SegmentTemplate` expansion
  (`$Number$`/`$Time$`, including `SegmentTimeline`), `BaseURL` inheritance
  down the element tree, and video/audio track selection. A chosen
  [`dash::Manifest::plan`] hands off to the exact same [`hls::fetch_all`]
  that assembles HLS — DASH's job is arithmetic and URL generation, not a
  second assembly pipeline.
- **URLs** (`url`) — absolute, protocol-relative, root-relative, and
  relative reference resolution against `http`, `https`, and `ftp` bases,
  sized for what a manifest needs rather than as a general URL library.
- **Container assembly** — MPEG-TS segments concatenate directly into a
  playable `.ts`; fragmented MP4 segments (`#EXT-X-MAP` init + `moof`/`mdat`
  fragments) concatenate directly into a playable `.mp4`. Turning MPEG-TS
  into MP4, or combining DASH's separate video and audio tracks into one
  file, is a genuine remux/mux and goes through a system `ffmpeg` as a
  stream copy — [`hls::plan_finish`] decides which is needed and
  [`hls::finish`]/[`hls::remux`]/[`hls::mux`] carry it out, refusing plainly
  rather than writing a file that will not play when `ffmpeg` is absent.

## What is deliberately not here

No DRM. Widevine, PlayReady, FairPlay, and SAMPLE-AES are recognised only so
a protected manifest can be refused by name, in both parsers — no key or
licence request is ever made. Plain AES-128, which is ordinary content
protection fetched over the same authenticated session as everything else,
is recognised and reported as unsupported for now rather than silently
producing noise.

## Example

```rust,no_run
use hya_stream::{hls, Meter, Resume};
use std::sync::Arc;

# async fn demo() -> std::io::Result<()> {
let text = "…"; // the playlist body, fetched by the caller
let playlist = hls::parse(text, "https://cdn.example/hls/index.m3u8");
let plan = hls::Plan::build(&playlist, None).unwrap();
let meter = Arc::new(Meter::default());
let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
let mut out = std::fs::File::create("out.ts")?;

// Eight segments in flight; 0 would take the crate's default.
hls::fetch_all(
    &plan, &mut out, "out.ts.part", my_fetcher, &meter, &cancel, Resume::default(),
    hls::Concurrency::fixed(8), &Default::default(),
)
.await?;
# Ok(()) }
# fn my_fetcher(_: hya_stream::Segment, _: String, _: std::sync::Arc<std::sync::atomic::AtomicU64>)
#     -> hya_stream::FetchSeg { unimplemented!() }
```

The `fetch` closure is the only IO the crate needs from the caller: given a
[`Segment`] and a destination path, fetch it (honouring its byte range, if
any) and report bytes as they arrive through the shared counter. That seam is
what lets this crate stay transport-agnostic — [`hya-net`](https://crates.io/crates/hya-net)
fills it in production, a hermetic stub fills it in tests.

The libraries are deliberately permissive so they remain usable as
dependencies; only the assembled tool is copyleft. See
[LICENSING.md](https://github.com/ja7ad/hydra/blob/main/LICENSING.md) for the
reasoning.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/ja7ad/hydra/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/ja7ad/hydra/blob/main/LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

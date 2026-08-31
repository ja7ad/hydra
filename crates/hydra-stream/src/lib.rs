// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HLS and MPEG-DASH: manifest parsing, segment planning, and assembly.
//!
//! A manifest is not a file. It names a few hundred short objects that have
//! to arrive, be ordered, and be concatenated — which is a different problem
//! from the byte-range scheduling in `hya-core`, and is what this crate
//! solves. It performs no IO of its own: the caller supplies a fetcher, so
//! the same code serves the desktop app, the CLI, and anything else.
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use hya_stream::{hls, Meter, Resume};
//! use std::sync::Arc;
//!
//! let text = "…"; // the playlist body, fetched by the caller
//! let playlist = hls::parse(text, "https://cdn.example/hls/index.m3u8");
//! let plan = hls::Plan::build(&playlist, None).unwrap();
//! let meter = Arc::new(Meter::default());
//! let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
//! let mut out = std::fs::File::create("out.ts")?;
//! // Eight segments in flight; 0 would take the crate's default.
//! hls::fetch_all(
//!     &plan, &mut out, "out.ts.part", my_fetcher, &meter, &cancel, Resume::default(),
//!     hls::Concurrency::fixed(8), &Default::default(),
//! )
//! .await?;
//! # Ok(()) }
//! # fn my_fetcher(_: hya_stream::Segment, _: String, _: std::sync::Arc<std::sync::atomic::AtomicU64>)
//! #     -> hya_stream::FetchSeg { unimplemented!() }
//! ```
//!
//! # What is deliberately not here
//!
//! No DRM. Widevine, PlayReady, FairPlay and SAMPLE-AES are recognised only
//! so a protected manifest can be refused by name; no key or licence request
//! is ever made, in either parser.

pub mod dash;
pub mod hls;
pub mod url;

pub use hls::{
    fetch_all, ffmpeg, ffmpeg_available, finish, mux, plan_finish, remux, Checkpoint, FetchSeg,
    Fetcher, Finish, Finished, InFlight, KeyRef, Keys, Meter, Plan, Refusal, Resume, Segment,
    Segments, DEFAULT_CONCURRENCY, MAX_CONCURRENCY,
};
pub use url::{join, join_url, parse_url, ParsedUrl};

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HLS: playlist parsing, variant selection, and segment assembly.
//!
//! A manifest is not a file, so it does not go through the range scheduler in
//! `hya-core` — there is nothing to range over. What it names is a few hundred
//! short objects that have to arrive, be ordered, and be concatenated. That is
//! this module: parse, plan, fetch with bounded concurrency, write in order.
//!
//! # What "assembled" means per container
//!
//! * **MPEG-TS segments → `.ts`** — transport-stream packets are
//!   self-framing, so the concatenation of the segments IS the stream. No
//!   rewriting at all.
//! * **Fragmented MP4 segments → `.mp4`** — an `#EXT-X-MAP` initialisation
//!   segment carries `ftyp` + `moov`, and each media segment is a
//!   `moof` + `mdat` pair. Init followed by the fragments in order is a valid
//!   fragmented MP4 that players seek and scrub normally.
//! * **MPEG-TS → `.mp4`** needs a genuine remux (demux PES, rebuild sample
//!   tables). [`remux`] does that when a system ffmpeg is available and
//!   otherwise says so plainly, rather than writing a file that will not play.
//!
//! # What is deliberately not here
//!
//! No DRM. A playlist whose keys name Widevine, PlayReady, FairPlay or
//! SAMPLE-AES is refused by [`Plan::build`] with the system named, and no key
//! request is ever made. Plain AES-128, which is an ordinary key fetched over
//! the same authenticated session, is recognised and reported as unsupported
//! for now rather than silently producing noise.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Ceiling on a playlist. Kilobytes in practice, even for a long VOD.
const PLAYLIST_MAX: usize = 8 * 1024 * 1024;
/// Segments in flight when the caller states no preference.
pub const DEFAULT_CONCURRENCY: usize = 8;
/// Ceiling on it. Past this the origin is being hammered rather than used,
/// and the staging files start to matter.
pub const MAX_CONCURRENCY: usize = 32;
/// Attempts per segment. One lost segment is a hole no player recovers from,
/// so this is the one place that insists.
const ATTEMPTS: usize = 3;
/// How long a segment may go without a single byte arriving before the
/// attempt is abandoned.
///
/// An idle watchdog, not a deadline: a stalled origin holds a slot until the
/// OS gives up on the socket, which can be minutes, and no error ever
/// arrives to trigger a retry — while a slow-but-healthy segment that takes
/// longer than this to arrive in full is not a stall and must not be killed.
/// [`ATTEMPT_CEILING`] bounds the whole attempt.
pub const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Absolute ceiling on one attempt at a segment, however steadily its bytes
/// trickle in. A last resort against an origin that drips one byte a
/// minute forever; generous enough that a single-segment feature film on a
/// slow link never meets it.
pub const ATTEMPT_CEILING: std::time::Duration = std::time::Duration::from_secs(4 * 3600);
/// How often the idle watchdog reads the byte counter.
const WATCHDOG_TICK: std::time::Duration = std::time::Duration::from_secs(1);

// --------------------------------------------------------------- parsing

/// The AES-128 key a segment is encrypted under.
///
/// Plain HLS content protection, not DRM: the playlist names an ordinary URL
/// and the key is fetched over the same authenticated session as everything
/// else. Nothing here circumvents anything — a stream whose key you are not
/// entitled to simply answers 403.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRef {
    /// Where the 16-byte key is served from, already resolved — or a `data:`
    /// URI carrying the key itself, which [`Plan::inline_keys`] decodes.
    pub uri: String,
    /// The initialisation vector, always concrete by the time it gets here:
    /// either the playlist's explicit `IV`, or — per RFC 8216 §5.2 — the
    /// segment's media sequence number as a 128-bit big-endian integer.
    /// Resolving it at parse time is what keeps the decryptor from needing
    /// to know where a segment sat in the playlist.
    pub iv: [u8; 16],
}

/// One thing to fetch.
///
/// A segment is usually its own object, but RFC 8216 §4.3.2.2 also lets a
/// playlist carve every segment out of ONE file with `#EXT-X-BYTERANGE` —
/// which Apple's own fMP4 examples do. Treating that as a plain URL fetches
/// the whole file once per segment, so the range travels with the URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub url: String,
    /// `(offset, length)` when the segment is a sub-range of `url`.
    pub range: Option<(u64, u64)>,
    /// Set when the segment is AES-128 encrypted.
    pub key: Option<KeyRef>,
}

impl Segment {
    pub fn new(url: impl Into<String>) -> Segment {
        Segment {
            url: url.into(),
            range: None,
            key: None,
        }
    }

    /// The `Range` header value this segment needs, if any.
    pub fn range_header(&self) -> Option<String> {
        let (off, len) = self.range.filter(|(_, len)| *len > 0)?;
        let last = off.checked_add(len)?.checked_sub(1)?;
        Some(format!("bytes={off}-{last}"))
    }
}

/// One playable rendition from a master playlist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variant {
    pub url: String,
    pub bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    /// `AUDIO` group this variant plays with. When it names a group whose
    /// chosen rendition has a URI of its own, the variant carries video
    /// only and the sound is a second playlist.
    pub audio: Option<String>,
}

/// An `#EXT-X-MEDIA` alternative rendition. Only `TYPE=AUDIO` is kept:
/// subtitles and closed captions are not written into the output file, so
/// parsing them would add state nothing reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rendition {
    pub group: String,
    pub name: Option<String>,
    pub language: Option<String>,
    pub default: bool,
    pub autoselect: bool,
    /// Absent means the audio is inside the variant playlist itself
    /// (RFC 8216 4.3.4.2.1), which is the muxed case and needs no second
    /// track.
    pub url: Option<String>,
}

/// The container the segments are in, which decides what can be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Segments {
    /// MPEG-TS packets: concatenable as `.ts`, remuxable to `.mp4`.
    Ts,
    /// Fragmented MP4: init map plus `moof`/`mdat` fragments.
    Fmp4,
}

/// A packed elementary audio stream (RFC 8216 §3.1): the segments are bare
/// frames, so their concatenation is the file and no container is involved.
///
/// Such a playlist reports [`Segments::Ts`] as its kind — the segments
/// concatenate and remux exactly as transport stream does — and carries this
/// beside it so the assembled file gets the extension its bytes deserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RawAudio {
    /// AAC in ADTS framing.
    Aac,
    Mp3,
    /// Dolby AC-3.
    Ac3,
    /// Dolby E-AC-3.
    Ec3,
}

impl RawAudio {
    /// The extension the concatenated segments should carry.
    pub fn ext(self) -> &'static str {
        match self {
            RawAudio::Aac => "aac",
            RawAudio::Mp3 => "mp3",
            RawAudio::Ac3 => "ac3",
            RawAudio::Ec3 => "ec3",
        }
    }

    fn from_path(path: &str) -> Option<RawAudio> {
        let lower = path.to_ascii_lowercase();
        let ext = lower.rsplit('.').next()?;
        match ext {
            "aac" => Some(RawAudio::Aac),
            "mp3" => Some(RawAudio::Mp3),
            "ac3" => Some(RawAudio::Ac3),
            "ec3" | "eac3" => Some(RawAudio::Ec3),
            _ => None,
        }
    }
}

/// Why a playlist cannot be downloaded. Separated from a plain IO error
/// because the answer for the user is different in each case.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Refusal {
    /// A DRM system was named in the playlist. Hydra does not circumvent DRM.
    Drm(String),
    /// Content protection other than AES-128, whose decrypt path is not
    /// implemented.
    Encrypted(String),
    /// A live playlist handed to the VOD path.
    Live,
    /// Nothing playable in the manifest.
    Empty,
    /// An AES-128 key whose `IV` is present but not 16 hex bytes. Guessing
    /// at it would corrupt the first block of every segment undetectably.
    UnreadableIv,
    /// An AES-128 key at a URI this client cannot fetch, or a `data:` URI
    /// that does not hold a 16-byte key. Writing the ciphertext instead
    /// would look like a download that worked.
    UnresolvableKey(String),
    /// A `{$name}` reference with no `EXT-X-DEFINE` behind it (RFC 8216bis
    /// §4.4.2.3 says the playlist must not be parsed).
    UndefinedVariable(String),
    /// The manifest uses a form this client does not assemble; the message
    /// names it.
    Unsupported(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Named as the boundary it is, not as a gap in the feature set.
            // "not supported" reads as "not supported YET", and leaves people
            // waiting for a release that is never coming.
            Refusal::Drm(s) => write!(
                f,
                "{s} DRM: this stream is protected. Hydra does not bypass the \
                 technical measures that protect audio and video content, so it \
                 cannot be downloaded."
            ),
            Refusal::Encrypted(m) => write!(f, "{m} encrypted streams are not supported yet"),
            Refusal::Live => write!(f, "this is a live stream; only VOD is supported so far"),
            Refusal::Empty => write!(f, "the playlist lists no segments"),
            Refusal::UnreadableIv => write!(
                f,
                "the playlist's AES-128 key has an IV that is not 16 hex bytes, so the \
                 segments cannot be decrypted correctly"
            ),
            Refusal::UnresolvableKey(u) => {
                write!(f, "the playlist's AES-128 key cannot be fetched from {u:?}")
            }
            Refusal::UndefinedVariable(n) => write!(
                f,
                "the playlist refers to a variable {{${n}}} that no EXT-X-DEFINE declares"
            ),
            Refusal::Unsupported(why) => write!(f, "{why}"),
        }
    }
}

/// A parsed playlist: either a master listing variants, or a media playlist
/// listing segments. One type for both, because a playlist does not announce
/// which it is until a tag says so.
#[derive(Clone, Debug, Default)]
pub struct Playlist {
    pub variants: Vec<Variant>,
    /// `#EXT-X-MEDIA:TYPE=AUDIO` renditions, in the order the master lists
    /// them.
    pub audio: Vec<Rendition>,
    pub segments: Vec<Segment>,
    /// Each segment's `#EXTINF`, seconds, in step with `segments`. A live
    /// recording has no size to report progress against, so how much TIME
    /// has been captured is the only honest measure of how far it has got.
    pub durations: Vec<f64>,
    /// Each segment's media sequence number, in step with `segments`. Not
    /// simply `media_sequence + index`: an `#EXT-X-GAP` segment is left out
    /// of the list but still counts, so the numbers jump where one was.
    pub sequences: Vec<u64>,
    /// The first `#EXT-X-MAP` initialisation segment, present for fragmented
    /// MP4.
    pub init: Option<Segment>,
    /// Every later `#EXT-X-MAP` that names a different initialisation
    /// segment, as `(index, init)`: the init to write before
    /// `segments[index]`. An `#EXT-X-DISCONTINUITY` alone needs nothing
    /// written — the concatenation stays valid — but the new map that
    /// usually follows one does.
    pub init_changes: Vec<(usize, Segment)>,
    /// Summed `#EXTINF`, seconds.
    pub duration: f64,
    pub live: bool,
    /// `#EXT-X-ENDLIST` was present: the playlist will not grow again.
    pub ended: bool,
    /// `#EXT-X-MEDIA-SEQUENCE`. The number of the FIRST segment listed, which
    /// is what makes a segment identifiable across refreshes: a live window
    /// slides, so position in the list means nothing but this plus position
    /// is stable.
    pub media_sequence: u64,
    /// `#EXT-X-TARGETDURATION`, seconds. Sets how often a live playlist is
    /// worth re-reading.
    pub target_duration: f64,
    pub drm: Option<String>,
    pub encryption: Option<String>,
    pub segments_kind: Option<Segments>,
    /// Set when the segments are a packed audio stream rather than a
    /// container. `segments_kind` is [`Segments::Ts`] then; see [`RawAudio`].
    pub raw_audio: Option<RawAudio>,
    /// `#EXT-X-DEFINE:NAME=…,VALUE=…` declarations, for a caller to hand to
    /// [`parse_with_variables`] when a media playlist `IMPORT`s them.
    pub variables: Vec<(String, String)>,
    /// A reason the playlist cannot be assembled that is neither DRM nor an
    /// unsupported cipher: a broken key, an undefined variable.
    pub refusal: Option<Refusal>,
}

impl Playlist {
    pub fn is_master(&self) -> bool {
        !self.variants.is_empty()
    }

    /// The separate audio playlist this variant needs, if it has one.
    ///
    /// A variant that names an `AUDIO` group carries no sound of its own
    /// unless the group's chosen rendition is the in-band one — so ignoring
    /// this tag is how a download ends up silent. The rendition a player
    /// would pick is the one taken: `DEFAULT=YES`, then `AUTOSELECT=YES`,
    /// then the first listed. `None` means there is nothing extra to fetch,
    /// either because no group was named or because the audio is already in
    /// the variant's own segments.
    pub fn audio_for(&self, variant: &Variant) -> Option<&Rendition> {
        let group = variant.audio.as_deref()?;
        let in_group = || self.audio.iter().filter(move |r| r.group == group);
        let chosen = in_group()
            .find(|r| r.default)
            .or_else(|| in_group().find(|r| r.autoselect))
            .or_else(|| in_group().next())?;
        chosen.url.is_some().then_some(chosen)
    }

    /// The window as `(sequence number, segment, seconds)`, so a caller
    /// refreshing a live playlist can tell which segments it has not seen
    /// and how much time it has captured.
    pub fn timed_window(&self) -> Vec<(u64, Segment, f64)> {
        self.segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    self.sequences
                        .get(i)
                        .copied()
                        .unwrap_or_else(|| self.media_sequence.saturating_add(i as u64)),
                    s.clone(),
                    self.durations.get(i).copied().unwrap_or(0.0),
                )
            })
            .collect()
    }

    /// How long to wait before re-reading a live playlist. Half the target
    /// duration keeps up with a sliding window without hammering the origin;
    /// the clamp keeps a manifest that declares something absurd sane.
    pub fn refresh_after(&self) -> std::time::Duration {
        let secs = if self.target_duration > 0.0 {
            self.target_duration / 2.0
        } else {
            3.0
        };
        std::time::Duration::from_secs_f64(secs.clamp(1.0, 10.0))
    }
}

/// Name the DRM system behind a `KEYFORMAT`. Recognising one is how a
/// playlist gets refused; nothing here requests a key.
fn drm_name(keyformat: &str) -> Option<&'static str> {
    let s = keyformat.to_ascii_lowercase();
    if s.contains("edef8ba9-79d6-4ace-a3c8-27dcd51d21ed") || s.contains("widevine") {
        Some("Widevine")
    } else if s.contains("9a04f079-9840-4286-ab92-e65be0885f95") || s.contains("playready") {
        Some("PlayReady")
    } else if s.contains("com.apple.streamingkeydelivery") || s.contains("fairplay") {
        Some("FairPlay")
    } else {
        None
    }
}

/// `KEY=value` / `KEY="quoted,value"` attribute lists, RFC 8216 §4.2.
fn attrs(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let value = if let Some(stripped) = rest.strip_prefix('"') {
            match stripped.find('"') {
                Some(end) => {
                    let v = &stripped[..end];
                    rest = &stripped[end + 1..];
                    v.to_string()
                }
                None => {
                    rest = "";
                    stripped.to_string()
                }
            }
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let v = &rest[..end];
            rest = &rest[end..];
            v.to_string()
        };
        out.push((key, value));
        rest = rest.trim_start_matches(',');
    }
    out
}

/// An enumerated HLS attribute whose only true value is `YES`.
fn yes(v: Option<&str>) -> bool {
    v.is_some_and(|v| v.eq_ignore_ascii_case("YES"))
}

fn attr<'a>(list: &'a [(String, String)], name: &str) -> Option<&'a str> {
    list.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// `#EXT-X-BYTERANGE:<n>[@<o>]` — length, and optionally where it starts.
fn parse_byterange(v: &str) -> Option<(Option<u64>, u64)> {
    let v = v.trim().trim_matches('"');
    let (len, off) = match v.split_once('@') {
        Some((l, o)) => (l, Some(o)),
        None => (v, None),
    };
    let len: u64 = len.trim().parse().ok()?;
    let off = match off {
        Some(o) => Some(o.trim().parse().ok()?),
        None => None,
    };
    Some((off, len))
}

/// `IV=0x...` — 16 bytes, hex, optionally `0X`-prefixed.
fn parse_hex_iv(v: &str) -> Option<[u8; 16]> {
    let h = v.trim().trim_start_matches("0x").trim_start_matches("0X");
    if h.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(h.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The implicit IV: the media sequence number as a 128-bit big-endian
/// number, which puts it in the low 64 bits of the block. Widening to
/// `u128` derives every byte from the sequence number, so no part of the IV
/// is a constant in the source.
fn iv_from_sequence(seq: u64) -> [u8; 16] {
    u128::from(seq).to_be_bytes()
}

fn tag_value(line: &str) -> &str {
    line.split_once(':').map(|(_, v)| v).unwrap_or("")
}

/// The key a `data:` URI carries (RFC 2397), when it is exactly 16 bytes.
fn data_key(uri: &str) -> Option<[u8; 16]> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let bytes = if meta
        .rsplit(';')
        .next()
        .is_some_and(|p| p.eq_ignore_ascii_case("base64"))
    {
        base64_decode(payload)?
    } else {
        percent_decode(payload)
    };
    bytes.try_into().ok()
}

/// Standard base64, padding optional. Small enough to own rather than pull
/// the transport crate in for.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]),
            3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
            2 => out.push((acc >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Replace every `{$name}` (RFC 8216bis §4.4.2.3), or name the first one
/// with no definition.
fn substitute(s: &str, vars: &[(String, String)]) -> Result<String, String> {
    if !s.contains("{$") {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{$") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        match vars.iter().find(|(n, _)| n == name) {
            Some((_, v)) => out.push_str(v),
            None => return Err(name.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The value of query parameter `name` in `url`, percent-decoded.
fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == name).then(|| String::from_utf8_lossy(&percent_decode(v)).into_owned())
    })
}

/// Parse a playlist. `base` is the URL it was fetched from; every URI is
/// resolved against it so the caller only ever sees absolute URLs.
pub fn parse(text: &str, base: &str) -> Playlist {
    parse_with_variables(text, base, &[])
}

/// [`parse`], with the master playlist's `EXT-X-DEFINE` variables available
/// to this playlist's `IMPORT` declarations.
pub fn parse_with_variables(text: &str, base: &str, imported: &[(String, String)]) -> Playlist {
    let mut pl = Playlist::default();
    let mut pending: Option<Vec<(String, String)>> = None;
    let mut want_segment = false;
    // `#EXT-X-BYTERANGE` applies to the URI on the NEXT line, and its offset
    // may be omitted, meaning "where the previous sub-range of this same
    // resource ended".
    let mut pending_range: Option<(Option<u64>, u64)> = None;
    let mut last_end: Option<(String, u64)> = None;
    let mut pending_duration = 0.0f64;
    let mut gap = false;
    // `(key uri, explicit IV)` from the most recent `#EXT-X-KEY`.
    let mut active_key: Option<(String, Option<[u8; 16]>)> = None;
    // The init map in force, so a MAP that merely repeats it is not a change.
    let mut current_init: Option<Segment> = None;
    // Media sequence number of the next segment URI, gaps included.
    let mut seq = 0u64;
    let mut ended = false;
    let mut vod = false;

    let refuse = |pl: &mut Playlist, r: Refusal| {
        pl.refusal.get_or_insert(r);
    };
    let key_for = |active: &Option<(String, Option<[u8; 16]>)>, seq: u64| {
        active.as_ref().map(|(uri, iv)| KeyRef {
            uri: uri.clone(),
            iv: iv.unwrap_or_else(|| iv_from_sequence(seq)),
        })
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let expanded;
        let line = if line.contains("{$") {
            match substitute(line, &pl.variables) {
                Ok(l) => {
                    expanded = l;
                    expanded.as_str()
                }
                Err(name) => {
                    refuse(&mut pl, Refusal::UndefinedVariable(name));
                    continue;
                }
            }
        } else {
            line
        };
        if let Some(v) = line.strip_prefix("#EXT-X-DEFINE:") {
            let a = attrs(v);
            let value = if let Some(name) = attr(&a, "IMPORT") {
                match imported.iter().find(|(n, _)| n == name) {
                    Some((_, v)) => Some((name.to_string(), v.clone())),
                    None => {
                        refuse(&mut pl, Refusal::UndefinedVariable(name.to_string()));
                        None
                    }
                }
            } else if let Some(name) = attr(&a, "QUERYPARAM") {
                match query_param(base, name) {
                    Some(v) => Some((name.to_string(), v)),
                    None => {
                        refuse(&mut pl, Refusal::UndefinedVariable(name.to_string()));
                        None
                    }
                }
            } else {
                attr(&a, "NAME")
                    .map(|n| (n.to_string(), attr(&a, "VALUE").unwrap_or("").to_string()))
            };
            if let Some(v) = value {
                pl.variables.push(v);
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attrs(v));
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(v);
            if attr(&a, "TYPE").is_some_and(|t| t.eq_ignore_ascii_case("AUDIO")) {
                pl.audio.push(Rendition {
                    group: attr(&a, "GROUP-ID").unwrap_or_default().to_string(),
                    name: attr(&a, "NAME").map(str::to_string),
                    language: attr(&a, "LANGUAGE").map(str::to_string),
                    default: yes(attr(&a, "DEFAULT")),
                    autoselect: yes(attr(&a, "AUTOSELECT")),
                    url: attr(&a, "URI").and_then(|u| crate::url::join(base, u)),
                });
            }
        } else if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
            let a = attrs(tag_value(line));
            let method = attr(&a, "METHOD").unwrap_or("").to_ascii_uppercase();
            if method == "NONE" {
                // The segments that FOLLOW are in the clear. Forgetting this
                // leaves the previous key attached to them, and the assembler
                // then "decrypts" plaintext — failing PKCS#7 on a perfectly
                // valid playlist, or worse, passing by coincidence.
                active_key = None;
                continue;
            }
            let fmt = attr(&a, "KEYFORMAT").unwrap_or("identity");
            if let Some(name) = drm_name(fmt) {
                pl.drm.get_or_insert_with(|| name.to_string());
            } else if method.starts_with("SAMPLE-AES") {
                // Encrypts within the media samples rather than the segment,
                // and in practice always arrives with a DRM key system.
                pl.drm.get_or_insert_with(|| "SAMPLE-AES".to_string());
            } else if method == "AES-128" {
                pl.encryption.get_or_insert_with(|| "AES-128".to_string());
                active_key = None;
                // Present but not 16 hex bytes. Falling back to the sequence
                // IV would be UNDETECTABLE corruption: a wrong IV damages only
                // the FIRST cipher block, so every later block decrypts
                // cleanly and the PKCS#7 padding still validates.
                let iv = match attr(&a, "IV").map(parse_hex_iv) {
                    Some(None) => {
                        refuse(&mut pl, Refusal::UnreadableIv);
                        continue;
                    }
                    iv => iv.flatten(),
                };
                let uri = attr(&a, "URI").unwrap_or("");
                let resolved = if uri.starts_with("data:") {
                    data_key(uri).map(|_| uri.to_string())
                } else {
                    crate::url::join(base, uri)
                };
                match resolved {
                    // The key for the segments that FOLLOW, until the next tag.
                    Some(u) => active_key = Some((u, iv)),
                    None => refuse(&mut pl, Refusal::UnresolvableKey(uri.to_string())),
                }
            } else if !method.is_empty() {
                refuse(&mut pl, Refusal::Encrypted(method));
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attrs(v);
            if let Some(uri) = attr(&a, "URI") {
                if let Some(url) = crate::url::join(base, uri) {
                    let range = attr(&a, "BYTERANGE")
                        .and_then(parse_byterange)
                        .map(|(off, len)| (off.unwrap_or(0), len));
                    // RFC 8216 §4.3.2.5: the key in force applies to the
                    // init section too. Its IV, when not explicit, is the
                    // next segment's sequence number.
                    let init = Segment {
                        url,
                        range,
                        key: key_for(&active_key, seq),
                    };
                    if current_init.as_ref() != Some(&init) {
                        let at = pl.segments.len();
                        if pl.init.is_none() && at == 0 {
                            pl.init = Some(init.clone());
                        } else if pl.init_changes.last().is_some_and(|(i, _)| *i == at) {
                            pl.init_changes.pop();
                            pl.init_changes.push((at, init.clone()));
                        } else {
                            pl.init_changes.push((at, init.clone()));
                        }
                        current_init = Some(init);
                    }
                }
                pl.segments_kind = Some(Segments::Fmp4);
            }
        } else if let Some(v) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = parse_byterange(v);
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            pl.media_sequence = v.trim().parse().unwrap_or(0);
            seq = pl.media_sequence;
        } else if let Some(v) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            pl.target_duration = v.trim().parse().unwrap_or(0.0);
        } else if let Some(v) = line.strip_prefix("#EXTINF:") {
            let secs = v
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            pl.duration += secs;
            pending_duration = secs;
            want_segment = true;
        } else if line == "#EXT-X-GAP" {
            gap = true;
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if let Some(v) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            vod = v.trim().eq_ignore_ascii_case("VOD");
        } else if !line.starts_with('#') {
            if want_segment {
                want_segment = false;
                let this_seq = seq;
                seq = seq.saturating_add(1);
                let range = pending_range.take();
                if gap {
                    // Not on the server; fetching it is a 404 and a failed
                    // download. It still occupies a sequence number.
                    gap = false;
                    last_end = None;
                    continue;
                }
                if pl.segments_kind.is_none() {
                    let path = line.split(['?', '#']).next().unwrap_or(line);
                    let lower = path.to_ascii_lowercase();
                    pl.raw_audio = RawAudio::from_path(&lower);
                    pl.segments_kind = Some(
                        if pl.raw_audio.is_some()
                            || lower.ends_with(".ts")
                            || lower.ends_with(".m2ts")
                            || lower.ends_with(".mts")
                        {
                            Segments::Ts
                        } else {
                            Segments::Fmp4
                        },
                    );
                }
                if let Some(url) = crate::url::join(base, line) {
                    let range = match range {
                        Some((off, len)) => {
                            let off = off.unwrap_or_else(|| match &last_end {
                                // Omitted offset: continue where the previous
                                // sub-range of this same resource ended.
                                Some((u, end)) if *u == url => *end,
                                _ => 0,
                            });
                            let Some(end) = off.checked_add(len) else {
                                // Past the end of any file: a range no server
                                // can satisfy, so the segment is dropped.
                                last_end = None;
                                continue;
                            };
                            last_end = Some((url.clone(), end));
                            Some((off, len))
                        }
                        None => {
                            last_end = None;
                            None
                        }
                    };
                    // RFC 8216 §5.2: with no explicit IV the segment's own
                    // media sequence number is the IV. Resolving it here is
                    // what lets a decryptor work from the segment alone.
                    let key = key_for(&active_key, this_seq);
                    pl.segments.push(Segment { url, range, key });
                    pl.durations.push(pending_duration);
                    pl.sequences.push(this_seq);
                }
            } else if let Some(a) = pending.take() {
                let res = attr(&a, "RESOLUTION").unwrap_or("");
                let (w, h) = res
                    .split_once('x')
                    .map(|(w, h)| (w.parse().ok(), h.parse().ok()))
                    .unwrap_or((None, None));
                if let Some(url) = crate::url::join(base, line) {
                    pl.variants.push(Variant {
                        url,
                        bandwidth: attr(&a, "AVERAGE-BANDWIDTH")
                            .or_else(|| attr(&a, "BANDWIDTH"))
                            .and_then(|b| b.parse().ok()),
                        width: w,
                        height: h,
                        codecs: attr(&a, "CODECS").map(str::to_string),
                        audio: attr(&a, "AUDIO").map(str::to_string),
                    });
                }
            }
        }
    }

    pl.ended = ended;
    if !pl.segments.is_empty() {
        // Nothing else distinguishes a growing playlist from a finished one.
        pl.live = !ended && !vod;
    }
    // Highest quality first, the order every picker wants.
    pl.variants
        .sort_by_key(|v| std::cmp::Reverse(v.bandwidth.unwrap_or(0)));
    pl
}

/// Pick the variant closest to what the browser asked for. `want_url` is the
/// exact variant playlist the extension named; failing that, match on height,
/// then take the highest bitrate.
pub fn choose<'a>(
    variants: &'a [Variant],
    want_url: Option<&str>,
    want_height: Option<u32>,
) -> Option<&'a Variant> {
    if let Some(u) = want_url {
        if let Some(v) = variants.iter().find(|v| v.url == u) {
            return Some(v);
        }
    }
    if let Some(h) = want_height {
        if let Some(v) = variants.iter().find(|v| v.height == Some(h)) {
            return Some(v);
        }
        // No exact match: the nearest at or below the request, else the
        // smallest available — never silently larger than asked for.
        let mut below: Vec<&Variant> = variants
            .iter()
            .filter(|v| v.height.is_some_and(|vh| vh <= h))
            .collect();
        below.sort_by_key(|v| std::cmp::Reverse(v.height.unwrap_or(0)));
        if let Some(v) = below.first() {
            return Some(v);
        }
        return variants.iter().min_by_key(|v| v.height.unwrap_or(u32::MAX));
    }
    variants.first()
}

// ------------------------------------------------------------- planning

/// Everything needed to write the file, resolved: no more playlist fetches.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub init: Option<Segment>,
    pub segments: Vec<Segment>,
    pub kind: Segments,
    pub duration: f64,
    /// Bytes the chosen variant implies, for the progress bar. An estimate:
    /// the real total is only known once the last segment lands.
    pub estimated_size: Option<u64>,
    /// `(index, init)`: an initialisation segment to write before
    /// `segments[index]`, wherever the stream switched to a new one.
    pub init_changes: Vec<(usize, Segment)>,
    /// The segments are packed audio frames; see [`RawAudio`].
    pub raw_audio: Option<RawAudio>,
    identity: String,
}

impl Plan {
    /// Turn a fetched media playlist into a plan, or say why not.
    pub fn build(media: &Playlist, bandwidth: Option<u64>) -> Result<Plan, Refusal> {
        if let Some(d) = &media.drm {
            return Err(Refusal::Drm(d.clone()));
        }
        if let Some(r) = &media.refusal {
            return Err(r.clone());
        }
        // AES-128 is NOT refused: it is ordinary content protection, and the
        // key is an ordinary URL. What it does require is that the caller
        // supply the keys (`Plan::key_uris`), which `fetch_all` enforces —
        // writing segments it could not decrypt would produce a file of
        // noise that looks like a successful download.
        if let Some(e) = &media.encryption {
            if e != "AES-128" {
                return Err(Refusal::Encrypted(e.clone()));
            }
        }
        if media.live {
            return Err(Refusal::Live);
        }
        if media.segments.is_empty() {
            return Err(Refusal::Empty);
        }
        Ok(Plan::assemble(
            media.init.clone(),
            media.init_changes.clone(),
            media.segments.clone(),
            media.segments_kind.unwrap_or(Segments::Ts),
            media.raw_audio,
            media.duration,
            bandwidth,
        ))
    }

    pub(crate) fn assemble(
        init: Option<Segment>,
        init_changes: Vec<(usize, Segment)>,
        segments: Vec<Segment>,
        kind: Segments,
        raw_audio: Option<RawAudio>,
        duration: f64,
        bandwidth: Option<u64>,
    ) -> Plan {
        let identity = identity(init.as_ref(), &segments);
        Plan {
            init,
            segments,
            kind,
            duration,
            estimated_size: bandwidth
                .filter(|_| duration > 0.0)
                .map(|b| (b as f64 * duration / 8.0) as u64),
            init_changes,
            raw_audio,
            identity,
        }
    }

    /// Every segment and init this plan fetches, in the order they are
    /// written.
    fn objects(&self) -> impl Iterator<Item = &Segment> {
        self.init
            .iter()
            .chain(self.init_changes.iter().map(|(_, s)| s))
            .chain(&self.segments)
    }

    /// Every distinct key URL this plan needs fetched, in first-use order.
    ///
    /// The library performs no IO, so fetching them is the caller's job: it
    /// already has the connector, the cookies and the session that the
    /// manifest itself was fetched with, and the key must be fetched with
    /// exactly those. Keys carried inline as `data:` URIs are not listed;
    /// [`Plan::inline_keys`] has them.
    pub fn key_uris(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.objects() {
            if let Some(k) = &s.key {
                if !k.uri.starts_with("data:") && !out.iter().any(|u| u == &k.uri) {
                    out.push(k.uri.clone());
                }
            }
        }
        out
    }

    /// The keys the playlist carried inline as `data:` URIs, ready to use.
    pub fn inline_keys(&self) -> Keys {
        let mut out = Keys::new();
        for s in self.objects() {
            if let Some(k) = &s.key {
                if k.uri.starts_with("data:") && !out.contains_key(&k.uri) {
                    if let Some(key) = data_key(&k.uri) {
                        out.insert(k.uri.clone(), key);
                    }
                }
            }
        }
        out
    }

    /// Whether any segment needs a key.
    pub fn is_encrypted(&self) -> bool {
        self.objects().any(|s| s.key.is_some())
    }

    /// A stable name for this exact plan, used to tell one rendition's
    /// staging file from another's: the first segment's URL without its
    /// query string, the segment count and the init's URL. Query strings
    /// are left out because signed and rotating tokens would otherwise make
    /// every refetched playlist a different plan, and nothing could resume.
    ///
    /// Whitespace-free by construction, which matters because the
    /// checkpoint sidecar is whitespace-separated.
    pub fn id(&self) -> &str {
        &self.identity
    }

    /// The extension the assembled file carries natively — `.ts`, `.mp4`,
    /// or the audio stream's own — before any remux the caller may ask for.
    pub fn native_ext(&self) -> &'static str {
        match (self.raw_audio, self.kind) {
            (Some(a), _) => a.ext(),
            (None, Segments::Ts) => "ts",
            (None, Segments::Fmp4) => "mp4",
        }
    }
}

fn identity(init: Option<&Segment>, segments: &[Segment]) -> String {
    let path_only = |s: &Segment| {
        s.url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .replace(char::is_whitespace, "%20")
    };
    format!(
        "{}|{}|{}",
        segments.first().map(path_only).unwrap_or_default(),
        segments.len(),
        init.map(path_only).unwrap_or_default()
    )
}

// ------------------------------------------------------------- assembly

/// What the progress ticker reads. Bytes are counted at TWO granularities on
/// purpose: segments already appended to the output, plus the live byte
/// counters of the segments still arriving. Without the second half the total
/// only moves when a whole segment lands, which makes a rate computed from it
/// collapse to zero between segments and an ETA built on that meaningless.
#[derive(Debug, Default)]
pub struct Meter {
    inner: std::sync::Mutex<MeterInner>,
}

#[derive(Debug, Default)]
struct MeterInner {
    /// Bytes appended to the output file.
    settled: u64,
    /// Segments appended.
    done: u64,
    /// Segments in flight: (label, bytes so far).
    inflight: Vec<(String, Arc<AtomicU64>)>,
    /// The connection table's rows. Fixed length once opened.
    lanes: Vec<LaneInner>,
}

/// One in-flight segment, for the per-connection rows.
#[derive(Clone, Debug, PartialEq)]
pub struct InFlight {
    pub label: String,
    pub bytes: u64,
}

#[derive(Debug, Default)]
struct LaneInner {
    /// Bytes this lane has pulled over the wire, across every segment it has
    /// carried. Includes the partial bytes of attempts that then failed,
    /// because those crossed the wire too.
    bytes: u64,
    segments: u64,
    state: LaneState,
    /// The counter of the segment on this lane right now.
    current: Option<Arc<AtomicU64>>,
}

/// What a connection is doing, or — once it has handed its lane back — what
/// it last did. A finished transfer therefore leaves a table of totals and
/// outcomes rather than blanking every row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaneState {
    /// Never used: the transfer needed fewer connections than were offered.
    #[default]
    Unused,
    /// Holds a slot, no bytes yet.
    Connecting,
    Receiving,
    Completed,
    Failed,
}

/// One row of the connection table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lane {
    /// Cumulative for the life of the transfer, in-flight bytes included.
    pub bytes: u64,
    /// Segments carried to completion on this lane.
    pub segments: u64,
    pub state: LaneState,
}

impl Meter {
    /// Start from bytes already on disk from an earlier run, so the bar and
    /// the rate continue instead of restarting at zero.
    pub fn preload(&self, bytes: u64, segments: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.settled += bytes;
            g.done += segments;
        }
    }

    pub fn begin(&self, label: String, counter: Arc<AtomicU64>) {
        if let Ok(mut g) = self.inner.lock() {
            g.inflight.push((label, counter));
        }
    }

    /// Move a finished segment from "arriving" to "written", in one step so
    /// a reader can never see it counted twice or not at all.
    pub fn settle(&self, counter: &Arc<AtomicU64>, bytes: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.inflight.retain(|(_, c)| !Arc::ptr_eq(c, counter));
            g.settled += bytes;
            g.done += 1;
        }
    }

    pub fn drop_inflight(&self, counter: &Arc<AtomicU64>) {
        if let Ok(mut g) = self.inner.lock() {
            g.inflight.retain(|(_, c)| !Arc::ptr_eq(c, counter));
        }
    }

    /// `(bytes so far, segments done, segments arriving)`.
    pub fn snapshot(&self) -> (u64, u64, Vec<InFlight>) {
        let Ok(g) = self.inner.lock() else {
            return (0, 0, vec![]);
        };
        let live: Vec<InFlight> = g
            .inflight
            .iter()
            .map(|(label, c)| InFlight {
                label: label.clone(),
                bytes: c.load(Ordering::Relaxed),
            })
            .collect();
        let total = g.settled + live.iter().map(|f| f.bytes).sum::<u64>();
        (total, g.done, live)
    }

    /// `(bytes so far, segments done)` — [`Meter::snapshot`] without the
    /// in-flight list, which the ticker reads ten times a second and only
    /// ever used to clone a label per segment in flight and throw it away.
    pub fn totals(&self) -> (u64, u64) {
        let Ok(g) = self.inner.lock() else {
            return (0, 0);
        };
        let live: u64 = g
            .inflight
            .iter()
            .map(|(_, c)| c.load(Ordering::Relaxed))
            .sum();
        (g.settled + live, g.done)
    }

    /// Bytes actually on disk in the output, which is what a resume or a
    /// final size must be based on.
    pub fn settled(&self) -> u64 {
        self.inner.lock().map(|g| g.settled).unwrap_or(0)
    }

    /// Open the connection table at `n` rows, once, before any fetching.
    ///
    /// The table is FIXED at the number of connections the transfer may
    /// actually run, which is the semaphore's permit count. Sizing it from
    /// the segments queued instead would show three times as many rows as
    /// there are connections, most of them idle, because the pipeline
    /// deliberately queues ahead of what it can admit.
    pub fn open_lanes(&self, n: usize) {
        if let Ok(mut g) = self.inner.lock() {
            // Idempotent at the same width. A split DASH transfer assembles
            // video and then audio through two calls of the same fetch loop,
            // and clearing here dropped every row's running total back to
            // zero halfway down the download.
            if g.lanes.len() == n {
                return;
            }
            g.lanes.clear();
            g.lanes.resize_with(n, LaneInner::default);
        }
    }

    /// Put a segment on the lowest free lane, returning which.
    ///
    /// Called once a permit is IN HAND, never at queue time: a lane stands
    /// for a connection that is really transferring. `None` means no table
    /// was opened, and the caller carries on without one.
    pub fn occupy(self: &Arc<Self>, counter: &Arc<AtomicU64>) -> Option<LaneGuard> {
        let i = {
            let mut g = self.inner.lock().ok()?;
            let i = g.lanes.iter().position(|l| l.current.is_none())?;
            let lane = &mut g.lanes[i];
            lane.current = Some(counter.clone());
            lane.state = LaneState::Connecting;
            i
        };
        // Handed back by Drop rather than by a matching call. A fetch future
        // that panics would otherwise leave its lane occupied forever, and
        // once every lane has leaked, `occupy` returns None and the whole
        // table freezes mid-transfer.
        Some(LaneGuard {
            meter: self.clone(),
            lane: i,
            ok: false,
        })
    }

    /// Hand a lane back, banking what it moved.
    ///
    /// The bytes stay on the lane after it goes free — that running total,
    /// not the current segment's counter, is what makes the table readable:
    /// a per-segment count resets every few seconds and never shows how much
    /// a connection has actually done.
    fn vacate(&self, lane: usize, ok: bool) {
        if let Ok(mut g) = self.inner.lock() {
            let Some(l) = g.lanes.get_mut(lane) else {
                return;
            };
            if let Some(c) = l.current.take() {
                l.bytes += c.load(Ordering::Relaxed);
            }
            l.segments += u64::from(ok);
            l.state = if ok {
                LaneState::Completed
            } else {
                LaneState::Failed
            };
        }
    }

    /// The connection table as it stands.
    pub fn lanes(&self) -> Vec<Lane> {
        let Ok(g) = self.inner.lock() else {
            return vec![];
        };
        g.lanes
            .iter()
            .map(|l| {
                let live = l
                    .current
                    .as_ref()
                    .map(|c| c.load(Ordering::Relaxed))
                    .unwrap_or(0);
                Lane {
                    // The in-flight bytes are added rather than banked, so
                    // the row climbs smoothly instead of standing still for
                    // a segment and then jumping.
                    bytes: l.bytes + live,
                    segments: l.segments,
                    state: match (l.current.is_some(), live) {
                        (true, 0) => LaneState::Connecting,
                        (true, _) => LaneState::Receiving,
                        (false, _) => l.state,
                    },
                }
            })
            .collect()
    }
}

/// How far a track's assembly got, so a paused stream can carry on.
///
/// A stream has no byte ranges to resume from — a server will not serve
/// "the second half of this playlist". What it does have is an ordered list
/// of whole segments, so the resumable unit is the segment: record how many
/// have been appended and how many bytes that is, and a restart can skip
/// them and keep appending.
///
/// The byte count is the check as well as the record: if the file on disk is
/// not exactly that long it is not the file this checkpoint describes, and
/// the assembly starts over rather than splicing onto something unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checkpoint {
    pub segments: u64,
    pub bytes: u64,
    /// Which plan these bytes came from.
    ///
    /// Count and length alone cannot tell two renditions apart. The desktop
    /// app pins the chosen variant in its persisted state, but the CLI
    /// re-chooses from flags on every run — so `--quality 360`, Ctrl-C,
    /// `--quality 1080` would find a checkpoint that looks perfectly usable
    /// and append 1080p segments onto a 360p prefix. The result is a file
    /// that plays wrong or not at all, with nothing to indicate why.
    pub plan_id: String,
}

impl Checkpoint {
    /// Read the sidecar next to a staging file. Anything unreadable or
    /// malformed reads as "start from the beginning".
    pub fn read(path: &str) -> Checkpoint {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Checkpoint::default();
        };
        let mut parts = text.split_whitespace();
        let segments = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let bytes = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        // A sidecar written before plan ids existed has no third field; it
        // reads as "belongs to no plan" and is refused, which restarts the
        // assembly rather than trusting bytes of unknown provenance.
        let plan_id = parts.next().unwrap_or_default().to_string();
        Checkpoint {
            segments,
            bytes,
            plan_id,
        }
    }

    fn write(path: &str, segments: u64, bytes: u64, plan_id: &str) {
        let _ = std::fs::write(path, format!("{segments} {bytes} {plan_id}\n"));
    }

    /// Whether `part` really is the file this checkpoint describes, and
    /// `plan` really is the plan it was recorded against.
    pub fn usable(&self, part: &str, plan: &Plan) -> bool {
        self.segments > 0
            && self.segments as usize <= plan.segments.len()
            && !self.plan_id.is_empty()
            && self.plan_id == plan.id()
            && std::fs::metadata(part).map(|m| m.len()).unwrap_or(0) == self.bytes
    }
}

/// A lane held for the life of one fetch attempt.
///
/// Dropping it hands the lane back and banks what it moved, so the table
/// cannot be left holding a row for work that is no longer running — however
/// the attempt ended, panic included.
pub struct LaneGuard {
    meter: Arc<Meter>,
    lane: usize,
    ok: bool,
}

impl LaneGuard {
    /// Record how the attempt went. Consumes the guard, which releases the
    /// lane, so an outcome can never be reported to a lane already reused.
    pub fn finish(mut self, ok: bool) {
        self.ok = ok;
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        self.meter.vacate(self.lane, self.ok);
    }
}

/// Trim a planned window to a media-duration budget, advancing `spent`.
///
/// A length ask — `--record-seconds`, or the GUI's equivalent — promises how
/// much MEDIA comes back, and the wall clock cannot keep that promise once a
/// window is fetched concurrently: draining a backlog gathers media far
/// faster than real time, so a DVR playlist holding hours would hand back all
/// of it well inside a "60 seconds" ask.
///
/// `spent` carries across windows, so successive refreshes continue one
/// budget rather than each getting a fresh one.
///
/// Shared because both recorders need exactly this rule and a second copy is
/// a second thing to get wrong — the GUI and the CLI had already drifted once
/// on how a length ask was enforced.
pub fn trim_to_budget<T>(window: &mut Vec<T>, spent: &mut f64, max: f64, secs: impl Fn(&T) -> f64) {
    window.retain(|e| {
        // Keeps the entry that CROSSES the mark rather than stopping short of
        // it: 30 s of a stream cut into 10 s segments should come back as 30,
        // not 20.
        let fits = *spent < max;
        if fits {
            *spent += secs(e);
        }
        fits
    });
}

/// AES-128 keys, by the URL the playlist named them at.
pub type Keys = std::collections::HashMap<String, [u8; 16]>;

/// Read budget for fetching one content key.
///
/// A key is exactly 16 bytes, so this looks absurdly generous — but the cap
/// `fetch_small` enforces covers the RESPONSE HEAD as well as the body. Sized
/// to the payload, the real headroom for headers is whatever slack that
/// function keeps, and a CDN answering with a long `Set-Cookie` or CSP can
/// spend it — failing a perfectly good key fetch with "response exceeds the
/// small-fetch cap". Nothing is lost by being generous here: the length is
/// checked exactly (`== 16`) once the bytes are in hand, so a server sending
/// anything else is still rejected.
pub const KEY_FETCH_CAP: usize = 4096;

/// Decrypt `part` when its segment is encrypted, and report the plaintext
/// length. A segment with no key passes through untouched.
fn decrypt_segment(
    key: Option<&KeyRef>,
    part: &str,
    keys: &Keys,
    raw: u64,
) -> std::io::Result<u64> {
    let Some(k) = key else { return Ok(raw) };
    let key = keys.get(&k.uri).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("no AES-128 key supplied for {}", k.uri),
        )
    })?;
    decrypt_in_place(part, key, &k.iv)
}

/// Decrypt a segment file in place.
///
/// AES-128-CBC with PKCS#7 padding, per RFC 8216 §5.2. Each segment is
/// independently encrypted under its own IV, which is why this can work on
/// one file at a time and why a lost segment does not poison the rest.
///
/// The whole segment is held in memory, and that is deliberate. PKCS#7 lives
/// in the FINAL block, so a streaming decrypt would have to buffer a block
/// behind the reader and special-case the tail — more moving parts guarding a
/// bound that is already small. Callers run this from the in-order append
/// loop, one segment at a time, never from the fetch tasks: peak cost is one
/// segment (single-digit MB for the 2–10 s targetdurations in the wild), not
/// one per connection in flight.
fn decrypt_in_place(path: &str, key: &[u8; 16], iv: &[u8; 16]) -> std::io::Result<u64> {
    use aes::cipher::{block_padding::Pkcs7, BlockModeDecrypt, KeyIvInit};

    let mut buf = std::fs::read(path)?;
    if buf.is_empty() || buf.len() % 16 != 0 {
        // CBC ciphertext is a whole number of blocks. Anything else is a
        // truncated download or not ciphertext at all, and decrypting it
        // would produce plausible-looking noise.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "encrypted segment is {} bytes, not a block multiple",
                buf.len()
            ),
        ));
    }
    let plain_len = cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded::<Pkcs7>(&mut buf)
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("could not decrypt the segment: {e}"),
            )
        })?
        .len();
    buf.truncate(plain_len);
    std::fs::write(path, &buf)?;
    Ok(plain_len as u64)
}

/// A segment being fetched: its task, its staging file, its live byte
/// counter, its key, and whether it is a media segment (an init between
/// two is written but not counted as one).
type Pending = (
    tokio::task::JoinHandle<std::io::Result<u64>>,
    String,
    Arc<AtomicU64>,
    Option<KeyRef>,
    bool,
);

/// Fetch one segment to `dest`, updating `counter` as bytes arrive.
pub type FetchSeg =
    std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<u64>> + Send>>;

/// What a caller must supply: fetch this segment (honouring its byte range,
/// if it has one) into `dest`, updating the counter as bytes arrive.
pub trait Fetcher:
    Fn(Segment, String, Arc<AtomicU64>) -> FetchSeg + Send + Sync + Clone + 'static
{
}
impl<F> Fetcher for F where
    F: Fn(Segment, String, Arc<AtomicU64>) -> FetchSeg + Send + Sync + Clone + 'static
{
}

/// How many segments to keep in flight.
///
/// Fixed, not adaptive, and that is a measured decision rather than a
/// simplification. Incremental admission — the controller the range
/// scheduler uses to find its connection count — was tried here and made
/// transfers consistently SLOWER: 17-19 s against 10-13 s for a fixed eight,
/// on the same 100-segment playlist, alternating runs.
///
/// Two reasons, both structural rather than fixable by tuning:
///
///  * A stream's useful ceiling is already small (eight or so), so a ramp
///    can only ever reach the same level later. There is no large win to
///    discover, only startup cost to pay.
///  * The goodput signal is contaminated. Segments are queued well ahead of
///    the append point, so bytes-per-second measured at the appending end
///    reports the burst rate of the queue draining, not what the requests
///    achieved. One window read 22 MB/s on a path doing 5.
///
/// If this is revisited, the signal has to be measured at the fetching end,
/// per completed request — not from the output file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Concurrency {
    /// Ceiling. 0 means [`DEFAULT_CONCURRENCY`].
    pub max: usize,
}

impl Concurrency {
    /// Exactly `n` in flight, or the default when `n` is 0.
    pub fn fixed(n: usize) -> Concurrency {
        Concurrency { max: n }
    }

    /// What `max` actually resolves to, 0 meaning the default. Public so a
    /// caller running its own gate lands on the same number this would.
    pub fn ceiling(&self) -> usize {
        match self.max {
            0 => DEFAULT_CONCURRENCY,
            n => n.min(MAX_CONCURRENCY),
        }
    }
}

/// Where a track's assembly is starting from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Resume<'a> {
    /// Segments already appended to the output.
    pub skip: usize,
    /// Bytes those segments account for.
    pub bytes: u64,
    /// Sidecar to record progress in, so a pause can be picked up later.
    pub checkpoint: Option<&'a str>,
}

pub async fn fetch_all<F>(
    plan: &Plan,
    out: &mut std::fs::File,
    staging: &str,
    fetch: F,
    meter: &Arc<Meter>,
    cancel: &Arc<AtomicBool>,
    resume: Resume<'_>,
    concurrency: Concurrency,
    keys: &Keys,
) -> std::io::Result<u64>
where
    F: Fetcher,
{
    let stopped = || cancel.load(Ordering::Relaxed);
    let interrupted = || std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled");
    let ceiling = concurrency.ceiling();
    let inline = plan.inline_keys();
    let merged;
    let keys = if inline.is_empty() {
        keys
    } else {
        merged = keys
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .chain(inline)
            .collect();
        &merged
    };
    // Refuse before a single byte is fetched. Discovering a missing key
    // segment-by-segment would leave a part-written file, and writing the
    // ciphertext instead would produce noise that looks like a download that
    // worked.
    for seg in plan.objects() {
        if let Some(k) = &seg.key {
            if !keys.contains_key(&k.uri) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("no AES-128 key supplied for {}", k.uri),
                ));
            }
        }
    }

    // Bytes already appended by an earlier run; every checkpoint written here
    // is an absolute total, not a delta.
    let mut appended = resume.bytes;
    let mut settled_segments = resume.skip as u64;
    let note = |segs: u64, bytes: u64| {
        if let Some(p) = resume.checkpoint {
            Checkpoint::write(p, segs, bytes, plan.id());
        }
    };

    // One gate for every request this call makes, the init map included.
    let gate = Arc::new(tokio::sync::Semaphore::new(ceiling));
    // The table has exactly as many rows as the gate has permits.
    meter.open_lanes(ceiling);

    // The init map must land first: without ftyp+moov the fragments that
    // follow are unplayable. It is part of the resumed prefix, so it is
    // fetched only on a fresh start.
    if resume.skip == 0 {
        if let Some(init) = &plan.init {
            if stopped() {
                return Err(interrupted());
            }
            let part = format!("{staging}.init");
            let counter = Arc::new(AtomicU64::new(0));
            meter.begin("init".into(), counter.clone());
            match with_retries(&fetch, init, &part, &counter, &gate, meter).await {
                Ok(n) => {
                    let n = decrypt_segment(init.key.as_ref(), &part, keys, n)?;
                    append(&part, out)?;
                    meter.settle(&counter, n);
                    appended += n;
                    note(settled_segments, appended);
                }
                Err(e) => {
                    meter.drop_inflight(&counter);
                    let _ = std::fs::remove_file(&part);
                    return Err(e);
                }
            }
        }
    }

    // A PIPELINE, not a batch, and the distinction is the whole performance
    // story.
    //
    // The output has to be written in playlist order, so the appending end
    // is inherently serial. The obvious implementation therefore fetches N,
    // waits for all N, appends them, and fetches the next N — which leaves
    // every socket idle from the moment the fastest segment in a group lands
    // until the slowest one does, and does it once per group.
    //
    // Here the two ends are separated. Tasks are queued well ahead of the
    // append point and a semaphore — not the append point — decides how many
    // may be in flight, so one slow segment stalls only its own append, not
    // the requests behind it. Finished segments wait their turn as staging
    // FILES, so running ahead costs disk rather than memory, and `lookahead`
    // bounds even that.
    // Queued ahead of the permit count, so admitting a request puts it to
    // work immediately rather than waiting for the next spawn.
    let lookahead = ceiling.saturating_mul(3).max(ceiling + 1);
    let mut inflight: std::collections::VecDeque<Pending> =
        std::collections::VecDeque::with_capacity(lookahead);
    let mut next = resume.skip;
    let total = plan.segments.len();
    // A changed init map is fetched and written in sequence, just ahead of
    // the first segment that needs it. Changes before the resume point are
    // already on disk.
    let changes: &[(usize, Segment)] = &plan.init_changes;
    let mut change_i = changes.partition_point(|(at, _)| *at < resume.skip);

    let outcome = loop {
        // Checked here rather than only in the spawn guard: with nothing yet
        // in flight, a cancelled transfer would otherwise fall straight out
        // of the loop and report success over zero bytes.
        if stopped() {
            break Err(interrupted());
        }
        while inflight.len() < lookahead && next < total {
            let due = changes.get(change_i).filter(|(at, _)| *at <= next);
            if due.is_some() {
                change_i += 1;
            }
            let (seg, part, label, is_segment) = match due {
                Some((at, init)) => (
                    init.clone(),
                    format!("{staging}.i{at}"),
                    "Init".to_string(),
                    false,
                ),
                None => {
                    let i = next;
                    next += 1;
                    (
                        plan.segments[i].clone(),
                        format!("{staging}.p{i}"),
                        format!("Segment {}", i + 1),
                        true,
                    )
                }
            };
            let key = seg.key.clone();
            let counter = Arc::new(AtomicU64::new(0));
            meter.begin(label, counter.clone());
            let (fetch, part2, c2, gate, m2) = (
                fetch.clone(),
                part.clone(),
                counter.clone(),
                gate.clone(),
                meter.clone(),
            );
            let task = tokio::spawn(async move {
                // The permit is acquired PER ATTEMPT, inside `with_retries`,
                // not held across the whole call: the backoff between tries
                // is a sleep, and sleeping while holding a permit idles a
                // slot the rest of the pipeline could be using. On a flaky
                // origin that is the difference between one slow segment and
                // a window running at reduced width for as long as it lasts.
                with_retries(&fetch, &seg, &part2, &c2, &gate, &m2).await
            });
            inflight.push_back((task, part, counter, key, is_segment));
        }
        let Some((task, part, counter, key, is_segment)) = inflight.pop_front() else {
            break Ok(());
        };
        let result = match task.await {
            Ok(r) => r,
            Err(e) => Err(std::io::Error::other(format!("segment task: {e}"))),
        };
        match result {
            Ok(n) if !stopped() => {
                // Decrypt before appending: the output is plaintext, so a
                // resumed run never has to know which of its existing bytes
                // were once encrypted.
                let n = match decrypt_segment(key.as_ref(), &part, keys, n) {
                    Ok(n) => n,
                    Err(e) => {
                        meter.drop_inflight(&counter);
                        let _ = std::fs::remove_file(&part);
                        break Err(e);
                    }
                };
                if let Err(e) = append(&part, out) {
                    meter.drop_inflight(&counter);
                    break Err(e);
                }
                meter.settle(&counter, n);
                appended += n;
                settled_segments += u64::from(is_segment);
                // Written after the bytes are in the output file, so a
                // checkpoint never claims more than is on disk.
                note(settled_segments, appended);
            }
            Ok(_) => {
                meter.drop_inflight(&counter);
                let _ = std::fs::remove_file(&part);
                break Err(interrupted());
            }
            Err(e) => {
                meter.drop_inflight(&counter);
                let _ = std::fs::remove_file(&part);
                break Err(e);
            }
        }
    };

    // Whatever happened, the siblings still running must be drained: their
    // staging files would otherwise be written after this returns.
    for (task, part, counter, _, _) in inflight {
        task.abort();
        let _ = task.await;
        meter.drop_inflight(&counter);
        let _ = std::fs::remove_file(&part);
    }
    outcome?;

    use std::io::Write;
    out.flush()?;
    note(settled_segments, appended);
    Ok(appended)
}

/// Append a staging file to the output and remove it.
fn append(part: &str, out: &mut std::fs::File) -> std::io::Result<()> {
    let mut f = std::fs::File::open(part)?;
    std::io::copy(&mut f, out)?;
    drop(f);
    let _ = std::fs::remove_file(part);
    Ok(())
}

/// One segment, retried. A single lost segment would leave a hole no player
/// recovers from, so this is the one place that insists. Each attempt starts
/// the staging file and the byte counter from zero, so a partial first try
/// cannot be counted twice or spliced onto a second.
async fn with_retries<F>(
    fetch: &F,
    seg: &Segment,
    dest: &str,
    counter: &Arc<AtomicU64>,
    gate: &Arc<tokio::sync::Semaphore>,
    meter: &Arc<Meter>,
) -> std::io::Result<u64>
where
    F: Fetcher,
{
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        // Zeroed only BETWEEN attempts, below — not here. Resetting before
        // the first try is a no-op, and resetting a counter the progress
        // ticker is already summing makes the total step backwards, which
        // reads as a rate that briefly collapses.
        if attempt > 0 {
            counter.store(0, Ordering::Relaxed);
        }
        // Held for the request only. Dropped before the backoff below, so a
        // waiting segment can use the slot while this one sits out its
        // penalty.
        let outcome = {
            let _permit = gate
                .acquire()
                .await
                .map_err(|_| std::io::Error::other("fetch gate closed"))?;
            // The permit IS the connection, so the lane is taken here and
            // given back below — never around the whole call, which would
            // hold a row open through the backoff while nothing transfers.
            let lane = meter.occupy(counter);
            let r = watched(
                fetch(seg.clone(), dest.to_string(), counter.clone()),
                counter,
            )
            .await;
            if let Some(l) = lane {
                l.finish(matches!(r, Ok(n) if n > 0));
            }
            r
        };
        match outcome {
            Ok(n) if n > 0 => return Ok(n),
            Ok(_) => last = Some(std::io::Error::other("empty segment")),
            // Cancellation is not a flaky segment; stop immediately.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(e),
            Err(e) => last = Some(e),
        }
        let _ = std::fs::remove_file(dest);
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(250 << attempt)).await;
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("segment failed")))
}

/// Run one fetch attempt under the idle watchdog: abandoned once `counter`
/// has not moved for [`ATTEMPT_TIMEOUT`], or after [`ATTEMPT_CEILING`]
/// regardless.
async fn watched<Fut>(fut: Fut, counter: &AtomicU64) -> std::io::Result<u64>
where
    Fut: std::future::Future<Output = std::io::Result<u64>>,
{
    tokio::pin!(fut);
    let started = tokio::time::Instant::now();
    let mut seen = counter.load(Ordering::Relaxed);
    let mut moved_at = started;
    let mut ticker = tokio::time::interval(WATCHDOG_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            r = &mut fut => return r,
            _ = ticker.tick() => {
                let now = counter.load(Ordering::Relaxed);
                if now != seen {
                    seen = now;
                    moved_at = tokio::time::Instant::now();
                } else if moved_at.elapsed() >= ATTEMPT_TIMEOUT {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("segment stalled: no bytes for {}s", ATTEMPT_TIMEOUT.as_secs()),
                    ));
                }
                if started.elapsed() >= ATTEMPT_CEILING {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "segment still arriving after {}h; abandoned",
                            ATTEMPT_CEILING.as_secs() / 3600
                        ),
                    ));
                }
            }
        }
    }
}

/// Cap for one playlist body.
pub const fn playlist_cap() -> usize {
    PLAYLIST_MAX
}

// ---------------------------------------------------------------- remux

/// What turning the assembled file into what was asked for actually takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// The assembled file already is the answer: rename it.
    Move,
    /// Hand it to ffmpeg — a stream copy, never a re-encode.
    Remux,
    /// It cannot be produced here, and saying so beats writing a file that
    /// will not play.
    Refuse,
}

/// Decide how to finish, given what the segments were, what was asked for,
/// and whether ffmpeg is on this machine.
///
/// The rule when ffmpeg IS available is "use it for MP4", including for
/// fragmented MP4 that would already play: a remux moves `moov` to the front
/// (`+faststart`) and produces an ordinary MP4 rather than a fragmented one,
/// which is what editors and older players actually want. Without ffmpeg,
/// fragmented MP4 is still handed over as-is, because it plays — only
/// MPEG-TS asked to become MP4 has no honest answer.
pub fn plan_finish(kind: Segments, want_ext: &str, ffmpeg_present: bool) -> Finish {
    let want_mp4 = want_ext.eq_ignore_ascii_case("mp4");
    match (want_mp4, kind, ffmpeg_present) {
        // Asked for what the segments already are: a rename.
        (false, Segments::Ts, _) => Finish::Move,
        // Fragmented MP4 asked to become MPEG-TS. ffmpeg can write that;
        // renaming cannot, and a `.ts` full of fMP4 is a file that lies
        // about itself.
        (false, Segments::Fmp4, true) => Finish::Remux,
        (false, Segments::Fmp4, false) => Finish::Refuse,
        // MP4 always goes through ffmpeg when it is available: it moves
        // `moov` to the front and produces an ordinary MP4 rather than a
        // fragmented one, which is what editors and older players want.
        (true, _, true) => Finish::Remux,
        // Without it, fragmented MP4 still plays as assembled...
        (true, Segments::Fmp4, false) => Finish::Move,
        // ...but MPEG-TS has no honest way to become MP4.
        (true, Segments::Ts, false) => Finish::Refuse,
    }
}

/// Produce `dst` from the assembled `src`, whichever way this machine can.
/// What [`finish`] actually did, so the caller can say so.
///
/// The fMP4 fallback is invisible otherwise: ffmpeg declined, the assembled
/// fragmented file was handed over instead, and nobody learns the output is
/// not the faststart MP4 they asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finished {
    /// Placed as assembled.
    AsIs,
    /// Remuxed through ffmpeg.
    Remuxed,
    /// The remux was attempted and refused; the playable assembly was kept.
    /// Carries ffmpeg's reason.
    RemuxSkipped(String),
}

pub fn finish(
    kind: Segments,
    want_ext: &str,
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<Finished, String> {
    let refusal = match kind {
        Segments::Ts => "MPEG-TS to MP4 needs ffmpeg; install it, or ask for TS instead",
        Segments::Fmp4 => {
            "these segments are fragmented MP4, not MPEG-TS; ask for MP4, or install ffmpeg"
        }
    };
    finish_by(
        plan_finish(kind, want_ext, ffmpeg().is_some()),
        kind == Segments::Fmp4,
        refusal,
        src,
        dst,
    )
}

/// [`plan_finish`] for a plan rather than a bare kind, so a packed audio
/// stream is placed under its own extension instead of being mistaken for
/// transport stream.
pub fn plan_finish_for(plan: &Plan, want_ext: &str, ffmpeg_present: bool) -> Finish {
    match plan.raw_audio {
        None => plan_finish(plan.kind, want_ext, ffmpeg_present),
        Some(audio) if want_ext.eq_ignore_ascii_case(audio.ext()) => Finish::Move,
        // Any container around bare frames is a real mux job.
        Some(_) if ffmpeg_present => Finish::Remux,
        Some(_) => Finish::Refuse,
    }
}

/// [`finish`] for a plan rather than a bare kind; see [`plan_finish_for`].
pub fn finish_plan(
    plan: &Plan,
    want_ext: &str,
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<Finished, String> {
    let Some(audio) = plan.raw_audio else {
        return finish(plan.kind, want_ext, src, dst);
    };
    let refusal = format!(
        "these segments are raw {} audio; ask for .{} , or install ffmpeg to get .{want_ext}",
        audio.ext().to_ascii_uppercase(),
        audio.ext()
    );
    finish_by(
        plan_finish_for(plan, want_ext, ffmpeg().is_some()),
        true,
        &refusal,
        src,
        dst,
    )
}

/// Carry out a [`Finish`] decision. `plays_as_is` says the assembly is
/// already a playable file, so a failed remux falls back to placing it and
/// saying so rather than losing the download.
fn finish_by(
    decision: Finish,
    plays_as_is: bool,
    refusal: &str,
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<Finished, String> {
    let place = || {
        std::fs::rename(src, dst)
            .or_else(|_| {
                std::fs::copy(src, dst).map(|_| {
                    let _ = std::fs::remove_file(src);
                })
            })
            .map_err(|e| format!("could not move the finished file: {e}"))
    };
    match decision {
        Finish::Move => place().map(|()| Finished::AsIs),
        Finish::Remux => match remux(src, dst, Segments::Fmp4) {
            Ok(()) => Ok(Finished::Remuxed),
            Err(e) if plays_as_is => place().map(|()| Finished::RemuxSkipped(e)),
            // MPEG-TS is different: what was asked for genuinely cannot be
            // produced, so the caller has to be told.
            Err(e) => Err(e),
        },
        Finish::Refuse => Err(refusal.to_string()),
    }
}

static FFMPEG_PATH: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

/// Set an explicit executable path, or restore automatic discovery with `None`.
pub fn set_ffmpeg_path(path: Option<std::path::PathBuf>) {
    if let Ok(mut selected) = FFMPEG_PATH.write() {
        *selected = path;
    }
}

/// Whether a selected FFmpeg path points to an executable file.
pub fn valid_ffmpeg_path(path: &std::path::Path) -> bool {
    executable(path)
}

/// A usable ffmpeg, or `None`.
///
/// Searching `PATH` alone is not enough on a desktop app. A macOS bundle
/// launched from Finder inherits a minimal `PATH` that does NOT include
/// `/opt/homebrew/bin` or `/usr/local/bin`, so an ffmpeg the user installed
/// with Homebrew is invisible to it while being obviously present in their
/// terminal. Windows has the same problem for a different reason: winget,
/// scoop and chocolatey all put their shim directory on `PATH` at install
/// time, and a session started before that install does not have it. The
/// well-known locations are therefore checked too, and `HYDRA_FFMPEG`
/// overrides automatic discovery for an install somewhere else. A selected
/// application path takes precedence over automatic discovery.
///
/// The answer is cached briefly rather than permanently: a UI that shows
/// "installed / not installed" would otherwise stat the filesystem on every
/// frame, or freeze the answer and never notice the user installing it.
pub fn ffmpeg() -> Option<std::path::PathBuf> {
    let selected = FFMPEG_PATH.read().ok().and_then(|path| path.clone());
    cached_ffmpeg(selected)
}

/// Find FFmpeg using the environment and standard locations, ignoring the selected path.
pub fn automatic_ffmpeg() -> Option<std::path::PathBuf> {
    cached_ffmpeg(None)
}

fn cached_ffmpeg(selected: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    type CachedPath = (
        Instant,
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
    );
    static CACHE: Mutex<Option<CachedPath>> = Mutex::new(None);
    const TTL: Duration = Duration::from_secs(5);

    if let Ok(g) = CACHE.lock() {
        if let Some((at, source, found)) = g.as_ref() {
            if source == &selected && at.elapsed() < TTL {
                return found.clone();
            }
        }
    }
    let found = match selected.as_ref() {
        Some(path) => executable(path).then(|| path.clone()),
        None => find_ffmpeg(),
    };
    if let Ok(mut g) = CACHE.lock() {
        *g = Some((Instant::now(), selected, found.clone()));
    }
    found
}

/// True when a remux or a mux can actually be performed here.
pub fn ffmpeg_available() -> bool {
    ffmpeg().is_some()
}

fn executable(p: &std::path::Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// The process environment, read through a function rather than directly.
///
/// The search is the part worth testing and the machine it runs on is the
/// part that makes that hard: a Linux CI box has no `%LOCALAPPDATA%`, and
/// mutating the real environment from a test races every other test in the
/// binary. Handing the lookup in costs one indirection and buys a search
/// that can be driven over either platform's layout from anywhere.
type Var<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

fn find_ffmpeg() -> Option<std::path::PathBuf> {
    locate(cfg!(target_os = "windows"), &|k| std::env::var_os(k))
}

/// The search itself: override, `PATH`, the well-known directories, and —
/// on Windows — winget's unpacked package tree.
fn locate(windows: bool, var: Var<'_>) -> Option<std::path::PathBuf> {
    let exe = if windows { "ffmpeg.exe" } else { "ffmpeg" };
    // An explicit override wins, and is the answer for an install in a place
    // nobody could reasonably guess.
    if let Some(p) = var("HYDRA_FFMPEG") {
        let p = std::path::PathBuf::from(p);
        if executable(&p) {
            return Some(p);
        }
        // A path that was set but is wrong should not silently fall back to
        // some other ffmpeg: that is how "why is it using the old one?"
        // happens.
        return None;
    }
    if let Some(paths) = var("PATH") {
        if let Some(p) = first_executable(std::env::split_paths(&paths), exe) {
            return Some(p);
        }
    }
    if let Some(p) = first_executable(well_known(windows, var), exe) {
        return Some(p);
    }
    if windows {
        return winget_package(var);
    }
    None
}

/// The first of `dirs` that holds a runnable `exe`.
fn first_executable(
    dirs: impl IntoIterator<Item = std::path::PathBuf>,
    exe: &str,
) -> Option<std::path::PathBuf> {
    dirs.into_iter()
        .map(|d| d.join(exe))
        .find(|p| executable(p))
}

/// The directories package managers install into, for a GUI process whose
/// `PATH` came from launchd or a desktop session rather than a shell.
///
/// The per-user ones are derived from the environment rather than spelled
/// out: `C:\Users\<name>` is not a constant, and a Windows install that has
/// not been signed out of since it happened has the shim directory on disk
/// but not yet on this process's `PATH`.
fn well_known(windows: bool, var: Var<'_>) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let env_dir = |name: &str, tail: &str| -> Option<std::path::PathBuf> {
        var(name).map(|v| std::path::PathBuf::from(v).join(tail))
    };
    if windows {
        // winget's own shim directory, then the Store alias directory both
        // it and Microsoft's packages publish through.
        dirs.extend(env_dir("LOCALAPPDATA", "Microsoft\\WinGet\\Links"));
        dirs.extend(env_dir("LOCALAPPDATA", "Microsoft\\WindowsApps"));
        // scoop (per user, and the machine-wide variant), then chocolatey.
        dirs.extend(env_dir("USERPROFILE", "scoop\\shims"));
        dirs.extend(env_dir("SCOOP", "shims"));
        dirs.extend(env_dir("ChocolateyInstall", "bin"));
        dirs.push("C:\\ProgramData\\chocolatey\\bin".into());
        // A plain unzip-it-yourself install, in the two places the guides
        // tell people to put it.
        dirs.extend(env_dir("ProgramFiles", "ffmpeg\\bin"));
        dirs.push("C:\\Program Files\\ffmpeg\\bin".into());
        dirs.push("C:\\ffmpeg\\bin".into());
    } else {
        dirs.extend([
            "/opt/homebrew/bin".into(),
            "/usr/local/bin".into(),
            "/usr/bin".into(),
            "/bin".into(),
            "/opt/local/bin".into(),
            "/snap/bin".into(),
            "/var/lib/flatpak/exports/bin".into(),
            "/home/linuxbrew/.linuxbrew/bin".into(),
        ]);
        dirs.extend(env_dir("HOME", ".local/bin"));
        dirs.extend(env_dir("HOME", "bin"));
    }
    dirs
}

/// winget installs ffmpeg as an unpacked archive rather than a shim: the
/// binary sits at
/// `%LOCALAPPDATA%\Microsoft\WinGet\Packages\Gyan.FFmpeg…\ffmpeg-<ver>-full_build\bin\ffmpeg.exe`,
/// where the version is part of the path and changes on every upgrade.
fn winget_package(var: Var<'_>) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(var("LOCALAPPDATA")?).join("Microsoft\\WinGet\\Packages");
    winget_scan(&root)
}

/// Two bounded directory reads over winget's package tree — the package
/// folder, then the versioned build folder inside it — so the version never
/// has to be guessed.
fn winget_scan(root: &std::path::Path) -> Option<std::path::PathBuf> {
    for pkg in std::fs::read_dir(root).ok()?.flatten() {
        let name = pkg.file_name();
        // Every publisher that ships ffmpeg through winget (Gyan, BtbN,
        // FFmpeg.FFmpeg) names the package after it.
        if !name.to_string_lossy().to_lowercase().contains("ffmpeg") {
            continue;
        }
        let direct = pkg.path().join("bin").join("ffmpeg.exe");
        if executable(&direct) {
            return Some(direct);
        }
        let Ok(inner) = std::fs::read_dir(pkg.path()) else {
            continue;
        };
        for sub in inner.flatten() {
            let cand = sub.path().join("bin").join("ffmpeg.exe");
            if executable(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// Remux `src` into `dst`. Stream copy only — no re-encoding, so this is
/// bounded by disk rather than CPU and never touches quality. `kind` is
/// accepted for compatibility; ffmpeg reads the source's own framing.
///
/// Pure-Rust TS→MP4 is the intended path and is not written yet; until it
/// is, a system ffmpeg does the job and its absence is reported as the
/// actionable thing it is, rather than leaving an unplayable file behind.
///
/// No `-bsf:a aac_adtstoasc`: the MP4 muxer inserts that filter itself when
/// it meets ADTS AAC, and forcing it breaks MP3 and AC-3 audio.
pub fn remux(src: &std::path::Path, dst: &std::path::Path, _kind: Segments) -> Result<(), String> {
    let Some(ff) = ffmpeg() else {
        return Err(
            "MPEG-TS to MP4 needs ffmpeg on PATH; install it, or choose the TS row instead".into(),
        );
    };
    let to_mp4 = dst
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    let mut cmd = std::process::Command::new(ff);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        cmd.creation_flags(0x0800_0000);
    }
    cmd.args(["-y", "-loglevel", "error", "-i"])
        .arg(src)
        .args(["-c", "copy"]);
    if to_mp4 {
        // Put `moov` at the front so the file is seekable immediately.
        // Meaningless (and rejected) for any other container.
        cmd.args(["-movflags", "+faststart"]);
    }
    let out = cmd
        .arg(dst)
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "ffmpeg could not remux: {}",
        why.lines().last().unwrap_or("unknown error")
    ))
}

/// Combine a video track and an audio track into one file. Stream copy, so
/// nothing is re-encoded and quality is untouched. `audio_kind` is accepted
/// for compatibility; ffmpeg reads each input's own framing and the MP4
/// muxer adds the ADTS filter itself when the audio is AAC.
///
/// DASH keeps the two apart, and so does HLS whenever a variant names an
/// `AUDIO` rendition group; putting them back together is genuinely a muxing
/// job — there is no concatenation that does it. Until the pure-Rust muxer
/// exists this needs ffmpeg, and says so plainly instead of leaving a silent
/// video behind.
pub fn mux(
    video: &std::path::Path,
    audio: &std::path::Path,
    dst: &std::path::Path,
    _audio_kind: Segments,
) -> Result<(), String> {
    let Some(ff) = ffmpeg() else {
        return Err(
            "combining separate video and audio tracks needs ffmpeg on PATH; install it to \
             get one file"
                .into(),
        );
    };
    let to_mp4 = dst
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    let mut cmd = std::process::Command::new(ff);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        cmd.creation_flags(0x0800_0000);
    }
    cmd.args(["-y", "-loglevel", "error", "-i"])
        .arg(video)
        .arg("-i")
        .arg(audio)
        .args(["-c", "copy", "-map", "0:v:0", "-map", "1:a:0"]);
    if to_mp4 {
        // Rejected outright by every other muxer, so it is not passed to one.
        cmd.args(["-movflags", "+faststart"]);
    }
    let out = cmd
        .arg(dst)
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "ffmpeg could not combine the tracks: {}",
        why.lines().last().unwrap_or("unknown error")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a1\",NAME=\"English\",URI=\"audio/en.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=6221600,AVERAGE-BANDWIDTH=5200000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
v8/index.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS=\"avc1.64001f\"\n\
v5/index.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=902000,RESOLUTION=640x360\n\
v2/index.m3u8\n";

    /// A master shaped like the ones X and every other fMP4 publisher
    /// serve: the variants carry video only, and the sound is a rendition
    /// group they point at.
    const SPLIT_AUDIO: &str = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"Deutsch\",LANGUAGE=\"de\",AUTOSELECT=YES,URI=\"a/de.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"a/en.m3u8\"\n\
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",URI=\"s/en.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,CODECS=\"avc1.64001f,mp4a.40.2\",AUDIO=\"aac\"\n\
v5/index.m3u8\n";

    #[test]
    fn audio_renditions_are_parsed_and_their_uris_resolved() {
        let pl = parse(SPLIT_AUDIO, "https://cdn.example/hls/master.m3u8");
        // Only TYPE=AUDIO is kept; the subtitle rendition is not a track.
        assert_eq!(pl.audio.len(), 2);
        let en = &pl.audio[1];
        assert_eq!(en.group, "aac");
        assert_eq!(en.name.as_deref(), Some("English"));
        assert_eq!(en.language.as_deref(), Some("en"));
        assert!(en.default && en.autoselect);
        assert_eq!(en.url.as_deref(), Some("https://cdn.example/hls/a/en.m3u8"));
        assert!(!pl.audio[0].default);
        assert_eq!(pl.variants[0].audio.as_deref(), Some("aac"));
    }

    #[test]
    fn a_variant_that_names_a_group_plays_with_its_default_rendition() {
        let pl = parse(SPLIT_AUDIO, "https://cdn.example/hls/master.m3u8");
        let audio = pl
            .audio_for(&pl.variants[0])
            .expect("a video-only variant must name its audio");
        // DEFAULT=YES wins over the rendition that merely comes first, which
        // is what a player would pick.
        assert_eq!(
            audio.url.as_deref(),
            Some("https://cdn.example/hls/a/en.m3u8")
        );
    }

    #[test]
    fn an_autoselect_rendition_is_taken_when_none_is_default() {
        let text = SPLIT_AUDIO.replace(",DEFAULT=YES", "");
        let pl = parse(&text, "https://cdn.example/hls/master.m3u8");
        let audio = pl.audio_for(&pl.variants[0]).expect("autoselect is a pick");
        assert_eq!(
            audio.url.as_deref(),
            Some("https://cdn.example/hls/a/de.m3u8")
        );
    }

    #[test]
    fn in_band_audio_is_not_fetched_as_a_second_track() {
        // A rendition with no URI means the audio is already inside the
        // variant's segments. Fetching "it" would mean fetching nothing, and
        // muxing would then be asked to find a track that is not there.
        let text = "#EXTM3U\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",DEFAULT=YES\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"Commentary\",URI=\"a/cm.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=2800000,RESOLUTION=1280x720,AUDIO=\"aac\"\n\
v5/index.m3u8\n";
        let pl = parse(text, "https://cdn.example/hls/master.m3u8");
        assert!(pl.audio_for(&pl.variants[0]).is_none());
    }

    #[test]
    fn a_variant_naming_no_group_has_no_alternate_audio() {
        // The master here lists an audio rendition, but no variant points at
        // it: the sound is muxed into the variants and taking the rendition
        // as well would double it.
        let pl = parse(MASTER, "https://cdn.example/hls/master.m3u8");
        assert!(pl.variants.iter().all(|v| pl.audio_for(v).is_none()));
    }

    fn media_vod() -> String {
        let mut s = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:6\n");
        for i in 0..3 {
            s.push_str(&format!("#EXTINF:6.0,\nseg{i}.ts\n"));
        }
        s.push_str("#EXT-X-ENDLIST\n");
        s
    }

    #[test]
    fn a_master_yields_variants_highest_first_with_urls_resolved() {
        let pl = parse(MASTER, "https://cdn.ex/hls/master.m3u8");
        assert!(pl.is_master());
        assert_eq!(pl.variants.len(), 3);
        assert_eq!(pl.variants[0].url, "https://cdn.ex/hls/v8/index.m3u8");
        // AVERAGE-BANDWIDTH is the honest number when both are given.
        assert_eq!(pl.variants[0].bandwidth, Some(5_200_000));
        assert_eq!(pl.variants[0].height, Some(1080));
        assert_eq!(
            pl.variants[0].codecs.as_deref(),
            Some("avc1.640028,mp4a.40.2")
        );
        assert_eq!(
            pl.variants
                .iter()
                .map(|v| v.height.unwrap())
                .collect::<Vec<_>>(),
            [1080, 720, 360]
        );
    }

    #[test]
    fn a_media_playlist_yields_ordered_segments_and_a_duration() {
        let pl = parse(&media_vod(), "https://cdn.ex/hls/v8/index.m3u8");
        assert!(!pl.is_master());
        assert_eq!(
            pl.segments
                .iter()
                .map(|s| s.url.as_str())
                .collect::<Vec<_>>(),
            [
                "https://cdn.ex/hls/v8/seg0.ts",
                "https://cdn.ex/hls/v8/seg1.ts",
                "https://cdn.ex/hls/v8/seg2.ts"
            ]
        );
        assert_eq!(pl.duration, 18.0);
        assert!(!pl.live);
        assert_eq!(pl.segments_kind, Some(Segments::Ts));
    }

    #[test]
    fn an_init_map_marks_the_stream_as_fragmented_mp4() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:4,\nseg0.m4s\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://cdn.ex/hls/v/idx.m3u8");
        assert_eq!(pl.segments_kind, Some(Segments::Fmp4));
        assert_eq!(
            pl.init.as_ref().map(|s| s.url.as_str()),
            Some("https://cdn.ex/hls/v/init.mp4")
        );
    }

    #[test]
    fn byte_range_segments_carve_one_file_into_many() {
        // Apple's own fMP4 examples are shaped exactly like this: one
        // main.mp4, and every segment a sub-range of it. Reading the URI
        // alone would fetch the whole file once per segment.
        let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-MAP:URI=\"main.mp4\",BYTERANGE=\"720@0\"\n\
                    #EXTINF:6.0,\n#EXT-X-BYTERANGE:540482@720\nmain.mp4\n\
                    #EXTINF:6.0,\n#EXT-X-BYTERANGE:551488\nmain.mp4\n\
                    #EXTINF:6.0,\n#EXT-X-BYTERANGE:499712\nmain.mp4\n\
                    #EXT-X-ENDLIST\n";
        let pl = parse(text, "https://cdn.ex/v3/prog_index.m3u8");

        let init = pl.init.as_ref().unwrap();
        assert_eq!(init.url, "https://cdn.ex/v3/main.mp4");
        assert_eq!(init.range, Some((0, 720)));
        assert_eq!(init.range_header().as_deref(), Some("bytes=0-719"));

        assert_eq!(pl.segments.len(), 3);
        // Every segment is the same object; only the range differs.
        assert!(pl
            .segments
            .iter()
            .all(|s| s.url == "https://cdn.ex/v3/main.mp4"));
        assert_eq!(pl.segments[0].range, Some((720, 540482)));
        // An omitted offset continues where the previous sub-range ended.
        assert_eq!(pl.segments[1].range, Some((720 + 540482, 551488)));
        assert_eq!(pl.segments[2].range, Some((720 + 540482 + 551488, 499712)));
        assert_eq!(
            pl.segments[0].range_header().as_deref(),
            Some("bytes=720-541201")
        );
    }

    #[test]
    fn a_plain_segment_has_no_range_and_asks_for_no_header() {
        let pl = parse(&media_vod(), "https://cdn.ex/hls/v8/index.m3u8");
        assert!(pl.segments.iter().all(|s| s.range.is_none()));
        assert_eq!(pl.segments[0].range_header(), None);
    }

    #[test]
    fn a_zero_length_range_asks_for_no_header() {
        // A malformed playlist should not produce `bytes=0-18446744073709551615`.
        let s = Segment {
            url: "https://e/x".into(),
            range: Some((0, 0)),
            key: None,
        };
        assert_eq!(s.range_header(), None);
    }

    #[test]
    fn a_growing_playlist_is_live_and_refused_by_the_vod_path() {
        let pl = parse("#EXTM3U\n#EXTINF:6,\ns.ts\n", "https://e/x.m3u8");
        assert!(pl.live);
        assert_eq!(Plan::build(&pl, None), Err(Refusal::Live));
    }

    #[test]
    fn drm_is_named_and_refused_without_requesting_a_key() {
        for (keyformat, want) in [
            ("urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed", "Widevine"),
            ("com.apple.streamingkeydelivery", "FairPlay"),
            ("urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95", "PlayReady"),
        ] {
            let text = format!(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                 #EXT-X-KEY:METHOD=SAMPLE-AES,KEYFORMAT=\"{keyformat}\",URI=\"skd://x\"\n\
                 #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
            );
            let pl = parse(&text, "https://e/x.m3u8");
            assert_eq!(pl.drm.as_deref(), Some(want));
            assert_eq!(Plan::build(&pl, None), Err(Refusal::Drm(want.into())));
        }
    }

    #[test]
    fn aes_128_is_downloadable_with_its_key_and_refused_without() {
        // Plain content protection, not DRM: the key is an ordinary URL
        // fetched over the same session. What must never happen is writing
        // the ciphertext and calling it a download.
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:7\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k.bin\"\n\
                    #EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/v/x.m3u8");
        assert_eq!(pl.drm, None);
        assert_eq!(pl.encryption.as_deref(), Some("AES-128"));

        let plan = Plan::build(&pl, None).expect("AES-128 is not a refusal");
        assert!(plan.is_encrypted());
        // One key, resolved against the playlist, listed once however many
        // segments use it.
        assert_eq!(plan.key_uris(), ["https://e/v/k.bin"]);
        // With no explicit IV the media sequence number IS the IV, so the
        // two segments differ: 7 and 8.
        let ivs: Vec<[u8; 16]> = plan
            .segments
            .iter()
            .map(|s| s.key.as_ref().unwrap().iv)
            .collect();
        assert_eq!(ivs[0][8..], 7u64.to_be_bytes());
        assert_eq!(ivs[1][8..], 8u64.to_be_bytes());
    }

    #[test]
    fn an_explicit_iv_beats_the_sequence_number() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:3\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x000102030405060708090A0B0C0D0E0F\n\
                    #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/x.m3u8");
        assert_eq!(
            pl.segments[0].key.as_ref().unwrap().iv,
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        // A malformed IV is refused, NOT quietly replaced with the sequence
        // number. The spec's sequence rule is for an ABSENT IV; guessing at a
        // present-but-broken one corrupts the first block of every segment in
        // a way no padding check can catch, so the download would report
        // success over damaged output. And it is its own refusal: nothing
        // about it is DRM.
        let bad = text.replace("0x000102030405060708090A0B0C0D0E0F", "0xnothex");
        let pl = parse(&bad, "https://e/x.m3u8");
        assert!(pl.segments[0].key.is_none());
        assert_eq!(pl.drm, None);
        assert_eq!(Plan::build(&pl, None), Err(Refusal::UnreadableIv));
        assert!(!Refusal::UnreadableIv.to_string().contains("protected"));

        // An IV of the wrong length is just as broken as one that is not hex.
        for wrong in ["0x0011", "0x000102030405060708090A0B0C0D0E0F00"] {
            let pl = parse(
                &text.replace("0x000102030405060708090A0B0C0D0E0F", wrong),
                "https://e/x.m3u8",
            );
            assert!(pl.segments[0].key.is_none(), "accepted IV {wrong}");
        }
    }

    #[test]
    fn a_rotated_key_applies_only_to_the_segments_after_it() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k1\"\n\
                    #EXTINF:4,\na.ts\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k2\"\n\
                    #EXTINF:4,\nb.ts\n#EXT-X-ENDLIST\n";
        let plan = Plan::build(&parse(text, "https://e/x.m3u8"), None).unwrap();
        assert_eq!(plan.segments[0].key.as_ref().unwrap().uri, "https://e/k1");
        assert_eq!(plan.segments[1].key.as_ref().unwrap().uri, "https://e/k2");
        assert_eq!(plan.key_uris(), ["https://e/k1", "https://e/k2"]);
    }

    #[test]
    fn method_none_clears_a_previous_key() {
        // An encrypted stretch followed by a clear one is legal, and the
        // clear segments must not inherit the key.
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k1\"\n\
                    #EXTINF:4,\na.ts\n\
                    #EXT-X-KEY:METHOD=NONE\n\
                    #EXTINF:4,\nb.ts\n#EXT-X-ENDLIST\n";
        let plan = Plan::build(&parse(text, "https://e/x.m3u8"), None).unwrap();
        assert!(plan.segments[0].key.is_some());
        assert!(
            plan.segments[1].key.is_none(),
            "METHOD=NONE did not clear the key; a clear segment would be \"decrypted\""
        );
        // Only the encrypted stretch needs a key fetched.
        assert_eq!(plan.key_uris(), ["https://e/k1"]);
    }

    #[test]
    fn aes_128_round_trips_through_the_decryptor() {
        use aes::cipher::{block_padding::Pkcs7, BlockModeEncrypt, KeyIvInit};
        let dir = scratch("aes");
        let path = dir.join("seg").to_string_lossy().into_owned();
        // Generated per run, not baked into the source: a literal here is
        // key material committed to the repository, however short its life.
        let key = test_key();
        let iv = iv_from_sequence(42);
        let plain = b"the quick brown fox jumps over the lazy dog, twice over".to_vec();

        let mut buf = vec![0u8; plain.len() + 16];
        buf[..plain.len()].copy_from_slice(&plain);
        let ct_len = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();
        buf.truncate(ct_len);
        std::fs::write(&path, &buf).unwrap();

        let n = decrypt_in_place(&path, &key, &iv).unwrap();
        assert_eq!(n as usize, plain.len());
        assert_eq!(std::fs::read(&path).unwrap(), plain);

        // The wrong key does not quietly produce noise: PKCS#7 will not
        // validate, which is the difference between a failed download and a
        // corrupt one.
        std::fs::write(&path, &buf).unwrap();
        let wrong = decrypt_in_place(&path, &key.map(|b| !b), &iv);
        assert!(wrong.is_err() || std::fs::read(&path).unwrap() != plain);

        // Neither does a truncated segment.
        std::fs::write(&path, &buf[..buf.len() - 3]).unwrap();
        assert!(decrypt_in_place(&path, &key, &iv).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn assembly_refuses_to_start_without_the_keys_it_needs() {
        // Better to refuse before a byte is fetched than to discover it
        // segment-by-segment and leave a part-written file behind.
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n\
                    #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        let plan = Plan::build(&parse(text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("nokey");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let fetch = move |_: Segment, _: String, _: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move { panic!("must not fetch without a key") })
        };
        let err = fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("no AES-128 key"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn method_none_is_not_encryption() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-KEY:METHOD=NONE\n\
                    #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/x.m3u8");
        assert_eq!((pl.drm, pl.encryption), (None, None));
    }

    #[test]
    fn choosing_prefers_the_exact_variant_then_the_requested_height() {
        let pl = parse(MASTER, "https://cdn.ex/hls/master.m3u8");
        let v = &pl.variants;
        assert_eq!(
            choose(v, Some("https://cdn.ex/hls/v5/index.m3u8"), None)
                .unwrap()
                .height,
            Some(720)
        );
        assert_eq!(choose(v, None, Some(360)).unwrap().height, Some(360));
        // Nothing at 900: step DOWN, never silently up.
        assert_eq!(choose(v, None, Some(900)).unwrap().height, Some(720));
        // Below everything on offer: the smallest.
        assert_eq!(choose(v, None, Some(144)).unwrap().height, Some(360));
        // No preference at all: the best.
        assert_eq!(choose(v, None, None).unwrap().height, Some(1080));
    }

    #[test]
    fn a_plan_estimates_its_size_from_bitrate_and_duration() {
        let pl = parse(&media_vod(), "https://cdn.ex/hls/v8/index.m3u8");
        let plan = Plan::build(&pl, Some(1_000_000)).unwrap();
        assert_eq!(plan.segments.len(), 3);
        assert_eq!(plan.native_ext(), "ts");
        assert_eq!(plan.estimated_size, Some(2_250_000)); // 1 Mbps x 18 s / 8
    }

    #[test]
    fn finishing_uses_ffmpeg_for_mp4_whenever_it_is_available() {
        use Finish::*;
        // With ffmpeg, MP4 always goes through it: faststart and a real
        // (non-fragmented) MP4 are worth the copy.
        assert_eq!(plan_finish(Segments::Ts, "mp4", true), Remux);
        assert_eq!(plan_finish(Segments::Fmp4, "mp4", true), Remux);
        // Without it, fragmented MP4 still plays as assembled...
        assert_eq!(plan_finish(Segments::Fmp4, "mp4", false), Move);
        // ...but MPEG-TS has no honest way to become MP4.
        assert_eq!(plan_finish(Segments::Ts, "mp4", false), Refuse);
        // TS asked for as TS is a rename either way.
        assert_eq!(plan_finish(Segments::Ts, "ts", true), Move);
        assert_eq!(plan_finish(Segments::Ts, "ts", false), Move);
    }

    #[test]
    fn fragmented_mp4_is_never_renamed_into_a_ts_file() {
        use Finish::*;
        // Renaming fMP4 to .ts produces a file that lies about what it is.
        // ffmpeg can genuinely write MPEG-TS; nothing else can.
        assert_eq!(plan_finish(Segments::Fmp4, "ts", true), Remux);
        assert_eq!(plan_finish(Segments::Fmp4, "ts", false), Refuse);
    }

    #[test]
    fn attribute_lists_survive_quoted_commas() {
        let a = attrs("BANDWIDTH=100,CODECS=\"avc1.4d401f,mp4a.40.2\",RESOLUTION=1920x1080");
        assert_eq!(attr(&a, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(attr(&a, "BANDWIDTH"), Some("100"));
        assert_eq!(attr(&a, "RESOLUTION"), Some("1920x1080"));
    }

    /// A fetcher that writes `body` to `dest` a byte at a time, so the
    /// counter really does move while the segment is arriving.
    fn slow_fetcher(per_byte: u64) -> impl Fetcher {
        move |seg: Segment, dest: String, counter: Arc<AtomicU64>| {
            Box::pin(async move {
                use std::io::Write;
                let n: u64 = seg
                    .url
                    .rsplit("seg")
                    .next()
                    .and_then(|s| s.split('.').next())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let body = format!("[{n}]").into_bytes();
                let mut f = std::fs::File::create(&dest)?;
                for b in &body {
                    tokio::time::sleep(std::time::Duration::from_millis(per_byte)).await;
                    f.write_all(&[*b])?;
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                f.flush()?;
                Ok(body.len() as u64)
            })
        }
    }

    /// The concurrency the assembly tests drive at, so "one window's worth"
    /// means something definite in them.
    const CONC: usize = 6;

    /// A fresh AES-128 key for one test run. Seeded from the process's
    /// `RandomState`, so the bytes exist only while the test does. The bytes
    /// are assembled from the hasher's output rather than written into a
    /// zeroed array, which static analysis reads as a hard-coded key.
    fn test_key() -> [u8; 16] {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let word = |i: usize| {
            let mut h = RandomState::new().build_hasher();
            h.write_usize(i);
            u128::from(h.finish())
        };
        ((word(0) << 64) | word(1)).to_ne_bytes()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hydra-hls-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// A scratch directory with nothing left in it from an earlier run: the
    /// ffmpeg searches assert on what they find, and a stale fixture is
    /// indistinguishable from a fresh one.
    fn fresh(name: &str) -> std::path::PathBuf {
        let d = scratch(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A file the search will accept as runnable, parents and all.
    fn fake_exe(path: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path.to_path_buf()
    }

    /// An environment of exactly these variables, in place of the process's.
    fn env_of(
        pairs: Vec<(&'static str, std::ffi::OsString)>,
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |want: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == want)
                .map(|(_, value)| value.clone())
        }
    }

    /// `PATH` as the OS spells it, from directories rather than a string
    /// with a separator this test would have to guess.
    fn path_var(dirs: &[&std::path::Path]) -> std::ffi::OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn an_override_beats_whatever_is_on_path() {
        let dir = fresh("ff-override");
        let on_path = fake_exe(&dir.join("path-copy").join("ffmpeg"));
        let chosen = fake_exe(&dir.join("elsewhere").join("ffmpeg"));
        let var = env_of(vec![
            ("HYDRA_FFMPEG", chosen.clone().into_os_string()),
            ("PATH", path_var(&[on_path.parent().unwrap()])),
        ]);
        assert_eq!(locate(false, &var), Some(chosen));
    }

    /// A wrong override is a mistake to be seen, not routed around: falling
    /// back to another ffmpeg is how "why is it still using the old one?"
    /// happens.
    #[test]
    fn an_override_that_points_at_nothing_finds_nothing() {
        let dir = fresh("ff-bad-override");
        let on_path = fake_exe(&dir.join("path-copy").join("ffmpeg"));
        let var = env_of(vec![
            (
                "HYDRA_FFMPEG",
                dir.join("gone").join("ffmpeg").into_os_string(),
            ),
            ("PATH", path_var(&[on_path.parent().unwrap()])),
        ]);
        assert_eq!(locate(false, &var), None);
    }

    #[test]
    fn path_is_searched_in_its_own_order() {
        let dir = fresh("ff-path");
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let wanted = fake_exe(&dir.join("second").join("ffmpeg"));
        let var = env_of(vec![(
            "PATH",
            path_var(&[&empty, wanted.parent().unwrap()]),
        )]);
        assert_eq!(locate(false, &var), Some(wanted));
    }

    /// The macOS case the well-known list exists for: an app bundle opened
    /// from Finder inherits a `PATH` that holds none of the places a package
    /// manager installs into.
    ///
    /// Asserted over the home-derived entries alone. The rest of the list is
    /// `/usr/local/bin` and friends, and a developer machine with a real
    /// ffmpeg in one of them would otherwise decide the answer.
    #[test]
    fn a_home_install_is_found_with_nothing_useful_on_path() {
        let dir = fresh("ff-home");
        let wanted = fake_exe(&dir.join("home").join(".local/bin").join("ffmpeg"));
        let var = env_of(vec![("HOME", dir.join("home").into_os_string())]);
        let from_home: Vec<_> = well_known(false, &var)
            .into_iter()
            .filter(|d| d.starts_with(&dir))
            .collect();
        assert_eq!(from_home.len(), 2, "both ~/.local/bin and ~/bin");
        assert_eq!(first_executable(from_home, "ffmpeg"), Some(wanted));
    }

    /// The Windows counterpart, and the reason for it: winget puts its shim
    /// directory on `PATH` at install time, so a session started before the
    /// install has the directory on disk but not in its environment.
    #[test]
    fn the_winget_shim_directory_is_searched_when_path_has_not_caught_up() {
        let dir = fresh("ff-winget-shim");
        let local = dir.join("local");
        let wanted = fake_exe(&local.join("Microsoft\\WinGet\\Links").join("ffmpeg.exe"));
        // The unpacked package is there too: the shim is the cheaper answer
        // and has to be the one that wins.
        fake_exe(
            &local
                .join("Microsoft\\WinGet\\Packages")
                .join("Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe")
                .join("ffmpeg-7.1-full_build")
                .join("bin")
                .join("ffmpeg.exe"),
        );
        let var = env_of(vec![("LOCALAPPDATA", local.into_os_string())]);
        assert_eq!(locate(true, &var), Some(wanted));
    }

    /// The path from the bug report: winget unpacks an archive rather than
    /// installing a shim, and the version is part of the directory name.
    #[test]
    fn a_versioned_winget_build_folder_is_found_without_knowing_the_version() {
        let dir = fresh("ff-winget-pkg");
        let local = dir.join("local");
        let packages = local.join("Microsoft\\WinGet\\Packages");
        // A package that is not ffmpeg, and happens to ship a binary by that
        // name, must not be mistaken for one.
        fake_exe(
            &packages
                .join("Some.Vendor_Microsoft.Winget.Source_8wekyb3d8bbwe")
                .join("bin")
                .join("ffmpeg.exe"),
        );
        let wanted = fake_exe(
            &packages
                .join("Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe")
                .join("ffmpeg-7.1-full_build")
                .join("bin")
                .join("ffmpeg.exe"),
        );
        // `winget_package` rather than `locate`: the fixed `C:\...` entries
        // ahead of it in the search are real directories on a Windows runner
        // and would decide this test's answer instead of the fixture.
        let var = env_of(vec![("LOCALAPPDATA", local.into_os_string())]);
        assert_eq!(winget_package(&var), Some(wanted));
    }

    /// Some publishers unpack straight into `bin`, with no version folder.
    #[test]
    fn a_winget_package_without_a_version_folder_is_found_too() {
        let dir = fresh("ff-winget-flat");
        let packages = dir.join("Packages");
        let wanted = fake_exe(
            &packages
                .join("BtbN.FFmpeg.GPL.Shared")
                .join("bin")
                .join("ffmpeg.exe"),
        );
        assert_eq!(winget_scan(&packages), Some(wanted));
    }

    #[test]
    fn an_empty_package_tree_is_no_answer_at_all() {
        let dir = fresh("ff-winget-empty");
        assert_eq!(winget_scan(&dir), None);
        // Nor is a tree that is not there.
        assert_eq!(winget_scan(&dir.join("absent")), None);
    }

    #[test]
    fn the_well_known_list_follows_the_platform_it_is_asked_about() {
        let var = env_of(vec![
            ("LOCALAPPDATA", "L".into()),
            ("USERPROFILE", "U".into()),
            ("ProgramFiles", "P".into()),
            ("HOME", "/home/me".into()),
        ]);
        let win = well_known(true, &var);
        assert!(win.contains(&std::path::PathBuf::from("L").join("Microsoft\\WinGet\\Links")));
        assert!(win.contains(&std::path::PathBuf::from("U").join("scoop\\shims")));
        assert!(win.contains(&std::path::PathBuf::from("P").join("ffmpeg\\bin")));
        assert!(win.contains(&std::path::PathBuf::from(
            "C:\\ProgramData\\chocolatey\\bin"
        )));
        // Unset variables drop out rather than contributing a relative path
        // that would be searched against the working directory.
        assert!(!win.iter().any(|d| d.starts_with("shims")));

        let unix = well_known(false, &var);
        assert!(unix.contains(&std::path::PathBuf::from("/opt/homebrew/bin")));
        assert!(unix.contains(&std::path::PathBuf::from("/home/me/.local/bin")));
        assert!(!unix.iter().any(|d| d.to_string_lossy().contains("scoop")));
    }

    /// The winget tree is the last resort, reached only once `PATH` and the
    /// shim directories have all come up empty.
    ///
    /// Driven over the Windows layout from a machine that is not Windows —
    /// which is the whole point of the seam, and also keeps a real Windows
    /// host's own `C:\ffmpeg\bin` out of the answer.
    #[cfg(not(windows))]
    #[test]
    fn the_package_tree_is_reached_when_no_directory_holds_a_shim() {
        let dir = fresh("ff-winget-fallback");
        let local = dir.join("local");
        let wanted = fake_exe(
            &local
                .join("Microsoft\\WinGet\\Packages")
                .join("Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe")
                .join("ffmpeg-7.1-full_build")
                .join("bin")
                .join("ffmpeg.exe"),
        );
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let var = env_of(vec![
            ("PATH", path_var(&[&empty])),
            ("LOCALAPPDATA", local.into_os_string()),
        ]);
        assert_eq!(locate(true, &var), Some(wanted));
    }

    /// Whatever this machine has, it has to keep saying the same thing: the
    /// UI asks on every frame and the cache is what stops that being a stat
    /// per frame.
    #[test]
    fn the_lookup_is_cached_and_only_ever_names_a_runnable_file() {
        let first = ffmpeg();
        assert_eq!(first, ffmpeg(), "the cached answer changed under us");
        assert_eq!(first.is_some(), ffmpeg_available());
        if let Some(path) = first {
            assert!(executable(&path), "named an unrunnable {}", path.display());
        }
    }

    #[test]
    fn only_a_runnable_file_counts_as_an_executable() {
        let dir = fresh("ff-executable");
        assert!(executable(&fake_exe(&dir.join("ffmpeg"))));
        // A directory of the right name is not a program.
        std::fs::create_dir_all(dir.join("bin").join("ffmpeg")).unwrap();
        assert!(!executable(&dir.join("bin").join("ffmpeg")));
        assert!(!executable(&dir.join("absent")));
        #[cfg(unix)]
        {
            let readme = dir.join("ffmpeg.txt");
            std::fs::write(&readme, b"not a program").unwrap();
            assert!(!executable(&readme));
        }
    }

    #[tokio::test]
    async fn segments_are_written_in_playlist_order_however_they_arrive() {
        let pl = parse(&media_vod(), "https://cdn.ex/hls/v8/index.m3u8");
        let plan = Plan::build(&pl, None).unwrap();
        let dir = scratch("order");
        let staging = dir.join("out.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        // Later segments answer FASTER, so anything that wrote on completion
        // would interleave them.
        let fetch = move |seg: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move {
                use std::io::Write;
                let n: u64 = seg
                    .url
                    .rsplit("seg")
                    .next()
                    .and_then(|s| s.split('.').next())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                tokio::time::sleep(std::time::Duration::from_millis(90 - n * 30)).await;
                let body = format!("[{n}]").into_bytes();
                std::fs::File::create(&dest)?.write_all(&body)?;
                counter.store(body.len() as u64, Ordering::Relaxed);
                Ok(body.len() as u64)
            })
        };
        let meter = Arc::new(Meter::default());
        let total = fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &meter,
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap();
        drop(f);
        assert_eq!(std::fs::read_to_string(&staging).unwrap(), "[0][1][2]");
        assert_eq!(total, 9);
        let (bytes, done, live) = meter.snapshot();
        assert_eq!((bytes, done), (9, 3));
        assert!(live.is_empty(), "nothing should still be in flight");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_length_budget_keeps_the_segment_that_crosses_the_mark() {
        // 30 s asked for, 10 s segments: 30 exactly, not 20.
        let mut w = vec![10.0f64; 6];
        let mut spent = 0.0;
        trim_to_budget(&mut w, &mut spent, 30.0, |s| *s);
        assert_eq!(w.len(), 3);
        assert_eq!(spent, 30.0);

        // 25 s asked for: the third segment crosses it and is kept, because
        // stopping short would hand back less media than was asked for.
        let mut w = vec![10.0f64; 6];
        let mut spent = 0.0;
        trim_to_budget(&mut w, &mut spent, 25.0, |s| *s);
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn a_length_budget_carries_across_windows() {
        // The bug this guards: a budget reset per refresh never ends, and a
        // window checked only as a whole overruns by everything it holds.
        let mut spent = 0.0;
        let mut first = vec![4.0f64; 3];
        trim_to_budget(&mut first, &mut spent, 10.0, |s| *s);
        assert_eq!(first.len(), 3, "12 s: the third crosses 10 and is kept");

        // Budget already spent, so the next refresh takes nothing at all.
        let mut second = vec![4.0f64; 3];
        trim_to_budget(&mut second, &mut spent, 10.0, |s| *s);
        assert!(second.is_empty(), "the budget does not refill: {second:?}");
    }

    /// A DVR window can hold hours. This is the case that made the rule
    /// necessary: one check per window would have taken all of it.
    #[test]
    fn a_huge_backlog_is_cut_to_the_ask() {
        let mut w = vec![6.0f64; 3000]; // five hours
        let mut spent = 0.0;
        trim_to_budget(&mut w, &mut spent, 60.0, |s| *s);
        assert_eq!(w.len(), 10, "60 s of 6 s segments");
    }

    /// The connection table is the user's only window onto concurrency, so
    /// it has to mean what it appears to mean: a fixed row per connection,
    /// each accumulating over every segment it carries.
    #[tokio::test]
    async fn the_connection_table_holds_one_stable_row_per_connection() {
        let mut m = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:2\n");
        for i in 0..12 {
            m.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        m.push_str("#EXT-X-ENDLIST\n");
        let pl = parse(&m, "https://cdn.ex/hls/index.m3u8");
        let plan = Plan::build(&pl, None).unwrap();
        let dir = scratch("lanes");
        let staging = dir.join("out.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        let fetch = move |_seg: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move {
                use std::io::Write;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let body = b"0123456789";
                std::fs::File::create(&dest)?.write_all(body)?;
                counter.store(body.len() as u64, Ordering::Relaxed);
                Ok(body.len() as u64)
            })
        };
        let meter = Arc::new(Meter::default());
        let total = fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &meter,
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::fixed(4),
            &Keys::new(),
        )
        .await
        .unwrap();
        drop(f);
        assert_eq!(total, 120);

        let lanes = meter.lanes();
        // Four, because four is what was asked for. The queue runs three
        // times deeper than the gate admits, and sizing the table from that
        // instead showed twelve rows for four connections, most of them
        // sitting at "Connecting..." forever.
        assert_eq!(lanes.len(), 4, "one row per connection: {lanes:?}");
        // Twelve segments over four lanes, and the bytes are ALL there —
        // which is the property a per-segment row cannot have, because it
        // resets every time a segment settles.
        assert_eq!(lanes.iter().map(|l| l.segments).sum::<u64>(), 12);
        assert_eq!(lanes.iter().map(|l| l.bytes).sum::<u64>(), 120);
        assert!(
            lanes.iter().all(|l| l.segments >= 1),
            "every connection should have been put to work: {lanes:?}"
        );
        assert!(
            lanes.iter().any(|l| l.segments > 1),
            "a lane must carry successive segments, not one each: {lanes:?}"
        );
        assert!(
            lanes.iter().all(|l| l.state == LaneState::Completed),
            "a finished transfer leaves its outcome on every row: {lanes:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn one_slow_segment_does_not_stall_the_requests_behind_it() {
        // Segment 2 takes ten times as long as the rest. The output still has
        // to be written in order, so its APPEND must wait — but the requests
        // for 3, 4, 5... must not, which is exactly what a batched window
        // gets wrong.
        let n = 12usize;
        let mut text = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for i in 0..n {
            text.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("pipeline");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        let started = Arc::new(AtomicU64::new(0));
        let live = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        // How many segments had been requested by the time the slow one
        // finished. A batched window can only have started its own group.
        let at_unblock = Arc::new(AtomicU64::new(0));
        let fetch = {
            let (started, live, peak, at_unblock) = (
                started.clone(),
                live.clone(),
                peak.clone(),
                at_unblock.clone(),
            );
            move |seg: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
                let (started, live, peak, at_unblock) = (
                    started.clone(),
                    live.clone(),
                    peak.clone(),
                    at_unblock.clone(),
                );
                Box::pin(async move {
                    use std::io::Write;
                    let slow = seg.url.ends_with("seg2.ts");
                    started.fetch_add(1, Ordering::SeqCst);
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(if slow {
                        400
                    } else {
                        40
                    }))
                    .await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    if slow {
                        at_unblock.store(started.load(Ordering::SeqCst), Ordering::SeqCst);
                    }
                    std::fs::File::create(&dest)?.write_all(b"xx")?;
                    counter.store(2, Ordering::Relaxed);
                    Ok(2)
                })
            }
        };
        fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::fixed(3),
            &Keys::new(),
        )
        .await
        .unwrap();

        assert_eq!(peak.load(Ordering::SeqCst), 3, "the pipeline never filled");
        // Batched at width 3 could only have requested segments 0..2 while
        // segment 2 was outstanding.
        assert!(
            at_unblock.load(Ordering::SeqCst) >= 6,
            "requests stalled behind one slow segment: only {} had started",
            at_unblock.load(Ordering::SeqCst)
        );
        drop(f);
        // ...and the file is still in playlist order and complete.
        assert_eq!(std::fs::read(&staging).unwrap().len(), n * 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Build a plan of `n` segments and run it through a fetcher whose
    /// latency is a function of how many requests are live, returning the
    /// highest concurrency the pipeline reached.
    async fn peak_level(n: usize, conc: Concurrency, saturated: bool, name: &str) -> u64 {
        let mut text = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for i in 0..n {
            text.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch(name);
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        let live = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let fetch = {
            let (live, peak) = (live.clone(), peak.clone());
            move |_: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
                let (live, peak) = (live.clone(), peak.clone());
                Box::pin(async move {
                    use std::io::Write;
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    // Saturated: each extra request slows every request by
                    // the same factor, so total goodput never improves.
                    // Otherwise the path has headroom and requests are
                    // independent.
                    let ms = if saturated { 12 * now } else { 12 };
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    let body = vec![b'x'; 8192];
                    std::fs::File::create(&dest)?.write_all(&body)?;
                    counter.store(body.len() as u64, Ordering::Relaxed);
                    Ok(body.len() as u64)
                })
            }
        };
        fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            conc,
            &Keys::new(),
        )
        .await
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        peak.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn a_fixed_setting_opens_the_ceiling_immediately() {
        // Not adaptive: the caller said how many, so no ramp and no probing.
        let peak = peak_level(40, Concurrency::fixed(6), false, "adapt-fixed").await;
        assert_eq!(peak, 6);
    }

    #[tokio::test]
    async fn concurrency_is_bounded_by_what_the_caller_asked_for() {
        let mut text = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for i in 0..20 {
            text.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("bounded");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let live = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let fetch = {
            let (live, peak) = (live.clone(), peak.clone());
            move |_: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
                let (live, peak) = (live.clone(), peak.clone());
                Box::pin(async move {
                    use std::io::Write;
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    live.fetch_sub(1, Ordering::SeqCst);
                    std::fs::File::create(&dest)?.write_all(b"x")?;
                    counter.store(1, Ordering::Relaxed);
                    Ok(1)
                })
            }
        };
        fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::fixed(4),
            &Keys::new(),
        )
        .await
        .unwrap();
        // An origin asked for 4 must never see 5.
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_window_of_segments_is_fetched_concurrently_not_one_after_another() {
        // Six segments, each taking three 40 ms byte-writes. Sequential would
        // be 6 x 120 ms; one window in parallel is about 120 ms.
        let mut text = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for i in 0..CONC {
            text.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("conc");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        let t0 = std::time::Instant::now();
        fetch_all(
            &plan,
            &mut f,
            &staging,
            slow_fetcher(40),
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap();
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(400),
            "segments ran sequentially: {elapsed:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn progress_moves_while_a_segment_is_still_arriving() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2.0,\nseg0.ts\n#EXT-X-ENDLIST\n";
        let plan = Plan::build(&parse(text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("live");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let meter = Arc::new(Meter::default());

        let watcher = {
            let meter = meter.clone();
            tokio::spawn(async move {
                let mut seen_partial = false;
                for _ in 0..40 {
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                    let (bytes, done, live) = meter.snapshot();
                    // Bytes counted before the segment has been settled is
                    // exactly what the rate needs and what was missing.
                    if done == 0 && bytes > 0 && !live.is_empty() {
                        seen_partial = true;
                    }
                }
                seen_partial
            })
        };
        fetch_all(
            &plan,
            &mut f,
            &staging,
            slow_fetcher(30),
            &meter,
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap();
        assert!(
            watcher.await.unwrap(),
            "the meter never moved mid-segment; the rate would collapse between segments"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_segment_is_abandoned_rather_than_held_forever() {
        // A hung origin never errors, so without a timeout the retry
        // machinery never gets a chance to run and the slot is held until
        // the OS gives up on the socket.
        let plan = Plan::build(
            &parse(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n",
                "https://e/x.m3u8",
            ),
            None,
        )
        .unwrap();
        let dir = scratch("stall");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let fetch = move |_: Segment, _: String, _: Arc<AtomicU64>| -> FetchSeg {
            // Never resolves, never errors.
            Box::pin(async move {
                std::future::pending::<()>().await;
                unreachable!()
            })
        };
        let err = fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_flaky_segment_is_retried_before_the_transfer_fails() {
        use std::sync::atomic::AtomicUsize;
        let plan = Plan::build(
            &parse(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n",
                "https://e/x.m3u8",
            ),
            None,
        )
        .unwrap();
        let dir = scratch("retry");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();

        static TRIES: AtomicUsize = AtomicUsize::new(0);
        TRIES.store(0, Ordering::Relaxed);
        let fetch = move |_: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move {
                use std::io::Write;
                if TRIES.fetch_add(1, Ordering::Relaxed) < 2 {
                    // A failed attempt leaves a partial file behind; the next
                    // one must not splice onto it.
                    std::fs::File::create(&dest)?.write_all(b"partial")?;
                    counter.store(7, Ordering::Relaxed);
                    return Err(std::io::Error::other("connection reset"));
                }
                std::fs::File::create(&dest)?.write_all(b"ok")?;
                counter.store(2, Ordering::Relaxed);
                Ok(2)
            })
        };
        let total = fetch_all(
            &plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap();
        drop(f);
        assert_eq!(TRIES.load(Ordering::Relaxed), 3);
        assert_eq!(total, 2);
        assert_eq!(std::fs::read_to_string(&staging).unwrap(), "ok");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_cancelled_assembly_resumes_from_its_checkpoint() {
        use std::io::Write;
        // Two windows' worth, so the first can finish and be recorded before
        // the second is interrupted.
        let mut text = String::from("#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n");
        for i in 0..(CONC + 2) {
            text.push_str(&format!("#EXTINF:2.0,\nseg{i}.ts\n"));
        }
        text.push_str("#EXT-X-ENDLIST\n");
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("resume");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let ckpt = format!("{staging}.ck");

        let body = |url: &str| -> Vec<u8> {
            let n: u64 = url
                .rsplit("seg")
                .next()
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            format!("<{n}>").into_bytes()
        };
        // Records which segments each run actually asked for.
        let asked: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let make_fetch = |cancel_after: usize, asked: Arc<std::sync::Mutex<Vec<String>>>| {
            move |seg: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
                let asked = asked.clone();
                Box::pin(async move {
                    let n = {
                        let mut g = asked.lock().unwrap();
                        g.push(seg.url.clone());
                        g.len()
                    };
                    if n > cancel_after {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "cancelled",
                        ));
                    }
                    let b = body(&seg.url);
                    std::fs::File::create(&dest)?.write_all(&b)?;
                    counter.store(b.len() as u64, Ordering::Relaxed);
                    Ok(b.len() as u64)
                })
            }
        };

        // First run: the whole first window lands, the second is cut off.
        let mut f = std::fs::File::create(&staging).unwrap();
        let err = fetch_all(
            &plan,
            &mut f,
            &staging,
            make_fetch(CONC, asked.clone()),
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume {
                skip: 0,
                bytes: 0,
                checkpoint: Some(&ckpt),
            },
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap_err();
        drop(f);
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);

        let saved = Checkpoint::read(&ckpt);
        assert_eq!(
            saved.segments, CONC as u64,
            "the finished window was not recorded"
        );
        assert!(
            saved.usable(&staging, &plan),
            "the checkpoint does not describe the file on disk"
        );

        // Second run: carries on, and must not refetch what it already has.
        asked.lock().unwrap().clear();
        let meter = Arc::new(Meter::default());
        meter.preload(saved.bytes, saved.segments);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&staging)
            .unwrap();
        fetch_all(
            &plan,
            &mut f,
            &staging,
            make_fetch(usize::MAX, asked.clone()),
            &meter,
            &Arc::new(AtomicBool::new(false)),
            Resume {
                skip: saved.segments as usize,
                bytes: saved.bytes,
                checkpoint: Some(&ckpt),
            },
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap();
        drop(f);

        let second = asked.lock().unwrap().clone();
        assert_eq!(
            second.len(),
            2,
            "resumed run refetched settled segments: {second:?}"
        );
        assert!(second[0].ends_with(&format!("seg{}.ts", CONC)));

        // The assembled file is byte-identical to a clean run.
        let want: String = (0..CONC + 2).map(|n| format!("<{n}>")).collect();
        assert_eq!(std::fs::read_to_string(&staging).unwrap(), want);
        assert_eq!(Checkpoint::read(&ckpt).segments, (CONC + 2) as u64);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_checkpoint_is_refused_when_it_does_not_describe_the_file() {
        let dir = scratch("ckpt");
        let part = dir.join("o.part").to_string_lossy().into_owned();
        std::fs::write(&part, b"12345").unwrap();
        let plan = Plan::build(&parse(&media_vod(), "https://e/v/x.m3u8"), None).unwrap();
        let id = plan.id().to_string();
        let ck = |segments, bytes, plan_id: &str| Checkpoint {
            segments,
            bytes,
            plan_id: plan_id.into(),
        };

        // Right length, right plan: usable.
        assert!(ck(2, 5, &id).usable(&part, &plan));
        // Wrong length: the file is not the one recorded, so start over.
        assert!(!ck(2, 9, &id).usable(&part, &plan));
        // More segments than the plan has: a different playlist.
        assert!(!ck(20, 5, &id).usable(&part, &plan));
        // Nothing recorded is not a resume.
        assert!(!ck(0, 0, &id).usable(&part, &plan));
        // A missing file cannot match.
        assert!(!ck(2, 5, &id).usable("/nonexistent/x", &plan));
        // The bytes on disk came from a DIFFERENT rendition. Length and
        // count both agree, so without the plan id this would splice two
        // resolutions into one unplayable file.
        assert!(!ck(2, 5, "https://e/v720/seg0.ts").usable(&part, &plan));
        // A sidecar written before plan ids existed says nothing about
        // provenance, so it is refused rather than trusted.
        assert!(!ck(2, 5, "").usable(&part, &plan));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_checkpoint_round_trips_through_its_sidecar() {
        let dir = scratch("ckpt-io");
        let path = dir.join("o.ck").to_string_lossy().into_owned();
        Checkpoint::write(&path, 7, 4096, "https://e/v/seg0.ts");
        assert_eq!(
            Checkpoint::read(&path),
            Checkpoint {
                segments: 7,
                bytes: 4096,
                plan_id: "https://e/v/seg0.ts".into()
            }
        );
        // Anything unreadable reads as "start from the beginning".
        assert_eq!(Checkpoint::read("/nonexistent/x"), Checkpoint::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn cancelling_stops_the_transfer() {
        let plan = Plan::build(&parse(&media_vod(), "https://e/x.m3u8"), None).unwrap();
        let dir = scratch("cancel");
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let err = fetch_all(
            &plan,
            &mut f,
            &staging,
            slow_fetcher(1),
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(true)),
            Resume::default(),
            Concurrency::default(),
            &Keys::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// AES-128-CBC/PKCS#7 of `plain` under `key`/`iv`, as a segment on disk
    /// would be.
    fn encrypt(plain: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        use aes::cipher::{block_padding::Pkcs7, BlockModeEncrypt, KeyIvInit};
        let mut buf = vec![0u8; plain.len() + 16];
        buf[..plain.len()].copy_from_slice(plain);
        let n = cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
            .encrypt_padded::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();
        buf.truncate(n);
        buf
    }

    /// A fetcher that answers each URL with a fixed body, all at once.
    fn bodies(map: Vec<(&'static str, Vec<u8>)>) -> impl Fetcher {
        let map = Arc::new(map);
        move |seg: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
            let map = map.clone();
            Box::pin(async move {
                use std::io::Write;
                let body = map
                    .iter()
                    .find(|(suffix, _)| seg.url.ends_with(suffix))
                    .map(|(_, b)| b.clone())
                    .ok_or_else(|| std::io::Error::other(format!("no body for {}", seg.url)))?;
                std::fs::File::create(&dest)?.write_all(&body)?;
                counter.store(body.len() as u64, Ordering::Relaxed);
                Ok(body.len() as u64)
            })
        }
    }

    async fn assemble(
        plan: &Plan,
        fetch: impl Fetcher,
        keys: &Keys,
        name: &str,
    ) -> std::io::Result<Vec<u8>> {
        let dir = scratch(name);
        let staging = dir.join("o.part").to_string_lossy().into_owned();
        let mut f = std::fs::File::create(&staging).unwrap();
        let r = fetch_all(
            plan,
            &mut f,
            &staging,
            fetch,
            &Arc::new(Meter::default()),
            &Arc::new(AtomicBool::new(false)),
            Resume::default(),
            Concurrency::default(),
            keys,
        )
        .await;
        drop(f);
        let out = std::fs::read(&staging).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        r.map(|_| out)
    }

    /// A segment whose bytes keep arriving, however slowly, is not a stall:
    /// the old wall-clock timeout killed an 80 s segment three times over
    /// and failed the download.
    #[tokio::test(start_paused = true)]
    async fn a_slow_but_moving_segment_is_not_abandoned() {
        let plan = Plan::build(
            &parse(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n",
                "https://e/x.m3u8",
            ),
            None,
        )
        .unwrap();
        // Five bytes, 25 s apart: 125 s in all, twice the idle allowance.
        let fetch = move |_: Segment, dest: String, counter: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move {
                use std::io::Write;
                let mut f = std::fs::File::create(&dest)?;
                for b in b"slow!" {
                    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
                    f.write_all(&[*b])?;
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                Ok(5)
            })
        };
        let out = assemble(&plan, fetch, &Keys::new(), "slow-moving")
            .await
            .unwrap();
        assert_eq!(out, b"slow!");
    }

    /// The absolute ceiling still exists, for a segment that drips forever.
    #[tokio::test(start_paused = true)]
    async fn a_segment_that_trickles_forever_meets_the_ceiling() {
        let plan = Plan::build(
            &parse(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n",
                "https://e/x.m3u8",
            ),
            None,
        )
        .unwrap();
        let fetch = move |_: Segment, _: String, counter: Arc<AtomicU64>| -> FetchSeg {
            Box::pin(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        let err = assemble(&plan, fetch, &Keys::new(), "trickle")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(err.to_string().contains("still arriving"), "{err}");
    }

    #[test]
    fn a_later_init_map_is_recorded_where_it_takes_over() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-MAP:URI=\"a.mp4\"\n#EXTINF:4,\ns0.m4s\n\
                    #EXT-X-MAP:URI=\"a.mp4\"\n#EXTINF:4,\ns1.m4s\n\
                    #EXT-X-DISCONTINUITY\n\
                    #EXT-X-MAP:URI=\"b.mp4\"\n#EXTINF:4,\ns2.m4s\n#EXTINF:4,\ns3.m4s\n\
                    #EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/v/x.m3u8");
        assert_eq!(pl.init, Some(Segment::new("https://e/v/a.mp4")));
        // A repeated map is not a change; the new one is, at the segment
        // that first needs it.
        assert_eq!(pl.init_changes, [(2, Segment::new("https://e/v/b.mp4"))]);
        assert_eq!(pl.segments.len(), 4);
    }

    #[tokio::test]
    async fn a_changed_init_map_is_written_before_the_segments_that_need_it() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                    #EXT-X-MAP:URI=\"a.mp4\"\n#EXTINF:4,\ns0.m4s\n\
                    #EXT-X-DISCONTINUITY\n\
                    #EXT-X-MAP:URI=\"b.mp4\"\n#EXTINF:4,\ns1.m4s\n#EXTINF:4,\ns2.m4s\n\
                    #EXT-X-ENDLIST\n";
        let plan = Plan::build(&parse(text, "https://e/v/x.m3u8"), None).unwrap();
        let fetch = bodies(vec![
            ("a.mp4", b"A".to_vec()),
            ("b.mp4", b"B".to_vec()),
            ("s0.m4s", b"[0]".to_vec()),
            ("s1.m4s", b"[1]".to_vec()),
            ("s2.m4s", b"[2]".to_vec()),
        ]);
        let out = assemble(&plan, fetch, &Keys::new(), "init-change")
            .await
            .unwrap();
        assert_eq!(
            out, b"A[0]B[1][2]",
            "the second init must precede segment 1"
        );
    }

    #[test]
    fn a_key_that_cannot_be_fetched_is_refused_not_dropped() {
        for uri in ["skd://vault/1", "mailto:x@y"] {
            let text = format!(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                 #EXT-X-KEY:METHOD=AES-128,URI=\"{uri}\"\n\
                 #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n"
            );
            let pl = parse(&text, "https://e/x.m3u8");
            // Writing the ciphertext as if it were clear would be the silent
            // failure; the segment carries no key AND the plan is refused.
            assert!(pl.segments[0].key.is_none());
            assert_eq!(
                Plan::build(&pl, None),
                Err(Refusal::UnresolvableKey(uri.into())),
                "{uri}"
            );
        }
        // A key tag with no URI at all is the same failure.
        let pl = parse(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-KEY:METHOD=AES-128\n\
             #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            "https://e/x.m3u8",
        );
        assert!(matches!(
            Plan::build(&pl, None),
            Err(Refusal::UnresolvableKey(_))
        ));
    }

    #[tokio::test]
    async fn an_inline_data_key_is_decoded_and_used_without_a_fetch() {
        let key = test_key();
        let iv = iv_from_sequence(0);
        let b64 = {
            const T: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for c in key.chunks(3) {
                let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                out.push(T[(n >> 18) as usize & 63] as char);
                out.push(T[(n >> 12) as usize & 63] as char);
                out.push(if c.len() > 1 {
                    T[(n >> 6) as usize & 63] as char
                } else {
                    '='
                });
                out.push(if c.len() > 2 {
                    T[n as usize & 63] as char
                } else {
                    '='
                });
            }
            out
        };
        let text = format!(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"data:text/plain;base64,{b64}\"\n\
             #EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n"
        );
        let plan = Plan::build(&parse(&text, "https://e/x.m3u8"), None).unwrap();
        assert!(plan.is_encrypted());
        assert!(plan.key_uris().is_empty(), "nothing to fetch");
        assert_eq!(plan.inline_keys().values().next(), Some(&key));

        let fetch = bodies(vec![("seg0.ts", encrypt(b"clear text", &key, &iv))]);
        let out = assemble(&plan, fetch, &Keys::new(), "data-key")
            .await
            .unwrap();
        assert_eq!(out, b"clear text");

        // Percent-encoded is the other form, and a wrong length is refused.
        let pct: String = key.iter().map(|b| format!("%{b:02X}")).collect();
        let pl = parse(
            &text.replace(
                &format!("data:text/plain;base64,{b64}"),
                &format!("data:,{pct}"),
            ),
            "https://e/x.m3u8",
        );
        assert_eq!(Plan::build(&pl, None).unwrap().inline_keys().len(), 1);
        let short = parse(
            &text.replace(
                &format!("data:text/plain;base64,{b64}"),
                "data:;base64,AAEC",
            ),
            "https://e/x.m3u8",
        );
        assert!(matches!(
            Plan::build(&short, None),
            Err(Refusal::UnresolvableKey(_))
        ));
    }

    #[test]
    fn raw_audio_playlists_keep_their_own_extension() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\na0.aac\n#EXTINF:4,\na1.aac\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/x.m3u8");
        // Concatenable like transport stream, so that is the kind...
        assert_eq!(pl.segments_kind, Some(Segments::Ts));
        assert_eq!(pl.raw_audio, Some(RawAudio::Aac));
        let plan = Plan::build(&pl, None).unwrap();
        // ...but the file is `.aac`, not `.ts`.
        assert_eq!(plan.native_ext(), "aac");
        assert_eq!(plan_finish_for(&plan, "aac", false), Finish::Move);
        assert_eq!(plan_finish_for(&plan, "AAC", true), Finish::Move);
        assert_eq!(plan_finish_for(&plan, "mp4", true), Finish::Remux);
        assert_eq!(plan_finish_for(&plan, "mp4", false), Finish::Refuse);
        assert_eq!(plan_finish_for(&plan, "ts", false), Finish::Refuse);

        let mp3 = parse(&text.replace(".aac", ".mp3?t=1"), "https://e/x.m3u8");
        assert_eq!(mp3.raw_audio, Some(RawAudio::Mp3));
        assert_eq!(Plan::build(&mp3, None).unwrap().native_ext(), "mp3");
        // A container playlist is untouched by any of this.
        let ts = Plan::build(&parse(&media_vod(), "https://e/x.m3u8"), None).unwrap();
        assert_eq!(ts.raw_audio, None);
        assert_eq!(
            plan_finish_for(&ts, "ts", false),
            plan_finish(Segments::Ts, "ts", false)
        );
    }

    #[test]
    fn a_gap_segment_is_skipped_but_still_counts_in_the_sequence() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:10\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n\
                    #EXTINF:4,\na.ts\n\
                    #EXT-X-GAP\n#EXTINF:4,\nmissing.ts\n\
                    #EXTINF:4,\nc.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/x.m3u8");
        assert_eq!(pl.segments.len(), 2, "the gap is not fetched");
        assert!(!pl.segments.iter().any(|s| s.url.ends_with("missing.ts")));
        assert_eq!(pl.sequences, [10, 12]);
        // The IV is the segment's real sequence number, gap included.
        assert_eq!(
            pl.segments[1].key.as_ref().unwrap().iv,
            iv_from_sequence(12)
        );
        assert_eq!(
            pl.timed_window()
                .iter()
                .map(|(n, _, _)| *n)
                .collect::<Vec<_>>(),
            [10, 12]
        );
    }

    #[test]
    fn variables_are_substituted_into_uris() {
        let master = "#EXTM3U\n#EXT-X-DEFINE:NAME=\"cdn\",VALUE=\"https://edge.example\"\n\
                      #EXT-X-STREAM-INF:BANDWIDTH=1000\n{$cdn}/v/index.m3u8?tok=abc\n";
        let m = parse(master, "https://origin.example/master.m3u8");
        assert_eq!(
            m.variants[0].url,
            "https://edge.example/v/index.m3u8?tok=abc"
        );
        assert_eq!(
            m.variables,
            [("cdn".to_string(), "https://edge.example".to_string())]
        );

        let media = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n\
                     #EXT-X-DEFINE:IMPORT=\"cdn\"\n\
                     #EXT-X-DEFINE:QUERYPARAM=\"tok\"\n\
                     #EXT-X-DEFINE:NAME=\"dir\",VALUE=\"seg\"\n\
                     #EXT-X-KEY:METHOD=AES-128,URI=\"{$cdn}/k?t={$tok}\",IV=0x000102030405060708090A0B0C0D0E0F\n\
                     #EXTINF:4,\n{$cdn}/{$dir}/0.ts?t={$tok}\n#EXT-X-ENDLIST\n";
        let pl = parse_with_variables(media, &m.variants[0].url, &m.variables);
        assert_eq!(pl.refusal, None);
        assert_eq!(pl.segments[0].url, "https://edge.example/seg/0.ts?t=abc");
        assert_eq!(
            pl.segments[0].key.as_ref().unwrap().uri,
            "https://edge.example/k?t=abc"
        );

        // A reference nothing defines is a refusal, not a literal `{$x}` URL.
        let undefined =
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\n{$nope}/0.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(undefined, "https://e/x.m3u8");
        assert!(pl.segments.is_empty());
        assert_eq!(
            Plan::build(&pl, None),
            Err(Refusal::UndefinedVariable("nope".into()))
        );
        // An IMPORT the master never declared is the same.
        let pl = parse_with_variables(media, "https://e/x.m3u8?tok=1", &[]);
        assert_eq!(pl.refusal, Some(Refusal::UndefinedVariable("cdn".into())));
    }

    #[test]
    fn an_init_map_under_an_active_key_is_encrypted_too() {
        let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:5\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x000102030405060708090A0B0C0D0E0F\n\
                    #EXT-X-MAP:URI=\"init.mp4\"\n\
                    #EXTINF:4,\ns0.m4s\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/v/x.m3u8");
        let init = pl.init.as_ref().unwrap();
        let k = init
            .key
            .as_ref()
            .expect("RFC 8216 §4.3.2.5: the key applies to the map");
        assert_eq!(k.uri, "https://e/v/k");
        assert_eq!(k.iv[..4], [0, 1, 2, 3]);
        let plan = Plan::build(&pl, None).unwrap();
        assert!(plan.is_encrypted());

        // Without an explicit IV the map takes the next segment's number.
        let no_iv = text.replace(",IV=0x000102030405060708090A0B0C0D0E0F", "");
        let pl = parse(&no_iv, "https://e/v/x.m3u8");
        assert_eq!(pl.init.unwrap().key.unwrap().iv, iv_from_sequence(5));
        // And a map before any key is in the clear.
        let clear = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n\
                     #EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\ns0.m4s\n#EXT-X-ENDLIST\n";
        assert!(parse(clear, "https://e/x.m3u8").init.unwrap().key.is_none());
    }

    #[test]
    fn absurd_numbers_do_not_overflow() {
        // A byte range past the end of any file is dropped, not wrapped.
        let text =
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n\
                    #EXTINF:4,\n#EXT-X-BYTERANGE:10@18446744073709551610\nbig.mp4\n\
                    #EXTINF:4,\n#EXT-X-BYTERANGE:18446744073709551615@5\nbig.mp4\n\
                    #EXTINF:4,\nok.ts\n#EXTINF:4,\nok2.ts\n#EXT-X-ENDLIST\n";
        let pl = parse(text, "https://e/x.m3u8");
        assert_eq!(pl.segments.len(), 2, "both overflowing ranges are dropped");
        assert_eq!(pl.sequences, [u64::MAX, u64::MAX], "saturated, not wrapped");
        assert_eq!(pl.timed_window().len(), 2);
        let s = Segment {
            url: "https://e/x".into(),
            range: Some((u64::MAX, 5)),
            key: None,
        };
        assert_eq!(s.range_header(), None);
    }

    #[test]
    fn a_plan_id_ignores_signed_query_strings() {
        let signed = |tok: &str| {
            format!(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4?sig={tok}\"\n\
                 #EXTINF:4,\ns0.m4s?sig={tok}\n#EXTINF:4,\ns1.m4s?sig={tok}\n#EXT-X-ENDLIST\n"
            )
        };
        let a = Plan::build(&parse(&signed("aaa"), "https://e/v/x.m3u8"), None).unwrap();
        let b = Plan::build(&parse(&signed("bbb"), "https://e/v/x.m3u8"), None).unwrap();
        assert_eq!(a.id(), b.id(), "a rotated token must not defeat a resume");
        assert!(!a.id().contains("aaa"));
        assert!(!a.id().chars().any(char::is_whitespace));
        // A different rendition, or a different length, is a different plan.
        let other = Plan::build(
            &parse(&signed("aaa").replace("v/", "w/"), "https://e/w/x.m3u8"),
            None,
        )
        .unwrap();
        assert_ne!(a.id(), other.id());
        let shorter = Plan::build(
            &parse(
                &signed("aaa").replace("#EXTINF:4,\ns1.m4s?sig=aaa\n", ""),
                "https://e/v/x.m3u8",
            ),
            None,
        )
        .unwrap();
        assert_ne!(a.id(), shorter.id());
    }

    #[test]
    fn every_refusal_explains_itself_without_calling_it_drm() {
        for r in [
            Refusal::UnreadableIv,
            Refusal::UnresolvableKey("skd://x".into()),
            Refusal::UndefinedVariable("cdn".into()),
            Refusal::Unsupported("no segment addressing".into()),
        ] {
            let msg = r.to_string();
            assert!(!msg.contains("protected"), "{msg}");
            assert!(!msg.is_empty());
        }
        assert!(Refusal::UnresolvableKey("skd://x".into())
            .to_string()
            .contains("skd://x"));
        assert!(Refusal::UndefinedVariable("cdn".into())
            .to_string()
            .contains("{$cdn}"));
        // An unknown METHOD is unsupported encryption, not clear text.
        let pl = parse(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-KEY:METHOD=AES-256,URI=\"k\"\n\
             #EXTINF:4,\na.ts\n#EXT-X-ENDLIST\n",
            "https://e/x.m3u8",
        );
        assert_eq!(
            Plan::build(&pl, None),
            Err(Refusal::Encrypted("AES-256".into()))
        );
    }

    #[test]
    fn a_raw_audio_assembly_is_placed_under_its_own_extension() {
        let dir = fresh("finish-raw");
        let src = dir.join("o.part");
        std::fs::write(&src, b"adts").unwrap();
        let plan = Plan::build(
            &parse(
                "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\na0.aac\n#EXT-X-ENDLIST\n",
                "https://e/x.m3u8",
            ),
            None,
        )
        .unwrap();
        let dst = dir.join("out.aac");
        assert_eq!(finish_plan(&plan, "aac", &src, &dst), Ok(Finished::AsIs));
        assert_eq!(std::fs::read(&dst).unwrap(), b"adts");
        assert!(!src.exists());
        // A container playlist takes the old path unchanged.
        let ts = Plan::build(&parse(&media_vod(), "https://e/x.m3u8"), None).unwrap();
        std::fs::write(&src, b"ts").unwrap();
        assert_eq!(
            finish_plan(&ts, "ts", &src, &dir.join("out.ts")),
            Ok(Finished::AsIs)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_extinf_without_a_uri_lists_no_segment() {
        let pl = parse(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\n#EXT-X-ENDLIST\n",
            "https://e/x.m3u8",
        );
        assert!(pl.segments.is_empty());
        assert_eq!(Plan::build(&pl, None), Err(Refusal::Empty));
        // A tag between EXTINF and its URI does not lose the segment.
        let pl = parse(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\n#EXT-X-PROGRAM-DATE-TIME:2026-01-01T00:00:00Z\na.ts\n#EXT-X-ENDLIST\n",
            "https://e/x.m3u8",
        );
        assert_eq!(pl.segments.len(), 1);
    }
}

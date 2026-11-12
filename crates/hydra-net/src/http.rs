//! HTTP/1.1 request construction, response-head parsing, probing, and the
//! per-range fetch path.
//!
//! Outgoing request headers are built centrally by [`build_request_head`]
//! to ensure consistency across probes and body fetches.

use crate::framebuf;
use crate::polite::Pace;
use crate::sink::SparseSink;
use crate::{is_field, Arrival, Connector, Target, READ_BUF};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// A 3xx answer, with the destination intact.
///
/// Following the hop belongs to the caller: it owns the URL the request was
/// built from and the budget for how many hops are too many. So the
/// destination has to survive the trip back through `io::Result`.
///
/// It travels as a downcastable payload rather than inside the message TEXT.
/// Four call sites resolve these, and parsing them back out of a formatted
/// string couples every one to the exact wording here — reword it and they
/// all silently stop following redirects, turning the common case (a CDN
/// pointing at a regional edge) into a hard failure with no compiler
/// complaint. Downcasting, the compiler sees the coupling.
#[derive(Clone, Debug)]
pub struct Redirect {
    pub status: u16,
    /// `Location`, verbatim — possibly relative to the URL that was asked
    /// for, so callers must resolve it against that, not use it bare.
    pub location: String,
}

impl std::fmt::Display for Redirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "redirect {} to {}", self.status, self.location)
    }
}

impl std::error::Error for Redirect {}

impl Redirect {
    /// The redirect carried by this error, if it carries one.
    pub fn of(e: &io::Error) -> Option<&Redirect> {
        e.get_ref()?.downcast_ref::<Redirect>()
    }
}

/// Probe a target: `Content-Length`, `Accept-Ranges`, validator.
/// What a HEAD request reveals about an object.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub size: u64,
    /// Server advertises byte-range support.
    pub ranges: bool,
    /// `ETag`, or `Last-Modified` when no ETag was offered.
    pub validator: Option<String>,
    /// `Last-Modified`, verbatim, whether or not an ETag was also sent.
    ///
    /// Kept SEPARATELY from `validator` because the two answer different
    /// questions. `validator` answers "may these bytes be spliced with those?",
    /// and an ETag is the better answer whenever there is one — so the collapse
    /// to `etag.or(last_mod)` is right for resume and mirror assembly. But
    /// `--remote-time` asks "when was this object last changed?", which an ETag
    /// cannot answer at all: it is opaque. Reading the collapsed field for that
    /// purpose meant every server sending BOTH headers — GitHub, S3, and most
    /// CDNs, i.e. the common case — had its date discarded before the flag saw
    /// it, and `--remote-time` reported "no date-form validator" about a response
    /// that plainly carried one.
    pub last_modified: Option<String>,
    /// The validator is WEAK (`W/"..."`) or is a `Last-Modified` date.
    ///
    /// RFC 9110 permits a weak validator to compare equal across representations
    /// that are merely "semantically equivalent" — which is exactly the case
    /// multi-source assembly must not tolerate. Two mirrors agreeing on a weak
    /// ETag is not evidence they will serve the same bytes, so the engine treats a
    /// weak validator as grounds to use ONE source rather than splicing several.
    pub weak_validator: bool,
    /// `Content-Type`, a weak hint for classification.
    pub content_type: Option<String>,
    /// `Content-Disposition`, which may carry a filename.
    pub disposition: Option<String>,
    /// Status of the probe response.
    ///
    /// Kept because a 3xx probe is not a description of the object: GitHub's release
    /// assets answer HEAD with `302` and `Content-Length: 0`, so treating the headers
    /// as metadata reports a zero-byte object and the caller cannot tell that apart
    /// from an object that really is empty.
    pub status: u16,
    /// `Location`, when the probe was a redirect.
    pub location: Option<String>,
    /// The response header block, verbatim.
    ///
    /// Kept because a paraphrase is not a diagnostic: when a server misbehaves, the
    /// useful output is the bytes it actually sent — the exact `Content-Range`
    /// spelling, the `Via`/`X-Cache` chain, the validator's exact form. A summary
    /// showing "size: 0" hides which header was missing.
    pub raw_head: String,
    /// The request line and headers we sent, verbatim.
    pub raw_request: String,
}

/// The standard reason phrase for an HTTP status, or the bare number.
///
/// For error messages: "server answered 400 Bad Request" tells a user what went
/// wrong; "server answered 400" tells them to look it up.
pub fn describe_status(status: u16) -> String {
    let phrase = match status {
        // The success codes are here because `Probe::refusal` names them: most
        // of 2xx is an answer rather than a file, and "server answered 202"
        // leaves the reader to look up the one word that explains why.
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        421 => "Misdirected Request",
        423 => "Locked",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        511 => "Network Authentication Required",
        _ => return status.to_string(),
    };
    format!("{status} {phrase}")
}

/// The text between two markers, if both are present in order.
///
/// A scan rather than [`crate::xml`]: an error body is read off the front of the
/// same buffer as the headers and is routinely truncated mid-document, which a
/// conforming parser rejects outright. The one element wanted is still there.
fn between<'a>(hay: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = hay.find(open)? + open.len();
    let rest = &hay[start..];
    let end = rest.find(close)?;
    Some(rest[..end].trim())
}

/// Shorten to `max` CHARACTERS, never bytes: slicing a UTF-8 string at a byte
/// offset panics mid-codepoint, and server messages are not all ASCII.
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s.to_string(),
    }
}

impl Probe {
    /// This probe described a redirect rather than the object.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308) && self.location.is_some()
    }

    /// Why this response cannot be planned against, or `None` when it can.
    ///
    /// Every caller that plans a transfer has to make this test, and each one
    /// that improvised it shipped a different half of it. `status >= 400` alone
    /// misses that most of `2xx` is an ANSWER rather than a representation:
    /// AWS WAF turns a bot challenge away with `202` and `Content-Length: 0`,
    /// which read as metadata describe a zero-byte object, so the download
    /// "succeeded" with an empty file on disk — the worst failure available,
    /// because nothing looks wrong. Only `200`, `203` and `206` carry bytes.
    ///
    /// The phrase names no host: every caller frames it with its own, and a
    /// message carrying two of them reads as a bug.
    pub fn refusal(&self) -> Option<String> {
        if self.status >= 400 {
            let mut msg = format!("server answered {}", describe_status(self.status));
            // Quoted, because callers append their own context: unquoted, the
            // server's sentence runs into the host and reads as one clause —
            // "403 Forbidden: Request has expired for s3q.ait.dtu.dk".
            if let Some(detail) = self.error_detail() {
                msg.push_str(&format!(" ({detail:?})"));
            }
            return Some(msg);
        }
        // A redirect is somebody else's job — `is_redirect` and the follower
        // above it — so it is deliberately not refused here.
        if (200..300).contains(&self.status) && !matches!(self.status, 200 | 203 | 206) {
            let mut msg = format!(
                "server answered {}, which carries no file",
                describe_status(self.status)
            );
            if self.bot_challenge() {
                msg.push_str(" (a bot challenge: this link opens only in a browser)");
            }
            return Some(msg);
        }
        None
    }

    /// The server's own explanation, lifted from an error response body.
    ///
    /// The body is already in hand: [`probe_via_get`] reads until the header
    /// block is complete, and a short error body arrives in the same packet, so
    /// this costs nothing but the parse. It is what turns "403 Forbidden" —
    /// which sends the user hunting for a proxy or a credential — into "Request
    /// has expired", which names the actual problem.
    ///
    /// Only consulted for error statuses: on a success the same trailing bytes
    /// are the first bytes of the object, and quoting those into a message
    /// would print binary at the user.
    fn error_detail(&self) -> Option<String> {
        if self.status < 400 {
            return None;
        }
        let body = self.raw_head.split_once("\r\n\r\n").map(|(_, b)| b)?.trim();
        // S3 and every dialect of it answer in this XML, and the message is the
        // one sentence a person needs.
        let text = match between(body, "<Message>", "</Message>") {
            Some(m) => m,
            // Anything else: a short body is a plausible sentence, a long one is
            // an error PAGE whose markup would bury the message it contains.
            None if !body.is_empty() && body.len() <= 300 && !body.starts_with('<') => body,
            _ => return None,
        };
        let clean: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
        (!clean.is_empty()).then(|| truncate_chars(&clean, 200))
    }

    /// The response is an anti-bot interstitial rather than the object.
    ///
    /// Read off AWS WAF's own header rather than guessed at from the body: the
    /// header states the verdict, and a downloader that inferred it from page
    /// text would misread every article that mentions the word.
    fn bot_challenge(&self) -> bool {
        header_value(&self.raw_head, "x-amzn-waf-action")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge"))
    }

    /// Mirrors this response advertised via `Link: <...>; rel=duplicate`
    /// (Metalink over HTTP, RFC 6249), best-first.
    ///
    /// Derived from [`Self::raw_head`] on demand rather than stored, because it
    /// is needed once per transfer and the header block is already kept verbatim
    /// for exactly this class of question.
    ///
    /// A server that sends these is telling the client what it needs in order to
    /// survive that server dying mid-transfer, and it costs no extra request to
    /// listen: the probe already happened. See [`crate::metalink`].
    pub fn mirrors(&self) -> Vec<crate::metalink::MetaUrl> {
        crate::metalink::mirrors_from_head(&self.raw_head)
    }

    /// URL of a Metalink document describing this object, when the response
    /// pointed at one with `Link: <...>; rel=describedby; type=...metalink...`.
    pub fn metalink_url(&self) -> Option<String> {
        crate::metalink::describedby_metalink(&self.raw_head)
    }

    /// This response IS a Metalink document rather than the object asked for.
    ///
    /// Checked on `Content-Type` alone, never on the body: the body is not in
    /// hand at probe time, and a downloader that decides what a URL serves by
    /// guessing at bytes it has not read is how a real download gets mistaken
    /// for a mirror list.
    pub fn serves_metalink(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(crate::metalink::is_metalink_media_type)
    }

    /// This response could be a redirector PAGE — a forward expressed in HTML
    /// instead of in a `Location` header (see [`crate::redirect`]).
    ///
    /// A cheap pre-filter, not an answer: it says only that fetching the body
    /// to look for a redirect directive is worth one small request. The three
    /// conditions are what separate a forwarding page from a download:
    ///
    /// * **HTML.** An object with any other type is the object.
    /// * **No `Content-Disposition`.** A server that names a file to save is
    ///   serving that file, whatever its type.
    /// * **Small, or of unstated length.** Redirector pages are under a
    ///   kilobyte; HEAD against one commonly omits `Content-Length` entirely
    ///   (chunked, or `Vary`-driven), which is why an unknown size qualifies.
    ///
    /// Deliberately permissive about ordinary web pages: one that really does
    /// carry a meta refresh is one a browser would follow too, so following it
    /// is the answer the user expects rather than a false positive.
    /// The length the server STATED, as opposed to the length this probe inferred.
    ///
    /// `size` cannot answer the question on its own: it is 0 both when the server
    /// sent no `Content-Length` and when it sent `Content-Length: 0`, and those are
    /// opposite facts. The first means "ask another way"; the second means the
    /// object is empty and every further request is waste — or worse, since a
    /// `bytes=0-0` GET against a zero-length object is unsatisfiable and answered
    /// `416`, which reads as a failure for a download that should have succeeded.
    pub fn stated_length(&self) -> Option<u64> {
        header_value(&self.raw_head, "content-length").and_then(|v| v.trim().parse().ok())
    }

    pub fn maybe_redirector(&self) -> bool {
        (200..300).contains(&self.status)
            && self.disposition.is_none()
            && self.size <= crate::redirect::MAX_REDIRECTOR_PAGE
            && self
                .content_type
                .as_deref()
                .map(|c| c.trim_start().to_ascii_lowercase().starts_with("text/html"))
                .unwrap_or(false)
    }
}

impl Probe {
    /// Filename from a `Content-Disposition` header, if it carries one.
    ///
    /// Parsed the way RFC 6266 defines the header rather than by scanning it
    /// for the substring `filename=`, because the servers this matters most
    /// for are exactly the ones such a scan misses. A name that is not ASCII
    /// cannot be stated in a plain `filename=` at all — RFC 6266 §4.3 sends
    /// it as the extended `filename*=UTF-8''<pct-encoded>` form, which does
    /// NOT contain `filename=` — so the scan found nothing, the caller fell
    /// back to the last segment of the URL, and a cloud drive's object id
    /// became the filename: a file the user picked as `无极助手3.3.exe` was
    /// offered as `211784095912171.rar`, wrong name and, by its extension,
    /// wrong category too.
    ///
    /// What is understood here, in order of preference:
    ///
    /// * `filename*=UTF-8''%E6%97%A0%E6%9E%81.exe` — the extended form,
    ///   percent-decoded. It beats a plain `filename` in the same header, as
    ///   RFC 6266 §4.3 requires: a server that sends both sends the plain one
    ///   for old clients, transliterated or mojibake.
    /// * `filename="..."` — a quoted-string, with `\"` escapes undone and an
    ///   embedded `;` kept as part of the name instead of read as the next
    ///   parameter.
    /// * `filename=%E6%97%A0%E6%9E%81.exe` — a percent-encoded name in the
    ///   plain parameter. Not legal, common anyway, and decoded only when the
    ///   result is unambiguous; see [`decode_legacy_filename`].
    pub fn suggested_filename(&self) -> Option<String> {
        let d = self.disposition.as_ref()?;
        let params = disposition_params(d);
        let named = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let name = named("filename*")
            .and_then(|v| decode_ext_value(v))
            .or_else(|| named("filename").map(|v| decode_legacy_filename(v)))?;
        crate::filename::portable(&name)
    }
}

/// The parameters of a `Content-Disposition`: lowercased keys with their raw
/// values, in the order the header states them. The disposition type itself
/// (`attachment`) carries no `=` and so is not one.
///
/// Splitting on `;` alone is wrong for this header: `filename="a;b.zip"` is
/// ONE parameter, and cutting at the semicolon truncates the name to `a`.
fn disposition_params(d: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in d.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            // Kept, not consumed: unescaping belongs to whoever reads the
            // value, which needs the quoted-string intact to know it was one.
            '\\' if quoted => {
                cur.push(c);
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ';' if !quoted => {
                push_param(&mut out, &cur);
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    push_param(&mut out, &cur);
    out
}

fn push_param(out: &mut Vec<(String, String)>, raw: &str) {
    if let Some((k, v)) = raw.split_once('=') {
        let k = k.trim().to_ascii_lowercase();
        if !k.is_empty() {
            out.push((k, v.trim().to_string()));
        }
    }
}

/// An RFC 8187 extended value: `charset'language'pct-encoded`.
///
/// Only the two charsets the specifications actually define are decoded.
/// RFC 8187 narrowed the extended value to UTF-8; RFC 5987 before it also
/// allowed ISO-8859-1. Anything else is a server naming an encoding this
/// crate would have to carry a conversion table for — GBK, EUC-KR,
/// Shift_JIS, windows-1256 — and GUESSING one is worse than refusing: a
/// wrong table yields mojibake that still LOOKS like a filename (`æ— æž`
/// for `无极`, `ÙÙØ§ÛÙ` for `فایل`), so it is saved, listed and
/// categorised as if it were right. Refusing hands the caller back to the
/// plain `filename` parameter or the URL, which are at least honest.
///
/// Bytes that do not decode are refused for the same reason, and that is
/// why the fallback is not `from_utf8_lossy`: a name is not improved by
/// having its Korean or Arabic replaced with U+FFFD.
fn decode_ext_value(v: &str) -> Option<String> {
    // The quotes are not part of the grammar, but servers add them.
    let v = v.trim().trim_matches('"');
    let (charset, rest) = v.split_once('\'')?;
    // The language tag is advisory for a filename; only its delimiter matters.
    let (_lang, encoded) = rest.split_once('\'')?;
    let bytes = crate::url::percent_decode_bytes(encoded);
    let s = match charset.trim().to_ascii_lowercase().as_str() {
        // Latin-1 is the one legacy encoding that needs no table: every byte
        // is its own code point.
        "iso-8859-1" | "latin1" => bytes.iter().map(|&c| c as char).collect(),
        // UTF-8, and any charset whose bytes turn out to BE UTF-8 — servers
        // mislabel the parameter far more often than they mean a codepage.
        _ => String::from_utf8(bytes).ok()?,
    };
    (!s.trim().is_empty()).then_some(s)
}

/// A plain `filename=` value: unquoted, and percent-decoded where the server
/// used an encoding the header has no room for.
///
/// The decoding is guarded from both sides, because a filename is not a URL
/// and `%` is a character names really contain.
///
/// * The value must be pure ASCII to begin with. Percent-encoding produces
///   nothing else, so a value carrying `و` or `한` or `の` is a raw name
///   being read literally — and one that happens to contain `%20` as well
///   (`تخفیف 100%20.pdf`) must not be silently reworded by decoding it.
/// * The result must be non-ASCII valid UTF-8. `100%20off.zip` is a name a
///   server may mean exactly as written, and its decoding (`100 off.zip`) is
///   ASCII — indistinguishable from an unencoded name, so it is left alone.
///   Escapes spelling a non-ASCII name are the opposite: no filename spells
///   `%D9%81%D8%A7%DB%8C%D9%84` by accident.
///
/// Bytes that decode to no valid UTF-8 are left alone too. They are usually
/// a legacy codepage — GBK, windows-1256, EUC-KR — which cannot be read
/// without a table, and a lossy read would replace the whole name with
/// U+FFFD.
fn decode_legacy_filename(v: &str) -> String {
    let raw = unquote(v);
    if !raw.is_ascii() || !raw.contains('%') {
        return raw;
    }
    match String::from_utf8(crate::url::percent_decode_bytes(&raw)) {
        Ok(d) if !d.is_ascii() && !d.trim().is_empty() => d,
        _ => raw,
    }
}

/// A quoted-string with its `\` escapes resolved; anything unquoted verbatim.
fn unquote(v: &str) -> String {
    let v = v.trim();
    let Some(inner) = v.strip_prefix('"') else {
        return v.to_string();
    };
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for c in inner.chars() {
        match c {
            _ if escaped => {
                out.push(c);
                escaped = false;
            }
            '\\' => escaped = true,
            _ => out.push(c),
        }
    }
    out
}

/// Build a complete HTTP/1.1 request head for `t`: request line, `Host`,
/// optional `Range` (inclusive first/last byte positions), `Accept`,
/// `User-Agent`, the target's extra headers, and `Connection: close`.
///
/// Central request head builder ensuring that custom headers and User-Agent
/// are consistently applied to both metadata probes and range body requests.
/// Whether this request intends to keep the connection open afterwards.
///
/// Spelled out as a type rather than a bare `bool` because the two callers want
/// opposite defaults and the consequence of getting it wrong is asymmetric: a
/// request that says `keep-alive` and then abandons the socket leaves the server
/// holding a connection until its idle timeout, while a request that says `close`
/// merely forgoes the reuse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Disposition {
    /// `Connection: close` — this response is the last on the socket.
    ///
    /// Correct for anything whose body extent the client cannot predict: a
    /// read-to-EOF stream, a probe that may be answered any number of ways, or a
    /// fetch the caller has no intention of following up.
    Close,
    /// `Connection: keep-alive` — the socket may be reused once the body has been
    /// consumed to its exact end.
    Reuse,
}

fn connection_header(d: Disposition) -> &'static str {
    match d {
        Disposition::Close => "Connection: close\r\n",
        // HTTP/1.1 is keep-alive by default and the header is strictly redundant,
        // but it is sent explicitly: intermediaries and older origins are
        // measurably more consistent when told, and it makes a packet capture
        // state the intent rather than leaving it to be inferred from an absence.
        Disposition::Reuse => "Connection: keep-alive\r\n",
    }
}

fn build_request_head(method: &str, t: &Target, range: Option<(u64, u64)>) -> String {
    build_request_head_disp(method, t, range, Disposition::Close)
}

fn build_request_head_disp(
    method: &str,
    t: &Target,
    range: Option<(u64, u64)>,
    disp: Disposition,
) -> String {
    let (target, host_hdr) = t.request_target();
    let mut req = format!("{method} {target} HTTP/1.1\r\nHost: {host_hdr}\r\n");
    if let Some(chrome) = chrome_profile(t) {
        push_chrome_request(&mut req, t, &chrome, range, disp);
        return req;
    }
    if let Some((first, last)) = range {
        req.push_str(&format!("Range: bytes={first}-{last}\r\n"));
    }
    req.push_str("Accept: */*\r\n");
    req.push_str(&format!("User-Agent: {}\r\n", t.user_agent()));
    for h in t.extra_headers() {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str(connection_header(disp));
    req.push_str("\r\n");
    req
}

/// What a Chrome-family `User-Agent` says about the browser behind it.
struct ChromeProfile {
    major: u32,
    platform: &'static str,
}

/// `t` is sent with a browser's whole header set, not the plain one.
pub(crate) fn has_browser_profile(t: &Target) -> bool {
    chrome_profile(t).is_some()
}

/// A request made under a Chrome `User-Agent` carries that browser's header
/// set; every other request keeps the plain one.
///
/// Akamai Bot Manager (download-center.epson.com) answers `403` to a Chrome
/// `User-Agent` whose headers are not Chrome's — missing `Accept-Language`,
/// `Accept-Encoding`, `Sec-Fetch-*`, or in the wrong order — while the same
/// request in Chrome's shape is served. The profile asks for `identity`
/// rather than Chrome's `gzip, deflate, br`: the same origin ranges the
/// compressed representation, whose offsets are not the file's.
fn chrome_profile(t: &Target) -> Option<ChromeProfile> {
    let ua = t.agent.as_deref()?;
    if !ua.starts_with("Mozilla/5.0") {
        return None;
    }
    let major = ua
        .split_once("Chrome/")?
        .1
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let platform = if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        return None;
    };
    Some(ChromeProfile { major, platform })
}

fn push_chrome_request(
    req: &mut String,
    t: &Target,
    chrome: &ChromeProfile,
    range: Option<(u64, u64)>,
    disp: Disposition,
) {
    let referer = t.extra_headers().find(|h| is_field(h, "referer"));
    let site = match referer {
        Some(r) if referer_is_same_origin(r, t) => "same-origin",
        Some(_) => "cross-site",
        None => "none",
    };
    req.push_str(connection_header(disp));
    req.push_str(&format!(
        "sec-ch-ua: \"Chromium\";v=\"{0}\", \"Not?A_Brand\";v=\"8\"\r\n\
         sec-ch-ua-mobile: ?0\r\n\
         sec-ch-ua-platform: \"{1}\"\r\n\
         Upgrade-Insecure-Requests: 1\r\n\
         User-Agent: {2}\r\n\
         Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8\r\n\
         Sec-Fetch-Site: {site}\r\n\
         Sec-Fetch-Mode: navigate\r\n\
         Sec-Fetch-User: ?1\r\n\
         Sec-Fetch-Dest: document\r\n",
        chrome.major,
        chrome.platform,
        t.user_agent(),
    ));
    if let Some(r) = referer {
        req.push_str(r);
        req.push_str("\r\n");
    }
    for (name, line) in [
        ("accept-encoding", "Accept-Encoding: identity"),
        ("accept-language", "Accept-Language: en-US,en;q=0.9"),
    ] {
        req.push_str(
            t.extra_headers()
                .find(|h| is_field(h, name))
                .unwrap_or(line),
        );
        req.push_str("\r\n");
    }
    for h in t.extra_headers().filter(|h| {
        !["referer", "accept-encoding", "accept-language"]
            .iter()
            .any(|name| is_field(h, name))
    }) {
        req.push_str(h);
        req.push_str("\r\n");
    }
    if let Some((first, last)) = range {
        req.push_str(&format!("Range: bytes={first}-{last}\r\n"));
    }
    req.push_str("\r\n");
}

fn referer_is_same_origin(referer_line: &str, t: &Target) -> bool {
    let value = referer_line.split_once(':').map_or("", |(_, v)| v.trim());
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = t.origin_endpoint();
    let default_port = if t.tls { 443 } else { 80 };
    let want = if port == default_port {
        host
    } else {
        format!("{host}:{port}")
    };
    authority.eq_ignore_ascii_case(&want)
}

/// A response that arrives encoded would be written to disk as the encoded
/// bytes, so a body with any `Content-Encoding` but `identity` is refused.
///
/// The engine decodes none of them, and an origin may encode a reply despite
/// the profile's `Accept-Encoding: identity`.
fn reject_content_encoding(head: &str) -> io::Result<()> {
    match header_value(head, "content-encoding") {
        Some(enc) if !enc.trim().eq_ignore_ascii_case("identity") => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "server answered with Content-Encoding: {}; the body is not the file's bytes",
                enc.trim()
            ),
        )),
        _ => Ok(()),
    }
}

/// Fetch an object whose length the server will not state, streaming to EOF.
///
/// Some resources have no knowable size in advance: a dynamically generated page, a
/// `Content-Range: bytes 0-0/*` reply (an asterisk total means "I will not say"), or a
/// chunked response with no length at all. Multi-source scheduling is impossible here —
/// with no size there are no ranges to divide — but *fetching* is not, and standard clients
/// handle these routinely. Refusing them made `hydra <html-page>` fail where other
/// tools succeed.
///
/// So this is the honest degradation: one connection, sequential, no resume, no
/// parallelism, and the caller is told that is what happened.
pub async fn fetch_streaming<C: Connector>(c: &C, t: &Target, path: &str) -> io::Result<u64> {
    fetch_streaming_observed(c, t, path, &AtomicU64::new(0), None, &Pace::unlimited()).await
}

/// The same streaming fetch, made observable, interruptible and rate-shaped.
///
/// The plain [`fetch_streaming`] hands back one number, once, when the whole body
/// has arrived. On a ten-gigabyte object that is an hour of a caller having nothing
/// to show and no way to stop: the byte counter sits at zero, the stop flag nothing
/// reads, and the socket keeps pulling until the object ends. All three are the same
/// missing thing — a loop nobody can see into.
///
/// `written` carries the running total, so a caller polling it can report progress
/// while the body is still arriving. `cancel` is checked between reads and ends the
/// transfer with [`io::ErrorKind::Interrupted`], the same signal
/// [`crate::run_transfer_cancellable`] uses, with the bytes so far flushed to disk.
/// `pace` shapes the reads themselves, so a rate cap applies here exactly as it does
/// on the ranged path.
pub async fn fetch_streaming_observed<C: Connector>(
    c: &C,
    t: &Target,
    path: &str,
    written: &AtomicU64,
    cancel: Option<&AtomicBool>,
    pace: &Pace,
) -> io::Result<u64> {
    let stopped = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let interrupted = || io::Error::new(io::ErrorKind::Interrupted, "cancelled");

    let mut s = c.connect(t).await?;
    let req = build_request_head("GET", t, None);
    s.write_all(req.as_bytes()).await?;

    let mut head = Vec::new();
    let mut buf = vec![0u8; 32 * 1024];
    let body_start = loop {
        if stopped() {
            return Err(interrupted());
        }
        let n = match s.read(&mut buf).await {
            Ok(0) => break find_crlf2(&head),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break find_crlf2(&head),
            Err(e) => return Err(e),
        };
        head.extend_from_slice(&buf[..n]);
        if let Some(i) = find_crlf2(&head) {
            break Some(i);
        }
        if head.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response headers exceed 64 KiB",
            ));
        }
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no header terminator"))?;

    let h = String::from_utf8_lossy(&head[..body_start]).to_string();
    let status: u16 = h
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status) {
        return Err(io::Error::other(format!(
            "server returned {status} for a streaming fetch"
        )));
    }
    reject_content_encoding(&h)?;
    let chunked = header_value(&h, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    use tokio::io::AsyncWriteExt as _;
    let f = tokio::fs::File::create(path).await?;
    let mut w = tokio::io::BufWriter::new(f);
    let mut total = 0u64;
    let first = &head[body_start..];

    if chunked {
        // Reuse the same de-framing rules as the ranged path: a chunk header, the
        // bytes, a CRLF, terminated by a zero-size chunk.
        let mut pending = first.to_vec();
        let mut need_size = true;
        let mut remaining = 0u64;
        loop {
            if stopped() {
                w.flush().await?;
                return Err(interrupted());
            }
            if need_size {
                if let Some(i) = find_crlf(&pending) {
                    let line = String::from_utf8_lossy(&pending[..i]).to_string();
                    let hex = line.split(';').next().unwrap_or("").trim();
                    let sz = u64::from_str_radix(hex, 16).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("bad chunk size {hex:?}"),
                        )
                    })?;
                    pending.drain(..i + 2);
                    if sz == 0 {
                        break;
                    }
                    remaining = sz;
                    need_size = false;
                    continue;
                }
            } else if !pending.is_empty() {
                let take = (remaining as usize).min(pending.len());
                w.write_all(&pending[..take]).await?;
                total += take as u64;
                written.store(total, Ordering::Relaxed);
                pending.drain(..take);
                remaining -= take as u64;
                if remaining == 0 {
                    // Consume the CRLF that terminates the chunk body.
                    while pending.len() < 2 {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        pending.extend_from_slice(&buf[..n]);
                    }
                    if pending.len() >= 2 {
                        pending.drain(..2);
                    }
                    need_size = true;
                }
                continue;
            }
            let want = pace.read_size(buf.len());
            let n = match s.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };
            pace.wait(n as u64).await;
            pending.extend_from_slice(&buf[..n]);
        }
    } else {
        w.write_all(first).await?;
        total += first.len() as u64;
        written.store(total, Ordering::Relaxed);
        loop {
            if stopped() {
                w.flush().await?;
                return Err(interrupted());
            }
            let want = pace.read_size(buf.len());
            match s.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => {
                    pace.wait(n as u64).await;
                    w.write_all(&buf[..n]).await?;
                    total += n as u64;
                    written.store(total, Ordering::Relaxed);
                }
                // An unclean TLS close is how many servers end a Connection: close
                // body. The bytes already read are still valid.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
    }
    w.flush().await?;
    Ok(total)
}

/// Fetch a small object whole, into memory, with a hard cap.
///
/// For checksum sidecars and manifests: tens to thousands of bytes, wanted as a string,
/// not worth a scheduler or a file. The cap is a refusal rather than a truncation — a
/// truncated manifest would parse as a manifest and select the wrong line.
/// Fetch a whole object to `path`, reusing a pooled connection when one is
/// available and giving it back afterwards.
///
/// # Why this exists next to [`fetch_streaming_observed`]
///
/// That one is for a body whose extent the client cannot predict, so it says
/// `Connection: close` and reads to EOF. Correct — and exactly wrong for the
/// workload this is for: an adaptive stream is a few hundred SMALL objects on
/// one origin, fetched back to back. Closing after each means a TCP and TLS
/// handshake per segment, which for a hundred-segment playlist is a hundred
/// handshakes to a host the client never stopped talking to. On a distant TLS
/// origin that is most of the wall clock.
///
/// So this one asks to keep the socket, frames the body exactly, and returns
/// the connection to the pool — the same reasoning, and the same safety
/// conditions, as the ranged path in [`crate::pool`]: a socket goes back only
/// when the client knows precisely where the body ended, because anything left
/// unread becomes the next response's first bytes.
///
/// A pooled socket may have been closed by the origin since it was put back.
/// That is not an error worth surfacing: the request is simply retried once on
/// a fresh connection.
pub async fn fetch_object<C: Connector>(
    c: &C,
    t: &Target,
    path: &str,
    written: &AtomicU64,
    cancel: Option<&AtomicBool>,
    pace: &Pace,
    pool: Option<&crate::pool::SharedPool<C::Stream>>,
) -> io::Result<u64> {
    let stopped = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let interrupted = || io::Error::new(io::ErrorKind::Interrupted, "cancelled");
    let req = build_request_head_disp("GET", t, None, Disposition::Reuse);

    let mut buf = vec![0u8; 32 * 1024];
    let mut redialed = false;
    // At most two attempts, and only ever because a POOLED socket turned out
    // to be dead. A fresh connection that fails is a real failure.
    let (mut s, head, body_start) = loop {
        if stopped() {
            return Err(interrupted());
        }
        let reused = if redialed {
            None
        } else {
            pool.and_then(|p| p.take(t))
        };
        let from_pool = reused.is_some();
        let mut s = match reused {
            Some(s) => s,
            None => {
                if let Some(p) = pool {
                    p.record_miss();
                }
                c.connect(t).await?
            }
        };
        let mut head = Vec::new();
        let outcome = match s.write_all(req.as_bytes()).await {
            Ok(()) => read_head(&mut s, &mut buf, &mut head).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(Some(p)) => break (s, head, p),
            // A silent close on a reused socket is the ordinary way an origin
            // retires a keep-alive connection; try once more on a new one.
            Ok(None) | Err(_) if from_pool && !redialed => {
                redialed = true;
                continue;
            }
            Ok(None) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before any response headers",
                ))
            }
            Err(e) => return Err(e),
        }
    };

    let h = String::from_utf8_lossy(&head[..body_start]).to_string();
    let status: u16 = h
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if (300..400).contains(&status) {
        // A CDN commonly answers a segment with a redirect to a regional
        // edge. The hop is the caller's to follow — it owns the URL and the
        // budget — so the destination travels in the error rather than being
        // flattened into "server returned 302".
        return Err(io::Error::other(Redirect {
            status,
            location: header_value(&h, "location").unwrap_or_default(),
        }));
    }
    if !(200..300).contains(&status) {
        return Err(io::Error::other(format!(
            "server returned {status} for {}",
            t.request_target().0
        )));
    }
    // A caller that put a `Range:` in the headers wants that slice and nothing
    // else: a 200 is the whole object, and a 206 from the wrong offset would be
    // written as if it were the right one.
    if let Some(lo) = t.extra_headers().find_map(range_header_start) {
        if status != 206 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("server ignored the Range request and answered {status}"),
            ));
        }
        let first = header_value(&h, "content-range")
            .as_deref()
            .and_then(parse_content_range_start)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "206 without a parsable Content-Range",
                )
            })?;
        if first != lo {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("206 Content-Range starts at {first}, requested {lo}"),
            ));
        }
    }
    let chunked = header_value(&h, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let stated: Option<u64> = header_value(&h, "content-length")
        .and_then(|v| v.trim().parse().ok())
        .filter(|_| !chunked);
    let server_will_close = header_value(&h, "connection")
        .map(|v| v.to_ascii_lowercase().contains("close"))
        .unwrap_or(false)
        || h.starts_with("HTTP/1.0");

    use tokio::io::AsyncWriteExt as _;
    let f = tokio::fs::File::create(path).await?;
    let mut w = tokio::io::BufWriter::new(f);
    let mut total = 0u64;
    let first = &head[body_start..];

    // `framed` records whether the body ended where the client stopped
    // reading. Only then is the socket safe to hand on.
    let framed;
    if chunked {
        let mut pending = first.to_vec();
        let mut need_size = true;
        let mut remaining = 0u64;
        let mut saw_end = false;
        loop {
            if stopped() {
                w.flush().await?;
                return Err(interrupted());
            }
            if need_size {
                if let Some(i) = find_crlf(&pending) {
                    let line = String::from_utf8_lossy(&pending[..i]).to_string();
                    let hex = line.split(';').next().unwrap_or("").trim();
                    let sz = u64::from_str_radix(hex, 16).map_err(|_| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("bad chunk size {hex:?}"),
                        )
                    })?;
                    pending.drain(..i + 2);
                    if sz == 0 {
                        saw_end = true;
                        break;
                    }
                    remaining = sz;
                    need_size = false;
                    continue;
                }
            } else if !pending.is_empty() {
                let take = (remaining as usize).min(pending.len());
                w.write_all(&pending[..take]).await?;
                total += take as u64;
                written.store(total, Ordering::Relaxed);
                pending.drain(..take);
                remaining -= take as u64;
                if remaining == 0 {
                    while pending.len() < 2 {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        pending.extend_from_slice(&buf[..n]);
                    }
                    if pending.len() >= 2 {
                        pending.drain(..2);
                    }
                    need_size = true;
                }
                continue;
            }
            let want = pace.read_size(buf.len());
            let n = match s.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };
            pace.wait(n as u64).await;
            pending.extend_from_slice(&buf[..n]);
        }
        if !saw_end {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("chunked body ended after {total} bytes, before its final chunk"),
            ));
        }
        // A trailer section may follow the zero chunk; leaving it unread
        // would poison the socket, so it is not offered back.
        framed = pending.is_empty();
    } else if let Some(len) = stated {
        // The common case, and the only one that makes reuse worthwhile:
        // read exactly as many bytes as were promised and stop there.
        let take = (first.len() as u64).min(len) as usize;
        w.write_all(&first[..take]).await?;
        total = take as u64;
        written.store(total, Ordering::Relaxed);
        while total < len {
            if stopped() {
                w.flush().await?;
                return Err(interrupted());
            }
            let room = ((len - total) as usize).min(buf.len());
            let want = pace.read_size(room);
            match s.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => {
                    pace.wait(n as u64).await;
                    w.write_all(&buf[..n]).await?;
                    total += n as u64;
                    written.store(total, Ordering::Relaxed);
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        if total < len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("body ended at {total} of {len} bytes"),
            ));
        }
        // Exactly framed only if the header read did not ALSO pull in bytes
        // past the declared length. When it did, those bytes are still in
        // `head` and the socket's next response would begin with them —
        // precisely the silent corruption `crate::pool` refuses to risk.
        framed = first.len() as u64 <= len;
    } else {
        // No length and no chunking: the body ends when the socket does, so
        // there is nothing to hand on.
        w.write_all(first).await?;
        total += first.len() as u64;
        written.store(total, Ordering::Relaxed);
        loop {
            if stopped() {
                w.flush().await?;
                return Err(interrupted());
            }
            let want = pace.read_size(buf.len());
            match s.read(&mut buf[..want]).await {
                Ok(0) => break,
                Ok(n) => {
                    pace.wait(n as u64).await;
                    w.write_all(&buf[..n]).await?;
                    total += n as u64;
                    written.store(total, Ordering::Relaxed);
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }
        framed = false;
    }
    w.flush().await?;

    if let Some(p) = pool {
        if framed && !server_will_close {
            p.put(t, s);
        }
    }
    Ok(total)
}

pub async fn fetch_small<C: Connector>(c: &C, t: &Target, cap: usize) -> io::Result<Vec<u8>> {
    small_get(c, t, None, cap).await
}

/// Fetch bytes `lo..=hi` of an object into memory.
///
/// The ranged twin of [`fetch_small`], for reading a piece of a large object
/// without the object: an archive's index from its tail, say. Insists on a
/// `206`: a server that ignores `Range` and answers `200` would hand back the
/// START of the object, which a caller asking for the tail would then parse
/// as the tail. `cap` bounds what is read either way, so such a server costs
/// one bounded read rather than the whole object.
pub async fn fetch_small_range<C: Connector>(
    c: &C,
    t: &Target,
    lo: u64,
    hi: u64,
    cap: usize,
) -> io::Result<Vec<u8>> {
    small_get(c, t, Some((lo, hi)), cap).await
}

async fn small_get<C: Connector>(
    c: &C,
    t: &Target,
    range: Option<(u64, u64)>,
    cap: usize,
) -> io::Result<Vec<u8>> {
    let mut s = c.connect(t).await?;
    let req = build_request_head("GET", t, range);
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    // Stop at `Content-Length` rather than waiting for the server to close:
    // `Connection: close` is a request, and an origin that keeps the socket
    // open anyway would otherwise hold a complete response hostage to its
    // idle timeout.
    let mut body_end: Option<usize> = None;
    loop {
        let n = match s.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > cap + 8192 {
            return Err(io::Error::other("response exceeds the small-fetch cap"));
        }
        if body_end.is_none() {
            if let Some(start) = find_crlf2(&buf) {
                let head = String::from_utf8_lossy(&buf[..start]);
                // A `200` to a `Range` request is the WHOLE object on its way:
                // refuse it here, before reading any of it, rather than after
                // hitting the cap.
                let status: u16 = head
                    .split_whitespace()
                    .nth(1)
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                if range.is_some() && (200..300).contains(&status) && status != 206 {
                    return Err(io::Error::other(format!(
                        "server ignored the Range request and answered {status}"
                    )));
                }
                body_end = header_value(&head, "content-length")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .map(|len| start + len);
            }
        }
        if body_end.is_some_and(|end| buf.len() >= end) {
            break;
        }
    }
    let Some(start) = find_crlf2(&buf) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no header terminator",
        ));
    };
    let head = String::from_utf8_lossy(&buf[..start]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if (300..400).contains(&status) {
        // Redirects are the caller's to resolve — it owns the URL and the
        // hop budget — but it cannot resolve what it cannot see, so the
        // destination travels in the error rather than being swallowed into
        // "server returned 302".
        return Err(io::Error::other(Redirect {
            status,
            location: header_value(&head, "location").unwrap_or_default(),
        }));
    }
    if !(200..300).contains(&status) {
        return Err(io::Error::other(format!("server returned {status}")));
    }
    if range.is_some() && status != 206 {
        return Err(io::Error::other(format!(
            "server ignored the Range request and answered {status}"
        )));
    }
    let body = buf[start..].to_vec();
    if header_value(&head, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        return Ok(dechunk(&body));
    }
    Ok(body)
}

/// De-frame a complete chunked body already held in memory.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = find_crlf(rest) {
        let Ok(line) = std::str::from_utf8(&rest[..i]) else {
            break;
        };
        let Ok(n) = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16) else {
            break;
        };
        if n == 0 {
            break;
        }
        let start = i + 2;
        if start + n > rest.len() {
            out.extend_from_slice(&rest[start..]);
            break;
        }
        out.extend_from_slice(&rest[start..start + n]);
        rest = &rest[(start + n + 2).min(rest.len())..];
    }
    out
}

/// Probe an object with a ranged GET instead of a HEAD.
///
/// Necessary because HEAD is not universally supported in practice. Some servers
/// close the connection on HEAD without replying at all (observed on a public
/// speed-test host: HEAD returns zero bytes and an unclean TLS shutdown, while GET
/// on the same path answers `200` normally). Others answer HEAD with a redirect and
/// `Content-Length: 0`.
///
/// A `bytes=0-0` GET is the robust alternative: it costs one byte of body, it proves
/// range support rather than trusting an `Accept-Ranges` advertisement, and its
/// `Content-Range` carries the total size, effectively probing without needing a separate HEAD request.
/// Append whatever an error response has left to say onto `head`.
///
/// Bounded twice over, because this runs against a server that is already
/// misbehaving: by size, so an error PAGE cannot be pulled into memory to
/// produce one sentence, and by time, so a peer that sends headers and then
/// neither body nor close cannot stall the probe. Failing to read the body is
/// not an error — the status alone is still a usable answer.
async fn read_error_body<S: tokio::io::AsyncRead + Unpin>(s: &mut S, head: &mut Vec<u8>) {
    const CAP: usize = 8 * 1024;
    const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);
    let already = find_crlf2(head).map(|i| head.len().saturating_sub(i + 4));
    // A body that already arrived whole needs nothing; `error_detail` caps the
    // message far below this.
    if already.is_some_and(|n| n >= 512) {
        return;
    }
    let mut buf = vec![0u8; 2048];
    let _ = tokio::time::timeout(PATIENCE, async {
        while head.len() < CAP {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
    })
    .await;
}

pub async fn probe_via_get<C: Connector>(c: &C, t: &Target) -> io::Result<Probe> {
    let mut s = c.connect(t).await?;
    let req = build_request_head("GET", t, Some((0, 0)));
    s.write_all(req.as_bytes()).await?;

    let mut head = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        // A server that closes without TLS close_notify surfaces as UnexpectedEof.
        // If a complete header block already arrived, that is not a failure: the
        // response is in hand and the peer was merely impolite about hanging up.
        let n = match s.read(&mut buf).await {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && find_crlf2(&head).is_some() => {
                break
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if find_crlf2(&head).is_some() || head.len() > 64 * 1024 {
            break;
        }
    }
    let status: u16 = String::from_utf8_lossy(&head)
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    // The reason an error gives is in its body, and the body is not reliably in
    // the same read as the headers — Qumulo puts them in separate segments, so
    // the S3 `<Message>Request has expired</Message>` that explains a 403
    // arrived one packet after the loop above had already stopped. Only on an
    // error: on a success these bytes are the object, and pulling them here
    // would read part of the file and throw it away.
    if status >= 400 {
        read_error_body(&mut s, &mut head).await;
    }
    let h = String::from_utf8_lossy(&head);
    if (200..300).contains(&status) {
        reject_content_encoding(&h)?;
    }
    let cr = header_value(&h, "content-range");
    let ranges = status == 206 && cr.is_some();
    let size = match cr.as_deref().and_then(parse_content_range_total) {
        Some(n) => n,
        // A 200 to a range request means ranges are unsupported; the object length is
        // then the whole body.
        None => header_value(&h, "content-length")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0),
    };
    let (validator, weak, last_modified) =
        validator_from(header_value(&h, "etag"), header_value(&h, "last-modified"));
    Ok(Probe {
        size,
        ranges,
        last_modified,
        validator,
        weak_validator: weak,
        content_type: header_value(&h, "content-type"),
        disposition: header_value(&h, "content-disposition"),
        status,
        location: header_value(&h, "location"),
        raw_head: h.to_string(),
        raw_request: req,
    })
}

/// Probe an object, falling back to a ranged GET when HEAD gives nothing usable.
///
/// HEAD is the cheap question and the common case answers it. But it is optional in
/// practice, and the servers that refuse it do not refuse politely: `ash-speed.hetzner.com`
/// answers a HEAD by closing the connection with an empty reply, which [`probe`] reports
/// as a successful response carrying status 0, no length and no range support. Trusting
/// that reads as "an object of unknown size that cannot be resumed", which sends a
/// ten-gigabyte transfer down the single-stream path: no parallelism, no resume, and no
/// size to show. The object is perfectly ordinary — the same URL answers `bytes=0-0`
/// with `206`, its full length and a strong ETag.
///
/// So a HEAD that returns no length is treated as no answer rather than as an answer of
/// zero, and the ranged GET is asked instead. A redirect IS an answer and is returned as
/// it stands: the object is elsewhere, and this host has nothing to say about its bytes.
pub async fn probe_resilient<C: Connector>(c: &C, t: &Target) -> io::Result<Probe> {
    // A bot challenge is not a verdict, and one sample cannot tell the two
    // cases apart. AWS WAF re-challenges INTERMITTENTLY: the same client, with
    // the same accepted cookie, is waved through on one request and challenged
    // on the next — the browser's own script re-solves it so quietly that it
    // looks constant from outside. Failing on the first `202` therefore
    // reported "this link opens only in a browser" about a link that had just
    // delivered 122 MB, and made Resume unusable against such an origin.
    //
    // So ask again, a bounded number of times. A client that genuinely cannot
    // get in is challenged every time and still gets the clear refusal, about
    // a second later; one that hit a passing challenge gets its download.
    const TRIES: u32 = 3;
    let mut answer = probe_resilient_once(c, t).await;
    for attempt in 1..TRIES {
        match &answer {
            Ok(p) if p.bot_challenge() => {}
            _ => return answer,
        }
        // Jittered, because every connection of a stalled transfer re-probes at
        // once and a fixed wait would march them back into the WAF in step.
        let d = crate::polite::backoff_with_jitter(
            attempt,
            std::time::Duration::from_millis(300),
            std::time::Duration::from_secs(2),
            (t.port as u64).wrapping_mul(2_654_435_761) ^ attempt as u64,
        );
        tokio::time::sleep(d).await;
        answer = probe_resilient_once(c, t).await;
    }
    answer
}

async fn probe_resilient_once<C: Connector>(c: &C, t: &Target) -> io::Result<Probe> {
    // A HEAD that STATED a length has answered, even when the length is zero.
    //
    // `Probe::size` cannot tell "no Content-Length" from "Content-Length: 0" —
    // both are 0 — and the difference decides whether a second request is needed.
    // Reading the header back off the raw block is what keeps a genuinely empty
    // object (`/Testdateien/0B`) to one request, and it matters beyond the cost:
    // a ranged GET against a zero-length object is unsatisfiable, so servers
    // answer `bytes=0-0` with `416`, and a fallback that trusted the GET would
    // turn a perfectly good empty file into a failed download.
    let answered = |p: &Probe| {
        p.status >= 200 && p.status < 400 && (p.size > 0 || p.stated_length().is_some())
    };
    // An origin that accepts the request and then says nothing (`s7.uplod.ir`)
    // has no usable answer; the budget covers connect, TLS and the round trip.
    const HEAD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);
    let head = match tokio::time::timeout(HEAD_PATIENCE, probe(c, t)).await {
        Ok(r) => r,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "no response to HEAD",
        )),
    };
    match &head {
        // A redirect is a complete answer: the object is elsewhere, and asking
        // this host for its bytes only wastes a request.
        Ok(p) if p.is_redirect() => return head,
        Ok(p) if answered(p) => return head,
        _ => {}
    }
    // No length, an error status, or no reply at all. Ask the way that cannot be
    // refused: a `bytes=0-0` GET costs one byte of body, proves range support
    // rather than trusting an `Accept-Ranges` advertisement, and carries the total
    // in its `Content-Range`.
    let get = probe_via_get(c, t).await;
    match (head, get) {
        (_, Ok(g)) if g.status >= 200 && g.status < 400 => Ok(g),
        // The GET was refused where the HEAD was not. Keep the HEAD: this is the
        // 416-on-an-empty-object case, and a `404` learned from HEAD is also a
        // better error than "the ranged GET was unsatisfiable".
        (Ok(h), Ok(g)) => Ok(if h.status >= 200 && h.status < 400 {
            h
        } else {
            g
        }),
        (Ok(h), Err(_)) => Ok(h),
        (Err(_), Ok(g)) => Ok(g),
        (Err(head_err), Err(get_err)) => Err(io::Error::new(
            head_err.kind(),
            format!("HEAD failed ({head_err}), ranged GET failed ({get_err})"),
        )),
    }
}

/// Total object size from a one-byte range request.
///
/// Needed because a HEAD is allowed to omit `Content-Length`, and CDNs do: a HEAD
/// against a jsDelivr object returns `200` with no length at all. Without this
/// fallback the size reads as zero, and a downloader that trusts it "succeeds"
/// while writing an empty file — the worst possible failure, because it looks like
/// success. `Content-Range: bytes 0-0/533653` carries the number.
pub async fn probe_size_via_range<C: Connector>(c: &C, t: &Target) -> io::Result<u64> {
    let mut s = c.connect(t).await?;
    let req = build_request_head("GET", t, Some((0, 0)));
    s.write_all(req.as_bytes()).await?;
    let mut head = Vec::new();
    let mut buf = vec![0u8; 2048];
    while find_crlf2(&head).is_none() {
        let n = s.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > 32 * 1024 {
            break;
        }
    }
    let h = String::from_utf8_lossy(&head);
    header_value(&h, "content-range")
        .as_deref()
        .and_then(parse_content_range_total)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "no Content-Length on HEAD and no total in Content-Range",
            )
        })
}

/// Total length from a `Content-Range: bytes lo-hi/total` header.
pub(crate) fn parse_content_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

/// Decide the resume validator, its strength, and the object's date from the two
/// headers that carry them.
///
/// Returns `(validator, weak, last_modified)`. Shared by both probe paths, which
/// had made this decision independently — and a rule duplicated at two sites is a
/// rule that will eventually differ at two sites.
///
/// The validator prefers the ETag and falls back to the date, because resume and
/// mirror assembly are only sound against one of the two. The date is ALSO
/// returned unchanged, because `--remote-time` needs it whether or not an ETag
/// exists: an ETag is opaque and carries no time, so collapsing the two lost the
/// date on every server that sends both — which is most of them.
pub(crate) fn validator_from(
    etag: Option<String>,
    last_mod: Option<String>,
) -> (Option<String>, bool, Option<String>) {
    // A weak ETag (`W/"..."`) or a bare Last-Modified date is not strong enough to
    // justify assembling one file from several mirrors: RFC 9110 lets a weak
    // validator compare equal across representations that merely mean the same
    // thing, which is exactly what byte-range assembly must not tolerate.
    let weak = match (&etag, &last_mod) {
        (Some(e), _) => e.trim_start().starts_with("W/"),
        (None, Some(_)) => true,
        _ => false,
    };
    let validator = etag.or_else(|| last_mod.clone());
    (validator, weak, last_mod)
}

pub async fn probe<C: Connector>(c: &C, t: &Target) -> io::Result<Probe> {
    // Reuse an idle connection if this connector keeps a pool, and give it back
    // afterwards. A HEAD is the client's FIRST contact with the origin on nearly
    // every run, so its handshake is the one most worth keeping: before this, the
    // probe dialled, asked for `Connection: close`, and the transfer that started
    // moments later dialled the same host again. Measured on a live TLS path,
    // 1.6-2.0 s elapsed before the first byte of a transfer whose body took
    // 3.7-5.5 s.
    let pool = c.pool();
    let (mut s, reused) = match pool.as_ref().and_then(|p| p.take(t)) {
        Some(s) => (s, true),
        None => {
            if let Some(p) = pool.as_ref() {
                p.record_miss();
            }
            (c.connect(t).await?, false)
        }
    };
    // The extra headers and User-Agent are included so headers meant to influence
    // what the server returns (such as Authorization or Accept) shape the probe as well.
    //
    // Keep-alive so the connection survives to be pooled. A HEAD response has no
    // body by definition, so the connection is clean the moment its header block
    // ends — no draining, no risk of leaving unread bytes for the next request.
    let req = build_request_head_disp("HEAD", t, None, Disposition::Reuse);
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    // Read only until the header block ends, rather than to EOF.
    //
    // `read_to_end` was correct while every request said `Connection: close`, since
    // the server's close was the terminator. With keep-alive there is no close, so
    // reading to EOF would block until the origin's idle timeout — turning a
    // fast probe into a multi-second stall. A HEAD has no body, so CRLFCRLF is the
    // complete response.
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(e) = find_crlf2(&buf) {
            break Some(e);
        }
        match s.read(&mut chunk).await {
            Ok(0) => break find_crlf2(&buf),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A server may close without sending TLS `close_notify`, which rustls
            // reports as UnexpectedEof. That is a protocol impoliteness, not a
            // failed request: if the header block already arrived, the response is
            // in hand. Treating it as fatal made an otherwise-fine HEAD fail
            // outright, which is how a legitimate host came back as "every probe
            // failed".
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break find_crlf2(&buf),
            Err(e) => return Err(e),
        }
        if buf.len() > 64 * 1024 {
            break None;
        }
    };
    // Return the connection only when the response ended exactly where the protocol
    // says it should: a complete header block, nothing after it, and no error. The
    // same rule the transfer path uses — a connection whose framing is uncertain is
    // dropped, never reused, because the cost of getting that wrong is silent
    // corruption of the NEXT response.
    if let (Some(end), Some(p)) = (head_end, pool.as_ref()) {
        let hdr = String::from_utf8_lossy(&buf[..end]).to_string();
        let server_will_close = header_value(&hdr, "connection")
            .map(|v| v.to_ascii_lowercase().contains("close"))
            .unwrap_or(false);
        // `buf.len() == end` is the framing check: a HEAD has no body, so anything
        // past the header block means the server sent something this code does not
        // understand, and a connection whose framing is uncertain is dropped.
        if buf.len() == end && !server_will_close {
            p.put(t, s);
        }
    }
    let _ = reused;
    let raw_head = String::from_utf8_lossy(&buf).to_string();
    let head = raw_head.clone();
    // The status line is not decoration. A 302 carries `Content-Length: 0` and an
    // HTML body, so parsing its headers as object metadata yields "size 0" and the
    // caller cannot tell a redirect from a genuinely empty object.
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut location = None;
    let mut len = 0u64;
    let mut ranges = false;
    let mut etag = None;
    let mut last_mod = None;
    let mut ctype = None;
    let mut disposition = None;
    for line in head.lines() {
        let l = line.to_ascii_lowercase();
        if let Some(v) = l.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = l.strip_prefix("accept-ranges:") {
            ranges = v.trim() == "bytes";
        } else if l.starts_with("etag:") {
            etag = line.split_once(':').map(|(_, v)| v.trim().to_string());
        } else if l.starts_with("last-modified:") {
            last_mod = line.split_once(':').map(|(_, v)| v.trim().to_string());
        } else if l.starts_with("content-type:") {
            ctype = line.split_once(':').map(|(_, v)| v.trim().to_string());
        } else if l.starts_with("content-disposition:") {
            disposition = line.split_once(':').map(|(_, v)| v.trim().to_string());
        } else if l.starts_with("location:") {
            location = line.split_once(':').map(|(_, v)| v.trim().to_string());
        }
    }
    let (validator, weak, last_modified) = validator_from(etag, last_mod);
    Ok(Probe {
        size: len,
        ranges,
        validator,
        last_modified,
        weak_validator: weak,
        content_type: ctype,
        disposition,
        status,
        location,
        raw_head,
        raw_request: req,
    })
}

/// Fetch one byte range, streaming arrivals to `tx` and writing them to `sink`.
///
/// `pace` is the aggregate `--limit-rate` cap, shared across every connection of
/// the transfer. It is applied HERE, at the read, rather than at the sink: the
/// point of a rate cap is to slow what is taken off the wire, and shaping only
/// the writes would let the kernel receive buffer fill at full speed while the
/// user was told the transfer was capped.
/// Read until the end of the response header block.
///
/// `Ok(Some(n))` is the offset of the first body byte; `Ok(None)` means the peer
/// closed the connection first, which is a fact the caller must decide about
/// rather than an outcome this can classify on its own — on a pooled connection
/// it means the socket had gone stale, on a fresh one it means the request was
/// dropped.
async fn read_head<S>(s: &mut S, buf: &mut [u8], head: &mut Vec<u8>) -> io::Result<Option<usize>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    loop {
        let n = s.read(buf).await?;
        if n == 0 {
            return Ok(None);
        }
        head.extend_from_slice(&buf[..n]);
        if let Some(p) = find_crlf2(head) {
            return Ok(Some(p));
        }
        if head.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
    }
}

/// Fetch one range, following a redirect itself instead of reporting it.
///
/// [`fetch_range`] reports a redirect to its caller, and for the static-split
/// path that is right: the caller owns the hop budget. The SCHEDULER path had
/// no such caller — a redirect mid-transfer was a hard failure — and that is
/// what made a short-lived signed URL undownloadable in parallel.
///
/// The distinction that matters: an origin handing out a presigned URL is not
/// telling us the object moved, it is minting a credential, and it mints a
/// FRESH one for every request. `data.dtu.dk` signs for ten seconds. So the
/// durable address is the one the user gave, the redirect is how each request
/// is authorised, and the hop has to be followed per request rather than
/// written back over the target — writing it back caches a credential that is
/// dead before the next range is asked for, which is the bug this fixes.
///
/// Re-issuing the same range is safe: a redirect is decided from the status
/// line, before any body byte is read, so a hop delivers nothing to the sink
/// and reports no arrival. Nothing is double-counted and no range moves.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_range_following<C: Connector>(
    c: Arc<C>,
    conn: usize,
    t: Target,
    lo: u64,
    bound: crate::Watermark,
    sink: Arc<SparseSink>,
    tx: mpsc::UnboundedSender<Arrival>,
    t0: Instant,
    pace: Pace,
    pool: Option<crate::pool::SharedPool<C::Stream>>,
) -> io::Result<()> {
    let mut t = t;
    let mut chain = crate::polite::RedirectChain::new(&t.url());
    for _ in 0..crate::polite::MAX_REDIRECTS {
        let out = fetch_range(
            c.clone(),
            conn,
            t.clone(),
            lo,
            bound.clone(),
            sink.clone(),
            tx.clone(),
            t0,
            pace.clone(),
            pool.clone(),
        )
        .await;
        let e = match out {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => e,
            other => return other,
        };
        let msg = e.to_string();
        let loc = msg
            .strip_prefix("redirect:")
            .unwrap_or_default()
            .to_string();
        match retarget(&t, &loc) {
            Some(next) if chain.advance(&next.url()) => t = next,
            // A hop back to an address already asked for is a loop, and the
            // remaining budget cannot break it — only spend round trips and
            // then misreport the reason. See `polite::RedirectChain`.
            Some(next) => return Err(redirect_loop(&next.url())),
            // An unusable Location is a broken origin, not a hop worth
            // spending: report it as the protocol error it is.
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unusable redirect target: {loc}"),
                ))
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "redirect budget exhausted after {} hops",
            crate::polite::MAX_REDIRECTS
        ),
    ))
}

/// Extend a read of `n` bytes in `buf[..n]` with whatever is already readable,
/// up to `buf.len()`, without suspending.
///
/// rustls yields at most one TLS record (<= 16 KiB) per read, so without this
/// every record costs its own write, channel send and clock read. A real error
/// is returned alongside the bytes already gathered so the caller can credit
/// them first; EOF is left for the next read to report.
fn drain_ready<S: tokio::io::AsyncRead + Unpin>(
    s: &mut S,
    buf: &mut [u8],
    mut n: usize,
) -> (usize, Option<io::Error>) {
    // The caller's next real `.await` re-registers its waker before it can
    // sleep, so polling with a no-op waker here cannot lose a readiness edge.
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    while n < buf.len() {
        let mut rb = tokio::io::ReadBuf::new(&mut buf[n..]);
        match std::pin::Pin::new(&mut *s).poll_read(&mut cx, &mut rb) {
            std::task::Poll::Ready(Ok(())) => match rb.filled().len() {
                0 => break,
                m => n += m,
            },
            std::task::Poll::Ready(Err(e)) => return (n, Some(e)),
            std::task::Poll::Pending => break,
        }
    }
    (n, None)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_range<C: Connector>(
    c: Arc<C>,
    conn: usize,
    t: Target,
    lo: u64,
    bound: crate::Watermark,
    sink: Arc<SparseSink>,
    tx: mpsc::UnboundedSender<Arrival>,
    t0: Instant,
    pace: Pace,
    pool: Option<crate::pool::SharedPool<C::Stream>>,
) -> io::Result<()> {
    // The far end is re-read from `bound` as the body streams, never captured. A
    // repair that gives this connection's tail away lowers it, and the loop stops
    // there instead of racing the taker for the same bytes. See `Watermark`.
    let hi = bound.get();

    // Reuse an idle connection to this endpoint when one is available. This is the
    // difference between n ranges costing n handshakes and costing one; see
    // `crate::pool` for why it matters disproportionately to this client and for
    // the conditions under which a stream may go back.
    let reused = pool.as_ref().and_then(|p| p.take(&t));
    let from_pool = reused.is_some();
    let mut s = match reused {
        Some(s) => s,
        None => {
            if let Some(p) = pool.as_ref() {
                p.record_miss();
            }
            c.connect(&t).await?
        }
    };
    // A User-Agent identifying the client is basic etiquette: a mirror operator
    // seeing unexplained parallel range requests should be able to tell what is
    // making them. The builder sends it on every request.
    let disp = if pool.is_some() {
        Disposition::Reuse
    } else {
        Disposition::Close
    };
    let req = build_request_head_disp("GET", &t, Some((lo, hi.saturating_sub(1))), disp);

    // Read headers, then stream the body straight to its file offset.
    let mut buf = vec![0u8; READ_BUF];
    let mut head = Vec::new();
    let body_start;
    // ---- send the request, retrying ONCE on a fresh connection ---------------
    //
    // A pooled connection may have been closed by the peer while it sat idle, and
    // that is indistinguishable from a healthy one until the write or the first
    // read fails. Both halves must be handled, and it is the second one that
    // matters in the field: a peer that has sent FIN still ACCEPTS the request —
    // it lands in the send buffer and `write_all` returns success — so the close
    // only ever surfaces as EOF on the first READ.
    //
    // Treating that EOF as a completed fetch (which is what an early `Ok(())`
    // here does) hands the scheduler a connection that will never deliver a byte
    // and never report a failure, so the range is not re-requested until the
    // stall timeout expires — 4 to 45 s of dead air per occurrence, and the whole
    // transfer freezes for it once the endgame has concentrated the remaining
    // work onto one or two connections. Measured against an origin that closes
    // every keep-alive socket after one response: a 4 MiB transfer took 30.5 s
    // instead of 0.35 s, all of it spent waiting out stall timeouts.
    //
    // So a pooled connection that produced NOTHING is retried on a fresh one,
    // which costs a redial and is invisible; anything else is an error, and the
    // caller reclaims the range at once rather than waiting for silence to expire.
    let mut redialed = false;
    loop {
        // Anything that goes wrong before a header block has been parsed is
        // retryable on a pooled socket, whatever shape it took: a refused write,
        // a reset, a clean EOF, or bytes that are not a response at all because
        // the socket was carrying the tail of the previous one. A ranged GET is
        // idempotent and nothing has been written to the sink yet, so re-sending
        // it on a fresh connection cannot duplicate anything.
        let retryable = from_pool && !redialed;
        let dead = match s.write_all(req.as_bytes()).await {
            Ok(()) => match read_head(&mut s, &mut buf, &mut head).await {
                Ok(Some(p)) => {
                    body_start = p;
                    break;
                }
                Ok(None) if head.is_empty() => None,
                // Part of a header block, then silence.
                Ok(None) => Some(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed part-way through the response headers",
                )),
                Err(e) => Some(e),
            },
            Err(e) => Some(e),
        };
        if !retryable {
            return Err(dead.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before the response headers arrived",
                )
            }));
        }
        // Whatever it was — a refused write, a half-written request, a clean EOF
        // — the pooled socket is dead and the range is still perfectly fetchable.
        if let Some(p) = pool.as_ref() {
            p.record_miss();
        }
        s = c.connect(&t).await?;
        head.clear();
        redialed = true;
    }

    // ------------------------------------------------------------------
    // Validate that this response is the range we asked for.
    //
    // Found by real-network testing, and it is a CORRECTNESS bug rather than a
    // performance one: without this check, a `200 OK` full-body reply to a
    // `Range` request has its bytes written starting at `lo`, so the file is
    // silently corrupted and still passes every length check. It was observed
    // in the wild -- one baseline transfer out of 24 produced a file whose
    // digest differed from the other 23 while reporting success.
    //
    // Rules enforced here:
    //   * 206 must carry a Content-Range whose first byte equals `lo`;
    //   * 200 is only acceptable when we asked from byte 0, and then only for
    //     the whole object -- otherwise the bytes belong at offsets we did not
    //     request and MUST NOT be written;
    //   * anything else (3xx/4xx/5xx) is an error, not a body.
    let head_str = String::from_utf8_lossy(&head[..body_start]);
    let status: u16 = head_str
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let content_range = header_value(&head_str, "content-range");
    if matches!(status, 200 | 206) {
        reject_content_encoding(&head_str)?;
    }
    match status {
        206 => {
            let first = content_range
                .as_deref()
                .and_then(parse_content_range_start)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "206 without a parsable Content-Range",
                    )
                })?;
            if first != lo {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("206 Content-Range starts at {first}, requested {lo}"),
                ));
            }
        }
        200 => {
            if lo != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("server ignored Range and sent 200 for a request from offset {lo}"),
                ));
            }
        }
        // Admission control: the server is telling us to slow down. Surface the
        // requested delay so the caller can honour it rather than retrying
        // immediately, which is how a client earns a ban.
        429 | 503 => {
            let ra = header_value(&head_str, "retry-after")
                .and_then(|v| crate::polite::parse_retry_after(&v, unix_now()))
                .unwrap_or(std::time::Duration::from_secs(1));
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "{status} throttled, retry after {}s",
                    ra.as_secs_f64().round()
                ),
            ));
        }
        // A bot challenge is the origin declining THIS request — not a verdict
        // on the object. AWS WAF answers `202` with no body and its own header,
        // and the very next request with the same cookie is let through: a
        // transfer that has already delivered bytes has proved it can get in.
        //
        // So it is retryable, like a 429. Treating it as a protocol error threw
        // away a completed 64 MiB transfer because one late request happened to
        // be challenged. A client that can NEVER get in is caught earlier and
        // more clearly, by `Probe::refusal` on the first request.
        202 if header_value(&head_str, "x-amzn-waf-action").is_some() => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "202 bot challenge, retry after 1s",
            ));
        }
        // Redirects are followed by the caller, which owns the hop budget: a
        // redirect loop must cost a bounded number of `delta`, not unbounded.
        301 | 302 | 303 | 307 | 308 => {
            let loc = header_value(&head_str, "location").unwrap_or_default();
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!("redirect:{loc}"),
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected status {other} for a range request"),
            ));
        }
    }

    // A chunked body carries size lines interleaved with the data. Writing the raw
    // stream to disk would embed those lines IN the file, so they must be stripped.
    // jsDelivr answers range requests this way, which is legal: RFC 9112 allows
    // chunked with 206.
    let chunked = header_value(&head_str, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    if chunked {
        return stream_chunked(s, conn, lo, bound, &head[body_start..], sink, tx, t0, pace).await;
    }

    let mut off = lo;
    let mut last = Instant::now();
    let first_body = &head[body_start..];
    // Body bytes that came in with the headers and are past the far end we asked
    // for. There should never be any — a 206's body is exactly the range — so
    // their presence means the response is not framed the way this code believes,
    // and the socket must not be offered for reuse.
    let mut over_read = false;
    if !first_body.is_empty() {
        let take = first_body.len().min((hi - off) as usize);
        over_read = first_body.len() > take;
        // These bytes arrived alongside the headers and are as real as any other:
        // they are charged against the cap too, or a small object could be
        // delivered entirely unshaped in the header read.
        pace.wait(take as u64).await;
        sink.write_at(off, &first_body[..take])?;
        let at_off = off;
        off += take as u64;
        let now = Instant::now();
        let _ = tx.send(Arrival {
            conn,
            off: at_off,
            bytes: take as u64,
            at: now.duration_since(t0).as_secs_f64(),
            dt: now.duration_since(last).as_secs_f64().max(1e-6),
        });
        last = now;
    }

    // Re-read the bound each pass: `hi` is only its value at request time, and it
    // must STAY that, because it is what "the far end moved" is measured against
    // once the body is done. The live cursor is a separate binding.
    //
    // Shadowing it with `let mut hi = hi` — which is what this was — silently
    // deleted the pool's shrink guard below: by the time it ran, `hi` had been
    // reassigned to the current watermark on every read, so `hi_final < hi` was
    // false however far the far end had moved. A preempted connection was
    // therefore pooled with the server still streaming the tail it had been
    // relieved of, and the next request to reuse that socket read that tail
    // instead of its own response headers — 64 KiB of body, no header block, and
    // a "header too large" failure on a perfectly healthy endpoint. Repairs
    // cluster in the endgame, so this fired hardest exactly where users saw it.
    let mut cur_hi = hi;
    let mut deferred: Option<io::Error> = None;
    while off < cur_hi {
        if let Some(e) = deferred.take() {
            return Err(e);
        }
        let want = pace.read_size(((cur_hi - off) as usize).min(READ_BUF));
        let n = s.read(&mut buf[..want]).await?;
        if n == 0 {
            break;
        }
        let (n, err) = drain_ready(&mut s, &mut buf[..want], n);
        deferred = err;
        // A repair may have moved the far end while this read was parked. Re-read
        // before crediting, and clamp: bytes past the new bound belong to whichever
        // connection was given them, so writing them here would double-count
        // coverage and reintroduce the duplicate-span cost this exists to remove.
        cur_hi = bound.get();
        let n = n.min(cur_hi.saturating_sub(off) as usize);
        if n == 0 {
            break;
        }
        // Pay for the bytes just taken off the wire, before doing anything with
        // them. Charging after the write would let a burst land first and shape
        // only the next read, which is the difference between a cap and an
        // average.
        pace.wait(n as u64).await;
        sink.write_at(off, &buf[..n])?;
        let at_off = off;
        off += n as u64;
        let now = Instant::now();
        let _ = tx.send(Arrival {
            conn,
            off: at_off,
            bytes: n as u64,
            at: now.duration_since(t0).as_secs_f64(),
            dt: now.duration_since(last).as_secs_f64().max(1e-6),
        });
        last = now;
    }
    let hi_final = bound.get();
    let shrunk = hi_final < hi;
    let server_will_close = header_value(&head_str, "connection")
        .map(|v| v.to_ascii_lowercase().contains("close"))
        .unwrap_or(false);
    if let Some(p) = pool.as_ref() {
        if may_pool(status, over_read, server_will_close, shrunk, off, hi_final) {
            p.put(&t, s);
        }
    }

    // A shrink is not a truncation: the bound moved below where the loop
    // stopped because the remainder was handed to another connection. The
    // peer closing early IS a truncation, and `UnexpectedEof` is retryable:
    // returning Ok here once let a short read pass as a delivered range.
    let hi = hi_final;
    if off < hi {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "connection closed after {} of {} bytes of the requested range",
                off - lo,
                hi - lo
            ),
        ));
    }
    Ok(())
}

/// Fetch one range with bounded retries, writing to `sink`. This is the
/// baseline transport used by the static-split policies: a part is retried in
/// place on its own connection, which is what "never reassign" means, and it is
/// why a permanently dead mirror strands that part.
/// Stream a chunked body to its file offsets, stripping the chunk framing.
///
/// A chunked body is a sequence of `<hex-size>\r\n<size bytes>\r\n`, terminated by a
/// zero-size chunk. The framing bytes are transport, not content: writing them to
/// disk corrupts the file in a way no length check catches, because the byte count
/// still looks plausible.
///
/// Written as an explicit state machine over a rolling buffer rather than with a
/// line-reader, because a chunk boundary can land anywhere inside a read — the
/// hex size line can be split across two reads, and so can the trailing CRLF.
///
/// # Why the buffer is a cursor and not a `Vec` you `drain`
///
/// The obvious implementation — `buf.windows(2).position(|w| w == b"\r\n")` to
/// find a size line, `buf.drain(..n)` to consume a token — is quadratic in two
/// separate ways, and both are per-read costs on the byte path:
///
/// * `windows(2).position` restarts at offset 0 of the buffer on every call, so
///   a size line that has not arrived yet costs a full rescan of everything
///   buffered, once per read, until it does.
/// * `drain(..n)` memmoves the entire residual tail down to offset 0 on every
///   token consumed — and a chunked body has three tokens per chunk.
///
/// Measured on an 8 MiB body with 64 KiB reads: 1.01 GiB/s at 1 KiB chunks
/// against 59.5 GiB/s at 1 MiB chunks. Since the payload is identical, that 59x
/// spread is entirely framing overhead — the small-chunk case pays it thousands
/// of times. Servers that stream 1-4 KiB chunks are common (nginx proxying a
/// dynamic backend, jsDelivr), so this is the realistic case, not the corner.
///
/// The rewrite keeps one `Vec` and a `head` cursor marking consumed bytes.
/// Consuming is `head += n`, an integer add. Space is reclaimed by compacting
/// only when the consumed prefix is worth reclaiming, so the memmove cost is
/// amortized to O(1) per byte instead of O(1) per token. The CRLF search
/// resumes from `scan`, so no byte is examined twice across reads, and it uses
/// `memchr` to find the `\r` rather than stepping a two-byte window.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_chunked<S>(
    mut s: S,
    conn: usize,
    lo: u64,
    bound: crate::Watermark,
    initial: &[u8],
    sink: Arc<SparseSink>,
    tx: mpsc::UnboundedSender<Arrival>,
    t0: Instant,
    pace: Pace,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Same discipline as the plain-body path: the far end can move under us.
    let mut hi = bound.get();
    enum St {
        Size,
        Data(u64),
        AfterData,
        Done,
    }
    let mut buf = framebuf::FrameBuf::with_initial(initial);
    let mut state = St::Size;
    let mut off = lo;
    let mut last = Instant::now();
    let mut read_buf = vec![0u8; READ_BUF];
    let mut deferred: Option<io::Error> = None;
    // Contiguous run of decoded payload not yet reported to the scheduler.
    // Flushed once per read, and before returning, so no bytes go uncredited.
    let mut pend_off = lo;
    let mut pend_bytes = 0u64;
    macro_rules! flush_pending {
        () => {
            if pend_bytes > 0 {
                let now = Instant::now();
                let _ = tx.send(Arrival {
                    conn,
                    off: pend_off,
                    bytes: pend_bytes,
                    at: now.duration_since(t0).as_secs_f64(),
                    dt: now.duration_since(last).as_secs_f64().max(1e-6),
                });
                // Both are dead stores on the final expansion (the one before a
                // `return`), which is what the unused-assignment warning reports.
                // They are load-bearing on every other expansion, so the write
                // stays and the lint is silenced at the site.
                #[allow(unused_assignments)]
                {
                    last = now;
                    pend_bytes = 0;
                }
            }
        };
    }

    loop {
        loop {
            match &mut state {
                St::Size => {
                    let Some(nl) = buf.find_crlf() else {
                        break;
                    };
                    // A chunk-extension may follow a semicolon; the size is before it.
                    // Parsed from bytes rather than through `String::from_utf8_lossy`,
                    // which allocated a String per chunk purely to read a hex number.
                    let line = &buf.data()[..nl];
                    let hex_end = line.iter().position(|&b| b == b';').unwrap_or(line.len());
                    let hex = trim_ascii(&line[..hex_end]);
                    let size = parse_hex_u64(hex).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "bad chunk size line {:?}",
                                String::from_utf8_lossy(&line[..line.len().min(64)])
                            ),
                        )
                    })?;
                    buf.consume(nl + 2);
                    state = if size == 0 { St::Done } else { St::Data(size) };
                }
                St::Data(remaining) => {
                    if buf.is_empty() {
                        break;
                    }
                    let take = (*remaining as usize).min(buf.len());
                    // Never write past the requested range: a server may answer a
                    // range request with more than was asked for.
                    let room = (hi.saturating_sub(off)) as usize;
                    let write = take.min(room);
                    if write > 0 {
                        sink.write_at(off, &buf.data()[..write])?;
                        // Accumulate rather than sending one Arrival per chunk.
                        //
                        // Every send allocates a node in the unbounded channel, and
                        // a chunked body has one chunk per few KiB: measured at
                        // 1 KiB chunks, that was 1027 allocations per MiB
                        // transferred, against 65 at 16 KiB — i.e. one allocation
                        // per chunk, entirely from this send. Consecutive chunks are
                        // contiguous in file offset, so the run [pend_off, off) is
                        // describable by a single Arrival and the scheduler credits
                        // it identically: `on_bytes_at` advances one cursor by the
                        // byte count and cares only that the run starts at that
                        // cursor. Flushed once per read below, so the scheduler
                        // still sees progress at read granularity — the rate
                        // sampler already accumulates over a 0.2s window, so it
                        // loses no fidelity it was using.
                        if pend_bytes == 0 {
                            pend_off = off;
                        }
                        pend_bytes += write as u64;
                        off += write as u64;
                    }
                    buf.consume(take);
                    *remaining -= take as u64;
                    if *remaining == 0 {
                        state = St::AfterData;
                    }
                }
                St::AfterData => {
                    if buf.len() < 2 {
                        break;
                    }
                    if &buf.data()[..2] != b"\r\n" {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "chunk not terminated by CRLF",
                        ));
                    }
                    buf.consume(2);
                    state = St::Size;
                }
                St::Done => {
                    flush_pending!();
                    return Ok(());
                }
            }
        }
        // One Arrival per read, covering every chunk decoded from it.
        flush_pending!();
        // Pick up a repair that lowered the far end. The `room` clamp in the
        // `St::Data` arm above then stops writing at the new boundary, and the
        // early-exit below ends the stream rather than draining a body whose tail
        // now belongs to another connection.
        hi = bound.get();
        if off >= hi {
            return Ok(());
        }
        // Shape the same way the unframed path does: cap the read size so one
        // pause stays short, then pay for the bytes as they come off the wire.
        // The charge is against RAW bytes read, framing included — that is what
        // crossed the link, and a cap that only counted payload would exceed the
        // configured rate on a body with many small chunks.
        let want = pace.read_size(read_buf.len());
        if let Some(e) = deferred.take() {
            return Err(e);
        }
        let mut n = s.read(&mut read_buf[..want]).await?;
        if n > 0 {
            (n, deferred) = drain_ready(&mut s, &mut read_buf[..want], n);
        }
        if n > 0 {
            pace.wait(n as u64).await;
        }
        if n == 0 {
            // A truncated chunked body is an error, not a completed transfer.
            // Pending bytes are still real bytes that reached the sink, so they
            // are credited before reporting the failure: the retry must re-request
            // only what genuinely never arrived.
            flush_pending!();
            return match state {
                St::Done => Ok(()),
                _ => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "chunked body ended early: {} of {} bytes",
                        off - lo,
                        hi - lo
                    ),
                )),
            };
        }
        buf.extend(&read_buf[..n]);
    }
}

/// Trim ASCII whitespace from both ends of a byte slice.
///
/// `[u8]::trim_ascii` exists on recent toolchains; this is spelled out so the
/// crate does not acquire an MSRV requirement for one call site.
#[inline]
fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while let [f, rest @ ..] = b {
        if f.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., l] = b {
        if l.is_ascii_whitespace() {
            b = rest;
        } else {
            break;
        }
    }
    b
}

/// Parse a hex chunk-size directly from bytes.
///
/// Replaces `u64::from_str_radix` on a `String::from_utf8_lossy` copy, which
/// allocated once per chunk to read at most 16 digits. Returns `None` for an
/// empty field, a non-hex byte, or a value that would overflow — all of which
/// the caller reports as a malformed size line rather than guessing.
#[inline]
fn parse_hex_u64(b: &[u8]) -> Option<u64> {
    if b.is_empty() || b.len() > 16 {
        return None;
    }
    let mut v: u64 = 0;
    for &c in b {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        v = (v << 4) | d as u64;
    }
    Some(v)
}

pub async fn fetch_range_retry<C: Connector>(
    c: Arc<C>,
    t: Target,
    lo: u64,
    hi: u64,
    sink: Arc<SparseSink>,
    max_tries: u32,
    per_try_timeout_s: f64,
) -> io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Arrival>();
    let t0 = Instant::now();
    let mut off = lo;
    let mut t = t;
    let mut chain = crate::polite::RedirectChain::new(&t.url());
    let mut redirects: u32 = 0;
    for attempt in 0..max_tries {
        if off >= hi {
            return Ok(());
        }
        let fut = fetch_range(
            c.clone(),
            0,
            t.clone(),
            off,
            // No scheduler above this entry point, so nothing ever moves the far
            // end: the static-split policies mean "never reassign" literally.
            crate::Watermark::fixed(hi),
            sink.clone(),
            tx.clone(),
            t0,
            // This entry point carries no cap parameter, so it is explicitly
            // unshaped rather than silently inheriting one.
            Pace::unlimited(),
            // No pool: this is the static-split transport, one range per
            // connection with retries in place, so there is no second request to
            // the same endpoint for a pooled connection to serve.
            None,
        );
        tokio::pin!(fut);
        loop {
            let step = tokio::time::timeout(
                tokio::time::Duration::from_secs_f64(per_try_timeout_s),
                async {
                    tokio::select! {
                        r = &mut fut => Some(Ok(r)),
                        Some(a) = rx.recv() => Some(Err(a)),
                    }
                },
            )
            .await;
            match step {
                Err(_) => break, // stalled: retry from `off`
                Ok(Some(Err(a))) => off = off.max(a.off + a.bytes),
                // The future COMPLETED -- but completing is not succeeding. An
                // earlier version returned Ok here unconditionally, so a
                // protocol error (a 200 to a mid-object Range, a Content-Range
                // starting at the wrong offset) was reported as a successful
                // fetch and the caller wrote a corrupt file. Propagate the
                // inner result.
                Ok(Some(Ok(inner))) => match inner {
                    Ok(()) => return Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                        // A server that ignores Range or mislabels its offsets
                        // will do so again: retrying cannot help, and retrying
                        // is how a client ends up hammering a broken mirror.
                        return Err(e);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // 429/503 with a Retry-After. Honour it, with jitter so
                        // several connections do not resynchronise onto the same
                        // instant and hammer the server in waves.
                        let secs = parse_secs_from(&e.to_string()).unwrap_or(1.0);
                        let d = crate::polite::backoff_with_jitter(
                            attempt,
                            std::time::Duration::from_secs_f64(secs.max(0.05)),
                            std::time::Duration::from_secs(60),
                            (lo ^ attempt as u64).wrapping_mul(2_654_435_761),
                        );
                        tokio::time::sleep(d).await;
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotConnected => {
                        // A redirect. The hop budget is bounded so a loop costs a
                        // bounded number of `delta` rather than spinning forever.
                        let msg = e.to_string();
                        let loc = msg.strip_prefix("redirect:").unwrap_or("").to_string();
                        if redirects >= crate::polite::MAX_REDIRECTS || loc.is_empty() {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("redirect budget exhausted after {redirects} hops"),
                            ));
                        }
                        match retarget(&t, &loc) {
                            Some(next) if !chain.advance(&next.url()) => {
                                return Err(redirect_loop(&next.url()))
                            }
                            Some(next) => {
                                t = next;
                                redirects += 1;
                                break;
                            }
                            None => {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!("unusable redirect target: {loc}"),
                                ))
                            }
                        }
                    }
                    Err(_) => break, // transient: retry from `off`
                },
                Ok(None) => break,
            }
        }
        while let Ok(a) = rx.try_recv() {
            off = off.max(a.off + a.bytes);
        }
    }
    if off >= hi {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "part never completed",
        ))
    }
}

/// Seconds a throttle error asks the caller to wait.
///
/// The `Retry-After` a 429/503 carried travels as text inside the error, because
/// `io::Error` is the only channel the fetch path has. Parsing it back is how
/// both callers — the retrying transport and the scheduler-driven one — honour a
/// server that has told them exactly how long to stand down.
pub(crate) fn retry_after_secs(e: &io::Error) -> Option<f64> {
    parse_secs_from(&e.to_string())
}

/// Seconds embedded in a throttle error message.
fn parse_secs_from(msg: &str) -> Option<f64> {
    msg.split("retry after ")
        .nth(1)?
        .split('s')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Rewrite a target for a redirect `Location`, preserving proxy routing.
///
/// Only plaintext HTTP is followed. An `https://` target is refused rather than
/// silently downgraded: TLS is not implemented in this transport, and pretending
/// otherwise would corrupt a transfer.
/// Split an authority into host and port, defaulting the port for the scheme.
///
/// Bracket-aware, because `rsplit_once(':')` is actively wrong on an IPv6
/// literal: `[2001:db8::1]` has no port and would be cut at its last colon.
fn split_authority(auth: &str, default_port: u16) -> Option<(String, u16)> {
    if auth.is_empty() {
        return None;
    }
    if let Some(rest) = auth.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }
    match auth.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((auth.to_string(), default_port)),
    }
}

/// The error a chain reports when a `Location` names an address it has already
/// asked for.
///
/// Distinct from the budget being spent, and deliberately worded so: a loop is
/// the origin declining the request, and "too many redirects" sends the reader
/// looking for a chain that is too long instead of one that never moves.
fn redirect_loop(location: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("redirect loop: {location} was already requested"),
    )
}

/// The location stays on the origin that issued it: same scheme, host and port.
///
/// Judged on the ORIGIN endpoint, never the socket peer, so a proxied chain is
/// measured by where the bytes come from rather than by which proxy carries
/// them. Scheme counts (RFC 6454 §4): an `http` hop is not the `https` origin
/// that set the cookie, however alike the two addresses read.
/// The target a `Location` names, relative to the one that produced it.
///
/// `https` is followed, not refused. It used to be: an absolute TLS `Location`
/// returned `None`, which reads as "this path cannot do TLS" and was true only
/// of the connector the helper was first written for. Every connector that
/// carries a real transfer honours `Target::tls`, and refusing meant an origin
/// that redirects to a signed URL on another host — the ordinary shape of every
/// object store — reported `unusable redirect target` and stopped, naming a URL
/// that was perfectly good.
///
/// Headers and the agent travel only to the SAME origin. Replaying a
/// `Cookie:` at whatever host a redirect names is how a session token leaks to
/// a third party, so a hop that changes scheme, host or port drops both. The
/// test used to be absolute-versus-relative, which is not the same question: a
/// mirror that answers with its own address in full form is still the origin
/// that was asked, and dropping the headers there discarded the one thing the
/// user had supplied to get in — the documented workaround for a site that
/// needs a cookie is `-H 'Cookie: ...'`, and an absolute same-origin hop threw
/// it away.
fn retarget(prev: &Target, location: &str) -> Option<Target> {
    let loc = location.trim();
    let absolute = |tls: bool, rest: &str| {
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = split_authority(auth, if tls { 443 } else { 80 })?;
        let next = match &prev.origin {
            // Through a forward proxy the request stays in absolute form and the
            // proxy does the reaching, so the authority travels as text — and so
            // does the scheme, which `via_proxy` alone would drop, turning a
            // proxied `https` chain into cleartext at its first hop.
            Some(_) => Target {
                tls,
                ..Target::via_proxy(&prev.host, prev.port, auth, path)
            },
            None if tls => Target::direct_tls(&host, port, path),
            None => Target::direct(&host, port, path),
        };
        // One rule for what a hop may carry, shared with the probe chains: the
        // agent and ordinary headers always, the origin's credentials only
        // while the chain is still on it.
        Some(next.with_headers_from(prev, prev.headers.clone(), prev.agent.clone()))
    };
    if let Some(rest) = loc.strip_prefix("https://") {
        return absolute(true, rest);
    }
    if let Some(rest) = loc.strip_prefix("http://") {
        return absolute(false, rest);
    }
    if loc.starts_with('/') {
        let mut next = prev.clone();
        next.path = loc.to_string();
        return Some(next);
    }
    None
}

/// Wall-clock seconds since the epoch, for `Retry-After` HTTP-date arithmetic.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Case-insensitive header lookup over a raw header block.
/// Value of a named header in a raw response head, case-insensitively.
///
/// Public so the CLI can answer a `-H NAME` query from a probe it already made,
/// rather than reimplementing header parsing or issuing a second request.
pub fn header_lookup(head: &str, name: &str) -> Option<String> {
    header_value(head, name)
}

/// Every value of a named header in a raw response head, in the order sent.
///
/// Separate from [`header_lookup`] because `Set-Cookie` is the one header a
/// server routinely repeats, and a lookup that answers with the first would
/// silently drop every session cookie after it.
pub fn header_all(head: &str, name: &str) -> Vec<String> {
    head.split("\r\n")
        .skip(1)
        .filter(|l| crate::is_field(l, name))
        .map(|l| l[name.len() + 1..].trim().to_string())
        .collect()
}

pub(crate) fn header_value(head: &str, name: &str) -> Option<String> {
    head.split("\r\n")
        .skip(1)
        .find(|l| {
            l.len() > name.len()
                && l.as_bytes()[name.len()] == b':'
                && l[..name.len()].eq_ignore_ascii_case(name)
        })
        .map(|l| l[name.len() + 1..].trim().to_string())
}

/// Whether a range connection can go back to the pool: only when the client
/// knows exactly where the response ended, because anything left unread in
/// the socket becomes the first bytes of the next response. `Connection:
/// close`, a body that stopped short of the far end, a far end moved by a
/// repair (the server is still sending toward the old one), or a `200`
/// whose body is the whole object all leave bytes behind.
fn may_pool(
    status: u16,
    over_read: bool,
    server_will_close: bool,
    shrunk: bool,
    off: u64,
    hi_final: u64,
) -> bool {
    status == 206 && !over_read && !server_will_close && !shrunk && off >= hi_final
}

/// First byte position of a verbatim `Range: bytes=<first>-<last>` header line.
fn range_header_start(line: &str) -> Option<u64> {
    if !crate::is_field(line, "range") {
        return None;
    }
    let spec = line["range:".len()..].trim().strip_prefix("bytes=")?;
    spec.split('-').next()?.trim().parse().ok()
}

/// First byte position of a `Content-Range: bytes <first>-<last>/<len>` header.
fn parse_content_range_start(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let first = rest.split('-').next()?.trim();
    first.parse().ok()
}

/// Offset of the first CRLF, or None. Used for chunk-size lines.
///
/// `memchr` for the `\r` rather than a two-byte `windows` walk: the scan is
/// vectorized with runtime dispatch (AVX2/SSE2 on x86-64, NEON on aarch64) and
/// falls back to a scalar loop on targets with neither, so the same source is
/// correct everywhere and fast where the hardware allows.
pub(crate) fn find_crlf(b: &[u8]) -> Option<usize> {
    let mut from = 0usize;
    while let Some(i) = memchr::memchr(b'\r', &b[from..]) {
        let at = from + i;
        match b.get(at + 1) {
            None => return None,
            Some(&b'\n') => return Some(at),
            // A bare `\r`: not a boundary, keep looking past it.
            Some(_) => from = at + 1,
        }
    }
    None
}

/// Offset just past the blank line that ends a header block.
///
/// Called once per read while the response head accumulates, so a full rescan
/// per call is quadratic in the number of reads the head takes to arrive. This
/// is bounded by the head size rather than the object size, which is why it is
/// merely wasteful and not pathological — but the vectorized form costs nothing
/// extra. `memchr` locates candidate `\r` bytes; only positions that could start
/// `\r\n\r\n` are compared.
pub(crate) fn find_crlf2(b: &[u8]) -> Option<usize> {
    if b.len() < 4 {
        return None;
    }
    let mut from = 0usize;
    while from + 4 <= b.len() {
        let i = memchr::memchr(b'\r', &b[from..b.len() - 3])?;
        let at = from + i;
        if &b[at..at + 4] == b"\r\n\r\n" {
            return Some(at + 4);
        }
        from = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands out scripted reads: `Some(bytes)` is one readable chunk, `None` is
    /// "not ready", and an `Err` is a transport failure.
    struct Script(std::collections::VecDeque<io::Result<Option<Vec<u8>>>>);

    impl tokio::io::AsyncRead for Script {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            rb: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            match self.0.pop_front() {
                None | Some(Ok(None)) => std::task::Poll::Pending,
                Some(Err(e)) => std::task::Poll::Ready(Err(e)),
                Some(Ok(Some(b))) => {
                    let n = b.len().min(rb.remaining());
                    rb.put_slice(&b[..n]);
                    std::task::Poll::Ready(Ok(()))
                }
            }
        }
    }

    fn script(items: Vec<io::Result<Option<Vec<u8>>>>) -> Script {
        Script(items.into())
    }

    #[test]
    fn drain_ready_gathers_every_record_already_readable() {
        let mut s = script(vec![
            Ok(Some(vec![2; 4])),
            Ok(Some(vec![3; 4])),
            Ok(None),
            Ok(Some(vec![9; 4])),
        ]);
        let mut buf = [0u8; 32];
        buf[..4].copy_from_slice(&[1; 4]);
        let (n, err) = drain_ready(&mut s, &mut buf, 4);
        assert!(err.is_none());
        assert_eq!(n, 12, "stops at the first read that would block");
        assert_eq!(&buf[..12], &[1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
    }

    #[test]
    fn drain_ready_never_reads_past_the_buffer_it_was_given() {
        let mut s = script(vec![Ok(Some(vec![7; 100]))]);
        let mut buf = [0u8; 10];
        let (n, err) = drain_ready(&mut s, &mut buf[..6], 4);
        assert!(err.is_none());
        assert_eq!(n, 6);
        assert_eq!(&buf[6..], &[0; 4]);
    }

    #[test]
    fn drain_ready_does_nothing_when_the_buffer_is_already_full() {
        let mut s = script(vec![Ok(Some(vec![7; 4]))]);
        let mut buf = [0u8; 4];
        let (n, err) = drain_ready(&mut s, &mut buf, 4);
        assert!(err.is_none());
        assert_eq!(n, 4);
        assert_eq!(s.0.len(), 1, "the unread record must stay queued");
    }

    #[test]
    fn drain_ready_leaves_eof_for_the_next_read() {
        let mut s = script(vec![Ok(Some(vec![])), Ok(Some(vec![5; 4]))]);
        let mut buf = [0u8; 16];
        let (n, err) = drain_ready(&mut s, &mut buf, 2);
        assert!(err.is_none());
        assert_eq!(n, 2);
    }

    /// A reset seen mid-drain must not turn into a clean end of stream: the
    /// bytes gathered so far are returned and the error travels with them.
    #[test]
    fn drain_ready_returns_the_error_with_the_bytes_before_it() {
        let mut s = script(vec![
            Ok(Some(vec![2; 4])),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
        ]);
        let mut buf = [0u8; 16];
        let (n, err) = drain_ready(&mut s, &mut buf, 4);
        assert_eq!(n, 8);
        assert_eq!(err.unwrap().kind(), io::ErrorKind::ConnectionReset);
    }

    /// A 3xx has to come back as something a caller can ACT on.
    ///
    /// Four call sites follow these hops; they used to recover the
    /// destination by parsing it out of the message text, so rewording the
    /// message would have silently turned every redirecting CDN into a hard
    /// failure. This pins the recovery to the type instead.
    #[test]
    fn a_redirect_error_carries_its_destination() {
        let e = io::Error::other(Redirect {
            status: 302,
            location: "https://edge.example/seg1.ts".into(),
        });

        let r = Redirect::of(&e).expect("a redirect must be recoverable from the error");
        assert_eq!(r.status, 302);
        assert_eq!(r.location, "https://edge.example/seg1.ts");

        // And an error that is NOT a redirect must not masquerade as one,
        // however much its text looks the part.
        let other = io::Error::other("redirect 302 to https://spoof.example/");
        assert!(Redirect::of(&other).is_none());
    }

    /// One origin, three objects: the second and third must ride the socket
    /// the first opened. That is the whole point of `fetch_object` — a
    /// hundred-segment playlist should cost one handshake, not a hundred.
    #[tokio::test]
    async fn fetch_object_reuses_one_connection_for_many_objects() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let opened = Arc::new(AtomicU64::new(0));
        let served = Arc::new(AtomicU64::new(0));
        {
            let (opened, served) = (opened.clone(), served.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(mut sock) = conn else { continue };
                    opened.fetch_add(1, Ordering::SeqCst);
                    let Ok(peek) = sock.try_clone() else { continue };
                    let mut r = BufReader::new(peek);
                    // Keep answering on the SAME socket until the peer goes.
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 || line.is_empty() {
                            break;
                        }
                        loop {
                            let mut h = String::new();
                            if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                                break;
                            }
                        }
                        let body = b"0123456789";
                        let resp =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                        if sock.write_all(resp.as_bytes()).is_err() || sock.write_all(body).is_err()
                        {
                            break;
                        }
                        let _ = sock.flush();
                        served.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }

        let connector = crate::TcpConnector;
        let pool: crate::pool::SharedPool<tokio::net::TcpStream> =
            Arc::new(crate::pool::ConnPool::new());
        let dir = std::env::temp_dir().join(format!("hya-net-pool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        for i in 0..3 {
            let t = Target::direct("127.0.0.1", port, &format!("/seg{i}"));
            let out = dir.join(format!("s{i}"));
            let n = fetch_object(
                &connector,
                &t,
                out.to_str().unwrap(),
                &AtomicU64::new(0),
                None,
                &Pace::unlimited(),
                Some(&pool),
            )
            .await
            .expect("fetch");
            assert_eq!(n, 10);
            assert_eq!(std::fs::read(&out).unwrap(), b"0123456789");
        }

        assert_eq!(
            served.load(Ordering::SeqCst),
            3,
            "three objects were served"
        );
        assert_eq!(
            opened.load(Ordering::SeqCst),
            1,
            "each object opened its own connection; the pool was not used"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The ranged small fetch returns exactly the span asked for on a `206`,
    /// and refuses a `200` — the start of the object is not its tail.
    #[tokio::test]
    async fn fetch_small_range_takes_206_and_refuses_200() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let object: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Range: bytes=") {
                        let (lo, hi) = v.trim().split_once('-').unwrap();
                        range = Some((lo.parse::<usize>().unwrap(), hi.parse::<usize>().unwrap()));
                    }
                }
                // `/deaf` ignores Range and answers 200 with the whole object.
                let resp = match range {
                    Some((lo, hi)) if !line.starts_with("GET /deaf") => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {lo}-{hi}/{}\r\nContent-Length: {}\r\n\r\n",
                        object.len(),
                        hi - lo + 1
                    ) + std::str::from_utf8(&[]).unwrap(),
                    _ => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", object.len()),
                };
                let body: &[u8] = match range {
                    Some((lo, hi)) if !line.starts_with("GET /deaf") => &object[lo..=hi],
                    _ => &object,
                };
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.write_all(body);
                let _ = sock.flush();
            }
        });

        let c = crate::TcpConnector;
        let t = Target::direct("127.0.0.1", port, "/obj");
        let tail = fetch_small_range(&c, &t, 900, 999, 100).await.expect("206");
        assert_eq!(tail.len(), 100);
        assert_eq!(tail[0], (900 % 256) as u8);
        assert_eq!(tail[99], (999 % 256) as u8);

        let deaf = Target::direct("127.0.0.1", port, "/deaf");
        let err = fetch_small_range(&c, &deaf, 900, 999, 100)
            .await
            .expect_err("a 200 is not the tail");
        assert!(err.to_string().contains("ignored the Range"), "{err}");
    }

    /// A pooled socket the origin has since closed must not surface as a
    /// failure: the request is retried once on a fresh connection.
    #[tokio::test]
    async fn a_dead_pooled_connection_is_retried_not_reported() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let opened = Arc::new(AtomicU64::new(0));
        {
            let opened = opened.clone();
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(mut sock) = conn else { continue };
                    let nth = opened.fetch_add(1, Ordering::SeqCst);
                    let Ok(peek) = sock.try_clone() else { continue };
                    let mut r = BufReader::new(peek);
                    let mut line = String::new();
                    let _ = r.read_line(&mut line);
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                    }
                    // Answer the first request, then hang up so the pooled
                    // socket is stale for the second.
                    // One response per connection, then hang up — so the
                    // socket the first request put back is already dead by
                    // the time the second one takes it.
                    let _ = nth;
                    let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
                    let _ = sock.flush();
                }
            });
        }

        let connector = crate::TcpConnector;
        let pool: crate::pool::SharedPool<tokio::net::TcpStream> =
            Arc::new(crate::pool::ConnPool::new());
        let dir = std::env::temp_dir().join(format!("hya-net-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = Target::direct("127.0.0.1", port, "/a");
        let out = dir.join("a");

        for _ in 0..2 {
            let n = fetch_object(
                &connector,
                &t,
                out.to_str().unwrap(),
                &AtomicU64::new(0),
                None,
                &Pace::unlimited(),
                Some(&pool),
            )
            .await
            .expect("a stale pooled socket must be retried, not surfaced");
            assert_eq!(n, 2);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A scripted origin for `fetch_object`: one thread, one response per
    /// request, chosen by the request line and the `Range` header it carried.
    fn scripted_origin(
        respond: impl Fn(&str, Option<(u64, u64)>) -> Vec<u8> + Send + 'static,
    ) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Range: bytes=") {
                        let (lo, hi) = v.trim().split_once('-').unwrap();
                        range = Some((lo.parse::<u64>().unwrap(), hi.parse::<u64>().unwrap()));
                    }
                }
                let _ = sock.write_all(&respond(&line, range));
                let _ = sock.flush();
            }
        });
        port
    }

    /// A chunked body that stops before its `0` chunk is a truncated
    /// segment, and must not come back as a successful fetch of what arrived.
    #[tokio::test]
    async fn a_chunked_body_cut_before_its_final_chunk_is_an_error() {
        let port = scripted_origin(|_, _| {
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3\r\nwor".to_vec()
        });
        let dir = std::env::temp_dir().join(format!("hya-net-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("seg");
        let t = Target::direct("127.0.0.1", port, "/seg");
        let err = fetch_object(
            &crate::TcpConnector,
            &t,
            out.to_str().unwrap(),
            &AtomicU64::new(0),
            None,
            &Pace::unlimited(),
            None,
        )
        .await
        .expect_err("a body cut mid-chunk is not a delivered object");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A caller that asked for a byte range gets exactly that range: a 200 is
    /// the whole object, and a 206 from another offset is the wrong bytes.
    #[tokio::test]
    async fn fetch_object_insists_on_206_at_the_requested_offset() {
        let port = scripted_origin(|line, range| {
            let object: Vec<u8> = (0..100u8).collect();
            match range {
                Some((lo, hi)) if line.starts_with("GET /ok") => {
                    let body = &object[lo as usize..=hi as usize];
                    let mut r = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {lo}-{hi}/100\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(body);
                    r
                }
                Some((lo, hi)) if line.starts_with("GET /shifted") => {
                    let mut r = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/100\r\nContent-Length: {}\r\n\r\n",
                        hi - lo,
                        hi - lo + 1
                    )
                    .into_bytes();
                    r.extend_from_slice(&object[..(hi - lo + 1) as usize]);
                    r
                }
                _ => {
                    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec();
                    r.extend_from_slice(&object);
                    r
                }
            }
        });
        let dir = std::env::temp_dir().join(format!("hya-net-206-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("seg");
        let fetch = |path: &str| {
            let t = Target::direct("127.0.0.1", port, path)
                .with_headers(vec!["Range: bytes=10-19".into()], None);
            let out = out.clone();
            async move {
                fetch_object(
                    &crate::TcpConnector,
                    &t,
                    out.to_str().unwrap(),
                    &AtomicU64::new(0),
                    None,
                    &Pace::unlimited(),
                    None,
                )
                .await
            }
        };
        assert_eq!(fetch("/ok").await.unwrap(), 10);
        assert_eq!(std::fs::read(&out).unwrap(), (10..20u8).collect::<Vec<_>>());

        let err = fetch("/deaf").await.expect_err("a 200 is the whole object");
        assert!(err.to_string().contains("ignored the Range"), "{err}");
        let err = fetch("/shifted")
            .await
            .expect_err("bytes from offset 0 are not 10-19");
        assert!(err.to_string().contains("starts at 0"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A server sending BOTH an ETag and a Last-Modified must keep its date.
    ///
    /// Regression test for `--remote-time`. The two headers were collapsed into a
    /// single `validator` field as `etag.or(last_mod)`, which is the right choice
    /// for resume and mirror assembly — but `--remote-time` read that same field
    /// and called `parse_http_date` on it. Every server that sends both headers
    /// (GitHub, S3, most CDNs: the common case) therefore had its date discarded
    /// before the flag ran, and the tool reported "server gave no date-form
    /// validator" about a response whose `Last-Modified` was right there.
    #[test]
    fn a_probe_keeps_last_modified_even_when_an_etag_is_present() {
        const DATE: &str = "Sat, 06 Jan 2024 19:38:59 GMT";

        // Both headers: the ETag wins the validator, the date is still kept.
        let (v, weak, lm) = validator_from(Some("\"0x8DC0EEF\"".into()), Some(DATE.into()));
        assert_eq!(v.as_deref(), Some("\"0x8DC0EEF\""));
        assert!(!weak, "a strong ETag is not a weak validator");
        assert_eq!(
            lm.as_deref(),
            Some(DATE),
            "the date must survive an ETag: --remote-time has nothing else to read"
        );
        assert!(
            crate::polite::parse_http_date(lm.as_deref().unwrap()).is_some(),
            "the kept date must be the parseable form the flag needs"
        );

        // Date only: it serves as BOTH the validator and the date, and is weak.
        let (v, weak, lm) = validator_from(None, Some(DATE.into()));
        assert_eq!(v.as_deref(), Some(DATE));
        assert!(weak, "a bare date is too weak to splice mirrors against");
        assert_eq!(lm.as_deref(), Some(DATE));

        // ETag only: no date to report, and the flag must say so rather than guess.
        let (v, weak, lm) = validator_from(Some("\"abc\"".into()), None);
        assert_eq!(v.as_deref(), Some("\"abc\""));
        assert!(!weak);
        assert_eq!(lm, None);

        // A weak ETag stays weak, and still does not shadow the date.
        let (v, weak, lm) = validator_from(Some("W/\"abc\"".into()), Some(DATE.into()));
        assert_eq!(v.as_deref(), Some("W/\"abc\""));
        assert!(weak, "W/ marks a weak validator");
        assert_eq!(lm.as_deref(), Some(DATE));

        // Neither header: nothing to resume against and nothing to date.
        let (v, weak, lm) = validator_from(None, None);
        assert_eq!(v, None);
        assert!(!weak);
        assert_eq!(lm, None);
    }

    /// The vectorized header scans must agree with the `windows` walks they
    /// replaced on every input shape — including the ones that make a naive
    /// memchr version wrong: a bare `\r`, a `\r` in the last three bytes, and
    /// `\r\n\r` without the final `\n`.
    #[test]
    fn vectorized_header_scans_match_the_window_walks_they_replaced() {
        fn ref_crlf(b: &[u8]) -> Option<usize> {
            b.windows(2).position(|w| w == b"\r\n")
        }
        fn ref_crlf2(b: &[u8]) -> Option<usize> {
            b.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
        }
        let cases: Vec<&[u8]> = vec![
            b"",
            b"\r",
            b"\n",
            b"\r\n",
            b"\r\r\n",
            b"\r\n\r",
            b"\r\n\r\n",
            b"a\r\nb\r\n\r\nbody",
            b"HTTP/1.1 200 OK\r\nA: 1\r\n\r\n",
            b"no boundary at all",
            b"trailing\r",
            b"\n\r\n\r",
            b"\r\r\r\r\n\r\n",
            b"x\r\n\r\n",
            b"\r\n\n\r\n\r\n",
        ];
        for c in cases {
            assert_eq!(find_crlf(c), ref_crlf(c), "find_crlf on {c:?}");
            assert_eq!(find_crlf2(c), ref_crlf2(c), "find_crlf2 on {c:?}");
        }
        // Exhaustive over short strings drawn from the alphabet that matters:
        // any disagreement on 4 bytes of {\r, \n, x} would be a real bug.
        let alpha = *b"\r\nx";
        let mut buf = [0u8; 5];
        for n in 0..=5usize {
            let mut idx = vec![0usize; n];
            loop {
                for k in 0..n {
                    buf[k] = alpha[idx[k]];
                }
                let c = &buf[..n];
                assert_eq!(find_crlf(c), ref_crlf(c), "find_crlf on {c:?}");
                assert_eq!(find_crlf2(c), ref_crlf2(c), "find_crlf2 on {c:?}");
                let mut k = 0;
                while k < n {
                    idx[k] += 1;
                    if idx[k] < alpha.len() {
                        break;
                    }
                    idx[k] = 0;
                    k += 1;
                }
                if k == n {
                    break;
                }
            }
        }
    }

    #[test]
    fn content_range_total_is_the_size_fallback() {
        // The number after the slash is the whole object, which is how a size is
        // recovered when HEAD omits Content-Length — jsDelivr does exactly that,
        // and without this fallback a 521 KiB object probes as 0 bytes.
        assert_eq!(parse_content_range_total("bytes 0-0/533653"), Some(533653));
        assert_eq!(parse_content_range_total("bytes 100-199/1000"), Some(1000));
        // An unknown total must not be invented.
        assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
        assert_eq!(parse_content_range_total("garbage"), None);
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        // Real servers mix cases: jsDelivr sends lowercase `etag` and capitalised
        // `Content-Type` in the same response.
        // Built with concat! rather than a `\` line continuation: the continuation
        // swallows the following indentation INTO the string, which silently
        // prefixed a header line with spaces and made this test fail against a
        // function that was already correct.
        let h = concat!(
            "HTTP/1.1 206 Partial Content\r\n",
            "content-range: bytes 0-9/100\r\n",
            "Transfer-Encoding: chunked\r\n",
            "\r\n"
        );
        assert_eq!(
            header_value(h, "content-range").as_deref(),
            Some("bytes 0-9/100")
        );
        assert_eq!(
            header_value(h, "TRANSFER-ENCODING").as_deref(),
            Some("chunked")
        );
        assert!(header_value(h, "etag").is_none());
    }

    /// A chunked body must be de-framed before it reaches the file.
    #[tokio::test]
    async fn chunked_bodies_are_deframed_including_split_boundaries() {
        use tokio::io::duplex;

        // "hello world!" split as 5 + 7 bytes, with a chunk extension on the first
        // size line and the terminating zero chunk.
        let body: &[u8] = b"5;ext=1\r\nhello\r\n7\r\n world!\r\n0\r\n\r\n";
        for split in [0usize, 3, 9, 14, body.len()] {
            let (mut server, client) = duplex(64);
            let head = &body[..split];
            let rest = body[split..].to_vec();
            let writer = tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                // Feed the remainder one byte at a time so every boundary lands
                // mid-read at least once across the parameter sweep.
                for b in rest {
                    let _ = server.write_all(&[b]).await;
                }
                let _ = server.shutdown().await;
            });

            let path = std::env::temp_dir().join(format!("hydra_chunk_{split}.bin"));
            let path_s = path.to_string_lossy().to_string();
            let sink = Arc::new(SparseSink::create(&path_s, 12).unwrap());
            let (tx, mut rx) = mpsc::unbounded_channel();
            let r = stream_chunked(
                client,
                0,
                0,
                crate::Watermark::fixed(12),
                head,
                sink.clone(),
                tx,
                Instant::now(),
                Pace::unlimited(),
            )
            .await;
            assert!(r.is_ok(), "split at {split} failed: {r:?}");
            writer.await.unwrap();
            drop(sink);

            let got = std::fs::read(&path).unwrap();
            assert_eq!(
                got, b"hello world!",
                "split at {split}: chunk framing leaked into the file"
            );
            let mut total = 0u64;
            while let Ok(a) = rx.try_recv() {
                total += a.bytes;
            }
            assert_eq!(total, 12, "split at {split}: arrivals must sum to the body");
            let _ = std::fs::remove_file(&path);
        }
    }

    /// A connection whose far end was lowered mid-body must not be pooled.
    ///
    /// The loop stops where the repair told it to, but the SERVER is still
    /// sending toward the end the request named, so the socket holds an unknown
    /// number of unread body bytes. Handing it to the next request makes that
    /// request read a response tail where a status line should be.
    ///
    /// The guard for this was written and then disabled by a shadowed variable:
    /// the read loop reassigned the very binding the guard compared against, so
    /// "the far end moved" was never true. The transfer-level tests could not see
    /// it, because the client REJECTS the mis-framed response it then reads — the
    /// file stays byte-exact and only the wasted request and the delay show, which
    /// is why this asserts on the pool itself.
    #[tokio::test]
    async fn a_connection_whose_range_was_shrunk_is_never_pooled() {
        use crate::origin::OriginSet;
        const SIZE: u64 = 1024 * 1024;
        let net = Arc::new(OriginSet::new());
        let (port, ctl) = net.spawn(SIZE, 2 * 1024 * 1024);
        // Keep-alive, or the response says `close` and the pool would refuse the
        // socket for a reason that has nothing to do with what is being tested.
        ctl.keep_alive
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let t = Target::direct("127.0.0.1", port, "/obj");

        let path = std::env::temp_dir().join("hydra_shrunk_not_pooled.bin");
        let path_s = path.to_string_lossy().to_string();
        let sink = Arc::new(SparseSink::create(&path_s, SIZE).unwrap());
        let (tx, _rx) = mpsc::unbounded_channel();
        let pool: crate::pool::SharedPool<tokio::io::DuplexStream> =
            Arc::new(crate::pool::ConnPool::new());

        let bound = crate::Watermark::fixed(SIZE);
        let b2 = bound.clone();
        tokio::spawn(async move {
            // Long enough that the body is well under way and short enough that
            // most of it is still to come.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            b2.shrink_to(SIZE / 4);
        });
        let r = fetch_range(
            net.clone(),
            0,
            t.clone(),
            0,
            bound,
            sink,
            tx,
            Instant::now(),
            Pace::unlimited(),
            Some(pool.clone()),
        )
        .await;
        assert!(r.is_ok(), "a shrink is not a failure: {r:?}");
        assert!(
            pool.take(&t).is_none(),
            "a preempted connection was offered back for reuse with the rest of \
             its response still arriving on it"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A chunked body that stops early is an error, not a short success.
    #[tokio::test]
    async fn a_truncated_chunked_body_is_an_error() {
        use tokio::io::duplex;
        let (mut server, client) = duplex(64);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            // Announces 12 bytes, sends 5, then hangs up.
            let _ = server.write_all(b"c\r\nhello").await;
            let _ = server.shutdown().await;
        });
        let path = std::env::temp_dir().join("hydra_chunk_trunc.bin");
        let path_s = path.to_string_lossy().to_string();
        let sink = Arc::new(SparseSink::create(&path_s, 12).unwrap());
        let (tx, _rx) = mpsc::unbounded_channel();
        let r = stream_chunked(
            client,
            0,
            0,
            crate::Watermark::fixed(12),
            b"",
            sink,
            tx,
            Instant::now(),
            Pace::unlimited(),
        )
        .await;
        assert!(
            r.is_err(),
            "a body that ends mid-chunk must not be reported as complete"
        );
        let _ = std::fs::remove_file(&path);
    }

    impl tokio::io::AsyncWrite for Script {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            b: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::task::Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    struct Scripted(std::sync::Mutex<Option<Script>>);

    impl Connector for Scripted {
        type Stream = Script;
        fn connect<'a>(
            &'a self,
            _: &'a Target,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<Script>> + Send + 'a>>
        {
            Box::pin(async move { Ok(self.0.lock().unwrap().take().expect("one connection")) })
        }
    }

    /// A reset that arrives right behind readable bytes is seen inside the
    /// drain, not by the read the loop awaits. It must still surface as the
    /// reset, after the bytes before it were credited, rather than vanish into
    /// a clean end of stream.
    #[tokio::test]
    async fn fetch_range_reports_a_reset_that_follows_readable_bytes() {
        let head = b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-15/16\r\nContent-Length: 16\r\n\r\n";
        let stream = script(vec![
            Ok(Some(head.to_vec())),
            Ok(Some(b"abcd".to_vec())),
            Ok(Some(b"efgh".to_vec())),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
        ]);
        let path =
            std::env::temp_dir().join(format!("hydra_drain_reset_{}.bin", std::process::id()));
        let path_s = path.to_string_lossy().to_string();
        let sink = Arc::new(SparseSink::create(&path_s, 16).unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let r = fetch_range(
            Arc::new(Scripted(std::sync::Mutex::new(Some(stream)))),
            0,
            Target::direct("127.0.0.1", 1, "/obj"),
            0,
            crate::Watermark::fixed(16),
            sink,
            tx,
            Instant::now(),
            Pace::unlimited(),
            None,
        )
        .await;
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
        let mut credited = 0;
        while let Ok(a) = rx.try_recv() {
            credited += a.bytes;
        }
        assert_eq!(
            credited, 8,
            "the bytes read before the reset are still credited"
        );
        assert_eq!(&std::fs::read(&path).unwrap()[..8], b"abcdefgh");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn stream_chunked_reports_a_reset_that_follows_readable_bytes() {
        let stream = script(vec![
            Ok(Some(b"c\r\nhello ".to_vec())),
            Ok(Some(b"wo".to_vec())),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
        ]);
        let path =
            std::env::temp_dir().join(format!("hydra_chunk_reset_{}.bin", std::process::id()));
        let path_s = path.to_string_lossy().to_string();
        let sink = Arc::new(SparseSink::create(&path_s, 12).unwrap());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let r = stream_chunked(
            stream,
            0,
            0,
            crate::Watermark::fixed(12),
            b"",
            sink,
            tx,
            Instant::now(),
            Pace::unlimited(),
        )
        .await;
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
        let mut credited = 0;
        while let Ok(a) = rx.try_recv() {
            credited += a.bytes;
        }
        assert_eq!(credited, 8);
        let _ = std::fs::remove_file(&path);
    }

    /// Build a probe that says nothing except how it named the object.
    fn named(disposition: &str) -> Probe {
        Probe {
            disposition: Some(disposition.to_string()),
            ..Probe::default()
        }
    }

    /// The reported defect: a cloud drive naming a non-ASCII file.
    ///
    /// The only place such a name can go is the extended parameter, and a
    /// scan for the substring `filename=` does not find `filename*=`. So the
    /// header was read as naming nothing, every caller fell back to the last
    /// segment of the URL — an opaque object id with an unrelated extension —
    /// and the download dialog offered `211784095912171.rar` for a file the
    /// user had just picked by its real name.
    #[test]
    fn an_extended_filename_is_read_and_beats_the_ascii_one_beside_it() {
        assert_eq!(
            named("attachment; filename*=UTF-8''%E6%97%A0%E6%9E%81.exe").suggested_filename(),
            Some("无极.exe".into())
        );

        // Both forms present is the common shape, and RFC 6266 §4.3 says the
        // extended one wins: the plain parameter is what the server keeps for
        // clients that cannot read the other, so it is the degraded copy.
        assert_eq!(
            named("attachment; filename=\"211784095912171.rar\"; filename*=UTF-8''%E6%97%A0%E6%9E%81.exe")
                .suggested_filename(),
            Some("无极.exe".into())
        );

        // Order in the header is not preference, and neither is spacing or case.
        assert_eq!(
            named("attachment;FileName*=utf-8''%E6%97%A0.exe;filename=x.rar").suggested_filename(),
            Some("无.exe".into())
        );

        // Latin-1 is the other charset RFC 8187 names.
        assert_eq!(
            named("attachment; filename*=ISO-8859-1'en'r%E9sum%E9.pdf").suggested_filename(),
            Some("résumé.pdf".into())
        );

        // A `filename*` too broken to decode must not shadow a usable plain
        // one: no charset delimiters at all is not an extended value.
        assert_eq!(
            named("attachment; filename*=junk; filename=setup.exe").suggested_filename(),
            Some("setup.exe".into())
        );

        // And a header naming nothing still names nothing.
        assert_eq!(named("attachment").suggested_filename(), None);
        assert_eq!(named("inline; size=42").suggested_filename(), None);
    }

    /// `filename` is a PARAMETER, so its value ends where the grammar says it
    /// does — not at the first `;` in the header.
    #[test]
    fn a_quoted_filename_keeps_what_is_inside_the_quotes() {
        // A semicolon inside the quotes is part of the name; splitting the
        // header on `;` first would have truncated it to `Live`.
        assert_eq!(
            named("attachment; filename=\"Live; Loud.mp3\"").suggested_filename(),
            Some("Live; Loud.mp3".into())
        );
        // An escaped quote is one character of the name, and one Windows
        // cannot store, so it is written as `_`.
        assert_eq!(
            named("attachment; filename=\"say \\\"hi\\\".txt\"").suggested_filename(),
            Some("say _hi_.txt".into())
        );
        // Unquoted token, with a parameter after it.
        assert_eq!(
            named("attachment; filename=setup.exe; size=99").suggested_filename(),
            Some("setup.exe".into())
        );
    }

    /// A percent-encoded name in the PLAIN parameter is not legal and is sent
    /// anyway. Decoding it is right; decoding everything is not.
    #[test]
    fn a_percent_encoded_legacy_name_is_decoded_only_where_it_means_something() {
        assert_eq!(
            named("attachment; filename=%E6%97%A0%E6%9E%81.exe").suggested_filename(),
            Some("无极.exe".into())
        );
        // `100%20off.zip` is a name a server may mean literally, and its
        // decoding is pure ASCII — indistinguishable from an unencoded name,
        // so the file is not renamed on a guess.
        assert_eq!(
            named("attachment; filename=\"100%20off.zip\"").suggested_filename(),
            Some("100%20off.zip".into())
        );
        // Escapes that spell no valid UTF-8 (GBK bytes, say) are left alone
        // rather than replaced with U+FFFD.
        assert_eq!(
            named("attachment; filename=%CE%DE%BC%AB.exe").suggested_filename(),
            Some("%CE%DE%BC%AB.exe".into())
        );
    }

    /// A name comes from the server, so it may not choose where the file goes.
    #[test]
    fn a_server_supplied_name_cannot_escape_the_download_folder() {
        assert_eq!(
            named("attachment; filename=\"/etc/cron.d/payload\"").suggested_filename(),
            Some("payload".into())
        );
        // A Windows path on a Unix client: no separator `file_name` knows, so
        // the whole traversal used to survive as one filename.
        assert_eq!(
            named("attachment; filename=\"..\\\\..\\\\evil.exe\"").suggested_filename(),
            Some("evil.exe".into())
        );
        // The traversal segments are not names either, however they arrive.
        assert_eq!(
            named("attachment; filename=\"..\"").suggested_filename(),
            None
        );
        assert_eq!(
            named("attachment; filename*=UTF-8''%2E%2E%2F%2E%2E%2F").suggested_filename(),
            None
        );
        assert_eq!(
            named("attachment; filename=\"\"").suggested_filename(),
            None
        );
        assert_eq!(
            named("attachment; filename=\"   \"").suggested_filename(),
            None
        );
    }

    /// The header names files in every script that has files, so the decoder
    /// has to be indifferent to which one. Each name is asserted in the three
    /// shapes a server actually sends it in: the extended parameter, the
    /// plain parameter carrying percent-escapes, and the plain parameter
    /// carrying the raw UTF-8 bytes.
    ///
    /// A loop rather than thirteen tests because the point is the ABSENCE of
    /// per-script behaviour: nothing here should be able to pass for Latin
    /// text and fail for Hangul, and a table makes a regression that does
    /// name the language it broke.
    #[test]
    fn a_name_in_any_script_survives_every_form_of_the_header() {
        /// Percent-encode every byte, which is always a legal encoding and
        /// keeps the fixture independent of which bytes need escaping.
        fn pct(s: &str) -> String {
            s.bytes().map(|b| format!("%{b:02X}")).collect()
        }

        for (script, name) in [
            ("Persian", "دانلود فایل.pdf"),
            ("Arabic", "ملف البرنامج.exe"),
            ("Hebrew", "קובץ ההתקנה.exe"),
            ("Turkish", "Çalışma Günlüğü.docx"),
            ("Korean", "설치 프로그램.exe"),
            ("Japanese", "無極アシスタント.exe"),
            ("Chinese", "无极助手3.3 用不习惯可退回此版本.exe"),
            ("Russian", "Установка.exe"),
            ("Greek", "Εγκατάσταση.exe"),
            ("Thai", "ตัวติดตั้ง.exe"),
            ("Hindi", "स्थापना.exe"),
            ("Vietnamese", "Cài đặt.exe"),
            ("Emoji", "report 📊 final.xlsx"),
        ] {
            let want = Some(name.to_string());
            assert_eq!(
                named(&format!("attachment; filename*=UTF-8''{}", pct(name))).suggested_filename(),
                want,
                "{script}: extended parameter"
            );
            assert_eq!(
                named(&format!("attachment; filename=\"{}\"", pct(name))).suggested_filename(),
                want,
                "{script}: percent-escaped plain parameter"
            );
            assert_eq!(
                named(&format!("attachment; filename=\"{name}\"")).suggested_filename(),
                want,
                "{script}: raw UTF-8 plain parameter"
            );
        }

        // The language tag is part of the grammar and part of nothing else:
        // a Persian name tagged `fa` is the same name.
        assert_eq!(
            named("attachment; filename*=utf-8'fa'%D9%81%D8%A7%DB%8C%D9%84.zip")
                .suggested_filename(),
            Some("فایل.zip".into())
        );
    }

    /// Invisible characters: the two that must go, and the two that must not.
    #[test]
    fn invisible_characters_are_judged_by_what_they_do() {
        // U+202E reverses everything after it, so this name is DRAWN ending
        // in `.jpg` while the file is an executable. It is the one spoof an
        // RTL-aware download manager invites, and it must not survive.
        assert_eq!(
            named("attachment; filename*=UTF-8''%D8%B9%DA%A9%D8%B3%E2%80%AEgpj.exe")
                .suggested_filename(),
            Some("عکسgpj.exe".into())
        );
        // A BOM left in front of a decoded name is not part of the name.
        assert_eq!(
            named("attachment; filename*=UTF-8''%EF%BB%BF%ED%8C%8C%EC%9D%BC.zip")
                .suggested_filename(),
            Some("파일.zip".into())
        );
        // Controls are not names either, whatever they interrupt.
        assert_eq!(
            named("attachment; filename*=UTF-8''%D9%81%00%D8%A7%DB%8C%D9%84.pdf")
                .suggested_filename(),
            Some("فایل.pdf".into())
        );

        // U+200C is the Persian نیم‌فاصله: `می‌روم` is one word spelled with
        // it and a misspelling without, so stripping it would corrupt the
        // name it was meant to protect.
        assert_eq!(
            named("attachment; filename=\"می‌روم.pdf\"").suggested_filename(),
            Some("می\u{200c}روم.pdf".into())
        );
        // U+200D holds a multi-person emoji together; without it the name
        // grows two extra characters.
        assert_eq!(
            named("attachment; filename=\"family 👨\u{200d}👩\u{200d}👧.png\"")
                .suggested_filename(),
            Some("family 👨\u{200d}👩\u{200d}👧.png".into())
        );
    }

    /// A limit counted in characters passes every Latin test and fails on the
    /// scripts that need it: 200 Hangul syllables are 600 bytes, and no
    /// mainstream filesystem takes a name past 255.
    #[test]
    fn a_long_name_is_clamped_by_bytes_with_its_extension_intact() {
        let long = format!("{}.exe", "설치프로그램".repeat(50));
        let got = named(&format!("attachment; filename=\"{long}\""))
            .suggested_filename()
            .expect("a long name is still a name");
        assert!(
            got.len() <= 255,
            "clamped to {} bytes, which no filesystem accepts",
            got.len()
        );
        // The extension decides how the file opens and where it is filed, so
        // it is the stem that gives way.
        assert!(got.ends_with(".exe"), "lost its extension: {got}");
        // And the cut is on a character boundary — half a `설` is not a
        // character, and would not have survived the trip back through UTF-8.
        assert!(long.starts_with(got.trim_end_matches(".exe")));

        // A name that already fits is returned untouched, extension or not.
        assert_eq!(
            named("attachment; filename=\"보고서\"").suggested_filename(),
            Some("보고서".into())
        );
    }
}

#[cfg(test)]
mod retarget_tests {
    use super::*;

    /// The `Location` from the bug report, verbatim.
    const S3: &str = "https://s3q.ait.dtu.dk:9000/figshare/26003087/TEP_Mode1.h5\
?X-Amz-Algorithm=AWS4-HMAC-SHA256\
&X-Amz-Credential=00000005001330512eb1/20260910/oha/s3/aws4_request\
&X-Amz-Date=20260910T120757Z&X-Amz-Expires=10&X-Amz-SignedHeaders=host\
&X-Amz-Signature=92429b6cd0794b68e3a79044dbc0f93d9ac32dba01650e246577cde1bab";

    /// Refusing this is what made the transfer stop with `unusable redirect
    /// target` while naming a URL that was perfectly good.
    #[test]
    fn an_absolute_https_location_is_followed() {
        let prev = Target::direct_tls("data.dtu.dk", 443, "/ndownloader/files/26003087");
        let next = retarget(&prev, S3).expect("a signed https location is usable");
        assert_eq!(next.host, "s3q.ait.dtu.dk");
        assert_eq!(next.port, 9000, "the signed authority includes its port");
        assert!(next.tls, "https must not be fetched in the clear");
        // The query must survive byte for byte: the credential contains literal
        // `/` characters, and re-encoding any of them breaks the signature.
        assert!(next.path.contains("aws4_request"), "{}", next.path);
        assert!(
            next.path.starts_with("/figshare/26003087/TEP_Mode1.h5?"),
            "{}",
            next.path
        );
        // What the Host header will say — the half that SigV4 signs.
        let head = build_request_head("GET", &next, Some((0, 9)));
        assert!(head.contains("\r\nHost: s3q.ait.dtu.dk:9000\r\n"), "{head}");
    }

    #[test]
    fn default_ports_are_supplied_per_scheme() {
        let prev = Target::direct("h", 80, "/a");
        let s = retarget(&prev, "https://example.com/o").expect("https");
        assert_eq!((s.port, s.tls), (443, true));
        let p = retarget(&prev, "http://example.com/o").expect("http");
        assert_eq!((p.port, p.tls), (80, false));
        // No path at all still names the root.
        assert_eq!(retarget(&prev, "https://example.com").unwrap().path, "/");
    }

    /// `rsplit_once(':')` cuts an IPv6 literal at its last colon, which invents
    /// a port out of part of the address.
    #[test]
    fn an_ipv6_literal_is_not_cut_at_its_last_colon() {
        let prev = Target::direct("h", 80, "/a");
        let n = retarget(&prev, "http://[2001:db8::1]/o").expect("bare v6");
        assert_eq!((n.host.as_str(), n.port), ("2001:db8::1", 80));
        let n = retarget(&prev, "https://[2001:db8::1]:8443/o").expect("v6 with port");
        assert_eq!((n.host.as_str(), n.port), ("2001:db8::1", 8443));
        assert!(n.tls);
    }

    #[test]
    fn a_relative_location_keeps_the_origin_and_its_headers() {
        let mut prev = Target::direct_tls("h", 9000, "/a");
        prev.headers = vec!["Cookie: s=1".into()];
        let n = retarget(&prev, "/b?x=1").expect("relative");
        assert_eq!((n.host.as_str(), n.port, n.tls), ("h", 9000, true));
        assert_eq!(n.path, "/b?x=1");
        assert_eq!(n.headers, prev.headers, "same origin keeps its headers");
    }

    /// A cross-origin hop must not replay the previous origin's headers: that
    /// is how a session cookie reaches whatever host a redirect names.
    #[test]
    fn an_absolute_location_does_not_carry_headers_to_another_host() {
        let mut prev = Target::direct_tls("data.dtu.dk", 443, "/f");
        prev.headers = vec!["Cookie: aws-waf-token=secret".into()];
        let n = retarget(&prev, S3).expect("usable");
        assert!(n.headers.is_empty(), "{:?}", n.headers);
    }

    #[test]
    fn a_location_that_names_nothing_usable_is_refused() {
        let prev = Target::direct("h", 80, "/a");
        for bad in ["", "   ", "ftp://h/o", "javascript:alert(1)", "https://"] {
            assert!(retarget(&prev, bad).is_none(), "{bad:?} must be refused");
        }
        let proxied = Target::via_proxy("proxy.local", 3128, "h:80", "/a");
        assert!(
            retarget(&proxied, "https://").is_none(),
            "an empty authority is no more usable through a proxy"
        );
    }

    /// The z-lib shape: an absolute `Location` naming the host that sent it.
    /// Judged on form rather than origin, this dropped the `Cookie:` the user
    /// had supplied — the one documented way to get into such a site.
    #[test]
    fn an_absolute_same_origin_location_keeps_the_headers() {
        let mut prev = Target::direct_tls("zh.z-lib.sk", 443, "/dl/omZxxOYdnp");
        prev.headers = vec!["Cookie: __diamwall=0x472138112".into()];
        prev.agent = Some("hydra-test/1".into());
        let n = retarget(&prev, "https://zh.z-lib.sk/dl/omZxxOYdnp").expect("usable");
        assert_eq!(n.headers, prev.headers, "same origin keeps its headers");
        assert_eq!(n.agent, prev.agent, "and its agent");
    }

    /// Host comparison is case-insensitive, port and scheme are part of the
    /// origin. The last two are what stop a cookie set over TLS, or for one
    /// service, from being replayed somewhere it was never issued for.
    ///
    /// The agent and an ordinary header go everywhere: they identify the
    /// client, not the account, and a hop that lost them used to arrive as a
    /// different program with no `-H` at all.
    #[test]
    fn only_a_genuinely_same_origin_hop_keeps_the_credentials() {
        let mut prev = Target::direct_tls("h.example", 443, "/a");
        prev.headers = vec!["Cookie: s=1".into(), "X-Trace: 1".into()];
        prev.agent = Some("hydra-test/1".into());
        let kept = retarget(&prev, "https://H.EXAMPLE/b").expect("usable");
        assert_eq!(kept.headers, prev.headers, "the host case is not the host");
        for elsewhere in [
            "http://h.example/b",
            "https://h.example:9000/b",
            "https://other.example/b",
        ] {
            let n = retarget(&prev, elsewhere).expect("usable");
            assert_eq!(
                n.headers,
                ["X-Trace: 1"],
                "{elsewhere} must not carry the cookie"
            );
            assert_eq!(
                n.agent, prev.agent,
                "{elsewhere} must still know who is asking"
            );
        }
    }

    /// Through a forward proxy the hop kept the authority but lost the scheme,
    /// so a proxied `https` chain continued in the clear from its first
    /// redirect.
    #[test]
    fn a_proxied_hop_keeps_its_scheme_and_same_origin_headers() {
        let mut prev = Target::via_proxy("proxy.local", 3128, "h.example:443", "/a");
        prev.tls = true;
        prev.headers = vec!["Cookie: s=1".into()];
        let n = retarget(&prev, "https://h.example:443/b").expect("usable");
        assert!(n.tls, "an https hop must not continue in the clear");
        assert_eq!(n.host, "proxy.local", "the socket still goes to the proxy");
        assert_eq!(n.origin.as_deref(), Some("h.example:443"));
        assert_eq!(n.headers, prev.headers);
        let away = retarget(&prev, "https://other.example/b").expect("usable");
        assert!(away.headers.is_empty(), "a proxy is not a same-origin pass");
    }
}

#[cfg(test)]
mod host_header_tests {
    use super::*;

    /// The reported defect, reduced to the header that caused it.
    ///
    /// `s3q.ait.dtu.dk:9000` hands out SigV4 presigned URLs, and SigV4 signs
    /// `host`. Sending `Host: s3q.ait.dtu.dk` for a signature minted over
    /// `s3q.ait.dtu.dk:9000` is a different canonical request, so the store
    /// answered `SignatureDoesNotMatch` — on a URL curl fetched from the same
    /// machine a second later. Every object store on a non-default port was
    /// unreachable, and the error blamed the signature rather than the client.
    #[test]
    fn a_non_default_port_is_carried_in_the_host_header() {
        let t = Target::direct_tls("s3q.ait.dtu.dk", 9000, "/figshare/o?X-Amz-Signature=ab");
        let head = build_request_head("GET", &t, None);
        assert!(
            head.contains("\r\nHost: s3q.ait.dtu.dk:9000\r\n"),
            "the port is part of the signed authority:\n{head}"
        );
    }

    /// ...and the default port is still omitted. Adding `:443` everywhere would
    /// be the same class of bug pointed the other way: it changes the canonical
    /// host for every ordinary signed URL, which is nearly all of them.
    #[test]
    fn a_default_port_is_left_out() {
        for (tls, port) in [(true, 443u16), (false, 80)] {
            let t = if tls {
                Target::direct_tls("example.com", port, "/o")
            } else {
                Target::direct("example.com", port, "/o")
            };
            let head = build_request_head("GET", &t, None);
            assert!(
                head.contains("\r\nHost: example.com\r\n"),
                "tls={tls} port={port}:\n{head}"
            );
        }
        // A plain-HTTP origin on a non-default port still needs it.
        let t = Target::direct("example.com", 8080, "/o");
        assert!(build_request_head("GET", &t, None).contains("\r\nHost: example.com:8080\r\n"));
        // ...as does an HTTPS origin on 80, which is not ITS default.
        let t = Target::direct_tls("example.com", 80, "/o");
        assert!(build_request_head("GET", &t, None).contains("\r\nHost: example.com:80\r\n"));
    }

    /// A proxied request is sent in absolute form and carries the authority the
    /// proxy was given, port included; the fix must not disturb that path.
    #[test]
    fn a_proxied_request_still_names_the_origin_authority() {
        let mut t = Target::direct_tls("s3q.ait.dtu.dk", 9000, "/o");
        t.origin = Some("s3q.ait.dtu.dk:9000".into());
        let head = build_request_head("GET", &t, None);
        assert!(
            head.starts_with("GET http://s3q.ait.dtu.dk:9000/o HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nHost: s3q.ait.dtu.dk:9000\r\n"), "{head}");
    }

    /// The reported defect: an alist share whose directory is written in kana.
    /// The address is legal and every browser fetches it, but the raw UTF-8 is
    /// not a request-target, and the origin answered `400 Bad Request`.
    #[test]
    fn a_non_ascii_path_is_encoded_into_the_request_line() {
        let t = Target::direct_tls(
            "alist-tousaka.imoutoheaven.org",
            443,
            "/d/guest/Public8/あけあけ/packs/2025-10d.part1.rar",
        );
        assert!(
            build_request_head("GET", &t, None).starts_with(
                "GET /d/guest/Public8/%E3%81%82%E3%81%91%E3%81%82%E3%81%91/packs/2025-10d.part1.rar HTTP/1.1\r\n"
            ),
            "{}",
            build_request_head("GET", &t, None)
        );
    }

    /// The same address in its encoded spelling must reach the same object:
    /// encoding `%` again would turn `%E3` into `%25E3` and 404 every URL
    /// copied out of a browser's address bar.
    #[test]
    fn an_already_encoded_path_is_left_alone() {
        let path = "/d/guest/%E3%81%82%E3%81%91/packs/2025-10d.part1.rar";
        let t = Target::direct_tls("alist-tousaka.imoutoheaven.org", 443, path);
        assert!(
            build_request_head("GET", &t, None).starts_with(&format!("GET {path} HTTP/1.1\r\n")),
            "{}",
            build_request_head("GET", &t, None)
        );
    }

    /// A space is excluded from a request-target for a sharper reason than the
    /// kana: it ends the target, so the start line parses as a request for the
    /// first word with a garbage version. The query's own delimiters have to
    /// survive the encoding that fixes it.
    #[test]
    fn a_space_is_encoded_and_the_query_delimiters_are_not() {
        let t = Target::direct("example.com", 80, "/my files/a b.zip?x=1&y=2 3");
        assert!(
            build_request_head("GET", &t, None)
                .starts_with("GET /my%20files/a%20b.zip?x=1&y=2%203 HTTP/1.1\r\n"),
            "{}",
            build_request_head("GET", &t, None)
        );
    }

    /// Anything already legal reaches the origin byte-identical. A presigned
    /// URL's signature covers the query as the signer spelled it, so encoding
    /// `[`, `+` or an existing escape on the way out turns a valid signature
    /// into `SignatureDoesNotMatch` on every object store.
    #[test]
    fn an_already_legal_query_is_passed_through_unchanged() {
        let path = "/o?X-Amz-Credential=AKIA%2F20260921%2Fus-east-1&filter[a]=b&s=x+y~z";
        let t = Target::direct_tls("s3.example.com", 443, path);
        assert!(
            build_request_head("GET", &t, None).starts_with(&format!("GET {path} HTTP/1.1\r\n")),
            "{}",
            build_request_head("GET", &t, None)
        );
    }

    /// Through a forward proxy the target is absolute, and the path inside it
    /// is the same string — a proxy rejects an invalid target as readily as an
    /// origin does.
    #[test]
    fn a_proxied_non_ascii_path_is_encoded_too() {
        let mut t = Target::direct_tls("example.com", 443, "/あ.rar");
        t.origin = Some("example.com".into());
        assert!(
            build_request_head("GET", &t, None)
                .starts_with("GET http://example.com/%E3%81%82.rar HTTP/1.1\r\n"),
            "{}",
            build_request_head("GET", &t, None)
        );
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// A probe as it comes off the wire: the raw block holds the headers and,
    /// for a short error, the body that arrived in the same read.
    fn answered(status: u16, head_and_body: &str) -> Probe {
        Probe {
            status,
            raw_head: head_and_body.replace('\n', "\r\n"),
            ..Probe::default()
        }
    }

    /// The reported defect. `data.dtu.dk` sits behind AWS WAF, which turns a
    /// non-browser away with `202` and `Content-Length: 0`. Read as metadata
    /// that describes a zero-byte object, so hydra wrote an empty `.h5` and
    /// exited 0 — a download that "succeeded" with no file in it.
    #[test]
    fn a_waf_challenge_is_refused_rather_than_read_as_an_empty_object() {
        let p = answered(
            202,
            "HTTP/1.1 202 Accepted\ncontent-length: 0\nx-amzn-waf-action: challenge\n\n",
        );
        let why = p.refusal().expect("202 carries no object");
        assert!(why.contains("202"), "{why}");
        assert!(why.contains("carries no file"), "{why}");
        assert!(why.contains("browser"), "{why}");
        // The trap that made this silent: the length is stated, and stated zero.
        assert_eq!(p.stated_length(), Some(0));
    }

    /// The other half of the report: a presigned URL whose ten-second window
    /// had closed. "403 Forbidden" sends a user hunting for a proxy or a
    /// password; the server had already said which it was.
    #[test]
    fn the_servers_own_explanation_is_lifted_out_of_an_s3_error_body() {
        let p = answered(
            403,
            "HTTP/1.1 403 Forbidden\nContent-Type: application/xml\n\n\
<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>AccessDenied</Code>\
<Message>Request has expired</Message></Error>",
        );
        assert_eq!(
            p.refusal().as_deref(),
            Some(r#"server answered 403 Forbidden ("Request has expired")"#)
        );
    }

    #[test]
    fn a_status_that_carries_bytes_is_not_refused() {
        for s in [200, 203, 206] {
            let p = answered(s, "HTTP/1.1 200 OK\ncontent-length: 10\n\n");
            assert_eq!(p.refusal(), None, "{s} carries the object");
        }
        // Status 0 is "the peer said nothing", which `probe_resilient` answers
        // with a ranged GET. Refusing it here would break that fallback.
        assert_eq!(answered(0, "").refusal(), None);
    }

    #[test]
    fn the_rest_of_2xx_is_an_answer_rather_than_a_representation() {
        for s in [201, 202, 204, 205] {
            let p = answered(s, "HTTP/1.1 x\ncontent-length: 0\n\n");
            assert!(p.refusal().is_some(), "{s} is not the object");
            // No WAF header, so no browser advice to give.
            assert!(!p.refusal().unwrap().contains("browser"), "{s}");
        }
    }

    #[test]
    fn an_error_page_is_not_quoted_and_a_short_sentence_is() {
        // HTML: the message is buried in markup, so say nothing extra.
        let page = answered(
            404,
            "HTTP/1.1 404 Not Found\n\n<html><body><h1>Not Found</h1></body></html>",
        );
        assert_eq!(
            page.refusal().as_deref(),
            Some("server answered 404 Not Found")
        );
        // A plain-text body is the server's own sentence.
        let text = answered(410, "HTTP/1.1 410 Gone\n\nthis link was revoked");
        assert_eq!(
            text.refusal().as_deref(),
            Some(r#"server answered 410 Gone ("this link was revoked")"#)
        );
        // A body with no headers/body split at all cannot be mined.
        assert_eq!(
            answered(500, "HTTP/1.1 500 x").refusal().as_deref(),
            Some("server answered 500 Internal Server Error")
        );
    }

    /// A success's trailing bytes are the START OF THE FILE, not a message.
    /// Quoting them would print binary at the user.
    #[test]
    fn a_body_is_never_mined_for_an_explanation_on_a_success() {
        let p = answered(200, "HTTP/1.1 200 OK\n\n<Message>not a message</Message>");
        assert_eq!(p.refusal(), None);
        assert_eq!(p.error_detail(), None);
    }

    #[test]
    fn a_long_or_multiline_message_is_flattened_and_capped() {
        let long = "x".repeat(500);
        let p = answered(
            403,
            &format!("HTTP/1.1 403 Forbidden\n\n<Message>{long}</Message>"),
        );
        let why = p.refusal().unwrap();
        assert!(why.contains('…'), "{why}");
        assert!(why.chars().count() < 260, "{}", why.chars().count());
        // Newlines and control bytes collapse to single spaces.
        let messy = answered(
            403,
            "HTTP/1.1 403 Forbidden\n\n<Message>a\n\n  b\tc</Message>",
        );
        assert_eq!(
            messy.refusal().as_deref(),
            Some(r#"server answered 403 Forbidden ("a b c")"#)
        );
    }

    /// Truncation is the normal case, not an edge one: the read stops once the
    /// header block is complete, so a longer body arrives cut off mid-document.
    #[test]
    fn a_truncated_error_body_still_yields_its_message() {
        let p = answered(
            403,
            "HTTP/1.1 403 Forbidden\n\n<Error><Message>Request has expired</Message><Resource>/fig",
        );
        assert_eq!(
            p.refusal().as_deref(),
            Some(r#"server answered 403 Forbidden ("Request has expired")"#)
        );
    }

    /// The defect this test exists for was invisible in unit tests and obvious
    /// against the real origin: `s3q.ait.dtu.dk` writes the header block and
    /// the XML body as SEPARATE segments, so the probe's read loop — which
    /// stops the moment the headers are complete — never saw the sentence that
    /// explained the 403. Hydra reported a bare "403 Forbidden" about a URL
    /// whose signature had simply run out.
    #[tokio::test]
    async fn an_error_body_in_its_own_segment_is_still_read() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let body = b"<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code>\
<Message>Request has expired</Message></Error>";
                let head = format!(
                    "HTTP/1.1 403 Forbidden\r\nContent-Type: application/xml\r\n\
Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                // The headers alone, flushed, and only THEN the body: this is
                // the segmentation that hid the message.
                if sock.write_all(head.as_bytes()).is_err() || sock.flush().is_err() {
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
                let _ = sock.write_all(body);
            }
        });

        let t = Target::direct("127.0.0.1", port, "/o?X-Amz-Expires=10");
        let p = probe_via_get(&crate::TcpConnector, &t)
            .await
            .expect("probe");
        assert_eq!(p.status, 403);
        assert_eq!(
            p.refusal().as_deref(),
            Some(r#"server answered 403 Forbidden ("Request has expired")"#)
        );
    }

    const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

    fn chrome_target(headers: Vec<String>) -> Target {
        Target::direct_tls("download-center.example.com", 443, "/f/a.exe")
            .with_headers(headers, Some(CHROME_UA.into()))
    }

    fn header_names(head: &str) -> Vec<String> {
        head.split("\r\n")
            .skip(1)
            .filter_map(|l| l.split_once(':').map(|(n, _)| n.to_ascii_lowercase()))
            .collect()
    }

    #[test]
    fn a_ranged_request_for_a_chrome_user_carries_chromes_header_set_in_its_order() {
        let t = chrome_target(vec![
            "Cookie: a=b".into(),
            "Referer: https://download-center.example.com/download/?m=1".into(),
        ]);
        let head = build_request_head("GET", &t, Some((0, 0)));
        assert_eq!(
            header_names(&head),
            [
                "host",
                "connection",
                "sec-ch-ua",
                "sec-ch-ua-mobile",
                "sec-ch-ua-platform",
                "upgrade-insecure-requests",
                "user-agent",
                "accept",
                "sec-fetch-site",
                "sec-fetch-mode",
                "sec-fetch-user",
                "sec-fetch-dest",
                "referer",
                "accept-encoding",
                "accept-language",
                "cookie",
                "range",
            ],
            "{head}"
        );
        assert!(
            head.contains("\r\nsec-ch-ua: \"Chromium\";v=\"141\""),
            "{head}"
        );
        assert!(
            head.contains("\r\nsec-ch-ua-platform: \"Windows\"\r\n"),
            "{head}"
        );
        assert!(
            head.contains("\r\nSec-Fetch-Site: same-origin\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nRange: bytes=0-0\r\n\r\n"), "{head}");
    }

    #[test]
    fn the_fetch_site_follows_where_the_referer_points() {
        let site = |referer: Option<&str>| {
            let t = chrome_target(
                referer
                    .map(|r| format!("Referer: {r}"))
                    .into_iter()
                    .collect(),
            );
            build_request_head("GET", &t, Some((0, 9)))
        };
        assert!(site(None).contains("Sec-Fetch-Site: none\r\n"));
        assert!(
            site(Some("https://other.example.org/p")).contains("Sec-Fetch-Site: cross-site\r\n")
        );
        assert!(site(Some("https://DOWNLOAD-center.example.com/p"))
            .contains("Sec-Fetch-Site: same-origin\r\n"));
    }

    #[test]
    fn a_header_the_user_typed_replaces_the_profile_default() {
        let t = chrome_target(vec![
            "Accept-Language: fa-IR".into(),
            "Accept-Encoding: br".into(),
        ]);
        let head = build_request_head("GET", &t, Some((0, 0)));
        assert_eq!(head.matches("Accept-Language").count(), 1, "{head}");
        assert!(head.contains("Accept-Language: fa-IR\r\n"), "{head}");
        assert!(head.contains("Accept-Encoding: br\r\n"), "{head}");
    }

    #[test]
    fn requests_outside_the_chrome_profile_keep_the_plain_header_set() {
        let sends_encoding =
            |t: &Target, range| build_request_head("GET", t, range).contains("Accept-Encoding");
        let firefox = Target::direct_tls("h", 443, "/o").with_headers(
            vec![],
            Some("Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0".into()),
        );
        assert!(!sends_encoding(&firefox, Some((0, 0))));
        let hydra = Target::direct_tls("h", 443, "/o");
        assert!(!sends_encoding(&hydra, Some((0, 0))));
        let plain = build_request_head("GET", &hydra, Some((0, 0)));
        assert!(plain.ends_with("Connection: close\r\n\r\n"), "{plain}");
    }

    #[test]
    fn the_profile_asks_for_an_unencoded_body_whole_or_ranged() {
        let t = chrome_target(vec![]);
        for (method, range) in [("GET", None), ("GET", Some((5, 9))), ("HEAD", None)] {
            let head = build_request_head(method, &t, range);
            assert!(head.contains("\r\nAccept-Encoding: identity\r\n"), "{head}");
            assert_eq!(head.contains("Range:"), range.is_some(), "{head}");
            assert!(head.ends_with("\r\n\r\n"), "{head}");
        }
    }

    #[test]
    fn an_encoded_body_is_refused_and_an_identity_one_is_not() {
        assert!(reject_content_encoding("HTTP/1.1 206 Partial Content\r\n\r\n").is_ok());
        assert!(
            reject_content_encoding("HTTP/1.1 206 X\r\nContent-Encoding: identity\r\n\r\n").is_ok()
        );
        let e = reject_content_encoding("HTTP/1.1 206 X\r\nContent-Encoding: gzip\r\n\r\n")
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("gzip"), "{e}");
    }

    #[tokio::test]
    async fn a_ranged_probe_answered_with_gzip_fails_instead_of_sizing_the_compressed_body() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut r = BufReader::new(sock.try_clone().unwrap());
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 20\r\n\
Connection: close\r\n\r\n",
                );
            }
        });
        let t = Target::direct("127.0.0.1", port, "/o");
        let err = probe_via_get(&crate::TcpConnector, &t).await.unwrap_err();
        assert!(err.to_string().contains("Content-Encoding"), "{err}");
    }

    #[tokio::test]
    async fn a_streamed_body_with_a_content_encoding_is_not_written_to_disk() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut r = BufReader::new(sock.try_clone().unwrap());
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                }
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nContent-Length: 3\r\n\
Connection: close\r\n\r\nabc",
                );
            }
        });
        let out = std::env::temp_dir().join(format!("hya-enc-{port}.bin"));
        let t = Target::direct("127.0.0.1", port, "/o");
        let err = fetch_streaming(&crate::TcpConnector, &t, out.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Content-Encoding"), "{err}");
        assert!(!out.exists(), "nothing was written");
    }

    /// A body that already arrived whole must not cost another read: the peer
    /// may hold the socket open, and the two-second patience would then be
    /// spent on every error against it.
    #[tokio::test]
    async fn a_body_already_in_hand_is_not_waited_on() {
        let body = "x".repeat(600);
        let mut head = format!("HTTP/1.1 403 Forbidden\r\n\r\n{body}").into_bytes();
        let before = head.len();
        // A reader that would block forever if it were consulted at all.
        let mut never = tokio::io::empty();
        let t0 = std::time::Instant::now();
        read_error_body(&mut never, &mut head).await;
        assert_eq!(head.len(), before, "nothing more was read");
        assert!(t0.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn truncate_never_splits_a_codepoint() {
        // Four-byte codepoints: a byte-offset slice would panic here.
        let s = "𝄞".repeat(10);
        assert_eq!(truncate_chars(&s, 3), format!("{}…", "𝄞".repeat(3)));
        assert_eq!(truncate_chars(&s, 100), s);
    }
}

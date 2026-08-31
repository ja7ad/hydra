//! HTTP/1.1 request construction, response-head parsing, probing, and the
//! per-range fetch path.
//!
//! Outgoing request headers are built centrally by [`build_request_head`]
//! to ensure consistency across probes and body fetches.

use crate::framebuf;
use crate::polite::Pace;
use crate::sink::SparseSink;
use crate::{Arrival, Connector, Target, READ_BUF};
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

impl Probe {
    /// This probe described a redirect rather than the object.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308) && self.location.is_some()
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
    pub fn suggested_filename(&self) -> Option<String> {
        let d = self.disposition.as_ref()?;
        let lower = d.to_ascii_lowercase();
        let idx = lower.find("filename=")?;
        let rest = d[idx + 9..].trim();
        let name = rest
            .trim_start_matches('"')
            .split(['"', ';'])
            .next()?
            .trim();
        // A server-supplied name must never escape the output directory.
        let base = std::path::Path::new(name)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())?;
        if base.is_empty() || base == "." || base == ".." {
            None
        } else {
            Some(base)
        }
    }
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
        Disposition::Close => "Connection: close\r\n\r\n",
        // HTTP/1.1 is keep-alive by default and the header is strictly redundant,
        // but it is sent explicitly: intermediaries and older origins are
        // measurably more consistent when told, and it makes a packet capture
        // state the intent rather than leaving it to be inferred from an absence.
        Disposition::Reuse => "Connection: keep-alive\r\n\r\n",
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
    req
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
        // A trailer section may follow the zero chunk; leaving it unread
        // would poison the socket, so it is not offered back.
        framed = saw_end && pending.is_empty();
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
    let mut s = c.connect(t).await?;
    let req = build_request_head("GET", t, None);
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
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
    let h = String::from_utf8_lossy(&head);
    let status: u16 = h
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
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
    let head = probe(c, t).await;
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
    while off < cur_hi {
        let want = pace.read_size(((cur_hi - off) as usize).min(READ_BUF));
        let n = s.read(&mut buf[..want]).await?;
        if n == 0 {
            break;
        }
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
    // The peer closing early is NOT success. A truncating origin advertises an
    // honest-looking Content-Length and then closes mid-body; returning Ok here
    // let the caller treat a short read as a delivered range, so the bytes that
    // never arrived were never re-requested. `UnexpectedEof` is retryable, which
    // is the correct disposition: the range may well arrive on a second attempt.
    // ---- return the connection for reuse, if and only if it is safe ----------
    //
    // The test is not "did the transfer succeed" but "does the client know exactly
    // where this response ended". Anything left unread in the socket becomes the
    // first bytes of whatever request goes out next, which corrupts that response
    // at the wrong file offsets while keeping a plausible length — the same failure
    // shape as the positional-write and double-count defects before it.
    //
    // Three separate reasons to refuse, and the third is peculiar to this
    // scheduler:
    //   * the server said `Connection: close` — it will not serve another request
    //     on this socket, so pooling it guarantees the next user a dead stream;
    //   * the body did not reach the far end we asked for (`off < hi_final`), so
    //     an unknown number of bytes is still arriving;
    //   * the far end MOVED (`hi_final < hi_at_request`), meaning a repair took
    //     this connection's tail. The loop stopped early by design and the server
    //     is still sending toward the original end, so the socket has unread body
    //     in it. Preemption remains free on the wire; the cost is that this one
    //     connection cannot be reused. Pooling it here would trade a bounded,
    //     already-paid cost for silent corruption.
    let hi_final = bound.get();
    let shrunk = hi_final < hi;
    let server_will_close = header_value(&head_str, "connection")
        .map(|v| v.to_ascii_lowercase().contains("close"))
        .unwrap_or(false);
    // A fourth reason, and the one that needs no repair to go wrong: the response
    // was not a `206`. A `200` to a request from offset 0 is accepted above,
    // because the bytes are the object's and they belong where they land — but its
    // body is the WHOLE object while this loop reads only as far as `hi`, so the
    // remainder is still in the socket. `off >= hi_final` is satisfied and says
    // nothing. Only a partial response is known to have ended where the client
    // stopped reading.
    let framed_exactly = status == 206 && !over_read;
    if let Some(p) = pool.as_ref() {
        if framed_exactly && !shrunk && !server_will_close && off >= hi_final {
            p.put(&t, s);
        }
    }

    // A shrink is not a truncation. If the bound moved below where the loop
    // stopped, the remainder was deliberately handed to another connection and
    // this range is complete as redefined; only a genuinely short body is an error.
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
        let n = s.read(&mut read_buf[..want]).await?;
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
fn retarget(prev: &Target, location: &str) -> Option<Target> {
    let loc = location.trim();
    if loc.starts_with("https://") {
        return None;
    }
    if let Some(rest) = loc.strip_prefix("http://") {
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        return Some(match &prev.origin {
            Some(_) => Target::via_proxy(&prev.host, prev.port, auth, path),
            None => {
                let (h, p) = match auth.rsplit_once(':') {
                    Some((h, p)) => (h.to_string(), p.parse().unwrap_or(80)),
                    None => (auth.to_string(), 80),
                };
                Target::direct(&h, p, path)
            }
        });
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
}

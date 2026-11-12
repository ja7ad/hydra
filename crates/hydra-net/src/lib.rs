// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// This library is intentionally permissive, not GPL, even though the `hydra`
// binary that ships it is GPL-3.0-or-later: Rust links statically, so copyleft
// here would propagate to every downstream crate. See LICENSING.md.

//! HTTP/1.1 range transport driving the `hydra-core` scheduler.
//!
//! Deliberately minimal: a hand-rolled HTTP/1.1 client over `tokio::net::TcpStream`,
//! because the point of this crate is to show that the scheduler core needs
//! nothing from the transport but *bytes arrived* and *when*. The same core runs
//! under the discrete-event simulator with no changes.
//!
//! Two properties the harness measures:
//!
//! * **Memory is independent of object size.** Bytes are written to their file
//!   offset as they arrive (positioned write, `pwrite`), never buffered whole and
//!   never reassembled. Resident memory is `O(connections × buffer)`.
//! * **The scheduler is I/O-free.** Everything in `hydra-core` is driven by
//!   `on_bytes` / `tick`; this crate contains all the syscalls.
//!
//! # Where SIMD is and is not used
//!
//! Three byte-parallel paths matter here, and each gets its vectorization from a
//! different place — deliberately, because hand-written intrinsics are a
//! maintenance and correctness cost that has to be paid for by a measurement:
//!
//! * **Byte search** (`find_crlf`, `find_crlf2`, [`framebuf::FrameBuf`]) uses
//!   `memchr`, which does runtime feature detection and dispatch: AVX2 or SSE2
//!   on x86-64, NEON on aarch64, scalar elsewhere. One binary is correct and
//!   fast on every target, with no `unsafe` in this crate.
//! * **Hex encoding** ([`digest::to_lower_hex`]) is a table lookup over a
//!   preallocated buffer, shaped so LLVM autovectorizes it for whatever
//!   `target-cpu` the build selects. Measured 11.8x over the `write!` form it
//!   replaced, which is all the gain intrinsics could have bought.
//! * **Hashing and erasure coding** are delegated: `sha2` dispatches to SHA-NI
//!   on x86-64 and the ARMv8 SHA-2 extensions via `cpufeatures`, `blake3` to
//!   AVX2/AVX-512/NEON, and `reed-solomon-simd` to its own kernels. These are
//!   the genuinely compute-bound operations and they are already
//!   hardware-accelerated by crates that test those paths on real silicon.
//!
//! Vectorized paths are covered by differential tests against scalar references,
//! maintaining safety and high performance across architectures.

pub mod base64;
pub mod cookies;
pub mod digest;
pub mod filename;
pub mod framebuf;
pub mod ftp;
pub mod ftp_origin;
pub mod http_scheme;
pub mod manifest;
pub mod metalink;
pub mod origin;
pub mod parity;
pub mod polite;
pub mod redirect;
pub mod scheme;
pub mod signed;
pub mod socks;
pub mod stream_digest;
pub mod tls;
pub mod url;
pub mod xml;
pub mod zipdir;

/// The identity sent when the user supplies no `--user-agent`.
///
/// One definition for the whole workspace: the CLI's flag default and the
/// queue manager reference it too, so a version bump cannot leave the paths
/// disagreeing about who they say they are.
pub const DEFAULT_USER_AGENT: &str = concat!("hydra/", env!("CARGO_PKG_VERSION"));

/// How many mirrors a mirror-list transfer probes at once.
///
/// Each probe goes to a different host, so this is not a politeness limit — the
/// per-host ceilings answer that — only a guard against a forty-mirror document
/// opening forty sockets at once and hitting an fd limit. Sixteen because the
/// cost is latency paid before the first byte: against a twelve-mirror Fedora
/// document, six in flight took two waves and 4.1 s of setup; one wave removes
/// almost all of it.
pub const PROBE_FANOUT: usize = 16;

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpStream;

/// Per-connection read buffer. The only memory that scales with concurrency.
pub const READ_BUF: usize = 64 * 1024;

/// The payload of an `Authorization: Basic` (or `Proxy-Authorization`) header
/// for these credentials: `user:pass`, base64-encoded, without the scheme word.
pub fn basic_auth(user: &str, pass: &str) -> String {
    base64::encode(format!("{user}:{pass}").as_bytes())
}

#[derive(Debug)]
pub struct Arrival {
    pub conn: usize,
    /// Absolute file offset these bytes landed at. The scheduler credits by
    /// offset, not by cursor, so a response still draining from a superseded
    /// range cannot advance the cursor of the range the connection holds now.
    pub off: u64,
    pub bytes: u64,
    pub at: f64,
    pub dt: f64,
}

/// Bytes a request-target may carry verbatim: the unreserved and reserved sets
/// of RFC 3986, plus `%` so an already-encoded path survives unchanged.
fn target_byte_is_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'%'
        )
}

/// Percent-encode everything in `path` that a request-target cannot carry.
///
/// A URL is pasted as the user reads it — `/d/guest/あけあけ/packs/x.rar` — but a
/// request-target is ASCII, so the raw UTF-8 goes on the wire as bytes an origin
/// is entitled to reject: nginx-fronted hosts answer `400 Bad Request` for an
/// address every browser fetches. A space, or any other excluded ASCII
/// character, fails the same way.
///
/// Encoding here rather than in each URL parser is what gives the CLI, the GUI,
/// the FFI and the stream front ends one rule, and it leaves the URL as typed
/// wherever it is displayed, written to a resume sidecar, or compared across a
/// redirect chain.
///
/// `%` passes through, so a path that already carries escapes is not encoded a
/// second time — the same address in either spelling reaches the same object.
fn encode_request_target(path: &str) -> std::borrow::Cow<'_, str> {
    if path.bytes().all(target_byte_is_safe) {
        return std::borrow::Cow::Borrowed(path);
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(path.len() + 16);
    for &b in path.as_bytes() {
        if target_byte_is_safe(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0xF) as usize] as char);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Request headers that authenticate the request to ONE origin, and so may not
/// follow a redirect off it.
///
/// The set the Fetch standard strips on a cross-origin redirect, less the
/// response-only names. `Cookie` is here for a header written out by hand;
/// a jar scopes its own (see [`Target::with_jar`]).
const ORIGIN_CREDENTIALS: &[&str] = &["authorization", "proxy-authorization", "cookie"];

/// A verbatim `Name: value` header line names this field.
pub(crate) fn is_field(line: &str, name: &str) -> bool {
    line.len() > name.len()
        && line.as_bytes()[name.len()] == b':'
        && line[..name.len()].eq_ignore_ascii_case(name)
}

#[derive(Clone, Debug)]
pub struct Target {
    /// Host to CONNECT the socket to. For a direct fetch this is the origin; for
    /// a proxied fetch it is the proxy.
    pub host: String,
    pub port: u16,
    pub path: String,
    /// Connect with TLS.
    pub tls: bool,
    /// Extra request headers, verbatim `Name: value` lines (`-H`).
    pub headers: Vec<String>,
    /// `User-Agent` to send.
    pub agent: Option<String>,
    /// Origin authority (`host` or `host:port`) when the request must be sent in
    /// absolute form through a forward proxy. `None` = origin-form request to a
    /// directly-connected origin.
    ///
    /// RFC 9112 §3.2.2: a client sending to a proxy MUST send the target URI in
    /// absolute form. Carrying it here keeps the proxy decision entirely inside
    /// the transport, so the scheduler core is unchanged.
    pub origin: Option<String>,
}

impl Target {
    /// Direct origin-form target.
    pub fn direct(host: &str, port: u16, path: &str) -> Self {
        Self {
            host: host.into(),
            port,
            path: path.into(),
            origin: None,
            tls: false,
            headers: Vec::new(),
            agent: None,
        }
    }

    /// Direct target over TLS.
    pub fn direct_tls(host: &str, port: u16, path: &str) -> Self {
        Self {
            tls: true,
            ..Self::direct(host, port, path)
        }
    }

    /// Name to present in SNI and to validate the certificate against.
    ///
    /// This is the ORIGIN authority, never the socket peer: through a proxy the
    /// socket connects to the proxy while the certificate belongs to the origin.
    pub fn tls_server_name(&self) -> &str {
        match &self.origin {
            Some(o) => o.split(':').next().unwrap_or(o),
            None => &self.host,
        }
    }

    /// The ORIGIN host and port, regardless of how the connection is routed.
    ///
    /// A SOCKS proxy needs this: the TCP connection goes to the proxy, but the
    /// handshake must name the origin. `host`/`port` may hold the HTTP proxy's
    /// address, so they cannot be used directly.
    pub fn origin_endpoint(&self) -> (String, u16) {
        match &self.origin {
            Some(o) => match o.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    p.parse().unwrap_or(if self.tls { 443 } else { 80 }),
                ),
                None => (o.clone(), if self.tls { 443 } else { 80 }),
            },
            None => (self.host.clone(), self.port),
        }
    }

    /// Authority for a proxy `CONNECT`: the origin host and port.
    pub fn proxy_authority(&self) -> &str {
        self.origin.as_deref().unwrap_or(&self.host)
    }

    /// The absolute URL this target addresses.
    ///
    /// Built from the ORIGIN endpoint, never the socket peer: through a
    /// forward proxy `host`/`port` name the proxy, and an address keyed on
    /// those would read every hop of a redirect chain as the same place. See
    /// [`crate::polite::RedirectChain`], which is what asks.
    pub fn url(&self) -> String {
        let (host, port) = self.origin_endpoint();
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{host}:{port}{}", self.path)
    }

    /// Attach extra request headers and a `User-Agent`, as the CLI flags request.
    pub fn with_headers(mut self, headers: Vec<String>, agent: Option<String>) -> Self {
        self.headers = headers;
        self.agent = agent;
        self
    }

    /// Attach `headers` and `agent` to a target a redirect chain reached from
    /// `source`, dropping the ones that authenticate `source` rather than this
    /// address.
    ///
    /// `source` is the address the USER named — the start of the chain, not the
    /// previous hop. That is what makes `a -> b -> a` restore at `a` what it
    /// dropped at `b`, and it is the rule curl follows for the same reason: the
    /// credential was typed for `a`, so leaving `a` is what revokes it and
    /// returning is what makes it applicable again.
    ///
    /// Ordinary headers and the `User-Agent` survive every hop. A hop that
    /// leaves the origin drops [`ORIGIN_CREDENTIALS`]: a bearer token typed for
    /// one host must not be handed to whatever that host redirects to, and a
    /// `https -> http` hop must not put it on the wire in the clear. Cookies
    /// have their own host-scoped path through [`Target::with_jar`]; the entry
    /// here is for a `Cookie:` a user wrote out by hand, which nothing else
    /// would scope.
    pub fn with_headers_from(
        self,
        source: &Target,
        headers: Vec<String>,
        agent: Option<String>,
    ) -> Self {
        if self.same_origin(source) {
            return self.with_headers(headers, agent);
        }
        let kept = headers
            .into_iter()
            .filter(|h| !ORIGIN_CREDENTIALS.iter().any(|name| is_field(h, name)))
            .collect();
        self.with_headers(kept, agent)
    }

    /// Both targets address the same origin: scheme, host and port (RFC 6454).
    ///
    /// The ORIGIN, never the socket peer, so two hops through the same forward
    /// proxy to different sites do not read as one origin. Hosts compare
    /// case-insensitively because DNS does, and nothing lowercases them on the
    /// way in.
    fn same_origin(&self, other: &Target) -> bool {
        let (a_host, a_port) = self.origin_endpoint();
        let (b_host, b_port) = other.origin_endpoint();
        self.tls == other.tls && a_port == b_port && a_host.eq_ignore_ascii_case(&b_host)
    }

    /// Attach the `Cookie:` header this jar produces for THIS target.
    ///
    /// The host is read off the target rather than passed in, and it is the
    /// ORIGIN host, never the proxy the socket connects to. That is the
    /// security boundary made structural: there is no argument a caller
    /// following a redirect could get wrong, because the only host in scope is
    /// the one the request is about to go to.
    ///
    /// A jar with something to say REPLACES any `Cookie:` already attached,
    /// including one from `-H`: the two are answers to the same question and
    /// only one can go on the wire. Calling this again after a redirect is
    /// therefore correct rather than cumulative.
    ///
    /// A jar with nothing for this host changes nothing. That is what keeps a
    /// run with no cookie flag byte-identical — every target is built through
    /// here, and an empty jar that stripped the header would silently delete
    /// the `-H 'Cookie: …'` that was the only way to do this before.
    pub fn with_jar(mut self, jar: &crate::cookies::CookieJar, now: u64) -> Self {
        let (host, _) = self.origin_endpoint();
        if let Some(v) = jar.header_value(&host, &self.path, self.tls, now) {
            self.headers.retain(|h| !is_field(h, "cookie"));
            self.headers.push(format!("Cookie: {v}"));
        }
        self
    }

    fn user_agent(&self) -> &str {
        self.agent.as_deref().unwrap_or(DEFAULT_USER_AGENT)
    }

    /// The extra headers the request line is followed by.
    ///
    /// Through a CONNECT tunnel the request reaches the ORIGIN, not the proxy
    /// that was just paid with `Proxy-Authorization`: the login is the
    /// tunnel's (RFC 9110 §11.7.2) and does not go through it.
    fn extra_headers(&self) -> impl Iterator<Item = &str> {
        let tunnelled = self.tls && self.origin.is_some();
        self.headers
            .iter()
            .map(String::as_str)
            .filter(move |h| !(tunnelled && is_field(h, "proxy-authorization")))
    }

    /// The `Proxy-Authorization` value among the headers, for the CONNECT
    /// tunnel a TLS connection through a forward proxy opens first. The
    /// front-ends attach the line for their plain requests, and a proxy that
    /// wants a login on those wants it on the tunnel too.
    pub fn proxy_authorization(&self) -> Option<&str> {
        const NAME: &str = "proxy-authorization";
        self.headers
            .iter()
            .find(|h| is_field(h, NAME))
            .map(|h| h[NAME.len() + 1..].trim())
    }

    /// Absolute-form target routed through a forward proxy.
    pub fn via_proxy(proxy_host: &str, proxy_port: u16, origin_host: &str, path: &str) -> Self {
        Self {
            host: proxy_host.into(),
            port: proxy_port,
            path: path.into(),
            origin: Some(origin_host.into()),
            tls: false,
            headers: Vec::new(),
            agent: None,
        }
    }

    /// The request-target for the start line, and the `Host` header value.
    fn request_target(&self) -> (String, std::borrow::Cow<'_, str>) {
        let target = encode_request_target(&self.path);
        match &self.origin {
            Some(o) => (
                format!("http://{o}{target}"),
                std::borrow::Cow::Borrowed(o.as_str()),
            ),
            None => (target.into_owned(), self.authority()),
        }
    }

    /// The `Host:` value for a direct request: the name, plus the port whenever
    /// it is not the scheme's default.
    ///
    /// RFC 9110 §7.2 requires the port here, and omitting it is not cosmetic.
    /// An AWS SigV4 presigned URL signs `host` as part of the canonical
    /// request, so a signature minted for `s3q.ait.dtu.dk:9000` is rejected
    /// outright when the request arrives claiming `s3q.ait.dtu.dk` —
    /// `SignatureDoesNotMatch`, on a URL that is perfectly valid and that curl
    /// fetches from the same machine a second later. Any object store on a
    /// non-default port was unreachable because of this.
    ///
    /// Borrowed in the common case: the default port is the overwhelming
    /// majority, and this runs once per request head.
    fn authority(&self) -> std::borrow::Cow<'_, str> {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            std::borrow::Cow::Borrowed(self.host.as_str())
        } else {
            std::borrow::Cow::Owned(format!("{}:{}", self.host, self.port))
        }
    }
}

/// The far end of an in-flight range, shared between the scheduler loop and the
/// fetch task streaming that range.
///
/// # Why a range's end has to be mutable
///
/// This scheduler's central claim is that shrinking a laggard's range costs
/// nothing: an HTTP range request names both ends, and the far end is enforced
/// by the client, so the client can simply decide to stop earlier and the server
/// is never told. No cancellation, no round trip.
///
/// That is a true statement about HTTP and it was a false statement about this
/// code. `fetch_range` used to take `hi: u64` by value, so the fetch loop tested
/// `off < hi` against a snapshot taken when the task was spawned. A repair moved
/// the scheduler's copy of the range and the running task never learned: the
/// victim went on requesting — and receiving — the exact span that had just been
/// handed to somebody else. Both connections pulled it, over one bottleneck,
/// and the resulting slowdown looked to the scheduler like fresh divergence,
/// which triggered another repair. At n=8 that loop turned 0 necessary repairs
/// into 32-49 and cost ~2.2x the fluid optimum.
///
/// Making the bound an `AtomicU64` behind an `Arc` is what makes preemption cost
/// what the theory says it costs. It is read once per `read()` — a relaxed load
/// against a 64 KiB buffer fill, which is not measurable next to the syscall.
///
/// # What "free" honestly means
///
/// Free on the wire, not free in bytes already in flight. When the loop stops at
/// the lowered bound, the server is still sending toward the original end, so up
/// to roughly one bandwidth-delay product may already be in the socket and is
/// discarded when the stream drops. That is bounded by the receive window and
/// does not scale with the size of the span given away, which is the whole
/// difference between this and re-requesting.
#[derive(Clone, Debug)]
pub struct Watermark(Arc<std::sync::atomic::AtomicU64>);

impl Watermark {
    /// A bound nobody will move. Used by every caller that fetches a range
    /// without a scheduler above it (`fetch_range_retry`, the static-split
    /// policies, the FTP path).
    pub fn fixed(hi: u64) -> Self {
        Watermark(Arc::new(std::sync::atomic::AtomicU64::new(hi)))
    }

    /// The current far end.
    #[inline]
    pub fn get(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lower the far end to `hi`.
    ///
    /// Monotonically decreasing by construction: a repair only ever gives work
    /// away, and `fetch_max` on the negation is not worth the complexity, so
    /// this uses `fetch_min` to make the direction an invariant rather than a
    /// convention. Raising a bound would hand a connection bytes another
    /// connection may already hold, which is a coverage violation, not an
    /// optimisation.
    pub fn shrink_to(&self, hi: u64) {
        self.0.fetch_min(hi, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Anything that can open a byte stream to a target.
///
/// # The `pool` hook
///
/// A connector may carry a connection pool that OUTLIVES a single transfer. This
/// exists because the client's own size probe talks to the origin before the
/// transfer starts, and without somewhere shared to put that connection its
/// handshake is thrown away and the transfer redials the host it was just talking
/// to. Measured on a live TLS path, that was 1.6-2.0 s of setup on a transfer whose
/// body took 3.7-5.5 s — a significant unnecessary latency gap and pure
/// waste rather than a design cost.
///
/// The default returns `None`, meaning "no shared pool": the transfer then creates
/// its own, which is correct for the in-process test connector and for any caller
/// that has not spoken to the origin yet.
///
/// This exists because the transport must be swappable: `TcpConnector` for real
/// networks, `DuplexConnector` (in `origin`) for hermetic tests. The scheduler
/// core sees neither -- it only ever receives `on_bytes`.
pub trait Connector: Send + Sync + 'static {
    type Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send;
    fn connect<'a>(
        &'a self,
        t: &'a Target,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + 'a>>;

    /// A connection pool shared across transfers, if this connector keeps one.
    ///
    /// Default `None`: the transfer then owns its own pool, which is right for a
    /// caller that has not yet contacted the origin. Returning a pool lets earlier
    /// requests — notably the size probe — contribute their established connections
    /// instead of having them closed and re-dialled.
    fn pool(&self) -> Option<crate::pool::SharedPool<Self::Stream>> {
        None
    }
}

/// Real TCP.
pub struct TcpConnector;

impl Connector for TcpConnector {
    type Stream = TcpStream;
    fn connect<'a>(
        &'a self,
        t: &'a Target,
    ) -> Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send + 'a>> {
        // Through the cached resolver, not `TcpStream::connect((host, port))`: that
        // helper resolves on tokio's blocking pool on every call, so n connections to
        // one host paid n resolutions plus n rounds of blocking-thread churn. See
        // `tls::connect_family` for the measurement.
        Box::pin(async move {
            crate::tls::connect_family(&t.host, t.port, crate::tls::IpFamily::Any).await
        })
    }
}

pub mod guise;
pub mod http;
pub mod pool;
pub mod sink;
pub mod transfer;

pub use cookies::{Cookie, CookieJar};

pub use guise::{probe_admitted, Guise, BROWSER_USER_AGENT};

pub use http::{
    describe_status, fetch_object, fetch_range_retry, fetch_small, fetch_small_range,
    fetch_streaming, fetch_streaming_observed, header_all, header_lookup, probe, probe_resilient,
    probe_size_via_range, probe_via_get, Probe, Redirect,
};
pub use redirect::{html_redirect, html_redirect_target};
pub use sink::{allocated_bytes, Growth, SparseSink};
pub use transfer::{
    run_transfer, run_transfer_cancellable, run_transfer_into, run_transfer_observed,
    run_transfer_paced, run_transfer_tick, run_transfer_with_reserves, Bench, OnSubstitute,
    Reserve,
};

pub use metalink::{MetaUrl, Metalink, MetalinkFile};

pub use socks::{Proxy, ProxyKind};
pub use tls::{connect_family, IpFamily, MaybeTls, TlsCapableConnector};

#[cfg(test)]
mod credential_tests {
    #[test]
    fn basic_auth_is_the_padded_base64_of_user_colon_pass() {
        assert_eq!(super::basic_auth("me", "pw"), "bWU6cHc=");
        assert_eq!(super::basic_auth("", ""), "Og==");
        assert_eq!(
            super::DEFAULT_USER_AGENT,
            format!("hydra/{}", env!("CARGO_PKG_VERSION"))
        );
        assert_ne!(super::DEFAULT_USER_AGENT, "hydra/0.1");
    }
}

#[cfg(test)]
mod target_jar_tests {
    use super::*;
    use crate::cookies::CookieJar;

    fn cookie_headers(t: &Target) -> Vec<&str> {
        t.headers
            .iter()
            .filter(|h| is_field(h, "cookie"))
            .map(String::as_str)
            .collect()
    }

    /// The header is derived from the target's OWN host, so a caller following
    /// a redirect cannot hand the previous hop's session to the next one.
    #[test]
    fn a_jar_answers_for_the_host_the_request_is_going_to() {
        let mut jar = CookieJar::new();
        jar.add_pairs("sid=abc", "files.example.org");

        let hit = Target::direct_tls("files.example.org", 443, "/x").with_jar(&jar, 0);
        assert_eq!(cookie_headers(&hit), ["Cookie: sid=abc"]);

        let miss = Target::direct_tls("other.test", 443, "/x").with_jar(&jar, 0);
        assert!(cookie_headers(&miss).is_empty());
    }

    /// Through a proxy the socket connects to the proxy while the cookies
    /// belong to the origin. Keying on `host` would send one site's session to
    /// every site reached through the same proxy.
    #[test]
    fn a_proxied_target_is_answered_for_its_origin_not_its_proxy() {
        let mut jar = CookieJar::new();
        jar.add_pairs("sid=abc", "origin.example.org");
        let t =
            Target::via_proxy("proxy.test", 8080, "origin.example.org:80", "/x").with_jar(&jar, 0);
        assert_eq!(cookie_headers(&t), ["Cookie: sid=abc"]);

        jar = CookieJar::new();
        jar.add_pairs("leak=1", "proxy.test");
        let t =
            Target::via_proxy("proxy.test", 8080, "origin.example.org:80", "/x").with_jar(&jar, 0);
        assert!(cookie_headers(&t).is_empty());
    }

    /// An empty jar must not touch the headers. Every target is built through
    /// `with_jar`, so stripping here would delete the `-H 'Cookie: …'` that was
    /// the only way to send a session before the jar existed.
    #[test]
    fn an_empty_jar_leaves_an_explicit_header_alone() {
        let t = Target::direct("example.org", 80, "/")
            .with_headers(vec!["Cookie: typed=1".into()], None)
            .with_jar(&CookieJar::new(), 0);
        assert_eq!(cookie_headers(&t), ["Cookie: typed=1"]);
    }

    #[test]
    fn a_jar_with_something_to_say_replaces_the_explicit_header_exactly_once() {
        let mut jar = CookieJar::new();
        jar.add_pairs("sid=fromjar", "example.org");
        let t = Target::direct("example.org", 80, "/")
            .with_headers(vec!["Cookie: typed=1".into(), "X-Trace: 1".into()], None)
            .with_jar(&jar, 0)
            .with_jar(&jar, 0);
        assert_eq!(cookie_headers(&t), ["Cookie: sid=fromjar"]);
        assert!(t.headers.iter().any(|h| h == "X-Trace: 1"));
    }

    /// A `Secure` cookie is withheld from a plaintext target, which is the one
    /// property of the jar that depends on how the target connects.
    #[test]
    fn tls_decides_whether_a_secure_cookie_is_attached() {
        let mut jar = CookieJar::new();
        jar.store_response(
            "HTTP/1.1 200 OK\r\nSet-Cookie: sid=abc; Secure\r\n\r\n",
            "example.org",
            "/",
            0,
        );
        assert!(
            cookie_headers(&Target::direct("example.org", 80, "/").with_jar(&jar, 0)).is_empty()
        );
        assert_eq!(
            cookie_headers(&Target::direct_tls("example.org", 443, "/").with_jar(&jar, 0)),
            ["Cookie: sid=abc"]
        );
    }
}

#[cfg(test)]
mod proxy_login_tests {
    use super::*;

    #[test]
    fn the_proxy_login_is_read_off_the_headers_whatever_its_case() {
        let t = Target::via_proxy("proxy.test", 3128, "h:443", "/")
            .with_headers(vec!["proxy-authorization:  Basic cHc= ".into()], None);
        assert_eq!(t.proxy_authorization(), Some("Basic cHc="));
        assert_eq!(
            Target::direct("h", 80, "/")
                .with_headers(vec!["Authorization: Basic cHc=".into()], None)
                .proxy_authorization(),
            None,
            "the origin's login is not the proxy's"
        );
    }

    /// Through a tunnel the request reaches the origin, which is not owed
    /// the proxy's password; on a plain proxied request the proxy reads it.
    #[test]
    fn a_tunnelled_request_keeps_the_proxy_login_off_the_origin() {
        let headers = vec![
            "Proxy-Authorization: Basic cHc=".to_string(),
            "X-Api-Key: k".to_string(),
        ];
        let mut tunnelled =
            Target::via_proxy("proxy.test", 3128, "h:443", "/").with_headers(headers.clone(), None);
        tunnelled.tls = true;
        assert_eq!(
            tunnelled.extra_headers().collect::<Vec<_>>(),
            ["X-Api-Key: k"]
        );
        assert_eq!(tunnelled.proxy_authorization(), Some("Basic cHc="));

        let plain = Target::via_proxy("proxy.test", 3128, "h:80", "/").with_headers(headers, None);
        assert_eq!(
            plain.extra_headers().collect::<Vec<_>>(),
            ["Proxy-Authorization: Basic cHc=", "X-Api-Key: k"]
        );
    }
}

#[cfg(test)]
mod target_redirect_tests {
    use super::*;

    fn headers() -> Vec<String> {
        [
            "Authorization: Bearer secret",
            "Proxy-Authorization: Basic cHc=",
            "Cookie: sid=secret",
            "X-Api-Key: also-secret-but-not-scoped",
            "Accept: */*",
        ]
        .iter()
        .map(|h| h.to_string())
        .collect()
    }

    fn names(t: &Target) -> Vec<&str> {
        t.headers
            .iter()
            .map(|h| h.split(':').next().unwrap_or(""))
            .collect()
    }

    /// The reported bug: a hop used to arrive with no headers and no agent at
    /// all, so `-H 'X-Api-Key: …'` and `-U` were silently lost after any
    /// redirect.
    #[test]
    fn a_same_origin_hop_carries_everything_it_was_given() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let hop = Target::direct_tls("example.org", 443, "/b").with_headers_from(
            &source,
            headers(),
            Some("hydra-test/1".into()),
        );
        assert_eq!(
            names(&hop),
            [
                "Authorization",
                "Proxy-Authorization",
                "Cookie",
                "X-Api-Key",
                "Accept"
            ]
        );
        assert_eq!(hop.agent.as_deref(), Some("hydra-test/1"));
    }

    /// The other half: restoring headers unconditionally would hand a bearer
    /// token to whatever the first host redirects to.
    #[test]
    fn a_hop_to_another_host_drops_the_credentials_and_keeps_the_rest() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let hop = Target::direct_tls("elsewhere.test", 443, "/b").with_headers_from(
            &source,
            headers(),
            Some("hydra-test/1".into()),
        );
        assert_eq!(names(&hop), ["X-Api-Key", "Accept"]);
        // The agent is not a credential and identifies the client, not the
        // account: it survives the hop the token does not.
        assert_eq!(hop.agent.as_deref(), Some("hydra-test/1"));
    }

    /// Same host, plaintext: the token would go on the wire where anyone on the
    /// path can read it, which is exactly what it must not do.
    #[test]
    fn a_hop_that_drops_tls_is_not_the_same_origin() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let hop =
            Target::direct("example.org", 443, "/b").with_headers_from(&source, headers(), None);
        assert_eq!(names(&hop), ["X-Api-Key", "Accept"]);
    }

    #[test]
    fn a_hop_to_another_port_on_the_same_host_is_not_the_same_origin() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let hop = Target::direct_tls("example.org", 8443, "/b").with_headers_from(
            &source,
            headers(),
            None,
        );
        assert_eq!(names(&hop), ["X-Api-Key", "Accept"]);
    }

    /// DNS is case-insensitive and nothing lowercases a host on the way in, so
    /// a `Location` that differs only in case is the same place.
    #[test]
    fn host_case_alone_does_not_make_a_different_origin() {
        let source = Target::direct_tls("Example.ORG", 443, "/a");
        let hop = Target::direct_tls("example.org", 443, "/b").with_headers_from(
            &source,
            headers(),
            None,
        );
        assert!(names(&hop).contains(&"Authorization"));
    }

    /// Through a forward proxy every hop connects to the SAME socket peer, so
    /// comparing `host` rather than the origin would read two unrelated sites
    /// as one and hand the first one's token to the second.
    #[test]
    fn two_sites_behind_one_proxy_are_not_the_same_origin() {
        let source = Target::via_proxy("proxy.test", 8080, "example.org:443", "/a");
        let hop = Target::via_proxy("proxy.test", 8080, "elsewhere.test:443", "/b")
            .with_headers_from(&source, headers(), None);
        assert_eq!(names(&hop), ["X-Api-Key", "Accept"]);

        let same = Target::via_proxy("proxy.test", 8080, "example.org:443", "/b")
            .with_headers_from(&source, headers(), None);
        assert!(names(&same).contains(&"Authorization"));
    }

    /// `source` is the address the user named, not the previous hop: a chain
    /// that leaves the origin and comes back is entitled to the credential
    /// again, because it was typed for that origin.
    #[test]
    fn a_chain_that_returns_to_the_first_origin_is_trusted_again() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let away = Target::direct_tls("elsewhere.test", 443, "/b").with_headers_from(
            &source,
            headers(),
            None,
        );
        assert!(!names(&away).contains(&"Authorization"));

        let back = Target::direct_tls("example.org", 443, "/c").with_headers_from(
            &source,
            headers(),
            None,
        );
        assert!(names(&back).contains(&"Authorization"));
    }

    /// The filter matches a field NAME, not the line: a header whose value
    /// merely mentions one must not be mistaken for it.
    #[test]
    fn only_the_field_name_decides_what_is_a_credential() {
        let source = Target::direct_tls("example.org", 443, "/a");
        let hop = Target::direct_tls("elsewhere.test", 443, "/b").with_headers_from(
            &source,
            vec![
                "X-Note: authorization: none".into(),
                "AUTHORIZATION: Bearer secret".into(),
            ],
            None,
        );
        assert_eq!(names(&hop), ["X-Note"]);
    }
}

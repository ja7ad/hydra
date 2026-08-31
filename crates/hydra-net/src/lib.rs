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

pub mod digest;
pub mod framebuf;
pub mod ftp;
pub mod ftp_origin;
pub mod http_scheme;
pub mod manifest;
pub mod origin;
pub mod parity;
pub mod polite;
pub mod redirect;
pub mod scheme;
pub mod socks;
pub mod stream_digest;
pub mod tls;

/// The identity sent when the user supplies no `--user-agent`.
///
/// One definition for the whole workspace: the CLI's flag default and the
/// queue manager reference it too, so a version bump cannot leave the paths
/// disagreeing about who they say they are.
pub const DEFAULT_USER_AGENT: &str = "hydra/0.1";

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpStream;

/// Per-connection read buffer. The only memory that scales with concurrency.
pub const READ_BUF: usize = 64 * 1024;

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

    /// Attach extra request headers and a `User-Agent`, as the CLI flags request.
    pub fn with_headers(mut self, headers: Vec<String>, agent: Option<String>) -> Self {
        self.headers = headers;
        self.agent = agent;
        self
    }

    fn user_agent(&self) -> &str {
        self.agent.as_deref().unwrap_or(DEFAULT_USER_AGENT)
    }

    fn extra_headers(&self) -> &[String] {
        &self.headers
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
    fn request_target(&self) -> (String, &str) {
        match &self.origin {
            Some(o) => (format!("http://{}{}", o, self.path), o.as_str()),
            None => (self.path.clone(), self.host.as_str()),
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

pub mod http;
pub mod pool;
pub mod sink;
pub mod transfer;

pub use http::{
    fetch_object, fetch_range_retry, fetch_small, fetch_streaming, fetch_streaming_observed,
    header_lookup, probe, probe_resilient, probe_size_via_range, probe_via_get, Probe, Redirect,
};
pub use redirect::{html_redirect, html_redirect_target};
pub use sink::SparseSink;
pub use transfer::{
    run_transfer, run_transfer_cancellable, run_transfer_into, run_transfer_observed,
    run_transfer_paced, run_transfer_tick,
};

pub use socks::{Proxy, ProxyKind};
pub use tls::{connect_family, IpFamily, MaybeTls, TlsCapableConnector};

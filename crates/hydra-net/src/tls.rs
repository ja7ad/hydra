//! TLS transport for HTTPS connections using rustls.
//!
//! Provides TLS encryption and verification with root certificate support,
//! session resumption caching, and SNI configuration.
//!
//! Certificate verification can be disabled, because self-signed certificates on
//! internal mirrors are a real situation. It is not quiet about it: the flag warns
//! on every use. A downloader that silently accepted any certificate would let an
//! interception replace the object with something else and still report a
//! successful verified transfer.

use crate::{Connector, Target};
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector as RustlsConnector;

/// A stream that is either plaintext or TLS.
///
/// An enum rather than a boxed trait object: the transport does millions of small
/// reads on this, and a vtable dispatch per read is a measurable cost for no gain
/// when there are exactly two variants.
pub enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for MaybeTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for MaybeTls {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Open a CONNECT tunnel through a forward proxy.
///
/// Required before a TLS handshake on a proxied target: cleartext absolute-form
/// requests work because the proxy parses the request line, and an encrypted
/// request line cannot be parsed. A 2xx means the tunnel is open and the socket is
/// now end-to-end with the origin.
///
/// `proxy_auth` is the `Proxy-Authorization` value the proxy is owed. The
/// tunnel is the proxy hop, so a proxy that wants a login on every
/// absolute-form request wants it here too, and answers 407 without it.
async fn connect_tunnel(
    sock: &mut TcpStream,
    authority: &str,
    proxy_auth: Option<&str>,
) -> io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut req = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n"
    );
    if let Some(auth) = proxy_auth {
        req.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    req.push_str("\r\n");
    sock.write_all(req.as_bytes()).await?;
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    // Read only to the end of the header block: everything after it belongs to the
    // TLS handshake and must be left in the socket, not consumed here.
    while !buf.ends_with(b"\r\n\r\n") {
        let n = sock.read(&mut byte).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy closed the connection during CONNECT",
            ));
        }
        buf.push(byte[0]);
        if buf.len() > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy CONNECT response header exceeded 8 KiB",
            ));
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 407 {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("proxy refused CONNECT to {authority}: status 407, it wants a login"),
        ))
    } else {
        Err(io::Error::other(format!(
            "proxy refused CONNECT to {authority}: status {status}"
        )))
    }
}

/// Verifier that accepts any certificate. Only reachable via `--insecure`.
///
/// Kept in one clearly-named place so it cannot be enabled by accident: an
/// accidental blanket-accept turns a verified download into an unverified one
/// while still reporting success.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Intermediates fetched from certificates' Authority Information Access
/// URLs, and the URL each failed handshake asked for.
///
/// A server that sends its leaf without the intermediate that signed it is
/// common, and browsers paper over it by fetching the intermediate from the
/// URL the leaf names. What is fetched only ever completes a chain: it must
/// still verify up to a bundled root, so it cannot make an untrusted
/// certificate trusted.
#[derive(Debug, Default)]
struct Aia {
    fetched: std::sync::Mutex<Vec<(String, rustls_pki_types::CertificateDer<'static>)>>,
    wanted: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

/// Bounds the cache: an origin can make us fetch, not make us hoard.
const AIA_CACHE_MAX: usize = 32;
/// Issuers fetched per handshake: real chains are two or three deep.
const AIA_MAX_DEPTH: usize = 3;
/// An intermediate is a couple of kilobytes; this is generous.
const AIA_FETCH_CAP: usize = 64 * 1024;

impl Aia {
    fn take_wanted(&self, server: &str) -> Option<String> {
        self.wanted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(server)
    }

    fn add(&self, url: &str, cert: rustls_pki_types::CertificateDer<'static>) {
        let mut fetched = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        fetched.retain(|(u, _)| u != url);
        if fetched.len() >= AIA_CACHE_MAX {
            fetched.remove(0);
        }
        fetched.push((url.to_string(), cert));
    }

    /// The next issuer to fetch for a chain that did not verify: the first
    /// CA Issuers URL along it that has not been fetched already, so each
    /// pass climbs one level.
    fn next_issuer(&self, chain: &[&[u8]]) -> Option<String> {
        let fetched = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        chain
            .iter()
            .filter_map(|cert| ca_issuers_url(cert))
            .find(|url| !fetched.iter().any(|(u, _)| u == url))
    }
}

/// The bundled-roots verifier, with the [`Aia`] intermediates offered
/// alongside whatever the server sent.
#[derive(Debug)]
struct AiaVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
    aia: Arc<Aia>,
}

impl rustls::client::danger::ServerCertVerifier for AiaVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls_pki_types::CertificateDer<'_>,
        intermediates: &[rustls_pki_types::CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let mut chain = intermediates.to_vec();
        chain.extend(
            self.aia
                .fetched
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(_, cert)| cert.clone()),
        );
        let verdict = self
            .inner
            .verify_server_cert(end_entity, &chain, server_name, ocsp, now);
        if let Err(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)) =
            &verdict
        {
            let path: Vec<&[u8]> = std::iter::once(end_entity.as_ref())
                .chain(chain.iter().map(|c| c.as_ref()))
                .collect();
            if let Some(url) = self.aia.next_issuer(&path) {
                self.aia
                    .wanted
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(server_name.to_str().into_owned(), url);
            }
        }
        verdict
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// The `http://` CA Issuers URL in a certificate's Authority Information
/// Access extension.
///
/// Read straight off the DER: the accessMethod OID for caIssuers is followed
/// by the location as a `[6]` uniformResourceIdentifier. Only `http` is
/// taken; RFC 5280 says that is what the extension carries, and following an
/// `https` one would need the very chain being repaired.
fn ca_issuers_url(cert: &[u8]) -> Option<String> {
    const CA_ISSUERS: [u8; 10] = [0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];
    let at = cert
        .windows(CA_ISSUERS.len())
        .position(|w| w == CA_ISSUERS)?
        + CA_ISSUERS.len();
    let rest = cert.get(at..)?;
    if *rest.first()? != 0x86 {
        return None;
    }
    let (len, start) = match *rest.get(1)? {
        n if n < 0x80 => (n as usize, 2),
        0x81 => (*rest.get(2)? as usize, 3),
        _ => return None,
    };
    let url = std::str::from_utf8(rest.get(start..start + len)?).ok()?;
    url.starts_with("http://").then(|| url.to_string())
}

/// A fetched issuer certificate: DER as published, or PEM.
fn issuer_cert(body: Vec<u8>) -> Option<rustls_pki_types::CertificateDer<'static>> {
    use rustls_pki_types::pem::PemObject;
    if body.first() == Some(&0x30) {
        return Some(rustls_pki_types::CertificateDer::from(body));
    }
    rustls_pki_types::CertificateDer::from_pem_slice(&body).ok()
}

/// Which IP version to connect over: `-4`, `-6`, or whatever resolves.
///
/// A property of the connection rather than of the request, so it lives on the
/// connector beside the proxy. The flags were parsed and forwarded through the
/// compatibility layer but never reached any socket: `-6` against an
/// IPv4-only host connected happily over IPv4 and reported success.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IpFamily {
    /// No preference: use whatever the resolver returns, in its own order.
    #[default]
    Any,
    /// `-4` / `--ipv4`: refuse anything but an A record.
    V4,
    /// `-6` / `--ipv6`: refuse anything but an AAAA record.
    V6,
}

impl IpFamily {
    /// Pick the family from the two flags. Both set is treated as no preference:
    /// the CLI rejects that combination before it reaches here.
    pub fn from_flags(v4: bool, v6: bool) -> Self {
        match (v4, v6) {
            (true, false) => Self::V4,
            (false, true) => Self::V6,
            _ => Self::Any,
        }
    }

    pub fn matches(&self, addr: &std::net::SocketAddr) -> bool {
        match self {
            Self::Any => true,
            Self::V4 => addr.is_ipv4(),
            Self::V6 => addr.is_ipv6(),
        }
    }

    /// How the flag is spelled, for error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::V4 => "IPv4",
            Self::V6 => "IPv6",
        }
    }
}

/// Connect to `host:port`, honouring an address-family restriction.
///
/// Resolution is explicit rather than left to `TcpStream::connect((host, port))`
/// because that helper hides the candidate list: it tries every address the
/// resolver returned, of either family, and there is no way to tell it not to.
/// Filtering here is what makes `-4`/`-6` mean anything.
///
/// A host with no address of the requested family is an ERROR, not a silent
/// fallback to the other one — the whole point of the flag is to refuse.
/// Resolved addresses, remembered for the life of the process.
///
/// # Why resolution has to be cached
///
/// Name resolution in tokio is not async — there is no async `getaddrinfo`, so both
/// `lookup_host` and `TcpStream::connect((host, port))` hand the call to the runtime's
/// BLOCKING thread pool. That pool grows on demand and reaps idle threads, so opening
/// n connections to one host paid for n resolutions and n rounds of thread
/// create/destroy, on top of whatever the resolver itself cost.
///
/// It showed up as involuntary context switches — "used up a timeslice", the overhead
/// kind, as opposed to the voluntary "waited for I/O" kind that is the actual work.
/// Measured on an 11 MB transfer: ~192 at `-x 1` but ~1250-2260 for any n > 1. The shape
/// is the tell: a STEP at "more than one connection"
/// rather than growth proportional to n, which is what a per-connection setup cost looks
/// like when the transfer is long enough for the connections to overlap. Sampling
/// `/proc/<pid>/task/*` mid-transfer showed 11 live worker threads under a 4-worker
/// runtime, with thread ids changing between samples — the blocking pool churning.
///
/// A downloader opens many connections to ONE host by construction, so the second
/// through nth resolutions are pure waste: same name, same answer, microseconds apart.
///
/// Cached for the process lifetime rather than with a TTL. A transfer lasts seconds to
/// minutes and a TTL only matters if the mapping changes mid-transfer, in which case the
/// connection would fail and be retried anyway. Honouring DNS TTLs would mean parsing
/// them, which `getaddrinfo` does not expose.
type DnsCache =
    std::sync::Mutex<std::collections::HashMap<(String, u16), Vec<std::net::SocketAddr>>>;

static DNS_CACHE: std::sync::OnceLock<DnsCache> = std::sync::OnceLock::new();

/// Resolve `host:port`, consulting the process-wide cache first.
async fn resolve_cached(host: &str, port: u16) -> io::Result<Vec<std::net::SocketAddr>> {
    let key = (host.to_ascii_lowercase(), port);
    let cache = DNS_CACHE.get_or_init(Default::default);
    if let Ok(g) = cache.lock() {
        if let Some(hit) = g.get(&key) {
            return Ok(hit.clone());
        }
    }
    // A literal address needs no resolver at all, and this is the common case for the
    // in-process test origins as well as for `--resolve`-style pinning.
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("{host} resolved to no addresses"),
        ));
    }
    if let Ok(mut g) = cache.lock() {
        g.insert(key, addrs.clone());
    }
    Ok(addrs)
}

pub async fn connect_family(host: &str, port: u16, family: IpFamily) -> io::Result<TcpStream> {
    let resolved = resolve_cached(host, port).await?;
    if family == IpFamily::Any {
        // Same candidate-walking behaviour as `TcpStream::connect((host, port))`, but
        // over the cached list so the resolver is consulted once per host rather than
        // once per connection.
        let mut last: Option<io::Error> = None;
        for a in &resolved {
            match TcpStream::connect(*a).await {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        return Err(last.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("{host} resolved to no usable addresses"),
            )
        }));
    }
    let addrs: Vec<std::net::SocketAddr> =
        resolved.into_iter().filter(|a| family.matches(a)).collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!(
                "{host} has no {} address (requested by -{})",
                family.as_str(),
                if family == IpFamily::V4 { '4' } else { '6' }
            ),
        ));
    }
    // Try each candidate: the first may be unreachable even when it resolves.
    let mut last: Option<io::Error> = None;
    for a in addrs {
        match TcpStream::connect(a).await {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("cannot reach {host}"),
        )
    }))
}

/// Connector that speaks plaintext or TLS depending on the target.
#[derive(Clone)]
pub struct TlsCapableConnector {
    config: Arc<ClientConfig>,
    /// How to reach the origin. A SOCKS proxy is handled here rather than in the
    /// target, because it is a property of the connection, not of the request: SOCKS
    /// forwards a TCP stream and never sees the HTTP.
    proxy: crate::socks::Proxy,
    /// `-4` / `-6`, applied to every socket this connector opens.
    family: IpFamily,
    /// Idle connections shared across every request this connector serves,
    /// INCLUDING the size probe that runs before the transfer. Without a pool that
    /// outlives one transfer, the probe's handshake is discarded and the transfer
    /// redials the origin it was just speaking to — measured at 1.6-2.0 s of setup
    /// on a live TLS path.
    pool: crate::pool::SharedPool<MaybeTls>,
    /// Shared with the verifier; `None` when nothing is verified.
    aia: Option<Arc<Aia>>,
}

impl TlsCapableConnector {
    /// Verifying client, trusting the bundled Mozilla root set.
    ///
    /// The bundled roots rather than the platform store: they make behaviour
    /// identical across the three platforms CI builds for, and a mirror set that
    /// works on one developer's machine and not another's is a bug that costs more
    /// than the flexibility is worth.
    pub fn new() -> io::Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::verifying(roots)
    }

    fn verifying(roots: rustls::RootCertStore) -> io::Result<Self> {
        let inner = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|e| io::Error::other(format!("tls config: {e}")))?;
        let aia = Arc::new(Aia::default());
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AiaVerifier {
                inner,
                aia: aia.clone(),
            }))
            .with_no_client_auth();
        Ok(Self {
            aia: Some(aia),
            ..Self::finish(config)
        })
    }

    /// The tail every constructor shares: session resumption, because a
    /// multi-source transfer opens several connections per host, and an
    /// explicit HTTP/1.1 ALPN offer. HTTP/1.1 is the deliberate choice, not a
    /// limitation: HTTP/2 would ride every range on one congestion window
    /// (see docs/HTTP2-ASSESSMENT.md).
    fn finish(mut config: ClientConfig) -> Self {
        config.resumption = rustls::client::Resumption::in_memory_sessions(64);
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self {
            config: Arc::new(config),
            proxy: crate::socks::Proxy::none(),
            family: IpFamily::Any,
            pool: std::sync::Arc::new(crate::pool::ConnPool::new()),
            aia: None,
        }
    }

    /// Route every connection through a SOCKS proxy.
    ///
    /// This belongs on the connector rather than on a target because SOCKS operates
    /// below HTTP: it forwards a TCP stream and never parses the request, so the choice
    /// affects how the socket is opened, not what is written to it. An HTTP proxy is the
    /// opposite — it rewrites the request line — and lives on the target.
    pub fn with_socks(mut self, proxy: crate::socks::Proxy) -> Self {
        self.proxy = proxy;
        self
    }

    /// Restrict every connection to one IP version (`-4` / `-6`).
    pub fn with_family(mut self, family: IpFamily) -> Self {
        self.family = family;
        self
    }

    /// Client that accepts any certificate. `--insecure` only.
    pub fn insecure() -> io::Result<Self> {
        let provider = rustls::crypto::ring::default_provider();
        let verifier = Arc::new(AcceptAnyCert(Arc::new(provider.clone())));
        let config = ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| io::Error::other(format!("tls config: {e}")))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(Self::finish(config))
    }

    /// Pick the verifying or accept-anything client.
    pub fn with_insecure(insecure: bool) -> io::Result<Self> {
        if insecure {
            eprintln!(
                "hydra: warning: --insecure disables certificate verification. \
                 The bytes you receive are not authenticated: an interception can \
                 substitute a different object and this transfer will still report \
                 success."
            );
            Self::insecure()
        } else {
            Self::new()
        }
    }
}

impl Connector for TlsCapableConnector {
    type Stream = MaybeTls;

    /// Share this connector's pool with the transfer, so a connection established
    /// by the size probe is reused instead of re-dialled.
    fn pool(&self) -> Option<crate::pool::SharedPool<MaybeTls>> {
        Some(self.pool.clone())
    }

    fn connect<'a>(
        &'a self,
        t: &'a Target,
    ) -> Pin<Box<dyn Future<Output = io::Result<Self::Stream>> + Send + 'a>> {
        Box::pin(async move {
            let tcp = self.dial(t).await?;
            if !t.tls {
                return Ok(MaybeTls::Plain(tcp));
            }
            // SNI must be the ORIGIN name, not the socket peer: when routing
            // through a proxy the socket connects to the proxy but the certificate
            // being validated belongs to the origin.
            let sni_host = t.tls_server_name();
            let server_name = ServerName::try_from(sni_host.to_string()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("bad TLS name: {sni_host}"),
                )
            })?;
            let mut handshake = RustlsConnector::from(self.config.clone())
                .connect(server_name.clone(), tcp)
                .await;
            // A chain that stops short of its root is completed from the URL
            // its top certificate names, one level per pass, then asked again.
            for _ in 0..AIA_MAX_DEPTH {
                if handshake.is_ok() {
                    break;
                }
                let Some(url) = self.aia.as_ref().and_then(|a| a.take_wanted(sni_host)) else {
                    break;
                };
                if !self.fetch_issuer(t, &url).await {
                    break;
                }
                let tcp = self.dial(t).await?;
                handshake = RustlsConnector::from(self.config.clone())
                    .connect(server_name.clone(), tcp)
                    .await;
            }
            Ok(MaybeTls::Tls(Box::new(handshake?)))
        })
    }
}

impl TlsCapableConnector {
    /// The TCP leg to `t`: direct, through SOCKS, or through an HTTP proxy's
    /// CONNECT tunnel when `t` is TLS.
    async fn dial(&self, t: &Target) -> io::Result<TcpStream> {
        let socks = &self.proxy;
        // With a SOCKS proxy the TCP connection goes to the PROXY, and the
        // handshake asks it to reach the origin. The origin name is sent as a
        // name, not an address, so the proxy resolves it — which is the whole
        // point when local DNS cannot.
        let mut tcp = if socks.kind.is_socks() {
            // The family restriction applies to the hop this process actually
            // opens, which is the one to the PROXY. Beyond it the proxy
            // resolves the origin itself and this client has no say — saying
            // otherwise would be a promise we cannot keep.
            let mut sock = connect_family(&socks.host, socks.port, self.family).await?;
            let _ = sock.set_nodelay(true);
            let (dst_host, dst_port) = t.origin_endpoint();
            crate::socks::handshake(&mut sock, socks, &dst_host, dst_port).await?;
            sock
        } else {
            let sock = connect_family(&t.host, t.port, self.family).await?;
            let _ = sock.set_nodelay(true);
            sock
        };
        // Through a proxy, TLS needs a CONNECT tunnel first: absolute-form
        // requests only work in cleartext, because the proxy has to read the
        // request line, and it cannot read an encrypted one.
        // A CONNECT tunnel is only for an HTTP proxy. Through SOCKS the stream is
        // already end-to-end with the origin, so a CONNECT would be sent TO the
        // origin as a bogus request.
        if t.tls && t.origin.is_some() && !socks.kind.is_socks() {
            connect_tunnel(&mut tcp, t.proxy_authority(), t.proxy_authorization()).await?;
        }
        Ok(tcp)
    }

    /// Fetch the issuer certificate at `url` the way `t` itself is routed,
    /// and cache it for the verifier. Whether one was added.
    async fn fetch_issuer(&self, t: &Target, url: &str) -> bool {
        let Some(rest) = url.strip_prefix("http://") else {
            return false;
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => match p.parse() {
                Ok(p) => (h, p),
                Err(_) => return false,
            },
            None => (authority, 80),
        };
        let mut aia = if t.origin.is_some() && !self.proxy.kind.is_socks() {
            Target::via_proxy(&t.host, t.port, authority, path)
        } else {
            Target::direct(host, port, path)
        };
        aia.headers = t
            .headers
            .iter()
            .filter(|h| crate::is_field(h, "proxy-authorization"))
            .cloned()
            .collect();
        let Ok(body) = crate::http::fetch_small(self, &aia, AIA_FETCH_CAP).await else {
            return false;
        };
        match (issuer_cert(body), &self.aia) {
            (Some(cert), Some(cache)) => {
                cache.add(url, cert);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verifying_client_builds_with_a_nonempty_root_set() {
        let c = TlsCapableConnector::new().expect("verifying client must build");
        // A client with no roots would fail every handshake at runtime rather than
        // at construction, which is the worst place to find out.
        assert!(
            !webpki_roots::TLS_SERVER_ROOTS.is_empty(),
            "the bundled root set must not be empty"
        );
        drop(c);
    }

    fn der(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if bytes.len() < 0x80 {
            out.push(bytes.len() as u8);
        } else {
            out.extend([0x81, bytes.len() as u8]);
        }
        out.extend(bytes);
        out
    }

    /// An Authority Information Access extension's value naming `url` as
    /// the CA Issuers location.
    fn aia_value(url: &str) -> Vec<u8> {
        let mut access = vec![
            0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02, 0x86,
        ];
        access.extend(der(url.as_bytes()));
        let mut description = vec![0x30];
        description.extend(der(&access));
        let mut outer = vec![0x30];
        outer.extend(der(&description));
        outer
    }

    #[test]
    fn ca_issuers_url_reads_the_http_location_only() {
        assert_eq!(
            ca_issuers_url(&aia_value("http://yr1.i.lencr.org/")).as_deref(),
            Some("http://yr1.i.lencr.org/")
        );
        let long = format!("http://ca.example/{}", "a".repeat(150));
        assert_eq!(ca_issuers_url(&aia_value(&long)), Some(long));
        assert_eq!(ca_issuers_url(&aia_value("https://ca.example/i.der")), None);
        assert_eq!(ca_issuers_url(&aia_value("ldap://ca.example/cn")), None);
        assert_eq!(ca_issuers_url(b"\x30\x03\x02\x01\x00"), None);
    }

    #[test]
    fn each_pass_asks_for_the_next_issuer_up_the_chain() {
        let aia = Aia::default();
        let leaf = aia_value("http://ca.example/intermediate");
        let intermediate = aia_value("http://ca.example/cross");
        assert_eq!(
            aia.next_issuer(&[&leaf]).as_deref(),
            Some("http://ca.example/intermediate")
        );
        aia.add(
            "http://ca.example/intermediate",
            rustls_pki_types::CertificateDer::from(intermediate.clone()),
        );
        assert_eq!(
            aia.next_issuer(&[&leaf, &intermediate]).as_deref(),
            Some("http://ca.example/cross")
        );
        aia.add(
            "http://ca.example/cross",
            rustls_pki_types::CertificateDer::from(vec![0x30, 0x00]),
        );
        assert_eq!(aia.next_issuer(&[&leaf, &intermediate]), None);
    }

    #[test]
    fn the_issuer_cache_is_bounded() {
        let aia = Aia::default();
        for n in 0..AIA_CACHE_MAX + 5 {
            aia.add(
                &format!("http://ca.example/{n}"),
                rustls_pki_types::CertificateDer::from(vec![0x30, n as u8]),
            );
        }
        let fetched = aia.fetched.lock().unwrap();
        assert_eq!(fetched.len(), AIA_CACHE_MAX);
        assert_eq!(fetched[0].0, "http://ca.example/5");
    }

    #[test]
    fn a_fetched_issuer_may_be_der_or_pem() {
        let der_body = vec![0x30, 0x03, 0x02, 0x01, 0x00];
        assert_eq!(
            issuer_cert(der_body.clone()).map(|c| c.as_ref().to_vec()),
            Some(der_body.clone())
        );
        let pem = rcgen::CertificateParams::new(vec!["ca.example".into()])
            .unwrap()
            .self_signed(&rcgen::KeyPair::generate().unwrap())
            .unwrap();
        assert_eq!(
            issuer_cert(pem.pem().into_bytes()).as_ref(),
            Some(pem.der())
        );
        assert!(issuer_cert(b"Expired".to_vec()).is_none());
    }

    struct Chain {
        root: rustls_pki_types::CertificateDer<'static>,
        intermediate: Vec<u8>,
        leaf: rustls_pki_types::CertificateDer<'static>,
        leaf_key: rustls_pki_types::PrivateKeyDer<'static>,
    }

    /// root -> intermediate -> leaf for `localhost`, the leaf naming
    /// `issuer_url` as where its issuer is published.
    fn chain(issuer_url: &str) -> Chain {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
        let ca = |name: &str| {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.distinguished_name.push(DnType::CommonName, name);
            p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            p
        };
        let root_key = KeyPair::generate().unwrap();
        let root_params = ca("Hydra Test Root");
        let root = root_params.self_signed(&root_key).unwrap();
        let int_key = KeyPair::generate().unwrap();
        let int_params = ca("Hydra Test Intermediate");
        let intermediate = int_params
            .signed_by(&int_key, &Issuer::from_params(&root_params, &root_key))
            .unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        leaf_params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        leaf_params
            .custom_extensions
            .push(rcgen::CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 5, 5, 7, 1, 1],
                aia_value(issuer_url),
            ));
        let leaf = leaf_params
            .signed_by(&leaf_key, &Issuer::from_params(&int_params, &int_key))
            .unwrap();
        Chain {
            root: root.der().clone(),
            intermediate: intermediate.der().to_vec(),
            leaf: leaf.der().clone(),
            leaf_key: rustls_pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
        }
    }

    /// Serves whatever `body` holds to every plain HTTP request, counting
    /// them. The body is filled in after the chain naming this server's port
    /// has been minted.
    async fn issuer_server() -> (
        u16,
        Arc<std::sync::Mutex<Vec<u8>>>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (served, counted) = (body.clone(), hits.clone());
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let body = served.lock().unwrap().clone();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/pkix-cert\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
            }
        });
        (port, body, hits)
    }

    /// A TLS origin that sends its leaf and nothing else: the misconfiguration
    /// `fs20.uploadrar.com` has, with the intermediate left to the client.
    async fn leaf_only_origin(chain: &Chain) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![chain.leaf.clone()], chain.leaf_key.clone_key())
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (s, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut tls) = acceptor.accept(s).await {
                        let mut buf = [0u8; 1024];
                        let _ = tls.read(&mut buf).await;
                        let _ = tls
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                            .await;
                    }
                });
            }
        });
        port
    }

    fn trusting(root: &rustls_pki_types::CertificateDer<'static>) -> TlsCapableConnector {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(root.clone()).unwrap();
        TlsCapableConnector::verifying(roots).unwrap()
    }

    #[tokio::test]
    async fn a_chain_missing_its_intermediate_is_completed_from_the_leafs_aia() {
        let (issuer_port, body, hits) = issuer_server().await;
        let chain = chain(&format!("http://127.0.0.1:{issuer_port}/int.der"));
        *body.lock().unwrap() = chain.intermediate.clone();
        let origin = leaf_only_origin(&chain).await;
        let connector = trusting(&chain.root);
        let t = Target::direct_tls("localhost", origin, "/");

        let body = crate::http::fetch_small(&connector, &t, 64).await.unwrap();
        assert_eq!(body, b"ok");
        let again = crate::http::fetch_small(&connector, &t, 64).await.unwrap();
        assert_eq!(again, b"ok");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the intermediate is fetched once and then reused"
        );
    }

    #[tokio::test]
    async fn an_issuer_that_does_not_chain_to_a_trusted_root_still_fails() {
        let (issuer_port, body, _) = issuer_server().await;
        let chain = chain(&format!("http://127.0.0.1:{issuer_port}/int.der"));
        let impostor = self::chain("http://unused.example/");
        *body.lock().unwrap() = impostor.intermediate.clone();
        let origin = leaf_only_origin(&chain).await;
        let connector = trusting(&chain.root);
        let t = Target::direct_tls("localhost", origin, "/");

        let err = crate::http::fetch_small(&connector, &t, 64)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid peer certificate"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_issuer_reports_the_original_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = listener.local_addr().unwrap().port();
        drop(listener);
        let chain = chain(&format!("http://127.0.0.1:{dead}/int.der"));
        let origin = leaf_only_origin(&chain).await;
        let connector = trusting(&chain.root);
        let t = Target::direct_tls("localhost", origin, "/");

        let err = crate::http::fetch_small(&connector, &t, 64)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("UnknownIssuer"), "{err}");
    }

    #[test]
    fn an_insecure_client_builds() {
        assert!(TlsCapableConnector::insecure().is_ok());
    }

    #[test]
    fn sni_uses_the_origin_name_not_the_socket_peer() {
        // Through a proxy the socket goes to the proxy, but the certificate being
        // validated belongs to the origin. Using the peer name here would either
        // fail every proxied handshake or, worse, validate the wrong identity.
        let direct = Target::direct_tls("example.org", 443, "/f");
        assert_eq!(direct.tls_server_name(), "example.org");
        let proxied = Target::via_proxy("proxy.local", 3128, "example.org:443", "/f");
        assert_eq!(
            proxied.tls_server_name(),
            "example.org",
            "SNI must name the origin, never the proxy"
        );
    }

    #[test]
    fn proxy_authority_is_the_origin_with_its_port() {
        let t = Target::via_proxy("proxy.local", 3128, "example.org:443", "/f");
        assert_eq!(
            t.proxy_authority(),
            "example.org:443",
            "CONNECT must name the ORIGIN and its port, not the proxy"
        );
    }

    #[test]
    fn tls_targets_are_marked_and_plaintext_ones_are_not() {
        assert!(Target::direct_tls("x.org", 443, "/f").tls);
        assert!(!Target::direct("x.org", 80, "/f").tls);
    }

    /// `-4` and `-6` must actually filter candidate addresses.
    ///
    /// Regression test: both flags were declared on the CLI and forwarded through
    /// the compatibility layer, but no DNS or socket code ever read them —
    /// `-6` against an IPv4-only host connected over IPv4 and reported success.
    /// The filter is what makes the flag mean something, and refusing is the
    /// correct outcome when the requested family is absent: falling back to the
    /// other one is precisely what the user asked not to happen.
    #[test]
    fn an_address_family_restriction_selects_and_refuses() {
        use std::net::SocketAddr;
        let v4: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let v6: SocketAddr = "[2606:2800:220:1:248:1893:25c8:1946]:443".parse().unwrap();

        assert!(IpFamily::Any.matches(&v4) && IpFamily::Any.matches(&v6));
        assert!(IpFamily::V4.matches(&v4) && !IpFamily::V4.matches(&v6));
        assert!(IpFamily::V6.matches(&v6) && !IpFamily::V6.matches(&v4));

        // A dual-stack resolution filtered by each flag keeps only its own family.
        let both = [v4, v6];
        assert_eq!(
            both.iter().filter(|a| IpFamily::V4.matches(a)).count(),
            1,
            "-4 must keep exactly the A record"
        );
        assert_eq!(
            both.iter().filter(|a| IpFamily::V6.matches(a)).count(),
            1,
            "-6 must keep exactly the AAAA record"
        );
        // An IPv4-only host under `-6` leaves nothing: the connect must fail
        // rather than quietly fall back to the address the user excluded.
        assert_eq!(
            [v4].iter().filter(|a| IpFamily::V6.matches(a)).count(),
            0,
            "-6 against an IPv4-only host must have no candidate left"
        );
    }

    /// The flags map to a family, and asking for neither restricts nothing.
    #[test]
    fn ip_family_from_flags_reads_the_pair() {
        assert_eq!(IpFamily::from_flags(false, false), IpFamily::Any);
        assert_eq!(IpFamily::from_flags(true, false), IpFamily::V4);
        assert_eq!(IpFamily::from_flags(false, true), IpFamily::V6);
        // Both is rejected by the CLI (`conflicts_with`); if one ever reaches
        // here it must not silently become a restriction the user did not choose.
        assert_eq!(IpFamily::from_flags(true, true), IpFamily::Any);
    }

    /// A forward proxy that wants a login: `CONNECT` is answered 407 without
    /// the header and 200 with it, and the request head it saw is handed
    /// back so the test can read what went over the wire.
    fn authenticating_proxy(want: &'static str) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let resp = if head.lines().any(|l| l == want) {
                    "HTTP/1.1 200 Connection established\r\n\r\n"
                } else {
                    "HTTP/1.1 407 Proxy Authentication Required\r\n\
                     Proxy-Authenticate: Basic realm=\"p\"\r\nContent-Length: 0\r\n\r\n"
                };
                let _ = sock.write_all(resp.as_bytes());
                let _ = tx.send(head);
            }
        });
        (port, rx)
    }

    /// The reported bug: plain-HTTP requests through an authenticated proxy
    /// carried `Proxy-Authorization` and worked, while an https URL through
    /// the same proxy was refused at the tunnel with 407 because the
    /// `CONNECT` went out without it.
    #[tokio::test]
    async fn a_connect_tunnel_carries_the_proxy_login() {
        let (port, seen) = authenticating_proxy("Proxy-Authorization: Basic cHU6cHc=");
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        connect_tunnel(&mut sock, "origin.example:443", Some("Basic cHU6cHc="))
            .await
            .expect("an authenticated CONNECT must open the tunnel");
        let head = seen.recv().unwrap();
        assert!(
            head.starts_with("CONNECT origin.example:443 HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(
            head.contains("Proxy-Authorization: Basic cHU6cHc=\r\n"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn a_connect_tunnel_without_the_login_reports_the_407() {
        let (port, seen) = authenticating_proxy("Proxy-Authorization: Basic cHU6cHc=");
        let mut sock = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let e = connect_tunnel(&mut sock, "origin.example:443", None)
            .await
            .expect_err("no login, no tunnel");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("407"), "{e}");
        assert!(
            !seen.recv().unwrap().contains("Proxy-Authorization"),
            "nothing to send, nothing sent"
        );
    }

    /// The connector reads the login off the target, where the front-ends
    /// already put it for their plain requests, so both hops through one
    /// proxy authenticate the same way.
    #[tokio::test]
    async fn the_connector_sends_the_targets_proxy_login_on_connect() {
        let (port, seen) = authenticating_proxy("Proxy-Authorization: Basic cHU6cHc=");
        let mut t = Target::via_proxy("127.0.0.1", port, "origin.example:443", "/f")
            .with_headers(vec!["Proxy-Authorization: Basic cHU6cHc=".into()], None);
        t.tls = true;
        // The tunnel opens; the handshake that follows fails, because the
        // proxy is not an origin. What the test pins is what the proxy saw.
        let _ = TlsCapableConnector::new().unwrap().connect(&t).await;
        let head = seen.recv().unwrap();
        assert!(head.starts_with("CONNECT origin.example:443"), "{head}");
        assert!(
            head.contains("Proxy-Authorization: Basic cHU6cHc="),
            "{head}"
        );
    }

    /// A family with no matching address must report which host and which flag.
    #[tokio::test]
    async fn connecting_with_no_address_of_the_requested_family_says_so() {
        // A name that resolves to a v4 loopback only. No connection is attempted
        // against a listener: resolution fails the family filter first.
        let e = connect_family("127.0.0.1", 9, IpFamily::V6)
            .await
            .expect_err("an IPv4 literal must not satisfy -6");
        assert_eq!(e.kind(), std::io::ErrorKind::AddrNotAvailable);
        let msg = e.to_string();
        assert!(
            msg.contains("127.0.0.1") && msg.contains("IPv6"),
            "the error must name the host and the family it lacks, got: {msg}"
        );
    }
}

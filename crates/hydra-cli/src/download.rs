//! The download engine the CLI drives: probe, plan, transfer, verify.
//!
//! This is where the theory meets a user's file. The scheduler decides *which
//! bytes go where*; this module decides everything around that — how many
//! connections politeness permits, whether a partial file can be resumed, what
//! the sidecar records, and whether the delivered bytes are the bytes asked for.

use crate::progress::{ConnView, Counters, Progress};
use crate::url::{ProxyPolicy, Sidecar, Url};
use hya_core::{detect_format, Category, Scheduler, Source};
use hya_net::cookies::CookieJar;
use hya_net::polite::{Politeness, RateLimiter};
use hya_net::{fetch_range_retry, probe_resilient, SparseSink, Target, TlsCapableConnector};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A requested byte range, kept symbolic until the object size is known.
///
/// `Suffix` cannot be resolved at parse time — "the last 512 bytes" depends on
/// the size, which only the probe reveals — so it stays an explicit variant
/// rather than a sentinel value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeSpec {
    /// `lo-hi` inclusive, as HTTP spells ranges.
    Closed(u64, u64),
    /// `lo-`: from an offset to the end.
    From(u64),
    /// `-n`: the final n bytes.
    Suffix(u64),
}

impl RangeSpec {
    /// Parse `0-1023`, `1024-`, or `-512` (the last 512 bytes).
    ///
    /// A suffix is its own variant rather than a sentinel: an earlier encoding
    /// as `u64::MAX - n` recognised by a threshold silently fetched the wrong
    /// region.
    pub fn parse(spec: &str) -> Option<RangeSpec> {
        let spec = spec.trim();
        if let Some(tail) = spec.strip_prefix('-') {
            let n: u64 = tail.parse().ok()?;
            return if n == 0 {
                None
            } else {
                Some(RangeSpec::Suffix(n))
            };
        }
        let (a, b) = spec.split_once('-')?;
        let lo: u64 = a.parse().ok()?;
        if b.is_empty() {
            return Some(RangeSpec::From(lo));
        }
        let hi: u64 = b.parse().ok()?;
        if hi < lo {
            return None;
        }
        Some(RangeSpec::Closed(lo, hi))
    }

    /// Resolve against a known object size into a half-open `[lo, hi)`.
    pub fn resolve(self, size: u64) -> Option<(u64, u64)> {
        let (lo, hi) = match self {
            RangeSpec::Closed(lo, hi) => (lo.min(size), (hi.saturating_add(1)).min(size)),
            RangeSpec::From(lo) => (lo.min(size), size),
            RangeSpec::Suffix(n) => (size.saturating_sub(n), size),
        };
        if hi > lo {
            Some((lo, hi))
        } else {
            None
        }
    }
}

/// Everything the engine needs for one transfer.
/// Live progress from a running transfer.
///
/// The queue manager needs this because a transfer is opaque otherwise: `run()` only
/// returns when it is finished, so a UI driving several jobs had nothing to show until
/// each one completed — every row sat at `?` for the whole download. Per-connection
/// detail rides along because that is the state that makes a multi-source transfer
/// debuggable (which mirror is slow, which range is stuck).
#[derive(Clone, Debug)]
pub struct Tick {
    pub id: u64,
    pub done: u64,
    pub size: Option<u64>,
    pub rate: f64,
    pub requests: u64,
    pub repairs: u64,
    pub conns: Vec<ConnLine>,
}

/// One connection's state, flattened for display.
#[derive(Clone, Debug)]
pub struct ConnLine {
    pub host: String,
    pub lo: u64,
    pub hi: u64,
    pub pos: u64,
    pub rate: f64,
    pub health: String,
}

#[derive(Clone)]
pub struct Job {
    pub force_stream: bool,
    pub plugin_options: Option<crate::plugins::Options>,
    /// Where to send live progress, and the id to tag it with.
    pub ticks: Option<(u64, tokio::sync::mpsc::UnboundedSender<Tick>)>,
    pub urls: Vec<String>,
    pub output: Option<PathBuf>,
    pub conns: Option<usize>,
    pub resume: bool,
    pub limit_rate: u64,
    /// Redirect hops permitted before giving up. `0` refuses to follow any.
    pub max_redirs: u32,
    /// `-4` / `-6`: restrict every connection to one IP version.
    pub ip_family: hya_net::IpFamily,
    /// `--show-error`: print failure reasons to stderr even under `-q`.
    pub show_error: bool,
    /// `--logfile` (truncate) or `--logfile-append`: human output goes to this
    /// file instead of the terminal. `bool` is the append flag.
    pub logfile: Option<(PathBuf, bool)>,
    pub tries: u32,
    pub timeout_s: f64,
    pub checksum: Option<String>,
    pub headers: Vec<String>,
    pub user_agent: String,
    pub verbose: u8,
    pub quiet: bool,
    pub no_progress: bool,
    pub polite: Politeness,
    /// Run the marginal-goodput search even though `conns` names a number,
    /// treating that number as a ceiling rather than a target (`--adaptive`).
    ///
    /// Separate from `probe` below because the two answer different questions.
    /// `probe` is "no number was given, go and find one"; this is "a number was
    /// given, but check how much of it is useful". Conflating them would either
    /// make `-x N` silently measure — breaking a flag whose whole purpose is to
    /// pin a value for a reproduction or a comparison against another client — or
    /// leave `--adaptive` unable to express a ceiling.
    pub adaptive: bool,
    /// Measure the useful connection count when no `-x` was given.
    ///
    /// True by default; `--no-probe` sets it false, in which case a run with no
    /// `-x` takes ONE connection rather than guessing a multi-connection default.
    /// Guessing is what produced transfers slower than a single stream on a
    /// saturated access link.
    pub probe: bool,
    /// Write to stdout rather than a file.
    pub to_stdout: bool,
    /// Skip entirely if the output already exists.
    pub no_clobber: bool,
    /// Create the output directory if missing.
    pub create_dirs: bool,
    /// Directory to place the output in.
    pub output_dir: Option<PathBuf>,
    /// Probe only: report what the server says and do not fetch the body.
    pub spider: bool,
    /// Print response headers.
    pub server_response: bool,
    /// Retrieve only this byte range.
    pub range: Option<RangeSpec>,
    /// Refuse an object larger than this.
    pub max_filesize: Option<u64>,
    /// Explicit proxy, overriding the environment.
    pub proxy: Option<String>,
    /// Ignore any proxy in the environment.
    pub no_proxy: bool,
    /// Set the local mtime from the server.
    pub remote_time: bool,
    /// Write the object's ETag here.
    pub etag_save: Option<PathBuf>,
    /// Skip if the stored ETag still matches.
    pub etag_compare: Option<PathBuf>,
    /// Sort the output into a per-category subdirectory based on file type.
    pub sort_by_type: bool,
    /// Content-Type the server reported, for classification.
    pub content_type: Option<String>,
    /// Accept any TLS certificate.
    pub insecure: bool,
    /// Overwrite an existing file without asking.
    pub force: bool,
    /// Discard the bytes instead of saving them.
    pub no_save: bool,
    /// Report the object's SHA-256; see `Cli::print_checksum`. Off by default:
    /// hashing is a pass over every byte and nothing needs it unless asked.
    pub print_checksum: bool,
    /// Write a per-chunk digest manifest for what arrived.
    pub emit_manifest: Option<PathBuf>,
    /// Verify each chunk against this manifest as it arrives.
    pub chunk_digests: Option<PathBuf>,
    /// Chunk grid for --emit-manifest.
    pub chunk_size: Option<u64>,
    /// Publisher ranking and per-mirror ceilings, index-aligned with `urls`.
    ///
    /// Empty means unranked, which is exactly what a bare list of URLs is — so
    /// every caller that does not read a mirror list gets the behaviour it
    /// always had.
    pub source_plans: Vec<hya_core::SourcePlan>,
    /// Size and digests attested by a Metalink document rather than by a mirror.
    ///
    /// See [`crate::metalink`] for why this changes what mirror agreement means:
    /// independent mirror operators cannot share an `ETag`, so the pairwise
    /// validator gate is unsatisfiable across a real mirror list, while a
    /// document that states the size and a content digest establishes the same
    /// thing more strongly and from outside the mirrors.
    pub attested: Option<crate::metalink::Attested>,
    /// Treat a URL that turns out to SERVE a Metalink document as a mirror list
    /// rather than as the file to save.
    ///
    /// Decided at probe time from `Content-Type`, because that is the only point
    /// at which the answer is free: `https://mirrors.example/metalink?repo=x`
    /// has no extension to read and probing it twice to find out would cost a
    /// round trip on every download. Off means "save whatever the URL serves",
    /// which is what a user debugging a redirector wants.
    pub follow_metalink: bool,
    /// What reading the mirror list revealed, for the run log.
    ///
    /// Carried on the job rather than printed where it was discovered so it goes
    /// through `Progress::event` like everything else: verbosity-gated, routed
    /// to `--logfile`, and silenced by `-q`. A bare `eprintln!` obeys none of
    /// those, and a mirror list has plenty to say.
    pub metalink_notes: Vec<String>,
    /// The cookie flags, unresolved.
    ///
    /// `None` when none were given, which is what keeps a run without a cookie
    /// flag byte-identical: no file is read, no header is added, nothing is
    /// written. Resolved inside [`run`] rather than by the caller because a
    /// browser import is scoped to the host being downloaded from, and only the
    /// job knows what that is.
    pub cookies: Option<crate::cookies::CookieSpec>,
    /// Which entry and which mirrors to take, when a document is followed.
    ///
    /// Carried on the job rather than read from `Cli` because the follow happens
    /// inside the engine — the discovery is a `Content-Type` on a probe that has
    /// already been paid for — and the engine has no access to the parsed
    /// command line.
    pub metalink_select: crate::metalink::Selection,
    /// Name the output from the server's `Content-Disposition`.
    pub content_disposition: bool,
    /// Set from outside (Ctrl-C, a queue's pause) to stop the transfer; the
    /// resume record is written from what was held.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Seconds the probe may take, when tighter than `timeout_s`.
    pub connect_timeout_s: Option<f64>,
    /// Draw the progress frame even when stdout is not a terminal.
    pub show_progress: bool,
}

/// Do not split an object finer than this: a range request costs a round
/// trip, and below a quarter megabyte per connection the setup outweighs the
/// bytes it could carry, so a 1 KiB file split six ways is six requests for
/// nothing. `-x` with `--mirrors` names the sources explicitly and is exempt.
pub const MIN_BYTES_PER_CONNECTION: u64 = 256 * 1024;

/// Connections worth opening for an object of `size` bytes, at most `wanted`.
pub fn connections_for_size(wanted: usize, size: u64) -> usize {
    let by_size = usize::try_from(size / MIN_BYTES_PER_CONNECTION).unwrap_or(usize::MAX);
    wanted.min(by_size).max(1)
}

/// Where the assembled object is staged before it is copied to stdout.
///
/// A temp path of its own, never the URL's basename in the working
/// directory: staging there clobbered an unrelated file of the same name and
/// then deleted it.
fn stdout_stage_path() -> PathBuf {
    std::env::temp_dir().join(format!("hydra_stdout_{}", scratch_name()))
}

/// Add `line` unless a header of that name is already attached.
fn push_header_once(headers: &mut Vec<String>, line: String) {
    let name = line.split(':').next().unwrap_or("");
    let present = headers.iter().any(|h| {
        h.split(':')
            .next()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    });
    if !present {
        headers.push(line);
    }
}

/// The credentials a target for `u` carries: the URL's own userinfo and the
/// HTTP proxy's login. Applied after `with_headers`, which replaces the list.
fn with_credentials(mut t: Target, u: &Url, policy: &ProxyPolicy) -> Target {
    if let Some(line) = u.basic_auth_header() {
        push_header_once(&mut t.headers, line);
    }
    if let Some(line) = policy.auth_header(u) {
        push_header_once(&mut t.headers, line);
    }
    t
}

/// Build the HTTP target for `u` under `policy`, before headers are attached.
fn routed_target(u: &Url, policy: &ProxyPolicy) -> Result<Target, String> {
    let route = policy.http_route(u)?;
    u.to_target(route.as_ref().map(|(h, p)| (h.as_str(), *p)))
}

/// Run `fut` under the per-request limit, naming what timed out.
async fn within<T, E: std::fmt::Display>(
    secs: f64,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String> {
    let limit = std::time::Duration::from_secs_f64(secs.max(0.001));
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!(
            "{what} did not complete within {secs}s (--timeout)"
        )),
    }
}

/// What happened, for `--json` and for the report.
///
/// `Default` is the canonical "nothing happened yet" value (`ok: false`, every
/// counter zero, every option `None`); construct partial outcomes with
/// struct-update syntax rather than spelling out all 22 fields.
#[derive(serde::Serialize, Clone, Default, Debug)]
pub struct Outcome {
    pub url: String,
    pub output: String,
    pub size: u64,
    /// Wall time for the whole invocation, including probing and the concurrency
    /// measurement. This is the number a user timing the command sees.
    pub elapsed_s: f64,
    /// Wall time for the byte transfer alone.
    ///
    /// Reported separately because the two differ by seconds on a slow path — the
    /// progress bar's clock starts when bytes start, so a single `elapsed_s` next to a
    /// bar reading "1.7s" looked like two clocks disagreeing. Setup is a per-path cost
    /// a real client caches; transfer is the steady-state figure.
    pub transfer_s: f64,
    /// Wall time spent probing before the transfer began.
    pub setup_s: f64,
    pub throughput_bps: f64,
    pub requests: u64,
    /// The connection count the transfer actually ran at.
    pub connections: usize,
    /// The highest active limit reached, which under `--adaptive` is the top of the
    /// search rather than the answer. Equal to `connections` for fixed `-x`.
    pub peak_connections: usize,
    /// The most connections that ever actually held a range simultaneously. Differs from
    /// the limit in both directions: lowering the limit lets a busy connection finish, and
    /// a connection can be idle between ranges.
    pub peak_busy_connections: usize,
    /// Integral of busy connections over time. The only concurrency figure here that
    /// summarises the whole run rather than a moment or a bound.
    pub connection_seconds: f64,
    pub delta_s: f64,
    pub sha256: Option<String>,
    pub checksum_ok: Option<bool>,
    pub resumed_from: u64,
    pub ok: bool,
    pub note: Option<String>,
    /// Detected format name, e.g. "gzip".
    pub format: Option<String>,
    /// Detected category, e.g. "archive".
    pub category: Option<String>,
    /// Disagreement between the payload, the name, and the server's type.
    pub format_conflict: Option<String>,
    /// Short human label for the format, e.g. "gzip-compressed tar".
    pub format_label: Option<String>,
    /// One-sentence explanation, for a CLI hint or a GUI tooltip.
    pub format_description: Option<String>,
    /// What the category is and what to do with it.
    pub category_description: Option<String>,
}

fn targets_for(
    urls: &[String],
    headers: &[String],
    agent: &str,
    policy: &ProxyPolicy,
) -> Result<Vec<(Url, Target)>, String> {
    urls.iter()
        .map(|u| {
            let parsed = Url::parse(u).ok_or_else(|| {
                // Name the scheme's own reason when there is one: "sftp is not implemented
                // yet, and here is what to use instead" is actionable where "unsupported"
                // is not.
                match u.split_once("://") {
                    Some((s, _)) => format!(
                        "{u}: {} (supported: {})",
                        hya_net::scheme::unsupported_reason(&s.to_ascii_lowercase()),
                        hya_net::scheme::supported().join(", ")
                    ),
                    None => format!(
                        "unparsable URL: {u} (supported schemes: {})",
                        hya_net::scheme::supported().join(", ")
                    ),
                }
            })?;
            // `to_target` builds an HTTP request target and rejects any other scheme. FTP
            // does not use one — it gets an Endpoint instead — so a placeholder is paired
            // here and the FTP branch replaces it. Calling to_target for an ftp:// URL
            // returned an error BEFORE the FTP branch was ever reached, which is why an
            // ftp:// fetch failed silently with exit 1 and no message.
            if parsed.is_ftp() {
                let t = hya_net::Target::direct(&parsed.host, parsed.port, &parsed.path);
                return Ok((parsed, t));
            }
            let t = routed_target(&parsed, policy)?
                .with_headers(headers.to_vec(), Some(agent.to_string()));
            let t = with_credentials(t, &parsed, policy);
            Ok((parsed, t))
        })
        .collect()
}

/// Probe every mirror, and keep only those that agree with the first on both
/// size and validator.
///
/// This is a correctness gate, not an optimisation. Assembling ranges from two
/// mirrors that serve *different* bytes produces a corrupt file that passes every
/// length check — the unsound case the capability lattice names. Mirrors that
/// disagree are dropped with a warning rather than silently mixed in.
/// Probe an object, following redirects and falling back from HEAD to a ranged GET.
///
/// Three real behaviours make this more than a single request:
///   * a redirect: GitHub release assets answer HEAD with `302` and
///     `Content-Length: 0`, so the redirect must be followed rather than described;
///   * a server that does not answer HEAD at all: a public speed-test host closes the
///     connection with no reply (and no TLS `close_notify`) on HEAD while answering
///     GET normally on the same path;
///   * a server that omits `Content-Length` on HEAD, which a ranged GET recovers from
///     `Content-Range`.
///
/// The redirect budget is bounded, so a redirect loop costs a fixed number of round
/// trips rather than running forever.
/// Proxy setting for a URL, honouring `--no-proxy` semantics via the environment.
/// How many bytes of `path` are really there.
///
/// A resume record is authoritative when present. Otherwise fall back to the
/// allocation, which for a sparse file is far smaller than its apparent length.
fn bytes_present(path: &Path, sidecar: Option<&Sidecar>) -> u64 {
    if let Some(sc) = sidecar {
        return sc.bytes_done();
    }
    hya_net::allocated_bytes(path).unwrap_or(0)
}

/// An Outcome with every field at a neutral value, for tests that care about two of them.
#[cfg(test)]
pub fn stub_outcome() -> Outcome {
    Outcome::default()
}

/// Where a job's bytes are destined.
///
/// Decided before a transfer begins so that `--no-save` discards bytes in memory
/// without creating or leaving temporary files on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    /// Write to this path and keep it.
    File(String),
    /// Write nothing anywhere: measure the stream and drop the bytes.
    Discard,
    /// Assemble, then stream to stdout. Needs storage because positioned writes
    /// land out of order, so the object is only correct once complete.
    Stdout(String),
}

/// Decide the destination for a job that would otherwise write `out_path`.
pub fn output_target(job: &Job, out_path: &str) -> OutputTarget {
    // `--stdout` is checked first: it needs a real file even under `--no-save`,
    // because out-of-order ranges cannot be streamed to a pipe as they arrive.
    if job.to_stdout {
        return OutputTarget::Stdout(out_path.to_string());
    }
    if job.no_save {
        return OutputTarget::Discard;
    }
    OutputTarget::File(out_path.to_string())
}

/// Rebuild a job around the mirror list a URL turned out to be serving.
///
/// The derived job keeps everything the user asked for — output path, rate cap,
/// headers, politeness — and replaces only the source list, the attestation, and
/// the ranking. `follow_metalink` is cleared on the result, which bounds the
/// recursion at one hop: a document that names itself, or a redirector that
/// answers a mirror URL with another mirror list, costs one wasted fetch rather
/// than an unbounded chain.
///
/// The output NAME is left alone when the user gave `-O`, and otherwise comes
/// from the document rather than from the URL — `metalink?repo=fedora-40` is not
/// a filename, and the document knows what the object is called.
async fn follow_metalink(job: &Job, from: &Url) -> Result<Job, String> {
    let conn = hya_net::TlsCapableConnector::with_insecure(job.insecure)
        .map(Arc::new)
        .map_err(|e| format!("tls setup failed: {e}"))?;
    let url = from.to_string();
    let doc = crate::metalink::load_url(&conn, &url, &job.headers, &job.user_agent, job.max_redirs)
        .await?;
    let origin = crate::metalink::Origin::Url(url);
    let files = crate::metalink::resolve(&doc, &job.metalink_select, &origin)?;
    // One job fetches one object. A document describing several needs one job
    // each, which only the command-line path can arrange — so a follow that
    // lands on a multi-file document takes the first entry and says so, rather
    // than silently fetching one of several and reporting success.
    let first = files
        .first()
        .ok_or_else(|| format!("{origin}: no usable file entry"))?;
    if files.len() > 1 {
        eprintln!(
            "hydra: {origin} describes {} files; fetching {:?}. Use --metalink <url> --metalink-file NAME to choose another, or --metalink <url> alone to fetch them all.",
            files.len(),
            first.name
        );
    }
    Ok(job.clone().with_metalink(first))
}

impl Job {
    /// Point this job at the sources, ranking and attestation of one document
    /// entry.
    pub fn with_metalink(mut self, r: &crate::metalink::Resolved) -> Job {
        self.urls = r.urls.clone();
        self.source_plans = r.plans.clone();
        self.attested = r.attested.clone();
        self.metalink_notes = r.notes.clone();
        if let Some(n) = r.signature_note() {
            self.metalink_notes.push(n.to_string());
        }
        // One hop only; see `follow_metalink`.
        self.follow_metalink = false;
        if self.output.is_none() {
            self.output = Some(PathBuf::from(&r.name));
        }
        self
    }
}

/// How wide a transfer driven by a mirror list should open: the width a
/// single-URL download would open, spread across hosts.
///
/// Seating every mirror measured worse (a twelve-mirror Fedora document: 5.8 s
/// at three sources, 8.2 s at twelve) because the first split hands every seat
/// a share and the slow ones must be repaired off it. The surplus of a long
/// list pays as reserves, not as seats.
fn mirror_list_width(job: &Job, sources: usize) -> usize {
    let ceiling = job.polite.per_host.min(job.polite.total).max(1);
    sources.clamp(1, ceiling)
}

/// How a source is named in the progress view and in the log.
///
/// The host, plus the port when it is not the scheme's default. The port is
/// almost always redundant and occasionally the only thing that distinguishes
/// two rows — several mirrors behind one name on different ports, or a local
/// test set — and a connection row that cannot be told from its neighbour is
/// not a diagnostic.
fn source_label(u: &Url) -> String {
    let default = match u.scheme.as_str() {
        "https" => 443,
        "ftp" => 21,
        _ => 80,
    };
    if u.port == default {
        u.host.clone()
    } else {
        format!("{}:{}", u.host, u.port)
    }
}

/// A scratch filename unique to this verification.
///
/// Per-CALL, not per-process: several files verify concurrently when multiple URLs are
/// given, and a shared name has them overwriting each other's window.
fn scratch_name() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "hydra_verify_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Check that the bytes already on disk are a genuine prefix of the remote object.
///
/// Re-fetches a window ending at the current file length and compares it byte for byte.
/// This is what lets `hydra <url>` resume a file it did not write: an interrupted
/// download leaves a valid prefix, and one small range request turns that assumption
/// into a check. The window is the last `WINDOW` bytes rather than the whole file
/// because a mismatch anywhere earlier would have to have been written by a *different*
/// object, and the tail is where a truncated write leaves damage.
///
/// Returns the number of verified bytes, or `None` when the prefix does not match.
async fn verify_prefix<C: hya_net::Connector>(
    c: &Arc<C>,
    t: &Target,
    path: &Path,
    on_disk: u64,
    tries: u32,
    timeout_s: f64,
) -> Option<u64> {
    const WINDOW: u64 = 64 * 1024;
    if on_disk == 0 {
        return Some(0);
    }
    let lo = on_disk.saturating_sub(WINDOW);
    let want = (on_disk - lo) as usize;
    // Read what we have.
    let mut local = vec![0u8; want];
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::Start(lo)).ok()?;
        f.read_exact(&mut local).ok()?;
    }
    // Fetch the same window into a scratch sink.
    //
    // The name must be unique PER VERIFICATION, not per process: with several files
    // verifying concurrently (multiple URLs on one command line) a PID-only name has
    // every one of them writing the same scratch file, so each reads back another
    // object's bytes and reports a mismatch on a file that is in fact identical.
    let tmp = std::env::temp_dir().join(scratch_name());
    let tmps = tmp.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&tmps, on_disk).ok()?);
    let ok = fetch_range_retry(
        c.clone(),
        t.clone(),
        lo,
        on_disk,
        sink.clone(),
        tries,
        timeout_s,
    )
    .await
    .is_ok();
    drop(sink);
    let remote = if ok {
        let mut buf = vec![0u8; want];
        use std::io::{Read, Seek, SeekFrom};
        std::fs::File::open(&tmps).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(lo)).ok()?;
            f.read_exact(&mut buf).ok()?;
            Some(buf)
        })
    } else {
        None
    };
    let _ = std::fs::remove_file(&tmp);
    match remote {
        Some(r) if r == local => Some(on_disk),
        _ => None,
    }
}

/// Print the probe exchange with request prefixed by `>` and response prefixed by
/// `<`, so a pasted transcript is unambiguous about direction.
fn print_exchange(pr: &hya_net::Probe) {
    for line in pr.raw_request.lines() {
        if !line.is_empty() {
            println!("> {line}");
        }
    }
    println!(">");
    for line in pr.raw_head.lines() {
        if !line.is_empty() {
            println!("< {line}");
        }
    }
    println!("<");
}

/// Fetch over FTP: single source, sequential, with the protocol's costs made explicit.
///
/// Deliberately not routed through the scheduler. The scheduler decides how to split work
/// across sources and when to reassign it, and both decisions rest on properties FTP does
/// not have: a validator to prove two sources agree, and free preemption. Driving it anyway
/// would produce reassignment decisions priced for the wrong protocol. What FTP does support
/// is a resumable sequential transfer, which is what this does.
async fn ftp_fetch<C: hya_net::Connector + 'static>(
    job: &Job,
    u: &Url,
    p: &mut Progress,
    outs: String,
    conn: Arc<C>,
) -> Outcome {
    // `Outcome::stopped` records a note but prints nothing; `failed` prints. Every failure
    // below goes through this so an exit code always arrives with a reason — an earlier
    // version returned a bare stopped Outcome and an ftp:// fetch exited 1 in silence.
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let why = format!($($arg)*);
            p.end_phase();
            return failed(job, 0, why);
        }};
    }
    use hya_net::scheme::Fetcher;
    let t_all = Instant::now();
    let px = job.proxy_policy().http_route(u).ok().flatten();
    let ep = u.to_endpoint(px.as_ref().map(|(h, pt)| (h.as_str(), *pt)));
    // `--limit-rate` applies here too. One connection means one limiter, and it
    // is built for this fetch alone; the aggregate story the HTTP path tells
    // across connections has nothing to aggregate over.
    let pace = if job.limit_rate > 0 {
        hya_net::polite::Pace::shared(Arc::new(hya_net::polite::RateLimiter::new(job.limit_rate)))
    } else {
        hya_net::polite::Pace::unlimited()
    };
    let f = hya_net::ftp::FtpFetcher::new(conn).with_pace(pace);

    p.phase("connecting and logging in");
    let probe = match f.probe(&ep).await {
        Ok(pr) => pr,
        Err(e) => bail!("ftp: {e}"),
    };
    p.end_phase();
    p.event(
        1,
        &format!(
            "ftp {} -> {} bytes, ranges={}, login={}",
            u.host,
            probe.size,
            if probe.ranged { "yes (REST)" } else { "no" },
            if ep.has_credentials() {
                "explicit"
            } else {
                "anonymous"
            }
        ),
    );
    p.event(
        1,
        &format!(
            "ftp: preemption costs {:.0} control round trips here (HTTP pays none), so the \
             object is fetched sequentially from one source",
            f.capabilities().preempt_cost_rtt
        ),
    );
    if job.server_response {
        for line in probe.raw.lines() {
            println!("{line}");
        }
    }
    if job.spider {
        let ok = probe.size > 0;
        return Outcome::stopped(
            job,
            outs.clone(),
            probe.size,
            ok,
            "spider: FTP object exists",
        );
    }
    if probe.size == 0 {
        bail!(
            "ftp: the server does not implement SIZE, and FTP offers no other way to learn \
             the length; refusing rather than writing a file of unknown extent"
        );
    }
    if job.range.is_some() {
        bail!(
            "ftp: --range is not supported over FTP: REST names a start and nothing names an end"
        );
    }
    if let Some(cap) = job.max_filesize {
        if probe.size > cap {
            bail!(
                "object is {} bytes, exceeding --max-filesize {cap}",
                probe.size
            );
        }
    }
    let keeps_file = !job.no_save && !job.to_stdout;
    let mut outs = if job.to_stdout {
        stdout_stage_path().to_string_lossy().to_string()
    } else {
        outs
    };
    let out_exists = keeps_file && Path::new(&outs).exists();
    if out_exists && !job.resume {
        let on_disk = std::fs::metadata(&outs).map(|m| m.len()).unwrap_or(0);
        let offer = crate::prompt::ResumeOffer::Refused(
            "FTP has no validator to check the bytes on disk against; -c continues them unverified"
                .into(),
        );
        let flags = crate::prompt::Flags {
            resume: false,
            no_clobber: job.no_clobber,
            force: job.force,
            assume_default: job.quiet,
        };
        p.end_phase();
        match crate::prompt::ask(Path::new(&outs), on_disk, probe.size, &offer, flags)
            .unwrap_or(crate::prompt::Existing::Rename)
        {
            crate::prompt::Existing::Skip => {
                return Outcome::stopped(job, outs, on_disk, true, "kept the existing file");
            }
            crate::prompt::Existing::Rename => {
                match crate::prompt::next_free_name(Path::new(&outs)) {
                    Some(fresh) => {
                        p.event(0, &format!("writing to {}", fresh.display()));
                        outs = fresh.to_string_lossy().to_string();
                    }
                    None => bail!("no free filename beside the existing one"),
                }
            }
            _ => {
                let _ = std::fs::remove_file(&outs);
            }
        }
    }

    // Resume uses REST, which is exactly a ranged read — the one place FTP's range support
    // is a clean fit.
    let start = if job.resume && keeps_file {
        std::fs::metadata(&outs).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if start >= probe.size {
        return Outcome::stopped(job, outs.clone(), probe.size, true, "already complete");
    }
    let want_digest_value = job.print_checksum || job.checksum.is_some();
    let sink = if job.no_save {
        let sk = hya_net::SparseSink::discarding();
        Arc::new(if want_digest_value {
            sk.with_digest(hya_net::stream_digest::DEFAULT_REORDER_CAP)
        } else {
            sk
        })
    } else {
        match hya_net::SparseSink::create(&outs, probe.size) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                bail!("cannot create {outs}: {e}");
            }
        }
    };
    // Draw the same progress bar HTTP gets. There is no scheduler to observe —
    // `fetch_range` is one `await` — but the sink counts every byte it writes,
    // so the bar is driven from that counter while the fetch runs. `select!` on
    // a pinned future rather than a spawned task: `Progress` is a `&mut` the
    // caller owns and the fetch borrows `ep`.
    p.end_phase();
    // The size FTP already answered has to reach the renderer, or the bar cannot
    // draw. `ftp_fetch` is handed the setup-phase `Progress`, built before any
    // request when no size was known, and a `None` total renders an empty rule, a
    // `?` percentage and a `?` ETA for the whole transfer when total size is unknown,
    // even though SIZE had returned the exact length during the probe two lines above.
    p.set_total(probe.size);
    p.set_baseline(start);
    let t_xfer = Instant::now();
    let r = {
        let fut = f.fetch_range(&ep, start, probe.size, sink.clone());
        tokio::pin!(fut);
        // 100 ms: `draw` already rate-limits itself to ~12 fps, so a faster tick
        // buys nothing and a slower one makes the bar visibly stutter.
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        ticker.tick().await; // the first tick completes immediately
        let mut last_done = start;
        let mut last_at = Instant::now();
        let mut rate = 0.0f64;
        loop {
            tokio::select! {
                res = &mut fut => break res,
                _ = ticker.tick() => {
                    let done = start + sink.written.load(std::sync::atomic::Ordering::Relaxed);
                    let dt = last_at.elapsed().as_secs_f64();
                    if dt > 0.0 {
                        let sample = done.saturating_sub(last_done) as f64 / dt;
                        // Same smoothing the scheduler applies to its own rate
                        // estimate, so the two paths report comparably rather
                        // than FTP showing a jumpier number for the same link.
                        rate = if rate <= 0.0 { sample } else { 0.3 * sample + 0.7 * rate };
                        last_done = done;
                        last_at = Instant::now();
                    }
                    // One connection: FTP has no validator, so the fetch is
                    // single-source. `(lo, pos, hi)` in that order — passing
                    // `(start, size, done)` swapped the cursor with the end and
                    // drew a permanently full row.
                    let views = vec![ConnView {
                        idx: 0,
                        host: u.host.clone(),
                        range: Some((start, done, probe.size)),
                        rate,
                        health: hya_core::detect::Health::Healthy,
                    }];
                    p.draw(done, &views, Counters { requests: 1, ..Default::default() });
                }
            }
        }
    };
    let xfer = t_xfer.elapsed().as_secs_f64();
    let stream_result = job.no_save.then(|| sink.take_digest(probe.size)).flatten();
    drop(sink);
    if let Err(e) = r {
        if job.to_stdout {
            let _ = std::fs::remove_file(&outs);
        }
        return failed(job, start, format!("ftp: {e}"));
    }

    // Classify what arrived, exactly as the HTTP path does: the payload is the only
    // trustworthy signal, and FTP supplies no content type at all.
    let head = match &stream_result {
        Some((_, h, _)) => h.clone(),
        None => head_of(Path::new(&outs)),
    };
    let name = std::path::Path::new(&outs)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let detection = detect_format(&head, &name, None);
    let digest = match &stream_result {
        Some((d, _, _)) => d.clone(),
        None => want_digest_value
            .then(|| sha256_file(Path::new(&outs)))
            .flatten(),
    };
    let checksum_ok = match (job.checksum.as_deref(), job.no_save) {
        (None, _) => None,
        (Some(spec), true) => match parse_digest_spec(spec) {
            Some((hya_net::digest::Algo::Sha256, want)) => digest.as_ref().map(|d| *d == want),
            _ => {
                if !job.quiet {
                    eprintln!("hydra: --no-save keeps no file, so only a sha256 --checksum can be checked");
                }
                None
            }
        },
        (Some(spec), false) => verify_file_digest(job, Path::new(&outs), spec, digest.as_deref()),
    };
    let mut ok = checksum_ok != Some(false);
    let mut stdout_error = None;
    if job.to_stdout {
        if ok {
            if let Err(e) = copy_span_to_stdout(Path::new(&outs), 0, probe.size) {
                stdout_error = Some(format!("cannot stream to stdout: {e}"));
                ok = false;
            }
        }
        let _ = std::fs::remove_file(&outs);
    }
    let out_path =
        if keeps_file && ok && job.sort_by_type && detection.category != Category::Unknown {
            sort_into_category(PathBuf::from(&outs), detection.category, p)
        } else {
            PathBuf::from(&outs)
        };
    if keeps_file && ok && job.remote_time {
        p.event(
            1,
            "--remote-time: FTP reports no modification time this client reads, skipped",
        );
    }
    if ok {
        save_etag(job, probe.validator.as_deref(), p);
    }
    let elapsed = t_all.elapsed().as_secs_f64();
    p.finish(
        probe.size,
        ok,
        crate::progress::Counters {
            requests: 1,
            ..Default::default()
        },
        digest.as_deref(),
    );
    if let Some(e) = &stdout_error {
        if !job.quiet || job.show_error {
            eprintln!("hydra: {e}");
        }
    }
    if checksum_ok == Some(false) {
        p.note(
            "  checksum MISMATCH: the delivered bytes are not the bytes requested",
            true,
        );
    }
    if !job.quiet && job.verbose == 0 {
        if let Some(fm) = detection.format {
            p.note(&format!("  {}", fm.hint()), false);
        }
    }
    Outcome {
        url: job.urls[0].clone(),
        output: if keeps_file {
            out_path.to_string_lossy().to_string()
        } else {
            String::new()
        },
        size: probe.size,
        elapsed_s: elapsed,
        transfer_s: xfer,
        setup_s: (elapsed - xfer).max(0.0),
        throughput_bps: if xfer > 0.0 {
            (probe.size - start) as f64 / xfer
        } else {
            0.0
        },
        requests: 1,
        connections: 1,
        // No concurrency search on this path, so the peak is what it ran at.
        peak_connections: 1,
        // FTP has no validator, so fetches are single-source by design: one connection
        // held a range for the whole transfer.
        peak_busy_connections: 1,
        connection_seconds: elapsed,
        delta_s: 0.0,
        sha256: digest,
        checksum_ok,
        resumed_from: start,
        ok,
        note: stdout_error,
        format: detection.format.map(|fm| fm.name.to_string()),
        category: Some(detection.category.as_str().to_string()),
        format_conflict: detection.conflict,
        format_label: detection.format.map(|fm| fm.label().to_string()),
        format_description: detection.format.map(|fm| fm.description().to_string()),
        category_description: Some(detection.category.description().to_string()),
    }
}

/// The target a reporting command (`hydra checksum`, `-H NAME`) sends to `u`.
pub fn target_for_public(u: &Url, args: &crate::cli::Cli) -> Result<Target, String> {
    let policy = ProxyPolicy::new(args.proxy.as_deref(), args.no_proxy);
    let mut headers = args.headers.clone();
    if let Some(line) = args.basic_auth_header() {
        push_header_once(&mut headers, line);
    }
    let t = routed_target(u, &policy)?.with_headers(headers, Some(args.user_agent.clone()));
    Ok(with_credentials(t, u, &policy))
}

impl Job {
    /// Everything a transfer takes from the command line.
    ///
    /// One place, so the one-shot path and the queue manager cannot disagree on
    /// what a flag means.
    pub fn from_cli(
        args: &crate::cli::Cli,
        urls: Vec<String>,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Job, String> {
        let limit_rate = args.rate_limit()?;
        let range = match args.range.as_deref() {
            None => args.start_pos.map(RangeSpec::From),
            Some(spec) => Some(RangeSpec::parse(spec).ok_or_else(|| {
                format!("unparsable --range: {spec} (try 0-1023, 1024-, or -512)")
            })?),
        };
        let cookies = crate::cookies::CookieSpec::from_cli(args)?;
        let mut headers = args.headers.clone();
        if let Some(line) = args.basic_auth_header() {
            headers.push(line);
        }
        Ok(Job {
            force_stream: false,
            plugin_options: (!args.no_plugins && crate::plain_download_conflict(args).is_none())
                .then(|| crate::plugins::Options::from_cli(args)),
            ticks: None,
            cookies,
            urls,
            output: args.output.clone(),
            conns: args.requested_conns(),
            resume: args.resume,
            limit_rate,
            max_redirs: args.max_redirs,
            show_error: args.show_error,
            // --logfile truncates, --logfile-append appends. Both name the same sink,
            // so they are collapsed to one field with the mode as a flag; append wins
            // if somehow both are given, since it is the non-destructive reading.
            logfile: args
                .logfile_append
                .clone()
                .map(|p| (p, true))
                .or_else(|| args.logfile.clone().map(|p| (p, false))),
            ip_family: hya_net::IpFamily::from_flags(args.ipv4, args.ipv6),
            tries: args.tries,
            timeout_s: args.timeout,
            connect_timeout_s: args.connect_timeout,
            checksum: args.checksum.clone(),
            headers,
            user_agent: args.user_agent.clone(),
            verbose: args.verbose,
            // --json implies quiet: stdout carries the document, nothing else.
            quiet: args.quiet || args.json,
            no_progress: args.no_progress || args.no_verbose,
            show_progress: args.show_progress,
            polite: args.politeness(),
            adaptive: args.adaptive,
            probe: !args.no_probe,
            to_stdout: args.stdout,
            no_clobber: args.no_clobber,
            create_dirs: args.create_dirs,
            output_dir: args.output_dir.clone(),
            spider: args.spider,
            server_response: args.server_response,
            range,
            max_filesize: args.max_filesize,
            proxy: args.proxy.clone(),
            no_proxy: args.no_proxy,
            remote_time: args.remote_time,
            etag_save: args.etag_save.clone(),
            etag_compare: args.etag_compare.clone(),
            sort_by_type: args.sort_by_type,
            content_type: None,
            content_disposition: args.content_disposition,
            insecure: args.insecure,
            force: args.force,
            no_save: args.no_save,
            print_checksum: args.print_checksum,
            emit_manifest: args.emit_manifest.clone(),
            chunk_digests: args.chunk_digests.clone(),
            chunk_size: args.chunk_size,
            source_plans: Vec::new(),
            attested: None,
            metalink_notes: Vec::new(),
            follow_metalink: !args.no_follow_metalink,
            metalink_select: args.metalink_selection(),
            cancel: Some(cancel.clone()),
        })
    }

    fn proxy_policy(&self) -> ProxyPolicy {
        ProxyPolicy::new(self.proxy.as_deref(), self.no_proxy)
    }

    /// The probe's time budget: `--connect-timeout` when given, else `-T`.
    fn probe_timeout_s(&self) -> f64 {
        self.connect_timeout_s.unwrap_or(self.timeout_s)
    }
}

/// A Job with every option at its default, for callers that only need one or two set.
pub fn default_job() -> Job {
    Job {
        force_stream: false,
        plugin_options: None,
        ticks: None,
        cookies: None,
        urls: Vec::new(),
        output: None,
        conns: None,
        resume: false,
        limit_rate: 0,
        max_redirs: 8,
        ip_family: hya_net::IpFamily::Any,
        show_error: false,
        logfile: None,
        tries: 3,
        timeout_s: 30.0,
        checksum: None,
        headers: Vec::new(),
        user_agent: hya_net::DEFAULT_USER_AGENT.into(),
        verbose: 0,
        quiet: true,
        no_progress: true,
        polite: hya_net::polite::Politeness::default(),
        adaptive: false,
        probe: true,
        to_stdout: false,
        no_clobber: false,
        create_dirs: false,
        output_dir: None,
        spider: false,
        server_response: false,
        range: None,
        max_filesize: None,
        proxy: None,
        no_proxy: false,
        remote_time: false,
        etag_save: None,
        etag_compare: None,
        sort_by_type: false,
        content_type: None,
        insecure: false,
        force: false,
        no_save: false,
        print_checksum: false,
        emit_manifest: None,
        chunk_digests: None,
        chunk_size: None,
        source_plans: Vec::new(),
        attested: None,
        metalink_notes: Vec::new(),
        metalink_select: crate::metalink::Selection::default(),
        // On by default: a URL that answers with `application/metalink4+xml` is
        // a mirror list, and saving it as `big.iso` would hand the user 6 KB of
        // XML named after the 4 GB image they asked for.
        follow_metalink: true,
        content_disposition: false,
        cancel: None,
        connect_timeout_s: None,
        show_progress: false,
    }
}

/// A `Progress` configured from the job: logfile attached, and stdout reserved
/// for the payload under `--stdout`.
///
/// `run` builds two — one for the setup phase (size unknown), one for the
/// transfer (size known) — and both must agree on this configuration: the
/// setup-phase instance missing the stdout reservation was what prepended 71
/// bytes to a piped archive. stdout belongs to the object, on the same
/// principle `--json` follows: a machine channel carries one thing, or it
/// carries nothing usable.
pub(crate) fn progress_for(job: &Job, name: &str, size: Option<u64>) -> Result<Progress, String> {
    let mut p = Progress::new(name, size, job.verbose, job.no_progress, job.quiet);
    if let Some((path, append)) = &job.logfile {
        if let Err(e) = p.set_logfile(path, *append) {
            return Err(format!("cannot open log file {}: {e}", path.display()));
        }
    }
    if job.to_stdout {
        p.reserve_stdout_for_payload();
    }
    if job.show_progress {
        p.force_frame();
    }
    Ok(p)
}

/// Probe `u` for metadata, following redirects, for the reporting commands.
///
/// `hydra checksum` and `--server-response` want the headers of the FINAL
/// response — a 302's headers answer a different question than the one asked.
/// Unlike the transfer path's [`probe_resolving`] there is no `Progress` to
/// log hops to, and reporting prefers to describe what it reached over
/// refusing: on an exhausted hop budget or an unusable `Location` the last
/// probe is returned as-is rather than as an error.
///
/// A HEAD that fails or reports no size falls back to a one-byte ranged GET,
/// the same recovery the transfer path uses — CDNs that mishandle HEAD
/// (unclean TLS close, `Content-Length: 0`) answer the GET correctly.
///
/// Returns the final probe, the URL it came from, and the jar as the chain
/// left it, for a follow-up request to the object that must carry the same
/// cookies the probe did.
pub async fn probe_public<C: hya_net::Connector>(
    c: &C,
    u: &Url,
    args: &crate::cli::Cli,
) -> Result<(hya_net::Probe, Url, CookieJar), String> {
    let mut cur = u.clone();
    let mut chain = hya_net::polite::RedirectChain::new(&cur.to_string());
    let mut hops = 0u32;
    // The same jar the transfer path keeps, for the same reason: `hydra
    // checksum` and `--server-response` describe the object REACHED, and a
    // login-gated one is only reachable by a chain that carries what it was
    // handed. The cookie flags are read here too, so a jar file or a browser
    // import answers a question about the object as well as a download of it.
    let now = hya_net::cookies::now_secs();
    let mut jar = open_jar_for(args, &cur.host, now).await?;
    // The address the user named, kept for the whole chain: it is what decides
    // whether a later hop is still entitled to the credentials they typed.
    let first = target_for_public(u, args)?;
    let policy = ProxyPolicy::new(args.proxy.as_deref(), args.no_proxy);
    let probe_secs = args.connect_timeout.unwrap_or(args.timeout);
    loop {
        let target = with_credentials(
            routed_target(&cur, &policy)?
                .with_headers_from(&first, first.headers.clone(), first.agent.clone())
                .with_jar(&jar, now),
            &cur,
            &policy,
        );
        // One rule for "HEAD said nothing usable", shared with the GUI and the
        // engine rather than restated here: a HEAD that states `Content-Length: 0`
        // has answered, and a ranged GET against a zero-length object is refused.
        let pr = within(probe_secs, "probe", hya_net::probe_resilient(c, &target)).await?;
        jar.store_response(&pr.raw_head, &cur.host, &cur.path, now);
        if pr.is_redirect() && hops < args.max_redirs {
            let next = pr
                .location
                .as_deref()
                .and_then(|loc| crate::url::Url::parse(loc).or_else(|| cur.join(loc)));
            // A hop to an address already asked for is a loop, and a loop has
            // no final response to reach: stop and describe the one in hand,
            // which is what this function does with a budget it cannot spend.
            if let Some(next) = next.filter(|n| chain.advance(&n.to_string())) {
                cur = next;
                hops += 1;
                continue;
            }
        }
        // The same forwarding expressed in HTML rather than in a header, so
        // that `--server-response` and `hydra checksum` describe the object
        // reached rather than the referrer stripper in front of it.
        if pr.maybe_redirector() && hops < args.max_redirs {
            if let Some(next) = hya_net::html_redirect(c, &target)
                .await
                .and_then(|loc| cur.join(&loc))
                .filter(|n| chain.advance(&n.to_string()))
            {
                cur = next;
                hops += 1;
                continue;
            }
        }
        return Ok((pr, cur, jar));
    }
}

/// The jar a reporting command starts with.
///
/// Notices go to stderr: these commands write ONE answer to stdout — a digest,
/// a header value, a JSON document — and the consent line must not end up
/// inside it, but a browser store read without a word is exactly what the line
/// exists to prevent.
pub(crate) async fn open_jar_for(
    args: &crate::cli::Cli,
    host: &str,
    now: u64,
) -> Result<CookieJar, String> {
    let Some(spec) = crate::cookies::CookieSpec::from_cli(args)? else {
        return Ok(CookieJar::new());
    };
    let host = host.to_string();
    let (jar, notes) = tokio::task::spawn_blocking(move || spec.open(&host, now))
        .await
        .map_err(|e| format!("cookie import did not finish: {e}"))??;
    if !args.quiet {
        for n in notes {
            eprintln!("hydra: {n}");
        }
    }
    Ok(jar)
}

/// The jar this job starts with, and what the user should be told about it.
///
/// Off the executor: reading a jar file touches the filesystem and a browser
/// import may spawn the platform's secret-store helper, neither of which
/// belongs on an async runtime thread.
async fn open_jar(job: &Job, now: u64) -> Result<(CookieJar, Vec<String>), String> {
    let Some(spec) = job.cookies.clone() else {
        return Ok((CookieJar::new(), Vec::new()));
    };
    let Some(host) = job.urls.first().and_then(|u| Url::parse(u)).map(|u| u.host) else {
        return Ok((CookieJar::new(), Vec::new()));
    };
    tokio::task::spawn_blocking(move || spec.open(&host, now))
        .await
        .map_err(|e| format!("cookie import did not finish: {e}"))?
}

/// One source after redirect resolution: what described it, and where it ended up.
struct Resolved {
    probe: hya_net::Probe,
    /// The target the transfer must use — post-redirect, which is commonly a
    /// different host from the one asked for.
    target: Target,
    /// The URL the last hop arrived at.
    url: Url,
    /// At least one hop was a redirector PAGE rather than a `3xx`.
    ///
    /// Kept because it changes what the file should be called: a `3xx` chain
    /// starts from a URL the user named, so its filename is theirs to keep
    /// (this is curl's and wget's rule, and `-O` exists for the rest), while a
    /// redirector page means the URL they had named a forwarding stub. Nobody
    /// asked for `index.html`.
    via_html: bool,
    /// The jar as this chain left it.
    ///
    /// Returned rather than shared through a lock because mirrors are probed
    /// CONCURRENTLY: one jar behind a mutex would serialise the fan-out and,
    /// worse, let one mirror's `Set-Cookie` be selected for another mirror's
    /// next hop. A clone per chain cannot do either.
    jar: CookieJar,
}

///
/// Logs into `log` rather than straight to `Progress`, because the callers probe
/// mirrors CONCURRENTLY and a shared renderer cannot be borrowed by several
/// futures at once. Replaying the buffers in index order afterwards also keeps
/// the output deterministic: interleaved by completion, the same mirror list
/// would print in a different order on every run.
async fn probe_resolving<C>(
    c: &C,
    u: &Url,
    t: &Target,
    log: &mut Vec<(u8, String)>,
    max_redirs: u32,
    jar: &CookieJar,
    now: u64,
    policy: &ProxyPolicy,
    probe_secs: f64,
    force_stream: bool,
) -> Result<Resolved, String>
where
    C: hya_net::Connector,
{
    // The hop budget is the CLI's, not a constant. `--max-redirs` was parsed,
    // validated, and listed in `--help`, but the resolver used a hardcoded
    // `MAX_HOPS = 8` that nothing could influence: `--max-redirs 0` followed the
    // redirect and downloaded the object anyway.
    //
    // `0` means refuse to follow any redirect hops. One probe still
    // happens — that is how a redirect is discovered at all — but the hop is not
    // taken.
    let max_hops = max_redirs as usize;
    // `t` carries the USER's headers and nothing the jar produced: the jar's
    // `Cookie:` is derived here, per hop, so a cookie the chain expires with
    // `Max-Age=0` is gone from the next request rather than restored from a
    // copy taken before it was.
    let mut target = t.clone().with_jar(jar, now);
    let mut current = u.clone();
    let mut chain = hya_net::polite::RedirectChain::new(&current.to_string());
    let mut via_html = false;
    let mut guise = hya_net::Guise::default();
    // The jar travels with the chain, whether or not a cookie flag was given.
    // A login-gated CDN answers the first request with `Set-Cookie` and a `302`
    // and expects the cookie back on the second; without this the second hop
    // goes out bare and the download `403`s in a way that looks like a server
    // fault. Holding it costs nothing and reaches nothing — the jar dies with
    // the chain, is never written, and selects by the destination host — so
    // making it conditional on a flag would only mean the users who did not
    // know to pass one still cannot fetch the file.
    let mut jar = jar.clone();
    // `0..=max_hops`: the extra pass is what answers the request that the last
    // permitted hop arrived at. Without it a budget of N would resolve only N-1.
    for hop in 0..=max_hops {
        // HEAD, then a ranged GET when it gives nothing usable — see
        // [`hya_net::probe_resilient`], which is also what the GUI and the engine
        // ask, so the three cannot drift apart on which servers they can read.
        let (pr, admitted) = hya_net::probe_admitted(&target, |t| async move {
            let probe = async {
                if force_stream {
                    hya_net::probe(c, &t).await
                } else {
                    probe_resilient(c, &t).await
                }
            };
            within(probe_secs, "probe", probe).await
        })
        .await?;
        if admitted != hya_net::Guise::default() {
            log.push((
                1,
                format!(
                    "{} refused the request as written; served as {admitted:?}",
                    current.host
                ),
            ));
            target = target.in_guise(&admitted);
            guise.absorb(admitted);
        }
        // Counted, never quoted: the value is a bearer credential and `-v` is
        // read in terminals, CI logs and bug reports.
        let set = jar.store_response(&pr.raw_head, &current.host, &current.path, now);
        if set > 0 {
            log.push((2, format!("{} set {set} cookie(s)", current.host)));
        }

        if pr.is_redirect() {
            let loc = pr.location.clone().unwrap_or_default();
            if hop >= max_hops {
                return Err(if max_hops == 0 {
                    format!("refusing to follow a redirect to {loc:?}: --max-redirs 0")
                } else {
                    format!("too many redirects (--max-redirs {max_redirs})")
                });
            }
            let next = current
                .join(&loc)
                .ok_or_else(|| format!("unparsable redirect target {loc:?}"))?;
            // A hop back to an address already asked for is a loop. The
            // remaining budget cannot break it, and spending it reports
            // "too many redirects" for a chain that never moved.
            if !chain.advance(&next.to_string()) {
                return Err(format!("redirect loop: {next} was already requested"));
            }
            log.push((1, format!("redirect {} -> {}", current.host, next.host)));
            target = with_credentials(
                routed_target(&next, policy)
                    .map_err(|e| format!("redirect target unusable: {e}"))?
                    .with_headers_from(t, t.headers.clone(), t.agent.clone())
                    .in_guise(&guise)
                    .with_jar(&jar, now),
                &next,
                policy,
            );
            current = next;
            continue;
        }

        // A redirect the server expressed in HTML instead of in a header: a
        // referrer stripper or link filter answering `200` with a page whose
        // whole content is "go here instead". Charged to the SAME hop budget —
        // two such pages pointing at each other is a loop like any other, and
        // `--max-redirs` is what bounds it.
        if pr.maybe_redirector() && hop < max_hops {
            let limit = std::time::Duration::from_secs_f64(probe_secs.max(0.001));
            if let Some(next) = tokio::time::timeout(limit, hya_net::html_redirect(c, &target))
                .await
                .ok()
                .flatten()
                .and_then(|loc| current.join(&loc))
            {
                if !chain.advance(&next.to_string()) {
                    return Err(format!("redirect loop: {next} was already requested"));
                }
                log.push((
                    1,
                    format!("html redirect {} -> {}", current.host, next.host),
                ));
                target = with_credentials(
                    routed_target(&next, policy)
                        .map_err(|e| format!("redirect target unusable: {e}"))?
                        .with_headers_from(t, t.headers.clone(), t.agent.clone())
                        .in_guise(&guise)
                        .with_jar(&jar, now),
                    &next,
                    policy,
                );
                current = next;
                via_html = true;
                continue;
            }
        }
        // An error status is an answer, not a description of the object:
        // `probe_resilient` returns it rather than failing so a `404` from HEAD
        // is reported instead of "the ranged GET was unsatisfiable", and without
        // this check a `400` with a 24-byte JSON body became a 24-byte object.
        // No host in the message: both callers name it themselves.
        if let Some(why) = pr.refusal() {
            return Err(why);
        }
        return Ok(Resolved {
            probe: pr,
            target,
            url: current,
            via_html,
            jar,
        });
    }
    Err(format!("too many redirects (--max-redirs {max_redirs})"))
}

/// One mirror's probe, as it comes back from the concurrent fan-out.
///
/// The index is what everything downstream is aligned to; the URL is carried
/// alongside because a probe may fail before there is a `Resolved` to read it
/// from, and a failure that cannot name its host is not a diagnostic.
type ProbeOutcome = (usize, Url, Result<Resolved, String>, Vec<(u8, String)>);

/// Probe every mirror and keep the ones that can be assembled together.
///
/// # Two different tests, because there are two different kinds of evidence
///
/// Without a mirror list the only evidence available is what the mirrors
/// themselves say, so agreement has to be established PAIRWISE: same size and
/// the same strong validator as the first source. That is the right test for its
/// evidence, and it is deliberately strict — two mirrors serving different
/// builds produce a file that passes every length check and is silently wrong.
///
/// `attested_size` is different evidence. It comes from a Metalink document,
/// published by whoever built the object, on a host that is usually not any of
/// the mirrors, alongside a content digest for the whole file. Against that, the
/// pairwise validator test is not merely unnecessary — it is unsatisfiable:
/// independent mirror operators run independent web servers and cannot share an
/// `ETag`, so requiring one keeps exactly ONE source out of a nineteen-mirror
/// list. The document's size is the admission test instead, and its digest (per
/// chunk, where `<pieces>` is published) is what actually catches a mirror
/// serving something else.
/// May a mirror that answered AFTER the transfer started join the bench?
///
/// The oath is the seats': a reserve's bytes are spliced into the same file on
/// substitution, so its admission cannot be weaker than the front door's. With
/// a document, that is the attested size. Without one, the pairwise gate: the
/// first source's size AND its strong validator — a weak or absent validator
/// admits nothing late, exactly as it admits nothing up front. Ranges are
/// required either way, because the first thing a substituted source is asked
/// for is a range.
fn bench_admission(
    attested_size: Option<u64>,
    first_size: u64,
    first_strong_validator: Option<&str>,
    pr: &hya_net::Probe,
) -> bool {
    if !pr.ranges {
        return false;
    }
    match attested_size {
        Some(want) => pr.size == want,
        None => {
            pr.size == first_size
                && !pr.weak_validator
                && first_strong_validator.is_some()
                && pr.validator.as_deref() == first_strong_validator
        }
    }
}

/// Mirrors admitted after the transfer has already started.
///
/// `(index into the caller's `pairs`, post-redirect target)`. The caller pairs
/// them with the rankings it holds and feeds them to the reserve bench.
type LateMirrors = tokio::sync::mpsc::UnboundedReceiver<(usize, Target)>;

/// What probing the mirror list produced.
struct Probed {
    /// The probe the transfer's size, validator and type are read from.
    first: hya_net::Probe,
    /// Indices of the mirrors that may be SEATED, in the caller's order.
    keep: Vec<usize>,
    /// Post-redirect targets, by index.
    resolved: Vec<(usize, Target)>,
    /// The name a redirector page moved the object to, if it did.
    renamed: Option<String>,
    /// Mirrors still being probed when the transfer was allowed to start.
    late: Option<LateMirrors>,
    /// Wall clock to the first successful probe, as the per-request setup cost.
    ///
    /// This is the only measurement of the path's latency taken before the
    /// transfer, and everything timed in units of `delta` depends on it: the
    /// ramp's measurement windows, the stall timeout, the repair deadband. It
    /// used to be discarded and a 50 ms prior used instead, which is roughly
    /// right for a nearby origin and half the truth on a transatlantic one —
    /// measured on a 100 ms path, the ramp judged each newly admitted connection
    /// over a 0.15 s window, caught it mid-slow-start, concluded it was slower
    /// than the level below, and settled at one connection on a link that
    /// scaled to eight.
    first_rtt: f64,
    /// Every chain's jar, merged, for `--save-cookies` to write out.
    jar: CookieJar,
}

async fn probe_all(
    conn: &Arc<TlsCapableConnector>,
    pairs: &[(Url, Target)],
    p: &mut Progress,
    job: &Job,
    attested_size: Option<u64>,
    want_seats: usize,
    jar: &CookieJar,
    now: u64,
) -> Result<Probed, String> {
    let max_redirs = job.max_redirs;
    let policy = job.proxy_policy();
    let probe_secs = job.probe_timeout_s();
    let force_stream = job.force_stream;
    // Concurrent: in series, a twelve-mirror Fedora document cost 14.4 s of
    // HEADs before the first byte. See `hya_net::PROBE_FANOUT` for the bound.
    let gate = Arc::new(tokio::sync::Semaphore::new(hya_net::PROBE_FANOUT));
    let mut set = tokio::task::JoinSet::new();
    for (i, (u, t)) in pairs.iter().enumerate() {
        let c = conn.clone();
        let gate = gate.clone();
        let (u, t) = (u.clone(), t.clone());
        let jar = jar.clone();
        let policy = policy.clone();
        set.spawn(async move {
            let mut log: Vec<(u8, String)> = Vec::new();
            let permit = gate.acquire_owned().await;
            let r = probe_resolving(
                c.as_ref(),
                &u,
                &t,
                &mut log,
                max_redirs,
                &jar,
                now,
                &policy,
                probe_secs,
                force_stream,
            )
            .await;
            drop(permit);
            (i, u, r, log)
        });
    }
    // How long to hold still hoping for one more seat. A mirror that misses
    // the window keeps probing and joins the reserve bench when it answers, so
    // this is short: three times the fastest mirror's round trip, floored so a
    // LAN-fast first answer cannot make it unreasonably tight, capped so a
    // pathological one cannot reintroduce the wait. It opens only once a mirror
    // has been admitted, and never applies to a single-source run.
    const PROBE_GRACE_MULTIPLE: f64 = 3.0;
    const PROBE_GRACE_MIN: std::time::Duration = std::time::Duration::from_millis(600);
    const PROBE_GRACE_MAX: std::time::Duration = std::time::Duration::from_secs(10);
    // Stop waiting once the seats are filled: everything past that only fills
    // a reserve bench nothing consults until a source fails, and waiting for
    // the slowest HEAD cost 2.0 s of dead time in front of a ~5 s transfer.
    // Mirrors still in flight keep probing and join the bench as they are
    // admitted. The grace window below is only the backstop for a list that
    // never yields enough seats at all.
    let (late_tx, late_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut done: Vec<ProbeOutcome> = Vec::new();
    let probe_start = Instant::now();
    let mut first_ok: Option<std::time::Duration> = None;
    let mut seated = 0usize;
    let mut streaming = false;
    while !set.is_empty() {
        if seated >= want_seats {
            streaming = true;
            break;
        }
        let joined = match first_ok {
            Some(fastest) if pairs.len() > 1 => {
                let window = std::time::Duration::from_secs_f64(
                    fastest.as_secs_f64() * PROBE_GRACE_MULTIPLE,
                )
                .clamp(PROBE_GRACE_MIN, PROBE_GRACE_MAX);
                let left = window.saturating_sub(probe_start.elapsed());
                match tokio::time::timeout(left, set.join_next()).await {
                    Ok(v) => v,
                    // Out of patience for seats — not out of interest. The
                    // stragglers keep probing and become reserves.
                    Err(_) => {
                        streaming = true;
                        break;
                    }
                }
            }
            _ => set.join_next().await,
        };
        let Some(joined) = joined else { break };
        match joined {
            Ok(v) => {
                if v.2.is_ok() {
                    if first_ok.is_none() {
                        first_ok = Some(probe_start.elapsed());
                    }
                    // Counted on the ADMISSION test the loop below applies, so
                    // "enough seats" means enough usable mirrors and not merely
                    // enough answers. A document that states a size makes this
                    // exact. Without one the pairwise validator gate can still
                    // reject a counted mirror afterwards, so the transfer may
                    // open on fewer sources than it hoped — a slight
                    // undershoot, healed by substitution from the bench, and
                    // cheaper than holding the transfer to find out.
                    let usable = match (attested_size, v.2.as_ref().ok()) {
                        (Some(want), Some(r)) => r.probe.size == want,
                        _ => true,
                    };
                    seated += usize::from(usable);
                }
                done.push(v);
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => return Err(format!("probe task failed: {e}")),
        }
    }
    if streaming && !set.is_empty() {
        p.event(
            1,
            &format!(
                "starting on {seated} source(s) after {:.2}s; {} more mirror(s) are still \
                 being probed and join the reserve bench as they answer",
                probe_start.elapsed().as_secs_f64(),
                set.len(),
            ),
        );
    }
    // Back into the ORDER THE CALLER GAVE, not the order the network answered
    // in. Everything downstream is index-aligned with `pairs` — the mirror
    // ranking, the per-source connection split, the progress rows — and "which
    // mirror is source 0" must not depend on which handshake finished first, or
    // two runs against the same document cannot be compared.
    done.sort_by_key(|(i, ..)| *i);

    let mut first: Option<hya_net::Probe> = None;
    // The first probe failure, worded for a user and naming its host. Kept
    // because a lone source's failure IS the transfer's error: summarising a
    // one-item list as "every probe failed" throws away the only thing the
    // user can act on, which for an object that answers `400` is the status
    // the server already gave.
    let mut lone_failure: Option<String> = None;
    // What every chain learned, folded back together for `--save-cookies`.
    // Merging is safe because a jar selects by the host of the request it is
    // asked about, so two mirrors' sessions cannot be confused for each other.
    let mut merged = jar.clone();
    let mut keep = Vec::new();
    // Targets after redirect resolution, paired with the index they came from.
    let mut resolved_targets: Vec<(usize, Target)> = Vec::new();
    // The name the FIRST source ended up at, when a redirector page moved it.
    let mut renamed: Option<String> = None;
    for (i, u, res, log) in done {
        for (level, line) in log {
            p.event(level, &line);
        }
        match res {
            Ok(r) => {
                merged.extend(r.jar);
                let (pr, resolved) = (r.probe, r.target);
                // The transfer uses the resolved target, unless it is a
                // short-lived signed URL (`hya_net::signed::perishable`): then the
                // durable address is the original, re-followed per request.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let target = match hya_net::signed::perishable(&r.url.to_string(), now) {
                    true => pairs.get(i).map(|(_, t)| t.clone()).unwrap_or(resolved),
                    false => resolved,
                };
                resolved_targets.push((i, target));
                if i == 0 && r.via_html {
                    // Content-Disposition first: a redirector's destination is
                    // as entitled to name its own file as any other object.
                    renamed = pr
                        .suggested_filename()
                        .or_else(|| Some(r.url.suggested_filename()));
                }
                let (size, ranges, validator) = (pr.size, pr.ranges, pr.validator.clone());
                p.event(
                    1,
                    &format!(
                        "probe {} -> {} bytes, ranges={}, validator={}",
                        u.host,
                        size,
                        if ranges { "yes" } else { "no" },
                        validator.as_deref().unwrap_or("none")
                    ),
                );
                // A document's stated size is the admission test when there is
                // one, for every source including the first: a mirror that
                // disagrees with the document is wrong even if it happens to be
                // the one probed first.
                if let Some(want) = attested_size {
                    if size == want {
                        keep.push(i);
                        if first.is_none() {
                            first = Some(pr);
                        }
                    } else {
                        p.event(
                            0,
                            &format!(
                                "skipping {}: serves {size} bytes, the mirror list says {want}",
                                u.host
                            ),
                        );
                    }
                    continue;
                }
                match &first {
                    None => {
                        first = Some(pr);
                        keep.push(i);
                    }
                    Some(f0) => {
                        let same_size = f0.size == size;
                        let same_val = match (&f0.validator, &validator) {
                            (Some(a), Some(b)) => a == b,
                            // Without validators on both sides, byte identity
                            // across mirrors is unverifiable. Refuse to mix.
                            _ => false,
                        };
                        if same_size && same_val {
                            keep.push(i);
                        } else {
                            p.event(
                                0,
                                &format!(
                                    "skipping {}: {} (cannot prove it serves the same bytes)",
                                    u.host,
                                    if same_size {
                                        "validator differs"
                                    } else {
                                        "size differs"
                                    }
                                ),
                            );
                        }
                    }
                }
            }
            Err(e) => {
                p.event(0, &format!("probe failed for {}: {e}", u.host));
                if lone_failure.is_none() {
                    lone_failure = Some(format!("{e} for {}", u.host));
                }
            }
        }
    }
    // Only now, with `first` in hand, can the stragglers be given their
    // admission test. A reserve's bytes are spliced into the same file, so the
    // test is the one the seats took — the attested size with a document, the
    // pairwise size-and-strong-validator gate without — plus range support,
    // which is the first thing a substituted source is asked for.
    let late = match (streaming && !set.is_empty(), &first) {
        (true, Some(f0)) => {
            let first_size = f0.size;
            // Only a STRONG validator authorises pairwise splicing; a weak one
            // admits nothing late, exactly as it admits nothing up front.
            let first_validator = f0.validator.clone().filter(|_| !f0.weak_validator);
            tokio::spawn(async move {
                while let Some(joined) = set.join_next().await {
                    let Ok((i, _u, res, _log)) = joined else {
                        continue;
                    };
                    let Ok(r) = res else { continue };
                    if !bench_admission(
                        attested_size,
                        first_size,
                        first_validator.as_deref(),
                        &r.probe,
                    ) {
                        continue;
                    }
                    // A closed receiver means the transfer ended; nothing left
                    // to probe for.
                    if late_tx.send((i, r.target)).is_err() {
                        return;
                    }
                }
            });
            Some(late_rx)
        }
        _ => None,
    };
    match first {
        Some(pr) if !keep.is_empty() => Ok(Probed {
            first: pr,
            keep,
            resolved: resolved_targets,
            renamed,
            late,
            // A probe is a connect plus a request, so this overstates one
            // round trip and understates nothing. Clamped the way the GUI
            // clamps the same measurement.
            first_rtt: first_ok
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.05)
                .clamp(0.05, 45.0),
            jar: merged,
        }),
        // One URL, one answer. With a mirror list the per-source lines above
        // have already said what each one did, and the summary is the honest
        // description of the set; with a single source there is no set.
        _ => Err(match lone_failure {
            Some(e) if pairs.len() == 1 => e,
            _ => "no usable source: every probe failed".into(),
        }),
    }
}

/// Reduce a range-mode output to just the fetched span.
///
/// The transfer wrote `[lo, hi)` at its true offsets inside a file of the whole
/// object's length, so everything outside the span is a hole that reads as
/// zeros. This moves the span to offset 0 and truncates.
///
/// Copied in bounded blocks rather than read whole: the point of positioned
/// writes is that memory does not scale with the object, and a range can be
/// gigabytes. `lo == 0` skips the move and only truncates.
fn extract_span(path: &Path, lo: u64, hi: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let span = hi.saturating_sub(lo);
    if lo > 0 {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let mut buf = vec![0u8; 1 << 20];
        let (mut src, mut dst, mut left) = (lo, 0u64, span);
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            f.seek(SeekFrom::Start(src))?;
            f.read_exact(&mut buf[..want])?;
            f.seek(SeekFrom::Start(dst))?;
            f.write_all(&buf[..want])?;
            src += want as u64;
            dst += want as u64;
            left -= want as u64;
        }
        f.flush()?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(span)
}

/// Verify a finished file chunk-by-chunk against a manifest, refetching any
/// chunk whose digest does not match.
///
/// Returns a human summary. An unrepairable mismatch is an error, not a warning:
/// the file is known-wrong at a known offset, and reporting success for it would
/// be the silent-corruption failure this project keeps designing against.
async fn verify_and_repair_chunks(
    conn: &Arc<TlsCapableConnector>,
    usable: &[(Url, Target)],
    out_path: &Path,
    manifest_path: &Path,
    job: &Job,
    p: &mut Progress,
) -> Result<String, String> {
    use hya_net::manifest::{Manifest, Trust};

    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read manifest {}: {e}", manifest_path.display()))?;
    let m = Manifest::parse(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    // A manifest handed as a local file is trusted for chunk verification.
    verify_and_repair(conn, usable, out_path, m, Trust::Trusted, job, p).await
}

/// Verify every chunk against a manifest already in hand, refetching failures.
///
/// Split from [`verify_and_repair_chunks`] because the manifest does not always
/// come from a file. A Metalink `<pieces>` list is the same thing arriving by a
/// different road, and it is the road that matters more: `--chunk-digests`
/// requires a user who already has a manifest, while a mirror list ships one
/// with the mirrors it belongs to.
///
/// `trust` decides only whether the digests may name erasure positions for a
/// parity decode — see [`hya_net::manifest::Trust`]. Detection and targeted
/// refetch work at either level, which is what this function does.
async fn verify_and_repair(
    conn: &Arc<TlsCapableConnector>,
    usable: &[(Url, Target)],
    out_path: &Path,
    m: hya_net::manifest::Manifest,
    trust: hya_net::manifest::Trust,
    job: &Job,
    p: &mut Progress,
) -> Result<String, String> {
    use hya_net::manifest::ChunkVerifier;

    let mut v = ChunkVerifier::new(m, trust);

    p.phase("verifying chunks");
    {
        let mut f = std::fs::File::open(out_path)
            .map_err(|e| format!("cannot reopen {} to verify: {e}", out_path.display()))?;
        v.write_reader(&mut f)
            .map_err(|e| format!("read failed while verifying: {e}"))?;
    }
    p.end_phase();

    if v.all_verified() {
        return Ok(format!("all {} chunks verified", v.verified_count()));
    }

    let bad = v.failed_indices().to_vec();
    p.event(
        0,
        &format!(
            "{} chunk(s) failed verification: {:?} — refetching",
            bad.len(),
            bad
        ),
    );

    let sink = Arc::new(
        hya_net::SparseSink::create(&out_path.to_string_lossy(), v.manifest().object.size)
            .map_err(|e| format!("cannot reopen {} to repair: {e}", out_path.display()))?,
    );

    let mut repaired = 0usize;
    for (nth, idx) in bad.into_iter().enumerate() {
        let (lo, hi) = v.manifest().span(idx);
        // Rotate through the sources, starting past the first. Which host
        // served the corrupt chunk is unknowable from here, so a FIXED
        // alternate is a coin-flip that repeats itself: if the alternate is the
        // bad mirror, every refetch fails and the repair dies on its first
        // candidate. Rotation costs nothing and puts each retry somewhere new.
        let t = usable[(1 + nth) % usable.len()].1.clone();
        p.event(1, &format!("refetching chunk {idx} [{lo},{hi})"));
        hya_net::fetch_range_retry(
            conn.clone(),
            t,
            lo,
            hi,
            sink.clone(),
            job.tries,
            job.timeout_s,
        )
        .await
        .map_err(|e| format!("chunk {idx} refetch failed: {e}"))?;

        // Re-verify from disk. A refetch that is ALSO corrupt must not be
        // accepted just because it was requested again.
        let mut fresh = vec![0u8; (hi - lo) as usize];
        {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut f = std::fs::File::open(out_path).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(lo)).map_err(|e| e.to_string())?;
            f.read_exact(&mut fresh).map_err(|e| e.to_string())?;
        }
        v.retry(idx);
        if !v.write(lo, &fresh).is_empty() {
            return Err(format!(
                "chunk {idx} [{lo},{hi}) still fails its digest after refetch: the source is \
                 serving bytes that do not match the manifest, so the file cannot be completed \
                 correctly from it"
            ));
        }
        repaired += 1;
    }

    Ok(format!(
        "{} chunks verified, {repaired} repaired by targeted refetch",
        v.verified_count()
    ))
}

/// Hash a file in fixed-size chunks.
///
/// # Why not `std::fs::read`
///
/// Reading the whole object into a `Vec` to hash it makes peak memory scale with the
/// object, which is the one thing a downloader must never do: the transfer itself
/// writes at exact offsets and holds no reassembly buffer, so the digest was the only
/// part of the program that could not fetch a file larger than RAM.
///
/// Measured on a 121.7 MiB release asset: buffering the entire file scaled RSS
/// to 127 MB instead of a constant ~11-14 MB. The signature that identified it was
/// that hydra's FAILED runs used 11 MB — a failed transfer skips the digest, so the
/// footprint was entirely this function. A 1 GiB download would have needed 1 GiB
/// of resident memory.
///
/// 1 MiB chunks: large enough that syscall overhead is negligible against disk
/// throughput, small enough to stay in L2 and to keep the resident cost constant
/// regardless of object size.
/// Hash a file with any algorithm a mirror list may publish.
///
/// # Why this is not just `sha256_file`
///
/// A Metalink publishes whatever the project's build system produced, and that
/// is frequently not SHA-256: Metalink 3.0 documents in the wild carry MD5 and
/// SHA-1 far more often, sometimes alongside SHA-256 and SHA-512 and sometimes
/// instead of it. Verifying only when the document happens to have published a
/// SHA-256 would silently skip the check on the documents most likely to need
/// it.
///
/// Streamed in fixed chunks for the same reason [`sha256_file`] is: buffering
/// the object to hash it makes peak memory scale with the object, which is the
/// one thing a downloader must never do.
///
/// MD5 and SHA-1 are used here as INTEGRITY checks, not as authentication.
/// Against a transmission fault, a truncating proxy, or a mirror serving a stale
/// build — which is what a published digest is actually for — they work. Against
/// an adversary who chose the bytes, they do not, and no amount of care at this
/// call site changes that: the digest and the object came down the same wire.
fn digest_file(path: &Path, algo: hya_net::digest::Algo) -> Option<String> {
    use hya_net::digest::Algo;
    use std::io::Read;
    // CRC32 is not implemented here: it is an error-detecting code whose
    // collisions are arithmetic, and a "verified" that means that little is
    // worse than an honest "not checked".
    if matches!(algo, Algo::Crc32 | Algo::Crc32c) {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sha256 = Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut sha1 = sha1::Sha1::new();
    let mut md5 = md5::Md5::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => match algo {
                Algo::Sha256 => sha256.update(&buf[..n]),
                Algo::Sha512 => sha512.update(&buf[..n]),
                Algo::Sha1 => sha1.update(&buf[..n]),
                Algo::Md5 => md5.update(&buf[..n]),
                Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(hya_net::digest::to_lower_hex(&match algo {
        Algo::Sha256 => sha256.finalize().to_vec(),
        Algo::Sha512 => sha512.finalize().to_vec(),
        Algo::Sha1 => sha1.finalize().to_vec(),
        Algo::Md5 => md5.finalize().to_vec(),
        Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
    }))
}

/// Split an `algo:hex` digest spec, defaulting a bare digest to SHA-256.
///
/// A bare 64-hex value is what `--checksum` has always accepted; anything with a
/// prefix comes from a mirror list and names its own algorithm.
fn parse_digest_spec(spec: &str) -> Option<(hya_net::digest::Algo, String)> {
    let t = spec.trim().to_ascii_lowercase();
    match t.split_once(':') {
        Some((a, h)) => hya_net::digest::Algo::parse(a).map(|algo| (algo, h.trim().to_string())),
        None => Some((hya_net::digest::Algo::Sha256, t)),
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(hya_net::digest::to_lower_hex(&h.finalize()))
}

/// Copy `[lo, hi)` of `path` to stdout in bounded blocks: a pipeline is where
/// buffering the whole object is least affordable.
fn copy_span_to_stdout(path: &Path, lo: u64, hi: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(lo))?;
    let mut so = std::io::stdout().lock();
    let mut left = hi.saturating_sub(lo);
    let mut buf = vec![0u8; 1 << 20];
    while left > 0 {
        let want = (buf.len() as u64).min(left) as usize;
        f.read_exact(&mut buf[..want])?;
        so.write_all(&buf[..want])?;
        left -= want as u64;
    }
    so.flush()
}

/// The category directory a finished file moves into: beside where it landed,
/// so an absolute `-O /a/b/f` sorts into `/a/b/<Category>/f` and never into
/// the working directory.
pub fn sorted_destination(out_path: &Path, category_dir: &str) -> PathBuf {
    let base = out_path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(category_dir)
        .join(out_path.file_name().unwrap_or_default())
}

fn sort_into_category(out_path: PathBuf, category: Category, p: &mut Progress) -> PathBuf {
    let dest = sorted_destination(&out_path, category.directory());
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("hydra: could not create {}: {e}", dir.display());
        return out_path;
    }
    match std::fs::rename(&out_path, &dest) {
        Ok(()) => {
            p.event(0, &format!("sorted into {}", dir.display()));
            dest
        }
        Err(e) => {
            eprintln!("hydra: could not move into {}: {e}", dir.display());
            out_path
        }
    }
}

/// `--remote-time`: `Last-Modified` first, the validator as the fallback for
/// servers that send only a date. An ETag is opaque and carries no time.
fn apply_remote_time(
    path: &Path,
    last_modified: Option<&str>,
    validator: Option<&str>,
    p: &mut Progress,
) {
    match last_modified
        .or(validator)
        .and_then(hya_net::polite::parse_http_date)
    {
        Some(secs) => {
            let _ = std::fs::File::options()
                .write(true)
                .open(path)
                .and_then(|f| f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs)));
        }
        None => p.event(
            1,
            "--remote-time: server sent no Last-Modified header, skipped",
        ),
    }
}

/// `--etag-save`, written only for a complete transfer that verified: a stored
/// validator for bytes that never fully arrived would make the next
/// `--etag-compare` skip a download that still needs doing.
fn save_etag(job: &Job, validator: Option<&str>, p: &mut Progress) {
    if let (Some(path), Some(v)) = (&job.etag_save, validator) {
        if let Err(e) = std::fs::write(path, v) {
            p.event(
                0,
                &format!("cannot write --etag-save {}: {e}", path.display()),
            );
        }
    }
}

/// `--checksum` against a file on disk, reusing `sha256` when it is already
/// known. `None` means the algorithm could not be checked.
fn verify_file_digest(job: &Job, path: &Path, spec: &str, sha256: Option<&str>) -> Option<bool> {
    let Some((algo, want)) = parse_digest_spec(spec) else {
        if !job.quiet {
            eprintln!("hydra: cannot check {spec:?}: unknown digest algorithm");
        }
        return None;
    };
    let got = match (algo, sha256) {
        (hya_net::digest::Algo::Sha256, Some(d)) => Some(d.to_string()),
        _ => digest_file(path, algo),
    };
    got.map(|g| g == want)
}

/// The first bytes of a finished file, for classification.
fn head_of(path: &Path) -> Vec<u8> {
    use std::io::Read as _;
    let mut buf = vec![0u8; 8192];
    match std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => {
            buf.truncate(n);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// One sequential stream for an object whose size the server would not state.
///
/// No ranges, so no parallelism and no resume; everything else the command
/// line asked for still applies, which is why this is not a bare fetch: the
/// rate cap, the digest check, the output target, the existing-file decision,
/// the mtime and the ETag are all honoured here as on the ranged path.
#[allow(clippy::too_many_arguments)]
async fn stream_unknown_size(
    job: &Job,
    conn: &Arc<TlsCapableConnector>,
    target: &Target,
    probe_info: &hya_net::Probe,
    name: &str,
    out_path: PathBuf,
    empty_object: bool,
    p: &mut Progress,
) -> Outcome {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    if job.range.is_some() {
        return failed(
            job,
            0,
            "--range needs an object whose size the server states; this one has none".into(),
        );
    }
    p.event(
        0,
        if empty_object {
            "the object is empty (the server stated Content-Length: 0)"
        } else {
            "size unknown: streaming with one connection (no parallelism, no resume)"
        },
    );
    if job.resume {
        p.event(
            0,
            "size unknown: nothing on disk can be verified as part of this object, so -c starts over",
        );
    }
    let mut out_path = out_path;
    let keeps_file = !job.no_save && !job.to_stdout;
    if job.no_save {
        out_path = std::env::temp_dir().join(format!("hydra_discard_{}", scratch_name()));
    }
    if keeps_file && out_path.exists() {
        let on_disk = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
        let offer = crate::prompt::ResumeOffer::Refused(
            "the server does not state a size, so nothing on disk can be verified as part of \
             this object"
                .into(),
        );
        let flags = crate::prompt::Flags {
            resume: job.resume,
            no_clobber: job.no_clobber,
            force: job.force,
            assume_default: job.quiet,
        };
        p.end_phase();
        match crate::prompt::ask(&out_path, on_disk, 0, &offer, flags)
            .unwrap_or(crate::prompt::Existing::Rename)
        {
            crate::prompt::Existing::Skip => {
                return Outcome::stopped(
                    job,
                    out_path.to_string_lossy().to_string(),
                    on_disk,
                    true,
                    "kept the existing file",
                );
            }
            crate::prompt::Existing::Rename => match crate::prompt::next_free_name(&out_path) {
                Some(fresh) => {
                    p.event(0, &format!("writing to {}", fresh.display()));
                    out_path = fresh;
                }
                None => {
                    return failed(job, 0, "no free filename beside the existing one".into());
                }
            },
            crate::prompt::Existing::Restart
            | crate::prompt::Existing::Resume
            | crate::prompt::Existing::Verify => {
                let _ = std::fs::remove_file(&out_path);
            }
        }
    }
    let outs = out_path.to_string_lossy().to_string();
    let written = AtomicU64::new(0);
    let pace = if job.limit_rate > 0 {
        hya_net::polite::Pace::shared(Arc::new(RateLimiter::new(job.limit_rate)))
    } else {
        hya_net::polite::Pace::unlimited()
    };
    let cancel = job
        .cancel
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let mut too_big = false;
    p.end_phase();
    let t0 = Instant::now();
    let r = {
        let fut = hya_net::fetch_streaming_observed(
            conn.as_ref(),
            target,
            &outs,
            &written,
            Some(cancel.as_ref()),
            &pace,
        );
        tokio::pin!(fut);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        ticker.tick().await;
        let mut last_done = 0u64;
        let mut last_at = Instant::now();
        let mut rate = 0.0f64;
        loop {
            tokio::select! {
                res = &mut fut => break res,
                _ = ticker.tick() => {
                    let done = written.load(Ordering::Relaxed);
                    if job.max_filesize.is_some_and(|cap| done > cap) {
                        too_big = true;
                        cancel.store(true, Ordering::Relaxed);
                    }
                    let dt = last_at.elapsed().as_secs_f64();
                    if dt > 0.0 {
                        let sample = done.saturating_sub(last_done) as f64 / dt;
                        rate = if rate <= 0.0 { sample } else { 0.3 * sample + 0.7 * rate };
                        last_done = done;
                        last_at = Instant::now();
                    }
                    let views = vec![ConnView {
                        idx: 0,
                        host: target.origin_endpoint().0,
                        range: None,
                        rate,
                        health: hya_core::detect::Health::Healthy,
                    }];
                    p.draw(done, &views, Counters { requests: 1, ..Default::default() });
                }
            }
        }
    };
    let el = t0.elapsed().as_secs_f64();
    let n = written.load(Ordering::Relaxed);
    let discard_staging = |path: &Path| {
        if !keeps_file {
            let _ = std::fs::remove_file(path);
        }
    };
    match r {
        Err(_) if too_big => {
            let _ = std::fs::remove_file(&out_path);
            return failed(
                job,
                n,
                format!(
                    "object exceeds --max-filesize {} ({n} bytes had arrived when it was stopped)",
                    job.max_filesize.unwrap_or(0)
                ),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            discard_staging(&out_path);
            return failed(
                job,
                n,
                "interrupted (nothing to resume: the size was unknown)".into(),
            );
        }
        Err(e) => {
            discard_staging(&out_path);
            return failed(job, n, format!("streaming fetch failed: {e}"));
        }
        Ok(0) if !empty_object => {
            discard_staging(&out_path);
            return failed(job, 0, "the server sent no body".into());
        }
        Ok(_) => {}
    }
    p.event(
        0,
        &format!("streamed {} in {:.1}s", hya_core::fmt::bytes(n), el),
    );

    // The whole object is on disk, in order, so everything the ranged path
    // does after its transfer applies here too.
    let digest = (job.print_checksum || job.checksum.is_some())
        .then(|| sha256_file(&out_path))
        .flatten();
    let checksum_ok = job
        .checksum
        .as_deref()
        .and_then(|spec| verify_file_digest(job, &out_path, spec, digest.as_deref()));
    let ok = checksum_ok != Some(false);
    let det = detect_format(
        &head_of(&out_path),
        name,
        probe_info.content_type.as_deref(),
    );
    let mut stdout_error = None;
    if job.to_stdout && ok {
        if let Err(e) = copy_span_to_stdout(&out_path, 0, n) {
            stdout_error = Some(format!("cannot stream to stdout: {e}"));
        }
    }
    let out_path = if keeps_file && ok && job.sort_by_type && det.category != Category::Unknown {
        sort_into_category(out_path, det.category, p)
    } else {
        out_path
    };
    if keeps_file && ok && job.remote_time {
        apply_remote_time(
            &out_path,
            probe_info.last_modified.as_deref(),
            probe_info.validator.as_deref(),
            p,
        );
    }
    if ok {
        save_etag(job, probe_info.validator.as_deref(), p);
    }
    discard_staging(&out_path);
    let ok = ok && stdout_error.is_none();
    p.finish(
        n,
        ok,
        Counters {
            requests: 1,
            ..Default::default()
        },
        digest.as_deref(),
    );
    if let Some(e) = &stdout_error {
        if !job.quiet || job.show_error {
            eprintln!("hydra: {e}");
        }
    }
    if checksum_ok == Some(false) {
        p.note(
            "  checksum MISMATCH: the delivered bytes are not the bytes requested",
            true,
        );
    }
    if !job.quiet {
        if let Some(c) = &det.conflict {
            eprintln!("hydra: warning: {c}");
        }
        if let Some(f) = det.format {
            p.note(&format!("  {}", f.hint()), det.conflict.is_some());
        }
        // An HTML body where a file was expected is the captive-portal / login-wall
        // case, and on an unknown-size response it is the likeliest outcome of all.
        if det.category == Category::Markup && job.output.is_some() {
            eprintln!(
                "hydra: note: this is a web page, not a file. If you meant a release \
                 asset, use the download URL rather than the page URL."
            );
        }
    }
    Outcome {
        url: job.urls[0].clone(),
        output: if keeps_file {
            out_path.to_string_lossy().to_string()
        } else {
            String::new()
        },
        size: n,
        elapsed_s: el,
        transfer_s: el,
        throughput_bps: if el > 0.0 { n as f64 / el } else { 0.0 },
        requests: 1,
        connections: 1,
        peak_connections: 1,
        peak_busy_connections: 1,
        connection_seconds: el,
        sha256: digest,
        checksum_ok,
        ok,
        note: Some(
            stdout_error.unwrap_or_else(|| "streamed (size was not knowable in advance)".into()),
        ),
        format: det.format.map(|f| f.name.to_string()),
        category: Some(det.category.as_str().to_string()),
        format_conflict: det.conflict,
        format_label: det.format.map(|f| f.label().to_string()),
        format_description: det.format.map(|f| f.description().to_string()),
        category_description: Some(det.category.description().to_string()),
        ..Outcome::default()
    }
}

/// Everything [`prepare`] establishes before the output file is considered.
struct Prepared {
    conn: Arc<TlsCapableConnector>,
    /// Every source, with post-redirect targets adopted.
    pairs: Vec<(Url, Target)>,
    /// Indices into `pairs` that probed consistently.
    keep: Vec<usize>,
    /// `pairs` filtered by `keep`.
    usable: Vec<(Url, Target)>,
    probe_info: hya_net::Probe,
    name: String,
    out_path: PathBuf,
    size: u64,
    validator: Option<String>,
    /// Kept apart from `validator`: `--remote-time` needs the date, and the
    /// validator is an opaque ETag whenever the server sent one.
    last_modified: Option<String>,
    served_type: Option<String>,
    first_rtt: f64,
    late_mirrors: Option<LateMirrors>,
    t_start: Instant,
    p: Progress,
}

/// What [`decide_output`] settled about the file the transfer writes into.
struct Placement {
    discarding: bool,
    resumed_from: u64,
    prior: Option<Sidecar>,
    /// Bytes verified as a genuine prefix of the object, for a resumed file
    /// that has no sidecar record.
    adopted_prefix: Option<u64>,
}

/// The connection plan, as the report needs it after the transfer.
#[derive(Clone, Copy)]
struct Plan {
    delta: f64,
    want_digest_value: bool,
    n_conns: usize,
    want_lo: u64,
    want_hi: u64,
    partial: bool,
}

/// The seated sources and the bench, consumed by the transfer.
struct Seats {
    tgts: Vec<Target>,
    per: Vec<usize>,
    sources: Vec<Source>,
    bench: hya_net::Bench,
    hosts: Vec<String>,
    discard_sink: Option<Arc<SparseSink>>,
}

/// What the transfer left behind, sampled from the scheduler as it ran.
struct Transferred {
    ok: bool,
    interrupted: bool,
    transfer_error: Option<String>,
    requests: u64,
    held_now: Vec<(u64, u64)>,
    bytes_held: u64,
    used_conns: usize,
    settled_conns: usize,
    peak_busy: usize,
    conn_secs: u64,
    stream_result: Option<(Option<String>, Vec<u8>, Option<String>)>,
    file_stream_sha256: Option<String>,
    transfer_elapsed: f64,
    elapsed: f64,
}

/// What an existing output offers a run about to write it.
fn resume_offer(
    existing: Option<&Sidecar>,
    ranges: bool,
    on_disk: u64,
    size: u64,
    validator: Option<&str>,
) -> crate::prompt::ResumeOffer {
    use crate::prompt::ResumeOffer;
    match existing {
        Some(sc) => match sc.can_resume(size, validator) {
            Ok(()) => ResumeOffer::Sound(sc.bytes_done()),
            Err(why) => ResumeOffer::Refused(why),
        },
        // No sidecar is the ordinary case for a file another tool started, or
        // one hydra was killed during before writing its record, and it is NOT
        // a reason to re-fetch from zero: the bytes can be checked against the
        // server.
        None if !ranges => ResumeOffer::Refused(
            "the server does not support byte ranges, so a partial file cannot be \
             continued from"
                .into(),
        ),
        None if on_disk >= size => ResumeOffer::LooksComplete(on_disk),
        None if on_disk > 0 => ResumeOffer::Verifiable(on_disk),
        None => ResumeOffer::Refused("the existing file is empty".into()),
    }
}

pub async fn run(job: Job) -> Outcome {
    if job.plugin_options.is_some() && job.urls.len() == 1 {
        match crate::plugins::resolve_job(&job).await {
            Ok(Some((id, plan))) => {
                let preferences = job
                    .plugin_options
                    .as_ref()
                    .map(|o| o.preferences.clone())
                    .unwrap_or_default();
                if !plan.entries.is_empty() {
                    return match crate::plan::run_playlist(plan, job.clone(), preferences, &id)
                        .await
                    {
                        Ok(out) => out,
                        Err(error) => failed(&job, 0, format!("plugin {id}: {error}")),
                    };
                }
                return match crate::plan::run(plan, job.clone(), preferences, &id).await {
                    Ok(out) => out,
                    Err(error) => failed(&job, 0, format!("plugin {id}: {error}")),
                };
            }
            Ok(None) => {}
            Err(error) => return failed(&job, 0, error),
        }
    }
    let mut outcome = run_file(job.clone()).await;
    if outcome.ok && job.plugin_options.is_some() && !job.to_stdout {
        if let Err(error) =
            crate::plugins::finish_job(outcome.url.clone(), outcome.output.clone().into()).await
        {
            return failed(&job, outcome.size, error);
        }
        outcome.size = std::fs::metadata(&outcome.output).map_or(outcome.size, |m| m.len());
    }
    outcome
}

pub async fn run_file(job: Job) -> Outcome {
    match phases(&job).await {
        Ok(o) => o,
        Err(early) => *early,
    }
}

/// The phases in order. `Err` is an outcome settled without a transfer — a
/// refusal, a skip, a spider report, a delegated run — not necessarily a
/// failure.
async fn phases(job: &Job) -> Result<Outcome, Box<Outcome>> {
    let mut s = prepare(job).await?;
    let mut placement = decide_output(job, &mut s).await?;
    let (plan, seats) = plan(job, &mut s, &placement)?;
    let moved = transfer(job, &mut s, &mut placement, &plan, seats).await?;
    Ok(finish(job, s, placement, plan, moved).await)
}

/// Setup, probe, naming and size: everything settled before the output file
/// is looked at.
async fn prepare(job: &Job) -> Result<Prepared, Box<Outcome>> {
    let now = hya_net::cookies::now_secs();
    // Resolved before any target is built, because the `Cookie:` header is part
    // of the target. The host it is scoped to is the FIRST url — the one the
    // user named. Mirrors discovered from a Metalink document are other
    // operators' hosts and are served by the same jar only if a cookie actually
    // domain-matches them, which is the jar's own rule and not a special case
    // here.
    let (jar, cookie_notes) = match open_jar(job, now).await {
        Ok(v) => v,
        Err(e) => return Err(Box::new(failed(job, 0, e))),
    };
    let policy = job.proxy_policy();
    let pairs = match targets_for(&job.urls, &job.headers, &job.user_agent, &policy) {
        Ok(v) => v,
        Err(e) => return Err(Box::new(failed(job, 0, e))),
    };
    let name = job
        .output
        .clone()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| pairs[0].0.suggested_filename());
    let mut out_path = job.output.clone().unwrap_or_else(|| PathBuf::from(&name));
    if let Some(dir) = &job.output_dir {
        if out_path.is_relative() {
            out_path = dir.join(&out_path);
        }
    }
    // `-P DIR` is created like wget creates it; `--create-dirs` extends that
    // to whatever parents an `-O` path names.
    if let Some(dir) = job
        .output_dir
        .as_deref()
        .filter(|_| !job.no_save && !job.to_stdout)
    {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Err(Box::new(failed(
                job,
                0,
                format!("cannot create {}: {e}", dir.display()),
            )));
        }
    }
    if job.create_dirs {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
    }
    // --no-clobber must short-circuit BEFORE any request: its purpose is to
    // avoid touching the network at all for a file already present.
    if job.no_clobber && out_path.exists() && !job.resume {
        if !job.quiet {
            eprintln!(
                "hydra: {} already exists; not retrieved (--no-clobber)",
                out_path.display()
            );
        }
        return Err(Box::new(Outcome::stopped(
            job,
            out_path.to_string_lossy().to_string(),
            std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0),
            true,
            "skipped: file exists",
        )));
    }

    // Reserved on BOTH progress instances (see `progress_for`): this first one
    // covers the setup phase (probe, redirects, concurrency measurement), and
    // its setup line was what prepended 71 bytes to a piped archive.
    let mut p = match progress_for(job, &name, None) {
        Ok(p) => p,
        Err(e) => return Err(Box::new(failed(job, 0, e))),
    };
    let t_start = Instant::now();

    // What reading the mirror list revealed. Level 1 because it is real
    // diagnostic detail about a source list the user did not type — which
    // mirrors were dropped and why, whether per-chunk verification is available,
    // whether the publisher capped the connection count — but not the answer to
    // "did it work", which is the bar and the result line.
    for n in &job.metalink_notes {
        p.event(1, n);
    }
    // Level 0: reading a browser's cookie store is something the user is TOLD
    // about, not something they have to raise the verbosity to discover. A
    // download manager that opens a keychain quietly is indistinguishable from
    // malware, and the difference is entirely whether it said so.
    for n in &cookie_notes {
        p.event(0, n);
    }

    // One connector for the whole job: it carries the TLS session cache, so the
    // second and later connections to a host skip a full handshake. That directly
    // lowers the per-request setup cost the scheduler is measuring.
    // Resolve the proxy spec once. A SOCKS proxy is configured on the CONNECTOR (it
    // carries the TCP stream); an HTTP proxy is configured on each TARGET (it rewrites
    // the request). Conflating them sends a CONNECT to the origin, or an absolute-form
    // request to a SOCKS port.
    let socks = match policy.for_url(&pairs[0].0) {
        Ok(px) => px.filter(|px| px.kind.is_socks()),
        Err(e) => return Err(Box::new(failed(job, 0, e))),
    };
    let conn = match TlsCapableConnector::with_insecure(job.insecure) {
        // A SOCKS proxy belongs on the connector: it carries the TCP stream and never
        // parses HTTP, so every connection this client opens must go through it.
        Ok(c) => Arc::new(
            match socks.clone() {
                Some(px) => {
                    p.event(
                        1,
                        &format!(
                            "routing through {} proxy {}:{}",
                            px.kind.as_str(),
                            px.host,
                            px.port
                        ),
                    );
                    c.with_socks(px)
                }
                None => c,
            }
            .with_family(job.ip_family),
        ),
        Err(e) => return Err(Box::new(failed(job, 0, format!("tls setup failed: {e}")))),
    };

    // FTP takes a separate path: SIZE+MDTM is no validator, so two mirrors
    // cannot be proven to serve identical bytes and multi-source assembly is
    // refused; and preemption costs two control round trips (ABOR, PASV)
    // against zero for HTTP, since REST names a start and nothing names an
    // end. So an FTP fetch is single-source and sequential.
    if pairs.first().map(|(u, _)| u.is_ftp()).unwrap_or(false) {
        if pairs.len() > 1 {
            p.event(
                0,
                "ftp: using the first source only — FTP offers no validator that can prove \
                 two servers hold identical bytes",
            );
        }
        let outs = out_path.to_string_lossy().to_string();
        return Err(Box::new(
            ftp_fetch(
                job,
                &pairs[0].0,
                &mut p,
                outs,
                Arc::new(hya_net::TcpConnector),
            )
            .await,
        ));
    }

    p.phase("resolving and probing sources");
    // How many mirrors the probe phase must produce before the transfer may
    // start. Every mirror can take at least one connection, so the connection
    // budget is the seat count — bounded by the list itself and by politeness.
    // Anything past this only fills the reserve bench, which is why it does not
    // have to be waited for.
    let want_seats = match job.conns {
        Some(n) => n.clamp(1, job.polite.total.max(1)),
        None => mirror_list_width(job, pairs.len()),
    }
    .min(pairs.len());
    let Probed {
        first: probe_info,
        keep,
        resolved,
        renamed,
        late: late_mirrors,
        first_rtt,
        jar: final_jar,
    } = match probe_all(
        &conn,
        &pairs,
        &mut p,
        job,
        job.attested.as_ref().map(|a| a.size),
        want_seats,
        &jar,
        now,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return Err(Box::new(failed(job, 0, e))),
    };

    // Written HERE rather than after the transfer, and that is the complete
    // moment rather than an early one: cookies are handed out by the redirect
    // chain, which has just finished, and the range requests that follow reuse
    // the resolved target without reading `Set-Cookie` again. Saving here also
    // means a transfer that fails half way still leaves the session that was
    // issued to it, which is the one the retry will want.
    if let Some(spec) = &job.cookies {
        match spec.save(&final_jar, now) {
            Ok(Some(note)) => p.event(0, &note),
            Ok(None) => {}
            // A jar the user asked to keep and silently did not get is worse
            // than a failed download: they find out at the next login.
            Err(e) => return Err(Box::new(failed(job, 0, e))),
        }
    }

    // The URL is a mirror list, not the object. `.../metalink?repo=fedora-40`
    // has no extension to read; the probe already carries the `Content-Type`,
    // so asking here is free. Saving it instead would hand the user 6 KB of
    // XML under the name of the image they asked for.
    if job.follow_metalink && job.attested.is_none() && probe_info.serves_metalink() {
        p.event(
            0,
            &format!(
                "{} serves a Metalink document; reading it as a mirror list \
                 (--no-follow-metalink to save it instead)",
                pairs[keep[0]].0.host
            ),
        );
        p.end_phase();
        return Err(Box::new(
            match follow_metalink(job, &pairs[keep[0]].0).await {
                Ok(next) => {
                    // Say WHAT the document turned out to describe, at level 0.
                    //
                    // The user typed one URL and is about to receive a file with a
                    // different name and possibly a very different size — a
                    // repository-metadata redirector and an image redirector look
                    // identical on the command line, and only the document knows
                    // which one this was. Reporting the mirror list without
                    // reporting its contents leaves "why did I get 6 KiB?" as the
                    // first thing the user has to work out for themselves.
                    p.event(
                        0,
                        &format!(
                            "the document describes {} ({}) on {} mirror(s)",
                            next.output
                                .as_deref()
                                .map(|o| o.to_string_lossy().to_string())
                                .unwrap_or_else(|| "one file".into()),
                            next.attested
                                .as_ref()
                                .map(|a| hya_core::fmt::bytes(a.size))
                                .unwrap_or_else(|| "size not stated".into()),
                            next.urls.len(),
                        ),
                    );
                    Box::pin(run(next)).await
                }
                Err(e) => failed(job, 0, e),
            },
        ));
    }
    // The requested URL was a redirector PAGE, so the name taken from it names
    // the stub, not the file — `href.li/?…` yields `index.html`. Adopt the name
    // the object actually landed under. Only when the user named nothing: `-O`
    // and `--output-dir`-relative paths are explicit and are never second-guessed.
    let disposed = job
        .content_disposition
        .then(|| probe_info.suggested_filename())
        .flatten();
    let (name, mut out_path) = match renamed.or(disposed).filter(|_| job.output.is_none()) {
        Some(fresh) => {
            p.event(1, &format!("naming the output {fresh}"));
            let path = match &job.output_dir {
                Some(dir) => dir.join(&fresh),
                None => PathBuf::from(&fresh),
            };
            (fresh, path)
        }
        None => (name, out_path),
    };
    if job.to_stdout {
        out_path = stdout_stage_path();
    }
    // Adopt the post-redirect targets: a release asset commonly redirects to a
    // different host, and fetching from the pre-redirect URL would 302 on every range.
    let mut pairs = pairs;
    for (i, t) in resolved {
        if let Some(slot) = pairs.get_mut(i) {
            slot.1 = t;
        }
    }
    // A HEAD is allowed to omit Content-Length, and CDNs do. Falling back to a
    // one-byte range request is what turns "0 bytes, success" into a real transfer.
    let mut size = probe_info.size;
    let validator = probe_info.validator.clone();
    // Kept separately from `validator`: `--remote-time` needs the DATE, and the
    // validator is whichever of the two headers is better for resume — an ETag
    // when the server sent one, which is opaque and carries no time.
    let last_modified = probe_info.last_modified.clone();
    // The server's own Content-Type, used as the weakest of the three
    // classification signals.
    let served_type = job
        .content_type
        .clone()
        .or_else(|| probe_info.content_type.clone());
    let usable: Vec<(Url, Target)> = keep.iter().map(|&i| pairs[i].clone()).collect();
    // `Content-Length: 0` is a SIZE, not a missing size.
    //
    // The two are indistinguishable in `size`, and treating the second as the first
    // fails a download that had already succeeded: a zero-length object (
    // `speedtest.bitel.io/Testdateien/0B`) went through the size fallback, then the
    // unknown-size streaming path, and came out as "the server sent no body" —
    // exit code 1 over a correctly written empty file.
    let empty_object = probe_info.stated_length() == Some(0);
    if size == 0 && !job.spider && !empty_object && !job.force_stream {
        // `keep` holds the indices that probed consistently; any of them can answer.
        match hya_net::probe_size_via_range(conn.as_ref(), &pairs[keep[0]].1).await {
            Ok(n) if n > 0 => {
                p.event(1, &format!("size from Content-Range: {n} bytes"));
                size = n;
            }
            Ok(_) => {}
            Err(e) => p.event(1, &format!("size fallback failed: {e}")),
        }
    }
    if (size == 0 || job.force_stream) && !job.spider {
        if job.server_response {
            p.end_phase();
            print_exchange(&probe_info);
        }
        return Err(Box::new(
            stream_unknown_size(
                job,
                &conn,
                &usable[0].1,
                &probe_info,
                &name,
                out_path,
                empty_object,
                &mut p,
            )
            .await,
        ));
    }

    // --spider / -I: report and stop. No body is requested, so this is the safe
    // way to inspect a URL (and what a link checker wants).
    if job.spider {
        // `-S`/`-i` must be honoured here too. The only other place headers are
        // printed is after the transfer, which `--spider` returns before ever
        // reaching — so `hydra --spider -S <url>` silently showed no headers,
        // despite being the most natural way to ask for exactly them.
        if job.server_response && !job.quiet {
            p.end_phase();
            print_exchange(&probe_info);
        }
        if !job.quiet {
            println!(
                "{}  {} bytes  ranges={}  validator={}",
                name,
                size,
                match (probe_info.ranges, usable.len() > 1) {
                    (true, true) => "yes (multi-source usable)",
                    (true, false) => "yes",
                    (false, _) => "no",
                },
                validator.as_deref().unwrap_or("none")
            );
        }
        return Err(Box::new(Outcome::stopped(
            job,
            String::new(),
            size,
            true,
            "spider: headers only",
        )));
    }

    // --max-filesize: refuse before opening a socket for the body, which is the
    // only point at which refusing actually saves anything.
    if let Some(cap) = job.max_filesize {
        if size > cap {
            return Err(Box::new(failed(
                job,
                size,
                format!("object is {size} bytes, exceeding --max-filesize {cap}"),
            )));
        }
    }

    // --etag-compare: if the stored validator still matches, the object has not
    // changed and there is nothing to retrieve.
    if let Some(path) = &job.etag_compare {
        if let (Ok(stored), Some(current)) = (std::fs::read_to_string(path), validator.as_deref()) {
            if stored.trim() == current.trim() {
                if !job.quiet {
                    eprintln!(
                        "hydra: unchanged (ETag matches {}), not retrieved",
                        path.display()
                    );
                }
                return Err(Box::new(Outcome::stopped(
                    job,
                    out_path.to_string_lossy().to_string(),
                    size,
                    true,
                    "unchanged: ETag matches",
                )));
            }
        }
    }
    // A second Progress now that the size is known. The phase line from the probe
    // stage is cleared first so the two never share a terminal row.
    p.end_phase();
    let p = match progress_for(job, &name, Some(size)) {
        Ok(p) => p,
        Err(e) => return Err(Box::new(failed(job, size, e))),
    };
    Ok(Prepared {
        conn,
        pairs,
        keep,
        usable,
        probe_info,
        name,
        out_path,
        size,
        validator,
        last_modified,
        served_type,
        first_rtt,
        late_mirrors,
        t_start,
        p,
    })
}

/// The existing-file question: skip, rename, restart, verify or resume, and
/// whether the bytes are written at all.
async fn decide_output(job: &Job, s: &mut Prepared) -> Result<Placement, Box<Outcome>> {
    let size = s.size;
    let validator = s.validator.clone();
    let mut out_path = std::mem::take(&mut s.out_path);
    let p = &mut s.p;
    // Four outcomes are possible and none is a safe default for every case, so an
    // interactive run asks. The flags are answers and are never re-asked; a
    // non-interactive run picks the option that cannot destroy data.
    let mut resumed_from = 0u64;
    let mut prior: Option<Sidecar> = None;
    let existing_sidecar = Sidecar::load(&out_path);
    // Bytes verified as a genuine prefix of the remote object, when resuming a file
    // that has no sidecar record.
    let mut adopted_prefix: Option<u64> = None;
    // Bytes ACTUALLY present, not the file's apparent length.
    //
    // The output is a sparse file created at full length before the first byte
    // arrives. Allocated blocks and resume records determine actual progress.
    let on_disk = bytes_present(&out_path, existing_sidecar.as_ref());

    // Where the bytes are going, decided BEFORE anything touches the filesystem.
    //
    // Under `--no-save` this is `Discard`, and the whole existing-file question
    // below becomes moot: a run that will never write cannot clobber, cannot
    // resume, and must not prompt about — or rename around — a file it is not
    // going to open. The previous implementation created the file, wrote it,
    // hashed it and deleted it at the end, so all of that machinery ran and the
    // bytes sat on disk for the duration.
    let destination = output_target(job, &out_path.to_string_lossy());
    let discarding = destination == OutputTarget::Discard;

    if out_path.exists() && !job.to_stdout && !discarding {
        let offer = resume_offer(
            existing_sidecar.as_ref(),
            s.probe_info.ranges,
            on_disk,
            size,
            validator.as_deref(),
        );
        let flags = crate::prompt::Flags {
            resume: job.resume,
            no_clobber: job.no_clobber,
            force: job.force,
            assume_default: job.quiet,
        };
        p.end_phase();
        let choice = crate::prompt::ask(&out_path, on_disk, size, &offer, flags)
            .unwrap_or(crate::prompt::Existing::Rename);
        match choice {
            crate::prompt::Existing::Skip => {
                return Err(Box::new(Outcome::stopped(
                    job,
                    out_path.to_string_lossy().to_string(),
                    on_disk,
                    true,
                    "kept the existing file",
                )));
            }
            crate::prompt::Existing::Rename => match crate::prompt::next_free_name(&out_path) {
                Some(fresh) => {
                    p.event(0, &format!("writing to {}", fresh.display()));
                    out_path = fresh;
                }
                None => {
                    return Err(Box::new(failed(
                        job,
                        size,
                        "no free filename beside the existing one".into(),
                    )))
                }
            },
            crate::prompt::Existing::Restart => {
                Sidecar::remove(&out_path);
                let _ = std::fs::remove_file(&out_path);
            }
            crate::prompt::Existing::Verify => {
                p.phase("verifying the existing file against the server");
                let full = verify_prefix(
                    &s.conn,
                    &s.usable[0].1,
                    &out_path,
                    on_disk.min(size),
                    job.tries,
                    job.timeout_s,
                )
                .await;
                p.end_phase();
                return Err(Box::new(match full {
                    Some(_) if on_disk == size => Outcome::stopped(
                        job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        true,
                        "already complete: the file matches the server",
                    ),
                    Some(_) => Outcome::stopped(
                        job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        false,
                        "the file is a valid prefix but is shorter than the object; \
                         re-run with -c to continue it",
                    ),
                    None => Outcome::stopped(
                        job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        false,
                        "the existing file does NOT match the server",
                    ),
                }));
            }
            crate::prompt::Existing::Resume if existing_sidecar.is_none() => {
                // Resuming a file we did not write: prove the prefix first. Without
                // this the transfer would append to bytes of unknown provenance, which
                // is precisely the silent-corruption class this project keeps finding.
                p.phase("checking the existing bytes against the server");
                let v = verify_prefix(
                    &s.conn,
                    &s.usable[0].1,
                    &out_path,
                    on_disk,
                    job.tries,
                    job.timeout_s,
                )
                .await;
                p.end_phase();
                match v {
                    Some(n) => {
                        p.event(
                            0,
                            &format!(
                                "verified {} already on disk; continuing from there",
                                hya_core::fmt::bytes(n)
                            ),
                        );
                        adopted_prefix = Some(n);
                    }
                    None => {
                        p.event(
                            0,
                            "the existing bytes do not match the server; starting over",
                        );
                        let _ = std::fs::remove_file(&out_path);
                    }
                }
            }
            crate::prompt::Existing::Resume => {
                // Fall through: the resume block below picks up the sidecar.
            }
        }
    }

    if !s.probe_info.ranges && (job.resume || existing_sidecar.is_some()) {
        p.event(
            0,
            "the server does not support byte ranges, so nothing can be continued; starting over",
        );
        Sidecar::remove(&out_path);
    } else if job.resume || existing_sidecar.is_some() {
        if let Some(sc) = Sidecar::load(&out_path) {
            match sc.can_resume(size, validator.as_deref()) {
                Ok(()) => {
                    resumed_from = sc.bytes_done();
                    p.event(
                        0,
                        &format!(
                            "resuming: {} already held",
                            hya_core::fmt::bytes(resumed_from)
                        ),
                    );
                    prior = Some(sc);
                }
                Err(why) => {
                    p.event(0, &format!("cannot resume ({why}); starting over"));
                    Sidecar::remove(&out_path);
                }
            }
        }
    }
    s.out_path = out_path;
    Ok(Placement {
        discarding,
        resumed_from,
        prior,
        adopted_prefix,
    })
}

/// Connection count, source seating and the byte span to schedule.
fn plan(job: &Job, s: &mut Prepared, placement: &Placement) -> Result<(Plan, Seats), Box<Outcome>> {
    let discarding = placement.discarding;
    let size = s.size;
    let first_rtt = s.first_rtt;
    let usable = &s.usable;
    let keep = &s.keep;
    let p = &mut s.p;
    // A digest is wanted unless the user declined it — and `--checksum` or a
    // document's digest is a request for one however they answered, since a
    // verification that cannot run is worse than one that costs a pass.
    let want_digest_value = job.print_checksum
        || job.checksum.is_some()
        || job.attested.as_ref().is_some_and(|a| a.digest.is_some());
    let discard_sink = discarding.then(|| {
        let sk = SparseSink::discarding();
        Arc::new(if want_digest_value {
            sk.with_digest(hya_net::stream_digest::DEFAULT_REORDER_CAP)
        } else {
            sk
        })
    });

    // An explicit `-x N` is an instruction, not a hint: a measurement that
    // quietly overrode it made `-x 5` open one connection.
    let (n_conns, delta) = match job.conns {
        // `--adaptive` keeps N as a ceiling: the in-band ramp (`hya_core::ramp`)
        // starts at one connection and admits more while they pay, which a
        // pre-transfer probe could not do without re-fetching its samples.
        Some(n) => (job.polite.allow(n), first_rtt),
        // `--no-probe` with no `-x` at all: the user has asked not to measure and
        // named no number, so take one connection rather than probing anyway.
        // Guessing a multi-connection default here is what produced transfers
        // slower than a single stream on a saturated link.
        None if !job.probe => (job.polite.allow(1), first_rtt),
        // A mirror list answers the question the probe was going to ask: one
        // host's marginal goodput does not describe a transfer across N servers
        // (on a twelve-mirror document it answered "2", and the transfer took
        // 10.2 s against 5.2 s for the eight the list could seat). The
        // scheduler still reassigns ranges on observed rate.
        None if !job.source_plans.is_empty() && usable.len() > 1 => {
            let n = mirror_list_width(job, usable.len());
            p.event(
                1,
                &format!(
                    "{n} source(s) from the mirror list; measuring one host's marginal goodput \
                     would not describe them"
                ),
            );
            (n, first_rtt)
        }
        // No `-x` and no mirror list: the politeness budget. A pre-transfer
        // measurement read a 100 ms path's latency as its bandwidth and settled
        // on one connection where eight were worth 3x; `429`s, starvation and
        // range stealing handle the paths that will not serve the budget.
        None => (job.polite.allow(job.polite.per_host), first_rtt),
    };
    // A server that ignores `Range` can answer exactly one request, from offset
    // zero. Opening more would have every other connection fail with a `200`.
    let n_conns = if !s.probe_info.ranges {
        if n_conns > 1 || job.conns.is_some_and(|n| n > 1) {
            p.event(
                0,
                "the server does not support byte ranges: one connection, one request",
            );
        }
        1
    } else if job.conns.is_some() && usable.len() > 1 {
        n_conns
    } else {
        let n = connections_for_size(n_conns, size);
        if n < n_conns {
            p.event(
                1,
                &format!(
                    "{n} connection(s) for a {} object; more would cost round trips, not time",
                    hya_core::fmt::bytes(size)
                ),
            );
        }
        n
    };
    // Split under both ceilings: the earlier arithmetic never read
    // `Politeness.total`, so `--max-total-connections 2 -x 8` opened eight.
    // Plans are carried across by index: `keep` is a subset of the URL list,
    // and a dropped mirror would otherwise shift every rank after it.
    let plans: Vec<hya_core::SourcePlan> = if job.source_plans.is_empty() {
        vec![hya_core::SourcePlan::default(); usable.len()]
    } else {
        keep.iter()
            .map(|&i| job.source_plans.get(i).copied().unwrap_or_default())
            .collect()
    };
    let split = if job.source_plans.is_empty() {
        job.polite.split(n_conns, usable.len())
    } else {
        job.polite.split_plan(n_conns, &plans)
    };
    // Seated sources are the ones the split gave connections to; the rest are
    // the bench. With an even split the seated set is a prefix, but a ranked
    // allocation can leave a gap — a mirror that stated `maxconnections` may be
    // passed over while a lower-ranked one is seated — so this filters rather
    // than truncating.
    let seated: Vec<usize> = (0..usable.len()).filter(|&i| split[i] > 0).collect();
    let tgts: Vec<Target> = seated.iter().map(|&i| usable[i].1.clone()).collect();
    let per: Vec<usize> = seated.iter().map(|&i| split[i]).collect();
    // Everything the split did not seat, best-ranked first. This is what makes a
    // nineteen-mirror list worth more than a four-mirror one at four
    // connections: `run_transfer_with_reserves` substitutes from here in place
    // when a source dies, so the socket count stays what politeness authorised.
    let bench_ready: Vec<hya_net::Reserve> = {
        let mut idx: Vec<usize> = (0..usable.len()).filter(|&i| split[i] == 0).collect();
        idx.sort_by_key(|&i| (plans[i].priority, i));
        idx.into_iter()
            .map(|i| hya_net::Reserve {
                target: usable[i].1.clone(),
                plan: plans[i],
                host: source_label(&usable[i].0),
            })
            .collect()
    };
    // Mirrors still being probed when the transfer was allowed to start. They
    // arrive as `(index into pairs, target)`; the ranking and the display name
    // live here, so a small bridge turns each into a `Reserve` as it lands.
    // Nothing is awaited — the transfer already has its seats.
    let bench_late = s.late_mirrors.take().map(|mut rx| {
        let plans_all = job.source_plans.clone();
        let labels: Vec<(String, hya_core::SourcePlan)> = s
            .pairs
            .iter()
            .enumerate()
            .map(|(i, (u, _))| {
                (
                    source_label(u),
                    plans_all.get(i).copied().unwrap_or_default(),
                )
            })
            .collect();
        let (tx, out) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some((i, target)) = rx.recv().await {
                let Some((host, plan)) = labels.get(i).cloned() else {
                    continue;
                };
                if tx.send(hya_net::Reserve { target, plan, host }).is_err() {
                    return;
                }
            }
        });
        out
    });
    let seated_plans: Vec<hya_core::SourcePlan> = seated.iter().map(|&i| plans[i]).collect();
    // What the transfer will actually open, which is what `--json.connections`
    // must report: a number the run did not use is not a measurement.
    let n_conns: usize = per.iter().sum();
    // Level 1, not 0: at default verbosity the useful signal is the progress bar
    // and the one-line result. How many sources were chosen and what the measured
    // setup cost was are diagnostics — real ones, but the answer to "did it work"
    // should not be preceded by two lines of internals.
    p.event(
        1,
        &format!(
            "{} source(s), {} connection(s) total, delta ~{:.3}s",
            tgts.len(),
            n_conns,
            delta
        ),
    );

    // Where the size and digest being trusted came from. A verification result
    // is only as meaningful as its source, and a user reading "checksum OK"
    // should be able to find out which document said so.
    if let Some(a) = &job.attested {
        p.event(
            1,
            &format!(
                "attested by {}: {} bytes, {}",
                a.origin,
                a.size,
                a.digest.as_deref().unwrap_or("no digest published")
            ),
        );
    }

    let sources: Vec<Source> = seated_plans
        .iter()
        .map(|plan| Source {
            gamma_est: 1.0e6,
            delta_est: delta.max(1e-3),
            // The publisher's ranking, used once — for the first split, before
            // anything has been measured. See `hya_core::sched::Source::priority`
            // for why it is deliberately not consulted again.
            priority: plan.priority,
            ..Default::default()
        })
        .collect();
    // Which mirror is which. Without this a multi-source run shows four
    // connection rows and no way to tell which host a rank was assigned to, so a
    // "mirror 2 is slow" observation cannot be turned into a URL to check.
    if !job.source_plans.is_empty() {
        for (k, &i) in seated.iter().enumerate() {
            p.event(
                1,
                &format!(
                    "source {k}: rank {} {} — {} connection(s){}",
                    plans[i].priority,
                    source_label(&usable[i].0),
                    split[i],
                    plans[i]
                        .max_connections
                        .map(|n| format!(", mirror states a ceiling of {n}"))
                        .unwrap_or_default(),
                ),
            );
            p.event(2, &format!("  {}", usable[i].0));
        }
    }
    if !bench_ready.is_empty() {
        p.event(
            1,
            &format!(
                "{} reserve mirror(s) held back; a source that fails is replaced rather than \
                 stranding its range",
                bench_ready.len()
            ),
        );
        // The whole bench at -vv: when a substitution happens, the next question
        // is always "to what", and the answer is deterministic and knowable now.
        for (k, r) in bench_ready.iter().enumerate() {
            p.event(
                2,
                &format!("  reserve {k}: rank {} {}", r.plan.priority, r.host),
            );
        }
    }
    let bench = hya_net::Bench {
        ready: bench_ready,
        late: bench_late,
    };
    // --range: schedule only the requested interval. Resolving it here rather
    // than in the argument parser is what lets a suffix range like `-512` mean
    // "the last 512 bytes" — that needs the object size, known only now.
    let (want_lo, want_hi) = match job.range {
        None => (0u64, size),
        Some(spec) => match spec.resolve(size) {
            Some(r) => r,
            None => {
                return Err(Box::new(failed(
                    job,
                    size,
                    format!("{spec:?} is empty for a {size}-byte object"),
                )))
            }
        },
    };
    let partial = (want_lo, want_hi) != (0, size);
    if partial && !s.probe_info.ranges {
        return Err(Box::new(failed(
            job,
            size,
            "the server does not support byte ranges, so --range cannot be honoured".into(),
        )));
    }
    if partial {
        p.event(
            0,
            &format!(
                "range mode: bytes {}-{} of {} ({})",
                want_lo,
                want_hi - 1,
                size,
                hya_core::fmt::bytes(want_hi - want_lo)
            ),
        );
    }
    let hosts: Vec<String> = seated.iter().map(|&i| source_label(&usable[i].0)).collect();
    Ok((
        Plan {
            delta,
            want_digest_value,
            n_conns,
            want_lo,
            want_hi,
            partial,
        },
        Seats {
            tgts,
            per,
            sources,
            bench,
            hosts,
            discard_sink,
        },
    ))
}

/// The second span of a parallel transfer opens ahead of the write frontier, so
/// the stream digest must buffer `(n-1)/n` of the object. When that exceeds the
/// reorder cap the digest is certain to be abandoned, and buffering toward it
/// only wastes copies.
fn digest_cannot_finish(size: u64, spans: usize) -> bool {
    let spans = spans as u64;
    spans > 1
        && size.saturating_mul(spans - 1) / spans
            > hya_net::stream_digest::DEFAULT_REORDER_CAP as u64
}

/// Run the scheduler over the seats and sample what it did.
async fn transfer(
    job: &Job,
    s: &mut Prepared,
    placement: &mut Placement,
    plan: &Plan,
    seats: Seats,
) -> Result<Transferred, Box<Outcome>> {
    let Plan {
        delta,
        want_digest_value,
        n_conns,
        want_lo,
        want_hi,
        partial,
    } = *plan;
    let Seats {
        tgts,
        per,
        sources,
        bench,
        hosts,
        discard_sink,
    } = seats;
    let discarding = placement.discarding;
    let mut resumed_from = placement.resumed_from;
    let prior = placement.prior.as_ref();
    let adopted_prefix = placement.adopted_prefix;
    let size = s.size;
    let conn = &s.conn;
    let out_path = &s.out_path;
    let validator = &s.validator;
    let t_start = s.t_start;
    let p = &mut s.p;
    let t_transfer = Instant::now();
    let mut sched =
        Scheduler::new(size, sources, &per).with_stall_timeout((12.0 * delta).clamp(4.0, 45.0));
    // `--adaptive`: open the budget but start with one connection active and let the
    // in-band ramp earn the rest. Without this the scheduler runs every connection
    // from the first tick, which is the fixed-concurrency behaviour `-x` already
    // provides.
    if job.adaptive && n_conns > 1 {
        sched.set_active_limit(1);
    }
    // Everything outside the requested range is marked held so the scheduler
    // never issues a request for it.
    if partial {
        sched.mark_done(0, want_lo);
        sched.mark_done(want_hi, size);
    }
    // Pre-mark resumed ranges as held so they are never re-fetched.
    if let Some(sc) = &prior {
        for (lo, hi) in &sc.done {
            sched.mark_done(*lo, *hi);
        }
    }
    // A verified prefix from a sidecar-less file: mark it held so it is never
    // re-fetched. This is the whole point of verifying it.
    if let Some(n) = adopted_prefix {
        let n = n.min(want_hi);
        if n > want_lo {
            sched.mark_done(want_lo, n);
            resumed_from = n;
        }
    }

    p.set_baseline(resumed_from);
    // Live byte accounting, sampled from the scheduler: the one component that
    // knows which bytes arrived (`finish` says why every cheaper answer lies).
    // Seeded from its state now, after every `mark_done` above, because a
    // transfer with nothing left to fetch completes before the first tick.
    let progress = Arc::new(std::sync::atomic::AtomicU64::new(sched.bytes_held()));
    let progress_obs = progress.clone();

    // Peak concurrency the transfer actually ran at, sampled from the scheduler.
    // Seeded from its current active limit so a transfer that completes before the
    // first observation tick still reports a real number.
    let used_conns = Arc::new(std::sync::atomic::AtomicUsize::new(
        sched.active_limit().min(sched.n_conns()),
    ));
    let used_conns_obs = used_conns.clone();
    // The limit the transfer FINISHED at, as opposed to the peak it explored.
    let settled_conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let settled_conns_obs = settled_conns.clone();
    // The most connections that ever actually held a range at once, which is not the
    // same as the limit that permitted them.
    let peak_busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak_busy_obs = peak_busy.clone();
    // Busy-connection-seconds, in micro-units so the accumulator can be atomic.
    let conn_secs = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let conn_secs_obs = conn_secs.clone();
    let conn_secs_last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));

    // Requests the scheduler actually issued, sampled live. Needed because a failed
    // transfer returns no count, and reporting zero there printed a request count
    // the run did not measure.
    let observed_requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed_requests_obs = observed_requests.clone();

    // Checkpoint what is already held before the first byte: the periodic
    // checkpoint fires after 2 s, so a ^C before that left no sidecar and the
    // next `-c` restarted from zero. This is the first point where the resume
    // record and `--range` have been folded into one authority on what is held.
    if !discarding {
        let held = sched.held_ranges();
        if !held.is_empty() && held != vec![(0, size)] {
            let rec = Sidecar {
                size,
                validator: validator.clone(),
                done: held,
                url: job.urls[0].clone(),
            };
            let _ = rec.save(out_path);
        }
    }
    let limiter = Arc::new(if job.limit_rate > 0 {
        RateLimiter::new(job.limit_rate)
    } else {
        RateLimiter::unlimited()
    });
    let outs = out_path.to_string_lossy().to_string();
    let c = conn.clone();
    // Host names for the progress view, one per SOURCE — behind a lock because a
    // reserve substitution changes them mid-transfer. A view that keeps naming
    // the dead mirror is worse than one naming none: it attributes the
    // replacement's throughput to a machine that is not serving it.
    let hosts: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(hosts));
    // Digest, head bytes and any unavailability reason gathered from the stream,
    // for the `--no-save` path that has no file to read them back from.
    let mut stream_result: Option<(Option<String>, Vec<u8>, Option<String>)> = None;
    // The SHA-256 of a saved file, hashed as the bytes land: a post-download
    // re-read cost three times the transfer on a loopback origin, and on a
    // network-bound transfer the in-band hash is free. Only a single-connection,
    // from-zero transfer can be hashed this way (see `stream_digest`); anything
    // else, or a hash that could not finish, falls back to hashing the file.
    let mut file_stream_sha256: Option<String> = None;
    // What the scheduler held, as of the last observation. The resume record a
    // failed or interrupted run leaves must describe exactly these ranges: a
    // multi-connection transfer holds several disjoint spans, and recording
    // their total as one prefix made the next `-c` treat holes as bytes.
    let last_held: Arc<std::sync::Mutex<Vec<(u64, u64)>>> =
        Arc::new(std::sync::Mutex::new(sched.held_ranges()));
    let last_held_obs = last_held.clone();
    let res = {
        // Clear the phase line before the first frame: they share a terminal row.
        p.end_phase();
        p.set_baseline(resumed_from);
        // Checkpoint the resume record AS THE TRANSFER RUNS, not only at the end.
        //
        // Writing it only on exit means a ^C or a crash leaves no record, so the next
        // run cannot tell which bytes are held — and because the output is a SPARSE
        // file created at full length from the start, its apparent size is the whole
        // object even when almost nothing has arrived. Together those two facts made an
        // interrupted 2 MB transfer look like a finished 121.7 MiB download. The record
        // carries explicit ranges rather than a byte count, because positioned writes
        // land out of order and a count cannot describe a hole.
        let tick_sink = job.ticks.clone();
        // Substitutions, published by the transport's callback and drained by the
        // renderer on its next tick.
        //
        // The callback cannot report them itself: `Progress` is owned by the
        // render closure for the duration of the transfer, and two closures
        // cannot hold it at once. Draining here rather than after the transfer is
        // what makes the message arrive when the mirror changed — reported at the
        // end it reads as a summary of something that is over, when in fact it is
        // the reason the next ten minutes look different from the last.
        let swaps: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let swaps_r = swaps.clone();
        let mut last_ckpt = Instant::now();
        let mut last_snapshot_bytes = sched.bytes_held();
        let ckpt_path = out_path.clone();
        let ckpt_size = size;
        let ckpt_validator = validator.clone();
        let ckpt_url = job.urls[0].clone();
        // The last concurrency decision reported, so each is printed once.
        let mut last_reason = hya_core::LimitReason::None;
        let mut render = |sc: &Scheduler, done: u64| {
            for line in swaps_r.lock().unwrap().drain(..) {
                p.event(0, &line);
            }
            // The transport's concurrency decisions, as they are made. Without
            // this the only account of the adaptive search was `HYDRA_RAMP_TRACE`,
            // and a run that settled at one connection looked like seven dropped
            // ones.
            let reason = sc.limit_reason();
            if reason != last_reason {
                last_reason = reason;
                use hya_core::LimitReason as R;
                let human_rate = |r: f64| format!("{}/s", hya_core::fmt::bytes(r as u64));
                match reason {
                    R::Measured {
                        chosen,
                        chosen_rate,
                        tried,
                        tried_rate,
                        // A rate of zero was never measured; see the GUI's
                        // `describe_limit` for the same guard.
                    } if tried != chosen && tried_rate > 0.0 && chosen_rate > 0.0 => p.event(
                        1,
                        &format!(
                            "adaptive: measured {tried} connections at {} against {chosen} \
                             at {}; using {chosen}",
                            human_rate(tried_rate),
                            human_rate(chosen_rate)
                        ),
                    ),
                    R::Measured { chosen, .. } if chosen < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "adaptive: {chosen} of {} connections pay for themselves",
                            sc.n_conns()
                        ),
                    ),
                    R::Refused { serving } if serving < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "origin refuses more than {serving} connection(s) at once; \
                             using {serving}"
                        ),
                    ),
                    R::Starved { serving } if serving < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "origin serves {serving} connection(s) at once and starves the \
                             rest; using {serving}"
                        ),
                    ),
                    _ => {}
                }
            }
            // Carry the scheduler's own count out to the completeness check. A
            // monotonic max rather than a plain store: the observer is called on
            // every tick and the last tick before completion is not necessarily
            // the highest, so a bare store can report less than actually arrived.
            progress_obs.fetch_max(sc.bytes_held(), std::sync::atomic::Ordering::Relaxed);
            observed_requests_obs
                .fetch_max(sc.stats.requests, std::sync::atomic::Ordering::Relaxed);
            // Four distinct concurrency figures; `Outcome` documents each.
            let limit_now = sc.active_limit().min(sc.n_conns());
            used_conns_obs.fetch_max(limit_now, std::sync::atomic::Ordering::Relaxed);
            settled_conns_obs.store(limit_now, std::sync::atomic::Ordering::Relaxed);
            let busy_now = sc.busy_conns();
            peak_busy_obs.fetch_max(busy_now, std::sync::atomic::Ordering::Relaxed);
            // Rectangle rule over the observer's own interval. The observer runs on the
            // transfer's tick, so the interval is the tick period; deriving it from the
            // clock rather than assuming a constant keeps the integral honest if the
            // tick is ever retimed or a tick is late.
            {
                let mut last = conn_secs_last.lock().unwrap();
                let now = std::time::Instant::now();
                let dt = now.duration_since(*last).as_secs_f64();
                *last = now;
                // Stored as micro-connection-seconds in an integer so the accumulator
                // needs no lock of its own and cannot drift on float addition order.
                conn_secs_obs.fetch_add(
                    (busy_now as f64 * dt * 1e6) as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            // No resume record under `--no-save`: there is no file to resume INTO,
            // so a sidecar would be a stray file created by the flag that promises
            // to create none. (Caught end-to-end, not by a unit test: the periodic
            // checkpoint runs inside this closure and is separate from the
            // completion path.)
            // Snapshotted only when the held count moved: `held_ranges`
            // allocates, and a tick that landed nothing has nothing new to say.
            let held_bytes = sc.bytes_held();
            if !discarding && held_bytes != last_snapshot_bytes {
                last_snapshot_bytes = held_bytes;
                let held = sc.held_ranges();
                if last_ckpt.elapsed().as_secs_f64() >= 2.0 && !held.is_empty() {
                    last_ckpt = Instant::now();
                    let rec = Sidecar {
                        size: ckpt_size,
                        validator: ckpt_validator.clone(),
                        done: held.clone(),
                        url: ckpt_url.clone(),
                    };
                    let _ = rec.save(&ckpt_path);
                }
                *last_held_obs.lock().unwrap() = held;
            }
            let views = {
                let names = hosts.lock().unwrap();
                conn_views(sc, &names)
            };
            // Publish live state for any UI driving this job. Done here rather than in
            // the transport because this closure already has the scheduler, so there is
            // no extra plumbing and no extra tick rate to reconcile.
            if let Some((id, tx)) = &tick_sink {
                let _ = tx.send(Tick {
                    id: *id,
                    done,
                    size: Some(size),
                    rate: views.iter().map(|v| v.rate).sum(),
                    requests: sc.stats.requests,
                    repairs: sc.stats.repairs,
                    conns: views
                        .iter()
                        .map(|v| {
                            let (lo, pos, hi) = v.range.unwrap_or((0, 0, 0));
                            ConnLine {
                                host: v.host.clone(),
                                lo,
                                hi,
                                pos,
                                rate: v.rate,
                                health: format!("{:?}", v.health).to_lowercase(),
                            }
                        })
                        .collect(),
                });
            }
            p.draw(
                done,
                &views,
                Counters {
                    requests: sc.stats.requests,
                    repairs: sc.stats.repairs,
                    reclaims: sc.stats.reclaims,
                    wasted: 0,
                    retries: 0,
                },
            );
        };
        // Under `--no-save` the sink stores nothing and carries the digest
        // instead, so the size, checksum, and format classification are all
        // computed from the STREAM. There is no file at any point.
        let sink = discard_sink.clone();
        if digest_cannot_finish(size, per.iter().sum()) {
            if let Some(sk) = &sink {
                sk.abandon_digest();
            }
        }
        // One limiter for the whole transfer, however many connections it opens:
        // `--limit-rate 1M` means the transfer uses 1 MB/s, not 1 MB/s per
        // connection. `--no-save` is capped too — the bytes still cross the
        // network, which is what the flag is about.
        let pace = hya_net::polite::Pace::shared(limiter.clone());
        // Keep the progress view honest across a substitution, and say so at
        // level 0: a mirror changing under a running transfer is exactly the
        // kind of thing a user wants in the log when they come back to a
        // download that took longer than expected.
        let sub_hosts = hosts.clone();
        let sub_note_w = swaps.clone();
        let mut on_sub = move |src: usize, r: &hya_net::Reserve| {
            let mut names = sub_hosts.lock().unwrap();
            let was = names.get(src).cloned().unwrap_or_else(|| "?".into());
            if let Some(slot) = names.get_mut(src) {
                slot.clone_from(&r.host);
            }
            sub_note_w.lock().unwrap().push(format!(
                "{was} failed; switched to reserve mirror {}",
                r.host
            ));
        };
        // One transfer entry point for both paths. `--no-save`'s discarding sink
        // and the ordinary sparse file differ only in where the bytes land, and
        // splitting the call in two meant the reserve bench had to be threaded
        // through twice — so the sink is chosen here and the call is made once.
        // Hash in-band only where the stream digest can finish (one connection,
        // starting from zero, the whole object) — see `file_stream_sha256`.
        let hash_in_band = want_digest_value
            && !discarding
            && resumed_from == 0
            && job.range.is_none()
            && per.iter().sum::<usize>() == 1;
        let sk = match &sink {
            Some(sk) => sk.clone(),
            None => match SparseSink::create(&outs, size) {
                Ok(sk) if hash_in_band => {
                    Arc::new(sk.with_digest(hya_net::stream_digest::DEFAULT_REORDER_CAP))
                }
                Ok(sk) => Arc::new(sk),
                Err(e) => {
                    return Err(Box::new(failed(
                        job,
                        size,
                        format!("cannot create {outs}: {e}"),
                    )))
                }
            },
        };
        let file_sink = sink.is_none().then(|| sk.clone());
        let r = hya_net::run_transfer_with_reserves(
            c,
            tgts,
            &per,
            size,
            sk,
            sched,
            20,
            &mut render,
            pace,
            job.cancel.clone(),
            bench,
            Some(&mut on_sub),
        )
        .await;
        if let Some(discarding_sink) = &sink {
            stream_result = discarding_sink.take_digest(size);
        }
        if let Some(fs) = &file_sink {
            // `None` when no digest was attached or it could not finish; either
            // way the file is hashed afterwards, so nothing is lost but time.
            file_stream_sha256 = fs.take_digest(size).and_then(|(d, _, _)| d);
        }
        r
    };
    // Two clocks, both reported. `elapsed` is the whole invocation; `transfer_elapsed`
    // is what the progress bar measured. Conflating them made a 1.7s transfer report
    // 4.0s with a throughput the network never delivered.
    let transfer_elapsed = t_transfer.elapsed().as_secs_f64();
    let elapsed = t_start.elapsed().as_secs_f64();
    drop(limiter);

    // Report why a transfer failed, with the request count the observer
    // measured: `Err(_) => (false, 0)` once printed "83.6 MiB, 0 requests" for
    // a byte-complete file and threw away the one piece of evidence.
    let interrupted = res
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::Interrupted);
    let transfer_error: Option<String> = res.as_ref().err().map(|e| e.to_string());
    let (ok, requests) = match &res {
        Ok((_, r)) => (true, *r),
        // The request count is a property of what the scheduler DID, not of
        // whether it finished. Sampled from the observer, which ran on every tick.
        Err(_) => (
            false,
            observed_requests.load(std::sync::atomic::Ordering::Relaxed),
        ),
    };
    let held_now: Vec<(u64, u64)> = last_held.lock().unwrap().clone();
    if let Some(why) = transfer_error.as_deref() {
        if !job.quiet || job.show_error {
            if interrupted {
                let held: u64 = held_now.iter().map(|(a, b)| b - a).sum();
                eprintln!(
                    "hydra: interrupted; {} held{}",
                    hya_core::fmt::bytes(held),
                    if discarding || job.to_stdout {
                        String::new()
                    } else {
                        format!(", re-run with -c to continue {}", out_path.display())
                    }
                );
            } else {
                eprintln!("hydra: transfer error: {why}");
            }
        }
    }
    placement.resumed_from = resumed_from;
    Ok(Transferred {
        ok,
        interrupted,
        transfer_error,
        requests,
        held_now,
        bytes_held: progress.load(std::sync::atomic::Ordering::Relaxed),
        used_conns: used_conns.load(std::sync::atomic::Ordering::Relaxed),
        settled_conns: settled_conns.load(std::sync::atomic::Ordering::Relaxed),
        peak_busy: peak_busy.load(std::sync::atomic::Ordering::Relaxed),
        conn_secs: conn_secs.load(std::sync::atomic::Ordering::Relaxed),
        stream_result,
        file_stream_sha256,
        transfer_elapsed,
        elapsed,
    })
}

/// Verify, deliver the span, classify, sort, and write the sidecar and the
/// report.
async fn finish(
    job: &Job,
    s: Prepared,
    placement: Placement,
    plan: Plan,
    moved: Transferred,
) -> Outcome {
    let Prepared {
        conn,
        usable,
        probe_info,
        name,
        out_path,
        size,
        validator,
        last_modified,
        served_type,
        mut p,
        ..
    } = s;
    let Placement {
        discarding,
        resumed_from,
        ..
    } = placement;
    let Plan {
        delta,
        want_digest_value,
        n_conns,
        want_lo,
        want_hi,
        partial,
    } = plan;
    let Transferred {
        ok,
        interrupted,
        transfer_error,
        requests,
        held_now,
        bytes_held,
        used_conns,
        settled_conns,
        peak_busy,
        conn_secs,
        stream_result,
        mut file_stream_sha256,
        transfer_elapsed,
        elapsed,
    } = moved;
    let counters = Counters {
        requests,
        ..Default::default()
    };
    // Completeness is judged on the scheduler's own count, on the object's
    // coordinate scale: the sparse output reads as full-length from the first
    // byte, allocated blocks read as full on a filesystem without holes, and
    // the pre-transfer sidecar is stale. In range mode the spans outside
    // `[want_lo, want_hi)` are marked held and must keep counting as present.
    // Under `--no-save` the transfer's own success is the only evidence.
    let on_disk = if discarding {
        if ok {
            want_hi.min(size)
        } else {
            0
        }
    } else {
        bytes_held
    };
    // In range mode the sparse file is still `size` long but only the requested
    // span was fetched, so completion is judged on the scheduler's own accounting
    // rather than on file length.
    let complete = ok && on_disk >= want_hi.min(size);

    // The sparse file has the object's whole extent and everything outside
    // `[want_lo, want_hi)` is a hole that reads as zeros: `-r 0-1023` once
    // delivered a 34 041-byte file with 1 024 real bytes. Done before the
    // digest, which must describe the bytes the user receives.
    if partial && complete && !discarding && !job.to_stdout {
        if let Err(e) = extract_span(&out_path, want_lo, want_hi) {
            return failed(
                job,
                size,
                format!(
                    "range mode: cannot reduce {} to its span: {e}",
                    out_path.display()
                ),
            );
        }
    }

    // What the user was handed. In range mode that is the span, not the object:
    // reporting "33.2 KiB in 1.2s" for a 1 KiB range described a transfer that
    // did not happen, and the throughput derived from it was wrong by the same
    // factor.
    let delivered = if partial {
        want_hi.saturating_sub(want_lo)
    } else {
        on_disk
    };

    // After the transfer, because a chunk is only checkable once its last byte
    // has landed. A mismatch is refetched from a different source than the one
    // that served it — a mirror that served corrupt bytes once is the least
    // likely to serve them correctly now — which beats carrying parity while
    // the source is reachable.
    let mut chunk_report: Option<String> = None;
    if let Some(mpath) = job.chunk_digests.clone() {
        if complete && !discarding {
            match verify_and_repair_chunks(&conn, &usable, &out_path, &mpath, job, &mut p).await {
                Ok(r) => chunk_report = Some(r),
                Err(e) => return failed(job, size, e),
            }
        }
    } else if let Some(pieces) = job.attested.as_ref().and_then(|a| a.pieces.clone()) {
        // The document's `<pieces>`: a corrupt chunk costs one chunk refetched
        // from a different mirror, and the manifest says which. `Advertised`,
        // not `Trusted`: the `<signature>` is recorded, not verified, so the
        // pieces may drive a self-correcting refetch but not a parity decode.
        if complete && !discarding {
            match verify_and_repair(
                &conn,
                &usable,
                &out_path,
                pieces,
                hya_net::manifest::Trust::Advertised,
                job,
                &mut p,
            )
            .await
            {
                Ok(r) => chunk_report = Some(r),
                Err(e) => return failed(job, size, e),
            }
        }
    }

    let digest = if !complete {
        None
    } else if discarding {
        // Computed from the stream as it passed through the discarding sink.
        // `None` here means the digest genuinely could not be established (the
        // ranges arrived too far out of order to hash within the buffer budget),
        // and the reason is reported rather than a wrong value substituted.
        let d = stream_result.as_ref().and_then(|(d, _, _)| d.clone());
        if d.is_none() {
            if let Some(reason) = stream_result.as_ref().and_then(|(_, _, r)| r.clone()) {
                if !job.quiet {
                    eprintln!("hydra: {reason}");
                }
            }
        }
        d
    } else {
        // Chunk verification may have refetched and rewritten spans after the
        // transfer, so a digest taken during it no longer describes the file.
        file_stream_sha256
            .take()
            .filter(|_| chunk_report.is_none())
            // The whole-file pass is the expensive one: on a CPU without the SHA
            // extensions it is several seconds per gigabyte, and `--no-checksum`
            // exists to decline exactly that.
            .or_else(|| want_digest_value.then(|| sha256_file(&out_path)).flatten())
    };
    // What must the bytes hash to?
    //
    // `--checksum` first: a digest the user typed came from somewhere they chose
    // — a release page, a signed announcement, a colleague — and it outranks one
    // that arrived over the same session as the mirror list. The document's is
    // the fallback, and it is the common case, because nobody types a SHA-512 by
    // hand for a file they are about to download from nineteen mirrors.
    let want_digest: Option<String> = job
        .checksum
        .clone()
        .or_else(|| job.attested.as_ref().and_then(|a| a.digest.clone()));
    let checksum_ok = match (&want_digest, complete) {
        // Nothing to check against, or an incomplete file: an object that did
        // not arrive cannot match a digest, and saying `None` there would report
        // "not checked" about a file that is definitively wrong.
        (None, _) => None,
        (Some(_), false) => Some(false),
        (Some(spec), true) if discarding => match parse_digest_spec(spec) {
            None => {
                if !job.quiet {
                    eprintln!("hydra: cannot check {spec:?}: unknown digest algorithm");
                }
                None
            }
            Some((hya_net::digest::Algo::Sha256, want)) => digest.as_ref().map(|d| *d == want),
            Some((algo, _)) => {
                // Nothing was written, so there is no file to re-read. The
                // stream digest is SHA-256 only, so any other algorithm
                // genuinely cannot be checked — worth saying rather than
                // silently reporting "verified".
                if !job.quiet {
                    eprintln!(
                        "hydra: --no-save keeps no file, so the {} digest could not be checked (sha256 is computed from the stream)",
                        algo.as_str()
                    );
                }
                None
            }
        },
        (Some(spec), true) => verify_file_digest(job, &out_path, spec, digest.as_deref()),
    };

    // Classification happens AFTER the transfer because the payload is the only
    // trustworthy signal, and it is read from the head of the finished file
    // rather than from a separate probe request.
    let detection = {
        let mut head = vec![0u8; 0];
        if discarding {
            // The leading bytes were retained by the stream observer; there is no
            // file to reopen. Classification is therefore unchanged by --no-save,
            // which is the point of the flag: probe what a URL serves without
            // leaving anything behind.
            if let Some((_, h, _)) = &stream_result {
                head = h.clone();
            }
        } else if complete {
            use std::io::Read as _;
            if let Ok(mut fh) = std::fs::File::open(&out_path) {
                let mut buf = vec![0u8; 8192];
                if let Ok(n) = fh.read(&mut buf) {
                    buf.truncate(n);
                    head = buf;
                }
            }
        }
        detect_format(&head, &name, served_type.as_deref())
    };
    if let Some(msg) = &detection.conflict {
        // Worth saying even though nothing failed: an HTML body delivered where an
        // archive was expected is the signature of a captive portal or an error
        // page saved as a file, and both the byte count and the status look fine.
        if !job.quiet {
            eprintln!("hydra: warning: {msg}");
            if detection.looks_intercepted() {
                eprintln!(
                    "  the saved file is a web page, not the object requested \
                     (captive portal, login wall, or an error page served with status 200)"
                );
            }
        }
    } else if let Some(f) = detection.format {
        // At -v, name the format. At -vv, explain it: the description is aimed at
        // someone deciding what to do with the file they just fetched.
        p.event(1, &format!("detected {} ({})", f.name, f.category.as_str()));
        p.event(2, &format!("  {}", f.hint()));
    }

    let out_path = if job.sort_by_type
        && complete
        && !discarding
        && !job.to_stdout
        && detection.category != Category::Unknown
    {
        sort_into_category(out_path, detection.category, &mut p)
    } else {
        out_path
    };

    if job.server_response {
        p.end_phase();
        // Verbatim first, interpretation second. Print raw headers as received; the
        // paraphrase below is a convenience, and when the two disagree the raw block
        // is the evidence.
        print_exchange(&probe_info);
        println!("hydra: interpretation");
        println!("  size: {size}");
        println!("  validator: {}", validator.as_deref().unwrap_or("none"));
        println!(
            "  validator strength: {}",
            if probe_info.weak_validator {
                "weak (single-source only: a weak validator may compare equal across \
                 different bytes, so cross-mirror assembly is not sound)"
            } else if validator.is_some() {
                "strong (cross-mirror assembly permitted)"
            } else {
                "none"
            }
        );
        println!(
            "  range support: {}",
            if probe_info.ranges { "yes" } else { "no" }
        );
        println!("  sources accepted: {}", usable.len());
        println!("  connections used: {n_conns}");
    }

    if discarding || job.to_stdout {
        // No file to resume into: `--no-save` keeps nothing and `--stdout`
        // stages in a temp name nobody will name again. A sidecar here would
        // be litter.
    } else if complete && checksum_ok != Some(false) {
        Sidecar::remove(&out_path);
    } else if !held_now.is_empty() {
        // Keep the sidecar so `-c` can pick up where this left off — from the
        // ranges actually held, never a prefix summing to the same count.
        let sc = Sidecar {
            size,
            validator: validator.clone(),
            done: held_now.clone(),
            url: job.urls[0].clone(),
        };
        let _ = sc.save(&out_path);
    }

    // `--no-save` needs no cleanup: nothing was ever created. The digest and the
    // format classification came from the stream (see `stream_digest`), which is
    // what made deleting-afterwards unnecessary. The earlier create-write-hash-
    // delete implementation is what left a 45 MB file behind when a run was
    // interrupted.

    // --stdout: stream the assembled object out, then remove the staging file.
    // Not as bytes arrive: positioned writes land out of order, so the file is
    // only correct once complete. Copied in fixed-size chunks — reading into a
    // `Vec` needed the whole object resident — and only the requested span,
    // since outside `[want_lo, want_hi)` the staging file is a hole.
    let mut stdout_error: Option<String> = None;
    if job.to_stdout {
        if complete && checksum_ok != Some(false) {
            if let Err(e) = copy_span_to_stdout(&out_path, want_lo, want_hi) {
                stdout_error = Some(format!("cannot stream to stdout: {e}"));
            }
        }
        let _ = std::fs::remove_file(&out_path);
    }
    if let Some(e) = &stdout_error {
        if !job.quiet || job.show_error {
            eprintln!("hydra: {e}");
        }
    }
    let verified = complete && checksum_ok != Some(false) && stdout_error.is_none();

    if job.remote_time && complete && !discarding && !job.to_stdout {
        apply_remote_time(
            &out_path,
            last_modified.as_deref(),
            validator.as_deref(),
            &mut p,
        );
    }
    if verified {
        save_etag(job, validator.as_deref(), &mut p);
    }

    p.finish(delivered, verified, counters, digest.as_deref());
    // At default verbosity a format note is printed only when it is a WARNING —
    // the served bytes are not what was asked for. "gzip stream — compresses a
    // single stream…" is a description of a successful download and belongs at
    // `-v`; "this is an HTML page where a file was expected" is the difference
    // between a good file and a captive-portal page saved with status 200, and
    // suppressing it would hide the failure this project cares most about.
    if !job.quiet {
        if let Some(f) = detection.format {
            // `Markup` is the tell: an HTML page delivered where a file was
            // expected is the captive-portal / login-wall / error-page-with-200
            // case. A conflict between the sniffed type and the served
            // Content-Type is the other.
            let suspicious = detection.conflict.is_some() || f.category == Category::Markup;
            // At default verbosity this is a VALUE, not a sentence: `HTML page`
            // rather than `HTML page — A web page. Where a real file was
            // expected, this usually means...`. The explanation is real and worth
            // having, but it belongs at `-v`; a user who has seen it once does not
            // need the paragraph on every subsequent run.
            let note = if job.verbose > 0 {
                f.hint()
            } else {
                f.label().to_string()
            };
            if suspicious || job.verbose > 0 {
                // Never onto stdout when the object is going there: appending a
                // hint to a piped archive corrupts it (measured: 344 extra bytes
                // on a 34 041-byte .tar.gz, which then failed to decompress).
                // Under `--logfile` it goes to the file with the rest of the run.
                p.note(&format!("  {note}"), suspicious);
            }
        }
    }
    if checksum_ok == Some(false) {
        p.note(
            "  checksum MISMATCH: the delivered bytes are not the bytes requested",
            true,
        );
    }

    // Only for a download that verified. A manifest over bytes we already
    // believe are wrong would record the corruption as if it were the truth,
    // and every later check against it would agree.
    if let Some(mpath) = &job.emit_manifest {
        if complete && checksum_ok != Some(false) && !discarding {
            let cs = job
                .chunk_size
                .unwrap_or(hya_net::manifest::DEFAULT_CHUNK)
                .max(1);
            match hya_net::manifest::from_file(
                &out_path.to_string_lossy(),
                cs,
                hya_net::manifest::ChunkAlgo::Blake3,
                Some(job.urls[0].clone()),
                validator.clone(),
            ) {
                Ok(m) => match std::fs::write(mpath, m.to_json()) {
                    Ok(()) => {
                        if !job.quiet {
                            eprintln!(
                                "  manifest: {} chunks of {} bytes -> {}",
                                m.chunks.digests.len(),
                                cs,
                                mpath.display()
                            );
                        }
                    }
                    Err(e) => eprintln!("hydra: cannot write manifest {}: {e}", mpath.display()),
                },
                Err(e) => eprintln!("hydra: cannot build manifest: {e}"),
            }
        } else if !job.quiet {
            eprintln!(
                "hydra: --emit-manifest skipped: a manifest is only written for a download \
                 that verified"
            );
        }
    }
    if let Some(r) = &chunk_report {
        if !job.quiet {
            eprintln!("  chunk integrity: {r}");
        }
    }

    Outcome {
        url: job.urls[0].clone(),
        output: if job.to_stdout {
            String::new()
        } else {
            out_path.to_string_lossy().to_string()
        },
        // The bytes delivered, which in range mode is the span rather than the
        // object's length. A consumer of `--json` comparing `size` against the
        // file it just received must find them equal.
        size: delivered,
        elapsed_s: elapsed,
        transfer_s: transfer_elapsed,
        setup_s: (elapsed - transfer_elapsed).max(0.0),
        throughput_bps: if transfer_elapsed > 0.0 {
            // Throughput of the TRANSFER, not of the invocation: dividing by setup time
            // too reports a rate the network never achieved.
            delivered as f64 / transfer_elapsed
        } else {
            0.0
        },
        requests,
        // What the transfer actually ran at, not the budget it was allowed.
        // Report what the transfer RAN at, not the peak the search explored: a number
        // the run did not use is not a measurement. `peak_connections` carries the
        // exploration separately for anyone diagnosing the search itself.
        connections: settled_conns.max(1).min(used_conns.max(1)),
        peak_connections: used_conns.max(1),
        peak_busy_connections: peak_busy,
        connection_seconds: conn_secs as f64 / 1e6,
        delta_s: delta,
        sha256: digest,
        checksum_ok,
        resumed_from,
        ok: verified,
        note: if interrupted {
            Some("interrupted".into())
        } else {
            transfer_error.or(stdout_error)
        },
        format: detection.format.map(|f| f.name.to_string()),
        category: Some(detection.category.as_str().to_string()),
        format_conflict: detection.conflict,
        format_label: detection.format.map(|f| f.label().to_string()),
        format_description: detection.format.map(|f| f.description().to_string()),
        category_description: Some(detection.category.description().to_string()),
    }
}

impl Outcome {
    /// An outcome for a path that ended before any bytes moved: a refusal, a skip,
    /// or an unchanged object. Keeps the four early-return sites from each
    /// carrying their own copy of every field.
    fn stopped(job: &Job, output: String, size: u64, ok: bool, note: &str) -> Self {
        if !ok && (!job.quiet || job.show_error) {
            eprintln!("hydra: {note}");
        }
        Self {
            url: job.urls.first().cloned().unwrap_or_default(),
            output,
            size,
            ok,
            note: Some(note.to_string()),
            ..Self::default()
        }
    }
}

fn failed(job: &Job, size: u64, why: String) -> Outcome {
    // `-q` silences normal output; `--show-error` carves out the exception for
    // failures when requested. Without it a quiet run that failed
    // was indistinguishable from a quiet run that succeeded: empty stdout, empty
    // stderr, and only the exit code to tell them apart.
    if !job.quiet || job.show_error {
        eprintln!("hydra: {why}");
    }
    Outcome {
        url: job.urls.first().cloned().unwrap_or_default(),
        size,
        note: Some(why),
        ..Outcome::default()
    }
}

/// Unused by the engine, but the renderer needs the type in scope for callers
/// that build views from a live scheduler.
pub fn conn_views(sched: &Scheduler, hosts: &[String]) -> Vec<ConnView> {
    (0..sched.n_conns())
        .map(|j| ConnView {
            idx: j,
            host: hosts.get(sched.conn_source(j)).cloned().unwrap_or_default(),
            range: sched.conn_range(j),
            rate: sched.conn_rate(j),
            health: sched.conn_health(j),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ambient `http_proxy` of a CI sandbox must not divert a loopback origin.
    fn no_proxy() -> ProxyPolicy {
        ProxyPolicy::new(None, true)
    }

    /// Answers every request with `400 Bad Request` and a 24-byte JSON body —
    /// the shape of a CDN's "no such file" answer, with a `Content-Length` that
    /// a probe reading only the size would take for the object's.
    async fn spawn_400_origin() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let body = br#"{"error":"bad request!"}"#;
                    let _ = s
                        .write_all(
                            format!(
                                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = s.write_all(body).await;
                });
            }
        });
        port
    }

    /// A ranged origin for whole-job tests: HEAD states the length, a ranged
    /// GET answers `206`, and a plain GET answers `200`. Keeps the connection
    /// open across requests, since the client pools them.
    async fn spawn_ranged_origin(body: std::sync::Arc<Vec<u8>>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        // One request head, then answer, then the next.
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let text = String::from_utf8_lossy(&head[..end]).to_string();
                        head.drain(..end);
                        let method = text.split_whitespace().next().unwrap_or("").to_string();
                        let range = text
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()));
                        let total = body.len();
                        let (status, lo, hi) = match range.as_deref() {
                            Some(r) => {
                                let (a, b) = r.split_once('-').unwrap_or(("0", ""));
                                let lo: usize = a.parse().unwrap_or(0);
                                let hi: usize = b.parse().unwrap_or(total - 1);
                                ("206 Partial Content", lo, hi.min(total - 1))
                            }
                            None => ("200 OK", 0, total - 1),
                        };
                        let len = hi - lo + 1;
                        let mut h = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n\
                             ETag: \"ranged\"\r\nContent-Type: application/octet-stream\r\n"
                        );
                        if range.is_some() {
                            h.push_str(&format!("Content-Range: bytes {lo}-{hi}/{total}\r\n"));
                        }
                        h.push_str("\r\n");
                        if s.write_all(h.as_bytes()).await.is_err() {
                            return;
                        }
                        if method != "HEAD" && s.write_all(&body[lo..=hi]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    /// A login-gated origin: `/login` hands out a session and forwards to
    /// `/file`, which serves the object only to a request that carries it.
    ///
    /// This is issue #227's third failure in miniature. A client with no jar
    /// sends the second hop bare and is answered `403`, which looks like a
    /// server fault and is not.
    async fn spawn_gated_origin(body: std::sync::Arc<Vec<u8>>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let text = String::from_utf8_lossy(&head[..end]).to_string();
                        head.drain(..end);
                        let line = text.lines().next().unwrap_or("").to_string();
                        let method = line.split_whitespace().next().unwrap_or("").to_string();
                        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                        let has_session = text
                            .lines()
                            .filter(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                            .any(|l| l.contains("sid=granted"));

                        if path.starts_with("/login") {
                            let h = "HTTP/1.1 302 Found\r\nLocation: /file\r\n\
                                     Set-Cookie: sid=granted; Path=/; Max-Age=3600\r\n\
                                     Set-Cookie: tracker=x; Domain=127.0.0.2; Path=/\r\n\
                                     Content-Length: 0\r\n\r\n";
                            if s.write_all(h.as_bytes()).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        if !has_session {
                            let h = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
                            if s.write_all(h.as_bytes()).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        let total = body.len();
                        let range = text
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()));
                        let (status, lo, hi) = match range.as_deref() {
                            Some(r) => {
                                let (a, b) = r.split_once('-').unwrap_or(("0", ""));
                                let lo: usize = a.parse().unwrap_or(0);
                                let hi: usize = b.parse().unwrap_or(total - 1);
                                ("206 Partial Content", lo, hi.min(total - 1))
                            }
                            None => ("200 OK", 0, total - 1),
                        };
                        let len = hi - lo + 1;
                        let mut h = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n\
                             ETag: \"gated\"\r\nContent-Type: application/octet-stream\r\n"
                        );
                        if range.is_some() {
                            h.push_str(&format!("Content-Range: bytes {lo}-{hi}/{total}\r\n"));
                        }
                        h.push_str("\r\n");
                        if s.write_all(h.as_bytes()).await.is_err() {
                            return;
                        }
                        if method != "HEAD" && s.write_all(&body[lo..=hi]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    /// `hydra checksum` and `--server-response` describe an object a login
    /// gate may hide, so they read the same jar a download would — and say so
    /// on stderr, where the consent line cannot land inside the one answer
    /// they write to stdout.
    #[tokio::test]
    async fn a_reporting_command_reads_the_jar_it_was_given() {
        let dir = std::env::temp_dir().join(format!("hydra_cookie_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        let jar_path = dir.join("jar.txt");
        std::fs::write(&jar_path, "127.0.0.1\tFALSE\t/\tFALSE\t0\tsid\tabc\n").unwrap();

        let argv = [
            "hydra",
            "--load-cookies",
            jar_path.to_str().unwrap(),
            "http://127.0.0.1/x",
        ];
        let args = <crate::cli::Cli as clap::Parser>::parse_from(argv);
        let jar = open_jar_for(&args, "127.0.0.1", 0).await.unwrap();
        assert_eq!(
            jar.header_value("127.0.0.1", "/x", false, 0).as_deref(),
            Some("sid=abc")
        );

        let args = <crate::cli::Cli as clap::Parser>::parse_from(["hydra", "http://127.0.0.1/x"]);
        assert!(
            open_jar_for(&args, "127.0.0.1", 0)
                .await
                .unwrap()
                .is_empty(),
            "no flag, no jar"
        );

        let absent = dir.join("absent.txt");
        let argv = [
            "hydra",
            "--load-cookies",
            absent.to_str().unwrap(),
            "http://127.0.0.1/x",
        ];
        let args = <crate::cli::Cli as clap::Parser>::parse_from(argv);
        let e = open_jar_for(&args, "127.0.0.1", 0).await.unwrap_err();
        assert!(e.contains("absent.txt"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The headline behaviour: a redirect that SETS a session must be able to
    /// send it back on the next hop.
    ///
    /// This works with NO cookie flag, which is a deliberate departure from
    /// "no flag, no change": a chain-scoped jar is what makes `-L` correct, it
    /// touches no disk and no other host, and without it the only users who can
    /// fetch from a login-gated CDN are the ones who already knew to pass a
    /// flag. What stays opt-in is everything that OUTLIVES the chain — reading
    /// a jar file, writing one, importing from a browser.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_redirect_that_hands_out_a_session_can_be_followed() {
        let body: Vec<u8> = (0..200_000u64).map(|i| (i % 251) as u8).collect();
        let want = hya_net::digest::to_lower_hex(&Sha256::digest(&body));
        let port = spawn_gated_origin(std::sync::Arc::new(body)).await;
        let dir = std::env::temp_dir().join(format!("hydra_cookie_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("http://127.0.0.1:{port}/login");

        let mut bare = default_job();
        bare.urls = vec![url.clone()];
        bare.output = Some(dir.join("bare.bin"));
        bare.print_checksum = true;
        let out = run(bare).await;
        assert!(out.ok, "the hop must carry what the hop was given: {out:?}");
        assert_eq!(out.sha256.as_deref(), Some(want.as_str()));

        let jar_path = dir.join("jar.txt");
        let argv = [
            "hydra",
            "--cookie-jar",
            jar_path.to_str().unwrap(),
            "--keep-session-cookies",
            &url,
        ];
        let args = <crate::cli::Cli as clap::Parser>::parse_from(argv);
        let mut job = default_job();
        job.urls = vec![url.clone()];
        job.output = Some(dir.join("gated.bin"));
        job.cookies = crate::cookies::CookieSpec::from_cli(&args).unwrap();
        assert!(run(job).await.ok);

        // Written back, and scoped: the origin also set a cookie for a host it
        // is not under, which must never have been stored at all.
        let saved = std::fs::read_to_string(&jar_path).unwrap();
        assert!(saved.contains("sid"), "the session was not saved: {saved}");
        assert!(
            !saved.contains("tracker"),
            "a Domain the setting host is not under must never be stored: {saved}"
        );

        // And a second run reads it back, so the login hop is not needed at all:
        // `/file` answers `403` to a request that arrives without a session.
        let argv = ["hydra", "--load-cookies", jar_path.to_str().unwrap(), &url];
        let args = <crate::cli::Cli as clap::Parser>::parse_from(argv);
        let mut job = default_job();
        job.urls = vec![format!("http://127.0.0.1:{port}/file")];
        job.output = Some(dir.join("direct.bin"));
        job.cookies = crate::cookies::CookieSpec::from_cli(&args).unwrap();
        assert!(
            run(job).await.ok,
            "a jar read from disk must authenticate the very first request"
        );

        // Without one, that same first request is refused — which is what the
        // jar is for, and proof the origin really is gated.
        let mut job = default_job();
        job.urls = vec![format!("http://127.0.0.1:{port}/file")];
        job.output = Some(dir.join("refused.bin"));
        assert!(!run(job).await.ok, "the origin is not actually gated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reported SHA-256 must be the file's SHA-256, however it was computed.
    ///
    /// A single-connection transfer hashes the bytes as they land instead of
    /// reading the finished file back (`file_stream_sha256`); anything else
    /// hashes the file. Both must agree with each other and with the object, or
    /// the optimisation has produced the failure this project fears most: a
    /// stable, plausible, wrong digest.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_reported_sha256_is_the_files_sha256_on_every_hashing_path() {
        let body: Vec<u8> = (0..3_000_017u64).map(|i| (i % 251) as u8).collect();
        let want = hya_net::digest::to_lower_hex(&Sha256::digest(&body));
        let port = spawn_ranged_origin(std::sync::Arc::new(body)).await;
        let dir = std::env::temp_dir().join(format!("hydra_digest_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        // `None` is the default path, where the concurrency probe chooses the
        // count and may pre-fill bytes the sink never sees. It is covered here
        // because it is what a plain `hydra URL` runs, and because a pre-filled
        // prefix is exactly the case where the in-band hash must decline and let
        // the file be hashed instead.
        for conns in [Some(1usize), Some(2), None] {
            let label = conns.map_or("default".to_string(), |n| format!("-x {n}"));
            let out = dir.join(format!("obj{}.bin", conns.unwrap_or(0)));
            let mut job = default_job();
            job.urls = vec![format!("http://127.0.0.1:{port}/obj.bin")];
            job.output = Some(out.clone());
            job.conns = conns;
            job.print_checksum = true;
            let o = run(job).await;
            assert!(o.ok, "transfer at {label} failed: {:?}", o.note);
            assert_eq!(
                o.sha256.as_deref(),
                Some(want.as_str()),
                "{label}: reported digest is not the object's"
            );
            assert_eq!(
                sha256_file(&out).as_deref(),
                Some(want.as_str()),
                "{label}: file on disk is not the object"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An origin that redirects `/start` once and records the headers of every
    /// request it was sent, so a test can assert on what actually reached the
    /// wire rather than on what the code meant to send.
    ///
    /// `to` is where it forwards, which lets one helper cover both a
    /// same-origin hop and a hop to somewhere else. `html` picks WHICH kind of
    /// forwarding: a `302`, or the page that says the same thing in a meta
    /// refresh — two different branches of the same hop rule.
    async fn spawn_recording_origin(
        to: String,
        html: bool,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        let sink = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let (sink, to) = (sink.clone(), to.clone());
                let page = format!(
                    "<html><head><meta http-equiv=\"refresh\" content=\"0; url={to}\">\
                     </head><body>going</body></html>"
                );
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let text = String::from_utf8_lossy(&head[..end]).to_string();
                        head.drain(..end);
                        sink.lock().unwrap().push(text.clone());
                        let method = text.split_whitespace().next().unwrap_or("").to_string();
                        let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                        let reply = if path.starts_with("/start") && html {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                                 Content-Length: {}\r\n\r\n",
                                page.len()
                            )
                        } else if path.starts_with("/start") {
                            format!(
                                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n"
                            )
                        } else {
                            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nAccept-Ranges: bytes\r\n\
                             ETag: \"rec\"\r\n\r\n"
                                .to_string()
                        };
                        if s.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                        let body: &[u8] = match (path.starts_with("/start"), html, &method[..]) {
                            (_, _, "HEAD") => b"",
                            (true, true, _) => page.as_bytes(),
                            (true, false, _) => b"",
                            (false, _, _) => b"forty-two",
                        };
                        if !body.is_empty() && s.write_all(body).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (port, seen)
    }

    /// Requests recorded for `path`, as raw head text.
    fn requests_for<'a>(seen: &'a [String], path: &str) -> Vec<&'a String> {
        seen.iter()
            .filter(|r| r.split_whitespace().nth(1) == Some(path))
            .collect()
    }

    /// The reported bug: every hop after a redirect went out with no `-H`
    /// headers and no `-U` agent, because the hop rebuilt its target from the
    /// URL alone. A `hydra -H 'X-Api-Key: …'` against a redirecting host
    /// silently lost the key, and the transfer — which runs against this
    /// resolved target — lost it too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_redirect_hop_still_carries_the_headers_the_user_passed() {
        let (port, seen) = spawn_recording_origin("/landed".into(), false).await;
        let u = crate::url::Url::parse(&format!("http://127.0.0.1:{port}/start")).expect("url");
        let t = u.to_target(None).expect("target").with_headers(
            vec![
                "X-Api-Key: k-123".to_string(),
                "Authorization: Bearer t-456".to_string(),
            ],
            Some("hydra-test/1".to_string()),
        );
        let c = hya_net::TlsCapableConnector::new().expect("connector");
        let mut log = Vec::new();
        let r = probe_resolving(
            &c,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
        .expect("the chain resolves");

        let seen = seen.lock().unwrap();
        let landed = requests_for(&seen, "/landed");
        assert!(!landed.is_empty(), "the redirect was never followed");
        for req in &landed {
            assert!(
                req.contains("X-Api-Key: k-123"),
                "header lost on the hop: {req}"
            );
            assert!(
                req.contains("User-Agent: hydra-test/1"),
                "agent lost on the hop: {req}"
            );
            // Same origin, so the credential is still this host's to receive.
            assert!(req.contains("Authorization: Bearer t-456"), "{req}");
        }
        // And the target handed to the transfer carries them, which is the half
        // of the bug a probe-only assertion would miss.
        assert!(r.target.headers.iter().any(|h| h == "X-Api-Key: k-123"));
        assert_eq!(r.target.agent.as_deref(), Some("hydra-test/1"));
    }

    /// The boundary the fix must not cross: a hop to a different origin keeps
    /// the ordinary headers and leaves the credential behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_redirect_to_another_origin_leaves_the_credential_behind() {
        let (other, seen_other) = spawn_recording_origin("/landed".into(), false).await;
        // `localhost` and `127.0.0.1` resolve to the same machine and are
        // different origins, which is exactly the case a host comparison has to
        // get right.
        let (port, _seen) =
            spawn_recording_origin(format!("http://localhost:{other}/landed"), false).await;
        let u = crate::url::Url::parse(&format!("http://127.0.0.1:{port}/start")).expect("url");
        let t = u.to_target(None).expect("target").with_headers(
            vec![
                "X-Api-Key: k-123".to_string(),
                "Authorization: Bearer t-456".to_string(),
                "Cookie: sid=secret".to_string(),
            ],
            Some("hydra-test/1".to_string()),
        );
        let c = hya_net::TlsCapableConnector::new().expect("connector");
        let mut log = Vec::new();
        probe_resolving(
            &c,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
        .expect("the chain resolves");

        let seen = seen_other.lock().unwrap();
        let landed = requests_for(&seen, "/landed");
        assert!(!landed.is_empty(), "the cross-origin hop was never taken");
        for req in &landed {
            assert!(
                req.contains("X-Api-Key: k-123"),
                "ordinary header lost: {req}"
            );
            assert!(
                req.contains("User-Agent: hydra-test/1"),
                "agent lost: {req}"
            );
            assert!(
                !req.contains("Bearer t-456"),
                "a bearer token reached another origin: {req}"
            );
            assert!(
                !req.contains("sid=secret"),
                "a hand-written cookie reached another origin: {req}"
            );
        }
    }

    /// A forwarding page is the same hop written in HTML, and it rebuilds its
    /// target through the same line — so it drops the same headers unless it is
    /// fixed too. Charged to the same rule and tested separately because a
    /// `3xx` test cannot reach this branch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_html_redirector_hop_carries_the_headers_too() {
        let (port, seen) = spawn_recording_origin("/landed".into(), true).await;
        let u = crate::url::Url::parse(&format!("http://127.0.0.1:{port}/start")).expect("url");
        let t = u.to_target(None).expect("target").with_headers(
            vec!["X-Api-Key: k-123".to_string()],
            Some("hydra-test/1".to_string()),
        );
        let c = hya_net::TlsCapableConnector::new().expect("connector");
        let mut log = Vec::new();
        let r = probe_resolving(
            &c,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
        .expect("the page forwards");
        assert!(r.via_html, "the hop was a 3xx, not the page");

        let seen = seen.lock().unwrap();
        let landed = requests_for(&seen, "/landed");
        assert!(!landed.is_empty(), "the page was never followed");
        for req in &landed {
            assert!(
                req.contains("X-Api-Key: k-123"),
                "header lost on the hop: {req}"
            );
            assert!(
                req.contains("User-Agent: hydra-test/1"),
                "agent lost on the hop: {req}"
            );
        }
    }

    /// Serve only requests whose head contains `pass`, `403` the rest;
    /// `/start` forwards to `/landed`.
    async fn spawn_dress_checking_origin(
        pass: impl Fn(u16) -> String,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        let pass = pass(port);
        let sink = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let (sink, pass) = (sink.clone(), pass.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&head).to_string();
                    sink.lock().unwrap().push(text.clone());
                    let reply = if !text.contains(&pass) {
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n"
                    } else if text.split_whitespace().nth(1) == Some("/start") {
                        "HTTP/1.1 302 Found\r\nLocation: /landed\r\nContent-Length: 0\r\n"
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nAccept-Ranges: bytes\r\n"
                    };
                    let _ = s
                        .write_all(format!("{reply}Connection: close\r\n\r\n").as_bytes())
                        .await;
                });
            }
        });
        (port, seen)
    }

    async fn resolve_gated(
        port: u16,
        path: &str,
        log: &mut Vec<(u8, String)>,
    ) -> Result<Resolved, String> {
        let u = crate::url::Url::parse(&format!("http://127.0.0.1:{port}{path}")).expect("url");
        let t = u
            .to_target(None)
            .expect("target")
            .with_headers(Vec::new(), Some("hydra-test/1".to_string()));
        let c = hya_net::TlsCapableConnector::new().expect("connector");
        probe_resolving(
            &c,
            &u,
            &t,
            log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
    }

    /// #277: `hydra <url>` failed with `403` against an origin that serves
    /// the same link to any browser.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_origin_that_serves_only_browsers_resolves_and_the_transfer_goes_as_one() {
        let pass = format!("User-Agent: {}", hya_net::BROWSER_USER_AGENT);
        let (port, _) = spawn_dress_checking_origin(|_| pass.clone()).await;
        let mut log = Vec::new();
        let r = resolve_gated(port, "/file.bin", &mut log)
            .await
            .expect("served to a browser");

        assert_eq!(r.probe.size, 9);
        assert_eq!(r.target.agent.as_deref(), Some(hya_net::BROWSER_USER_AGENT));
        assert!(
            log.iter()
                .any(|(_, line)| line.contains("refused the request as written")),
            "{log:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_a_gate_accepted_is_still_sent_after_a_redirect() {
        let (port, seen) =
            spawn_dress_checking_origin(|port| format!("Referer: http://127.0.0.1:{port}/\r\n"))
                .await;
        let mut log = Vec::new();
        let r = resolve_gated(port, "/start", &mut log)
            .await
            .expect("the hop is served");

        assert_eq!(r.url.path, "/landed");
        let own_site = format!("Referer: http://127.0.0.1:{port}/");
        assert!(
            r.target.headers.contains(&own_site),
            "{:?}",
            r.target.headers
        );
        let seen = seen.lock().unwrap();
        let landed = requests_for(&seen, "/landed");
        assert!(!landed.is_empty());
        assert!(
            landed.iter().all(|req| req.contains(&own_site)),
            "the hop was asked bare again: {landed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_link_refused_however_it_is_asked_keeps_its_plain_error() {
        let (port, _) = spawn_dress_checking_origin(|_| "X-Never: sent".into()).await;
        let err = resolve_gated(port, "/expired", &mut Vec::new())
            .await
            .err()
            .expect("refused");
        assert_eq!(err, "server answered 403 Forbidden");
    }

    /// An error status from the probe is an answer about the URL, not a
    /// description of a 24-byte object.
    ///
    /// `probe_resilient` returns the status rather than failing, on purpose, so
    /// that a `404` learned from HEAD is the error reported. The test is the
    /// caller's to make, and this caller did not make it: the error body's
    /// `Content-Length` became the file size, a transfer was planned, 24 bytes
    /// were split across eight connections, and only the range requests failed a
    /// second later — with "unexpected status 400 for a range request", which
    /// names the symptom and not the answer the server had already given.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_error_status_from_the_probe_is_reported_not_downloaded() {
        let port = spawn_400_origin().await;
        let u =
            crate::url::Url::parse(&format!("http://127.0.0.1:{port}/VSCode.zip0")).expect("url");
        let t = u.to_target(None).expect("target");
        let c = hya_net::TlsCapableConnector::new().expect("connector");
        let mut log = Vec::new();
        let r = probe_resolving(
            &c,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await;
        let err = match r {
            Ok(res) => panic!(
                "a 400 resolved to a {}-byte object instead of an error",
                res.probe.size
            ),
            Err(e) => e,
        };
        assert!(
            err.contains("400 Bad Request"),
            "the error must name the status the server gave: {err:?}"
        );
        // The host belongs to the caller, which prints it either side of this
        // message; carrying it here too put it on the line twice.
        assert!(
            !err.contains("127.0.0.1"),
            "the probe error must not repeat the host: {err:?}"
        );
    }

    /// Regression test for a sentinel-encoding bug: "the last 512 bytes" was
    /// encoded as `u64::MAX - 512` and recognised by a `> u64::MAX / 2` test,
    /// which mis-resolved and fetched bytes from the middle of the object
    /// (measured landing at exactly size/2 instead of the tail).
    #[test]
    fn suffix_range_resolves_to_the_actual_tail() {
        let size = 2_957_812u64;
        assert_eq!(
            RangeSpec::Suffix(512).resolve(size),
            Some((size - 512, size)),
            "a suffix range must be the LAST n bytes"
        );
        // A suffix larger than the object is the whole object, not an underflow.
        assert_eq!(RangeSpec::Suffix(size + 99).resolve(size), Some((0, size)));
    }

    #[test]
    fn closed_ranges_are_inclusive_as_http_spells_them() {
        // A range 0-1023 represents 1024 bytes.
        assert_eq!(RangeSpec::Closed(0, 1023).resolve(10_000), Some((0, 1024)));
        assert_eq!(
            RangeSpec::Closed(1000, 2023).resolve(10_000),
            Some((1000, 2024))
        );
        // Clamped to the object rather than requesting past the end.
        assert_eq!(
            RangeSpec::Closed(9990, 99_999).resolve(10_000),
            Some((9990, 10_000))
        );
    }

    #[test]
    fn open_ended_range_runs_to_the_end() {
        assert_eq!(RangeSpec::From(4096).resolve(10_000), Some((4096, 10_000)));
    }

    #[test]
    fn empty_and_degenerate_ranges_are_rejected() {
        assert_eq!(
            RangeSpec::From(10_000).resolve(10_000),
            None,
            "start at EOF is empty"
        );
        assert_eq!(RangeSpec::From(99_999).resolve(10_000), None);
        assert_eq!(RangeSpec::Closed(500, 499).resolve(10_000), None);
        assert_eq!(RangeSpec::Suffix(0).resolve(10_000), None);
    }

    #[test]
    fn a_parallel_digest_is_abandoned_only_when_it_cannot_fit_the_reorder_buffer() {
        const MIB: u64 = 1024 * 1024;
        assert!(
            !digest_cannot_finish(1024 * MIB, 1),
            "one span is sequential"
        );
        assert!(
            !digest_cannot_finish(4 * MIB, 4),
            "small objects still hash"
        );
        assert!(
            !digest_cannot_finish(16 * MIB, 2),
            "exactly the cap still fits"
        );
        assert!(digest_cannot_finish(16 * MIB + 2, 2));
        assert!(digest_cannot_finish(64 * MIB, 8));
        assert!(!digest_cannot_finish(u64::MAX, 1));
        assert!(
            digest_cannot_finish(u64::MAX, 16),
            "no overflow on huge sizes"
        );
    }

    /// `--no-save` must never create the output file.
    #[test]
    fn no_save_never_creates_a_file() {
        let mut job = default_job();
        job.no_save = true;
        assert_eq!(
            output_target(&job, "/tmp/hydra_should_not_exist.bin"),
            OutputTarget::Discard,
            "--no-save must resolve to a discarding sink, not a file to delete later"
        );
    }

    /// `--no-save --stdout` still needs storage: positioned writes land out of
    /// order, so the object is only correct once complete and cannot be streamed
    /// as it arrives. The temporary file is removed after streaming.
    #[test]
    fn no_save_with_stdout_still_needs_a_staging_file() {
        let mut job = default_job();
        job.no_save = true;
        job.to_stdout = true;
        assert_eq!(
            output_target(&job, "/tmp/hydra_stage.bin"),
            OutputTarget::Stdout("/tmp/hydra_stage.bin".into())
        );
    }

    #[test]
    fn a_plain_job_writes_a_file() {
        let job = default_job();
        assert_eq!(
            output_target(&job, "/tmp/hydra_plain.bin"),
            OutputTarget::File("/tmp/hydra_plain.bin".into())
        );
    }

    /// A scratch object for the span tests: `size` patterned bytes (modulus
    /// 251, a prime, so no page-aligned repeat can mask an offset error) in a
    /// private temp directory. Returns the directory (for cleanup) and the
    /// file path.
    fn span_scratch_object(size: usize) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("hydra_span_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("obj.bin");
        let mut whole = vec![0u8; size];
        for (i, b) in whole.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&p, &whole).unwrap();
        (dir, p, whole)
    }

    /// Range mode must deliver the SPAN, not the object's extent.
    ///
    /// The transfer writes `[lo, hi)` at true offsets inside a file the length of
    /// the whole object, so everything outside the span is a hole reading as
    /// zeros. `hydra -r 0-1023` delivered 34 041 bytes: 1 024 correct, 33 017
    /// zeros. Right prefix, plausible size, silently wrong file.
    #[test]
    fn a_prefix_range_is_cut_down_to_its_span() {
        let (dir, p, whole) = span_scratch_object(4096);

        extract_span(&p, 0, 1024).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(
            got.len(),
            1024,
            "must be the span's length, not the object's"
        );
        assert_eq!(
            got[..],
            whole[..1024],
            "must be the object's first 1024 bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A suffix range must move the span to the front, not merely truncate.
    #[test]
    fn a_suffix_range_moves_its_span_to_the_front() {
        let (dir, p, whole) = span_scratch_object(4096);

        extract_span(&p, 3584, 4096).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got.len(), 512);
        assert_eq!(
            got[..],
            whole[3584..],
            "a suffix range must yield the LAST bytes, at offset 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The copy is blocked rather than read whole, so a span longer than the
    /// block size must still come out byte-exact.
    #[test]
    fn a_span_larger_than_one_copy_block_is_exact() {
        // 3 MiB object, 2.5 MiB span starting mid-block: crosses the 1 MiB
        // copy block boundary at a non-multiple offset.
        let (dir, p, whole) = span_scratch_object(3 << 20);

        let (lo, hi) = (300_000u64, 300_000 + (5 << 19));
        extract_span(&p, lo, hi).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got.len() as u64, hi - lo);
        assert_eq!(got[..], whole[lo as usize..hi as usize]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ensure metadata probe follows redirects to the target object.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_probe_follows_a_redirect_to_the_object() {
        let net = hya_net::origin::OriginSet::new();
        let (real_port, _real) = net.spawn(64 * 1024, 1_000_000);
        let (hop_port, _hop) =
            net.spawn_redirecting(0, 1_000_000, &format!("http://127.0.0.1:{real_port}/obj"));

        // `--no-proxy`: the ambient http_proxy of a CI sandbox must not divert
        // an in-memory origin lookup.
        let args = crate::cli::Cli::parse_with_queries([
            "hydra",
            "--no-proxy",
            &format!("http://127.0.0.1:{hop_port}/obj"),
        ])
        .unwrap();
        let u = Url::parse(&args.urls[0]).unwrap();
        let (pr, final_url, _) = probe_public(&net, &u, &args)
            .await
            .expect("probe through the redirect");
        assert_eq!(pr.size, 64 * 1024, "size must come from the object");
        assert!(
            !pr.is_redirect(),
            "the reported response must be the final one"
        );
        assert_eq!(
            final_url.port, real_port,
            "must land on the redirect target"
        );
    }

    /// A `Location` naming the request just made is a loop, and the transfer
    /// path must say so. It used to spend `--max-redirs` round trips and then
    /// report "too many redirects", which describes a chain that is too long
    /// rather than one that never moves.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_transfer_path_names_a_self_redirect_a_loop() {
        let net = hya_net::origin::OriginSet::new();
        let (port, ctl) = net.spawn_redirecting(0, 1_000_000, "/obj");
        *ctl.redirect_to.lock().unwrap() = Some(format!("http://127.0.0.1:{port}/obj"));
        let u = Url::parse(&format!("http://127.0.0.1:{port}/obj")).expect("url");
        let t = u.to_target(None).expect("target");
        let mut log = Vec::new();
        let err = match probe_resolving(
            &net,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
        {
            Ok(r) => panic!("a loop resolved to a {}-byte object", r.probe.size),
            Err(e) => e,
        };
        assert!(err.contains("redirect loop"), "{err:?}");
        assert!(
            !err.contains("max-redirs"),
            "the budget was not the cause: {err:?}"
        );
        assert_eq!(
            ctl.requests.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the first answer already said everything the chain needed"
        );
    }

    /// A forwarding page that forwards to itself is the same loop written in
    /// HTML, and it is charged to the same chain: two such pages pointing at
    /// each other would otherwise spend the budget too.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_html_redirector_pointing_at_itself_is_a_loop() {
        let net = hya_net::origin::OriginSet::new();
        let (port, ctl) = net.spawn_html_redirecting(0, 1_000_000, "/obj");
        *ctl.html_redirect_to.lock().unwrap() = Some(format!("http://127.0.0.1:{port}/obj"));
        let u = Url::parse(&format!("http://127.0.0.1:{port}/obj")).expect("url");
        let t = u.to_target(None).expect("target");
        let mut log = Vec::new();
        let err = match probe_resolving(
            &net,
            &u,
            &t,
            &mut log,
            8,
            &CookieJar::new(),
            0,
            &no_proxy(),
            30.0,
            false,
        )
        .await
        {
            Ok(r) => panic!("a loop resolved to a {}-byte object", r.probe.size),
            Err(e) => e,
        };
        assert!(err.contains("redirect loop"), "{err:?}");

        // The reporting path takes the same chain and stops on it too.
        let args = crate::cli::Cli::parse_with_queries([
            "hydra",
            "--no-proxy",
            &format!("http://127.0.0.1:{port}/obj"),
        ])
        .unwrap();
        let (pr, _, _) = probe_public(&net, &u, &args).await.expect("a probe");
        assert!(pr.maybe_redirector(), "the page itself is what is left");
    }

    /// The reporting path prefers to describe what it reached over refusing,
    /// so a loop stops the chain and hands back the response in hand — after
    /// one request, not after the whole budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_reporting_path_stops_at_a_self_redirect() {
        let net = hya_net::origin::OriginSet::new();
        let (port, ctl) = net.spawn_redirecting(0, 1_000_000, "/obj");
        *ctl.redirect_to.lock().unwrap() = Some(format!("http://127.0.0.1:{port}/obj"));
        let args = crate::cli::Cli::parse_with_queries([
            "hydra",
            "--no-proxy",
            &format!("http://127.0.0.1:{port}/obj"),
        ])
        .unwrap();
        let u = Url::parse(&args.urls[0]).unwrap();
        let (pr, _, _) = probe_public(&net, &u, &args).await.expect("a probe");
        assert!(pr.is_redirect(), "the loop's own response is what is left");
        assert_eq!(ctl.requests.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn the_bench_takes_the_same_oath_the_seats_took() {
        // A reserve's bytes are spliced into the same file on substitution, so
        // a mirror that answers late must clear the same bar it would have
        // cleared at the front door. An earlier revision required only range
        // support here, which would have benched an unvalidated mirror.
        let probe = |size: u64, ranges: bool, validator: Option<&str>, weak: bool| hya_net::Probe {
            size,
            ranges,
            validator: validator.map(str::to_string),
            weak_validator: weak,
            last_modified: None,
            content_type: None,
            disposition: None,
            status: 200,
            location: None,
            raw_head: String::new(),
            raw_request: String::new(),
        };

        // With a document: the attested size is the whole test.
        assert!(bench_admission(
            Some(9),
            0,
            None,
            &probe(9, true, None, false)
        ));
        assert!(!bench_admission(
            Some(9),
            0,
            None,
            &probe(8, true, None, false)
        ));
        // ...but never without ranges: the first thing a substituted source is
        // asked for is a range.
        assert!(!bench_admission(
            Some(9),
            0,
            None,
            &probe(9, false, None, false)
        ));

        // Without a document: the pairwise gate, exactly as at the front door.
        let first = Some("\"etag-1\"");
        assert!(bench_admission(
            None,
            9,
            first,
            &probe(9, true, Some("\"etag-1\""), false)
        ));
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, Some("\"etag-2\""), false)),
            "a different validator is a different build"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(8, true, Some("\"etag-1\""), false)),
            "a different size is a different object"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, Some("\"etag-1\""), true)),
            "a weak validator may compare equal across different bytes"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, None, false)),
            "no validator, no proof"
        );
        // A first source with no strong validator authorises nothing late.
        assert!(!bench_admission(
            None,
            9,
            None,
            &probe(9, true, Some("\"x\""), false)
        ));
    }

    #[test]
    fn a_digest_spec_names_its_algorithm_or_defaults_to_sha256() {
        use hya_net::digest::Algo;
        // The two roads a spec arrives by: `--checksum`, normalised to
        // `algo:hex` at parse time, and a Metalink digest, which always carries
        // its prefix. A bare value is the historical `--checksum` form.
        assert_eq!(
            parse_digest_spec("sha512:AbC1"),
            Some((Algo::Sha512, "abc1".into())),
            "the algorithm is read and the hex lowercased"
        );
        assert_eq!(
            parse_digest_spec("f00d"),
            Some((Algo::Sha256, "f00d".into())),
            "a bare digest has always meant sha256 here"
        );
        assert_eq!(
            parse_digest_spec("whirlpool:aa"),
            None,
            "an algorithm this build cannot compute is 'not checked', never 'ok'"
        );
    }

    #[test]
    fn digest_file_computes_every_algorithm_a_document_publishes_and_refuses_crc() {
        use hya_net::digest::Algo;
        let p = std::env::temp_dir().join(format!("hydra-digest-file-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        // Published test vectors for "abc", so a wrong wiring of algorithm to
        // hasher is caught here rather than as a checksum mismatch against a
        // real mirror.
        for (algo, want) in [
            (Algo::Md5, "900150983cd24fb0d6963f7d28e17f72"),
            (Algo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                Algo::Sha256,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            assert_eq!(
                digest_file(&p, algo).as_deref(),
                Some(want),
                "{} vector",
                algo.as_str()
            );
        }
        assert_eq!(digest_file(&p, Algo::Sha512).map(|h| h.len()), Some(128));
        // A CRC is an error-detecting code, not a digest; "verified" against
        // one would mean almost nothing, and None reports it as unchecked.
        assert_eq!(digest_file(&p, Algo::Crc32), None);
        // A file that cannot be read is "could not check", not a wrong value.
        assert_eq!(
            digest_file(std::path::Path::new("/definitely/not/here"), Algo::Sha256),
            None
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn verify_scratch_names_are_unique_per_call_not_per_process() {
        // Two concurrent verifications sharing one scratch file made each read back the
        // OTHER object's bytes and report a mismatch on a byte-identical file. The name
        // must therefore vary within a process, not just across processes.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            assert!(
                seen.insert(scratch_name()),
                "a scratch name repeated within one process"
            );
        }
    }

    /// How a test origin answers, so one server covers the shapes the engine
    /// has to cope with rather than one copy of the accept loop per shape.
    #[derive(Clone, Default)]
    struct OriginOpts {
        /// Ignore `Range` and answer `200` without `Accept-Ranges`.
        no_ranges: bool,
        /// Reject any request with a Range header.
        reject_ranges: bool,
        /// Answer chunked, with no `Content-Length` anywhere.
        chunked: bool,
        /// Sleep this long between 16 KiB blocks of body.
        throttle: Option<std::time::Duration>,
        /// Refuse with `401` unless this exact header line is present.
        require_auth: Option<&'static str>,
        /// Refuse with `407` unless this exact header line is present.
        require_proxy_auth: Option<&'static str>,
        /// Verbatim extra response header lines.
        extra: &'static str,
    }

    /// An origin serving `body`, shaped by `opts`. Returns the port and a
    /// count of GET requests answered.
    async fn spawn_origin(
        body: std::sync::Arc<Vec<u8>>,
        opts: OriginOpts,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let gets = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        let counter = gets.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let (body, opts, counter) = (body.clone(), opts.clone(), counter.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let text = String::from_utf8_lossy(&head[..end]).to_string();
                        head.drain(..end);
                        let method = text.split_whitespace().next().unwrap_or("").to_string();
                        let has = |line: &str| text.lines().any(|l| l == line);
                        if opts.reject_ranges
                            && text
                                .lines()
                                .any(|l| l.to_ascii_lowercase().starts_with("range:"))
                        {
                            let _ = s
                                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                                .await;
                            return;
                        }
                        let refusal = match (opts.require_auth, opts.require_proxy_auth) {
                            (Some(a), _) if !has(a) => Some("401 Unauthorized"),
                            (_, Some(a)) if !has(a) => Some("407 Proxy Authentication Required"),
                            _ => None,
                        };
                        if let Some(status) = refusal {
                            let h = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                            if s.write_all(h.as_bytes()).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        if method == "GET" {
                            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        let total = body.len();
                        let range = text
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                            .filter(|_| !opts.no_ranges)
                            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()));
                        let (status, lo, hi) = match range.as_deref() {
                            Some(r) => {
                                let (a, b) = r.split_once('-').unwrap_or(("0", ""));
                                let lo: usize = a.parse().unwrap_or(0);
                                let hi: usize = b.parse().unwrap_or(total - 1);
                                ("206 Partial Content", lo, hi.min(total - 1))
                            }
                            None => ("200 OK", 0, total - 1),
                        };
                        let mut h = format!(
                            "HTTP/1.1 {status}\r\nETag: \"origin\"\r\n\
                             Last-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\n\
                             Content-Type: application/octet-stream\r\n{}",
                            opts.extra
                        );
                        if opts.chunked {
                            h.push_str("Transfer-Encoding: chunked\r\n");
                        } else {
                            h.push_str(&format!("Content-Length: {}\r\n", hi - lo + 1));
                        }
                        if !opts.no_ranges {
                            h.push_str("Accept-Ranges: bytes\r\n");
                        }
                        if range.is_some() {
                            h.push_str(&format!("Content-Range: bytes {lo}-{hi}/{total}\r\n"));
                        }
                        h.push_str("\r\n");
                        if s.write_all(h.as_bytes()).await.is_err() {
                            return;
                        }
                        if method == "HEAD" {
                            continue;
                        }
                        for block in body[lo..=hi].chunks(16 * 1024) {
                            if opts.chunked {
                                let frame = format!("{:x}\r\n", block.len());
                                if s.write_all(frame.as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                            if s.write_all(block).await.is_err() {
                                return;
                            }
                            if opts.chunked && s.write_all(b"\r\n").await.is_err() {
                                return;
                            }
                            if let Some(d) = opts.throttle {
                                tokio::time::sleep(d).await;
                            }
                        }
                        if opts.chunked && s.write_all(b"0\r\n\r\n").await.is_err() {
                            return;
                        }
                        if text
                            .lines()
                            .any(|line| line.eq_ignore_ascii_case("connection: close"))
                        {
                            return;
                        }
                    }
                });
            }
        });
        (port, gets)
    }

    fn patterned(n: usize) -> std::sync::Arc<Vec<u8>> {
        std::sync::Arc::new((0..n as u64).map(|i| (i % 251) as u8).collect())
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hydra_{tag}_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn job_at(port: u16, path: &str, out: Option<PathBuf>) -> Job {
        let mut job = default_job();
        job.urls = vec![format!("http://127.0.0.1:{port}{path}")];
        job.output = out;
        job.no_proxy = true;
        job
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_range_denial_never_sends_range_even_for_unknown_size() {
        for chunked in [false, true] {
            let body = patterned(100_003);
            let (port, gets) = spawn_origin(
                body.clone(),
                OriginOpts {
                    reject_ranges: true,
                    chunked,
                    ..Default::default()
                },
            )
            .await;
            let dir = scratch_dir("plugin_no_ranges");
            let out = dir.join("obj.bin");
            let mut job = job_at(port, "/obj.bin", Some(out.clone()));
            job.force_stream = true;
            job.conns = Some(8);
            let result = run_file(job).await;
            assert!(result.ok, "{:?}", result.note);
            assert_eq!(std::fs::read(&out).unwrap(), *body);
            assert_eq!(gets.load(std::sync::atomic::Ordering::Relaxed), 1);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// The reported failure: `-x 4` against a server that ignores `Range`
    /// opened four ranged GETs and three of them died on "server ignored
    /// Range and sent 200". One request from offset zero is the only shape
    /// such a server can answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_server_that_ignores_range_gets_exactly_one_request() {
        let body = patterned(1_200_000);
        let (port, gets) = spawn_origin(
            body.clone(),
            OriginOpts {
                no_ranges: true,
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("norange");
        let out = dir.join("obj.bin");
        let mut job = job_at(port, "/obj.bin", Some(out.clone()));
        job.conns = Some(4);
        let o = run(job).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(std::fs::read(&out).unwrap(), *body);
        assert_eq!(
            gets.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one GET, from offset zero"
        );
        assert_eq!(o.connections, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A stopped multi-connection transfer holds several disjoint spans. The
    /// sidecar must record those spans, not a prefix of the same total: the
    /// prefix would tell the next `-c` that bytes it never fetched are held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_interrupted_transfer_records_the_ranges_it_held_and_resumes_from_them() {
        let body = patterned(2_000_000);
        let (port, _) = spawn_origin(
            body.clone(),
            OriginOpts {
                throttle: Some(std::time::Duration::from_millis(15)),
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("held");
        let out = dir.join("obj.bin");
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Tick>();
        let mut job = job_at(port, "/obj.bin", Some(out.clone()));
        job.conns = Some(2);
        job.cancel = Some(cancel.clone());
        job.ticks = Some((0, tx));
        let handle = tokio::spawn(run(job));
        // Stop once both connections have visibly landed something.
        while let Some(t) = rx.recv().await {
            if t.conns.iter().filter(|c| c.pos > c.lo + 40_000).count() >= 2 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
        let o = handle.await.unwrap();
        assert!(!o.ok);
        assert_eq!(o.note.as_deref(), Some("interrupted"));

        let sc = Sidecar::load(&out).expect("a resume record must be written");
        assert!(
            sc.done.len() >= 2,
            "two connections hold two spans: {:?}",
            sc.done
        );
        let on_disk = std::fs::read(&out).unwrap();
        for (lo, hi) in &sc.done {
            assert_eq!(
                on_disk[*lo as usize..*hi as usize],
                body[*lo as usize..*hi as usize],
                "the record claims [{lo},{hi}) but those bytes never arrived"
            );
        }
        assert!(
            sc.done.iter().any(|(lo, _)| *lo > 0),
            "a multi-connection transfer does not hold one contiguous prefix: {:?}",
            sc.done
        );

        let (port2, _) = spawn_origin(body.clone(), OriginOpts::default()).await;
        let mut again = job_at(port2, "/obj.bin", Some(out.clone()));
        again.resume = true;
        again.conns = Some(2);
        let o = run(again).await;
        assert!(o.ok, "{:?}", o.note);
        assert!(o.resumed_from > 0, "the held spans must be reused");
        assert_eq!(std::fs::read(&out).unwrap(), *body);
        assert!(Sidecar::load(&out).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--stdout` used to stage under the URL's basename in the working
    /// directory, overwriting and then deleting an unrelated file of that
    /// name. It now stages in a temp path of its own, and in range mode it
    /// emits the span rather than the zero-padded extent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdout_stages_away_from_the_basename_and_emits_only_the_span() {
        assert!(
            stdout_stage_path()
                .to_string_lossy()
                .contains("hydra_stdout_"),
            "the staging name is hydra's own, never the URL's basename"
        );
        let body = patterned(64);
        let (port, _) = spawn_origin(body.clone(), OriginOpts::default()).await;
        let dir = scratch_dir("stdout");
        let bystander = dir.join("obj.bin");
        std::fs::write(&bystander, b"KEEP").unwrap();
        let mut job = job_at(port, "/obj.bin", None);
        job.output_dir = Some(dir.clone());
        job.to_stdout = true;
        job.range = Some(RangeSpec::Closed(8, 15));
        let o = run(job).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(o.size, 8, "the span is what was delivered");
        assert_eq!(
            std::fs::read(&bystander).unwrap(),
            b"KEEP",
            "an unrelated file of the same name must be untouched"
        );
        assert!(
            !dir.join("obj.bin.hydra").exists(),
            "no resume record for a staging file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown-size response took an early return that ignored every
    /// post-transfer flag: a wrong `--checksum` exited 0, `--no-save` left a
    /// file, `--remote-time` and `--etag-save` did nothing, and an existing
    /// file was overwritten without a word.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_size_response_still_honours_the_post_transfer_flags() {
        let body = patterned(150_000);
        let want = hya_net::digest::to_lower_hex(&Sha256::digest(&*body));
        let (port, _) = spawn_origin(
            body.clone(),
            OriginOpts {
                chunked: true,
                no_ranges: true,
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("nolen");

        // A wrong digest is a failure, and the ETag of bytes that failed is
        // not saved.
        let etag = dir.join("etag.txt");
        let mut wrong = job_at(port, "/obj.bin", Some(dir.join("wrong.bin")));
        wrong.checksum = Some(format!("sha256:{}", "0".repeat(64)));
        wrong.etag_save = Some(etag.clone());
        let o = run(wrong).await;
        assert!(!o.ok, "a checksum mismatch must fail: {o:?}");
        assert_eq!(o.checksum_ok, Some(false));
        assert!(!etag.exists(), "no ETag for a transfer that did not verify");

        // The right digest passes, the mtime comes from Last-Modified, and
        // the ETag is saved.
        let good_path = dir.join("good.bin");
        let mut good = job_at(port, "/obj.bin", Some(good_path.clone()));
        good.checksum = Some(format!("sha256:{want}"));
        good.remote_time = true;
        good.etag_save = Some(etag.clone());
        let o = run(good).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(o.checksum_ok, Some(true));
        assert_eq!(o.sha256.as_deref(), Some(want.as_str()));
        assert_eq!(std::fs::read(&good_path).unwrap(), *body);
        let mtime = std::fs::metadata(&good_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(mtime, 1_445_412_480, "--remote-time from Last-Modified");
        assert_eq!(std::fs::read_to_string(&etag).unwrap(), "\"origin\"");

        // A non-interactive run never overwrites the file it just made.
        let o = run(job_at(port, "/obj.bin", Some(good_path.clone()))).await;
        assert!(o.ok, "{:?}", o.note);
        assert!(o.output.ends_with("good.bin.1"), "{}", o.output);
        assert!(dir.join("good.bin.1").exists());
        let mut nc = job_at(port, "/obj.bin", Some(good_path.clone()));
        nc.no_clobber = true;
        let o = run(nc).await;
        assert!(
            o.ok && o.note.as_deref().is_some_and(|n| n.contains("exists")),
            "{o:?}"
        );

        // `--no-save` keeps nothing and still reports the digest.
        let mut discard = job_at(port, "/obj.bin", Some(dir.join("never.bin")));
        discard.no_save = true;
        discard.print_checksum = true;
        let o = run(discard).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(o.sha256.as_deref(), Some(want.as_str()));
        assert!(!dir.join("never.bin").exists());

        // `--range` cannot be honoured without a size and says so.
        let mut ranged = job_at(port, "/obj.bin", Some(dir.join("r.bin")));
        ranged.range = Some(RangeSpec::Closed(0, 9));
        let o = run(ranged).await;
        assert!(
            !o.ok && o.note.as_deref().is_some_and(|n| n.contains("--range")),
            "{o:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--etag-save` was written before the transfer, so a run that then
    /// failed its checksum left a validator claiming the object was held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_etag_is_saved_only_after_a_verified_transfer() {
        let body = patterned(40_000);
        let want = hya_net::digest::to_lower_hex(&Sha256::digest(&*body));
        let (port, _) = spawn_origin(body.clone(), OriginOpts::default()).await;
        let dir = scratch_dir("etag");
        let etag = dir.join("etag.txt");
        let mut bad = job_at(port, "/obj.bin", Some(dir.join("a.bin")));
        bad.checksum = Some(format!("sha256:{}", "1".repeat(64)));
        bad.etag_save = Some(etag.clone());
        assert!(!run(bad).await.ok);
        assert!(!etag.exists());
        let mut good = job_at(port, "/obj.bin", Some(dir.join("b.bin")));
        good.checksum = Some(format!("sha256:{want}"));
        good.etag_save = Some(etag.clone());
        assert!(run(good).await.ok);
        assert_eq!(std::fs::read_to_string(&etag).unwrap(), "\"origin\"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `-P d/sub` failed with "cannot create d/sub/name" when the directory
    /// did not exist; wget creates it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_output_directory_is_created_like_wget_does() {
        let body = patterned(5_000);
        let (port, _) = spawn_origin(body.clone(), OriginOpts::default()).await;
        let dir = scratch_dir("outdir").join("d").join("sub");
        let mut job = job_at(port, "/5000", None);
        job.output_dir = Some(dir.clone());
        let o = run(job).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(std::fs::read(dir.join("5000")).unwrap(), *body);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap().parent().unwrap());
    }

    /// `--content-disposition` was parsed and never read.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn content_disposition_names_the_file_only_when_asked() {
        let body = patterned(3_000);
        let (port, _) = spawn_origin(
            body.clone(),
            OriginOpts {
                extra: "Content-Disposition: attachment; filename=\"renamed-by-server.bin\"\r\n",
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("cd");
        let mut plain = job_at(port, "/cd", None);
        plain.output_dir = Some(dir.clone());
        assert!(run(plain).await.ok);
        assert!(
            dir.join("cd").exists(),
            "without the flag the URL names the file"
        );
        let mut named = job_at(port, "/cd", None);
        named.output_dir = Some(dir.clone());
        named.content_disposition = true;
        let o = run(named).await;
        assert!(o.ok, "{:?}", o.note);
        assert!(dir.join("renamed-by-server.bin").exists());
        // An explicit -O always wins.
        let mut explicit = job_at(port, "/cd", Some(dir.join("mine.bin")));
        explicit.content_disposition = true;
        assert!(run(explicit).await.ok);
        assert!(dir.join("mine.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `http://user:pass@host/` was parsed and the credentials went nowhere.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn userinfo_in_an_http_url_is_sent_as_basic_auth() {
        let body = patterned(2_000);
        let (port, _) = spawn_origin(
            body.clone(),
            OriginOpts {
                require_auth: Some("Authorization: Basic YWxpY2U6c2VjcmV0"),
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("auth");
        let mut bare = job_at(port, "/auth", Some(dir.join("bare.bin")));
        bare.urls = vec![format!("http://127.0.0.1:{port}/auth")];
        assert!(!run(bare).await.ok, "the origin must really be gated");
        let mut job = job_at(port, "/auth", Some(dir.join("auth.bin")));
        job.urls = vec![format!("http://alice:secret@127.0.0.1:{port}/auth")];
        let o = run(job).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(std::fs::read(dir.join("auth.bin")).unwrap(), *body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An HTTP proxy with `user:pass@` answered 407: the CLI built the
    /// target without the proxy's credentials.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_authenticated_http_proxy_receives_its_credentials() {
        let body = patterned(30_000);
        let (proxy_port, _) = spawn_origin(
            body.clone(),
            OriginOpts {
                require_proxy_auth: Some("Proxy-Authorization: Basic cHU6cHc="),
                ..Default::default()
            },
        )
        .await;
        let dir = scratch_dir("proxy");
        let mut job = default_job();
        // The origin is unreachable by name; everything goes to the proxy.
        job.urls = vec!["http://origin.invalid/obj.bin".into()];
        job.output = Some(dir.join("via.bin"));
        job.proxy = Some(format!("http://pu:pw@127.0.0.1:{proxy_port}"));
        let o = run(job).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(std::fs::read(dir.join("via.bin")).unwrap(), *body);
        let mut anon = default_job();
        anon.urls = vec!["http://origin.invalid/obj.bin".into()];
        anon.output = Some(dir.join("anon.bin"));
        anon.proxy = Some(format!("http://127.0.0.1:{proxy_port}"));
        assert!(!run(anon).await.ok, "the proxy must really require a login");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A proxy or origin that accepts the connection and never answers used
    /// to hang the probe forever, `-T` notwithstanding.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_probe_gives_up_after_the_timeout() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s);
            }
        });
        let mut job = job_at(
            port,
            "/never",
            Some(std::env::temp_dir().join("hydra_never.bin")),
        );
        job.timeout_s = 30.0;
        job.connect_timeout_s = Some(0.3);
        let t0 = Instant::now();
        let o = run(job).await;
        assert!(!o.ok);
        assert!(
            o.note.as_deref().is_some_and(|n| n.contains("--timeout")),
            "{:?}",
            o.note
        );
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn small_objects_are_not_split_into_more_requests_than_they_are_worth() {
        assert_eq!(connections_for_size(6, 31), 1);
        assert_eq!(connections_for_size(6, 1024), 1);
        assert_eq!(connections_for_size(6, MIN_BYTES_PER_CONNECTION * 2 - 1), 1);
        assert_eq!(connections_for_size(6, MIN_BYTES_PER_CONNECTION * 2), 2);
        assert_eq!(
            connections_for_size(6, 5 << 20),
            6,
            "a large object keeps the budget"
        );
        assert_eq!(connections_for_size(1, 0), 1);
    }

    #[test]
    fn a_sorted_file_stays_beside_where_it_landed() {
        assert_eq!(
            sorted_destination(Path::new("/abs/dir/f.mkv"), "Video"),
            PathBuf::from("/abs/dir/Video/f.mkv")
        );
        assert_eq!(
            sorted_destination(Path::new("f.mkv"), "Video"),
            PathBuf::from("./Video/f.mkv")
        );
        assert_eq!(
            sorted_destination(Path::new("out/f.mkv"), "Video"),
            PathBuf::from("out/Video/f.mkv")
        );
    }

    #[test]
    fn a_target_carries_the_url_and_proxy_credentials_exactly_once() {
        let policy = ProxyPolicy::new(Some("http://pu:pw@proxy.test:3128"), false);
        let pairs = targets_for(
            &["http://alice:secret@h.test/f".into()],
            &["X-Trace: 1".into()],
            "agent/1",
            &policy,
        )
        .unwrap();
        let t = &pairs[0].1;
        assert_eq!((t.host.as_str(), t.port), ("proxy.test", 3128));
        assert!(t.headers.contains(&"X-Trace: 1".to_string()));
        assert!(t
            .headers
            .contains(&"Authorization: Basic YWxpY2U6c2VjcmV0".to_string()));
        assert!(t
            .headers
            .contains(&"Proxy-Authorization: Basic cHU6cHc=".to_string()));
        let again = with_credentials(t.clone(), &pairs[0].0, &policy);
        assert_eq!(again.headers.len(), t.headers.len(), "no duplicates");
    }

    async fn ftp_run(
        job: &Job,
        out: &Path,
        origin: Arc<hya_net::ftp_origin::FtpOriginSet>,
    ) -> Outcome {
        let u = Url::parse(&job.urls[0]).unwrap();
        let mut p = progress_for(job, "object.bin", None).unwrap();
        ftp_fetch(job, &u, &mut p, out.to_string_lossy().to_string(), origin).await
    }

    /// FTP reported `ok: true` with `checksum_ok: null` for a wrong
    /// `--checksum`, wrote a file under `--no-save`, and ignored
    /// `--max-filesize` and `--range`.
    #[tokio::test]
    async fn the_ftp_path_verifies_and_refuses_like_the_http_path() {
        use hya_net::ftp_origin::{byte_at, FtpOriginSet};
        const SIZE: u64 = 300_000;
        let body: Vec<u8> = (0..SIZE).map(byte_at).collect();
        let want = hya_net::digest::to_lower_hex(&Sha256::digest(&body));
        let dir = scratch_dir("ftp");
        let template = {
            let mut j = default_job();
            j.urls = vec!["ftp://ftp.test/pub/object.bin".into()];
            j.no_proxy = true;
            j
        };

        let (origin, _) = FtpOriginSet::new(21, SIZE);
        let mut wrong = template.clone();
        wrong.checksum = Some(format!("sha256:{}", "0".repeat(64)));
        let o = ftp_run(&wrong, &dir.join("wrong.bin"), origin).await;
        assert!(!o.ok, "a wrong digest must fail: {o:?}");
        assert_eq!(o.checksum_ok, Some(false));

        let (origin, _) = FtpOriginSet::new(21, SIZE);
        let mut good = template.clone();
        good.checksum = Some(format!("sha256:{want}"));
        let o = ftp_run(&good, &dir.join("good.bin"), origin).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(o.checksum_ok, Some(true));
        assert_eq!(std::fs::read(dir.join("good.bin")).unwrap(), body);

        let (origin, _) = FtpOriginSet::new(21, SIZE);
        let mut discard = template.clone();
        discard.no_save = true;
        discard.print_checksum = true;
        let o = ftp_run(&discard, &dir.join("never.bin"), origin).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(o.sha256.as_deref(), Some(want.as_str()));
        assert!(!dir.join("never.bin").exists());

        let (origin, ctl) = FtpOriginSet::new(21, SIZE);
        let mut capped = template.clone();
        capped.max_filesize = Some(SIZE - 1);
        let o = ftp_run(&capped, &dir.join("capped.bin"), origin).await;
        assert!(
            !o.ok
                && o.note
                    .as_deref()
                    .is_some_and(|n| n.contains("--max-filesize")),
            "{o:?}"
        );
        assert_eq!(ctl.count("RETR"), 0, "refused before any byte");

        let (origin, _) = FtpOriginSet::new(21, SIZE);
        let mut ranged = template.clone();
        ranged.range = Some(RangeSpec::Closed(0, 9));
        let o = ftp_run(&ranged, &dir.join("r.bin"), origin).await;
        assert!(
            !o.ok && o.note.as_deref().is_some_and(|n| n.contains("--range")),
            "{o:?}"
        );

        // An existing file is not overwritten without a word: the run writes
        // beside it, and `--force` replaces it.
        let (origin, _) = FtpOriginSet::new(21, SIZE);
        let o = ftp_run(&template, &dir.join("good.bin"), origin).await;
        assert!(o.ok && o.output.ends_with("good.bin.1"), "{o:?}");
        let (origin, _) = FtpOriginSet::new(21, SIZE);
        std::fs::write(dir.join("forced.bin"), b"old").unwrap();
        let mut forced = template.clone();
        forced.force = true;
        let o = ftp_run(&forced, &dir.join("forced.bin"), origin).await;
        assert!(o.ok, "{:?}", o.note);
        assert_eq!(
            std::fs::read(dir.join("forced.bin")).unwrap().len() as u64,
            SIZE
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_request_that_never_answers_is_named_by_the_timeout() {
        let never = std::future::pending::<Result<(), std::io::Error>>();
        let e = within(0.05, "probe", never).await.unwrap_err();
        assert!(e.contains("probe") && e.contains("--timeout"), "{e}");
        let quick = within(1.0, "probe", async { Ok::<_, std::io::Error>(7) })
            .await
            .unwrap();
        assert_eq!(quick, 7);
    }

    fn cli(args: &[&str]) -> crate::cli::Cli {
        use clap::Parser as _;
        crate::cli::Cli::try_parse_from(args).expect("parses")
    }

    fn cancel_flag() -> Arc<std::sync::atomic::AtomicBool> {
        Arc::new(std::sync::atomic::AtomicBool::new(false))
    }

    #[test]
    fn from_cli_carries_the_transport_flags_the_queue_template_needs() {
        let a = cli(&[
            "hydra",
            "--limit-rate",
            "1M",
            "-H",
            "X-Trace: 1",
            "-u",
            "a:b",
            "--proxy",
            "http://p:1/",
            "--connect-timeout",
            "2",
            "--no-verbose",
            "http://h/a",
        ]);
        let j = Job::from_cli(&a, vec!["http://h/a".into()], &cancel_flag()).unwrap();
        assert_eq!(j.urls, vec!["http://h/a".to_string()]);
        assert_eq!(j.limit_rate, 1 << 20);
        assert!(j.headers.iter().any(|h| h == "X-Trace: 1"));
        assert!(j
            .headers
            .iter()
            .any(|h| h.starts_with("Authorization: Basic ")));
        assert_eq!(j.proxy.as_deref(), Some("http://p:1/"));
        assert_eq!(j.connect_timeout_s, Some(2.0));
        assert!(j.no_progress, "-nv drops the frame");
        assert!(j.cancel.is_some());
    }

    #[test]
    fn from_cli_json_implies_quiet_and_the_append_logfile_wins() {
        let a = cli(&[
            "hydra",
            "--json",
            "--logfile",
            "trunc.log",
            "--logfile-append",
            "keep.log",
            "--range",
            "-512",
            "http://h/a",
        ]);
        let j = Job::from_cli(&a, Vec::new(), &cancel_flag()).unwrap();
        assert!(j.quiet, "--json owns stdout");
        assert_eq!(
            j.logfile,
            Some((PathBuf::from("keep.log"), true)),
            "append is the non-destructive reading"
        );
        assert_eq!(j.range, Some(RangeSpec::Suffix(512)));
        assert!(j.urls.is_empty());
    }

    #[test]
    fn from_cli_maps_a_start_offset_and_the_negative_flags() {
        let a = cli(&[
            "hydra",
            "--start-pos",
            "1024",
            "-4",
            "--no-probe",
            "--no-follow-metalink",
            "--tries",
            "5",
            "--timeout",
            "9",
            "http://h/a",
        ]);
        let j = Job::from_cli(&a, Vec::new(), &cancel_flag()).unwrap();
        assert_eq!(j.range, Some(RangeSpec::From(1024)));
        assert_eq!(j.ip_family, hya_net::IpFamily::V4);
        assert!(!j.probe);
        assert!(!j.follow_metalink);
        assert_eq!(j.tries, 5);
        assert_eq!(j.timeout_s, 9.0);

        let bad = cli(&["hydra", "--range", "10-5", "http://h/a"]);
        let e = Job::from_cli(&bad, Vec::new(), &cancel_flag())
            .err()
            .expect("10-5 is an empty range");
        assert!(e.contains("--range"), "{e}");
    }

    #[test]
    fn range_specs_parse_every_spelling_and_refuse_the_empty_ones() {
        for (spec, want) in [
            ("0-1023", Some(RangeSpec::Closed(0, 1023))),
            (" 1024- ", Some(RangeSpec::From(1024))),
            ("-512", Some(RangeSpec::Suffix(512))),
            ("-0", None),
            ("10-5", None),
            ("abc", None),
            ("", None),
        ] {
            assert_eq!(RangeSpec::parse(spec), want, "{spec:?}");
        }
    }

    #[test]
    fn an_existing_file_is_offered_by_what_can_be_proven_about_it() {
        use crate::prompt::ResumeOffer;
        let sc = Sidecar {
            size: 1000,
            validator: Some("\"v1\"".into()),
            done: vec![(0, 400)],
            url: "http://h/a".into(),
        };
        assert_eq!(
            resume_offer(Some(&sc), true, 400, 1000, Some("\"v1\"")),
            ResumeOffer::Sound(400)
        );
        assert!(matches!(
            resume_offer(Some(&sc), true, 400, 2000, Some("\"v1\"")),
            ResumeOffer::Refused(why) if !why.is_empty()
        ));
        assert!(matches!(
            resume_offer(None, false, 400, 1000, None),
            ResumeOffer::Refused(why) if why.contains("byte ranges")
        ));
        assert_eq!(
            resume_offer(None, true, 1000, 1000, None),
            ResumeOffer::LooksComplete(1000)
        );
        assert_eq!(
            resume_offer(None, true, 400, 1000, None),
            ResumeOffer::Verifiable(400)
        );
        assert!(matches!(
            resume_offer(None, true, 0, 1000, None),
            ResumeOffer::Refused(why) if why.contains("empty")
        ));
    }
}

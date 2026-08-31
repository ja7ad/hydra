//! Command-line surface.
//!
//! Flags follow standard retriever conventions for muscle memory: `-O`, `-c`,
//! `--limit-rate`, `-x`, `-s`, `-H`, `-U`, `-q`, `-v` all behave predictably.

use clap::{Parser, Subcommand};
use hya_net::polite::{parse_rate, Politeness, DEFAULT_PER_HOST, DEFAULT_TOTAL};
use std::path::PathBuf;

/// Validate a `--header/-H` argument as an RFC 9110 field line.
///
/// This runs during argument parsing, which is the point: header values are
/// written onto the wire verbatim by the request builders in `hydra-net`, so a
/// malformed one produces a malformed request head. The observed failure was
/// `-H Age` — a bare word with no field-value — which left the server with
/// nothing to reply to and the client waiting on a response that never came. A
/// hang is a terrible error message for a mistake that is visible in the
/// argument itself.
///
/// The grammar enforced, from RFC 9110 §5:
///
///   field-line = field-name ":" OWS field-value OWS
///   field-name = token
///   token      = 1*tchar
///
/// A value need not follow a space (`X-Trace:1` is fine). What is refused is an
/// empty or non-token field-name, and any CR or LF anywhere — the last being
/// header injection rather than a typo, since a newline in a value lets a caller
/// append arbitrary extra fields to the request.
///
/// **A field-name with no VALUE is a QUERY, not a header to send** — `ETag` and
/// `ETag:` alike. Those are split out by [`Cli::parse_with_queries`] before this
/// parser runs, so everything reaching here carries a value and is destined for
/// the wire.
/// Validate a `--checksum` spec at parse time.
///
/// Accepts `sha256:<64 hex>` or a bare 64-hex digest, which is what the
/// comparison downstream understands: it strips a `sha256:` prefix and compares
/// the rest against the computed sha256.
///
/// Validating HERE rather than at comparison time is the point. An unparsable
/// value used to survive as far as the digest check, fail it, and be reported as
/// `checksum MISMATCH: the delivered bytes are not the bytes requested` — which
/// accuses the server of serving wrong bytes when the argument itself was
/// malformed. The exit code was right and the diagnosis was wrong, which is the
/// more expensive half. `--limit-rate` and `--range` are already validated at
/// parse time; this brings `--checksum` in line with them.
fn parse_checksum(s: &str) -> Result<String, String> {
    let t = s.trim();
    let (algo, hex) = match t.split_once(':') {
        Some((a, h)) => (Some(a.trim().to_ascii_lowercase()), h.trim()),
        None => (None, t),
    };
    if let Some(a) = &algo {
        if a != "sha256" {
            return Err(format!(
                "--checksum {s:?}: {a:?} is not a supported algorithm; this build compares \
                 sha256 only, written as 'sha256:<64 hex digits>' or as the bare digest"
            ));
        }
    }
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let what = if hex.len() != 64 {
            format!("{} hex digits, expected 64", hex.len())
        } else {
            "a non-hexadecimal character".into()
        };
        return Err(format!(
            "--checksum {s:?} is not a sha256 digest ({what}); write \
             'sha256:<64 hex digits>' or the bare digest"
        ));
    }
    Ok(t.to_ascii_lowercase())
}

fn parse_header(s: &str) -> Result<String, String> {
    // Checked first so that injection is reported as injection rather than as a
    // malformed name.
    if let Some(bad) = s.find(['\r', '\n']) {
        return Err(format!(
            "--header {s:?} contains a line break at byte {bad}; a CR or LF would inject \
             additional header fields into the request"
        ));
    }

    let Some((name, value)) = s.split_once(':') else {
        // A bare token is a query and is split out before parsing; anything else
        // without a colon is a malformed send-header.
        return Err(format!(
            "--header {s:?} has no ':' — to SEND a header write NAME: VALUE \
             (--header 'Age: 3600'); a bare NAME reads that header off the response instead"
        ));
    };

    if name.is_empty() {
        return Err(format!("--header {s:?} has an empty field-name"));
    }

    // RFC 9110 tchar. Notably excludes space, so `X Trace: 1` is refused rather
    // than silently sent as a field-name a server will reject.
    fn is_tchar(c: char) -> bool {
        c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
    }
    if let Some(c) = name.chars().find(|&c| !is_tchar(c)) {
        return Err(format!(
            "--header {s:?} has {c:?} in its field-name; a header name may contain only \
             letters, digits, and !#$%&'*+-.^_`|~ (no spaces)"
        ));
    }

    // Value keeps its interior shape; only surrounding optional whitespace is
    // stripped, exactly as a server would when parsing it.
    let _ = value;
    Ok(s.to_string())
}

impl Cli {
    /// Parse, separating `-H NAME` queries from `-H NAME: VALUE` sends.
    ///
    /// The two spellings mean opposite things and clap cannot tell them apart,
    /// because to clap they are both just values of `--header`. A bare token is a
    /// **query**: report the value of that header from the server's response. A
    /// token with a colon is a **send**.
    ///
    /// Queries are stripped from the argv handed to clap so the value parser only
    /// ever sees things destined for the wire. That keeps one rule — everything
    /// in `headers` is a well-formed field line — instead of a parser that must
    /// decide what each entry was for.
    pub fn parse_with_queries<I, S>(argv: I) -> Result<Cli, clap::Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let raw: Vec<String> = argv.into_iter().map(Into::into).collect();
        let mut kept: Vec<String> = Vec::with_capacity(raw.len());
        let mut queries: Vec<String> = Vec::new();

        // A query is a field-name with NO VALUE, written either way: `ETag` or
        // `ETag:`. Both mean "what did the server answer with", because neither
        // supplies anything to send — a trailing colon is punctuation, not a
        // value, and making the two spellings differ would be a trap.
        //
        // Anything else without a colon (`X Trace`) is a malformed send-header
        // and must still reach the parser, so the user gets an error rather than
        // a silently ignored argument.
        //
        // This does cost the ability to SEND a deliberately empty header
        // (such as `Name;`) — an unusual thing to want, and the tradeoff is
        // stated in the flag's help text.
        fn query_name(s: &str) -> Option<&str> {
            let name = match s.split_once(':') {
                // `NAME:` or `NAME:   ` — punctuation, no value.
                Some((n, v)) if v.trim().is_empty() => n,
                // `NAME: VALUE` — something to send.
                Some(_) => return None,
                // `NAME` — bare.
                None => s,
            };
            let name = name.trim();
            (!name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)))
            .then_some(name)
        }

        let mut i = 0usize;
        while i < raw.len() {
            let a = &raw[i];
            // `-H VALUE` / `--header VALUE`
            if a == "-H" || a == "--header" {
                if let Some(n) = raw.get(i + 1).and_then(|v| query_name(v)) {
                    queries.push(n.to_string());
                    i += 2;
                    continue;
                }
            }
            // `-HVALUE` and `--header=VALUE`
            if let Some(v) = a
                .strip_prefix("--header=")
                .or_else(|| a.strip_prefix("-H").filter(|v| !v.is_empty()))
            {
                if let Some(n) = query_name(v) {
                    queries.push(n.to_string());
                    i += 1;
                    continue;
                }
            }
            kept.push(a.clone());
            i += 1;
        }

        let mut cli = <Cli as Parser>::try_parse_from(kept)?;
        cli.header_queries = queries;
        Ok(cli)
    }
}

/// What several URLs mean.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum UrlMode {
    /// Each URL is a separate file; fetch them concurrently. Default.
    Same,
    /// Each URL is a separate file; fetch them one after another.
    Queue,
}

/// # Short flags where wget and curl disagree
///
/// wget and curl assign incompatible meanings to 15 of 19 common short flags, so no
/// single namespace can be compatible with both. `--compat=wget|curl` exists for exact
/// fidelity to either. Native mode still needs *an* assignment, and the rule used here
/// is: **whichever tool's meaning is expressible in a downloader that does not crawl
/// and does not upload wins the letter; the other meaning is reachable only under its
/// own personality.**
///
/// That rule decides every case without a coin flip:
///
/// | Flag | wget | curl | Native | Why |
/// |---|---|---|---|---|
/// | `-O` | output file | remote-name | **wget** | naming the output is the common need; curl's is `--remote-name` |
/// | `-o` | log file | output file | **wget** | `-O` is already the output; two spellings for it would be a trap |
/// | `-c` | continue | — | **wget** | curl spells it `-C -`, which `--compat=curl` accepts |
/// | `-q` | quiet | — | **wget** | curl's `-s` also maps to quiet |
/// | `-H` | span-hosts (crawl) | header | **curl** | hydra does not crawl, so the letter is free |
/// | `-U` | user-agent | — | **wget** | curl's `-A` is a visible alias, so both work |
/// | `-A` | accept-list (crawl) | user-agent | **curl** | crawl filters are meaningless here |
/// | `-t` | tries | — | **wget** | curl has no short form for retries |
/// | `-T` | timeout | upload-file | **wget** | hydra never uploads |
/// | `-r` | recursive (crawl) | range | **curl** | hydra does not crawl |
/// | `-L` | relative-only (crawl) | follow redirects | **curl** | only one is meaningful |
/// | `-i` | input file | include headers | **curl** | see the clustering rule below |
/// | `-N` | timestamping | — | **wget** | curl's `-R` also maps to remote-time |
/// | `-P` | directory-prefix | proxytunnel | **wget** | there is no tunnel flag to name |
/// | `-x` | force-directories | proxy | **curl** | directory forcing does not apply |
///
/// A second criterion overrides the first when they disagree: **a short flag that
/// takes a value cannot be clustered, so when one tool's meaning is boolean and the
/// other's takes a value, the boolean wins the letter.** `-i` is the case that proves
/// it. curl's `-i` is boolean, so `curl -iv` means include-headers plus verbose; wget's
/// `-i` takes a filename. Assigning wget's meaning made `hydra -iv <url>` parse `v` as
/// an input filename and fail with "cannot read --input-file v" — a silent
/// misinterpretation rather than an error about the flag. `--input-file` therefore has
/// no short form in native mode; `--compat=wget` still accepts `-i FILE`.
///
/// Under `--compat=wget` or `--compat=curl` the source tool's meaning always applies,
/// and a flag that cannot be honoured is refused with a reason rather than silently
/// ignored — an ignored `--limit-rate` can saturate a metered link.
#[derive(Parser, Debug)]
#[command(
    name = "hydra",
    version,
    // GPL-3.0 section 5(a) wants a distributed binary to state its own terms, and
    // the release profile strips symbols, so a user handed just the binary has no
    // other way to learn them. `--version` carries the notice in the GNU form
    // (`-V` still prints the bare version); `after_long_help` is deliberately not
    // used here because `after_help` below is the footer that actually renders.
    long_version = concat!(
        env!("CARGO_PKG_VERSION"), "\n",
        "Copyright (C) 2026 Javad Rajabzadeh\n",
        "License GPL-3.0-or-later: GNU GPL version 3 or later ",
        "<https://gnu.org/licenses/gpl.html>.\n",
        "This is free software: you are free to change and redistribute it.\n",
        "There is NO WARRANTY, to the extent permitted by law.\n\n",
        "The hydra-core and hydra-net libraries this binary links are\n",
        "MIT OR Apache-2.0, not GPL. Third-party terms: THIRD-PARTY-NOTICES.md.",
    ),
    about = "Adaptive file retriever: gets bytes from wherever they are, as fast as they can arrive",
    long_about = "hydra retrieves an object from one or more sources and keeps the work \
                  reassignable while it runs, instead of committing to a split up front.\n\n\
                  The scheduling result it is built on is that a byte range is not a \
                  commitment: a range request has a server-side start but a CLIENT-side \
                  end, so a laggard's range can be shrunk at zero cost and its tail handed \
                  to whichever source will finish first. A source that dies or throttles \
                  mid-transfer therefore does not strand its share, which is what a static \
                  split cannot avoid.\n\n\
                  That mechanism is not specific to HTTP or to downloading. Anything \
                  addressable by byte range, from any number of interchangeable replicas, \
                  is the same problem: mirrors, CDN edges, object stores, local caches. \
                  hydra is the retriever for that shape of problem, and the scheduler core \
                  is a library you can point at your own transport.",
    after_help = "EXAMPLES:\n  \
      hydra http://example.com/big.iso                       retrieve, measuring the useful concurrency\n  \
      hydra -O out.iso -x 4 http://a.example/f http://b.example/f    two replicas of one object\n  \
      hydra -c --limit-rate 2M http://example.com/big.iso    resume, capped at 2 MB/s\n  \
      hydra --checksum sha256:abc... http://example.com/f    verify what arrived\n  \
      hydra --json http://example.com/f                      machine-readable result\n  \
      hydra interactive                                      queue manager\n  \
      hydra bench --real                                     measure against a real network\n\n\
    COMPATIBILITY:\n  \
      wget and curl disagree on 15 of 19 common short flags (-O, -o, -c, -q, -H, -U,\n  \
      -t, -T, -r, -A, -L, -i, -N, -P, -x), so one namespace cannot serve both.\n  \
      Pick a dialect with --compat=wget|curl, or install named entry points:\n    \
        hydra compat-link --dry-run    show where the wget/curl links would go\n    \
        hydra compat-link              create them next to this binary\n  \
      The dialect comes from the name the binary is invoked as, so a link works\n  \
      only from a directory on $PATH that comes BEFORE the real curl/wget --\n  \
      compat-link checks that and says so. `ln -s hydra curl` does the same\n  \
      thing by hand, but only reaches hydra as ./curl unless that holds.\n  \
      Flags hydra cannot honour are REFUSED with a reason, never ignored."
)]
pub struct Cli {
    /// URLs to download.
    ///
    /// Several URLs mean several FILES by default (`--mode same|queue`). Pass `--mirrors`
    /// to treat them as interchangeable copies of ONE object and assemble byte ranges
    /// across them.
    #[arg(value_name = "URL")]
    pub urls: Vec<String>,

    /// Preferred rendition height for an HLS/DASH stream, e.g. `--quality 720`.
    ///
    /// The nearest rendition at or below it is used — never a larger one, so
    /// asking for 720 on a ladder that only has 1080 gives you the smallest
    /// available rather than a surprise 4K download.
    #[arg(long = "quality", value_name = "HEIGHT")]
    pub quality: Option<u32>,

    /// Container for a downloaded stream: `mp4` (default) or `ts`.
    ///
    /// MPEG-TS segments become MP4 through ffmpeg when it is installed;
    /// without it, ask for `ts` and get the assembled transport stream.
    #[arg(long = "container", value_name = "FMT", default_value = "mp4")]
    pub container: String,

    /// Report what the server says about a URL, then exit.
    ///
    /// For an ordinary object that is its size, type, and modification date;
    /// for a stream manifest it is the renditions on offer. Nothing is
    /// downloaded either way.
    ///
    /// Kept separate from `--list-streams` because the two differ on what a
    /// URL that is NOT a manifest means: here it is an ordinary answer,
    /// there it is a failed premise.
    #[arg(long = "inspect")]
    pub inspect: bool,

    /// List the renditions a stream manifest offers, then exit.
    ///
    /// A manifest is a few kilobytes, so this is the cheap way to see what a
    /// `--quality` is choosing between. A URL that turns out not to be a
    /// manifest is an error, not an invitation to download it.
    #[arg(long = "list-streams")]
    pub list_streams: bool,

    /// Record a LIVE stream for this many seconds, then finish the file.
    ///
    /// A live stream has no end of its own, so without this the only way to
    /// stop is Ctrl-C. Either way the result is a complete, playable file —
    /// stopping a recording is not an interruption, it is the end of it.
    #[arg(long = "record-seconds", value_name = "SECONDS")]
    pub record_seconds: Option<u64>,

    /// Write output to this file (`-O`).
    #[arg(short = 'O', long = "output", value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Continue a partially-downloaded file (`-c`).
    #[arg(short = 'c', long = "continue")]
    pub resume: bool,

    /// Connections per source (`-x`). Omit to measure the useful number.
    #[arg(short = 'x', long = "max-connection-per-server", value_name = "N")]
    pub connections: Option<usize>,

    /// Alias for -x.
    ///
    /// Declared here rather than translated by the compat layer: native long
    /// options pass through untouched, so the parser is the single place that
    /// defines the native namespace.
    #[arg(short = 's', long = "split", value_name = "N")]
    pub split: Option<usize>,

    /// Cap the aggregate transfer rate, e.g. 500k, 2M (`--limit-rate`).
    #[arg(long = "limit-rate", value_name = "RATE")]
    pub limit_rate: Option<String>,

    /// Total connections across all hosts.
    #[arg(long = "max-total-connections", value_name = "N", default_value_t = DEFAULT_TOTAL)]
    pub max_total: usize,

    /// One connection per host, for volunteer mirrors.
    #[arg(long = "polite")]
    pub polite: bool,

    /// Send a request header (`-H 'Name: value'`), or read one off the response
    /// (`-H Name` or `-H Name:`, which print the value and exit).
    ///
    /// A name with no value is a question, not a header to send, in either
    /// spelling — the trailing colon is punctuation. This means an intentionally
    /// empty request header (`-H 'Name;'`) cannot be sent; that is a rare
    /// need traded for the common one.
    ///
    /// Values are validated here rather than at send time: they are written onto
    /// the wire verbatim, so a malformed one produces a request a server never
    /// answers, which used to present as a hang.
    #[arg(
        short = 'H',
        long = "header",
        value_name = "LINE",
        value_parser = parse_header
    )]
    pub headers: Vec<String>,

    /// Response headers to report, from a bare `-H NAME` (no colon).
    ///
    /// Populated by [`Cli::parse_with_queries`], not by clap: `-H NAME: VALUE`
    /// sends a header, `-H NAME` asks what the server answered with. Kept
    /// separate because they are opposite directions on the wire.
    #[arg(skip)]
    pub header_queries: Vec<String>,

    /// User-Agent to send (`-U`).
    #[arg(
        short = 'U',
        visible_short_alias = 'A',
        long = "user-agent",
        value_name = "STRING",
        default_value = hya_net::DEFAULT_USER_AGENT
    )]
    pub user_agent: String,

    /// Retries per range before giving up on a source (`--tries`).
    #[arg(short = 't', long = "tries", value_name = "N", default_value_t = 4)]
    pub tries: u32,

    /// Per-request timeout in seconds (`--timeout`).
    #[arg(
        long = "timeout",
        short = 'T',
        value_name = "SECS",
        default_value_t = 30.0
    )]
    pub timeout: f64,

    /// Write a per-chunk digest manifest for what arrived.
    ///
    /// The manifest localizes future corruption to a single chunk, allowing
    /// targeted single-chunk refetches instead of re-downloading the entire file.
    #[arg(long = "emit-manifest", value_name = "FILE")]
    pub emit_manifest: Option<PathBuf>,

    /// Verify each chunk against a manifest as it arrives, refetching mismatches.
    ///
    /// A manifest read from a local path is trusted for verifying chunk integrity
    /// and guiding chunk refetches.
    #[arg(long = "chunk-digests", value_name = "FILE")]
    pub chunk_digests: Option<PathBuf>,

    /// Chunk grid for --emit-manifest, in bytes (default 4 MiB).
    #[arg(long = "chunk-size", value_name = "BYTES")]
    pub chunk_size: Option<u64>,

    /// Verify the finished file, e.g. --checksum sha256:abc...
    #[arg(long = "checksum", value_name = "HASH", value_parser = parse_checksum)]
    pub checksum: Option<String>,

    /// Suppress all output except errors (`-q`).
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Verbose; repeat for scheduler-level detail (-vv).
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Machine-readable result on stdout.
    /// How to treat several URLs on one command line.
    ///
    /// The default changed to `same` because treating extra URLs as MIRRORS is the
    /// surprising reading: `hydra <a> <b>` with two unrelated links reported "skipping
    /// b: size differs (cannot prove it serves the same bytes)" and fetched only the
    /// first, which looks like a bug even though the mirror logic was working exactly as
    /// designed. Standard convention is to treat multiple URLs as multiple files.
    ///
    /// Mirror assembly is still available and still safe — it just has to be asked for
    /// with `--mirrors`, because splitting one object across sources is only sound when
    /// every source provably serves identical bytes.
    #[arg(long = "mode", value_name = "MODE", default_value = "same")]
    pub mode: UrlMode,

    /// Treat every URL as a mirror of ONE object and assemble byte ranges across them.
    ///
    /// Requires that all sources agree on size and on a strong validator; any that
    /// disagree are dropped with a reason rather than silently mixed in.
    #[arg(long = "mirrors", conflicts_with = "mode")]
    pub mirrors: bool,

    /// Fetch without writing a file.
    ///
    /// The transfer runs normally — size, digest, format and timings are all measured —
    /// but the bytes are discarded instead of being saved. Useful for checking a
    /// mirror, computing a checksum, or seeing what a URL actually serves without
    /// leaving a file behind. Combine with `--json` for a machine-readable probe.
    #[arg(long = "no-save", visible_alias = "discard")]
    pub no_save: bool,

    /// Emit the result as JSON on stdout, and nothing else.
    ///
    /// Implies quiet: a machine-readable result must be the ONLY thing on stdout or it
    /// cannot be piped to a parser. Human progress, the summary line, and the format
    /// hint move to stderr-suppressed silence rather than being interleaved with the
    /// document — `hydra --json <url> | jq .size` has to work.
    #[arg(long = "json")]
    pub json: bool,

    /// Suppress the live progress display but keep summaries.
    #[arg(long = "no-progress")]
    pub no_progress: bool,

    /// Take -x on faith instead of measuring the useful concurrency.
    #[arg(long = "no-probe")]
    pub no_probe: bool,

    /// Measure the useful connection count even when -x is given, treating -x as
    /// a ceiling rather than a target.
    ///
    /// Without this, `-x N` opens N connections whether or not they help. On a
    /// saturated access link they do not: each one adds a request setup against
    /// capacity that is already committed, and a fixed multi-connection setting
    /// measured SLOWER than a single stream on four of five live objects. With
    /// this, the marginal-goodput search runs and stops where adding a connection
    /// stops paying, never exceeding N.
    #[arg(long = "adaptive", conflicts_with = "no_probe")]
    pub adaptive: bool,

    /// Command-line dialect: native, wget, or curl. Also inferred from the name
    /// the binary is invoked as.
    #[arg(long = "compat", value_name = "DIALECT")]
    pub compat: Option<String>,

    /// Write to stdout instead of a file (`--stdout`).
    #[arg(long = "stdout")]
    pub stdout: bool,

    /// Name the output from the URL's last path component.
    #[arg(long = "remote-name")]
    pub remote_name: bool,

    /// Skip the download if the output file already exists (`-nc`).
    #[arg(long = "no-clobber")]
    pub no_clobber: bool,

    /// Create the output directory if it does not exist (`--create-dirs`).
    #[arg(long = "create-dirs")]
    pub create_dirs: bool,

    /// Directory to save into (`-P`).
    #[arg(long = "output-dir", short = 'P', value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Probe headers and report, without retrieving the body (`--spider`).
    #[arg(long = "spider")]
    pub spider: bool,

    /// Print the server's response headers (`-S` / `-i`).
    /// Show the request and response headers.
    #[arg(
        long = "server-response",
        short = 'S',
        visible_short_alias = 'i',
        visible_alias = "include"
    )]
    pub server_response: bool,

    /// Overwrite an existing file without asking.
    ///
    /// Without this, an interactive run asks what to do when the output file already
    /// exists; a non-interactive one writes to a new name rather than risk destroying
    /// a completed download.
    #[arg(long = "force", short = 'f')]
    pub force: bool,

    /// Retrieve only this byte range, e.g. 0-1023, 1024-, or -512 (`-r`).
    ///
    /// `allow_hyphen_values` is required: a suffix range like `-512` otherwise
    /// parses as an unknown short flag rather than a value.
    #[arg(
        long = "range",
        short = 'r',
        value_name = "RANGE",
        allow_hyphen_values = true
    )]
    pub range: Option<String>,

    /// Start at this byte offset (`--start-pos`).
    #[arg(long = "start-pos", value_name = "OFFSET")]
    pub start_pos: Option<u64>,

    /// Exit non-zero on an HTTP error without writing a file (`--fail`).
    #[arg(long = "fail")]
    pub fail: bool,

    /// Show errors even when quiet (`--show-error`).
    #[arg(long = "show-error")]
    pub show_error: bool,

    /// Force the progress display on, even when not a terminal (`--show-progress`).
    #[arg(long = "show-progress")]
    pub show_progress: bool,

    /// Reduce output without silencing it (`-nv`).
    #[arg(long = "no-verbose")]
    pub no_verbose: bool,

    /// Log to this file instead of the terminal (`-o`).
    #[arg(long = "logfile", short = 'o', value_name = "FILE")]
    pub logfile: Option<PathBuf>,

    /// Append to the log file instead of truncating (`-a`).
    #[arg(long = "logfile-append", value_name = "FILE")]
    pub logfile_append: Option<PathBuf>,

    /// Set the local file's mtime from the server (`-N`).
    #[arg(long = "remote-time", short = 'N')]
    pub remote_time: bool,

    /// Name the output from a Content-Disposition header.
    #[arg(long = "content-disposition")]
    pub content_disposition: bool,

    /// Follow redirects (`-L`). On by default; the flag exists for scripts.
    #[arg(long = "location", short = 'L')]
    pub location: bool,

    /// Maximum redirects to follow (`--max-redirs`).
    #[arg(long = "max-redirs", value_name = "N", default_value_t = 8)]
    pub max_redirs: u32,

    /// Refuse an object larger than this many bytes (`--max-filesize`).
    #[arg(long = "max-filesize", value_name = "BYTES")]
    pub max_filesize: Option<u64>,

    /// Proxy to use. Defaults to $http_proxy / $all_proxy.
    ///
    /// Accepts `http://`, `socks4://`, `socks4a://`, `socks5://`, and `socks5h://`,
    /// with optional `user:pass@` credentials. SOCKS is a different mechanism, not a
    /// different spelling: an HTTP proxy rewrites requests or opens a CONNECT tunnel,
    /// while SOCKS forwards a raw TCP stream and lets the proxy resolve DNS — which is
    /// what ssh -D and Tor expose, and the only option when local DNS cannot resolve
    /// the origin at all.
    #[arg(long = "proxy", value_name = "URL")]
    pub proxy: Option<String>,

    /// Ignore any proxy in the environment (`--no-proxy`).
    #[arg(long = "no-proxy")]
    pub no_proxy: bool,

    /// Allow certificates that do not verify (`--insecure`).
    #[arg(long = "insecure")]
    pub insecure: bool,

    /// Resolve names to IPv4 only.
    #[arg(long = "ipv4", short = '4')]
    pub ipv4: bool,

    /// Resolve names to IPv6 only.
    ///
    /// Conflicts with `-4`: "IPv4 only AND IPv6 only" has no satisfiable reading,
    /// and silently picking one of them is how a flag comes to look like it works
    /// while doing nothing.
    #[arg(long = "ipv6", short = '6', conflicts_with = "ipv4")]
    pub ipv6: bool,

    /// Seconds to wait between retries (`--retry-delay`).
    #[arg(long = "retry-delay", value_name = "SECS", default_value_t = 0.5)]
    pub retry_delay: f64,

    /// Seconds to wait between separate URLs (`--wait`).
    #[arg(long = "wait", value_name = "SECS", default_value_t = 0.0)]
    pub wait: f64,

    /// Connection timeout in seconds.
    #[arg(long = "connect-timeout", value_name = "SECS")]
    pub connect_timeout: Option<f64>,

    /// Read URLs from a file, one per line (`--input-file`).
    #[arg(long = "input-file", value_name = "FILE")]
    pub input_file: Option<PathBuf>,

    /// Compare against an ETag stored in this file (`--etag-compare`).
    #[arg(long = "etag-compare", value_name = "FILE")]
    pub etag_compare: Option<PathBuf>,

    /// Save the object's ETag to this file (`--etag-save`).
    #[arg(long = "etag-save", value_name = "FILE")]
    pub etag_save: Option<PathBuf>,

    /// Retrieve several URLs concurrently (`-Z`). Distinct objects, not mirrors.
    #[arg(long = "parallel", short = 'Z')]
    pub parallel: bool,

    /// Maximum concurrent transfers with --parallel.
    #[arg(long = "parallel-max", value_name = "N", default_value_t = 4)]
    pub parallel_max: usize,

    /// Sort the finished file into a per-category subdirectory (Video, Music,
    /// Compressed, Documents, Programs, ...).
    ///
    /// The category comes from the payload's magic bytes, not from the extension
    /// or the server's Content-Type, both of which are frequently wrong.
    #[arg(long = "sort-by-type", short = 'y')]
    pub sort_by_type: bool,

    /// Queue file for interactive mode. Defaults to a per-user state directory.
    #[arg(long = "queue-file", value_name = "FILE")]
    pub queue_file: Option<PathBuf>,

    /// Render a representative multi-file frame and exit (documentation aid).
    #[arg(long = "demo-multi", hide = true)]
    pub demo_multi: bool,

    /// Render a representative progress frame and exit (documentation aid).
    #[arg(long = "demo-frame", hide = true)]
    pub demo_frame: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ParityCmd {
    /// Generate parity for a file, alongside its manifest.
    Make {
        /// The file to protect.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Manifest describing the file's chunk grid (from --emit-manifest).
        #[arg(long = "manifest", value_name = "FILE")]
        manifest: PathBuf,

        /// Number of parity shards. Repair succeeds iff damaged shards <= this.
        #[arg(long = "parity", value_name = "N", default_value_t = 2)]
        parity: usize,

        /// Where to write the parity. Defaults to FILE.parity.
        ///
        /// Prefer a SEPARATE DEVICE: drive checksum errors show strong spatial
        /// locality, so parity beside its data can die with it.
        #[arg(long = "out", value_name = "FILE")]
        out: Option<PathBuf>,

        /// Repair window in bytes. Memory is O(n*window), not O(file).
        #[arg(long = "window", value_name = "BYTES")]
        window: Option<u64>,
    },

    /// Repair a damaged file using its manifest and parity.
    ///
    /// The manifest supplies the erasure positions. Without them there is no
    /// repair: a decoder cannot tell a corrupt shard from a good one.
    Repair {
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[arg(long = "manifest", value_name = "FILE")]
        manifest: PathBuf,

        #[arg(long = "parity-file", value_name = "FILE")]
        parity_file: Option<PathBuf>,

        /// Report what would be repaired without writing.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Full-screen download manager with a queue.
    ///
    /// Its options are declared HERE rather than borrowed from the top level: a
    /// subcommand competing with variadic positional URLs is ambiguous in both
    /// orders (`hydra interactive --queue-file X` rejects the flag, and
    /// `hydra --queue-file X URL interactive` swallows the word "interactive" as
    /// a URL).
    Interactive {
        /// URLs to enqueue on startup.
        #[arg(value_name = "URL")]
        urls: Vec<String>,

        /// Queue file. Defaults to a per-user state directory.
        #[arg(long = "queue-file", value_name = "FILE")]
        queue_file: Option<PathBuf>,

        /// Work the queue with no screen, one log line per event.
        ///
        /// This is also how `b` (background) detaches: the UI re-execs itself in this
        /// mode in a new session, so transfers survive the UI exiting and the terminal
        /// closing. Without a real process to own them, "background" would just mean
        /// "stopped", which is what it used to mean.
        #[arg(long = "headless")]
        headless: bool,

        /// How many transfers run at once.
        #[arg(
            short = 'n',
            long = "max-active",
            value_name = "N",
            default_value_t = 2
        )]
        max_active: usize,
    },
    /// Generate or apply local Reed-Solomon parity for a verified file.
    ///
    /// For bitrot protection and offline repair when remote sources are unavailable.
    Parity {
        #[command(subcommand)]
        what: ParityCmd,
    },

    /// Report the checksums a server advertises, without downloading the object.
    ///
    /// A checksum is a function of the bytes, so this cannot compute one for bytes it has
    /// not received — no protocol lets a client ask a server to hash an object on demand
    /// (RFC 9530 defines `Want-Digest` for it; essentially nothing implements it). What
    /// this does is retrieve a digest the PUBLISHER already computed, from the response
    /// headers, a `.sha256`-style sidecar, or a `SHA256SUMS` manifest. That verifies the
    /// bytes against what the publisher claims; it does not verify the publisher.
    ///
    /// With `--verify <file>` it compares a local file against the advertised digest,
    /// which is the fast way to answer "is my copy current?" without refetching.
    Checksum {
        /// Object to inspect.
        #[arg(value_name = "URL")]
        urls: Vec<String>,

        /// Compare this local file against the advertised digest.
        #[arg(long = "verify", value_name = "FILE")]
        verify: Option<PathBuf>,

        /// Machine-readable output.
        #[arg(long = "json")]
        json: bool,

        /// Also try sidecar and manifest files, not just headers (one extra request each).
        #[arg(long = "sidecars", default_value_t = true)]
        sidecars: bool,

        /// Compute the digest by streaming the body when nothing is advertised.
        ///
        /// Off by default because it transfers the whole object, which is the opposite of
        /// what this subcommand is for; on, it is the honest fallback.
        #[arg(long = "download-if-needed")]
        download_if_needed: bool,
    },

    /// List the file formats this build recognises, with descriptions.
    ///
    /// Text by default; `--json` emits the same catalogue for a GUI to render as
    /// tooltips or a type filter.
    Formats {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// Show only one category (video, audio, image, archive, document,
        /// application, "disk image", font, data, markup).
        #[arg(long, value_name = "NAME")]
        category: Option<String>,
        /// Describe one format or extension and exit, e.g. `hydra formats --what tar.gz`.
        #[arg(long, value_name = "NAME")]
        what: Option<String>,
    },
    /// Measurement harnesses.
    Bench {
        /// Download real objects over the network.
        #[arg(long)]
        real: bool,
        /// Repetitions.
        #[arg(long, default_value_t = 3)]
        reps: usize,
        /// Which harness: memprofile | xval | ticksweep | sizesweep | detectab
        #[arg(long, default_value = "xval")]
        which: String,
    },

    /// Print a shell completion script to stdout.
    ///
    /// Generated from the same `clap` definition the parser itself uses, so a
    /// flag added to this file shows up in completions without a hand-written
    /// script to keep in sync. Pipe the output to wherever the shell loads
    /// completions from; `hydra install-completions` does that for the common
    /// locations automatically.
    Completions {
        /// bash | zsh | fish | elvish | powershell
        #[arg(value_enum)]
        shell: clap_complete::Shell,

        /// Command the script completes. Defaults to `hydra`; pass `hya` for
        /// the short name the installers link next to it, which needs its own
        /// script because a completion script names the command it serves.
        #[arg(long = "bin-name", value_name = "NAME")]
        bin_name: Option<String>,
    },

    /// Check whether a newer hydra release is available.
    ///
    /// Asks the release API for the latest version and, when it is newer than
    /// this binary, prints the release notes, the release page, and the direct
    /// download link for this OS and architecture. It never installs anything:
    /// the CLI's update story is "here is the link", by design — package
    /// managers and scripted installs own the actual replacement.
    Update {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,

        /// Include release candidates: report the newest `-rc` pre-release
        /// while it is ahead of the latest stable release. Once stable
        /// catches up, this reports stable again.
        #[arg(long)]
        beta: bool,
    },

    /// Install the `wget` / `curl` dialect entry points as links to this binary.
    ///
    /// The dialect is chosen from `argv[0]`, so a link named `wget` or `curl`
    /// pointing here is the whole mechanism. Doing it by hand with `ln -s` is
    /// where it goes wrong: the link lands in the current directory rather than
    /// a `$PATH` one, or the real tool sits in a directory that comes earlier in
    /// `$PATH` and keeps answering, or `ln` refuses because the name is taken.
    /// This places the links and then reports, per name, whether typing it will
    /// actually reach hydra.
    ///
    /// Existing files are never replaced without `--force` — the point is not to
    /// clobber a system `curl`.
    CompatLink {
        /// Where to put the links. Defaults to the directory this binary is in.
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Names to install. Repeatable; defaults to `wget` and `curl`.
        ///
        /// `hydra-wget` and `hydra-curl` are recognised too, and are the safe
        /// choice when the real tools must keep their names.
        #[arg(long = "name", value_name = "NAME")]
        names: Vec<String>,

        /// Replace an existing file of that name.
        #[arg(long)]
        force: bool,

        /// Report what would be done, touching nothing.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },

    /// Generate a completion script AND install it into the shell's standard
    /// completion directory.
    ///
    /// `hydra completions <shell>` only prints the script; this also picks a
    /// destination, writes the file, and prints the one remaining manual step
    /// (if any) to make the shell pick it up — most completion systems only
    /// rescan their directory on a new shell session, and none of them can be
    /// made to reload themselves from inside this process.
    InstallCompletions {
        /// bash | zsh | fish | elvish | powershell. Detected from `$SHELL` when omitted.
        #[arg(value_enum)]
        shell: Option<clap_complete::Shell>,

        /// Install for all users (e.g. `/etc/bash_completion.d`,
        /// `/usr/share/zsh/site-functions`) instead of the current user's
        /// home directory. Usually needs to be run with elevated privileges;
        /// this flag only picks the path, it does not escalate.
        #[arg(long)]
        system: bool,

        /// Print the destination path and script without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Command the script completes. Defaults to `hydra`; pass `hya` for
        /// the short name the installers link next to it, which needs its own
        /// script because a completion script names the command it serves.
        #[arg(long = "bin-name", value_name = "NAME")]
        bin_name: Option<String>,
    },
}

impl Cli {
    /// Connections per source the user asked for, honouring both spellings.
    pub fn requested_conns(&self) -> Option<usize> {
        self.connections.or(self.split)
    }

    pub fn politeness(&self) -> Politeness {
        if self.polite {
            Politeness::conservative()
        } else {
            Politeness {
                per_host: self.requested_conns().unwrap_or(DEFAULT_PER_HOST).max(1),
                total: self.max_total.max(1),
                ..Default::default()
            }
        }
    }

    /// Parsed `--limit-rate`, or 0 for unlimited.
    ///
    /// An unparsable value is a hard error rather than a silent fallback to
    /// unlimited: a user who asked for a cap and did not get one may saturate a
    /// metered link.
    pub fn rate_limit(&self) -> Result<u64, String> {
        match &self.limit_rate {
            None => Ok(0),
            Some(s) => {
                parse_rate(s).ok_or_else(|| format!("unparsable --limit-rate: {s} (try 500k, 2M)"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn both_connection_spellings_work() {
        let a = Cli::try_parse_from(["hydra", "-x", "6", "http://x/f"]).unwrap();
        assert_eq!(a.requested_conns(), Some(6));
        let b = Cli::try_parse_from(["hydra", "-s", "3", "http://x/f"]).unwrap();
        assert_eq!(b.requested_conns(), Some(3));
        let c = Cli::try_parse_from(["hydra", "http://x/f"]).unwrap();
        assert_eq!(c.requested_conns(), None, "absent means measure it");
    }

    /// The three concurrency modes must be distinguishable from the parsed args.
    ///
    /// This project's recurring defect is a flag that parses, exits cleanly, and
    /// does nothing — `--max-total-connections`, `-4`/`-6`, `--show-error`,
    /// `--logfile` and `--max-redirs` were all found that way. `--no-probe` was
    /// another: it set a `Job` field that carried `#[allow(dead_code)]` and was
    /// never read, so taking `-x` on faith was the only behaviour available and
    /// `--adaptive` was referenced in a comment as though it existed.
    #[test]
    fn the_three_concurrency_modes_are_distinguishable() {
        // Default: no number given, so the useful count is measured.
        let a = Cli::try_parse_from(["hydra", "http://x/f"]).unwrap();
        assert_eq!(a.requested_conns(), None);
        assert!(!a.no_probe && !a.adaptive);

        // Pinned: honour the number exactly, for a reproduction or a comparison
        // against another client.
        let b = Cli::try_parse_from(["hydra", "-x", "8", "http://x/f"]).unwrap();
        assert_eq!(b.requested_conns(), Some(8));
        assert!(!b.adaptive, "-x alone must not request measurement");

        // Ceiling: measure, but never exceed the number.
        let c = Cli::try_parse_from(["hydra", "--adaptive", "-x", "8", "http://x/f"]).unwrap();
        assert_eq!(c.requested_conns(), Some(8));
        assert!(c.adaptive);

        // Asking to measure and not to measure at once is a usage error, not a
        // silent precedence rule.
        assert!(
            Cli::try_parse_from(["hydra", "--adaptive", "--no-probe", "http://x/f"]).is_err(),
            "--adaptive and --no-probe contradict each other and must be refused"
        );
    }

    /// A malformed `--checksum` must be rejected as a bad ARGUMENT, not reported
    /// as wrong bytes.
    ///
    /// Regression test: an unparsable spec survived to the digest comparison,
    /// failed it, and printed "checksum MISMATCH: the delivered bytes are not the
    /// bytes requested" — accusing the server of serving the wrong object when the
    /// argument itself was the problem. Non-zero exit either way, but the wrong
    /// diagnosis is the expensive half.
    #[test]
    fn a_malformed_checksum_spec_is_refused_at_parse_time() {
        const GOOD: &str = "1e61c37477a1626458e36f7b1d82aa5c9b094fa4802892072e49de9c60c4c926";

        // Accepted: bare digest, prefixed digest, and mixed case normalised down.
        assert_eq!(parse_checksum(GOOD).unwrap(), GOOD);
        assert_eq!(
            parse_checksum(&format!("sha256:{GOOD}")).unwrap(),
            format!("sha256:{GOOD}")
        );
        assert_eq!(parse_checksum(&GOOD.to_uppercase()).unwrap(), GOOD);

        // Refused, each naming what is actually wrong.
        let e = parse_checksum("notarealformat").unwrap_err();
        assert!(
            e.contains("not a sha256 digest") && e.contains("14 hex digits"),
            "a short non-hex token must be reported as a bad digest: {e}"
        );
        let e = parse_checksum(&format!("md5:{GOOD}")).unwrap_err();
        assert!(
            e.contains("not a supported algorithm"),
            "an unsupported algorithm must say so: {e}"
        );
        // Right length, one character out of alphabet.
        let almost = format!("{}z", &GOOD[..63]);
        let e = parse_checksum(&almost).unwrap_err();
        assert!(
            e.contains("non-hexadecimal"),
            "a 64-char non-hex string must be reported as non-hex: {e}"
        );
        assert!(parse_checksum("").is_err());
    }

    #[test]
    fn wget_style_flags_parse() {
        let c = Cli::try_parse_from([
            "hydra",
            "-c",
            "-O",
            "out.bin",
            "--limit-rate",
            "2M",
            "-t",
            "9",
            "-q",
            "http://x/f",
        ])
        .unwrap();
        assert!(c.resume && c.quiet);
        assert_eq!(c.output.unwrap().to_str().unwrap(), "out.bin");
        assert_eq!(c.tries, 9);
        assert_eq!(c.limit_rate.as_deref(), Some("2M"));
    }

    /// A bare field-name is a QUERY: report that response header's value.
    ///
    /// `-H X-GitHub-Request-Id` asks what the server answered with; it does not
    /// send anything. The two spellings are opposite directions on the wire, so
    /// they are separated before clap sees them and land in different fields.
    #[test]
    fn a_bare_field_name_is_a_query_not_a_header_to_send() {
        let c = Cli::parse_with_queries(["hydra", "-H", "X-GitHub-Request-Id", "http://x/f"])
            .expect("a bare name is a query and must parse");
        assert_eq!(c.header_queries, vec!["X-GitHub-Request-Id".to_string()]);
        assert!(
            c.headers.is_empty(),
            "a query must not be sent as a request header"
        );
    }

    /// `NAME: VALUE` still sends, and the two can be combined in one invocation.
    #[test]
    fn a_valued_header_still_sends_and_composes_with_a_query() {
        let c = Cli::parse_with_queries([
            "hydra",
            "-H",
            "Authorization: Bearer t",
            "-H",
            "etag",
            "http://x/f",
        ])
        .expect("send + query together");
        assert_eq!(c.headers, vec!["Authorization: Bearer t".to_string()]);
        assert_eq!(c.header_queries, vec!["etag".to_string()]);
    }

    /// `ETag` and `ETag:` must mean the SAME thing.
    ///
    /// A trailing colon is punctuation, not a value; a user who writes one is
    /// asking the same question as one who does not. Reported directly: the two
    /// spellings behaved differently, which is a trap rather than a feature.
    #[test]
    fn a_trailing_colon_does_not_change_what_a_name_means() {
        for spelling in ["ETag", "ETag:", "ETag:   "] {
            let c = Cli::parse_with_queries(["hydra", "-H", spelling, "http://x/f"])
                .unwrap_or_else(|e| panic!("-H {spelling:?} must parse: {e}"));
            assert_eq!(
                c.header_queries,
                vec!["ETag".to_string()],
                "-H {spelling:?} must be the same query as a bare name"
            );
            assert!(
                c.headers.is_empty(),
                "-H {spelling:?} supplies no value, so there is nothing to send"
            );
        }
    }

    /// Every spelling clap accepts for the flag must split the same way.
    #[test]
    fn all_flag_spellings_split_queries_identically() {
        for argv in [
            vec!["hydra", "-H", "etag", "http://x/f"],
            vec!["hydra", "-Hetag", "http://x/f"],
            vec!["hydra", "--header", "etag", "http://x/f"],
            vec!["hydra", "--header=etag", "http://x/f"],
        ] {
            let c = Cli::parse_with_queries(argv.clone())
                .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            assert_eq!(c.header_queries, vec!["etag".to_string()], "{argv:?}");
            assert!(c.headers.is_empty(), "{argv:?}");
        }
    }

    /// A no-colon value that is NOT a bare token is still a malformed send.
    ///
    /// `X Trace` has a space, so it is neither a legal field-name nor a value
    /// anyone meant to send. Silently treating it as a query would hide a typo.
    #[test]
    fn a_no_colon_value_that_is_not_a_token_is_still_an_error() {
        assert!(Cli::parse_with_queries(["hydra", "-H", "X Trace", "http://x/f"]).is_err());
    }

    /// The reject list, each entry malformed for a different reason.
    ///
    /// A missing colon is deliberately absent from this list — it is normalized
    /// (see above). What remains are shapes with no sound interpretation.
    #[test]
    fn malformed_header_shapes_are_all_refused() {
        for bad in [
            "",                         // empty
            ": 1",                      // empty field-name
            "X Trace: 1",               // space inside the field-name
            "X-Trace\r\nX-Evil: 1",     // CRLF injection
            "X-Trace: ok\r\nX-Evil: 1", // CRLF injection after a valid pair
            "X-Trace\n: 1",             // bare LF
            "X(Trace): 1",              // non-token character in the field-name
        ] {
            assert!(
                Cli::try_parse_from(["hydra", "-H", bad, "http://x/f"]).is_err(),
                "-H {bad:?} must be refused"
            );
        }
    }

    /// The accept list. A header is not required to have a space after the colon,
    /// and an empty field-value is legal per RFC 9110.
    #[test]
    fn well_formed_headers_still_parse() {
        for good in [
            "Accept: */*",
            "X-Trace:1",
            "If-None-Match: \"abc\"",
            "Cookie: a=1; b=2",
        ] {
            let c = Cli::try_parse_from(["hydra", "-H", good, "http://x/f"])
                .unwrap_or_else(|e| panic!("-H {good:?} must parse, got: {e}"));
            assert_eq!(c.headers.len(), 1);
        }
    }

    #[test]
    fn multiple_urls_are_mirrors_and_headers_repeat() {
        let c = Cli::try_parse_from([
            "hydra",
            "-H",
            "Accept: */*",
            "-H",
            "X-Trace: 1",
            "http://a/f",
            "http://b/f",
            "http://c/f",
        ])
        .unwrap();
        assert_eq!(c.urls.len(), 3);
        assert_eq!(c.headers.len(), 2);
    }

    #[test]
    fn verbosity_counts_up() {
        assert_eq!(
            Cli::try_parse_from(["hydra", "http://x/f"])
                .unwrap()
                .verbose,
            0
        );
        assert_eq!(
            Cli::try_parse_from(["hydra", "-v", "http://x/f"])
                .unwrap()
                .verbose,
            1
        );
        assert_eq!(
            Cli::try_parse_from(["hydra", "-vv", "http://x/f"])
                .unwrap()
                .verbose,
            2
        );
    }

    #[test]
    fn bad_rate_limit_is_an_error_not_a_silent_unlimited() {
        let c = Cli::try_parse_from(["hydra", "--limit-rate", "fast", "http://x/f"]).unwrap();
        assert!(
            c.rate_limit().is_err(),
            "silently ignoring a cap can saturate a metered link"
        );
        let ok = Cli::try_parse_from(["hydra", "--limit-rate", "1.5M", "http://x/f"]).unwrap();
        assert_eq!(ok.rate_limit().unwrap(), 1_572_864);
        let none = Cli::try_parse_from(["hydra", "http://x/f"]).unwrap();
        assert_eq!(none.rate_limit().unwrap(), 0);
    }

    #[test]
    fn polite_mode_overrides_an_ambitious_x() {
        let c = Cli::try_parse_from(["hydra", "--polite", "-x", "16", "http://x/f"]).unwrap();
        assert_eq!(c.politeness().allow(16), 1);
    }

    #[test]
    fn formats_subcommand_parses_its_options() {
        let a = Cli::try_parse_from(["hydra", "formats"]).unwrap();
        match a.command {
            Some(Command::Formats {
                json,
                category,
                what,
            }) => {
                assert!(!json);
                assert!(category.is_none() && what.is_none());
            }
            _ => panic!("bare formats did not parse"),
        }
        let b = Cli::try_parse_from(["hydra", "formats", "--json", "--category", "video"]).unwrap();
        match b.command {
            Some(Command::Formats { json, category, .. }) => {
                assert!(json, "a GUI needs the machine-readable form");
                assert_eq!(category.as_deref(), Some("video"));
            }
            _ => panic!("formats --json did not parse"),
        }
        let c = Cli::try_parse_from(["hydra", "formats", "--what", ".tar.gz"]).unwrap();
        match c.command {
            Some(Command::Formats { what, .. }) => assert_eq!(what.as_deref(), Some(".tar.gz")),
            _ => panic!("formats --what did not parse"),
        }
    }

    #[test]
    fn subcommands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["hydra", "interactive"])
                .unwrap()
                .command,
            Some(Command::Interactive { .. })
        ));
        // The subcommand carries its own options, so this ordering must work — a
        // top-level variadic URL list would otherwise swallow the word after it.
        let iv = Cli::try_parse_from([
            "hydra",
            "interactive",
            "--queue-file",
            "/tmp/q.json",
            "-n",
            "3",
            "http://x/f",
        ])
        .unwrap();
        match iv.command {
            Some(Command::Interactive {
                urls,
                queue_file,
                headless,
                max_active,
            }) => {
                assert_eq!(urls, vec!["http://x/f".to_string()]);
                assert_eq!(queue_file.unwrap().to_str().unwrap(), "/tmp/q.json");
                assert_eq!(max_active, 3);
                assert!(!headless, "headless is opt-in; the UI is the default");
            }
            _ => panic!("interactive with its own options did not parse"),
        }
        let b = Cli::try_parse_from(["hydra", "bench", "--real", "--reps", "5"]).unwrap();
        match b.command {
            Some(Command::Bench { real, reps, .. }) => {
                assert!(real);
                assert_eq!(reps, 5);
            }
            _ => panic!("bench did not parse"),
        }
    }

    /// Clustered boolean shorts must parse as flags, not swallow the next letter.
    ///
    /// This is the bug `hydra -iv <url>` hit: `-i` took a value, so `v` became a
    /// filename and the failure was "cannot read --input-file v" rather than anything
    /// about flags. Any short whose counterpart in curl or wget is boolean must be
    /// boolean here too, or muscle-memory clustering silently misparses.
    #[test]
    fn boolean_shorts_cluster_the_way_curl_and_wget_users_expect() {
        for cluster in ["-iv", "-vi", "-Sv", "-vv", "-qi", "-Lv", "-fv", "-Nv"] {
            let c = Cli::try_parse_from(["hydra", cluster, "http://x/f"])
                .unwrap_or_else(|e| panic!("{cluster} must parse as clustered booleans, got: {e}"));
            assert!(
                c.input_file.is_none(),
                "{cluster} must not be read as an --input-file value"
            );
            assert_eq!(c.urls, vec!["http://x/f"], "{cluster} ate the URL");
        }
    }

    #[test]
    fn dash_i_means_include_headers_like_curl() {
        let c = Cli::try_parse_from(["hydra", "-i", "http://x/f"]).unwrap();
        assert!(c.server_response, "-i is curl's include-headers");
        let both = Cli::try_parse_from(["hydra", "-iv", "http://x/f"]).unwrap();
        assert!(both.server_response && both.verbose > 0);
    }

    #[test]
    fn input_file_keeps_its_long_form_only() {
        // The short belongs to the boolean; the long form is unambiguous.
        let c = Cli::try_parse_from(["hydra", "--input-file", "urls.txt"]).unwrap();
        assert_eq!(
            c.input_file.as_deref(),
            Some(std::path::Path::new("urls.txt"))
        );
        // wget's spelling still works under its own personality (compat layer test
        // covers the rewrite; here we only assert native does not claim -i for a value).
        assert!(
            Cli::try_parse_from(["hydra", "-i", "urls.txt", "http://x/f"])
                .map(|c| c.input_file.is_none())
                .unwrap_or(false)
        );
    }

    #[test]
    fn the_conflicting_short_flags_all_resolve_to_something() {
        // Every letter the two tools fight over must be defined in native mode, so a
        // user coming from either tool gets an action or a clear error — never
        // "unexpected argument".
        // The sample value is per-flag rather than a universal "1": `-H` now
        // validates its argument as a field line, so a placeholder that is not a
        // well-formed header would fail here for a reason this test is not about.
        // A rejected VALUE means the letter is defined, which is what is being
        // asserted; the shape of each value is covered by its own tests.
        for (flag, value) in [
            ("-O", Some("1")),
            ("-o", Some("1")),
            ("-c", None),
            ("-q", None),
            ("-H", Some("X-Sample: 1")),
            ("-U", Some("1")),
            ("-A", Some("1")),
            ("-t", Some("1")),
            ("-T", Some("1")),
            ("-r", Some("1")),
            ("-L", None),
            ("-i", None),
            ("-N", None),
            ("-P", Some("1")),
            ("-x", Some("1")),
        ] {
            let argv: Vec<&str> = match value {
                Some(v) => vec!["hydra", flag, v, "http://x/f"],
                None => vec!["hydra", flag, "http://x/f"],
            };
            let r = Cli::try_parse_from(argv);
            assert!(
                r.is_ok(),
                "{flag} is undefined in native mode: {:?}",
                r.err().map(|e| e.to_string())
            );
        }
    }
}

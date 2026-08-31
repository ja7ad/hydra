// Copyright (C) 2026 Javad Rajabzadeh
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, either version 3 of the License, or (at your option) any later
// version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with
// this program. If not, see <https://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later
//
// NOTE: the copyleft applies to this crate (the `hydra` binary) only. The
// `hydra-core` and `hydra-net` libraries are MIT OR Apache-2.0; see LICENSING.md.

//! hydra: multi-source download harness.
//!
//! `hydra memprofile` measures resident memory against object size. This is the
//! claim positioned writes make: memory is O(connections x buffer) and
//! INDEPENDENT of the object being downloaded, because bytes go straight to
//! their file offset and are never reassembled in RAM.

//! hydra — a multi-source downloader.
//!
//! The scheduler lives in `hydra-core` and knows nothing about I/O; the transport
//! lives in `hydra-net`. This crate is the product surface: argument parsing, the
//! live display, resume, verification, and the measurement harnesses that keep
//! the theory honest.

mod cli;
mod compat;
mod compat_link;
mod completions;
mod download;
mod progress;
mod prompt;
mod queue;
mod realnet;
mod stream;
mod tui;
mod update;
mod url;
mod xval;

use hya_core::{Scheduler, Source};
use hya_net::origin::OriginSet;
use hya_net::{run_transfer, Target};
use std::sync::Arc;

/// Peak resident set size in bytes, via `getrusage(RUSAGE_SELF)`.
///
/// `ps` is not executable in the sandbox this was measured in, and pulling in a
/// crate for one syscall is not worth it, so the struct is declared directly.
/// `ru_maxrss` is bytes on macOS and kilobytes on Linux.
#[cfg(unix)]
mod rusage {
    #[repr(C)]
    #[derive(Default)]
    pub struct Timeval {
        pub sec: i64,
        pub usec: i64,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct Rusage {
        pub utime: Timeval,
        pub stime: Timeval,
        pub maxrss: i64,
        pub ixrss: i64,
        pub idrss: i64,
        pub isrss: i64,
        pub minflt: i64,
        pub majflt: i64,
        pub nswap: i64,
        pub inblock: i64,
        pub oublock: i64,
        pub msgsnd: i64,
        pub msgrcv: i64,
        pub nsignals: i64,
        pub nvcsw: i64,
        pub nivcsw: i64,
    }

    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }

    /// Peak RSS in bytes.
    pub fn peak_rss() -> u64 {
        let mut ru = Rusage::default();
        let rc = unsafe { getrusage(0, &mut ru as *mut Rusage) };
        if rc != 0 {
            return 0;
        }
        let raw = ru.maxrss.max(0) as u64;
        if cfg!(target_os = "macos") {
            raw // bytes
        } else {
            raw * 1024 // kilobytes
        }
    }
}

#[cfg(unix)]
fn rss_bytes() -> u64 {
    rusage::peak_rss()
}

#[cfg(not(unix))]
fn rss_bytes() -> u64 {
    0
}

async fn memprofile() {
    println!(
        "size_mb,conns,elapsed_s,requests,rss_before_mb,rss_peak_mb,rss_delta_mb,throughput_mbps"
    );
    for size_mb in [4u64, 16, 64, 256, 512, 1024] {
        let size = size_mb * 1024 * 1024;
        let net = Arc::new(OriginSet::new());
        let mut targets = Vec::new();
        let mut sources = Vec::new();
        let per = [2usize, 2, 2];
        for _ in 0..3 {
            let (p, _c) = net.spawn(size, 400 * 1024 * 1024);
            targets.push(Target::direct("127.0.0.1", p, "/obj"));
            sources.push(Source {
                gamma_est: 2.0e8,
                delta_est: 0.005,
                ..Default::default()
            });
        }
        let out = std::env::temp_dir().join(format!("hydra_mem_{size_mb}.bin"));
        let outs = out.to_string_lossy().to_string();

        // getrusage reports a process high-water mark, so it only ever rises:
        // the delta across a transfer is what this measures, and a flat delta as
        // the object grows is the claim being tested.
        let before = rss_bytes();

        let sched = Scheduler::new(size, sources, &per).with_stall_timeout(5.0);
        let (elapsed, reqs) = run_transfer(net.clone(), targets, &per, size, &outs, sched)
            .await
            .expect("transfer must complete");
        let pkv = rss_bytes();
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        println!(
            "{size_mb},{},{elapsed:.3},{reqs},{:.1},{:.1},{:.1},{:.1}",
            per.iter().sum::<usize>(),
            mb(before),
            mb(pkv),
            mb(pkv.saturating_sub(before)),
            (size as f64 / (1024.0 * 1024.0)) / elapsed
        );
        let _ = std::fs::remove_file(&out);
    }
}

/// Print the recognised-format catalogue.
///
/// The point of `--json` is that a GUI should not have to scrape text to build its
/// tooltips or its file-type filter; it reads the same table the classifier uses.
fn print_formats(as_json: bool, category: Option<&str>, what: Option<&str>) {
    use hya_core::{catalogue, describe, from_extension_pub as by_ext};

    // A single lookup: "what is a .tar.gz?" is the question a user actually asks.
    if let Some(query) = what {
        let key = query.trim().trim_start_matches('.');
        // Try the format name first, then the extension. `from_extension` wants
        // something that LOOKS like a filename, so a bare extension is given a stem
        // — otherwise ".mkv" finds nothing while "x.mkv" resolves to matroska.
        let hit = describe(key)
            .map(|(l, d)| (key.to_string(), l, d))
            .or_else(|| {
                by_ext(&format!("query.{key}"))
                    .or_else(|| by_ext(key))
                    .and_then(|f| describe(f.name).map(|(l, d)| (f.name.to_string(), l, d)))
            });
        match hit {
            Some((name, label, text)) => {
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({"format": name, "label": label, "description": text})
                    );
                } else {
                    println!("{name}: {label}\n  {text}");
                }
            }
            None => {
                eprintln!("hydra: no format known as {query:?} (try `hydra formats`)");
            }
        }
        return;
    }

    let want = category.map(|c| c.trim().to_ascii_lowercase());
    let rows: Vec<_> = catalogue()
        .into_iter()
        .filter(|(_, cat, ..)| match &want {
            Some(w) => cat.as_str() == w.as_str(),
            None => true,
        })
        .collect();
    if rows.is_empty() {
        eprintln!("hydra: no formats in category {:?}", category.unwrap_or(""));
        return;
    }

    if as_json {
        let items: Vec<_> = rows
            .iter()
            .map(|(name, cat, ext, label, text)| {
                serde_json::json!({
                    "format": name,
                    "category": cat.as_str(),
                    "category_description": cat.description(),
                    "directory": cat.directory(),
                    "extension": ext,
                    "label": label,
                    "description": text,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"formats": items}))
                .unwrap_or_default()
        );
        return;
    }

    let mut current = "";
    for (name, cat, ext, label, text) in &rows {
        if cat.as_str() != current {
            current = cat.as_str();
            println!(
                "\n\x1b[1m{}\x1b[0m  \x1b[90m{}\x1b[0m",
                current.to_uppercase(),
                cat.description()
            );
        }
        let ext_s = if ext.is_empty() {
            String::new()
        } else {
            format!(".{ext}")
        };
        println!("  \x1b[36m{name:<12}\x1b[0m {ext_s:<10} {label}");
        println!("               \x1b[90m{text}\x1b[0m");
    }
}

/// The flag that stops this from being a plain, whole-object download, if
/// any. `None` means the command line asks for the ordinary thing.
///
/// ONE list, two callers, on purpose. The stream path uses it to decide
/// whether a manifest may be attempted at all, and `--inspect` uses it to
/// refuse rather than be silently dropped. Kept as two hand-maintained
/// copies, adding a mode flag to one and forgetting the other is what
/// silently ignores a flag the user typed — the exact bug the refusal
/// exists to prevent.
fn plain_download_conflict(args: &cli::Cli) -> Option<&'static str> {
    if args.spider {
        return Some("--spider");
    }
    if args.stdout {
        return Some("--stdout");
    }
    if args.no_save {
        return Some("--no-save");
    }
    if args.checksum.is_some() {
        return Some("--checksum");
    }
    if args.range.is_some() {
        return Some("--range");
    }
    if args.start_pos.is_some() {
        return Some("--start-pos");
    }
    None
}

/// Parse an HTTP byte range: `0-1023`, `1024-`, or `-512` (last 512 bytes).
///
/// Returns a [`download::RangeSpec`] rather than a sentinel-encoded pair. An
/// earlier version encoded "suffix of n bytes" as `u64::MAX - n` and recognised
/// it with a `> u64::MAX / 2` test; that silently mis-resolved and fetched the
/// wrong region. A magic number that must be recognised by a threshold is not a
/// representation, it is a latent bug.
fn parse_range(spec: &str) -> Option<download::RangeSpec> {
    use download::RangeSpec;
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

/// Size the runtime to the WORKLOAD, not to the machine.
///
/// `#[tokio::main]` with no arguments starts one worker thread per CPU core. On a
/// 12-core box that is 12 threads to service at most a couple of dozen sockets, and
/// the work each one does per wakeup is a 64 KiB read and a positioned write — far
/// too little to amortise being scheduled. The cost shows up as involuntary context
/// switches: measured on an 11 MB transfer, hydra was preempted 1516 times at `-x 8`
/// compared to single-threaded baseline of 33 (46x), while VOLUNTARY switches were essentially identical
/// (2127 vs 2145). Voluntary switches are "waited for I/O", which is the work;
/// involuntary switches are "used up a timeslice", which here is overhead.
///
/// **This is a resource-policy choice, not a measured optimisation.** The hypothesis
/// above was tested and rejected: interleaved over five repetitions the paired ratio was
/// 1.04 with Wilcoxon p = 0.81 and per-repetition deltas inconsistent in sign. An
/// earlier apparent 1516 → 921 improvement was between-job noise — the two builds ran in
/// separate jobs minutes apart.
///
/// The cap is kept because sizing a thread pool to the workload is defensible on its own
/// terms for a tool intended to run on servers, where many concurrent transfers share a
/// box and a per-core pool per process multiplies. Four workers: enough that TLS
/// handshakes and the digest can overlap the transfer, few enough that the pool is not
/// fighting itself; capped by the machine so a single-core VM does not get four.
///
/// The remaining CPU gap is still open and is being investigated at the
/// transport/executor boundary. See `docs/PERFORMANCE.md` for the hypotheses already
/// rejected, so they are not re-tried: pool width, the fixed-rate tick loop, per-arrival
/// channel traffic, task spawn/abort per request, and DNS resolution.
fn main() -> std::process::ExitCode {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2)
        .max(1);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async_main())
}

async fn async_main() -> std::process::ExitCode {
    // Translate the invoking dialect into canonical native options BEFORE parsing.
    // Different CLI personalities disagree on short flag semantics, so the personality
    // decides what `-O` means.
    let argv: Vec<String> = std::env::args().collect();
    let argv0 = argv.first().cloned().unwrap_or_else(|| "hydra".into());
    let rest: Vec<String> = argv.iter().skip(1).cloned().collect();
    let dialect = compat::detect(&argv0, &rest);
    let (canon, notes) = match compat::canonicalize(dialect, &rest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("hydra: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let mut full = vec![argv0];
    full.extend(canon);
    // `parse_with_queries`, not `try_parse_from`: a bare `-H NAME` asks what the
    // server answered with, which is the opposite direction from `-H NAME: VALUE`.
    let args = match cli::Cli::parse_with_queries(full.clone()) {
        Ok(a) => a,
        Err(e) => {
            // clap prints --help/--version to stdout with exit 0; real errors to stderr.
            let _ = e.print();
            return if e.use_stderr() {
                std::process::ExitCode::from(2)
            } else {
                std::process::ExitCode::SUCCESS
            };
        }
    };
    if args.verbose > 0 && dialect != compat::Personality::Native {
        eprintln!("hydra: {} compatibility mode", dialect.name());
    }
    for n in &notes {
        if args.verbose > 0 {
            eprintln!("hydra: {n}");
        }
    }

    match &args.command {
        Some(cli::Command::Parity { what }) => {
            std::process::exit(run_parity(what));
        }

        Some(cli::Command::Checksum {
            urls,
            verify,
            json,
            sidecars,
            download_if_needed,
        }) => {
            let mut list = args.urls.clone();
            list.extend(urls.clone());
            return checksum_report(
                &list,
                verify.as_deref(),
                *json,
                *sidecars,
                *download_if_needed,
                &args,
            )
            .await;
        }
        Some(cli::Command::Interactive {
            urls,
            queue_file,
            headless,
            max_active,
        }) => {
            let path = queue_file
                .clone()
                .or_else(|| args.queue_file.clone())
                .unwrap_or_else(queue::Queue::default_path);
            let max = (*max_active).clamp(1, 16);
            let mut list = args.urls.clone();
            list.extend(urls.clone());
            if let Err(e) = tui::run_with(path, list, max, *headless).await {
                eprintln!("hydra: {e}");
                return std::process::ExitCode::FAILURE;
            }
            return std::process::ExitCode::SUCCESS;
        }
        Some(cli::Command::Formats {
            json,
            category,
            what,
        }) => {
            print_formats(*json, category.as_deref(), what.as_deref());
            return std::process::ExitCode::SUCCESS;
        }
        Some(cli::Command::Update { json, beta }) => {
            return update::run(*json, *beta).await;
        }
        Some(cli::Command::Completions { shell, bin_name }) => {
            let bin = bin_name.as_deref().unwrap_or(completions::DEFAULT_BIN);
            print!("{}", completions::render(*shell, bin));
            return std::process::ExitCode::SUCCESS;
        }
        Some(cli::Command::CompatLink {
            dir,
            names,
            force,
            dry_run,
        }) => {
            let names: Vec<String> = if names.is_empty() {
                compat_link::DEFAULT_NAMES
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            } else {
                names.clone()
            };
            let (exe, plans) = match compat_link::plan(dir.as_deref(), &names) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("hydra: {e}");
                    return std::process::ExitCode::from(2);
                }
            };
            let mut failed = false;
            for p in &plans {
                let verb = match &p.action {
                    compat_link::Action::AlreadyLinked => {
                        println!("{} -> {} (already linked)", p.path.display(), exe.display());
                        None
                    }
                    compat_link::Action::Occupied(what) if !*force => {
                        eprintln!(
                            "hydra: {} already exists ({what}); --force replaces it, \
                             --dir puts the links elsewhere, or use --name hydra-{} \
                             to keep the real tool's name free",
                            p.path.display(),
                            p.name
                        );
                        failed = true;
                        None
                    }
                    compat_link::Action::Occupied(_) => Some("replaced"),
                    compat_link::Action::Create => Some("linked"),
                };
                let Some(verb) = verb else { continue };
                if *dry_run {
                    println!(
                        "would have {verb} {} -> {}",
                        p.path.display(),
                        exe.display()
                    );
                } else if let Err(e) = compat_link::apply(p, &exe, *force) {
                    eprintln!("hydra: {e}");
                    failed = true;
                    continue;
                } else {
                    println!("{verb} {} -> {}", p.path.display(), exe.display());
                }
            }
            // Placing the link is the easy half; being the name the shell
            // actually resolves is the half that silently fails.
            for p in &plans {
                if let Some(note) = compat_link::shadow_note(p) {
                    eprintln!("hydra: {note}");
                }
            }
            return if failed {
                std::process::ExitCode::FAILURE
            } else {
                std::process::ExitCode::SUCCESS
            };
        }
        Some(cli::Command::InstallCompletions {
            shell,
            system,
            dry_run,
            bin_name,
        }) => {
            let bin = bin_name.as_deref().unwrap_or(completions::DEFAULT_BIN);
            let shell = match shell.or_else(completions::detect_shell) {
                Some(s) => s,
                None => {
                    eprintln!(
                        "hydra: could not detect a shell from $SHELL; pass one explicitly, \
                         e.g. `hydra install-completions zsh`"
                    );
                    return std::process::ExitCode::from(2);
                }
            };
            match completions::install(shell, *system, *dry_run, bin) {
                Ok(dest) => {
                    if *dry_run {
                        println!(
                            "would install {shell} completions for {bin} to {}",
                            dest.path.display()
                        );
                    } else {
                        println!(
                            "installed {shell} completions for {bin} to {}",
                            dest.path.display()
                        );
                    }
                    if let Some(step) = dest.remaining_step {
                        println!("remaining step: {step}");
                    }
                    return std::process::ExitCode::SUCCESS;
                }
                Err(e) => {
                    eprintln!("hydra: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
        Some(cli::Command::Bench { real, reps, which }) => {
            if *real {
                realnet::bench(*reps).await;
            } else {
                match which.as_str() {
                    "memprofile" => memprofile().await,
                    "xval" => xval::cross_validate().await,
                    "ticksweep" => xval::tick_sweep().await,
                    "sizesweep" => xval::size_sweep().await,
                    "detectab" => xval::detect_ab(*reps).await,
                    other => {
                        eprintln!("hydra: unknown harness {other}");
                        return std::process::ExitCode::from(2);
                    }
                }
            }
            return std::process::ExitCode::SUCCESS;
        }
        None => {}
    }

    if args.demo_multi {
        progress::demo_multi();
        return std::process::ExitCode::SUCCESS;
    }
    if args.demo_frame {
        progress::demo_frame();
        println!();
        tui::demo_screen();
        return std::process::ExitCode::SUCCESS;
    }
    // -i / --input-file: URLs from a file, one per line, '#' comments allowed.
    let mut urls = args.urls.clone();
    if let Some(path) = &args.input_file {
        match std::fs::read_to_string(path) {
            Ok(body) => urls.extend(
                body.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string),
            ),
            Err(e) => {
                eprintln!("hydra: cannot read --input-file {}: {e}", path.display());
                return std::process::ExitCode::from(2);
            }
        }
    }
    if urls.is_empty() {
        eprintln!("hydra: no URL given (try --help)");
        return std::process::ExitCode::from(2);
    }
    // `-H NAME` (no colon) asks what the server answered with. Answer it and
    // stop: the question is about the response head, so downloading the body
    // would be work the user did not ask for. Output is the VALUE alone, one per
    // line, so it composes with `$(...)` and a pipe.
    if !args.header_queries.is_empty() {
        return report_header_values(&urls, &args).await;
    }

    let limit_rate = match args.rate_limit() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("hydra: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // --range narrows the retrieval to a byte interval.
    let range = match args.range.as_deref() {
        None => args.start_pos.map(download::RangeSpec::From),
        Some(spec) => match parse_range(spec) {
            Some(r) => Some(r),
            None => {
                eprintln!("hydra: unparsable --range: {spec} (try 0-1023, 1024-, or -512)");
                return std::process::ExitCode::from(2);
            }
        },
    };

    // Several URLs mean several FILES unless mirror assembly is asked for. Building one
    // job per URL rather than one job with many sources is what makes that true.
    let multi = urls.len() > 1 && !args.mirrors;

    // `--inspect` and `--list-streams` ask a QUESTION about one object. When
    // the rest of the command line makes that question unanswerable, dropping
    // the flag and carrying on turns "tell me about this" into "download
    // this" — writing files the user never asked for. Say what clashed.
    //
    // Only flags the user TYPED are checked here. The manifest auto-detect
    // below falls back to a plain download on purpose: nobody asked it a
    // question, so it has nothing to refuse.
    if args.inspect || args.list_streams {
        let flag = if args.inspect {
            "--inspect"
        } else {
            "--list-streams"
        };
        if let Some(other) = plain_download_conflict(&args) {
            eprintln!("hydra: {flag} cannot be combined with {other}");
            return std::process::ExitCode::from(2);
        }
        if multi {
            eprintln!(
                "hydra: {flag} reports on one URL at a time, but {} were given \
                 (pass one, or --mirrors if they are mirrors of the same object)",
                urls.len()
            );
            return std::process::ExitCode::from(2);
        }
    }

    // A manifest is not a file, so it cannot go through the range scheduler.
    // Only URLs that claim to be one are tried, and only in the plain
    // download mode: --spider, --stdout, --checksum and friends all ask a
    // question about a single object that a segmented stream cannot answer.
    let plain = plain_download_conflict(&args).is_none();
    // Either flag forces the attempt: both owe an answer that a plain
    // download cannot give.
    if !multi
        && plain
        && (args.list_streams || args.inspect || stream::looks_like_manifest(&urls[0]))
    {
        let sjob = stream::Job {
            url: urls[0].clone(),
            output: args.output.clone(),
            output_dir: args.output_dir.clone(),
            quality: args.quality,
            container: args.container.clone(),
            headers: args.headers.clone(),
            user_agent: args.user_agent.clone(),
            limit_rate,
            quiet: args.quiet || args.json,
            no_progress: args.no_progress,
            insecure: args.insecure,
            list: args.list_streams || args.inspect,
            record_seconds: args.record_seconds,
            // The same `-n` that governs connections for a file governs
            // segments in flight for a stream.
            conns: args.requested_conns().unwrap_or(0),
        };

        // `--adaptive` is about ranged file downloads; saying so beats
        // silently ignoring a flag the user typed.
        if args.adaptive && !args.quiet {
            eprintln!("hydra: --adaptive does not apply to streams; using -x segments in flight");
        }
        match stream::run(sjob.clone()).await {
            // Not a manifest. What that means depends on which flag asked:
            // `--inspect` still owes an answer about the object, while
            // `--list-streams` asked a question whose premise just failed.
            // Neither may quietly start a download nobody requested.
            stream::Verdict::NotAManifest if args.inspect => {
                return match stream::inspect_file(&sjob).await {
                    Ok(()) => std::process::ExitCode::SUCCESS,
                    Err(e) => {
                        eprintln!("hydra: {e}");
                        std::process::ExitCode::FAILURE
                    }
                };
            }
            stream::Verdict::NotAManifest if args.list_streams => {
                eprintln!("hydra: {} is not an HLS or DASH manifest", urls[0]);
                return std::process::ExitCode::FAILURE;
            }
            // Not a stream, and nobody asked a question: download it.
            stream::Verdict::NotAManifest => {}
            stream::Verdict::Listed | stream::Verdict::Done { .. } => {
                return std::process::ExitCode::SUCCESS
            }
            stream::Verdict::Failed(e) => {
                eprintln!("hydra: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let job = download::Job {
        ticks: None,
        urls: if multi {
            vec![urls[0].clone()]
        } else {
            urls.clone()
        },
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
        checksum: args.checksum.clone(),
        headers: args.headers.clone(),
        user_agent: args.user_agent.clone(),
        verbose: args.verbose,
        // --json implies quiet: stdout carries the document, nothing else.
        quiet: args.quiet || args.json,
        no_progress: args.no_progress,
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
        insecure: args.insecure,
        force: args.force,
        no_save: args.no_save,
        emit_manifest: args.emit_manifest.clone(),
        chunk_digests: args.chunk_digests.clone(),
        chunk_size: args.chunk_size,
    };
    if multi {
        return run_many(job, &urls, args.mode, args.json, args.quiet).await;
    }
    let out = download::run(job).await;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    }
    if out.ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Report the digests a server advertises for each URL, without transferring the body.
///
/// Order of attempts matches descending cost and ascending desperation: response headers
/// (one HEAD, no body), then sidecar/manifest files (one small GET each), then — only if
/// asked — streaming the object to compute the digest locally, which defeats the purpose
/// but is the honest fallback when nothing is published.
async fn checksum_report(
    urls: &[String],
    verify: Option<&std::path::Path>,
    json: bool,
    sidecars: bool,
    download_if_needed: bool,
    args: &cli::Cli,
) -> std::process::ExitCode {
    use hya_net::digest::{self, Advertised};
    let conn = match hya_net::TlsCapableConnector::with_insecure(args.insecure) {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            eprintln!("hydra: tls setup failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut records = Vec::new();
    let mut all_ok = true;

    for u in urls {
        let Some(parsed) = crate::url::Url::parse(u) else {
            eprintln!("hydra: cannot parse {u}");
            all_ok = false;
            continue;
        };
        // Kept for the sidecar fetches below; the metadata probe resolves its
        // own proxy per redirect hop.
        let px = crate::download::proxy_for_public(&parsed, args.proxy.as_deref(), args.no_proxy);

        let mut found: Vec<Advertised> = Vec::new();
        let mut size: Option<u64> = None;
        let mut validator: Option<String> = None;

        // 1. Headers: free apart from the round trip we already need for size.
        // Follow redirects first — a 302's headers carry no size, no validator,
        // and no digest, so probing the hop instead of the object reported
        // `size: null` for anything served through a CDN redirect.
        match crate::download::probe_public(conn.as_ref(), &parsed, args).await {
            Ok((pr, _final_url)) => {
                size = Some(pr.size).filter(|s| *s > 0);
                validator = pr.validator.clone();
                found.extend(digest::from_headers(&pr.raw_head));
                // An S3-style store puts the MD5 in the ETag for single-part uploads.
                // Offered only when nothing authoritative was published, and always
                // labelled unconfirmed: a mismatch here may mean the convention was not
                // followed rather than that the bytes are wrong.
                if found.is_empty() {
                    if let Some(c) = digest::md5_candidate_from_etag(&pr.raw_head) {
                        found.push(c);
                    }
                }
            }
            Err(e) => {
                eprintln!("hydra: probe failed for {u}: {e}");
                all_ok = false;
            }
        }

        // 2. Sidecars and manifests: cheap, and by far the most common in practice.
        if sidecars && found.is_empty() {
            for (path, algo) in digest::sidecar_candidates(&parsed.path) {
                let mut side = parsed.clone();
                side.path = path.clone();
                let Ok(t2) = side.to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p))) else {
                    continue;
                };
                if let Ok(body) = hya_net::fetch_small(conn.as_ref(), &t2, 1 << 20).await {
                    let text = String::from_utf8_lossy(&body);
                    let name = parsed
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&parsed.path)
                        .to_string();
                    if let Some(mut d) = digest::from_sidecar(&text, algo, &name) {
                        d.source = path.clone();
                        found.push(d);
                        break;
                    }
                }
            }
        }

        // 3. Last resort, and only on request: transfer the object and hash it.
        let mut computed = None;
        if found.is_empty() && download_if_needed {
            eprintln!("hydra: nothing advertised for {u}; streaming to compute sha256");
            let tmp = std::env::temp_dir().join(format!("hydra_ck_{}", std::process::id()));
            let job = crate::download::Job {
                urls: vec![u.clone()],
                output: Some(tmp.clone()),
                quiet: true,
                no_progress: true,
                force: true,
                no_save: true,
                emit_manifest: None,
                chunk_digests: None,
                chunk_size: None,
                ..crate::download::default_job()
            };
            let out = crate::download::run(job).await;
            if out.ok {
                computed = out.sha256.clone();
            }
            let _ = std::fs::remove_file(&tmp);
        }

        // Verification against a local file, when asked.
        //
        // The digest `--download-if-needed` just computed counts as something to
        // verify against. It was previously ignored here: `local_digests` consults
        // only `found` (server-advertised digests), so an object whose server
        // advertises nothing produced a correct `computed_sha256` beside the
        // verdict "no comparable digest" — and a deliberately corrupted local file
        // produced the identical verdict. The one case the flag combination exists
        // to serve was the one case it could not answer.
        let mut comparable = found.clone();
        if let Some(hex) = &computed {
            let already = comparable.iter().any(|a| a.algo == digest::Algo::Sha256);
            if !already {
                comparable.push(digest::Advertised {
                    algo: digest::Algo::Sha256,
                    hex: hex.clone(),
                    source: "computed by streaming the object".into(),
                });
            }
        }
        let found = comparable;
        let mut verdict = None;
        if let Some(path) = verify {
            match local_digests(path, &found) {
                Ok(local) => {
                    let matched = found.iter().find(|a| {
                        local
                            .iter()
                            .any(|(al, hx)| *al == a.algo && hx.eq_ignore_ascii_case(&a.hex))
                    });
                    let mismatched = found.iter().find(|a| {
                        local
                            .iter()
                            .any(|(al, hx)| *al == a.algo && !hx.eq_ignore_ascii_case(&a.hex))
                    });
                    verdict = match (matched, mismatched) {
                        (Some(a), _) => Some(format!("MATCH on {}", a.algo.as_str())),
                        (None, Some(a)) => {
                            all_ok = false;
                            Some(format!("MISMATCH on {}", a.algo.as_str()))
                        }
                        // Nothing comparable is not a pass: say so rather than implying one.
                        (None, None) => Some(
                            "no comparable digest (the server advertised none this tool can \
                             compute)"
                                .into(),
                        ),
                    };
                }
                Err(e) => {
                    eprintln!("hydra: cannot read {}: {e}", path.display());
                    all_ok = false;
                }
            }
        }

        if !json {
            println!("{u}");
            if let Some(s) = size {
                println!("  size: {} ({s} bytes)", progress::human(s));
            }
            if found.is_empty() && computed.is_none() {
                println!("  no digest advertised by the server");
                // The distinction matters: an ETag is not a checksum, and users reach for
                // it as one.
                if let Some(v) = &validator {
                    println!("  validator: {v}  (an opaque ETag, NOT a content digest)");
                }
                println!("  to compute one, the bytes must be transferred: --download-if-needed");
            }
            for a in &found {
                let strength = if a.algo.is_cryptographic() {
                    ""
                } else {
                    "  [not collision-resistant]"
                };
                println!(
                    "  {}: {}{}\n    from {}",
                    a.algo.as_str(),
                    a.hex,
                    strength,
                    a.source
                );
            }
            if let Some(c) = &computed {
                println!("  sha256: {c}\n    computed locally after transferring the object");
            }
            if let Some(v) = &verdict {
                println!("  verify: {v}");
            }
        }
        records.push(serde_json::json!({
            "url": u,
            "size": size,
            "validator": validator,
            "validator_is_not_a_digest": true,
            "advertised": found.iter().map(|a| serde_json::json!({
                "algo": a.algo.as_str(),
                "hex": a.hex,
                "source": a.source,
                "collision_resistant": a.algo.is_cryptographic(),
            })).collect::<Vec<_>>(),
            "computed_sha256": computed,
            "verify": verdict,
        }));
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "objects": records,
                "ok": all_ok,
            }))
            .unwrap_or_default()
        );
    }
    if all_ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// Compute local digests for exactly the algorithms that were advertised.
///
/// Only the advertised ones: hashing a large file three times to produce digests nobody
/// asked for is wasted I/O, and the file is read once with all needed hashers fed in
/// parallel.
fn local_digests(
    path: &std::path::Path,
    advertised: &[hya_net::digest::Advertised],
) -> std::io::Result<Vec<(hya_net::digest::Algo, String)>> {
    use hya_net::digest::Algo;
    use md5::Digest as _;
    use std::io::Read;
    let want: Vec<Algo> = advertised
        .iter()
        .map(|a| a.algo)
        .filter(|a| matches!(a, Algo::Sha256 | Algo::Sha512 | Algo::Sha1 | Algo::Md5))
        .collect();
    if want.is_empty() {
        return Ok(Vec::new());
    }
    let mut f = std::fs::File::open(path)?;
    let mut sha256 = sha2::Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut sha1 = sha1::Sha1::new();
    let mut md5h = md5::Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if want.contains(&Algo::Sha256) {
            sha256.update(chunk);
        }
        if want.contains(&Algo::Sha512) {
            sha512.update(chunk);
        }
        if want.contains(&Algo::Sha1) {
            sha1.update(chunk);
        }
        if want.contains(&Algo::Md5) {
            md5h.update(chunk);
        }
    }
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut out = Vec::new();
    for a in want {
        let h = match a {
            Algo::Sha256 => hex(&sha256.clone().finalize()),
            Algo::Sha512 => hex(&sha512.clone().finalize()),
            Algo::Sha1 => hex(&sha1.clone().finalize()),
            Algo::Md5 => hex(&md5h.clone().finalize()),
            _ => continue,
        };
        out.push((a, h));
    }
    Ok(out)
}

/// Decide what to do about already-present output files, before any transfer starts.
///
/// Returns one decision per URL. Files that do not exist need no decision. A URL whose
/// file the user chose to skip is dropped from the run rather than fetched and discarded.
struct Decision {
    resume: bool,
    skip: bool,
}

async fn resolve_existing(
    urls: &[String],
    template: &download::Job,
) -> Result<Vec<Decision>, std::process::ExitCode> {
    let mut out = Vec::with_capacity(urls.len());
    for u in urls {
        let name = crate::url::Url::parse(u)
            .map(|p| p.suggested_filename())
            .unwrap_or_else(|| "download".into());
        let path = match &template.output_dir {
            Some(d) => d.join(&name),
            None => std::path::PathBuf::from(&name),
        };
        // `-c` is an explicit answer and is never re-asked, matching the single-URL path.
        if template.resume || !path.exists() {
            out.push(Decision {
                resume: template.resume,
                skip: false,
            });
            continue;
        }
        if template.force {
            out.push(Decision {
                resume: false,
                skip: false,
            });
            continue;
        }
        if template.no_clobber {
            out.push(Decision {
                resume: false,
                skip: true,
            });
            continue;
        }
        let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "hydra: {} already exists ({} on disk)",
            path.display(),
            progress::human(on_disk)
        );
        eprintln!("  [c] continue it   [r] start over   [s] skip this file");
        eprint!("hydra: what would you like to do? ");
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line).unwrap_or(0);
        // No answer available (piped, redirected, or EOF): choose the option that cannot
        // destroy data, as the single-URL prompt does.
        let ans = if n == 0 {
            "s".to_string()
        } else {
            line.trim().to_ascii_lowercase()
        };
        match ans.as_str() {
            "c" | "continue" | "y" => out.push(Decision {
                resume: true,
                skip: false,
            }),
            "r" | "restart" => out.push(Decision {
                resume: false,
                skip: false,
            }),
            _ => out.push(Decision {
                resume: false,
                skip: true,
            }),
        }
    }
    Ok(out)
}

/// Fetch several independent objects, concurrently or one at a time.
///
/// `same` (the default) runs them together, which is what a user pasting three links
/// expects; `queue` runs them in order, which is the polite choice against one host and
/// the predictable one on a narrow link. Either way each URL gets its own job and its own
/// output file — mirror assembly is a different operation and needs `--mirrors`.
async fn run_many(
    template: download::Job,
    urls: &[String],
    mode: cli::UrlMode,
    json: bool,
    quiet: bool,
) -> std::process::ExitCode {
    // Existing files are decided ONCE, before anything starts.
    //
    // Concurrent jobs cannot each own the terminal: several prompts would interleave on
    // the same rows and an answer would land on whichever job read stdin first, which is
    // not the one the question came from. So the prompt runs here, serially, with the
    // progress renderer not yet drawing — and the engines then run with the decision
    // already made (`force`), never prompting.
    let decisions = match resolve_existing(urls, &template).await {
        Ok(d) => d,
        Err(code) => return code,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<download::Tick>();
    let jobs: Vec<download::Job> = urls
        .iter()
        .enumerate()
        .map(|(i, u)| download::Job {
            urls: vec![u.clone()],
            // Each URL names its own file; an explicit -O cannot apply to several.
            output: None,
            // Already answered above; the engine must not ask again.
            force: true,
            resume: decisions[i].resume,
            // Per-file frames would fight for the same terminal rows, so the engines stay
            // silent and one aggregate renderer owns the screen.
            no_progress: true,
            quiet: true,
            ticks: Some((i as u64, tx.clone())),
            ..template.clone()
        })
        .collect();
    drop(tx);

    let skipped: Vec<usize> = decisions
        .iter()
        .enumerate()
        .filter(|(_, d)| d.skip)
        .map(|(i, _)| i)
        .collect();
    for i in &skipped {
        eprintln!("hydra: skipping {} (kept the existing file)", urls[*i]);
    }
    let names: Vec<String> = urls
        .iter()
        .map(|u| {
            crate::url::Url::parse(u)
                .map(|p| p.suggested_filename())
                .unwrap_or_else(|| "download".into())
        })
        .collect();
    let mut multi = progress::Multi::new(names.clone(), template.verbose, quiet);

    let mut outs: Vec<Option<download::Outcome>> = vec![None; jobs.len()];
    match mode {
        cli::UrlMode::Same => {
            let mut set = tokio::task::JoinSet::new();
            for (i, j) in jobs.into_iter().enumerate() {
                if decisions[i].skip {
                    continue;
                }
                multi.start(i as u64);
                set.spawn(async move { (i, download::run(j).await) });
            }
            loop {
                tokio::select! {
                    Some(t) = rx.recv() => multi.tick(t),
                    r = set.join_next() => match r {
                        Some(Ok((i, o))) => {
                            multi.done(i as u64, &o);
                            outs[i] = Some(o);
                        }
                        Some(Err(_)) | None => break,
                    },
                }
            }
        }
        cli::UrlMode::Queue => {
            for (i, j) in jobs.into_iter().enumerate() {
                if decisions[i].skip {
                    continue;
                }
                multi.start(i as u64);
                let h = tokio::spawn(download::run(j));
                loop {
                    tokio::select! {
                        Some(t) = rx.recv() => multi.tick(t),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(120)) => {}
                    }
                    if h.is_finished() {
                        break;
                    }
                }
                if let Ok(o) = h.await {
                    multi.done(i as u64, &o);
                    outs[i] = Some(o);
                }
            }
        }
    }
    multi.finish();

    let results: Vec<&download::Outcome> = outs.iter().flatten().collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "files": results,
                "ok": results.iter().all(|o| o.ok),
                "count": results.len(),
            }))
            .unwrap_or_default()
        );
    }
    let expected = urls.len() - skipped.len();
    if results.iter().all(|o| o.ok) && results.len() == expected {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

/// `hydra parity make|repair` — local Reed-Solomon parity generation and repair.
fn run_parity(what: &cli::ParityCmd) -> i32 {
    use hya_net::manifest::Manifest;
    use hya_net::parity::{generate, repair, Layout, DEFAULT_WINDOW};
    use std::path::PathBuf;

    match what {
        cli::ParityCmd::Make {
            file,
            manifest,
            parity,
            out,
            window,
        } => {
            let text = match std::fs::read_to_string(manifest) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("hydra: cannot read manifest {}: {e}", manifest.display());
                    return 2;
                }
            };
            let mut m = match Manifest::parse(&text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("hydra: {}: {e}", manifest.display());
                    return 2;
                }
            };
            // Shard size == manifest chunk size. A download chunk straddling two
            // shards would mean one corrupt chunk destroys two shards, halving
            // effective parity.
            let lay = match Layout::new(
                m.object.size,
                m.object.chunk_size,
                *parity,
                window.unwrap_or(DEFAULT_WINDOW),
            ) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("hydra: {e}");
                    return 2;
                }
            };
            let pp = out
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("{}.parity", file.display())));
            let digests = match generate(&file.to_string_lossy(), &pp.to_string_lossy(), &lay) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("hydra: parity generation failed: {e}");
                    return 1;
                }
            };
            // Record the code in the manifest, including the parity shards' own
            // digests: a corrupt parity shard fed into a repair produces a
            // silently wrong result.
            m.parity = Some(hya_net::manifest::ParityMeta {
                algo: "reed-solomon".into(),
                field: "gf16".into(),
                k: lay.k,
                parity_shards: lay.parity,
                window: lay.window,
                file: pp.to_string_lossy().to_string(),
                shard_digests: digests,
            });
            if let Err(e) = std::fs::write(manifest, m.to_json()) {
                eprintln!("hydra: cannot update manifest: {e}");
                return 1;
            }
            println!(
                "parity: {} data + {} parity shards of {} B, window {} B -> {}",
                lay.k,
                lay.parity,
                lay.shard_size,
                lay.window,
                pp.display()
            );
            println!(
                "  repairs up to {} damaged shard(s); resident memory {:.1} MB, independent of file size",
                lay.parity,
                lay.resident_bytes() as f64 / 1e6
            );
            0
        }

        cli::ParityCmd::Repair {
            file,
            manifest,
            parity_file,
            dry_run,
        } => {
            let text = match std::fs::read_to_string(manifest) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("hydra: cannot read manifest {}: {e}", manifest.display());
                    return 2;
                }
            };
            let m = match Manifest::parse(&text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("hydra: {}: {e}", manifest.display());
                    return 2;
                }
            };
            let Some(pm) = m.parity.clone() else {
                eprintln!(
                    "hydra: {} records no parity; run `hydra parity make` first",
                    manifest.display()
                );
                return 2;
            };

            // Erasure positions come from digest mismatches against the trusted manifest.
            let bad;
            {
                use hya_net::manifest::{ChunkVerifier, Trust};
                let mut v = ChunkVerifier::new(m.clone(), Trust::Trusted);
                let mut f = match std::fs::File::open(file) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("hydra: cannot read {}: {e}", file.display());
                        return 2;
                    }
                };
                if let Err(e) = v.write_reader(&mut f) {
                    eprintln!("hydra: read failed: {e}");
                    return 1;
                }
                if v.all_verified() {
                    println!("{}: all chunks verify; nothing to repair", file.display());
                    return 0;
                }
                bad = v.failed_indices().to_vec();
            }

            println!(
                "{}: {} damaged chunk(s): {:?}",
                file.display(),
                bad.len(),
                bad
            );
            if bad.len() > pm.parity_shards {
                eprintln!(
                    "hydra: {} damaged but only {} parity shards: unrecoverable",
                    bad.len(),
                    pm.parity_shards
                );
                return 1;
            }
            if *dry_run {
                println!("  (dry run: {} shard(s) would be repaired)", bad.len());
                return 0;
            }

            let lay = match Layout::new(
                m.object.size,
                m.object.chunk_size,
                pm.parity_shards,
                pm.window,
            ) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("hydra: {e}");
                    return 2;
                }
            };
            let pp = parity_file
                .clone()
                .unwrap_or_else(|| PathBuf::from(pm.file.clone()));

            // Verify the PARITY before decoding from it. A decoder cannot tell a
            // corrupt parity shard from a good one, and repair writes in place, so
            // decoding from rotted parity damages chunks that were intact — turning
            // one recoverable chunk into several. Checked here rather than trusting
            // the length, which a rotted file of the right size passes.
            match hya_net::parity::verify_parity(&pp.to_string_lossy(), &lay, &pm.shard_digests) {
                Ok(bad_shards) if !bad_shards.is_empty() => {
                    eprintln!(
                        "hydra: {} is itself damaged (parity shard(s) {:?} fail their digests); \
                         refusing to decode from it — {} still holds its original bytes",
                        pp.display(),
                        bad_shards,
                        file.display()
                    );
                    return 1;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("hydra: cannot verify {}: {e}", pp.display());
                    return 1;
                }
            }

            match repair(&file.to_string_lossy(), &pp.to_string_lossy(), &lay, &bad) {
                Ok(n) => {
                    // Re-verify repaired object against manifest chunks.
                    use hya_net::manifest::{ChunkVerifier, Trust};
                    let mut v = ChunkVerifier::new(m.clone(), Trust::Trusted);
                    let mut f = std::fs::File::open(file).expect("just repaired");
                    // A read error surfaces as unverified chunks below, which is
                    // the honest report: the re-check could not prove the repair.
                    let _ = v.write_reader(&mut f);
                    if v.all_verified() {
                        println!("repaired {n} shard(s); all chunks now verify");
                        0
                    } else {
                        eprintln!(
                            "hydra: repair ran but {} chunk(s) still fail: the parity or the \
                             manifest does not match this file",
                            v.failed_indices().len()
                        );
                        1
                    }
                }
                Err(e) => {
                    eprintln!("hydra: repair failed: {e}");
                    1
                }
            }
        }
    }
}

/// Report the value of one or more response headers, and nothing else.
///
/// `hydra <url> -H X-GitHub-Request-Id` prints
/// `AC6C:16A281:52CCDD5:53451AC:6A813D03` — the value, on its own line, with no
/// bar, no summary, no format note. That output composes: `$(...)` captures it
/// and a pipe reads it.
///
/// Only the value, because that is what was asked for. When several headers are
/// queried at once the name is prefixed (`Name: value`), since otherwise the
/// lines are unattributable; with a single query it stays bare. Same rule
/// `--json` follows: a machine channel carries one thing.
///
/// No body is transferred. The question is about the response head, so fetching
/// the object would be work nobody asked for — this is `--spider` with a
/// projection.
async fn report_header_values(urls: &[String], args: &cli::Cli) -> std::process::ExitCode {
    let conn = match hya_net::TlsCapableConnector::with_insecure(args.insecure) {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            eprintln!("hydra: tls setup failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let multi = args.header_queries.len() > 1 || urls.len() > 1;
    let mut all_found = true;

    for u in urls {
        let Some(parsed) = crate::url::Url::parse(u) else {
            eprintln!("hydra: cannot parse {u}");
            all_found = false;
            continue;
        };
        // Follow redirects: the header almost always belongs to the final
        // response, and reporting a 302's headers would answer a different
        // question than the one asked. Headers the user asked to SEND still
        // apply — the value a server returns can depend on what it was asked
        // (Vary, auth, conditional GETs) — and `probe_public` carries them.
        let pr = match crate::download::probe_public(conn.as_ref(), &parsed, args).await {
            Ok((p, _final_url)) => p,
            Err(e) => {
                eprintln!("hydra: {u}: {e}");
                all_found = false;
                continue;
            }
        };

        for name in &args.header_queries {
            match hya_net::header_lookup(&pr.raw_head, name) {
                Some(v) if multi => println!("{name}: {v}"),
                Some(v) => println!("{v}"),
                None => {
                    // A missing header is not a crash, but it is not success
                    // either: a script substituting an empty string would be
                    // acting on an answer that was never given.
                    eprintln!("hydra: {u}: no {name} header in the response");
                    all_found = false;
                }
            }
        }
    }

    if all_found {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

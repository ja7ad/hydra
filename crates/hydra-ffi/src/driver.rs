// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Download execution driver coordinating probing, transfer scheduling, and verification.

use crate::abi::{hydra_error_code_t as E, hydra_event_type_t as EV, hydra_job_state_t as S};
use crate::engine::{now_ms, Creds, Engine, Job, SourceStat, Stop};
use crate::err::{self, Detail};
use crate::url::Url;
use hya_core::{Capability, RateMeter, Scheduler, Source};
use hya_net::polite::Pace;
use hya_net::{
    basic_auth, probe_resilient, Probe, SparseSink, Target, TlsCapableConnector, PROBE_FANOUT,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Maximum redirect hops allowed during URL resolution.
const MAX_REDIRECTS: usize = 10;

/// Polling interval for cooperative cancellation checks on streaming paths.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// Do not split an object finer than this: a range request costs a round trip,
/// and below a quarter megabyte per connection the setup outweighs the bytes
/// it could carry. The same rule the CLI and the GUI apply.
const MIN_BYTES_PER_CONNECTION: u64 = 256 * 1024;

/// Connections worth opening for an object of `size` bytes, at most `wanted`.
fn connections_for_size(wanted: usize, size: u64) -> usize {
    let by_size = usize::try_from(size / MIN_BYTES_PER_CONNECTION).unwrap_or(usize::MAX);
    wanted.min(by_size).max(1)
}

/// Add what `now` says on top of what was already credited to `counter`.
///
/// The scheduler reports a running total per transfer; the engine's counter is
/// a total over every transfer, so only the increase since the last call may be
/// added — storing the per-transfer figure would make two jobs overwrite each
/// other.
fn credit_increase(counter: &AtomicU64, credited: &mut u64, now: u64) {
    counter.fetch_add(now.saturating_sub(*credited), Ordering::Relaxed);
    *credited = now;
}

/// Spawns a job execution task on the engine's async runtime.
pub(crate) fn spawn(engine: &Arc<Engine>, job: &Arc<Job>) -> Result<(), Detail> {
    if engine.shutdown.load(Ordering::Relaxed) {
        return Err(Detail {
            code: E::HYDRA_ERR_SHUTDOWN as u32,
            message: "engine is shutting down".into(),
            ..Default::default()
        });
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let generation = {
        let mut g = job.lock();
        if g.is_running() {
            return Err(Detail {
                code: E::HYDRA_ERR_INVALID_STATE as u32,
                message: format!("job {} is already running", job.id),
                ..Default::default()
            });
        }
        if g.state == S::HYDRA_JOB_COMPLETED as u32 {
            return Err(Detail {
                code: E::HYDRA_ERR_INVALID_STATE as u32,
                message: format!("job {} has already completed", job.id),
                ..Default::default()
            });
        }
        g.generation += 1;
        g.stop = Stop::None;
        g.error = None;
        g.cancel = Some(cancel.clone());
        g.state = S::HYDRA_JOB_QUEUED as u32;
        g.started_at_ms = now_ms();
        g.finished_at_ms = 0;
        g.generation
    };
    let ticket = engine.gate.enqueue(job.cfg.priority, job.seq);
    let (e, j) = (engine.clone(), job.clone());
    engine.rt.spawn(async move {
        run(e, j, generation, cancel, ticket).await;
    });
    Ok(())
}

/// Main execution lifecycle loop for a transfer attempt sequence.
async fn run(
    engine: Arc<Engine>,
    job: Arc<Job>,
    generation: u64,
    cancel: Arc<AtomicBool>,
    ticket: crate::gate::Ticket,
) {
    engine.emit(&job, EV::HYDRA_EVENT_JOB_QUEUED);

    // Wait for the slot, but stay cancellable while waiting: a queued job the
    // user cancels must not sit in the queue until something ahead of it
    // finishes. Dropping the ticket's future gives the place up.
    let permit = tokio::select! {
        p = ticket.wait() => p,
        _ = wait_for_cancel(&cancel) => {
            settle_stopped(&engine, &job, generation);
            return;
        }
    };

    if let Some(reason) = engine.network_blocked() {
        let d = Detail {
            code: E::HYDRA_ERR_RESOURCE_LIMIT as u32,
            message: reason.to_string(),
            ..Default::default()
        };
        settle_failed(&engine, &job, generation, d);
        drop(permit);
        return;
    }

    {
        let mut g = job.lock();
        if g.generation != generation {
            return;
        }
        g.state = S::HYDRA_JOB_RESOLVING as u32;
    }
    engine.emit(&job, EV::HYDRA_EVENT_JOB_STARTED);

    let tries = job.cfg.max_retries.saturating_add(1);
    let mut last: Option<Detail> = None;
    for attempt in 0..tries {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match attempt_transfer(&engine, &job, &cancel).await {
            Ok(()) => {
                last = None;
                break;
            }
            Err(d) if d.code == E::HYDRA_ERR_CANCELLED as u32 => {
                last = Some(d);
                break;
            }
            Err(d) => {
                engine.metrics.error_count.fetch_add(1, Ordering::Relaxed);
                let retryable = is_retryable(d.code);
                last = Some(d);
                if !retryable || attempt + 1 >= tries {
                    break;
                }
                engine.metrics.retry_count.fetch_add(1, Ordering::Relaxed);
                engine.emit_error(
                    &job,
                    EV::HYDRA_EVENT_RETRYING,
                    last.as_ref().expect("just set"),
                );
                // Jittered exponential backoff, so a fleet of clients failing
                // against the same origin does not re-converge on it in step.
                let wait = hya_net::polite::backoff_with_jitter(
                    attempt,
                    Duration::from_millis(500),
                    Duration::from_secs(30),
                    job.id.wrapping_mul(0x9E37_79B9_7F4A_7C15),
                );
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = wait_for_cancel(&cancel) => break,
                }
            }
        }
    }
    drop(permit);

    if cancel.load(Ordering::Relaxed) {
        settle_stopped(&engine, &job, generation);
        return;
    }
    match last {
        None => settle_completed(&engine, &job, generation).await,
        Some(d) if d.code == E::HYDRA_ERR_CANCELLED as u32 => {
            settle_stopped(&engine, &job, generation)
        }
        Some(d) => settle_failed(&engine, &job, generation, d),
    }
}

/// Failures where trying again is a reasonable thing to do.
///
/// A checksum mismatch is not in this list on purpose: re-downloading an object
/// the server is serving incorrectly just burns the user's data allowance to
/// arrive at the same answer.
fn is_retryable(code: u32) -> bool {
    matches!(
        code,
        x if x == E::HYDRA_ERR_NETWORK as u32
            || x == E::HYDRA_ERR_CONNECTION as u32
            || x == E::HYDRA_ERR_TIMEOUT as u32
            || x == E::HYDRA_ERR_PROTOCOL as u32
    )
}

/// Resolve while a cancel is pending.
async fn wait_for_cancel(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

fn settle_stopped(engine: &Arc<Engine>, job: &Arc<Job>, generation: u64) {
    let stop = {
        let mut g = job.lock();
        if g.generation != generation {
            return;
        }
        g.cancel = None;
        let stop = g.stop;
        g.stop = Stop::None;
        stop
    };
    match stop {
        Stop::Pause | Stop::None => {
            {
                let mut g = job.lock();
                g.state = S::HYDRA_JOB_PAUSED as u32;
            }
            engine.emit(job, EV::HYDRA_EVENT_PAUSED);
        }
        Stop::CancelKeep | Stop::CancelRemove => {
            if stop == Stop::CancelRemove {
                let path = job.lock().output_path.clone();
                // Best effort: the file may never have been created, and a
                // cancel that fails because the partial file is already gone is
                // not a failure the application can act on.
                let _ = std::fs::remove_file(&path);
                let mut g = job.lock();
                g.held.clear();
                g.progress.bytes_downloaded = 0;
            }
            {
                let mut g = job.lock();
                g.state = S::HYDRA_JOB_CANCELLED as u32;
                g.finished_at_ms = now_ms();
            }
            engine.emit(job, EV::HYDRA_EVENT_CANCELLED);
        }
    }
    crate::persist::autosave(engine);
}

fn settle_failed(engine: &Arc<Engine>, job: &Arc<Job>, generation: u64, d: Detail) {
    {
        let mut g = job.lock();
        if g.generation != generation {
            return;
        }
        g.cancel = None;
        g.state = S::HYDRA_JOB_FAILED as u32;
        g.finished_at_ms = now_ms();
        g.error = Some(d.clone());
    }
    engine.metrics.jobs_failed.fetch_add(1, Ordering::Relaxed);
    engine.emit_error(job, EV::HYDRA_EVENT_FAILED, &d);
    crate::persist::autosave(engine);
}

/// The transfer finished on the wire; verify if asked, then declare success.
async fn settle_completed(engine: &Arc<Engine>, job: &Arc<Job>, generation: u64) {
    if let Some((algo, want)) = job.cfg.checksum.clone() {
        {
            let mut g = job.lock();
            if g.generation != generation {
                return;
            }
            g.state = S::HYDRA_JOB_VERIFYING as u32;
        }
        engine.emit(job, EV::HYDRA_EVENT_VERIFYING);
        let path = job.lock().output_path.clone();
        // Hashing a large file is CPU- and disk-bound and would otherwise
        // occupy a runtime worker for seconds while other transfers wait.
        let res = tokio::task::spawn_blocking(move || crate::verify::check(&path, algo, &want))
            .await
            .unwrap_or_else(|e| {
                Err(Detail {
                    code: E::HYDRA_ERR_INTERNAL as u32,
                    message: format!("verification task failed: {e}"),
                    ..Default::default()
                })
            });
        if let Err(d) = res {
            settle_failed(engine, job, generation, d);
            return;
        }
    }
    {
        let mut g = job.lock();
        if g.generation != generation {
            return;
        }
        g.cancel = None;
        g.state = S::HYDRA_JOB_COMPLETED as u32;
        g.finished_at_ms = now_ms();
        g.error = None;
        if let Some(size) = g.size {
            g.progress.bytes_downloaded = size;
            g.progress.total_bytes = size;
            g.held = vec![(0, size)];
        }
        g.progress.eta_seconds = 0;
    }
    engine
        .metrics
        .jobs_completed
        .fetch_add(1, Ordering::Relaxed);
    engine.emit(job, EV::HYDRA_EVENT_COMPLETED);
    crate::persist::autosave(engine);
}

/// What a probe established about one source.
struct Resolved {
    url: Url,
    target: Target,
    probe: Probe,
    /// The configured URL this came from, BEFORE any redirect.
    ///
    /// A publisher's ranking is index-aligned with the URLs the job was created
    /// with, and probing may drop some and redirect others — so the way back to
    /// a mirror's rank is the string it started as, not the host it ended at.
    requested: String,
    /// Measured per-request setup cost, which the scheduler needs to decide
    /// whether a repair can pay for itself.
    delta: f64,
}

fn cancelled() -> Detail {
    Detail {
        code: E::HYDRA_ERR_CANCELLED as u32,
        message: "stopped by request".into(),
        ..Default::default()
    }
}

/// The `Proxy-Authorization` line an HTTP forward proxy with a login is owed.
///
/// The transport reads it off the headers for the CONNECT tunnel too, and
/// never forwards it to the origin through the tunnel.
fn proxy_auth_line(job: &Job) -> Option<String> {
    let p = job.cfg.proxy.as_ref().filter(|p| !p.kind.is_socks())?;
    let user = p.username.as_deref()?;
    Some(format!(
        "Proxy-Authorization: Basic {}",
        basic_auth(user, p.password.as_deref().unwrap_or(""))
    ))
}

/// Build the request target for one URL under this job's proxy and auth.
fn target_for(engine: &Engine, job: &Job, creds: &Creds, u: &Url) -> Target {
    let mut headers: Vec<String> = job
        .cfg
        .headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    if let Some(user) = &creds.username {
        let pass = creds.password.clone().unwrap_or_default();
        headers.push(format!("Authorization: Basic {}", basic_auth(user, &pass)));
    }
    headers.extend(proxy_auth_line(job));
    bare_target(job, u).with_headers(headers, Some(engine.cfg.user_agent.clone()))
}

/// The target a redirect chain reached at `u`, carrying `first`'s headers.
///
/// Through `Target::with_headers_from`, so a credential typed for the origin
/// the job named does not follow a redirect off it — and the proxy's own login
/// is put back, because the socket still goes to the same proxy.
fn redirected_target(job: &Job, first: &Target, u: &Url) -> Target {
    let mut t =
        bare_target(job, u).with_headers_from(first, first.headers.clone(), first.agent.clone());
    if let Some(line) = proxy_auth_line(job) {
        if t.proxy_authorization().is_none() {
            t.headers.push(line);
        }
    }
    t
}

/// The socket and request shape for `u`, before any header is attached.
fn bare_target(job: &Job, u: &Url) -> Target {
    let mut t = match &job.cfg.proxy {
        // An HTTP forward proxy changes the request itself: absolute-form for
        // cleartext, a CONNECT tunnel for TLS. Both are expressed by naming the
        // origin authority in `origin` while the socket goes to the proxy.
        //
        // The authority carries an explicit port even when it is the default:
        // a `Host` header should omit it, but a CONNECT request line without
        // one is rejected by real proxies.
        Some(p) if !p.kind.is_socks() => {
            Target::via_proxy(&p.host, p.port, &format!("{}:{}", u.host, u.port), &u.path)
        }
        // SOCKS is invisible at this layer: the connector dials the proxy and
        // hands back an end-to-end stream, so the request must look direct. An
        // absolute-form GET sent at a SOCKS port is a bug this shape prevents.
        _ if u.tls() => Target::direct_tls(&u.host, u.port, &u.path),
        _ => Target::direct(&u.host, u.port, &u.path),
    };
    t.tls = u.tls();
    t
}

/// Probe one URL, following redirects.
async fn resolve_one(
    engine: &Arc<Engine>,
    job: &Arc<Job>,
    conn: &Arc<TlsCapableConnector>,
    raw: &str,
    creds: &Creds,
    cancel: &Arc<AtomicBool>,
) -> Result<Resolved, Detail> {
    let mut u = Url::parse(raw).map_err(|e| Detail {
        code: E::HYDRA_ERR_INVALID_URL as u32,
        message: e,
        ..Default::default()
    })?;
    let first = target_for(engine, job, creds, &u);
    let mut t = first.clone();
    let mut guise = hya_net::Guise::default();
    for _ in 0..MAX_REDIRECTS {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let hop = Instant::now();
        // Resilient rather than a bare HEAD: a server that answers HEAD with an
        // empty reply (hetzner's speed-test hosts do) otherwise resolves to
        // "unknown size, no ranges" and sends the whole object down the
        // single-stream path — no size, no resume, no parallelism.
        let (p, admitted) =
            hya_net::probe_admitted(
                &t,
                |t| async move { probe_resilient(conn.as_ref(), &t).await },
            )
            .await
            .map_err(|e| err::from_io(&e))?;
        t = t.in_guise(&admitted);
        guise.absorb(admitted);
        if p.is_redirect() {
            let loc = p.location.clone().unwrap_or_default();
            u = u.join(&loc).map_err(|e| Detail {
                code: E::HYDRA_ERR_INVALID_URL as u32,
                message: format!("redirect to {loc:?}: {e}"),
                ..Default::default()
            })?;
            t = redirected_target(job, &first, &u).in_guise(&guise);
            continue;
        }
        // A forward expressed in HTML rather than in a header: a referrer
        // stripper or link filter answering `200` with a page whose whole
        // content is "go here instead". Following it costs one small GET on a
        // response that was already going to be downloaded, and not following
        // it means saving the forwarding page under the object's name — a
        // failure that reports success. Charged to the same hop budget.
        if p.maybe_redirector() {
            if let Some(next) = hya_net::html_redirect(conn.as_ref(), &t)
                .await
                .and_then(|loc| u.join(&loc).ok())
            {
                u = next;
                t = redirected_target(job, &first, &u).in_guise(&guise);
                continue;
            }
        }
        if let Some(why) = p.refusal() {
            return Err(Detail {
                code: E::HYDRA_ERR_NETWORK as u32,
                os_error: 0,
                http_status: p.status as i32,
                message: format!("{why} for {}", u.host),
            });
        }
        // Timed on the FINAL hop only. Folding a long redirect chain into the
        // estimate would hand the slowest repair decisions to exactly the
        // origins that need the fastest ones. Floored so a probe served from a
        // pooled connection cannot make a repair look free.
        let delta = hop.elapsed().as_secs_f64().clamp(0.05, 45.0);
        return Ok(Resolved {
            url: u,
            target: t,
            probe: p,
            requested: raw.to_string(),
            delta,
        });
    }
    Err(Detail {
        code: E::HYDRA_ERR_PROTOCOL as u32,
        message: format!("more than {MAX_REDIRECTS} redirects"),
        ..Default::default()
    })
}

/// One end-to-end attempt: resolve, choose a strategy, transfer.
async fn attempt_transfer(
    engine: &Arc<Engine>,
    job: &Arc<Job>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), Detail> {
    let conn = engine.connector(job.cfg.proxy.as_ref())?;
    let creds = job.creds();

    // A restored job comes back without the headers that carried credentials.
    // Say so once, loudly, rather than letting the application discover it as an
    // unexplained 401 three seconds later.
    if !job.cfg.withheld_headers.is_empty() {
        crate::log::log_at!(
            engine,
            crate::abi::hydra_log_level_t::HYDRA_LOG_WARN,
            "job {}: restored without {} (credential-bearing headers are never persisted); \
             re-supply them before expecting authenticated access",
            job.id,
            job.cfg.withheld_headers.join(", ")
        );
    }

    // FTP first: range preemption costs control-channel round trips HTTP pays
    // nothing for, so the object streams sequentially from one connection.
    if Url::parse(&job.cfg.urls[0])
        .map(|u| u.is_ftp())
        .unwrap_or(false)
    {
        return ftp_transfer(engine, job, &conn, &creds, cancel).await;
    }

    // Probe every mirror concurrently: in series a dozen HEADs cost 14.4 s
    // before the first byte, and on mobile those round trips are the long ones.
    let gate = Arc::new(tokio::sync::Semaphore::new(PROBE_FANOUT));
    let mut set = tokio::task::JoinSet::new();
    for (i, raw) in job.cfg.urls.iter().enumerate() {
        let (engine, job, conn, creds, cancel, gate) = (
            engine.clone(),
            job.clone(),
            conn.clone(),
            creds.clone(),
            cancel.clone(),
            gate.clone(),
        );
        let raw = raw.clone();
        set.spawn(async move {
            let _permit = gate.acquire_owned().await;
            (
                i,
                resolve_one(&engine, &job, &conn, &raw, &creds, &cancel).await,
            )
        });
    }
    // Collected in the order the CALLER gave, not the order the network
    // answered in. Everything downstream is index-aligned with `cfg.urls` —
    // the publisher's ranking, the connection split, the source rows — and
    // which mirror is source 0 must not depend on which handshake finished
    // first, or two runs against the same document cannot be compared.
    let mut answers: Vec<(usize, Result<Resolved, Detail>)> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(v) => answers.push(v),
            Err(e) => {
                return Err(Detail {
                    code: E::HYDRA_ERR_INTERNAL as u32,
                    message: format!("probe task failed: {e}"),
                    ..Default::default()
                })
            }
        }
    }
    answers.sort_by_key(|(i, _)| *i);
    let mut resolved: Vec<Resolved> = Vec::new();
    let mut first_error: Option<Detail> = None;
    for (_, res) in answers {
        match res {
            Ok(r) => resolved.push(r),
            // A cancel outranks any transport error: the caller asked to stop,
            // and reporting "connection refused" for that would be a lie.
            Err(d) if d.code == E::HYDRA_ERR_CANCELLED as u32 => return Err(d),
            Err(d) => {
                if first_error.is_none() {
                    first_error = Some(d);
                }
            }
        }
    }
    if resolved.is_empty() {
        return Err(first_error.unwrap_or_else(|| Detail {
            code: E::HYDRA_ERR_NETWORK as u32,
            message: "no source could be reached".into(),
            ..Default::default()
        }));
    }

    let primary = &resolved[0];
    let file_name = primary
        .probe
        .suggested_filename()
        .or_else(|| primary.url.file_name());
    let known_size =
        (primary.probe.status < 300 && primary.probe.size > 0).then_some(primary.probe.size);
    let strong = |p: &Probe| p.validator.is_some() && !p.weak_validator;
    let validator = primary
        .probe
        .validator
        .clone()
        .filter(|_| strong(&primary.probe));
    // What the range map on disk was recorded against, before it is replaced:
    // spans held for one object must not be spliced into another.
    let (prev_size, prev_validator) = {
        let mut g = job.lock();
        let prev = (g.size, g.validator.take());
        g.size = known_size;
        g.validator = validator.clone();
        g.file_name = file_name;
        g.resolved_url = Some(format!(
            "{}://{}{}",
            primary.url.scheme,
            primary.url.authority(),
            primary.url.path
        ));
        if let Some(s) = known_size {
            g.progress.total_bytes = s;
        }
        prev
    };
    engine.emit(job, EV::HYDRA_EVENT_RESOLVED);

    let output = job.lock().output_path.clone();

    // No size or no ranges: nothing to partition, so one streaming GET is the
    // correct answer rather than a degraded one.
    let Some(size) = known_size.filter(|_| primary.probe.ranges) else {
        return stream_transfer(engine, job, &conn, &primary.target, &output, cancel).await;
    };

    // Splice only mirrors that agree on the object — a strong validator, or the
    // size a Metalink document attested, since independent mirrors cannot share
    // an `ETag` and the document's digest is what catches a wrong one.
    let usable: Vec<&Resolved> = match job.cfg.attested_size {
        Some(want) => resolved
            .iter()
            .filter(|r| r.probe.size == want && r.probe.ranges)
            .collect(),
        None if resolved.len() == 1 || !strong(&primary.probe) => vec![primary],
        None => resolved
            .iter()
            .filter(|r| {
                std::ptr::eq(*r, primary)
                    || (r.probe.size == size
                        && r.probe.ranges
                        && strong(&r.probe)
                        && r.probe.validator == primary.probe.validator)
            })
            .collect(),
    };
    // Every mirror disagreed with the document. Continuing on the primary would
    // assemble an object the publisher says is a different size, so it is a
    // failure with a reason rather than a silently wrong file.
    if usable.is_empty() {
        return Err(Detail {
            code: E::HYDRA_ERR_VERIFICATION as u32,
            message: format!(
                "the mirror list states {} bytes and no reachable mirror serves an object of \
                 that size with range support",
                job.cfg.attested_size.unwrap_or(size)
            ),
            ..Default::default()
        });
    }

    let mut held: Vec<(u64, u64)> = if job.cfg.resume {
        job.lock().held.clone()
    } else {
        Vec::new()
    };
    // A different size, or a different strong validator, is a different
    // object: splicing what is on disk into it would be the same corruption
    // the mirror gate refuses.
    let changed = prev_size.is_some_and(|s| s != size)
        || matches!((&prev_validator, &validator), (Some(a), Some(b)) if a != b);
    if changed {
        held.clear();
    }

    let ceiling = connections_for_size(engine.connection_ceiling(job.cfg.max_connections), size);
    // Plans for the mirrors that SURVIVED probing, in the order they survived
    // in. `usable` is a subset of the configured URL list, so the ranking has to
    // be carried across by URL or a dropped mirror shifts every rank after it —
    // the second-best host would be allocated as though it were the fourth.
    let plans: Vec<hya_core::SourcePlan> = if job.cfg.source_plans.is_empty() {
        crate::metalink::unranked(usable.len())
    } else {
        usable
            .iter()
            .map(|r| {
                job.cfg
                    .urls
                    .iter()
                    .position(|u| u == &r.requested)
                    .and_then(|i| job.cfg.source_plans.get(i).copied())
                    .unwrap_or_default()
            })
            .collect()
    };
    // The connection budget is split across the agreeing sources, never
    // multiplied by them: eight connections over three mirrors is eight
    // connections, not twenty-four. With a ranking in hand the split follows it,
    // and honours any ceiling a mirror stated for itself.
    let split: Vec<usize> = if job.cfg.source_plans.is_empty() {
        let v = split_connections(ceiling, usable.len());
        // `split_connections` drops empty entries, so pad back to one entry per
        // source: everything below indexes `split` by source.
        let mut out = vec![0usize; usable.len()];
        for (i, n) in v.into_iter().enumerate() {
            out[i] = n;
        }
        out
    } else {
        hya_core::plan::allocate(&plans, ceiling, ceiling, ceiling)
    };
    let seated: Vec<usize> = (0..usable.len()).filter(|&i| split[i] > 0).collect();
    let n_sources = seated.len().max(1);
    let per: Vec<usize> = seated.iter().map(|&i| split[i]).collect();
    let targets: Vec<Target> = seated.iter().map(|&i| usable[i].target.clone()).collect();
    let source_urls: Vec<String> = seated
        .iter()
        .map(|&i| {
            let r = usable[i];
            format!("{}://{}{}", r.url.scheme, r.url.authority(), r.url.path)
        })
        .collect();
    // Everything the split did not seat, best-ranked first. A mirror list names
    // far more sources than politeness authorises sockets for, and without a
    // bench that surplus is decoration: the transfer survives on the mirrors it
    // opened with or it does not.
    let bench: Vec<hya_net::Reserve> = {
        let mut idx: Vec<usize> = (0..usable.len()).filter(|&i| split[i] == 0).collect();
        idx.sort_by_key(|&i| (plans[i].priority, i));
        idx.into_iter()
            .map(|i| hya_net::Reserve {
                target: usable[i].target.clone(),
                plan: plans[i],
                host: usable[i].url.authority(),
            })
            .collect()
    };
    let n_conns: usize = per.iter().sum::<usize>().max(1);

    let sources: Vec<Source> = seated
        .iter()
        .map(|&i| {
            let r = usable[i];
            Source {
                caps: if job.cfg.attested_size.is_some() || strong(&r.probe) {
                    // A document that states the size and a content digest
                    // establishes agreement more strongly than an `ETag` does,
                    // and from outside the mirrors — so a mirror admitted on
                    // that evidence is a full source, not a pinned one.
                    Capability::Full
                } else {
                    Capability::NoValidator
                },
                delta_est: r.delta.max(1e-3),
                // The publisher's ranking, used once — for the first split,
                // before anything has been measured. See
                // `hya_core::sched::Source::priority`.
                priority: plans[i].priority,
                ..Source::default()
            }
        })
        .collect();

    let delta = seated
        .iter()
        .map(|&i| usable[i].delta)
        .fold(0.05f64, f64::max);
    let mut sched = Scheduler::new(size, sources, &per)
        // Scaled to the measured setup cost rather than fixed: a slow path
        // through a proxy deserves proportionally more patience than a LAN
        // mirror, with no constant to retune.
        .with_stall_timeout((12.0 * delta).clamp(4.0, 45.0));
    if !engine.cfg.range_stealing {
        // No knob turns repairs off, and none should: the deadband is the
        // mechanism. Widening it past any divergence the transfer can produce
        // is what "do not take ranges from a slow connection" means in the
        // scheduler's own terms.
        sched = sched.with_theta_scale(f64::MAX / 4.0);
    }
    if job.cfg.adaptive && n_conns > 1 {
        // Open the budget but start with one connection live; the in-band ramp
        // admits the rest only while the aggregate rate says they pay for
        // themselves. On a link the origin or an ISP shaper saturates at one or
        // two streams, a fixed eight divides the same capacity and adds setup
        // cost for it.
        sched.set_active_limit(1);
    }

    let mut already = 0u64;
    for &(lo, hi) in &held {
        let hi = hi.min(size);
        if lo < hi {
            sched.mark_done(lo, hi);
            already += hi - lo;
        }
    }

    let sink = Arc::new(SparseSink::create(&output, size).map_err(|e| err::from_io(&e))?);

    {
        let mut g = job.lock();
        g.state = S::HYDRA_JOB_DOWNLOADING as u32;
        g.progress.total_bytes = size;
        g.progress.bytes_downloaded = already;
        g.sources = source_urls
            .iter()
            .map(|u| SourceStat {
                url: u.clone(),
                ..SourceStat::default()
            })
            .collect();
    }
    crate::log::log_at!(
        engine,
        crate::abi::hydra_log_level_t::HYDRA_LOG_INFO,
        "job {}: {} byte(s) over {} connection(s) across {} source(s)",
        job.id,
        size,
        n_conns,
        n_sources
    );

    let mut observe = progress_observer(
        engine.clone(),
        job.clone(),
        size,
        already,
        n_sources,
        cancel.clone(),
    );
    let pace = pace_for(engine, job);
    let tick_ms = if engine.progress_interval_ms() >= 1000 {
        80
    } else {
        20
    };

    // Substitutions rename a source row as they happen: a view that keeps naming
    // the dead mirror attributes the replacement's throughput to a machine that
    // is not serving it.
    let sub_job = job.clone();
    let sub_engine = engine.clone();
    let mut on_sub = move |src: usize, r: &hya_net::Reserve| {
        {
            let mut g = sub_job.lock();
            if let Some(slot) = g.sources.get_mut(src) {
                slot.url.clone_from(&r.host);
                slot.active = true;
            }
        }
        crate::log::log_at!(
            sub_engine,
            crate::abi::hydra_log_level_t::HYDRA_LOG_WARN,
            "job {}: source {} failed; switched to reserve mirror {}",
            sub_job.id,
            src,
            r.host
        );
    };
    let result = hya_net::run_transfer_with_reserves(
        conn.clone(),
        targets,
        &per,
        size,
        sink.clone(),
        sched,
        tick_ms,
        &mut observe,
        pace,
        Some(cancel.clone()),
        hya_net::Bench::fixed(bench),
        Some(&mut on_sub),
    )
    .await;

    engine
        .metrics
        .bytes_written
        .fetch_add(sink.written.load(Ordering::Relaxed), Ordering::Relaxed);

    match result {
        Ok((_elapsed, requests)) => {
            engine
                .metrics
                .request_count
                .fetch_add(requests, Ordering::Relaxed);
            {
                let mut g = job.lock();
                g.held = vec![(0, size)];
                g.progress.bytes_downloaded = size;
            }
            // The document's `<pieces>`, verified here rather than in
            // `settle_completed` because repairing one needs the mirrors, and
            // this is the last point at which they are in hand.
            if let Some(m) = job.cfg.pieces.clone() {
                let targets: Vec<Target> = usable.iter().map(|r| r.target.clone()).collect();
                verify_pieces(engine, job, &conn, &targets, &output, m, cancel).await?;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(cancelled()),
        Err(e) => Err(err::from_io(&e)),
    }
}

/// Check every chunk against a Metalink `<pieces>` manifest and refetch the
/// failures from a different mirror.
///
/// # Why this is worth a second pass over the file
///
/// A whole-file digest answers one question — is this object right — and when
/// the answer is no, the only remedy it licenses is downloading all of it again.
/// On a multi-gigabyte image over a mirror set where one node is serving a stale
/// build, that is the difference between finishing and not.
///
/// A piece list localises the fault. The manifest names the chunk, the chunk is
/// refetched from a mirror that did not serve it, and the refetched bytes are
/// checked against the same digest before being accepted — so a second corrupt
/// copy is not taken on faith merely because it was asked for twice.
///
/// `Trust::Advertised`, always, however the document arrived: nothing here has
/// authenticated it. Detection and targeted refetch are self-correcting and are
/// allowed; naming erasure positions for a parity decode is not. That cap is
/// enforced inside `ChunkVerifier::new`, which also refuses to grant trust to a
/// SHA-1 grid — the algorithm most Metalink 3.0 documents actually use.
async fn verify_pieces(
    engine: &Arc<Engine>,
    job: &Arc<Job>,
    conn: &Arc<TlsCapableConnector>,
    targets: &[Target],
    output: &str,
    m: hya_net::manifest::Manifest,
    cancel: &Arc<AtomicBool>,
) -> Result<(), Detail> {
    use hya_net::manifest::{ChunkVerifier, Trust};

    let size = m.object.size;
    let mut v = ChunkVerifier::new(m, Trust::Advertised);
    {
        let path = output.to_string();
        let mut f = std::fs::File::open(&path).map_err(|e| err::from_io(&e))?;
        // Hashing the object is CPU- and disk-bound; keeping it off a runtime
        // worker is the same reason `settle_completed` spawns its digest.
        v = tokio::task::spawn_blocking(move || v.write_reader(&mut f).map(|()| v))
            .await
            .map_err(|e| Detail {
                code: E::HYDRA_ERR_INTERNAL as u32,
                message: format!("chunk verification task failed: {e}"),
                ..Default::default()
            })?
            .map_err(|e| err::from_io(&e))?;
    }
    if v.all_verified() {
        crate::log::log_at!(
            engine,
            crate::abi::hydra_log_level_t::HYDRA_LOG_INFO,
            "job {}: all {} chunk(s) verified against the mirror list",
            job.id,
            v.verified_count()
        );
        return Ok(());
    }

    let bad = v.failed_indices().to_vec();
    crate::log::log_at!(
        engine,
        crate::abi::hydra_log_level_t::HYDRA_LOG_WARN,
        "job {}: {} chunk(s) failed their digest; refetching",
        job.id,
        bad.len()
    );
    let sink = Arc::new(SparseSink::create(output, size).map_err(|e| err::from_io(&e))?);
    for (nth, idx) in bad.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let (lo, hi) = v.manifest().span(idx);
        // Rotate through the mirrors, starting past the primary. Which host
        // served the corrupt chunk is unknowable from here, so a FIXED
        // alternate is a coin-flip that repeats itself: if the alternate is the
        // bad mirror, every refetch fails and the repair dies on its first
        // candidate. Rotation costs nothing and puts each retry somewhere new.
        let t = targets[(1 + nth) % targets.len()].clone();
        hya_net::fetch_range_retry(
            conn.clone(),
            t,
            lo,
            hi,
            sink.clone(),
            job.cfg.max_retries.max(1),
            30.0,
        )
        .await
        .map_err(|e| Detail {
            code: E::HYDRA_ERR_VERIFICATION as u32,
            message: format!("chunk {idx} refetch failed: {e}"),
            ..Default::default()
        })?;
        let mut fresh = vec![0u8; (hi - lo) as usize];
        {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut f = std::fs::File::open(output).map_err(|e| err::from_io(&e))?;
            f.seek(SeekFrom::Start(lo)).map_err(|e| err::from_io(&e))?;
            f.read_exact(&mut fresh).map_err(|e| err::from_io(&e))?;
        }
        v.retry(idx);
        if !v.write(lo, &fresh).is_empty() {
            return Err(Detail {
                code: E::HYDRA_ERR_CHECKSUM as u32,
                message: format!(
                    "chunk {idx} [{lo},{hi}) still fails its digest after refetch: the mirrors \
                     are serving bytes the document does not describe"
                ),
                ..Default::default()
            });
        }
    }
    Ok(())
}

/// Split a connection budget across sources, never exceeding it in total.
fn split_connections(total: usize, sources: usize) -> Vec<usize> {
    let sources = sources.max(1);
    let total = total.max(1);
    let base = total / sources;
    let extra = total % sources;
    (0..sources)
        .map(|i| base + usize::from(i < extra))
        .filter(|&n| n > 0)
        .collect()
}

/// The rate limiters this job's transfer answers to.
///
/// Both are attached, and both are read live on every read:
///
/// * the engine's limiter is shared by every job, so an engine-wide cap is a
///   true aggregate however many jobs are running;
/// * the job's own limiter binds this job alone.
///
/// Whichever is lower at that instant is what the transfer moves at. Neither is
/// resolved once at start: `hydra_engine_set_max_bytes_per_second` and
/// `hydra_job_set_max_bytes_per_second` both take effect on a transfer that is
/// already running, in either direction, including on a job that began with no
/// cap at all. Picking one limiter here — as this used to, returning
/// `Pace::unlimited()` whenever neither cap was set at start — froze that
/// decision for the life of the transfer and made both setters inert.
fn pace_for(engine: &Arc<Engine>, job: &Arc<Job>) -> Pace {
    Pace::pair(engine.limiter.clone(), job.limiter.clone())
}

/// The callback `run_transfer_cancellable` invokes once per scheduler tick.
///
/// Two rates are computed and only one is published. The scheduler's own
/// per-connection estimates are a control signal: they twitch by design,
/// because that is what makes them useful for detecting divergence. A readout
/// built from them jumps every refresh. What is published instead is bytes over
/// wall clock through a 1.5-second time constant, which reads as a steady
/// counter.
fn progress_observer(
    engine: Arc<Engine>,
    job: Arc<Job>,
    size: u64,
    already: u64,
    n_sources: usize,
    cancel: Arc<AtomicBool>,
) -> impl FnMut(&Scheduler, u64) + Send {
    let interval = Duration::from_millis(engine.progress_interval_ms());
    let started = Instant::now();
    let mut last_emit = Instant::now() - Duration::from_secs(3600);
    let mut meter = RateMeter::new(1.5);
    // Per connection: (range start, cursor, bytes credited). The scheduler
    // reports a position, not a total, so the delta has to be accumulated.
    let mut per_conn: std::collections::HashMap<usize, (u64, u64, u64)> =
        std::collections::HashMap::new();
    let mut counted_bytes = already;
    let mut reclaims_credited = 0u64;

    move |s: &Scheduler, done: u64| {
        for j in 0..s.n_conns() {
            let e = per_conn.entry(j).or_insert((0, 0, 0));
            if let Some((lo, pos, _hi)) = s.conn_range(j) {
                if e.0 == lo && pos >= e.1 {
                    e.2 += pos - e.1;
                } else if pos > lo {
                    e.2 += pos - lo;
                }
                *e = (lo, pos, e.2);
            }
        }
        // The transfer's last call after a stop must land whatever the
        // interval says: the spans it reports are what the resume starts from.
        let stopping = cancel.load(Ordering::Relaxed);
        if !stopping && last_emit.elapsed() < interval && done < size {
            return;
        }
        last_emit = Instant::now();

        // `held_ranges` allocates, so it is computed at the publish cadence and
        // not at the 50 Hz tick rate.
        let held = s.held_ranges();
        let elapsed = started.elapsed().as_secs_f64();
        let smoothed = meter.sample(elapsed, done);
        let elapsed = elapsed.max(1e-3);
        let active_conns = (0..s.n_conns())
            .filter(|&j| s.conn_range(j).is_some())
            .count() as u32;
        let in_flight = active_conns as usize;

        engine
            .metrics
            .bytes_received
            .fetch_add(done.saturating_sub(counted_bytes), Ordering::Relaxed);
        counted_bytes = done;
        credit_increase(
            &engine.metrics.stall_count,
            &mut reclaims_credited,
            s.stats.reclaims,
        );

        let mut g = job.lock();
        g.progress.bytes_downloaded = done;
        g.progress.total_bytes = size;
        g.progress.bytes_per_second = smoothed as u64;
        g.progress.average_bytes_per_second =
            (done.saturating_sub(already) as f64 / elapsed) as u64;
        g.progress.eta_seconds = meter
            .eta_secs(size.saturating_sub(done.min(size)))
            .map_or(0, |eta| eta as u64);
        g.progress.active_connections = active_conns;
        g.progress.active_sources = n_sources as u32;
        g.progress.completed_ranges = held.len() as u32;
        g.progress.total_ranges = (held.len() + in_flight) as u32;
        g.progress.retry_count = s.stats.requests.saturating_sub(s.n_conns() as u64);
        g.progress.stall_count = s.stats.reclaims;
        g.held = held;

        // Per-source rollup for the experimental source API.
        for (i, st) in g.sources.iter_mut().enumerate() {
            let mut bytes = 0u64;
            let mut rate = 0.0f64;
            let mut conns = 0u32;
            for j in 0..s.n_conns() {
                if s.conn_source(j) == i {
                    bytes += per_conn.get(&j).map(|e| e.2).unwrap_or(0);
                    rate += s.conn_rate(j);
                    if s.conn_range(j).is_some() {
                        conns += 1;
                    }
                }
            }
            st.bytes = bytes;
            st.rate = rate as u64;
            st.conns = conns;
            st.active = conns > 0;
        }
        let progress = g.progress;
        let state = g.state;
        drop(g);

        engine.events.push(crate::abi::hydra_event_t {
            kind: EV::HYDRA_EVENT_PROGRESS,
            state: crate::err::to_state(state),
            job_id: job.id,
            progress,
            error: E::HYDRA_OK,
            http_status: 0,
            os_error: 0,
            reserved: 0,
            timestamp_ms: now_ms(),
            dropped_events: 0,
        });
    }
}

/// One GET, streamed to the destination, for a server offering no ranges or no
/// size.
async fn stream_transfer(
    engine: &Arc<Engine>,
    job: &Arc<Job>,
    conn: &Arc<TlsCapableConnector>,
    target: &Target,
    output: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<(), Detail> {
    {
        let mut g = job.lock();
        g.state = S::HYDRA_JOB_DOWNLOADING as u32;
        // Nothing here can be resumed: without ranges there is no second
        // request to make, so a stopped transfer starts over. Recording no held
        // spans is what makes that true rather than merely hoped for.
        g.held.clear();
        g.progress.bytes_downloaded = 0;
    }
    crate::log::log_at!(
        engine,
        crate::abi::hydra_log_level_t::HYDRA_LOG_INFO,
        "job {}: no range support or unknown size; single stream",
        job.id
    );

    // The byte counter the fetch keeps as it writes. Without it this path
    // reported nothing at all until the last byte arrived — on a large object
    // that is an hour of a caller seeing zero and assuming a hung transfer.
    let written = Arc::new(AtomicU64::new(0));
    let pace = pace_for(engine, job);
    let fut = hya_net::fetch_streaming_observed(
        conn.as_ref(),
        target,
        output,
        &written,
        Some(cancel.as_ref()),
        &pace,
    );
    tokio::pin!(fut);
    let interval = Duration::from_millis(engine.progress_interval_ms());
    let began = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(3600);
    loop {
        tokio::select! {
            r = &mut fut => {
                return match r {
                    Ok(n) => {
                        engine.metrics.bytes_received.fetch_add(n, Ordering::Relaxed);
                        engine.metrics.bytes_written.fetch_add(n, Ordering::Relaxed);
                        let mut g = job.lock();
                        g.size = Some(n);
                        g.progress.bytes_downloaded = n;
                        g.progress.total_bytes = n;
                        Ok(())
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Err(cancelled()),
                    Err(e) => Err(err::from_io(&e)),
                };
            }
            _ = tokio::time::sleep(CANCEL_POLL) => {
                if cancel.load(Ordering::Relaxed) {
                    // Dropping the future closes the socket. The partial file
                    // stays where it is; `Stop::CancelRemove` is what deletes it.
                    return Err(cancelled());
                }
                if last.elapsed() >= interval {
                    last = Instant::now();
                    let done = written.load(Ordering::Relaxed);
                    let secs = began.elapsed().as_secs_f64().max(1e-3);
                    let rate = (done as f64 / secs) as u64;
                    {
                        let mut g = job.lock();
                        g.progress.bytes_downloaded = done;
                        g.progress.bytes_per_second = rate;
                        g.progress.average_bytes_per_second = rate;
                        // No total to subtract from: this path runs precisely
                        // because the server would not state a size, so an ETA
                        // would be an invention.
                        g.progress.eta_seconds = 0;
                        g.progress.active_connections = 1;
                        g.progress.active_sources = 1;
                    }
                    engine.emit(job, EV::HYDRA_EVENT_PROGRESS);
                }
            }
        }
    }
}

/// One FTP transfer, on one connection, resuming from the contiguous prefix.
async fn ftp_transfer(
    engine: &Arc<Engine>,
    job: &Arc<Job>,
    conn: &Arc<TlsCapableConnector>,
    creds: &Creds,
    cancel: &Arc<AtomicBool>,
) -> Result<(), Detail> {
    use hya_net::scheme::Endpoint;

    let u = Url::parse(&job.cfg.urls[0]).map_err(|e| Detail {
        code: E::HYDRA_ERR_INVALID_URL as u32,
        message: e,
        ..Default::default()
    })?;
    let fetcher = hya_net::scheme::for_scheme("ftp", conn.clone()).ok_or_else(|| Detail {
        code: E::HYDRA_ERR_UNSUPPORTED as u32,
        message: "this build cannot serve ftp://".into(),
        ..Default::default()
    })?;
    // Credentials from the URL, then from the job configuration. The URL wins
    // because it is the more specific statement of intent.
    let user = u.user.clone().or_else(|| creds.username.clone());
    let pass = u.pass.clone().or_else(|| creds.password.clone());
    let ep =
        Endpoint::new(&u.host, u.port, &u.path).with_credentials(user.as_deref(), pass.as_deref());

    let sp = fetcher.probe(&ep).await.map_err(|e| err::from_io(&e))?;
    if sp.size == 0 {
        return Err(Detail {
            code: E::HYDRA_ERR_PROTOCOL as u32,
            message: format!("ftp://{} did not report a size", u.host),
            ..Default::default()
        });
    }
    let size = sp.size;
    let output = job.lock().output_path.clone();
    let prev_size = {
        let mut g = job.lock();
        let prev = g.size;
        g.size = Some(size);
        g.validator = None;
        g.file_name = u.file_name();
        g.resolved_url = Some(job.cfg.urls[0].clone());
        g.progress.total_bytes = size;
        prev
    };
    engine.emit(job, EV::HYDRA_EVENT_RESOLVED);

    // FTP resumes with REST, which names one offset — so only a contiguous
    // prefix is usable, not the arbitrary span set an HTTP transfer leaves.
    // A different size is a different object, and its prefix is not ours.
    let start = if job.cfg.resume && prev_size.is_none_or(|s| s == size) {
        contiguous_prefix(&job.lock().held)
    } else {
        0
    }
    .min(size);

    let sink = Arc::new(SparseSink::create(&output, size).map_err(|e| err::from_io(&e))?);
    {
        let mut g = job.lock();
        g.state = S::HYDRA_JOB_DOWNLOADING as u32;
        g.progress.bytes_downloaded = start;
        g.progress.active_connections = 1;
        g.progress.active_sources = 1;
        g.sources = vec![SourceStat {
            url: job.cfg.urls[0].clone(),
            active: true,
            conns: 1,
            ..SourceStat::default()
        }];
    }
    crate::log::log_at!(
        engine,
        crate::abi::hydra_log_level_t::HYDRA_LOG_INFO,
        "job {}: ftp, one connection, resuming at {}",
        job.id,
        start
    );

    if start >= size {
        return Ok(());
    }

    let fut = fetcher.fetch_range(&ep, start, size, sink.clone());
    tokio::pin!(fut);
    let interval = Duration::from_millis(engine.progress_interval_ms());
    let began = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(3600);
    loop {
        tokio::select! {
            r = &mut fut => {
                return match r {
                    Ok(()) => {
                        engine.metrics.bytes_written.fetch_add(size - start, Ordering::Relaxed);
                        engine.metrics.bytes_received.fetch_add(size - start, Ordering::Relaxed);
                        let mut g = job.lock();
                        g.held = vec![(0, size)];
                        g.progress.bytes_downloaded = size;
                        Ok(())
                    }
                    Err(e) => Err(err::from_io(&e)),
                };
            }
            _ = tokio::time::sleep(CANCEL_POLL) => {
                if cancel.load(Ordering::Relaxed) {
                    // The bytes already written stay valid at their offsets, and
                    // the contiguous prefix is what a later run resumes from.
                    let done = start + sink.written.load(Ordering::Relaxed);
                    let mut g = job.lock();
                    g.held = vec![(0, done.min(size))];
                    g.progress.bytes_downloaded = done.min(size);
                    return Err(cancelled());
                }
                if last.elapsed() >= interval {
                    last = Instant::now();
                    let done = (start + sink.written.load(Ordering::Relaxed)).min(size);
                    let secs = began.elapsed().as_secs_f64().max(1e-3);
                    let rate = (done.saturating_sub(start)) as f64 / secs;
                    {
                        let mut g = job.lock();
                        g.progress.bytes_downloaded = done;
                        g.progress.bytes_per_second = rate as u64;
                        g.progress.average_bytes_per_second = rate as u64;
                        g.progress.eta_seconds = if rate > 1024.0 {
                            ((size - done) as f64 / rate) as u64
                        } else {
                            0
                        };
                        g.progress.completed_ranges = 1;
                        g.progress.total_ranges = 1;
                        g.held = vec![(0, done)];
                    }
                    engine.emit(job, EV::HYDRA_EVENT_PROGRESS);
                }
            }
        }
    }
}

/// How many bytes are present from offset zero without a hole.
fn contiguous_prefix(held: &[(u64, u64)]) -> u64 {
    let mut spans: Vec<(u64, u64)> = held.to_vec();
    spans.sort_unstable();
    let mut end = 0u64;
    for (lo, hi) in spans {
        if lo > end {
            break;
        }
        end = end.max(hi);
    }
    end
}

/// The lock used by [`crate::persist`] to serialise state writes.
pub(crate) static PERSIST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_budget_is_split_not_multiplied() {
        assert_eq!(split_connections(8, 3), vec![3, 3, 2]);
        assert_eq!(split_connections(8, 3).iter().sum::<usize>(), 8);
        assert_eq!(split_connections(2, 5), vec![1, 1]);
        assert_eq!(split_connections(1, 1), vec![1]);
    }

    #[test]
    fn a_contiguous_prefix_stops_at_the_first_hole() {
        assert_eq!(contiguous_prefix(&[(0, 10), (10, 20)]), 20);
        assert_eq!(contiguous_prefix(&[(10, 20)]), 0);
        assert_eq!(contiguous_prefix(&[(0, 10), (20, 30)]), 10);
        assert_eq!(contiguous_prefix(&[]), 0);
        // Out of order and overlapping, which is what an interrupted
        // multi-connection transfer leaves behind.
        assert_eq!(contiguous_prefix(&[(5, 15), (0, 8), (30, 40)]), 15);
    }

    #[test]
    fn small_objects_are_not_split_below_a_quarter_megabyte_per_connection() {
        assert_eq!(connections_for_size(8, 31), 1);
        assert_eq!(connections_for_size(8, MIN_BYTES_PER_CONNECTION * 2 - 1), 1);
        assert_eq!(connections_for_size(8, MIN_BYTES_PER_CONNECTION * 2), 2);
        assert_eq!(connections_for_size(8, 5 << 20), 8);
        assert_eq!(connections_for_size(1, 0), 1);
    }

    #[test]
    fn a_running_total_is_credited_by_its_increase_only() {
        let counter = AtomicU64::new(0);
        let (mut a, mut b) = (0u64, 0u64);
        credit_increase(&counter, &mut a, 3);
        credit_increase(&counter, &mut b, 5);
        credit_increase(&counter, &mut a, 4);
        assert_eq!(counter.load(Ordering::Relaxed), 9, "3 + 5 + (4 - 3)");
        // A total that went backwards (a restarted transfer) adds nothing.
        credit_increase(&counter, &mut a, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 9);
    }

    #[test]
    fn only_transport_failures_are_retried() {
        assert!(is_retryable(E::HYDRA_ERR_TIMEOUT as u32));
        assert!(is_retryable(E::HYDRA_ERR_CONNECTION as u32));
        // Re-downloading cannot fix any of these.
        assert!(!is_retryable(E::HYDRA_ERR_CHECKSUM as u32));
        assert!(!is_retryable(E::HYDRA_ERR_PERMISSION as u32));
        assert!(!is_retryable(E::HYDRA_ERR_NO_SPACE as u32));
        assert!(!is_retryable(E::HYDRA_ERR_INVALID_URL as u32));
    }
}

//! The scheduler kernel: pure state machine, no I/O, no clock, no allocation
//! in the steady state.
//!
//! The caller drives it: feed observations (`on_bytes`, `on_complete`), call
//! `tick(now)`, and act on the returned `Action`s. This is what lets the same
//! code run under the discrete-event simulator and under real HTTP.
//!
//! Implements dynamic range partitioning, divergence-triggered steal-to-equalize,
//! work-conserving assignment, queue dispatch, stall reclamation, and greedy concurrency.

use crate::intervals::{IntervalSet, Range};

/// Minimum steal quantum. A range rebalance smaller than this is not worth request overhead.
pub const STEAL_QUANTUM: u64 = 64 * 1024;

/// Bounded repairs per tick, so a tick is O(R * n).
const MAX_REPAIRS_PER_TICK: usize = 4;

/// EWMA weight on the newest goodput sample.
const RATE_ALPHA: f64 = 0.3;
/// Rate samples a connection must have produced before repair may move work
/// on to it or off it.
///
/// Each sample is one `RATE_WINDOW` of arrivals, so this is the first
/// `REPAIR_WARM_SAMPLES * RATE_WINDOW` seconds of delivery on a request — long
/// enough that the estimate has left slow start behind, short enough that a
/// connection genuinely slower than its peers is noticed within the first
/// second. See `pick_victim_taker`.
const REPAIR_WARM_SAMPLES: u32 = 3;
/// How long a divergence between two BUSY connections must persist before it is
/// acted on.
///
/// The rate estimate is a smoothed window, and smoothing lags: two connections
/// on one fair path show apparent divergences of 15-20% for a window or two
/// while their estimates catch up with each other, and the repair test cannot
/// tell that from a connection that is genuinely slower. Traced on a uniform
/// four-connection transfer with perfectly fair TCP (four plain `curl` ranges
/// finish within 10 ms of each other): the good runs stole 9 and 17 MB slivers
/// at t=0.8 s and t=1.8 s on exactly such a blip, and each sliver became an
/// orphan that cost a fresh round trip in the tail; the bad runs cascaded into
/// twenty shrinks, each one changing the ETAs that justified the next. Lag
/// clears within a few windows. Slowness does not. Waiting this long separates
/// them, and a repair that is worth making in second three is still worth
/// making in second four.
///
/// Not applied to a victim the detector has graded as collapsing, nor to an
/// idle taker: the first is measured evidence, the second is free capacity.
const REPAIR_PERSIST: f64 = 3.0 * RATE_WINDOW;
/// The same wait expressed in setup costs, whichever is longer. Six is a few
/// round trips past the point where two flows that started together have
/// left slow start; see the repair loop in `tick`.
const REPAIR_PERSIST_DELTAS: f64 = 6.0;

/// Minimum wall clock a rate sample must span, in seconds.
///
/// Below this the quotient is dominated by socket buffering rather than by the
/// link: consecutive `read()` calls draining one already-arrived TCP window return
/// in microseconds and imply a rate the network never achieved. 200 ms is long
/// enough to average over several windows and short enough that a genuine collapse
/// is still graded within the stall timeout.
const RATE_WINDOW: f64 = 0.2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Issue `GET` with `Range: bytes=lo-(hi-1)` on this connection.
    Request { conn: usize, range: Range },
    /// Stop reading this connection's current response; its range was reclaimed.
    Cancel { conn: usize },
    /// The far end of this connection's in-flight range moved DOWN to `hi`: a
    /// repair handed the tail `[hi, old_hi)` to another connection. Stop reading
    /// at `hi`.
    ///
    /// # Why this action has to exist
    ///
    /// The whole claim of this scheduler is that shrinking a laggard's range is
    /// free, because an HTTP range request names both ends and the far end is
    /// enforced by the client. That is true of the protocol. It was NOT true of
    /// this implementation: the repair below moved `conns[vi].range` and emitted
    /// nothing, while the transport's fetch loop runs `while off < hi` against
    /// the `hi` it captured when the request was spawned. The victim therefore
    /// kept pulling the bytes it had just been relieved of, at the same time as
    /// the taker pulled them, over the same bottleneck.
    ///
    /// So each repair cost roughly one stolen span of duplicated traffic instead
    /// of nothing, and since the duplicate traffic slowed the honest
    /// connections, it manufactured the very divergence that triggers a repair.
    /// That positive feedback loop is the measured "repair storm": at n=8 on a
    /// stationary 5.3 MB transfer, 32-49 repairs where the correct count is 0,
    /// with in-run throughput decaying 439 -> 306 KiB/s.
    ///
    /// A caller that ignores this action is not merely leaving an optimisation
    /// on the table; it reintroduces the storm.
    Shrink { conn: usize, hi: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    /// Ranges honoured, length known, strong validator: full scheduling.
    Full,
    /// Ranges honoured but no validator: partition, but pin to one source.
    NoValidator,
    /// Ranges ignored or unsupported: race whole-object fetches.
    Race,
    /// Length unknown: single stream per source, no range arithmetic.
    Stream,
}

/// Preference value meaning "the publisher stated no preference".
///
/// The scale is RFC 5854's: 1 is best, larger is worse. See [`Source::priority`].
pub const NO_PRIORITY: u32 = 999_999;

#[derive(Clone, Debug)]
pub struct Source {
    pub caps: Capability,
    /// Per-connection goodput ceiling estimate, bytes/s.
    pub gamma_est: f64,
    /// Per-source shaping cap estimate, bytes/s.
    pub rho_est: f64,
    /// Measured request setup cost, seconds.
    pub delta_est: f64,
    /// Suspended until this time (429/503 Retry-After, or stall backoff).
    pub suspended_until: f64,
    /// Consecutive stalls observed on this source; drives exponential backoff.
    pub consecutive_stalls: u32,
    /// Publisher-stated preference: 1 is best, [`NO_PRIORITY`] means unranked.
    ///
    /// # Why this is a PRIOR and nothing more
    ///
    /// Everything else in this struct is measured. This is not: it is what a
    /// mirror list (Metalink `priority`/`preference`, an RFC 6249 `Link ... pri=`)
    /// says about a source before a single byte has moved. It is worth having
    /// because the first split has to be made from *something*, and a
    /// publisher's ranking is a better guess than an arbitrary order — it
    /// usually encodes geography, bandwidth, and whether the host is a CDN or a
    /// volunteer.
    ///
    /// It is worth having ONLY as a prior. A ranking cannot know that the
    /// preferred mirror is currently overloaded, and this scheduler exists
    /// precisely because that is discoverable at runtime and correctable for
    /// free. So `priority` biases [`Scheduler::initial_split`] and nothing else:
    /// once `gamma_est` and the per-connection rate estimates have real samples
    /// in them, repair decisions are made on measurement, and a highly-ranked
    /// mirror that is slow loses its work exactly as fast as an unranked one
    /// would. Letting the prior persist would be strictly worse than having no
    /// ranking at all, because it would defend the wrong source against
    /// evidence.
    pub priority: u32,
    /// False once a source has been retired: it is out of the transfer for good.
    ///
    /// Distinct from `suspended_until`, which is a pause with an end. A retired
    /// source is one a caller has replaced or abandoned — an unreachable host, a
    /// mirror serving the wrong object — and it must never be handed work again,
    /// however long the transfer runs. Conflating the two is how a dead mirror
    /// gets retried every backoff for the life of the download.
    pub live: bool,
}

impl Default for Source {
    fn default() -> Self {
        Source {
            caps: Capability::Full,
            gamma_est: 0.0,
            rho_est: f64::INFINITY,
            delta_est: 0.05,
            suspended_until: 0.0,
            consecutive_stalls: 0,
            priority: NO_PRIORITY,
            live: true,
        }
    }
}

impl Source {
    /// Relative share of the first split this source's ranking argues for.
    ///
    /// `1 / priority`, so rank 1 gets twice rank 2 and eight times rank 8, and
    /// every unranked source gets the same small share as every other. The exact
    /// curve is a judgement call and is deliberately gentle: it is a prior over a
    /// quantity nobody has measured yet, and the correction costs one repair.
    /// A steeper curve would concentrate the object on one mirror and turn the
    /// publisher's guess into a single point of failure — the opposite of what a
    /// mirror list is for.
    pub fn priority_weight(&self) -> f64 {
        1.0 / (self.priority.max(1) as f64)
    }

    /// May this source be given work right now?
    ///
    /// Retirement and suspension are separate conditions with one answer, and
    /// every assignment path must ask the combined question. A path that checks
    /// only `suspended_until` hands work to a retired mirror the moment its last
    /// backoff expires — which is exactly the host a caller retired it for.
    #[inline]
    pub fn usable_at(&self, now: f64) -> bool {
        self.live && now >= self.suspended_until
    }
}

#[derive(Clone, Debug)]
struct Conn {
    source: usize,
    /// Active range and how far into it we are.
    range: Option<Range>,
    pos: u64,
    /// One-slot pipeline: a range handed over by a repair.
    queued: Option<Range>,
    rate_est: f64,
    /// Changepoint detector. `rate_est` remains the smoothed rate used for ETA
    /// projection; this grades the connection so repair can pre-empt a collapse
    /// instead of waiting for the stall timeout (see `detect.rs`).
    detector: crate::detect::CollapseDetector,
    last_progress: f64,
    setup_end: f64,
    /// When the request this connection is running now was issued.
    ///
    /// Arrivals older than this belong to a request that has been superseded —
    /// reclaimed after a stall, cancelled, or failed — and must not be credited,
    /// even when they land exactly at the cursor. See `on_bytes_at`.
    started_at: f64,
    stalled: bool,
    /// Bytes and wall clock accumulated since the last RATE sample.
    ///
    /// Rate is measured over a fixed WINDOW, not per arrival. An arrival is one
    /// `read()` return, and a read served from the socket's already-buffered data
    /// completes in microseconds, so `bytes/dt` for that arrival measures memcpy
    /// speed rather than network speed — observed as 128 MiB/s on a connection
    /// whose link was doing well under 1 MiB/s.
    ///
    /// That is not merely a cosmetic display bug. Those inflated samples raise the
    /// detector's reference level, after which every honest sample looks like a
    /// collapse against it, and the CUSUM grades a perfectly healthy connection
    /// `Degraded` — which is why all eight connections of a working transfer
    /// showed as `bad`. Byte accounting stays exactly per-arrival (coverage must
    /// be exact); only the rate estimate is windowed.
    rate_acc_bytes: u64,
    rate_acc_dt: f64,
}

impl Conn {
    fn new(source: usize) -> Self {
        Conn {
            source,
            range: None,
            pos: 0,
            queued: None,
            rate_est: 0.0,
            detector: crate::detect::CollapseDetector::new(),
            rate_acc_bytes: 0,
            rate_acc_dt: 0.0,
            last_progress: 0.0,
            setup_end: 0.0,
            started_at: f64::NEG_INFINITY,
            stalled: false,
        }
    }

    #[inline]
    fn busy(&self) -> bool {
        self.range.map(|r| self.pos < r.hi).unwrap_or(false)
    }

    /// Bytes still owed on the active range plus anything pipelined.
    #[inline]
    fn outstanding(&self) -> u64 {
        let active = self
            .range
            .map(|r| r.hi.saturating_sub(self.pos))
            .unwrap_or(0);
        active + self.queued.map(|r| r.len()).unwrap_or(0)
    }

    /// Projected seconds to drain. A stalled or unmeasured connection projects
    /// to infinity so it is always chosen as the repair victim.
    fn eta(&self) -> f64 {
        let out = self.outstanding();
        if out == 0 {
            return 0.0;
        }
        if self.rate_est <= 0.0 {
            return f64::INFINITY;
        }
        out as f64 / self.rate_est
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub requests: u64,
    pub repairs: u64,
    pub reclaims: u64,
    pub bytes_held: u64,
}

/// Why the active connection count sits below the budget.
///
/// The scheduler does not act on this. It is carried here because every front
/// end observes a transfer through a callback that receives `&Scheduler` and
/// nothing else, and a dormant connection row that cannot say WHY it is dormant
/// reads as a dropped connection. That is how a transfer behaving correctly —
/// the in-band search measured two connections as slower than one and settled at
/// one, matching `curl` on the same object — came to be filed as a bug: the rows
/// said "waiting (connection limit)" and nothing said a measurement had been
/// taken or what it concluded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum LimitReason {
    /// Nothing has lowered the count; every connection may be admitted.
    #[default]
    None,
    /// The in-band search is still measuring whether more connections pay.
    Measuring,
    /// The search finished: `tried` connections delivered `tried_rate` bytes/s
    /// against `chosen` at `chosen_rate`, and the smaller count won. Rates are
    /// zero when that level was never measured directly.
    Measured {
        chosen: usize,
        chosen_rate: f64,
        tried: usize,
        tried_rate: f64,
    },
    /// The origin refused requests (`429`/`503`) beyond `serving` at once.
    Refused { serving: usize },
    /// The origin accepted connections beyond `serving` and then served them
    /// nothing for a whole stall timeout while the others streamed.
    Starved { serving: usize },
}

pub struct Scheduler {
    size: u64,
    unassigned: IntervalSet,
    held: u64,
    conns: Vec<Conn>,
    sources: Vec<Source>,
    /// Repair deadband scale; theta = scale * sqrt(delta * T_rem / n).
    /// Reused index buffer for the per-tick stalled-connection scan.
    ///
    /// The scan runs 50 times a second at the default tick and allocated a fresh `Vec`
    /// each time, to hold at most `n_conns` indices. Reusing one buffer costs a field
    /// and removes the allocation from the hot loop.
    scratch_idx: Vec<usize>,
    /// Reused index buffer for the per-tick assignment visit order.
    ///
    /// Separate from `scratch_idx` because the stalled scan above is still
    /// holding that one when this is built, and for the same reason it exists:
    /// this runs every tick, and a fresh `Vec` per tick is the allocation the
    /// module header promises not to make.
    scratch_order: Vec<usize>,
    theta_scale: f64,
    stall_timeout: f64,
    /// How many connections may hold work at once. Adjustable mid-transfer so the
    /// concurrency search can run on the real transfer rather than on probe
    /// traffic; see `set_active_limit`.
    active_limit: usize,
    /// The largest `active_limit` this transfer can still reach.
    ///
    /// Distinct from `conns.len()`, which is the connection BUDGET, because the two
    /// stopped meaning the same thing once the transport learned to lower its own
    /// concurrency: an origin answering `429` teaches the transfer a ceiling well
    /// below the budget, and nothing above that ceiling will ever be admitted.
    ///
    /// Assignment reads it. While concurrency can still grow, an idle connection is
    /// handed a share of the remaining work rather than all of it, so the
    /// connections admitted later find work waiting instead of having to steal.
    /// Sizing that share against the budget when the ceiling is a fraction of it
    /// hands out shares a fraction of the right size, and the transfer pays a
    /// request — a round trip, and on a refused origin a fresh handshake — for
    /// every one of them. Measured on a hermetic origin that serves two connections
    /// and refuses the rest: 31 requests to deliver a 16 MB object at `-x 8`
    /// against 2 at `-x 2`, and the difference was almost entirely first-byte
    /// latency.
    conn_ceiling: usize,
    /// Why `active_limit` is where it is, for the UI. See [`LimitReason`].
    limit_reason: LimitReason,
    /// `HYDRA_TRACE_REPAIR` was set at construction: print every repair decision.
    trace_repair: bool,
    /// The connection the divergence test has been naming as victim, and since
    /// when. A repair between two busy connections waits for the same victim to
    /// stay divergent for `REPAIR_PERSIST`; see the repair loop in `tick`.
    repair_candidate: Option<(usize, f64)>,
    /// When false, victim selection ignores detector health and ranks purely by
    /// projected ETA (the pre-detector behaviour). Exists so the detector's
    /// contribution can be A/B measured rather than assumed.
    health_ranking: bool,
    started: bool,
    pub stats: Stats,
}

impl Scheduler {
    pub fn new(size: u64, sources: Vec<Source>, conns_per_source: &[usize]) -> Self {
        let mut conns = Vec::new();
        for (i, &k) in conns_per_source.iter().enumerate() {
            for _ in 0..k {
                conns.push(Conn::new(i));
            }
        }
        Scheduler {
            size,
            unassigned: IntervalSet::full(size),
            held: 0,
            conns,
            sources,
            scratch_idx: Vec::new(),
            scratch_order: Vec::new(),
            theta_scale: 1.0,
            stall_timeout: 1.0,
            health_ranking: true,
            // Default: every connection active, so nothing changes for callers that
            // do not opt into the ramp.
            active_limit: usize::MAX,
            conn_ceiling: usize::MAX,
            limit_reason: LimitReason::None,
            trace_repair: std::env::var_os("HYDRA_TRACE_REPAIR").is_some(),
            repair_candidate: None,
            started: false,
            stats: Stats::default(),
        }
    }

    /// Cap how many connections may hold work at once, adjustable mid-transfer.
    ///
    /// # Why the concurrency search belongs here and not in a probe
    ///
    /// Finding the useful connection count by *probing* — fetch a slab with one
    /// connection, then with two, then three, comparing goodput — is the standard
    /// approach and it is what this client did. HARP (Kim, Yildirim, Kosar, SC'16)
    /// names the cost directly: probing "may bring too much probing overhead",
    /// because the samples are extra transfers whose price is paid before the real
    /// one starts. Measured here on a 3.15 MB object over a live path, the climbing
    /// probe made the transfer **1.96x slower** than not probing at all
    /// (paired over 9 interleaved reps, p = 0.004) — the search cost more than the
    /// concurrency it found could save.
    ///
    /// The probe is only necessary because concurrency is fixed when the transfer
    /// starts. Make it adjustable and the same search runs on the *real* transfer:
    /// start at one connection, measure aggregate goodput over a short window,
    /// admit another connection while the marginal gain justifies it, and stop.
    /// Every byte moved during the search is a byte of the object, so the search
    /// is free — the object had to be fetched anyway. What HARP buys with a
    /// historical corpus, this buys by putting the measurement in-band.
    ///
    /// Connections above the limit stay dormant: they are not given work and open
    /// no socket. Raising the limit lets the next tick hand them work through the
    /// ordinary work-conserving path, so no new admission machinery is needed.
    pub fn set_active_limit(&mut self, n: usize) {
        self.active_limit = n.clamp(1, self.conns.len().max(1));
    }

    /// The current concurrency cap.
    pub fn active_limit(&self) -> usize {
        self.active_limit
    }

    /// Record why the cap is where it is. The transport owns the reasons: it is
    /// the one that sees refusals, starvation and the search's verdict.
    pub fn set_limit_reason(&mut self, why: LimitReason) {
        self.limit_reason = why;
    }

    /// Why `active_limit` sits below the budget, for a front end to explain a
    /// dormant connection. Meaningful only while `active_limit() < n_conns()`.
    pub fn limit_reason(&self) -> LimitReason {
        self.limit_reason
    }

    /// Would this tick reclaim connection `j` for silence?
    ///
    /// The exact predicate `tick` applies, exposed so the transport can act on a
    /// stall in the SAME tick that reclaims it rather than one round later. A
    /// connection that was requested and has delivered nothing by the time this
    /// fires — while others on the same origin stream — is an origin that admits
    /// more connections than it serves. Lowering the concurrency cap after `tick`
    /// has already re-requested the reclaimed range hands it straight back to a
    /// connection the origin is going to starve again, and the transfer pays a
    /// whole stall timeout to learn nothing.
    pub fn conn_stalling(&self, j: usize, now: f64) -> bool {
        self.conns
            .get(j)
            .map(|c| {
                c.busy()
                    && now >= c.setup_end
                    && (now - c.last_progress.max(c.setup_end)) > self.stall_timeout
            })
            .unwrap_or(false)
    }

    /// Declare the largest concurrency this transfer can still reach.
    ///
    /// Lowered when the origin refuses requests, raised when a refusal-free stretch
    /// earns a connection back. Assignment reserves work only for connections that
    /// can actually arrive, so telling the scheduler the real ceiling is what stops
    /// a throttled transfer from carving the object into budget-sized shares nobody
    /// will ever come for.
    pub fn set_conn_ceiling(&mut self, n: usize) {
        self.conn_ceiling = n.clamp(1, self.conns.len().max(1));
    }

    /// The largest concurrency still reachable, never above the budget.
    fn ceiling(&self) -> usize {
        self.conn_ceiling.min(self.conns.len()).max(1)
    }

    /// How much unassigned work to hand an idle connection.
    ///
    /// Everything, once concurrency has settled: maximal ranges mean the fewest
    /// requests. A budget-sized share while connections THIS PASS cannot reach
    /// are still expected — not yet admitted by the ramp (`active_limit <
    /// ceiling`), or `waiting` on a cooldown with a seat under the limit still
    /// free (`+ 1` counts the caller itself) — because the first idle
    /// connection would otherwise swallow the reserve those later admissions
    /// are meant to pick up, leaving each of them a steal: a repair per
    /// admission. One extra request later is the cheaper mistake.
    ///
    /// `limit` rather than `active_limit` alone: the latter is `usize::MAX` for
    /// every caller that never opted into the ramp, which would make the share
    /// path swallow the fixed `-x N` case whole. See
    /// `settled_concurrency_hands_out_maximal_ranges_not_shares` and
    /// `a_ramping_connection_that_drains_its_quota_does_not_swallow_the_reserve`.
    fn share_for(&self, waiting: usize, admitted: usize) -> u64 {
        let ceiling = self.ceiling();
        let limit = self.active_limit.min(self.conns.len());
        if self.active_limit < ceiling || (waiting > 0 && admitted + 1 < limit) {
            let share = self.unassigned.total() / ceiling as u64;
            share.max(STEAL_QUANTUM * 4)
        } else {
            u64::MAX
        }
    }

    /// When every source is deliberately suspended, the earliest time one returns.
    ///
    /// `None` means at least one source is usable now, so a lack of progress is a
    /// genuine stall. `Some(t)` means the scheduler has *chosen* to pause every
    /// source until `t` — nothing can move before then, and that silence is planned
    /// rather than pathological.
    ///
    /// # Why a caller must consult this
    ///
    /// The transport's no-progress watchdog exists to fail a transfer where nothing
    /// will ever happen again. A scheduled retry is the opposite of that, and
    /// conflating the two is not hypothetical: with one source (the common case —
    /// one URL, one CDN), `stall_timeout` 4.0s gives a watchdog of
    /// `4 * (4.0 + delta)` = 16.2s, while five consecutive stalls suspend that sole
    /// source for `min(4.0 * 2^3, 30)` = 30s. The transfer is then killed at 16.2s
    /// for failing to make progress it had itself forbidden.
    ///
    /// Measured consequence on a 121.7 MiB GitHub release asset: 4 of 8 runs at
    /// `-x 8`/`-x 16` aborted with a digest mismatch, three of them having already
    /// received 126.9-127.0 MB of 127.6 MB — 99.6% complete, killed during a
    /// deliberate backoff over the last half-megabyte.
    pub fn all_sources_suspended_until(&self, now: f64) -> Option<f64> {
        let mut earliest = f64::INFINITY;
        for s in &self.sources {
            // A retired source is not "coming back at time t" — it is gone. Its
            // (never-updated) `suspended_until` must not be reported as a time
            // the transfer should wait for, or a caller that retires a dead
            // mirror hears "everything resumes at 0.0" and treats real silence
            // as planned.
            if !s.live {
                continue;
            }
            if s.suspended_until <= now {
                return None;
            }
            earliest = earliest.min(s.suspended_until);
        }
        if earliest.is_finite() {
            Some(earliest)
        } else {
            None
        }
    }

    /// Whether any work is still unclaimed by any connection.
    ///
    /// Exposed so the ramp's contract is testable: while concurrency is below the
    /// budget, work must remain here for connections admitted later to pick up.
    pub fn unassigned_is_empty(&self) -> bool {
        self.unassigned.is_empty()
    }

    /// How many bytes are still unclaimed by any connection.
    ///
    /// The same contract as [`Self::unassigned_is_empty`], measured rather than
    /// merely asserted: a reserve that has been whittled down to one sliver is
    /// not empty and is not a reserve either.
    pub fn unassigned_total(&self) -> u64 {
        self.unassigned.total()
    }

    /// How many connections currently hold a range.
    pub fn busy_conns(&self) -> usize {
        self.conns.iter().filter(|c| c.busy()).count()
    }

    /// Connections that count against `active_limit` right now: busy, or already
    /// holding queued work one tick from starting.
    ///
    /// This is what "dormant" is measured against, not connection index. The
    /// budget is a COUNT of connections in play, not a privilege attached to
    /// low indices — a connection above `active_limit` that is still busy is
    /// not "excess", it is simply already spending the budget it was granted
    /// when it was admitted, and one at any index is free to spend it once
    /// something else stops.
    ///
    /// Queued connections are counted for the same reason `on_bytes` and
    /// divergence repair must not both admit into the same headroom in one
    /// tick: a connection with `queued` set has already been promised a slot,
    /// even though it has not opened a socket yet.
    fn admitted(&self) -> usize {
        self.conns
            .iter()
            .filter(|c| c.busy() || c.queued.is_some())
            .count()
    }

    /// Start with only `n` connections active, ramping up from there.
    pub fn with_active_limit(mut self, n: usize) -> Self {
        self.set_active_limit(n);
        self
    }

    pub fn with_theta_scale(mut self, s: f64) -> Self {
        self.theta_scale = s;
        self
    }

    /// Disable health-ranked victim selection (for A/B measurement only).
    pub fn with_health_ranking(mut self, on: bool) -> Self {
        self.health_ranking = on;
        self
    }

    pub fn with_stall_timeout(mut self, t: f64) -> Self {
        self.stall_timeout = t;
        self
    }

    /// Mark `[lo, hi)` as already held, for resuming a partial transfer.
    ///
    /// Must be called before the first `tick`: the initial split assigns all
    /// unassigned work, and bytes already on disk must not be part of it.
    pub fn mark_done(&mut self, lo: u64, hi: u64) {
        let (lo, hi) = (lo.min(self.size), hi.min(self.size));
        if hi <= lo {
            return;
        }
        // Credit only the bytes this call actually claims, measured as the drop in
        // the unassigned set — NOT the width of the span asked for.
        //
        // Callers legitimately overlap. A `-c` resume marks the sidecar's ranges
        // held, and the concurrency probe separately reports the bytes it fetched;
        // both start at offset 0, so the same prefix is marked twice. Crediting
        // `hi - lo` each time made `held` exceed the bytes that exist, and `held`
        // is what `is_complete()` tests: the transfer stopped early believing it
        // was finished, leaving a zero-filled hole in the tail of a file reported
        // as a success. Measured on an interrupted-then-resumed 11 200 900-byte
        // object: 240 138 bytes of tail never written, `ok: true`, and the gzip
        // refused to decompress.
        let before = self.unassigned.total();
        self.unassigned.remove(lo, hi);
        let claimed = before.saturating_sub(self.unassigned.total());
        self.held = self.held.saturating_add(claimed);
    }

    /// Health grade of a connection, for the progress UI and for tests.
    pub fn conn_health(&self, j: usize) -> crate::detect::Health {
        self.conns
            .get(j)
            .map(|c| c.detector.health())
            .unwrap_or_default()
    }

    /// Source index a connection belongs to, for the progress UI.
    pub fn conn_source(&self, j: usize) -> usize {
        self.conns.get(j).map(|c| c.source).unwrap_or(0)
    }

    /// Smoothed rate estimate of a connection (bytes/s), for the progress UI.
    pub fn conn_rate(&self, j: usize) -> f64 {
        self.conns.get(j).map(|c| c.rate_est).unwrap_or(0.0)
    }

    /// Active range of a connection, for the progress UI.
    pub fn conn_range(&self, j: usize) -> Option<(u64, u64, u64)> {
        self.conns
            .get(j)
            .and_then(|c| c.range.map(|r| (r.lo, c.pos, r.hi)))
    }

    pub fn n_conns(&self) -> usize {
        self.conns.len()
    }

    pub fn is_complete(&self) -> bool {
        self.held >= self.size
    }

    pub fn bytes_held(&self) -> u64 {
        self.held
    }

    /// The ranges that are complete on disk, as `(lo, hi)` pairs.
    ///
    /// This is the complement of the unassigned set minus what is still in flight, and
    /// it is what a resume record must contain. Reporting only a byte COUNT is not
    /// enough: positioned writes land ranges out of order, so "2 MB held" says nothing
    /// about which 2 MB, and a resume that assumed a contiguous prefix would skip holes
    /// and silently corrupt the file.
    pub fn held_ranges(&self) -> Vec<(u64, u64)> {
        // Start from everything, then subtract what is unassigned and what is
        // outstanding on a connection; what remains has arrived.
        let mut done = IntervalSet::full(self.size);
        for r in self.unassigned.ranges() {
            done.remove(r.lo, r.hi);
        }
        for c in &self.conns {
            if let Some(r) = c.range {
                // Bytes before the cursor have arrived; the rest has not.
                done.remove(c.pos, r.hi);
            }
            if let Some(q) = c.queued {
                done.remove(q.lo, q.hi);
            }
        }
        done.ranges().iter().map(|r| (r.lo, r.hi)).collect()
    }

    /// Coverage audit: held + outstanding + unassigned == size.
    ///
    /// This is a SAFETY invariant and it does NOT imply liveness -- the
    /// livelock this code is written to avoid (a fully-stolen range leaving a
    /// connection idle with a non-empty queue) satisfies it at every instant.
    /// `liveness_holds` is the property that matters.
    /// The largest measured request setup cost across sources, in seconds.
    ///
    /// Exposed because a transport-layer watchdog must express its patience in
    /// units of what a request actually costs on this path rather than as a
    /// hardcoded constant: `delta` differs by an order of magnitude between a
    /// LAN mirror and a TLS connection through a proxy, and a fixed timeout is
    /// either trigger-happy on the slow path or useless on the fast one.
    ///
    /// This is the same quantity the repair deadband is built from
    /// (`theta = scale * sqrt(delta * T_rem / n)`), so a client that widens
    /// `delta` widens both together, which is the intended coupling.
    pub fn worst_delta(&self) -> f64 {
        self.sources
            .iter()
            .map(|s| s.delta_est)
            .fold(0.0f64, f64::max)
    }

    /// The configured stall timeout, in seconds.
    pub fn stall_timeout(&self) -> f64 {
        self.stall_timeout
    }

    pub fn coverage_holds(&self) -> bool {
        let outstanding: u64 = self.conns.iter().map(|c| c.outstanding()).sum();
        self.held + outstanding + self.unassigned.total() == self.size
            && self.unassigned.invariant_holds()
    }

    /// True when some enabled transition strictly decreases the unheld-byte count.
    /// False means the scheduler is stuck.
    pub fn liveness_holds(&self) -> bool {
        if self.is_complete() {
            return true;
        }
        // progress possible if: someone is receiving, or work is assignable,
        // or a connection holds a queue it can start, or a stall can be reclaimed
        self.conns.iter().any(|c| c.busy() && !c.stalled)
            || !self.unassigned.is_empty()
            || self.conns.iter().any(|c| c.queued.is_some())
            || self.conns.iter().any(|c| c.stalled)
    }

    // ---------------------------------------------------------------- input

    /// Record `n` bytes arriving on `conn` at time `now` over `dt` seconds.
    ///
    /// Convenience wrapper that assumes the arrival is contiguous at the
    /// connection's cursor. Real transports must use [`Scheduler::on_bytes_at`]:
    /// a response still draining from a range that was completed or stolen would
    /// otherwise be credited against whatever range the connection holds NOW,
    /// silently advancing a cursor over bytes that never arrived and leaving a
    /// hole of zeros in the output file.
    pub fn on_bytes(&mut self, conn: usize, n: u64, now: f64, dt: f64) {
        let at = self.conns[conn].pos;
        self.on_bytes_at(conn, at, n, now, dt);
    }

    /// Record `n` bytes that landed at absolute offset `off`.
    ///
    /// Arrivals that do not begin exactly at the connection's cursor are stale
    /// (they belong to a superseded request) and are discarded: the bytes are
    /// still written to the file by the transport, but they are not credited,
    /// so the scheduler's coverage accounting stays exact.
    pub fn on_bytes_at(&mut self, conn: usize, off: u64, n: u64, now: f64, dt: f64) {
        let c = &mut self.conns[conn];
        let Some(r) = c.range else { return };
        if off != c.pos || off < r.lo {
            return; // stale arrival from a superseded range
        }
        // ---- and stale by TIME, not only by offset --------------------------
        //
        // Matching the cursor is not enough to prove an arrival belongs to the
        // request in flight. When a connection is reclaimed and re-requested, the
        // new request starts at exactly the cursor the old one stopped at — so
        // the last writes of the aborted request, still in the caller's queue,
        // land at precisely the offset the new request is waiting for.
        //
        // Crediting them is not a coverage error (the bytes are on disk) but it
        // desynchronises the connection: the cursor moves past where the new
        // response begins, so every arrival that response produces fails the test
        // above and is discarded. The connection then delivers bytes that are
        // never counted, reads as silent, and is rescued only by the stall
        // timeout — seconds of dead air, and the transfer visibly frozen for them
        // once the endgame has left one connection carrying the remainder.
        //
        // A request cannot be answered before it was issued, so the arrival's own
        // timestamp settles it.
        if now < c.started_at {
            return;
        }
        let room = r.hi.saturating_sub(c.pos);
        let step = n.min(room);
        if step == 0 {
            return;
        }
        c.pos += step;
        self.held += step;
        c.last_progress = now;
        c.stalled = false;
        let src = c.source;
        self.sources[src].consecutive_stalls = 0;
        if dt > 0.0 {
            // Accumulate, and only take a rate sample once the window has enough
            // wall clock in it to mean something.
            c.rate_acc_bytes += step;
            c.rate_acc_dt += dt;
            if c.rate_acc_dt >= RATE_WINDOW {
                let sample = c.rate_acc_bytes as f64 / c.rate_acc_dt;
                c.rate_acc_bytes = 0;
                c.rate_acc_dt = 0.0;
                c.detector.observe_rate(sample);
                c.rate_est = if c.rate_est <= 0.0 {
                    sample
                } else {
                    RATE_ALPHA * sample + (1.0 - RATE_ALPHA) * c.rate_est
                };
            }
        }
        if c.pos >= r.hi {
            c.range = None;
        }
    }

    /// Suspend a source (429/503 with Retry-After) and reclaim its ranges.
    pub fn suspend_source(&mut self, src: usize, until: f64) {
        self.sources[src].suspended_until = until;
        let idxs: Vec<usize> = (0..self.conns.len())
            .filter(|&j| self.conns[j].source == src)
            .collect();
        for j in idxs {
            self.reclaim(j);
        }
    }

    // ------------------------------------------------------- source failover

    /// Take a source out of the transfer permanently and reclaim its ranges.
    ///
    /// # Why permanent removal is a different operation from suspension
    ///
    /// [`suspend_source`](Self::suspend_source) is a pause: the source comes back
    /// when the clock says so, which is right for a `429` or a transient stall.
    /// Some failures are not pauses. A host that no longer resolves, a mirror
    /// serving a different build, a TLS certificate that will not validate — none
    /// of those get better on a timer, and retrying them costs a full setup every
    /// backoff for the rest of the download.
    ///
    /// Retiring reclaims whatever the source's connections were holding, so those
    /// bytes go back to the unassigned set and the surviving connections pick
    /// them up through the ordinary work-conserving path. Nothing is stranded.
    ///
    /// Returns the connection indices that were freed, so the caller can tear
    /// down their sockets. Retiring the LAST live source is permitted — a
    /// transfer with nowhere left to fetch from has to be able to say so, and
    /// [`live_sources`](Self::live_sources) returning zero is how the caller
    /// learns it — but it is never something the scheduler does on its own.
    pub fn retire_source(&mut self, src: usize) -> Vec<usize> {
        let Some(s) = self.sources.get_mut(src) else {
            return Vec::new();
        };
        if !s.live {
            return Vec::new();
        }
        s.live = false;
        let idxs: Vec<usize> = (0..self.conns.len())
            .filter(|&j| self.conns[j].source == src)
            .collect();
        for &j in &idxs {
            self.reclaim(j);
        }
        idxs
    }

    /// Point a retired source's connections at a replacement mirror.
    ///
    /// # The reserve bench
    ///
    /// This is what a mirror list is worth. A Metalink for a distribution image
    /// commonly names fifteen to twenty mirrors, while politeness and physics
    /// together justify perhaps four connections — so most of the list is not a
    /// source, it is a *reserve*. Without a way to substitute, those reserves are
    /// decoration: the transfer either survives on the mirrors it opened with or
    /// it does not.
    ///
    /// Substituting in place, rather than growing the connection set, is what
    /// keeps the aggregate socket count equal to what politeness authorised. The
    /// replaced source's connections are reused as they are — same indices, same
    /// detector state reset — so the caller's own per-connection bookkeeping
    /// (hostnames for the progress view, per-connection targets) stays index-
    /// aligned with the scheduler's.
    ///
    /// The replacement starts with the incoming `Source`'s estimates and a clean
    /// stall count. Carrying the dead mirror's `gamma_est` across would price the
    /// new host by the old one's failure and bias the very next repair against
    /// it.
    ///
    /// Returns the connection indices now belonging to the replacement, empty if
    /// `src` does not exist.
    pub fn replace_source(&mut self, src: usize, mut replacement: Source) -> Vec<usize> {
        if src >= self.sources.len() {
            return Vec::new();
        }
        // Reclaim first: whatever the outgoing source held must go back to the
        // unassigned set before its connections are relabelled, or those bytes
        // are owned by a connection that will never be asked for them again.
        let idxs: Vec<usize> = (0..self.conns.len())
            .filter(|&j| self.conns[j].source == src)
            .collect();
        for &j in &idxs {
            self.reclaim(j);
        }
        replacement.live = true;
        replacement.consecutive_stalls = 0;
        replacement.suspended_until = 0.0;
        self.sources[src] = replacement;
        for &j in &idxs {
            let c = &mut self.conns[j];
            // A fresh detector: the collapse grade belongs to the host that
            // earned it, and inheriting it would have the new mirror graded
            // Suspect before its first byte — which bars it from ever being
            // chosen as a repair taker.
            c.detector = crate::detect::CollapseDetector::default();
            c.rate_est = 0.0;
            c.stalled = false;
        }
        idxs
    }

    /// Sources that have not been retired.
    pub fn live_sources(&self) -> usize {
        self.sources.iter().filter(|s| s.live).count()
    }

    /// Is this source still in the transfer?
    pub fn source_is_live(&self, src: usize) -> bool {
        self.sources.get(src).is_some_and(|s| s.live)
    }

    /// Consecutive stalls charged against a source, for a caller deciding
    /// whether to substitute a reserve mirror for it.
    pub fn source_stalls(&self, src: usize) -> u32 {
        self.sources
            .get(src)
            .map(|s| s.consecutive_stalls)
            .unwrap_or(0)
    }

    /// The publisher-stated preference a source was created with.
    pub fn source_priority(&self, src: usize) -> u32 {
        self.sources
            .get(src)
            .map(|s| s.priority)
            .unwrap_or(NO_PRIORITY)
    }

    /// How many sources this transfer was built with, live or retired.
    pub fn n_sources(&self) -> usize {
        self.sources.len()
    }

    /// A connection's transport failed: reclaim its range NOW, and hold that
    /// connection back for `retry_after` seconds.
    ///
    /// # Why silence is not the right signal for a failure
    ///
    /// The stall timeout exists to grade a connection that is *delivering
    /// nothing*, and it has to be patient — several seconds at least, scaled to
    /// the measured setup cost, because a slow path is not a broken one. A fetch
    /// that has already returned an error needs none of that patience: the
    /// question the timeout is there to answer has been answered, by the
    /// transport, definitively.
    ///
    /// Without this the two are conflated, and the cost is paid in whole stall
    /// timeouts. A connection whose socket was closed by the peer, whose body was
    /// truncated, or whose request was refused looks exactly like a slow one, so
    /// the range is not re-requested for 4-45 s (the range `stall_timeout` covers
    /// on real paths). Early in a transfer the other connections cover for it and
    /// nothing is visible; at the end, when the remaining work has concentrated
    /// onto one or two connections, the whole transfer freezes for it — the
    /// reported "downloads stall past 90%, transfer rate falls to zero, every
    /// connection shows disconnected" failure.
    ///
    /// `retry_after` is the caller's backoff for THIS connection only. The range
    /// goes back to the unassigned set immediately either way, so an idle
    /// connection can pick it up on the next tick without waiting for it.
    pub fn on_conn_error(&mut self, conn: usize, now: f64, retry_after: f64) {
        if conn >= self.conns.len() {
            return;
        }
        self.reclaim(conn);
        let until = now + retry_after.max(0.0);
        let c = &mut self.conns[conn];
        c.setup_end = until;
        // The stall clock starts when the connection is allowed to work again;
        // otherwise the backoff it was told to take is charged against it as
        // silence and it is graded stalled the moment it comes back.
        c.last_progress = until;
    }

    fn reclaim(&mut self, j: usize) {
        let c = &mut self.conns[j];
        if let Some(r) = c.range {
            if c.pos < r.hi {
                let back = Range::new(c.pos, r.hi);
                c.range = None;
                let q = c.queued.take();
                self.unassigned.insert(back);
                if let Some(q) = q {
                    self.unassigned.insert(q);
                }
                self.stats.reclaims += 1;
            } else {
                c.range = None;
            }
        } else if let Some(q) = c.queued.take() {
            self.unassigned.insert(q);
            self.stats.reclaims += 1;
        }
        let c = &mut self.conns[j];
        c.rate_est = 0.0;
        c.stalled = true;
    }

    // ---------------------------------------------------------------- tick

    /// Advance the scheduler. Returns the actions the caller must perform.
    pub fn tick(&mut self, now: f64) -> Vec<Action> {
        let mut acts = Vec::new();

        if !self.started {
            self.initial_split(now, &mut acts);
            self.started = true;
            return acts;
        }

        // ---- feed wall-clock silence to the detectors ----------------------
        // A connection delivering nothing produces no rate samples at all, so
        // silence is evidence that only the clock can supply. Grading it here
        // lets repair pre-empt at half the stall timeout instead of waiting for
        // the full timeout to expire.
        for j in 0..self.conns.len() {
            let c = &self.conns[j];
            if c.busy() && now >= c.setup_end {
                let quiet = now - c.last_progress.max(c.setup_end);
                let st = self.stall_timeout;
                self.conns[j].detector.observe_silence(quiet, st);
            }
        }

        // ---- liveness path 1: reclaim stalled connections -----------------
        //
        // Collected into a reused buffer rather than a fresh `Vec` each tick. The
        // indices cannot be reclaimed in the same pass that finds them — `reclaim`
        // takes `&mut self` while the filter borrows `self.conns` — so the two-phase
        // shape stays, but the allocation does not have to. `std::mem::take` moves the
        // buffer out so the loop below can hold it while `self` is borrowed mutably,
        // and it is put back at the end for the next tick.
        let mut stalled = std::mem::take(&mut self.scratch_idx);
        stalled.clear();
        stalled.extend((0..self.conns.len()).filter(|&j| self.conn_stalling(j, now)));
        for j in stalled.drain(..) {
            self.reclaim(j);
            acts.push(Action::Cancel { conn: j });
            // A source that keeps stalling must be suspended, not merely
            // retried: otherwise work-conserving assignment hands it the same
            // bytes repeatedly without making forward progress.
            let src = self.conns[j].source;
            self.sources[src].consecutive_stalls += 1;
            let k = self.sources[src].consecutive_stalls;
            if k >= 2 {
                let mut backoff = (self.stall_timeout * (1u64 << (k - 2).min(5)) as f64).min(30.0);
                // Never suspend the LAST usable source for longer than a caller's
                // watchdog will wait. Exponential backoff is right when there is
                // somewhere else to send the work; when this is the only source it
                // is a self-inflicted outage, and a transport that fails on silence
                // cannot tell it apart from the source being gone.
                //
                // Callers should also consult `all_sources_suspended_until` so a
                // planned pause is not charged against a no-progress deadline. This
                // clamp is the second line of defence: it keeps the invariant local
                // to the scheduler, so a caller that does not know about deliberate
                // suspension still cannot be starved by it.
                if self.sources.len() == 1 {
                    backoff = backoff.min(self.stall_timeout.max(1.0));
                }
                self.sources[src].suspended_until = now + backoff;
            }
        }

        // ---- liveness path 2: an idle connection holding a queue MUST start it
        //
        // Mandatory: a connection whose active range was entirely stolen goes
        // idle WITHOUT completing, so the completion path in on_bytes never fires
        // and the queued bytes would be owned by an idle connection that never
        // requests them.
        for j in 0..self.conns.len() {
            if !self.conns[j].busy()
                && self.conns[j].queued.is_some()
                && now >= self.conns[j].setup_end
            {
                let r = self.conns[j].queued.take().unwrap();
                self.start(j, r, now);
                acts.push(Action::Request { conn: j, range: r });
            }
        }

        // ---- divergence-triggered repair ---------------------------------
        let theta = self.theta(now);
        let mut divergent = false;
        for _ in 0..MAX_REPAIRS_PER_TICK {
            let Some((vi, ti)) = self.pick_victim_taker(now) else {
                break;
            };
            let (v_eta, t_eta) = (self.conns[vi].eta(), self.conns[ti].eta());
            // Explicit ordering test: an unknown ETA yields NaN, and a NaN
            // divergence must NOT trigger a repair (a repair costs a full delta,
            // so acting on an unmeasured quantity is strictly a loss).
            if !matches!(
                (v_eta - t_eta).partial_cmp(&theta),
                Some(core::cmp::Ordering::Greater)
            ) {
                break;
            }
            divergent = true;
            // Between two busy connections the divergence must PERSIST; see
            // `REPAIR_PERSIST`. A collapsing victim and an idle taker are exempt.
            let collapsing = self.conns[vi].detector.health().is_suspect_or_worse();
            if self.conns[ti].busy() && !collapsing {
                // Persistence in units of the setup cost as well as of samples:
                // a divergence that is really two flows at different points of
                // slow start lasts a number of round trips, and a fixed window
                // is one length on a LAN and another on a transatlantic path.
                let delta = self
                    .sources
                    .iter()
                    .map(|s| s.delta_est)
                    .fold(0.0f64, f64::max);
                let persist = REPAIR_PERSIST.max(REPAIR_PERSIST_DELTAS * delta);
                match self.repair_candidate {
                    Some((v, since)) if v == vi => {
                        if now - since < persist {
                            break;
                        }
                    }
                    _ => {
                        self.repair_candidate = Some((vi, now));
                        break;
                    }
                }
            }
            if self.conns[ti].queued.is_some() {
                break;
            }
            let Some(vr) = self.conns[vi].range else {
                break;
            };
            let left = vr.hi.saturating_sub(self.conns[vi].pos) as f64;
            let rv = self.conns[vi].rate_est;
            let rt = self.conns[ti].rate_est;
            let delta = self.sources[self.conns[ti].source].delta_est;
            // Equalise projected finishes, charging the taker one setup:
            //   (left - x)/rv == t_eta + delta + x/rt
            let x = if rv <= 0.0 {
                // Victim is stalled: hand over everything it has not received.
                left
            } else if rt <= 0.0 {
                0.0
            } else {
                ((left / rv - t_eta - delta) * (rv * rt) / (rv + rt)).clamp(0.0, left)
            };
            if x <= STEAL_QUANTUM as f64 {
                break;
            }
            if self.trace_repair {
                eprintln!(
                    "repair: t={now:.2} victim={vi} (eta {v_eta:.2}s rate {:.1}MB/s left {:.1}MB {:?} rising={}) \
                     taker={ti} (eta {t_eta:.2}s rate {:.1}MB/s busy={}) theta={theta:.2} x={:.1}MB",
                    rv / 1e6,
                    left / 1e6,
                    self.conns[vi].detector.health(),
                    self.conns[vi].detector.rising(),
                    rt / 1e6,
                    self.conns[ti].busy(),
                    x / 1e6
                );
            }

            // ---- does this repair actually pay for itself? -------------------
            //
            // The equalisation above solves `(left - x)/rv == t_eta + delta + x/rt`,
            // which treats `rt` as capacity that `x` bytes can be moved ONTO. That
            // is true when the connections have independent bottlenecks — separate
            // mirrors, separate paths. It is false in the case that dominates real
            // use: several connections to one origin, sharing one bottleneck. There
            // the taker's rate is not spare capacity, it is a share of the same
            // capacity the victim is using, so moving bytes across does not make
            // them arrive faster. It only re-labels which connection carries them,
            // and charges a setup for the privilege.
            //
            // Worse, the per-connection rate divergence that triggers the repair is
            // largely a property of the PATH, not of the assignment: flows sharing
            // a bottleneck settle at persistently unequal shares (roughly 1/RTT,
            // with cwnd history making the asymmetry outlive any round trip). A
            // repair cannot move that. So the divergence survives the repair, and
            // re-triggers it.
            //
            // The test: compare the makespan now against the makespan after, where
            // "after" charges the setup and credits only the improvement in the
            // WORST finishing time — because the makespan is a max, not a sum, and
            // improving anything other than the laggard buys nothing.
            let makespan_now =
                self.conns
                    .iter()
                    .map(|c| c.eta())
                    .fold(0.0f64, |a, b| if b > a { b } else { a });
            // The victim keeps `left - x` at its own rate; the taker takes on `x`
            // after paying `delta`, on top of what it already owes.
            let v_after = if rv > 0.0 {
                (left - x) / rv
            } else {
                f64::INFINITY
            };
            let t_after = if rt > 0.0 {
                t_eta + delta + x / rt
            } else {
                f64::INFINITY
            };
            // Every other connection is unaffected by this particular exchange.
            let others = self
                .conns
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != vi && *j != ti)
                .map(|(_, c)| c.eta())
                .fold(0.0f64, |a, b| if b > a { b } else { a });
            let makespan_after = v_after.max(t_after).max(others);
            // Require the gain to exceed the setup it costs, not merely to be
            // positive: a repair that improves the projected makespan by less than
            // one delta has not accounted for its own price. `theta` above is the
            // hysteresis that stops oscillation; this is the profitability test,
            // and both are needed — the first keeps jitter from triggering repair,
            // the second keeps a real-but-unprofitable divergence from doing so.
            // Explicit ordering, matching the theta test above: an unmeasured rate
            // makes this difference NaN, and a NaN must REFUSE the repair rather
            // than fall through either way. Acting on an unmeasured quantity is
            // strictly a loss, because the setup cost is certain and the gain is not.
            if !matches!(
                (makespan_now - makespan_after).partial_cmp(&delta),
                Some(core::cmp::Ordering::Greater)
            ) {
                break;
            }

            let x = x as u64;
            let new_hi = vr.hi - x;
            let stolen = Range::new(new_hi, vr.hi);
            // Client-side shrink: the victim's target end moves and the server is
            // never told. Free on the WIRE — no cancellation, no round trip — but
            // only if the local fetch loop is told, which is what `Shrink` does.
            // Without it the victim streams the stolen span anyway; see the
            // `Action::Shrink` docs for what that costs.
            self.conns[vi].range = Some(Range::new(vr.lo, new_hi));
            self.conns[ti].queued = Some(stolen);
            acts.push(Action::Shrink {
                conn: vi,
                hi: new_hi,
            });
            self.stats.repairs += 1;
            // The next repair earns its own persistence: a shrink changes every
            // ETA that justified this one, and a cascade of repairs each riding
            // on the previous one's disturbance is the storm this damps.
            self.repair_candidate = None;
        }
        if !divergent {
            self.repair_candidate = None;
        }

        // ---- work-conserving assignment (Lemma 2) -------------------------
        //
        // A connection is DORMANT once the budget is spent: it is skipped here, so
        // it is never given work and never opens a socket. This is the whole
        // mechanism behind the in-band concurrency ramp — raising the limit makes
        // the next tick admit a connection through this ordinary path, and
        // lowering it lets an already-busy connection finish its range and then go
        // quiet, with no cancellation and no wasted bytes.
        //
        // The budget is spent by COUNT (`admitted`), not by index. All connections
        // are dispatched at once — a fixed `-x N`, or the opening burst before any
        // refusal has taught the transfer anything — so which ones an origin
        // happens to grant is not correlated with index at all. Gating eligibility
        // on `j < active_limit` let a refusal-driven cap retire a connection the
        // origin was actively serving just because its index was too high, while
        // leaving a lower-index connection that was cooling down from its OWN
        // refusal as the only thing still allowed to pick up new work — collapsing
        // realised concurrency below what the origin would serve, which is the one
        // thing this cap exists to prevent. See `admitted` for what counts.
        //
        // Candidates are visited proven connections first, unproven ones after —
        // "proven" meaning `rate_est > 0.0`, which only a connection that has
        // actually delivered bytes on this source carries; a reclaim resets it to
        // zero. Plain index order reopens the exact bug above from the other
        // side: the moment one of two settled, working connections finishes a
        // chunk and goes idle for the one tick before this loop re-admits it, it
        // is indistinguishable BY INDEX from a connection that has never
        // delivered a byte and is only here because its OWN refusal cooldown
        // happens to have expired on the same tick. Whichever has the lower
        // index wins the freed slot — sometimes the untested one — and an origin
        // that only ever grants the same two connections now refuses the
        // newcomer, while the settled connection that actually earned the slot
        // sits idle for another tick waiting its turn. Repeated over a transfer's
        // life this is exactly the churn the ceiling exists to stop, just paid
        // in requests instead of in stranded concurrency.
        let mut order = std::mem::take(&mut self.scratch_order);
        order.clear();
        // One predicate and its exact complement, rather than `> 0.0` and
        // `<= 0.0`: the two passes must partition the connections, and BOTH of
        // those comparisons are false for a NaN rate. A connection that fell into
        // neither would be dropped from the visit order entirely — never assigned
        // work, and never reclaimed into service either, since reclaim only fires
        // on connections that HOLD a range. Silent permanent idleness is not a
        // failure mode worth leaving to the float rules.
        let proven = |c: &Conn| c.rate_est > 0.0;
        order.extend((0..self.conns.len()).filter(|&j| proven(&self.conns[j])));
        order.extend((0..self.conns.len()).filter(|&j| !proven(&self.conns[j])));
        // Connections that will want work on a LATER tick: idle, holding nothing,
        // and held back by their own cooldown or a suspended source rather than by
        // anything this pass can resolve. These are who the reserve below is for.
        // Counted once: assigning work in the loop only turns reachable
        // connections busy, and those were never in this set.
        let waiting = (0..self.conns.len())
            .filter(|&k| {
                let c = &self.conns[k];
                !c.busy()
                    && c.queued.is_none()
                    && (now < c.setup_end || !self.sources[c.source].usable_at(now))
            })
            .count();
        let mut admitted = self.admitted();
        for &j in &order {
            if admitted >= self.active_limit {
                break;
            }
            if self.conns[j].busy() || now < self.conns[j].setup_end {
                continue;
            }
            let src = self.conns[j].source;
            if !self.sources[src].usable_at(now) {
                continue;
            }
            let want = self.share_for(waiting, admitted);
            if let Some(r) = self.unassigned.take_front(want) {
                self.start(j, r, now);
                acts.push(Action::Request { conn: j, range: r });
                admitted += 1;
                continue;
            }
            // Nothing unassigned: steal from the worst laggard.
            //
            // This is the steal-half heuristic, and it fires on a DIFFERENT
            // trigger from the divergence repair above: not "the finishes have
            // diverged" but "a connection has gone idle and there is nothing left
            // to give it". Splitting the laggard's remainder down the middle is the
            // right move when the idle connection has capacity the laggard cannot
            // use. It is churn when they share one bottleneck — the same span is
            // re-requested, a setup is paid, and the aggregate rate is unchanged
            // because it was never the assignment that limited it.
            //
            // So the same profitability test applies. An idle connection is not a
            // reason to move work; it is a reason to ASK whether moving work helps.
            if let Some(vi) = self.worst_busy(j) {
                let vr = self.conns[vi].range.unwrap();
                let left = vr.hi.saturating_sub(self.conns[vi].pos);
                let half = left / 2;
                // Will the taker, paying one setup, actually finish this half
                // sooner than the victim would have finished the whole remainder?
                // With `rt` unknown (a connection that has just gone idle may have
                // no estimate yet) fall back to the victim's own rate, which makes
                // the test neutral rather than optimistic.
                let rv = self.conns[vi].rate_est;
                let rt = if self.conns[j].rate_est > 0.0 {
                    self.conns[j].rate_est
                } else {
                    rv
                };
                let delta = self.sources[self.conns[j].source].delta_est;
                let worth_it = if rv <= 0.0 {
                    // The victim is delivering nothing measurable: anything is better.
                    true
                } else if rt <= 0.0 {
                    false
                } else {
                    let before = left as f64 / rv;
                    let after = (half as f64 / rv).max(delta + half as f64 / rt);
                    before - after > delta
                };
                if half > STEAL_QUANTUM && worth_it {
                    let new_hi = vr.hi - half;
                    self.conns[vi].range = Some(Range::new(vr.lo, new_hi));
                    let stolen = Range::new(new_hi, vr.hi);
                    // Same shrink discipline as the divergence repair above: the
                    // victim must be told its far end moved, or it streams the
                    // half we just handed away.
                    acts.push(Action::Shrink {
                        conn: vi,
                        hi: new_hi,
                    });
                    self.start(j, stolen, now);
                    acts.push(Action::Request {
                        conn: j,
                        range: stolen,
                    });
                    admitted += 1;
                    self.stats.repairs += 1;
                }
            }
            // NOTE: no hedging. Redundant requests waste bandwidth on non-erasure channels.
        }

        self.stats.bytes_held = self.held;
        // Hand the scratch buffers back so their capacity survives to the next tick.
        // Without this the `mem::take` above would leave an empty Vec in the field and
        // the next tick would allocate again — the reuse would be nominal only.
        self.scratch_idx = stalled;
        self.scratch_order = order;
        acts
    }

    fn start(&mut self, j: usize, r: Range, now: f64) {
        let delta = self.sources[self.conns[j].source].delta_est;
        let c = &mut self.conns[j];
        c.range = Some(r);
        c.pos = r.lo;
        c.started_at = now;
        c.setup_end = now + delta;
        c.last_progress = now + delta;
        c.stalled = false;
        self.stats.requests += 1;
    }

    fn initial_split(&mut self, now: f64, acts: &mut Vec<Action>) {
        // Maximal ranges, proportional to rate estimate where known, else equal.
        //
        // Only the ACTIVE prefix takes part. With the ramp enabled the transfer
        // opens one connection, and the rest are admitted by `set_active_limit` as
        // the in-band search finds them worth their setup cost. Splitting the
        // object across connections that will not run would strand those bytes in
        // a quota nobody fetches.
        let n = self.conns.len().min(self.active_limit);
        if n == 0 || self.size == 0 {
            return;
        }
        let weights: Vec<f64> = self
            .conns
            .iter()
            .take(n)
            .map(|c| {
                let src = &self.sources[c.source];
                let g = if src.gamma_est > 0.0 {
                    src.gamma_est
                } else {
                    1.0
                };
                // The publisher's ranking enters HERE and only here. This is the
                // one moment in the transfer at which nothing has been measured,
                // so a stated preference is the best information available; from
                // the next tick onward `gamma_est` and the per-connection rate
                // estimates are real samples and the prior is not consulted
                // again. See `Source::priority`.
                g * src.priority_weight()
            })
            .collect();
        let total: f64 = weights.iter().sum();

        // Split what is ACTUALLY unassigned, not `[0, size)`.
        //
        // An earlier version partitioned the whole object arithmetically, which
        // silently ignored `mark_done`. That broke both features that depend on
        // it: `--range` fetched from offset 0 instead of the requested interval,
        // and `--continue` re-fetched bytes already on disk. The unassigned set is
        // the single source of truth for what remains, so the split must be taken
        // from it.
        let remaining: Vec<Range> = self.unassigned.ranges().to_vec();
        let avail: u64 = remaining.iter().map(|r| r.hi - r.lo).sum();
        if avail == 0 {
            return;
        }
        // Per-connection byte quotas, proportional to rate estimate.
        //
        // Divided over the FULL connection budget, not just the active prefix, and
        // this matters specifically when the ramp is running. With one connection
        // active, dividing by the active count alone hands that connection the
        // entire object — so a connection admitted later finds the unassigned set
        // empty and its only route to work is to STEAL, which pays a repair to
        // undo a split that should never have been made. Measured cost of getting
        // this wrong: every ramped transfer of a 3.15 MB object took ~21 s against
        // 6.3 s for fixed concurrency, and several were reported as failures
        // despite delivering byte-exact files.
        //
        // Quotas over the full budget leave the remainder UNASSIGNED, which is
        // exactly where a newly admitted connection takes work from through
        // ordinary work-conserving assignment — no repair, no steal, no duplicate
        // request. If the ramp never grows, nothing is lost: the active connection
        // finishes its quota and work-conserving assignment gives it the next
        // piece, which connection reuse now makes nearly free.
        let budget = self.ceiling();
        let mut quota: Vec<u64> = weights
            .iter()
            .map(|w| ((w / total) * (avail as f64 / budget as f64) * n as f64) as u64)
            .collect();
        // Rounding must not strand bytes — but only when every connection is
        // active. While ramping, the unclaimed remainder is deliberate.
        if n >= budget {
            let assigned: u64 = quota.iter().sum();
            if let Some(last) = quota.last_mut() {
                *last += avail.saturating_sub(assigned);
            }
        }

        // Walk the unassigned ranges, carving each connection's quota out of them
        // in order. A connection may receive a range that is not contiguous with
        // its neighbours' — that is fine, since ranges are independent requests.
        let mut it = remaining.into_iter();
        let mut cur = it.next();
        for (j, want_total) in quota.iter().enumerate() {
            let mut want = *want_total;
            while want > 0 {
                let Some(seg) = cur else { break };
                let take = want.min(seg.hi - seg.lo);
                let r = Range::new(seg.lo, seg.lo + take);
                // A connection holds one active range plus a one-slot pipeline.
                // Anything beyond that stays UNASSIGNED rather than being stashed:
                // work-conserving assignment will hand it out as connections free
                // up, and leaving it in the set is what keeps the coverage
                // invariant checkable.
                if self.conns[j].range.is_none() {
                    self.unassigned.remove(r.lo, r.hi);
                    self.start(j, r, now);
                    acts.push(Action::Request { conn: j, range: r });
                } else if self.conns[j].queued.is_none() {
                    self.unassigned.remove(r.lo, r.hi);
                    self.conns[j].queued = Some(r);
                } else {
                    break;
                }
                want -= take;
                cur = if seg.hi - seg.lo > take {
                    Some(Range::new(seg.lo + take, seg.hi))
                } else {
                    it.next()
                };
            }
        }
    }

    /// The current repair deadband, in seconds. Exposed for measurement.
    pub fn theta_now(&self, now: f64) -> f64 {
        self.theta(now)
    }

    fn theta(&self, now: f64) -> f64 {
        // One fold, no allocation. This is called from the tick loop — 50 times a
        // second at the default 20 ms tick, for the whole transfer — and it collected
        // a `Vec<&Conn>` on every call only to take its length and sum one field.
        // Nothing here needs the intermediate collection.
        let (live_count, agg) = self
            .conns
            .iter()
            .filter(|c| self.sources[c.source].usable_at(now))
            .fold((0usize, 0.0f64), |(k, sum), c| {
                (k + 1, sum + c.rate_est.max(0.0))
            });
        let n = live_count.max(1) as f64;
        let agg = if agg > 0.0 { agg } else { 1.0 };
        let remaining = self.size.saturating_sub(self.held) as f64;
        let t_rem = remaining / agg;
        let delta = self
            .sources
            .iter()
            .map(|s| s.delta_est)
            .fold(0.0f64, f64::max);
        let band = self.theta_scale * (delta * t_rem.max(0.0) / n).sqrt();

        // ---- floor the deadband at what a repair actually costs --------------
        //
        // `sqrt(delta * T_rem / n)` is the right SHAPE — it is the granularity
        // trade-off — but it is unbounded below, and it approaches zero from two
        // directions that both make repair a worse idea, not a better one:
        // `T_rem` shrinks as the transfer finishes, and `n` grows with
        // concurrency. So the deadband is narrowest exactly when a repair has the
        // least remaining time to earn its cost back and the most competitors to
        // pay it against.
        //
        // Measured on the shared-bottleneck harness (examples/storm.rs, 12 seeds):
        // theta reached 0.061-0.081 s against a delta of 0.12 s. Every repair
        // triggered in that regime spends one full setup to recover a divergence
        // smaller than the setup — a guaranteed loss, taken deliberately, dozens
        // of times per transfer.
        //
        // A repair cannot be worth making unless the divergence it corrects
        // exceeds what correcting it costs, so `delta` is the floor. This is not a
        // tuning constant: it is the break-even point, and it is measured per
        // source rather than guessed, so a high-RTT path widens it automatically.
        band.max(delta)
    }

    fn pick_victim_taker(&self, now: f64) -> Option<(usize, usize)> {
        // Victim ranking is (health, ETA), health first. A connection the
        // detector has graded Suspect is a victim even when its *projected* ETA
        // still looks acceptable -- which is the whole point of detecting a
        // collapse early, since the ETA is computed from a rate estimate that
        // the collapse has not yet dragged down.
        let mut victim: Option<(usize, crate::detect::Health, f64)> = None;
        let mut taker: Option<(usize, f64)> = None;
        // A dormant connection — idle, and not already counted in `admitted` —
        // may only become a taker if the budget has room for it: as taker,
        // admitting one would open a socket the concurrency ramp, or a refusal
        // that has capped the transfer, has not justified — quietly defeating the
        // limit through the repair path. An already-busy connection spends no new
        // budget by taking on queued work, so it is never gated on room. Index
        // plays no part: the budget is a count (`admitted`), not a privilege
        // attached to low indices — see `admitted` for why that distinction is
        // the fix, not decoration.
        let room = self.admitted() < self.active_limit;
        for j in 0..self.conns.len() {
            let c = &self.conns[j];
            if now < c.setup_end || !self.sources[c.source].usable_at(now) {
                continue;
            }
            let e = c.eta();
            let h = if self.health_ranking {
                c.detector.health()
            } else {
                crate::detect::Health::Healthy
            };
            // Neither role may be filled by a connection that has not been
            // MEASURED yet, and a rate is not a measurement until it has been
            // sampled a few times.
            //
            // Traced on a uniform 4-connection transfer with a 100 ms round trip:
            // 1109 repair decisions were evaluated, almost all in the first
            // 0.6 s, and the ones that executed had a victim reporting 0.2 MB/s
            // with a projected 1092 s to finish, against a taker whose first
            // sample happened to be larger. Both numbers were slow-start
            // artefacts a second away from 70 MB/s, the "gain" was hundreds of
            // seconds that did not exist, and the repair moved half of a range
            // that would have arrived on its own — costing a fresh request on
            // each side. The imbalance that left behind is what the endgame
            // repairs were then correcting. Four requests would have done;
            // there were 13 to 19, and the transfer ran 22% behind `aria2c`.
            //
            // A guard on the VICTIM alone was tried first and made things worse
            // (repairs 2-10 became 9-15), because the taker was just as unmeasured
            // and the choice merely shifted. Both sides have to be warm. The one
            // exception is a victim the detector has graded as collapsing: that
            // grade is itself a measurement, and pre-empting a collapse before the
            // stall timeout is what the detector is for.
            let warm = c.detector.samples() >= REPAIR_WARM_SAMPLES;
            // A rate that is still climbing is not a measurement of how slow the
            // connection is, it is a measurement of how far into slow start it
            // has got — see `CollapseDetector::rising`. Warm-up in samples cannot
            // catch this, because slow start lasts a number of ROUND TRIPS and
            // the sample window is fixed: on a 100 ms path the first repair fired
            // at t=1.4 s on a 3:1 ratio that was 1:1 by t=2.0 s, and moved 256 MB.
            let settled = warm && !c.detector.rising();
            // As victim a connection needs no room check: it already holds a
            // range, so it is not being newly admitted, wherever its index falls.
            if c.busy()
                && (settled || h.is_suspect_or_worse())
                && victim.map(|(_, vh, ve)| (h, e) > (vh, ve)).unwrap_or(true)
            {
                victim = Some((j, h, e));
            }
            // A degraded connection must never be chosen as the TAKER: handing
            // work to a collapsing connection is the failure mode this whole
            // mechanism exists to prevent. A busy taker must be warm for the same
            // reason as the victim; a dormant one has no rate at all and the
            // profitability test below refuses it on its own.
            if !h.is_suspect_or_worse()
                && ((c.busy() && warm) || (!c.busy() && room))
                && taker.map(|(_, te)| e < te).unwrap_or(true)
            {
                taker = Some((j, e));
            }
        }
        let (vi, _, _) = victim?;
        let (ti, _) = taker?;
        if vi == ti {
            return None;
        }
        Some((vi, ti))
    }

    fn worst_busy(&self, exclude: usize) -> Option<usize> {
        let mut best: Option<(usize, u64)> = None;
        for (j, c) in self.conns.iter().enumerate() {
            if j == exclude || !c.busy() {
                continue;
            }
            let left = c.range.unwrap().hi.saturating_sub(c.pos);
            if best.map(|(_, bl)| left > bl).unwrap_or(true) {
                best = Some((j, left));
            }
        }
        best.map(|(j, _)| j)
    }
}

/// Greedy concurrency allocation across multiple sources.
pub fn greedy_concurrency(
    rho: &[f64],
    gamma: &[f64],
    access_cap: f64,
    budget: usize,
) -> Vec<usize> {
    let m = rho.len();
    let mut n = vec![0usize; m];
    let g = |n: &[usize]| -> f64 {
        let sum: f64 = (0..m).map(|i| rho[i].min(n[i] as f64 * gamma[i])).sum();
        sum.min(access_cap)
    };
    let mut cur = g(&n);
    for _ in 0..budget {
        let mut best = (0usize, 0.0f64);
        for i in 0..m {
            n[i] += 1;
            let gain = g(&n) - cur;
            n[i] -= 1;
            if gain > best.1 {
                best = (i, gain);
            }
        }
        if best.1 <= 0.0 {
            break; // saturated: further connections are pure cost
        }
        n[best.0] += 1;
        cur += best.1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(gamma: f64) -> Source {
        Source {
            gamma_est: gamma,
            delta_est: 0.05,
            ..Default::default()
        }
    }

    /// An arrival from a request that has already been superseded must not be
    /// credited against the request that replaced it.
    ///
    /// The bytes are real and on disk, so crediting them looks harmless — but it
    /// advances the cursor past where the NEW request starts reading, and every
    /// arrival from that request then fails the `off == pos` test and is
    /// discarded. The connection delivers bytes the scheduler never counts, so it
    /// reads as silent and is only rescued by the stall timeout, seconds later.
    /// That is the same dead air the transport's error handling exists to remove,
    /// reintroduced through the arrival path.
    #[test]
    fn a_late_arrival_from_a_superseded_request_is_not_credited() {
        let mut s = Scheduler::new(1000, vec![src(1.0)], &[1]);
        s.tick(0.0);
        assert_eq!(s.conn_range(0), Some((0, 0, 1000)));
        s.on_bytes_at(0, 0, 100, 1.0, 0.5);
        assert_eq!(s.bytes_held(), 100);
        s.on_conn_error(0, 9.0, 0.0);
        let acts = s.tick(10.0);
        assert!(
            matches!(acts.as_slice(), [Action::Request { conn: 0, range }] if range.lo == 100),
            "the reclaimed remainder must be re-requested from 100: {acts:?}"
        );
        s.on_bytes_at(0, 100, 50, 9.5, 0.1);
        assert_eq!(
            s.bytes_held(),
            100,
            "an arrival older than the request in flight was credited to it"
        );
        s.on_bytes_at(0, 100, 50, 10.2, 0.1);
        assert_eq!(
            s.bytes_held(),
            150,
            "the live request's arrival was discarded as stale"
        );
    }

    #[test]
    fn initial_split_covers_exactly() {
        let mut s = Scheduler::new(1000, vec![src(1.0), src(1.0)], &[1, 1]);
        let acts = s.tick(0.0);
        assert_eq!(acts.len(), 2);
        assert!(s.coverage_holds());
        assert!(s.unassigned.is_empty());
    }

    #[test]
    fn coverage_and_liveness_hold_through_a_transfer() {
        let mut s = Scheduler::new(1_000_000, vec![src(1e5), src(5e4)], &[2, 2]);
        let mut now = 0.0;
        for _ in 0..4000 {
            s.tick(now);
            for j in 0..s.n_conns() {
                s.on_bytes(j, 500, now, 0.01);
            }
            assert!(s.coverage_holds(), "coverage broke at t={now}");
            assert!(s.liveness_holds(), "stuck at t={now}");
            now += 0.01;
            if s.is_complete() {
                break;
            }
        }
        assert!(
            s.is_complete(),
            "did not finish: {} / {}",
            s.bytes_held(),
            1_000_000
        );
    }

    /// A connection whose active range is stolen down to its current position
    /// goes idle WITHOUT completing; without the queue-start path its queued
    /// bytes were never requested.
    #[test]
    fn fully_stolen_range_does_not_livelock() {
        let mut s = Scheduler::new(200_000, vec![src(1e5), src(1e5)], &[1, 1]);
        s.tick(0.0);
        let mut now = 0.06;
        for _ in 0..50 {
            s.on_bytes(0, 1000, now, 0.01);
            now += 0.01;
            s.tick(now);
        }
        for _ in 0..20000 {
            s.tick(now);
            s.on_bytes(0, 1000, now, 0.01);
            now += 0.01;
            assert!(s.liveness_holds(), "livelocked at t={now}");
            if s.is_complete() {
                break;
            }
        }
        assert!(s.is_complete());
    }

    /// Feed `conn` a window of arrivals at `rate` bytes/s ending at `now`.
    fn deliver(s: &mut Scheduler, conn: usize, pos: &mut u64, rate: f64, now: f64, dt: f64) {
        let n = (rate * dt) as u64;
        s.on_bytes_at(conn, *pos, n, now, dt);
        *pos += n;
    }

    /// A connection whose rate is still climbing is in slow start, not slow, and
    /// repair must leave it alone.
    ///
    /// Traced on a 100 ms path: one flow at 16 MB/s against its twin at 49 had
    /// 256 MB taken from it at t=1.4 s, and both were at 90 MB/s by t=2.0 s. The
    /// cascade of repairs undoing that is what put `-x 2` at 7-8 s against
    /// `aria2c`'s 5.9 s on a path where two plain ranges finish together.
    #[test]
    fn a_victim_still_in_slow_start_is_not_robbed() {
        const SIZE: u64 = 1_000_000_000;
        let mut s = Scheduler::new(SIZE, vec![src(9e7)], &[2]).with_stall_timeout(30.0);
        let acts = s.tick(0.0);
        let (lo0, lo1) = (acts_range(&acts, 0).0, acts_range(&acts, 1).0);
        let (mut p0, mut p1) = (lo0, lo1);
        let mut shrinks = 0;
        let mut t = 0.0;
        // Conn 0 is settled at 90 MB/s. Conn 1 doubles every 0.4 s from 8 MB/s:
        // a textbook slow start that reaches its twin's rate at about t=2.
        while t < 3.0 {
            t += 0.1;
            let r1 = (8e6 * 2f64.powf(t / 0.4)).min(9e7);
            deliver(&mut s, 0, &mut p0, 9e7, t, 0.1);
            deliver(&mut s, 1, &mut p1, r1, t, 0.1);
            shrinks += s
                .tick(t)
                .iter()
                .filter(|a| matches!(a, Action::Shrink { .. }))
                .count();
        }
        assert_eq!(
            shrinks, 0,
            "a connection whose rate was still climbing was repaired against"
        );
        assert!(s.coverage_holds());
    }

    /// The other direction still works: a connection that has SETTLED at a
    /// fraction of its peer's rate is a real laggard, and repair must move work
    /// off it once its rate is established.
    #[test]
    fn a_settled_laggard_is_still_repaired() {
        const SIZE: u64 = 1_000_000_000;
        let mut s = Scheduler::new(SIZE, vec![src(9e7)], &[2]).with_stall_timeout(30.0);
        let acts = s.tick(0.0);
        let (lo0, lo1) = (acts_range(&acts, 0).0, acts_range(&acts, 1).0);
        let (mut p0, mut p1) = (lo0, lo1);
        let mut shrinks = 0;
        let mut t = 0.0;
        // Conn 1 is flat at 30 MB/s from its first sample: no climb to wait out.
        while t < 4.0 && shrinks == 0 {
            t += 0.1;
            deliver(&mut s, 0, &mut p0, 9e7, t, 0.1);
            deliver(&mut s, 1, &mut p1, 3e7, t, 0.1);
            shrinks += s
                .tick(t)
                .iter()
                .filter(|a| matches!(a, Action::Shrink { .. }))
                .count();
        }
        assert!(
            shrinks > 0,
            "a connection settled at a third of its peer's rate was never repaired"
        );
        assert!(s.coverage_holds());
    }

    fn acts_range(acts: &[Action], conn: usize) -> (u64, u64) {
        acts.iter()
            .find_map(|a| match a {
                Action::Request { conn: c, range } if *c == conn => Some((range.lo, range.hi)),
                _ => None,
            })
            .expect("initial split requests every connection")
    }

    #[test]
    fn stall_reclaim_returns_bytes() {
        let mut s = Scheduler::new(100_000, vec![src(1e5), src(1e5)], &[1, 1]);
        s.tick(0.0);
        let before = s.stats.reclaims;
        let acts = s.tick(5.0);
        assert!(s.stats.reclaims > before);
        assert!(acts.iter().any(|a| matches!(a, Action::Cancel { .. })));
        assert!(s.coverage_holds());
        assert!(s.liveness_holds());
    }

    #[test]
    fn suspend_source_reclaims_and_reassigns() {
        let mut s = Scheduler::new(100_000, vec![src(1e5), src(1e5)], &[1, 1]);
        s.tick(0.0);
        s.suspend_source(0, 10.0);
        // Reclaimed bytes are now unassigned. They are NOT reassigned instantly:
        // the surviving connection is still streaming its own range, and taking
        // work from it would violate nothing but achieve nothing either. Work
        // conservation only requires that no connection sit IDLE while work
        // remains -- so the reassignment happens when conn 1 next goes idle.
        assert!(s.coverage_holds());
        assert!(s.unassigned.total() > 0);

        let mut now = 0.2;
        let mut served_by_1 = false;
        for _ in 0..20_000 {
            let acts = s.tick(now);
            if acts
                .iter()
                .any(|a| matches!(a, Action::Request { conn, .. } if s.conns[*conn].source == 1))
            {
                served_by_1 = true;
            }
            s.on_bytes(1, 1000, now, 0.01);
            now += 0.01;
            assert!(s.coverage_holds());
            assert!(s.liveness_holds());
            if s.is_complete() {
                break;
            }
        }
        assert!(
            served_by_1,
            "surviving source never picked up the reclaimed work"
        );
        assert!(s.is_complete(), "held {} of 100000", s.bytes_held());
    }

    #[test]
    fn a_retired_source_never_gets_work_again_and_its_bytes_are_not_stranded() {
        // Suspension is a pause; retirement is not. A host that no longer
        // resolves does not get better on a timer, and retrying it costs a full
        // setup every backoff for the rest of the download.
        let mut s = Scheduler::new(100_000, vec![src(1e5), src(1e5)], &[1, 1]);
        s.tick(0.0);
        let freed = s.retire_source(0);
        assert_eq!(freed, vec![0], "the dead source's connections come back");
        assert_eq!(s.live_sources(), 1);
        assert!(!s.source_is_live(0));
        assert!(s.coverage_holds(), "reclaimed bytes must be accounted for");

        let mut now = 0.2;
        for _ in 0..20_000 {
            let acts = s.tick(now);
            assert!(
                !acts.iter().any(
                    |a| matches!(a, Action::Request { conn, .. } if s.conns[*conn].source == 0)
                ),
                "a retired source was handed work at t={now}"
            );
            s.on_bytes(1, 1000, now, 0.01);
            now += 0.01;
            assert!(s.coverage_holds());
            assert!(s.liveness_holds());
            if s.is_complete() {
                break;
            }
        }
        // The whole object still arrives: retirement reclaims, it does not strand.
        assert!(s.is_complete(), "held {} of 100000", s.bytes_held());
        // Retiring twice is a no-op rather than a second reclaim.
        assert!(s.retire_source(0).is_empty());
    }

    #[test]
    fn a_retired_source_is_not_reported_as_a_planned_pause() {
        // `all_sources_suspended_until` tells the transport that silence is
        // deliberate. A retired source has no return time, and reporting its
        // never-updated `suspended_until` of 0.0 would answer "everything is
        // available now" while nothing is.
        let mut s = Scheduler::new(1000, vec![src(1e5), src(1e5)], &[1, 1]);
        s.tick(0.0);
        s.retire_source(0);
        s.suspend_source(1, 10.0);
        assert_eq!(
            s.all_sources_suspended_until(1.0),
            Some(10.0),
            "the only LIVE source's return time is the answer"
        );
        // With every live source gone there is no return time at all.
        s.retire_source(1);
        assert_eq!(s.live_sources(), 0);
        assert_eq!(s.all_sources_suspended_until(1.0), None);
    }

    #[test]
    fn a_replacement_mirror_reuses_the_connections_the_dead_one_held() {
        // The reserve bench: a mirror list names far more sources than
        // politeness authorises sockets for. Substituting in place is what keeps
        // the aggregate socket count equal to what was authorised.
        let mut s = Scheduler::new(100_000, vec![src(1e5), src(1e5)], &[2, 2]);
        s.tick(0.0);
        let before = s.n_conns();
        let moved = s.replace_source(
            0,
            Source {
                priority: 3,
                ..src(2e5)
            },
        );
        assert_eq!(moved, vec![0, 1], "the same connection indices, relabelled");
        assert_eq!(s.n_conns(), before, "no new sockets are authorised");
        assert_eq!(s.n_sources(), 2);
        assert!(s.source_is_live(0));
        assert_eq!(s.source_priority(0), 3);
        assert!(s.coverage_holds());

        // The replacement is not graded by the dead host's failure: a connection
        // inheriting a Suspect grade could never be chosen as a repair taker.
        for j in moved {
            assert_eq!(s.conn_health(j), crate::detect::Health::Healthy);
            assert_eq!(s.conn_rate(j), 0.0);
        }

        let mut now = 0.2;
        for _ in 0..20_000 {
            s.tick(now);
            for j in 0..s.n_conns() {
                s.on_bytes(j, 500, now, 0.01);
            }
            now += 0.01;
            assert!(s.coverage_holds());
            if s.is_complete() {
                break;
            }
        }
        assert!(s.is_complete(), "held {} of 100000", s.bytes_held());
    }

    #[test]
    fn a_stated_priority_biases_the_first_split_and_nothing_after_it() {
        // The prior is worth having because the first split has to be made from
        // something. It is worth having ONLY as a prior: a ranking cannot know
        // the preferred mirror is overloaded, and defending it against evidence
        // would be strictly worse than having no ranking at all.
        let ranked = |p: u32| Source {
            priority: p,
            ..src(1e5)
        };
        let mut s = Scheduler::new(120_000, vec![ranked(1), ranked(4)], &[1, 1]);
        let acts = s.tick(0.0);
        let mut got = [0u64; 2];
        for a in &acts {
            if let Action::Request { conn, range } = a {
                got[s.conn_source(*conn)] += range.hi - range.lo;
            }
        }
        assert!(
            got[0] > got[1],
            "rank 1 must open with more than rank 4: {got:?}"
        );

        // Unranked sources split as they did before priorities existed, so
        // nothing changes for a caller that states none.
        let mut flat = Scheduler::new(120_000, vec![src(1e5), src(1e5)], &[1, 1]);
        let acts = flat.tick(0.0);
        let mut even = [0u64; 2];
        for a in &acts {
            if let Action::Request { conn, range } = a {
                even[flat.conn_source(*conn)] += range.hi - range.lo;
            }
        }
        assert_eq!(even[0], even[1], "no ranking must mean no bias: {even:?}");

        // And the prior does not survive contact with measurement: give the
        // top-ranked source nothing and the transfer still finishes off the
        // other one.
        let mut now = 0.2;
        for _ in 0..40_000 {
            s.tick(now);
            s.on_bytes(1, 400, now, 0.01);
            now += 0.01;
            assert!(s.coverage_holds());
            if s.is_complete() {
                break;
            }
        }
        assert!(
            s.is_complete(),
            "a silent top-ranked mirror must not hold the transfer: held {}",
            s.bytes_held()
        );
    }

    #[test]
    fn greedy_matches_exhaustive_small() {
        // rho/gamma chosen so the optimum is interior
        let rho = [2.2e6, 1.1e6, 0.7e6];
        let gam = [0.55e6, 0.45e6, 0.35e6];
        let cap = 5.0e6;
        for budget in 1..10usize {
            let n = greedy_concurrency(&rho, &gam, cap, budget);
            let g = |n: &[usize]| -> f64 {
                let s: f64 = (0..3).map(|i| rho[i].min(n[i] as f64 * gam[i])).sum();
                s.min(cap)
            };
            let mut best = 0.0f64;
            for a in 0..=budget {
                for b in 0..=budget {
                    for c in 0..=budget {
                        if a + b + c <= budget {
                            best = best.max(g(&[a, b, c]));
                        }
                    }
                }
            }
            assert!(
                (g(&n) - best).abs() < 1.0,
                "budget {budget}: greedy {} vs {}",
                g(&n),
                best
            );
        }
    }

    #[test]
    fn saturation_stops_allocation() {
        // one source, rho = 2*gamma: two connections saturate it
        let n = greedy_concurrency(&[2.0e6], &[1.0e6], 1e9, 10);
        assert_eq!(
            n[0], 2,
            "allocated {n:?}, expected exactly the saturation point"
        );
    }
    /// The detector must make the SCHEDULER act sooner, not merely grade sooner.
    ///
    /// A connection collapsing to 3% of its rate must be chosen as a repair
    /// victim well before the stall timeout would have reclaimed it. Without
    /// health-ranked victim selection the scheduler waits for the projected ETA
    /// to drift, which is the fixed detection cost measured at 0.25-0.9 s.
    #[test]
    fn collapsed_connection_becomes_a_repair_victim_before_the_stall_timeout() {
        const S: u64 = 40_000_000;
        let mut sc = Scheduler::new(S, vec![src(4e6), src(4e6)], &[1, 1]).with_stall_timeout(10.0);
        sc.tick(0.0);
        let mut now = 0.0;
        // Both healthy for a while.
        for _ in 0..12 {
            now += 0.1;
            sc.on_bytes(0, 400_000, now, 0.1);
            sc.on_bytes(1, 400_000, now, 0.1);
            sc.tick(now);
        }
        assert_eq!(sc.conn_health(0), crate::detect::Health::Healthy);

        // Connection 0 collapses; connection 1 keeps its rate.
        let mut flagged_at = None;
        for _ in 0..8 {
            now += 0.1;
            sc.on_bytes(0, 12_000, now, 0.1);
            sc.on_bytes(1, 400_000, now, 0.1);
            sc.tick(now);
            if flagged_at.is_none() && sc.conn_health(0).is_suspect_or_worse() {
                flagged_at = Some(now);
            }
        }
        let t = flagged_at.expect("collapse must be graded");
        assert!(
            t < 1.2 + 10.0,
            "must be flagged well before the 10 s stall timeout, was {t}"
        );
        // And the healthy connection must never be the one downgraded.
        assert_eq!(
            sc.conn_health(1),
            crate::detect::Health::Healthy,
            "the connection holding its rate must stay Healthy"
        );
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }
    /// The repair deadband must never fall below what a repair costs.
    ///
    /// `theta = scale*sqrt(delta*T_rem/n)` has the right shape but is unbounded
    /// below, and it approaches zero from two directions that both make repair a
    /// worse idea: `T_rem` shrinks as the transfer ends, `n` grows with
    /// concurrency. Measured on the shared-bottleneck harness, theta reached
    /// 0.061-0.081 s against a delta of 0.12 s — so the scheduler was spending a
    /// 0.12 s setup to recover a 0.06 s divergence, dozens of times per transfer.
    #[test]
    fn the_repair_deadband_never_drops_below_one_setup_cost() {
        const S: u64 = 8_000_000;
        const D: f64 = 0.12;
        let mk = |n: usize| {
            let sources = vec![Source {
                gamma_est: 1.4e6 / n as f64,
                delta_est: D,
                ..Default::default()
            }];
            Scheduler::new(S, sources, &[n])
        };
        // Sweep concurrency and progress: both drive theta down.
        for &n in &[1usize, 2, 4, 8, 16, 64] {
            let mut sc = mk(n);
            sc.tick(0.0);
            let mut now = 0.0;
            // Deliver most of the object, so T_rem — and with it the unfloored
            // band — becomes small.
            for _ in 0..60 {
                now += 0.05;
                for j in 0..n {
                    if sc.conn_range(j).is_some() {
                        sc.on_bytes(j, 100_000 / n as u64, now, 0.05);
                    }
                }
                sc.tick(now);
                let th = sc.theta_now(now);
                assert!(
                    th >= D - 1e-12,
                    "theta {th} fell below delta {D} at n={n}, progress {}/{S}: \
                     the scheduler would pay a full setup to recover a smaller divergence",
                    sc.bytes_held()
                );
            }
        }
    }

    /// A stable unequal split settles after ONE equalisation; a collapse still
    /// gets answered.
    ///
    /// These two assertions are one test on purpose. Suppressing spurious repair is
    /// trivial in isolation — never repair — and that would be a regression, not a
    /// fix: the mechanism exists for the mirror that dies mid-transfer. The
    /// property worth pinning is the DISCRIMINATION between the two cases.
    ///
    /// # What this test does NOT cover
    ///
    /// It does not reproduce the repair storm, and no test in this crate can. The
    /// storm was a feedback loop between the scheduler and the transport: a repair
    /// shrank the victim's range, the victim's socket kept streaming the span
    /// anyway, the duplicate traffic slowed the honest connections, and that
    /// slowdown re-diverged the finish times into another repair. The core cannot
    /// see any of that — it has no sockets — so it cannot close the loop. Feeding
    /// it a stable unequal split, as here, correctly produces exactly one repair
    /// (equalising a persistent 60/40 asymmetry IS profitable) and then stops.
    ///
    /// The loop itself is tested where it lives, against a served-byte count at the
    /// origin: `hydra-net/tests/shrink_e2e.rs`.
    #[test]
    fn a_stable_unequal_split_settles_and_a_collapse_is_still_answered() {
        const S: u64 = 40_000_000;
        let src4 = || Source {
            gamma_est: 2e6,
            delta_est: 0.12,
            ..Default::default()
        };

        // --- stationary: two connections at persistently unequal but stable shares.
        // This is what flows sharing one bottleneck look like (share ~ 1/RTT), and
        // no repair can change it — the asymmetry is a property of the path.
        let mut sc = Scheduler::new(S, vec![src4(), src4()], &[1, 1]);
        sc.tick(0.0);
        let mut now = 0.0;
        for k in 0..60 {
            now += 0.1;
            // 60/40 split, with a little jitter, conserving the aggregate.
            let wobble = if k % 3 == 0 { 12_000 } else { -8_000 };
            sc.on_bytes(0, (240_000i64 + wobble) as u64, now, 0.1);
            sc.on_bytes(1, (160_000i64 - wobble) as u64, now, 0.1);
            sc.tick(now);
        }
        // One equalisation is correct here and the scheduler must then SETTLE: the
        // 60/40 share ratio is a property of the path, so re-equalising cannot
        // improve it and every further repair is a pure setup cost. 60 ticks over
        // 6 s of simulated transfer would be ample room for a storm.
        let stationary_repairs = sc.stats.repairs;
        assert!(
            stationary_repairs <= 1,
            "a stable unequal split provoked {stationary_repairs} repairs over 60 \
             ticks; one equalisation is profitable, repeated ones only pay setups"
        );

        // --- collapse: connection 0 drops to 2% and stays there.
        let mut sc = Scheduler::new(S, vec![src4(), src4()], &[1, 1]);
        sc.tick(0.0);
        let mut now = 0.0;
        for _ in 0..20 {
            now += 0.1;
            sc.on_bytes(0, 200_000, now, 0.1);
            sc.on_bytes(1, 200_000, now, 0.1);
            sc.tick(now);
        }
        let before = sc.stats.repairs;
        for _ in 0..40 {
            now += 0.1;
            sc.on_bytes(0, 4_000, now, 0.1);
            sc.on_bytes(1, 200_000, now, 0.1);
            sc.tick(now);
        }
        assert!(
            sc.stats.repairs > before,
            "a connection collapsing to 2% of its rate produced no repair: the \
             profitability test is suppressing the case repair exists for"
        );
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }

    /// A sole source must never be suspended past a caller's patience.
    ///
    /// Exponential backoff is right when work can go somewhere else. With one source
    /// it is a self-inflicted outage: nothing can move until the suspension expires,
    /// and a transport whose watchdog fails on silence cannot distinguish that from
    /// the source being gone.
    ///
    /// The numbers that made this real: `stall_timeout` 4.0s gives the transport a
    /// no-progress deadline of `4 * (4.0 + delta)` = 16.2s, while five consecutive
    /// stalls suspended the sole source for `min(4.0 * 2^3, 30)` = 30s. Measured on a
    /// 121.7 MiB GitHub release asset, 4 of 8 multi-connection runs aborted with a
    /// digest mismatch — three holding 126.9-127.0 MB of 127.6 MB, killed during a
    /// deliberate backoff over the final half-megabyte.
    #[test]
    fn a_sole_source_is_never_suspended_longer_than_its_stall_timeout() {
        const S: u64 = 8_000_000;
        let st = 4.0;
        let mut sc = Scheduler::new(S, vec![src(4e6)], &[4]).with_stall_timeout(st);
        sc.tick(0.0);

        // Drive it through many consecutive stalls, which is what escalates backoff.
        let mut now = 0.0;
        let mut worst_suspension = 0.0f64;
        for _ in 0..12 {
            now += st * 1.5;
            sc.tick(now);
            if let Some(until) = sc.all_sources_suspended_until(now) {
                worst_suspension = worst_suspension.max(until - now);
            }
        }
        assert!(
            worst_suspension <= st.max(1.0) + 1e-9,
            "sole source suspended for {worst_suspension:.1}s against a {st:.1}s stall \
             timeout: a caller's no-progress watchdog will kill the transfer during a \
             pause the scheduler chose"
        );
    }

    /// Ramping concurrency must find work WAITING, not have to steal it.
    ///
    /// With the ramp enabled the transfer starts with one connection active. If the
    /// initial split gives that connection the whole object, every connection
    /// admitted afterwards finds the unassigned set empty and its only route to
    /// work is a steal — paying a repair to undo a split that should not have been
    /// made. Measured cost of that mistake on a live 3.15 MB transfer: ~21 s
    /// against 6.3 s for fixed concurrency, with several runs reported as failures
    /// despite delivering byte-exact files.
    ///
    /// The invariant: while the active limit is below the connection budget, some
    /// work stays unassigned, and raising the limit produces `Request` actions
    /// rather than repairs.
    #[test]
    fn a_ramping_transfer_finds_unassigned_work_instead_of_stealing() {
        const S: u64 = 40_000_000;
        let sources = vec![Source {
            gamma_est: 2e6,
            delta_est: 0.05,
            ..Default::default()
        }];
        let mut sc = Scheduler::new(S, sources, &[8]).with_active_limit(1);
        let acts = sc.tick(0.0);
        assert_eq!(
            acts.iter()
                .filter(|a| matches!(a, Action::Request { .. }))
                .count(),
            1,
            "only the active connection may be given work"
        );
        assert!(
            !sc.unassigned_is_empty(),
            "the whole object was handed to one connection: connections admitted \
             later can only steal, which costs a repair each"
        );

        // Deliver some bytes, then admit more connections as the ramp would.
        let mut now = 0.0;
        for _ in 0..5 {
            now += 0.1;
            sc.on_bytes(0, 200_000, now, 0.1);
            sc.tick(now);
        }
        let repairs_before = sc.stats.repairs;
        sc.set_active_limit(4);
        now += 0.1;
        let acts = sc.tick(now);
        let reqs = acts
            .iter()
            .filter(|a| matches!(a, Action::Request { .. }))
            .count();
        assert!(
            reqs >= 3,
            "admitting 3 connections produced {reqs} requests: they are not being \
             given the reserved work"
        );
        assert_eq!(
            sc.stats.repairs, repairs_before,
            "admitting a connection must not cost a repair"
        );
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }

    /// The reserve must survive the ramping connection RUNNING OUT OF WORK.
    ///
    /// [`a_ramping_transfer_finds_unassigned_work_instead_of_stealing`] pins the
    /// reserve as `initial_split` leaves it, and never lets the one active
    /// connection finish what it was given. That is the easy half. The half that
    /// actually decides whether a ramped transfer is fast is what work-conserving
    /// assignment hands out on the tick AFTER the active connection drains its
    /// quota — which, with the ramp still at one connection, is the common case on
    /// any object larger than a few of those quotas.
    ///
    /// Getting it wrong looks exactly like never having reserved at all: the idle
    /// connection takes `u64::MAX`, the reserve `initial_split` carefully held back
    /// is swallowed whole, and every connection the ramp admits afterwards finds
    /// nothing unassigned and must steal — the ~21 s against 6.3 s regression the
    /// sibling test above quotes, arrived at one tick later.
    ///
    /// The invariant, stated where it belongs: while the ramp can still grow, no
    /// single assignment may consume the reserve, whoever asks and whenever.
    #[test]
    fn a_ramping_connection_that_drains_its_quota_does_not_swallow_the_reserve() {
        const S: u64 = 40_000_000;
        let sources = vec![Source {
            gamma_est: 2e6,
            delta_est: 0.05,
            ..Default::default()
        }];
        let mut sc = Scheduler::new(S, sources, &[8]).with_active_limit(1);
        sc.tick(0.0);
        let (lo, _, hi) = sc
            .conn_range(0)
            .expect("the active connection holds a range");
        let quota = hi - lo;
        assert!(
            quota < S,
            "initial_split handed the whole object to one connection"
        );

        // Drain that quota completely, so the connection goes idle with the ramp
        // still at one and the reserve still untouched.
        let mut now = 0.0;
        now += 0.1;
        sc.on_bytes(0, quota, now, 0.1);
        assert!(
            sc.conn_range(0).is_none(),
            "the connection should have finished its range"
        );

        // The tick that re-assigns it. This is the one under test.
        now += 0.1;
        sc.tick(now);
        assert!(
            !sc.unassigned_is_empty(),
            "the idle connection took the entire remainder while the ramp was still \
             at one: every connection admitted later can only steal, which costs a \
             repair each"
        );

        // And the reserve must still be big enough to matter — not a token sliver
        // left by a rounding accident.
        let held_back = sc.unassigned_total();
        assert!(
            held_back > (S - quota) / 2,
            "only {held_back} of {} remaining bytes stayed unassigned: the reserve \
             is nominal, and connections admitted later will still have to steal",
            S - quota
        );
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }

    /// Settled concurrency must hand out MAXIMAL ranges, not shares.
    ///
    /// The reserve exists for connections that are still to be admitted. A caller
    /// that never opted into the ramp has none: `active_limit` is left at
    /// `usize::MAX` ("every connection active"), nothing is waiting in the wings,
    /// and holding work back only guarantees the connection that was given a
    /// share has to come back for the rest — a request per share, where range
    /// scheduling exists to make it one.
    ///
    /// This pins the sentinel specifically. Any test of the reserve condition
    /// written as `admitted < active_limit` is comparing against `usize::MAX` for
    /// this caller, is therefore always true, and silently turns every fixed
    /// `-x N` transfer into the share path with no test noticing — the maximal
    /// branch becomes unreachable outside the ramp.
    #[test]
    fn settled_concurrency_hands_out_maximal_ranges_not_shares() {
        const S: u64 = 40_000_000;
        let sources = vec![Source {
            gamma_est: 2e6,
            delta_est: 0.05,
            ..Default::default()
        }];
        // No `with_active_limit`: the default, fixed-concurrency caller.
        let mut sc = Scheduler::new(S, sources, &[4]);
        sc.tick(0.0);
        assert_eq!(
            sc.active_limit(),
            usize::MAX,
            "this test is about the usize::MAX sentinel; it is not being used"
        );

        // Hand two connections' ranges back, so there is unassigned work and two
        // idle connections to give it to. Their ranges are adjacent, so they
        // coalesce into one block.
        sc.on_conn_error(2, 0.0, 0.0);
        sc.on_conn_error(3, 0.0, 0.0);
        let reserve = sc.unassigned_total();
        assert!(reserve > 0, "nothing was handed back");

        let acts = sc.tick(0.1);
        let biggest = acts
            .iter()
            .filter_map(|a| match a {
                Action::Request { range, .. } => Some(range.hi - range.lo),
                _ => None,
            })
            .max()
            .expect("an idle connection must be given the returned work");
        assert_eq!(
            biggest, reserve,
            "the idle connection was handed {biggest} of {reserve} available bytes: \
             concurrency has settled and nothing is waiting to be admitted, so \
             holding a reserve back only buys a second request for the same work"
        );
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }

    /// Every range shrink must be ANNOUNCED, not just performed.
    ///
    /// Regression test for the repair storm. The scheduler used to move
    /// `conns[victim].range` and emit nothing, so the transport's fetch loop —
    /// which tests `off < hi` against the bound it captured at request time —
    /// went on pulling the span that had just been handed to another connection.
    /// Both connections then fetched the same bytes over the same bottleneck, the
    /// resulting slowdown read as fresh divergence, and that triggered further
    /// repairs: measured at 32-49 repairs on a stationary 5.3 MB transfer whose
    /// correct repair count is zero, for ~2.2x the fluid optimum.
    ///
    /// The invariant is therefore stronger than "a repair happened": for every
    /// repair counted, the victim whose far end moved must appear in a `Shrink`
    /// carrying the new bound. A caller cannot honour what it is not told.
    #[test]
    fn every_repair_announces_the_victims_new_far_end() {
        const S: u64 = 40_000_000;
        let mut sc = Scheduler::new(S, vec![src(4e6), src(4e6)], &[1, 1]).with_stall_timeout(10.0);
        sc.tick(0.0);
        let mut now = 0.0;
        for _ in 0..12 {
            now += 0.1;
            sc.on_bytes(0, 400_000, now, 0.1);
            sc.on_bytes(1, 400_000, now, 0.1);
            sc.tick(now);
        }

        // Collapse connection 0 so a divergence repair becomes correct to make.
        let mut shrinks: Vec<(usize, u64)> = Vec::new();
        let mut repairs_before = sc.stats.repairs;
        let mut saw_repair = false;
        for _ in 0..25 {
            now += 0.1;
            sc.on_bytes(0, 4_000, now, 0.1);
            sc.on_bytes(1, 400_000, now, 0.1);
            // Snapshot each victim's far end before the tick that may move it.
            let before: Vec<Option<u64>> = (0..sc.n_conns())
                .map(|j| sc.conn_range(j).map(|(_, _, hi)| hi))
                .collect();
            let acts = sc.tick(now);
            for a in &acts {
                if let Action::Shrink { conn, hi } = a {
                    shrinks.push((*conn, *hi));
                    // The announced bound must be the one actually installed, and
                    // it must be a genuine reduction — never a raise, which would
                    // hand out bytes another connection may already hold.
                    assert_eq!(
                        sc.conn_range(*conn).map(|(_, _, h)| h),
                        Some(*hi),
                        "announced bound must match the installed one"
                    );
                    if let Some(Some(b)) = before.get(*conn) {
                        assert!(*hi <= *b, "a shrink must lower the far end: {b} -> {hi}");
                    }
                }
            }
            if sc.stats.repairs > repairs_before {
                saw_repair = true;
                assert!(
                    !shrinks.is_empty(),
                    "a repair was counted with no Shrink announced: the victim's \
                     socket would keep streaming the stolen span"
                );
                repairs_before = sc.stats.repairs;
            }
        }
        assert!(saw_repair, "the scenario must produce at least one repair");
        assert!(sc.coverage_holds() && sc.liveness_holds());
    }

    /// The initial split must respect `mark_done`.
    ///
    /// Regression test: an earlier version partitioned `[0, size)` arithmetically
    /// and never consulted the unassigned set, so `mark_done` was silently
    /// ignored. That broke `--range` (fetched from offset 0 instead of the
    /// requested interval) and `--continue` (re-fetched bytes already on disk).
    #[test]
    fn initial_split_never_requests_bytes_marked_done() {
        let size = 100_000u64;
        let mut s = Scheduler::new(size, vec![src(1e6), src(1e6)], &[1, 1]);
        // Range mode: only [90_000, 90_512) is wanted.
        s.mark_done(0, 90_000);
        s.mark_done(90_512, size);
        let acts = s.tick(0.0);
        assert!(
            !acts.is_empty(),
            "the wanted interval must still be requested"
        );
        for a in &acts {
            if let Action::Request { range, .. } = a {
                assert!(
                    range.lo >= 90_000 && range.hi <= 90_512,
                    "requested {range:?} outside the wanted interval"
                );
            }
        }
        assert!(s.coverage_holds());
    }

    /// Overlapping `mark_done` calls must not inflate the held count.
    ///
    /// Regression test for a silent truncation. `mark_done` credited the width of
    /// the span it was given rather than the bytes it actually claimed, so two
    /// callers marking the same prefix — a `-c` resume replaying its sidecar, and
    /// the concurrency probe reporting the bytes it fetched, both of which start
    /// at offset 0 — pushed `held` past the object's real length. `is_complete()`
    /// tests exactly that counter, so the transfer stopped believing it was
    /// finished and left a zero-filled hole in the tail of a file it reported as
    /// a success: measured at 240 138 unwritten bytes on an 11 200 900-byte
    /// object whose gzip then refused to decompress.
    #[test]
    fn overlapping_mark_done_credits_each_byte_once() {
        let size = 100_000u64;
        let mut s = Scheduler::new(size, vec![src(1e6)], &[1]);
        s.mark_done(0, 30_000); // a resume record
        s.mark_done(0, 10_000); // the probe, re-reporting part of the same prefix
        assert_eq!(
            s.bytes_held(),
            30_000,
            "the overlap must be credited once, not twice"
        );
        assert!(!s.is_complete(), "70 000 bytes are still missing");

        // Marking every byte, in overlapping pieces, is completion — and exactly
        // completion, never more.
        s.mark_done(20_000, size);
        s.mark_done(0, size);
        assert_eq!(s.bytes_held(), size);
        assert!(s.is_complete());
    }

    /// After the probe's ranges are marked, `held_ranges` must describe them.
    ///
    /// This is what the pre-transfer checkpoint writes into the sidecar, so that a
    /// ^C during or shortly after the concurrency probe does not discard bytes the
    /// probe already fetched at true offsets. The periodic checkpoint inside the
    /// transfer only fires after 2 seconds, which an early interrupt beats.
    #[test]
    fn held_ranges_reports_probe_bytes_before_any_transfer() {
        let size = 11_200_900u64;
        let mut s = Scheduler::new(size, vec![Source::default()], &[1]);
        // Nothing fetched yet: nothing to checkpoint, and an empty record must not
        // be written as though it were progress.
        assert!(s.held_ranges().is_empty());

        // The probe fetched a 3 MiB prefix into the real output.
        s.mark_done(0, 3 << 20);
        assert_eq!(s.held_ranges(), vec![(0, 3 << 20)]);
        assert_eq!(s.bytes_held(), 3 << 20);

        // A second, disjoint probe range is reported as its own span rather than
        // merged into a count: a byte count cannot describe a hole, which is why
        // the sidecar stores ranges.
        s.mark_done(5 << 20, 6 << 20);
        assert_eq!(s.held_ranges(), vec![(0, 3 << 20), (5 << 20, 6 << 20)]);

        // Adjacent spans DO coalesce, so the record stays compact across a long run.
        s.mark_done(3 << 20, 5 << 20);
        assert_eq!(s.held_ranges(), vec![(0, 6 << 20)]);
    }

    /// Resume: bytes already on disk must never be re-requested.
    #[test]
    fn resume_does_not_refetch_held_prefix() {
        let size = 64_000u64;
        let mut s = Scheduler::new(size, vec![src(1e6)], &[2]);
        s.mark_done(0, 48_000); // three quarters already fetched
        let acts = s.tick(0.0);
        for a in &acts {
            if let Action::Request { range, .. } = a {
                assert!(
                    range.lo >= 48_000,
                    "re-requested a held byte at {}",
                    range.lo
                );
            }
        }
        assert_eq!(
            s.bytes_held(),
            48_000,
            "held count must include the resumed prefix"
        );
        assert!(s.coverage_holds());
    }
}

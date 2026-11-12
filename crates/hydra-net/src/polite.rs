//! Politeness: pacing, rate limiting, and server-friendly connection management.
//!
//! Features:
//! * Per-host connection ceilings suitable for public mirrors;
//! * `Retry-After` parsing (both delta-seconds and HTTP-date forms) with
//!   exponential backoff and jitter for 429/503 responses;
//! * Token-bucket rate limiter for bandwidth throttling;
//! * Bounded redirect handling with validation at each hop.
//!
//! The defaults here are deliberately conservative. A user who wants to saturate
//! their own server can raise them; a user downloading from a volunteer mirror
//! should not have to know they exist.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default connections per host. Public-mirror etiquette, not a physical limit:
/// four connections per host balances parallelism against server politeness and
/// remains within standard acceptable load thresholds.
pub const DEFAULT_PER_HOST: usize = 4;

/// Default ceiling across all hosts, so a large mirror list cannot fan out into
/// hundreds of sockets.
pub const DEFAULT_TOTAL: usize = 16;

/// Redirect depth limit. A download that redirects more than a handful of times
/// is typically misconfigured, and each hop costs a full `delta` round-trip.
pub const MAX_REDIRECTS: u32 = 8;

/// The addresses a redirect chain has already asked for.
///
/// [`MAX_REDIRECTS`] bounds a chain but cannot tell a long one from a loop,
/// and the two need different answers. An origin that replies to a request
/// with a `Location` naming that same request is not forwarding it anywhere —
/// it is refusing, typically a bot wall issuing a cookie it expects a browser
/// to echo back — so following it spends the whole budget on round trips and
/// then blames the budget, which was never the problem. Remembering where the
/// chain has been names the loop on the hop that closes it, for no extra
/// request.
///
/// Addresses compare with the host lowercased and the scheme's default port
/// elided, because `https://h/p` and `https://H:443/p` are one address and a
/// chain that read them as two would walk the loop anyway.
#[derive(Clone, Debug)]
pub struct RedirectChain {
    seen: Vec<String>,
}

impl RedirectChain {
    /// A chain whose first request goes to `url`.
    ///
    /// Seeded rather than empty: a `Location` echoing the URL just requested
    /// is the common shape of this failure, and an empty chain would have to
    /// take that hop once before it could recognise it.
    pub fn new(url: &str) -> Self {
        Self {
            seen: vec![canonical_url(url)],
        }
    }

    /// Take the chain to `url`. `false` when it has been there already, which
    /// is a loop rather than a hop worth spending.
    #[must_use]
    pub fn advance(&mut self, url: &str) -> bool {
        let next = canonical_url(url);
        if self.seen.contains(&next) {
            return false;
        }
        self.seen.push(next);
        true
    }
}

/// One spelling for an address, so that two spellings of the same one compare
/// equal.
///
/// Scheme and host are case-insensitive and the default port is redundant
/// (RFC 3986 §3.1, §6.2.3); the path is neither, and is left alone. Anything
/// this cannot read is returned unchanged — an address that cannot be
/// canonicalised still compares against itself, which is the case that
/// matters.
fn canonical_url(url: &str) -> String {
    let url = url.trim();
    let (scheme, default_port) = if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        ("https://", ":443")
    } else if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        ("http://", ":80")
    } else {
        return url.to_string();
    };
    let rest = &url[scheme.len()..];
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let authority = authority
        .strip_suffix(default_port)
        .unwrap_or(authority)
        .to_ascii_lowercase();
    format!("{scheme}{authority}{path}")
}

/// How far the rate limiter's cursor may lag the clock: the burst a transfer
/// may send without waiting, as time at the current rate.
///
/// Sized to what it must actually absorb, which is one timer tick of oversleep
/// plus scheduling jitter, and no more. It is also the allowance a transfer gets
/// at its very start, when the cursor is unset, so every millisecond here is
/// milliseconds of bytes delivered above the cap: at 50 ms a 512 MiB object
/// under a 200 MiB/s cap finished 2% early, which is the cap being exceeded
/// rather than a rounding artefact. At 10 ms the same run is within 0.5%.
/// See [`RateLimiter::reserve`].
const BURST_WINDOW: Duration = Duration::from_millis(10);

/// Debts shorter than this are carried to the next reservation rather than
/// slept, because a sleep this short cannot be timed by a 1 ms timer. See
/// [`Pace::wait`].
const MIN_PAUSE: Duration = Duration::from_millis(2);

/// Politeness configuration.
#[derive(Clone, Copy, Debug)]
pub struct Politeness {
    pub per_host: usize,
    pub total: usize,
    /// Ceiling on any single backoff, so a hostile `Retry-After: 86400` cannot
    /// hang the transfer.
    pub max_backoff: Duration,
    /// Conservative mode: one connection per host, for mirrors that should be
    /// treated gently regardless of what the measurement says.
    pub conservative: bool,
}

impl Default for Politeness {
    fn default() -> Self {
        Self {
            per_host: DEFAULT_PER_HOST,
            total: DEFAULT_TOTAL,
            max_backoff: Duration::from_secs(60),
            conservative: false,
        }
    }
}

impl Politeness {
    pub fn conservative() -> Self {
        Self {
            per_host: 1,
            total: 2,
            conservative: true,
            ..Default::default()
        }
    }

    /// Connections this configuration permits for `host`, given what the
    /// scheduler asked for. The minimum of physics (what the caller computed)
    /// and etiquette (this ceiling).
    pub fn allow(&self, requested: usize) -> usize {
        requested.clamp(1, self.per_host.max(1))
    }

    /// Split `requested` connections across `sources`, honouring BOTH ceilings.
    ///
    /// Returns the per-source counts, one entry per source. `allow` only ever
    /// clamped against `per_host`, so nothing in the download path read `total`
    /// at all: `--max-total-connections 2 -x 8` opened eight. The aggregate is
    /// the ceiling a server operator actually feels — eight connections split
    /// across two mirrors is still eight sockets — so it is enforced here, where
    /// the split is decided, rather than at connect time where refusing a slot
    /// would leave the scheduler holding a range nobody can fetch.
    ///
    /// Every source that gets a slot gets at least one. When the total is smaller
    /// than the source count, the surplus sources are dropped (empty entries)
    /// rather than shaved to zero-connection targets the scheduler would then
    /// have to reason about.
    pub fn split(&self, requested: usize, sources: usize) -> Vec<usize> {
        if sources == 0 {
            return Vec::new();
        }
        let per_host = self.per_host.max(1);
        let total = self.total.max(1);
        // Never more sockets in total than either the caller asked for or the
        // aggregate ceiling permits.
        let budget = requested.max(1).min(total);
        // Only as many sources as there is budget to give one connection each.
        let used = sources.min(budget);
        let mut out = vec![0usize; sources];
        for (i, slot) in out.iter_mut().enumerate().take(used) {
            // Distribute the remainder over the leading sources rather than
            // piling it on the first: `5` over `2` is `3, 2`, not `4, 1`.
            let share = budget / used + usize::from(i < budget % used);
            *slot = share.min(per_host).max(1);
        }
        out
    }

    /// Split `requested` connections across RANKED sources.
    ///
    /// The same two ceilings as [`Self::split`], plus whatever each source states
    /// about itself — see [`hya_core::plan::allocate`], which is where the
    /// arithmetic lives so that it is identical under the simulator and under
    /// real HTTP.
    ///
    /// [`Self::split`] remains the right call when the sources are
    /// interchangeable and nothing is known about them; this one exists because
    /// a mirror list is not that. A Metalink ranks its mirrors and some of them
    /// state their own connection ceilings, and an even split throws both away.
    pub fn split_plan(&self, requested: usize, sources: &[hya_core::SourcePlan]) -> Vec<usize> {
        hya_core::plan::allocate(
            sources,
            requested,
            if self.conservative { 1 } else { self.per_host },
            self.total,
        )
    }
}

/// Parse a `Retry-After` header value into a delay.
///
/// Accepts both forms RFC 9110 permits: delta-seconds, and an HTTP-date. The
/// date form is common enough on real 503s that ignoring it means backing off
/// for the wrong interval.
pub fn parse_retry_after(v: &str, now_unix: u64) -> Option<Duration> {
    let t = v.trim();
    if let Ok(secs) = t.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // IMF-fixdate: "Wed, 21 Oct 2015 07:28:00 GMT"
    let when = parse_http_date(t)?;
    Some(Duration::from_secs(when.saturating_sub(now_unix)))
}

/// Minimal IMF-fixdate parser returning a unix timestamp.
///
/// Hand-rolled rather than pulling in a date crate: this is the only date the
/// downloader ever parses, and a wrong answer here only changes a backoff.
/// Parse an HTTP-date (RFC 7231 IMF-fixdate) into a Unix timestamp.
///
/// Public because `--remote-time` needs it: a `Last-Modified` validator carries a
/// usable timestamp, while an opaque `ETag` does not.
pub fn parse_http_date(s: &str) -> Option<u64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    let day: u64 = p[1].parse().ok()?;
    let month = match p[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u64 = p[3].parse().ok()?;
    let hms: Vec<&str> = p[4].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let (h, mi, sec): (u64, u64, u64) = (
        hms[0].parse().ok()?,
        hms[1].parse().ok()?,
        hms[2].parse().ok()?,
    );
    // Out-of-range fields would underflow `days_from_civil` (day 0 in March
    // reaches `doy - 1` below zero), so they are not a date at all.
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // A date before the epoch has no `u64` timestamp, and `days_from_civil`
    // would wrap to the far future. Servers send one to delete a cookie
    // (`expires=Thu, 01 Jan 1900 00:00:00 GMT`), so it means "already past".
    // IMF-fixdate's year is `4DIGIT`. A longer one would overflow
    // `days_from_civil` and the `* 86_400` after it, so it is not a date.
    if year > 9999 {
        return None;
    }
    if year < 1970 {
        return Some(0);
    }
    Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
///
/// Shared with [`crate::signed`], which parses the same instant out of SigV4's
/// `YYYYMMDDTHHMMSSZ`. Two copies of leap-year arithmetic is one copy too many:
/// the second would only be found wrong on a date nobody tests.
pub(crate) fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Exponential backoff with full jitter, capped.
///
/// Jitter is not decoration: without it, N connections that all hit the same 503
/// retry in lockstep and hammer the server in synchronised waves.
pub fn backoff_with_jitter(attempt: u32, base: Duration, cap: Duration, seed: u64) -> Duration {
    let mult = 1u64 << attempt.min(6);
    let ceil = base.saturating_mul(mult as u32).min(cap);
    // xorshift on the seed: deterministic per (attempt, seed), no rand dep.
    let mut x = seed.max(1);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let frac = (x % 1000) as f64 / 1000.0;
    // Full jitter: uniform in [0, ceil]. Halves the synchronised-retry problem
    // relative to equal-jitter, at the cost of occasionally retrying very soon.
    Duration::from_secs_f64(ceil.as_secs_f64() * (0.5 + 0.5 * frac))
}

/// Tracks live connections per host so the ceiling can be enforced.
#[derive(Clone, Default)]
pub struct HostLimiter {
    live: Arc<Mutex<HashMap<String, usize>>>,
}

impl HostLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to reserve a slot for `host`. Returns false when the ceiling binds.
    pub fn try_acquire(&self, host: &str, per_host: usize, total: usize) -> bool {
        let mut g = self.live.lock().unwrap();
        let sum: usize = g.values().sum();
        let cur = g.get(host).copied().unwrap_or(0);
        if cur >= per_host || sum >= total {
            return false;
        }
        *g.entry(host.to_string()).or_insert(0) += 1;
        true
    }

    pub fn release(&self, host: &str) {
        let mut g = self.live.lock().unwrap();
        if let Some(v) = g.get_mut(host) {
            *v = v.saturating_sub(1);
        }
    }

    pub fn live_for(&self, host: &str) -> usize {
        self.live.lock().unwrap().get(host).copied().unwrap_or(0)
    }
}

/// Token bucket for `--limit-rate`, shared across all connections.
///
/// Enforced on the AGGREGATE, not per connection: a user asking for 1 MB/s means
/// the transfer should use 1 MB/s, not 1 MB/s times however many connections the
/// scheduler happened to open.
pub struct RateLimiter {
    /// Bytes per second; 0 = unlimited.
    rate: AtomicU64,
    /// Monotonic cursor: the time by which everything granted has been sent.
    cursor: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            rate: AtomicU64::new(bytes_per_sec),
            cursor: Mutex::new(None),
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    pub fn is_limited(&self) -> bool {
        self.rate.load(Ordering::Relaxed) > 0
    }

    /// Change the cap. 0 = unlimited. Takes effect on the next reservation.
    pub fn set_rate(&self, bytes_per_sec: u64) {
        let prev = self.rate.swap(bytes_per_sec, Ordering::Relaxed);
        if prev == bytes_per_sec {
            return;
        }
        // Forget the debt the OLD rate accrued. The cursor is a time computed
        // from a rate that no longer applies: a minute of queue built up at
        // 1 KB/s is not a minute of queue at 10 MB/s, and carrying it over
        // would make a transfer sit idle for the remainder of a schedule
        // nobody is asking for any more — switching the Speed Limiter OFF
        // would visibly stall the download it was meant to release.
        //
        // Lowering the cap forgives whatever was already reserved, which is at
        // most one read per connection. That is the right side to err on: the
        // cap binds from here, and a cap is not owed the bytes it did not stop.
        *self.cursor.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Reserve `n` bytes and return how long the caller must wait first.
    pub fn reserve(&self, n: u64) -> Duration {
        let r = self.rate.load(Ordering::Relaxed);
        if r == 0 {
            return Duration::ZERO;
        }
        let dur = Duration::from_secs_f64(n as f64 / r as f64);
        let now = Instant::now();
        let mut g = self.cursor.lock().unwrap();
        // The cursor may lag the clock by up to `BURST_WINDOW`, and time a
        // caller spent asleep past its debt is credited back instead of lost.
        //
        // Clamping the cursor to `now` looked harmless and was not: tokio's
        // timers tick at 1 ms, so a 0.3 ms debt sleeps a whole tick, and with
        // the overshoot forgotten every 64 KiB read cost at least 1 ms whatever
        // the cap said. Measured: `--limit-rate 200M` delivered 78 MiB/s and
        // `400M` delivered 89 MiB/s, against curl at the cap. `BURST_WINDOW`
        // bounds what a credit can buy, so this shapes the rate rather than
        // opening a loophole in it.
        let floor = now.checked_sub(BURST_WINDOW).unwrap_or(now);
        let base = match *g {
            Some(t) if t > floor => t,
            _ => floor,
        };
        *g = Some(base + dur);
        base.saturating_duration_since(now)
    }
}

/// A rate cap as the byte loops see it: cheap to clone, absent by default.
///
/// The transfer path is generic over many call sites — the multi-connection
/// scheduler loop, the single-range retry helper, the chunked decoder — and most
/// of them have no cap. Wrapping the limiters in a type keeps every one of those
/// signatures honest about that while giving the places that DO shape bytes one
/// thing to call.
///
/// Two caps can apply at once: an aggregate one shared by everything the process
/// is fetching, and this transfer's own. Both are charged and both are waited
/// on, so the effective ceiling is whichever is lower at that instant — no rate
/// is ever computed once and frozen.
#[derive(Clone, Default)]
pub struct Pace {
    /// The cap shared with other transfers (a true aggregate), if any.
    shared: Option<Arc<RateLimiter>>,
    /// This transfer's own cap, if any.
    own: Option<Arc<RateLimiter>>,
}

impl Pace {
    /// No cap, and none can appear later: `wait` returns immediately and reads
    /// stay at full size.
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Shape against `limiter`, live.
    ///
    /// The limiter is held even when its rate is currently 0 (unlimited): the
    /// rate is read on every single read, so a cap switched ON mid-transfer —
    /// the GUI's "Use Speed Limiter" checkbox, `hydra_engine_set_max_bytes_per_second`
    /// — binds the transfer that is already running.
    ///
    /// It used to collapse an unlimited limiter to "no cap" here to save an
    /// atomic load per read. That froze the decision at the instant the transfer
    /// started: every download begun without a cap ignored the limiter for the
    /// rest of its life, and the checkbox did nothing but change a number on
    /// screen. The load costs a few nanoseconds against a read that just came
    /// off a socket; the collapse cost the feature.
    pub fn shared(limiter: Arc<RateLimiter>) -> Self {
        Self {
            shared: Some(limiter),
            own: None,
        }
    }

    /// Shape against an aggregate cap AND this transfer's own, both live.
    ///
    /// Neither subsumes the other: `shared` stays a true aggregate over every
    /// transfer holding it, while `own` binds this one alone. A transfer under
    /// both moves at the lower of the two without either having to be recomputed
    /// when the other changes.
    pub fn pair(shared: Arc<RateLimiter>, own: Arc<RateLimiter>) -> Self {
        Self {
            shared: Some(shared),
            own: Some(own),
        }
    }

    /// Every limiter this pace answers to, in charge order.
    fn limiters(&self) -> impl Iterator<Item = &Arc<RateLimiter>> {
        [&self.shared, &self.own].into_iter().flatten()
    }

    /// The binding rate right now, in bytes/sec; 0 when nothing binds.
    fn rate(&self) -> u64 {
        let mut cap = 0u64;
        for l in self.limiters() {
            let r = l.rate.load(Ordering::Relaxed);
            if r > 0 && (cap == 0 || r < cap) {
                cap = r;
            }
        }
        cap
    }

    /// Whether a cap binds AT THIS MOMENT. Not a property of the transfer: a
    /// pace that answers to a limiter can go from false to true and back while
    /// the transfer runs.
    pub fn is_limited(&self) -> bool {
        self.rate() > 0
    }

    /// Largest read that keeps one pause short.
    ///
    /// A 64 KiB read under a 1 KiB/s cap owes 64 seconds of sleep, which is
    /// longer than the transfer's own no-progress watchdog: shaping the rate
    /// would trip the stall detector and kill the connection it was shaping.
    /// Reading in slices worth about an eighth of a second keeps every pause well
    /// inside that deadline, at the cost of more syscalls on a path that is by
    /// definition not throughput-bound.
    pub fn read_size(&self, want: usize) -> usize {
        let r = self.rate();
        if r == 0 || want == 0 {
            return want;
        }
        // An eighth of a second's worth, with a 1 KiB floor so a very low
        // cap does not degenerate into one syscall per byte.
        //
        // The floor is applied BEFORE clamping against `want`, never as a
        // `clamp(1024, want)`: on the last read of a range `want` is
        // whatever is left, which is routinely under 1 KiB, and
        // `clamp(1024, 969)` panics on min > max. That panic killed the
        // connection mid-transfer — the caller saw a stall, retried, and
        // the measured throughput came out FIVE times below the cap that
        // was supposed to be the ceiling.
        let slice = (r / 8).max(1024) as usize;
        want.min(slice).max(1)
    }

    /// Reserve `n` bytes against every cap and sleep for the longest debt.
    ///
    /// Both buckets are charged even when only one of them makes the caller
    /// wait: skipping the cheaper one would let the aggregate drift once the
    /// tighter cap was lifted.
    pub async fn wait(&self, n: u64) {
        let mut owed = Duration::ZERO;
        for l in self.limiters() {
            owed = owed.max(l.reserve(n));
        }
        // A debt shorter than a timer tick is carried, not slept: the bytes are
        // already charged to the cursor, so the next reservation owes more, and
        // the sleep happens once the debt is long enough to be timed accurately.
        // Sleeping every sub-millisecond debt is what pinned a single connection
        // to one read per tick — see `RateLimiter::reserve`.
        if owed >= MIN_PAUSE {
            tokio::time::sleep(owed).await;
        }
    }
}

/// Parse a `--limit-rate` value: bare bytes, or with a k/K/m/M/g/G suffix.
/// Accepts standard rate-limit unit conventions.
pub fn parse_rate(s: &str) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (num, mult) = match t.chars().last().unwrap() {
        'k' | 'K' => (&t[..t.len() - 1], 1024u64),
        'm' | 'M' => (&t[..t.len() - 1], 1024 * 1024),
        'g' | 'G' => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    let v: f64 = num.trim().parse().ok()?;
    if v < 0.0 {
        return None;
    }
    Some((v * mult as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_both_forms() {
        assert_eq!(parse_retry_after("120", 0), Some(Duration::from_secs(120)));
        // 2015-10-21T07:28:00Z = 1445412480
        let d = parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", 1_445_412_000).unwrap();
        assert_eq!(
            d,
            Duration::from_secs(480),
            "HTTP-date form must be honoured"
        );
    }

    #[test]
    fn http_date_before_the_epoch_is_zero_not_the_far_future() {
        assert_eq!(parse_http_date("Mon, 01 Jan 1900 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Wed, 31 Dec 1969 23:59:59 GMT"), Some(0));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT"), Some(1));
        assert_eq!(
            parse_retry_after("Mon, 01 Jan 1900 00:00:00 GMT", 1_445_412_000),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn http_date_with_out_of_range_fields_is_rejected() {
        assert_eq!(parse_http_date("Sun, 00 Mar 2015 07:28:00 GMT"), None);
        assert_eq!(parse_http_date("Wed, 32 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 24:00:00 GMT"), None);
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:60:00 GMT"), None);
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:61 GMT"), None);
        assert_eq!(parse_http_date("Wed, 21 Oct 10000 07:28:00 GMT"), None);
        assert_eq!(
            parse_http_date("Wed, 21 Oct 18446744073709551615 07:28:00 GMT"),
            None
        );
    }

    #[test]
    fn retry_after_in_the_past_is_zero_not_negative() {
        let d = parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT", 2_000_000_000).unwrap();
        assert_eq!(d, Duration::ZERO, "a past date must not underflow");
    }

    #[test]
    fn retry_after_rejects_garbage() {
        assert!(parse_retry_after("soon", 0).is_none());
        assert!(parse_retry_after("", 0).is_none());
    }

    #[test]
    fn backoff_grows_and_stays_capped() {
        let base = Duration::from_millis(200);
        let cap = Duration::from_secs(10);
        let a = backoff_with_jitter(0, base, cap, 7);
        let b = backoff_with_jitter(4, base, cap, 7);
        assert!(b > a, "backoff must grow with attempt");
        for att in 0..20 {
            assert!(
                backoff_with_jitter(att, base, cap, att as u64 + 1) <= cap,
                "attempt {att} exceeded the cap"
            );
        }
    }

    #[test]
    fn backoff_jitter_differs_across_connections() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(30);
        let a = backoff_with_jitter(3, base, cap, 1);
        let b = backoff_with_jitter(3, base, cap, 2);
        assert_ne!(
            a, b,
            "identical backoff across connections re-synchronises the herd"
        );
    }

    #[test]
    fn host_ceiling_binds_per_host_and_globally() {
        let l = HostLimiter::new();
        assert!(l.try_acquire("a.example", 2, 3));
        assert!(l.try_acquire("a.example", 2, 3));
        assert!(
            !l.try_acquire("a.example", 2, 3),
            "per-host ceiling must bind"
        );
        assert!(l.try_acquire("b.example", 2, 3));
        assert!(
            !l.try_acquire("b.example", 2, 3),
            "global ceiling must bind"
        );
        l.release("a.example");
        assert!(
            l.try_acquire("b.example", 2, 3),
            "a release must free global room"
        );
    }

    #[test]
    fn conservative_mode_is_one_per_host() {
        let p = Politeness::conservative();
        assert_eq!(
            p.allow(32),
            1,
            "conservative mode must ignore an ambitious request"
        );
        assert_eq!(Politeness::default().allow(32), DEFAULT_PER_HOST);
        assert_eq!(Politeness::default().allow(0), 1, "at least one connection");
    }

    #[test]
    fn rate_limiter_paces_the_aggregate() {
        let rl = RateLimiter::new(1_000_000);
        // First reservation is immediate, subsequent ones queue behind it.
        assert_eq!(rl.reserve(500_000), Duration::ZERO);
        let w = rl.reserve(500_000);
        assert!(
            w >= Duration::from_millis(400) && w <= Duration::from_millis(600),
            "500 KB at 1 MB/s should wait ~0.5 s, waited {w:?}"
        );
    }

    /// A high cap must be reachable: at 200 MiB/s a 64 KiB read owes 0.3 ms,
    /// and sleeping that on a 1 ms timer — overshoot uncredited — capped a
    /// transfer at 78 MiB/s. 64 MiB in 64 KiB reads is 1024 waits: at one tick
    /// each that is over a second; at the cap it is 0.32 s.
    #[tokio::test]
    async fn sub_tick_debts_are_carried_so_a_high_cap_is_reachable() {
        let rate = 200u64 * 1024 * 1024;
        let pace = Pace::shared(Arc::new(RateLimiter::new(rate)));
        let total = 64u64 * 1024 * 1024;
        let read = 64u64 * 1024;
        let t0 = Instant::now();
        let mut sent = 0;
        while sent < total {
            pace.wait(read).await;
            sent += read;
        }
        let elapsed = t0.elapsed().as_secs_f64();
        let ideal = total as f64 / rate as f64;
        assert!(
            elapsed < ideal * 2.0,
            "{elapsed:.3}s for {ideal:.3}s of bytes: sub-tick debts are being slept one tick each"
        );
        // Still a cap: the `BURST_WINDOW` credit is the only slack allowed.
        assert!(
            elapsed > ideal * 0.7,
            "{elapsed:.3}s for {ideal:.3}s of bytes: the cap is not being applied"
        );
    }

    #[test]
    fn unlimited_rate_limiter_never_waits() {
        let rl = RateLimiter::unlimited();
        assert!(!rl.is_limited());
        for _ in 0..100 {
            assert_eq!(rl.reserve(1 << 20), Duration::ZERO);
        }
    }

    /// Changing the cap must not carry the old cap's queue over.
    ///
    /// Regression test: the cursor is an absolute time, computed from whatever
    /// rate was in force when the reservation was made. A transfer held at
    /// 1 KB/s builds a cursor tens of seconds ahead; switching the limiter OFF
    /// left that cursor in place, and the very next reservation — now nominally
    /// unlimited — still waited it out. Turning a limit off is supposed to
    /// release the transfer, not park it.
    #[test]
    fn changing_the_rate_forgets_the_old_rate_queue() {
        let rl = RateLimiter::new(1024);
        // Queue up a long debt at the low rate: 64 KiB at 1 KiB/s is a minute.
        assert_eq!(rl.reserve(64 * 1024), Duration::ZERO);
        let queued = rl.reserve(64 * 1024);
        assert!(queued >= Duration::from_secs(60), "queued {queued:?}");

        rl.set_rate(0);
        assert_eq!(
            rl.reserve(64 * 1024),
            Duration::ZERO,
            "an unlimited limiter must not serve out the old cap's schedule"
        );

        // Same in the other direction: raising the cap re-prices the queue.
        let rl = RateLimiter::new(1024);
        assert_eq!(rl.reserve(64 * 1024), Duration::ZERO);
        assert!(rl.reserve(64 * 1024) >= Duration::from_secs(60));
        rl.set_rate(1024 * 1024);
        assert_eq!(rl.reserve(1024), Duration::ZERO);

        // A no-op change keeps the queue: this is not a way to burst past a cap
        // by writing the same number over and over.
        let rl = RateLimiter::new(1024);
        assert_eq!(rl.reserve(4096), Duration::ZERO);
        let owed = rl.reserve(4096);
        rl.set_rate(1024);
        let still = rl.reserve(4096);
        assert!(
            still > owed,
            "re-setting the same rate must not clear the queue ({still:?} vs {owed:?})"
        );
    }

    #[test]
    fn rate_parsing_matches_standard_forms() {
        assert_eq!(parse_rate("1024"), Some(1024));
        assert_eq!(parse_rate("2k"), Some(2048));
        assert_eq!(parse_rate("2K"), Some(2048));
        assert_eq!(parse_rate("1.5M"), Some(1_572_864));
        assert_eq!(parse_rate("1G"), Some(1_073_741_824));
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(parse_rate("-5"), None);
    }

    /// `--max-total-connections` must bind the AGGREGATE, not just one host.
    ///
    /// Regression test: `allow()` clamped against `per_host` only, and nothing in
    /// the download path read `Politeness.total` at all — the one function that
    /// did (`HostLimiter::try_acquire`) was never called outside its own tests.
    /// `--max-total-connections 2 -x 8` reported and opened eight connections.
    #[test]
    fn split_honours_the_aggregate_ceiling() {
        let p = Politeness {
            per_host: 8,
            total: 2,
            ..Default::default()
        };
        // Eight requested across one source, but only two permitted in total.
        assert_eq!(p.split(8, 1).iter().sum::<usize>(), 2);
        // Splitting across mirrors must not multiply the total: eight connections
        // over two hosts is still eight sockets, which is what the ceiling means.
        let across = p.split(8, 2);
        assert_eq!(across.iter().sum::<usize>(), 2, "got {across:?}");
        // More sources than budget: the surplus get nothing rather than zero-slot
        // targets the scheduler would still have to reason about.
        let sparse = p.split(8, 5);
        assert_eq!(sparse.iter().sum::<usize>(), 2);
        assert_eq!(sparse.iter().filter(|&&n| n > 0).count(), 2);
    }

    /// The per-host ceiling still binds, and the remainder is spread evenly.
    #[test]
    fn split_respects_per_host_and_spreads_the_remainder() {
        let p = Politeness {
            per_host: 2,
            total: 32,
            ..Default::default()
        };
        // Per-host caps each entry at 2 even though the total would allow more.
        assert_eq!(p.split(8, 2), vec![2, 2]);

        let q = Politeness {
            per_host: 8,
            total: 32,
            ..Default::default()
        };
        // 5 over 2 is 3+2, not 4+1: the remainder goes to the leading sources one
        // at a time rather than piling onto the first.
        assert_eq!(q.split(5, 2), vec![3, 2]);
        assert_eq!(q.split(8, 4), vec![2, 2, 2, 2]);
        // A single connection is always available: no configuration can ask for a
        // transfer with nothing to fetch it.
        assert_eq!(q.split(1, 1), vec![1]);
        assert_eq!(q.split(0, 1), vec![1]);
        assert_eq!(q.split(4, 0), Vec::<usize>::new());
    }

    /// `read_size` must never panic, whatever the cap and whatever is left to read.
    ///
    /// Regression test: the slice was computed as `(rate / 8).clamp(1024, want)`,
    /// which panics with `min > max` whenever fewer than 1 024 bytes remain in the
    /// range — the last read of very nearly every range. The panic unwound the
    /// connection task mid-transfer, the caller saw a stall and retried, and the
    /// measured throughput came out about five times BELOW the cap that was meant
    /// to be a ceiling. A rate limiter that makes transfers slower than requested
    /// is as wrong as one that does nothing.
    #[test]
    fn read_size_never_panics_on_a_short_tail() {
        let pace = Pace::shared(Arc::new(RateLimiter::new(8 * 1024)));
        // The tail of a range: fewer bytes left than the 1 KiB floor.
        for want in [0usize, 1, 969, 1023, 1024, 1025, 64 * 1024] {
            let n = pace.read_size(want);
            assert!(
                n <= want,
                "read_size({want}) = {n} asked for more than remained"
            );
            if want > 0 {
                assert!(n >= 1, "read_size({want}) = 0 would make no progress");
            }
        }
    }

    /// An unlimited pace must not shorten reads: no cap, no syscall overhead.
    #[test]
    fn an_unlimited_pace_reads_at_full_size() {
        let pace = Pace::unlimited();
        assert!(!pace.is_limited());
        assert_eq!(pace.read_size(64 * 1024), 64 * 1024);
        // A limiter constructed with no cap collapses to the same thing.
        let from_unlimited = Pace::shared(Arc::new(RateLimiter::unlimited()));
        assert!(!from_unlimited.is_limited());
        assert_eq!(from_unlimited.read_size(64 * 1024), 64 * 1024);
    }

    /// A cap switched on AFTER the pace was built must bind.
    ///
    /// Regression test: `Pace::shared` collapsed a limiter whose rate was 0 to
    /// "no cap at all", so the decision was frozen when the transfer started.
    /// Every download begun without a limit ignored the limiter for the rest of
    /// its life — the GUI's "Use Speed Limiter" checkbox and
    /// `hydra_engine_set_max_bytes_per_second` both wrote a rate that nothing
    /// downstream would ever read again, and an 8 MB/s transfer stayed at
    /// 8 MB/s under a 100 KB/s cap.
    #[test]
    fn a_cap_set_after_the_pace_was_built_binds() {
        let limiter = Arc::new(RateLimiter::unlimited());
        let pace = Pace::shared(limiter.clone());
        assert!(!pace.is_limited(), "no cap yet");
        assert_eq!(pace.read_size(64 * 1024), 64 * 1024);

        limiter.set_rate(100 * 1024);
        assert!(
            pace.is_limited(),
            "the limiter was switched on mid-transfer"
        );
        assert!(
            pace.read_size(64 * 1024) <= 100 * 1024 / 8,
            "reads must shorten to the new cap's slice"
        );
        // And the bucket must actually owe time now.
        assert_eq!(limiter.reserve(100 * 1024), Duration::ZERO);
        assert!(limiter.reserve(100 * 1024) >= Duration::from_millis(800));

        // Switching it off again returns full-size reads.
        limiter.set_rate(0);
        assert!(!pace.is_limited());
        assert_eq!(pace.read_size(64 * 1024), 64 * 1024);
    }

    /// Under two caps the lower one binds, whichever it happens to be.
    #[test]
    fn a_paired_pace_obeys_the_lower_of_the_two() {
        let aggregate = Arc::new(RateLimiter::new(1024 * 1024));
        let own = Arc::new(RateLimiter::new(8 * 1024));
        let pace = Pace::pair(aggregate.clone(), own.clone());
        assert_eq!(
            pace.rate(),
            8 * 1024,
            "the job's own cap is the tighter one"
        );
        own.set_rate(4 * 1024 * 1024);
        assert_eq!(
            pace.rate(),
            1024 * 1024,
            "raising the job's cap leaves the aggregate binding"
        );
        // 0 on one side means "no cap from me", not "no cap at all".
        aggregate.set_rate(0);
        assert_eq!(pace.rate(), 4 * 1024 * 1024);
        own.set_rate(0);
        assert!(!pace.is_limited());
    }

    /// A capped pace reads in slices short enough that one pause stays brief.
    ///
    /// A 64 KiB read under a 1 KiB/s cap owes 64 seconds of sleep, which is longer
    /// than the transfer's own no-progress watchdog: shaping the rate would trip
    /// the stall detector and kill the connection it was shaping.
    #[test]
    fn a_capped_pace_reads_in_short_slices() {
        let pace = Pace::shared(Arc::new(RateLimiter::new(1024)));
        assert!(pace.is_limited());
        let n = pace.read_size(64 * 1024);
        assert!(
            n <= 8 * 1024,
            "a 1 KiB/s cap must not take a {n}-byte read: the pause it owes would \
             outlast the stall watchdog"
        );
    }
}

#[cfg(test)]
mod redirect_chain_tests {
    use super::*;

    /// The reported failure: a `307` whose `Location` is the URL just asked
    /// for. It cost eight round trips and reported "too many redirects", which
    /// named the budget instead of the loop.
    #[test]
    fn a_location_naming_the_request_is_a_loop_on_the_first_hop() {
        const URL: &str = "https://zh.z-lib.sk/dl/omZxxOYdnp";
        let mut chain = RedirectChain::new(URL);
        assert!(!chain.advance(URL), "a self-redirect must not be followed");
    }

    #[test]
    fn a_chain_that_goes_somewhere_is_followed() {
        let mut chain = RedirectChain::new("https://a.example/f");
        assert!(chain.advance("https://b.example/f"));
        assert!(chain.advance("https://c.example/f"));
    }

    /// A cycle longer than one hop is the same defect with more steps: the
    /// budget would absorb it silently until it ran out.
    #[test]
    fn a_cycle_is_caught_where_it_closes() {
        let mut chain = RedirectChain::new("https://a.example/f");
        assert!(chain.advance("https://b.example/f"));
        assert!(chain.advance("https://c.example/f"));
        assert!(!chain.advance("https://b.example/f"), "b was already asked");
    }

    /// The hop that matters most is the one a relative `Location` produces:
    /// `join_url` spells the port, the URL the user pasted does not, and a
    /// byte comparison would call those two different places.
    #[test]
    fn one_address_spelled_two_ways_is_one_address() {
        let mut chain = RedirectChain::new("https://Zh.Z-Lib.SK/dl/x");
        assert!(!chain.advance("https://zh.z-lib.sk:443/dl/x"));
        let mut plain = RedirectChain::new("http://h.example:80/f");
        assert!(!plain.advance("HTTP://H.EXAMPLE/f"));
    }

    /// Case folding stops at the authority: a server that distinguishes `/A`
    /// from `/a` is serving two objects, and calling them one would report a
    /// loop on a chain that has none.
    #[test]
    fn the_path_is_case_sensitive() {
        let mut chain = RedirectChain::new("https://h.example/A");
        assert!(chain.advance("https://h.example/a"));
    }

    /// A non-default port is part of the address, not noise to elide.
    #[test]
    fn a_non_default_port_distinguishes_two_addresses() {
        let mut chain = RedirectChain::new("https://h.example/f");
        assert!(chain.advance("https://h.example:9000/f"));
    }

    /// `https://h` and `https://h/` name the root either way.
    #[test]
    fn an_empty_path_is_the_root() {
        let mut chain = RedirectChain::new("https://h.example");
        assert!(!chain.advance("https://h.example/"));
    }

    /// An address this cannot parse must still compare against itself, or a
    /// loop through one would be invisible.
    #[test]
    fn an_unparsable_address_still_compares_against_itself() {
        let mut chain = RedirectChain::new("ftp://h.example/f");
        assert!(!chain.advance("ftp://h.example/f"));
        assert!(chain.advance("ftp://other.example/f"));
    }
}

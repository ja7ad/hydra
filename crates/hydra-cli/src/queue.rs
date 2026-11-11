//! The download queue: a pure state machine, no terminal and no I/O.
//!
//! Separated from the TUI on purpose. A queue manager tangled with its renderer
//! cannot be tested — you end up asserting on escape sequences instead of on
//! behaviour — and the interesting behaviour here is all in the state
//! transitions: what happens when a running job fails, whether a paused job
//! resumes where it stopped, whether the concurrency ceiling is respected while
//! jobs are being added and cancelled.
//!
//! Persisted as JSON so a queue survives the process. The sidecar mechanism
//! (`url::Sidecar`) already makes an individual transfer resumable; this makes the
//! *plan* resumable too.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum State {
    /// Waiting for a slot.
    Queued,
    /// Transferring now.
    Running,
    /// Stopped by the user; bytes on disk are kept.
    Paused,
    Done,
    /// Failed, with the attempt count so far.
    Failed,
    /// Removed by the user.
    Cancelled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Paused => "paused",
            State::Done => "done",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
        }
    }

    /// A terminal state needs no further scheduling.
    pub fn is_terminal(self) -> bool {
        matches!(self, State::Done | State::Cancelled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    /// PID of the process transferring this item, when one is.
    ///
    /// Needed to tell a BACKGROUNDED transfer from a dead one. A queue reloaded after a
    /// crash must demote `Running` items or they occupy a slot forever; a queue reloaded
    /// while a detached process is still working must NOT, or the same object gets
    /// fetched twice into the same file. The PID answers which case it is.
    #[serde(default)]
    pub owner_pid: Option<u32>,
    pub id: u64,
    pub urls: Vec<String>,
    pub output: PathBuf,
    pub state: State,
    /// Total size once probed.
    pub size: Option<u64>,
    pub done_bytes: u64,
    /// Bytes per second, most recent observation.
    pub rate: f64,
    pub attempts: u32,
    pub error: Option<String>,
    pub category: Option<String>,
    pub sha256: Option<String>,
}

impl Item {
    pub fn fraction(&self) -> Option<f64> {
        match self.size {
            Some(s) if s > 0 => Some((self.done_bytes as f64 / s as f64).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    pub fn name(&self) -> String {
        self.output
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "?".into())
    }
}

/// The queue itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queue {
    pub items: Vec<Item>,
    /// How many transfers may run at once.
    pub max_active: usize,
    /// Retries before a job is left Failed.
    pub max_attempts: u32,
    next_id: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            max_active: 2,
            max_attempts: 3,
            next_id: 1,
        }
    }
}

impl Queue {
    pub fn new(max_active: usize) -> Self {
        Self {
            max_active: max_active.max(1),
            ..Default::default()
        }
    }

    pub fn add(&mut self, urls: Vec<String>, output: PathBuf) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Item {
            owner_pid: None,
            id,
            urls,
            output,
            state: State::Queued,
            size: None,
            done_bytes: 0,
            rate: 0.0,
            attempts: 0,
            error: None,
            category: None,
            sha256: None,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Item> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn active(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.state == State::Running)
            .count()
    }

    /// IDs that should be started now, respecting `max_active`.
    ///
    /// Returned in insertion order so a queue behaves predictably: the thing added
    /// first starts first.
    pub fn to_start(&self) -> Vec<u64> {
        let free = self.max_active.saturating_sub(self.active());
        self.items
            .iter()
            .filter(|i| i.state == State::Queued)
            .take(free)
            .map(|i| i.id)
            .collect()
    }

    pub fn mark_running(&mut self, id: u64) {
        let me = std::process::id();
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.owner_pid = Some(me);
        }
        if let Some(i) = self.get_mut(id) {
            i.state = State::Running;
            i.attempts += 1;
            i.error = None;
        }
    }

    pub fn progress(&mut self, id: u64, done: u64, size: Option<u64>, rate: f64) {
        if let Some(i) = self.get_mut(id) {
            i.done_bytes = done;
            if size.is_some() {
                i.size = size;
            }
            i.rate = rate;
        }
    }

    pub fn finish(&mut self, id: u64, sha: Option<String>, category: Option<String>) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.owner_pid = None;
        }
        if let Some(i) = self.get_mut(id) {
            i.state = State::Done;
            i.rate = 0.0;
            i.sha256 = sha;
            i.category = category;
            if let Some(s) = i.size {
                i.done_bytes = s;
            }
        }
    }

    /// Report a failure. Re-queues while attempts remain, else leaves it Failed.
    ///
    /// Retrying forever is how a queue manager turns one broken URL into a denial
    /// of service against a mirror, so the attempt ceiling is not optional.
    pub fn fail(&mut self, id: u64, why: String) -> State {
        let max = self.max_attempts;
        match self.get_mut(id) {
            Some(i) => {
                i.rate = 0.0;
                i.error = Some(why);
                i.state = if i.attempts < max {
                    State::Queued
                } else {
                    State::Failed
                };
                i.state
            }
            None => State::Cancelled,
        }
    }

    /// Pause a running or queued item. Bytes on disk are kept for resume.
    pub fn pause(&mut self, id: u64) {
        if let Some(i) = self.get_mut(id) {
            if matches!(i.state, State::Running | State::Queued) {
                i.state = State::Paused;
                i.rate = 0.0;
            }
        }
    }

    /// Put a running item back in line because its session is ending, not
    /// because anyone asked it to stop: the next session starts it first,
    /// ahead of the items that were still waiting behind it. The interrupted
    /// run is not a failed attempt.
    pub fn interrupt(&mut self, id: u64) {
        if let Some(i) = self.get_mut(id) {
            if i.state == State::Running {
                i.state = State::Queued;
                i.rate = 0.0;
                i.owner_pid = None;
                i.attempts = i.attempts.saturating_sub(1);
            }
        }
    }

    /// Resume a paused or failed item, clearing its attempt count so an
    /// explicit user action is not throttled by earlier automatic retries.
    pub fn resume(&mut self, id: u64) {
        if let Some(i) = self.get_mut(id) {
            if matches!(i.state, State::Paused | State::Failed) {
                i.state = State::Queued;
                i.attempts = 0;
                i.error = None;
            }
        }
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            i.owner_pid = None;
        }
        if let Some(i) = self.get_mut(id) {
            i.state = State::Cancelled;
            i.rate = 0.0;
        }
    }

    /// Remove items that will not run again, returning how many were dropped.
    ///
    /// Failed items count: their retries are spent, and they are the ones a user
    /// most wants out of the list.
    pub fn clear_finished(&mut self) -> usize {
        let before = self.items.len();
        self.items
            .retain(|i| !(i.state.is_terminal() || i.state == State::Failed));
        before - self.items.len()
    }

    /// Move an item up or down in the queue order.
    pub fn reorder(&mut self, id: u64, delta: isize) {
        let Some(pos) = self.items.iter().position(|i| i.id == id) else {
            return;
        };
        let target = (pos as isize + delta).clamp(0, self.items.len() as isize - 1) as usize;
        if target != pos {
            let it = self.items.remove(pos);
            self.items.insert(target, it);
        }
    }

    /// Aggregate rate across running items.
    pub fn total_rate(&self) -> f64 {
        self.items
            .iter()
            .filter(|i| i.state == State::Running)
            .map(|i| i.rate)
            .sum()
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut c = (0, 0, 0, 0); // queued, running, done, failed
        for i in &self.items {
            match i.state {
                State::Queued => c.0 += 1,
                State::Running => c.1 += 1,
                State::Done => c.2 += 1,
                State::Failed => c.3 += 1,
                _ => {}
            }
        }
        c
    }

    /// True when nothing is left to do.
    pub fn is_idle(&self) -> bool {
        !self
            .items
            .iter()
            .any(|i| matches!(i.state, State::Queued | State::Running))
    }

    /// Default queue file location.
    pub fn default_path() -> PathBuf {
        let base = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .unwrap_or_else(|_| PathBuf::from("."));
        base.join("hydra").join("queue.json")
    }

    /// `Ok(None)` when there is no queue file yet. A file that exists and does
    /// not parse is an error: replacing it with an empty queue would silently
    /// discard every item, so it is moved aside and named.
    pub fn load(path: &std::path::Path) -> Result<Option<Self>, String> {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Ok(None);
        };
        let mut q: Queue = match serde_json::from_str(&raw) {
            Ok(q) => q,
            Err(e) => {
                let aside = path.with_extension(format!(
                    "json.corrupt-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                ));
                let _ = std::fs::rename(path, &aside);
                return Err(format!(
                    "{} is not a queue file ({e}); moved it to {} and stopped rather than \
                     start with an empty queue",
                    path.display(),
                    aside.display()
                ));
            }
        };
        // A job recorded as Running belonged to a process that is gone. Treating
        // it as still running would leave a permanent phantom occupying a slot.
        for i in &mut q.items {
            if i.state == State::Running {
                // Only demote when the owning process is really gone. A live PID means a
                // detached session is still transferring this item, and stealing it would
                // have two writers on one file.
                match i.owner_pid {
                    Some(pid) if pid_alive(pid) => {}
                    _ => {
                        i.state = State::Queued;
                        i.rate = 0.0;
                        i.owner_pid = None;
                    }
                }
            }
        }
        Ok(Some(q))
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }
}

/// A bounded ring of recent log lines, for the TUI's event pane.
/// Is a process with this id alive?
///
/// `kill(pid, 0)` is the portable existence probe: it runs the permission checks and
/// returns without delivering a signal. EPERM counts as alive — the process exists, it
/// simply is not ours. Declared directly rather than pulling in a crate for one call.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // PID 0 must be rejected before the syscall: `kill(0, sig)` addresses the caller's
    // whole PROCESS GROUP, not a process numbered zero, so it returns success and would
    // report a dead owner as alive. Verified against the syscall rather than assumed.
    if pid == 0 {
        return false;
    }
    let r = unsafe { kill(pid as i32, 0) };
    // EPERM (1) means the process exists but belongs to another user.
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    // The Win32 existence probe: open the process with the weakest right and ask
    // whether it is still running. Declared directly rather than pulling in a crate
    // for three calls, mirroring the unix `kill` block above.
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, pid: u32) -> isize;
        fn GetExitCodeProcess(process: isize, exit_code: *mut u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: i32 = 5;

    // PID 0 is the System Idle Process, never a user transfer; reject it up front so a
    // zeroed owner field reads as dead, matching the unix path.
    if pid == 0 {
        return false;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        // ACCESS_DENIED means the process exists but belongs to another user — alive,
        // the same carve-out as EPERM on unix. Any other failure (typically
        // ERROR_INVALID_PARAMETER for a recycled or never-existing PID) means dead.
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
    }
    // A handle can still be opened on an exited process while something holds it open,
    // so "openable" is not "alive": only STILL_ACTIVE confirms it is running.
    let mut code: u32 = 0;
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    unsafe { CloseHandle(handle) };
    ok != 0 && code == STILL_ACTIVE
}

#[cfg(not(any(unix, windows)))]
pub fn pid_alive(_pid: u32) -> bool {
    // Without a probe, assume alive: wrongly demoting a live transfer causes two writers
    // on one file, which is worse than a stale slot.
    true
}

pub struct EventLog {
    lines: VecDeque<String>,
    cap: usize,
}

impl EventLog {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            cap,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line.into());
    }

    pub fn recent(&self, n: usize) -> impl Iterator<Item = &String> {
        self.lines.iter().rev().take(n).rev()
    }

    /// Number of retained lines. Used by tests to assert the ring is bounded.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q3() -> Queue {
        let mut q = Queue::new(2);
        q.add(vec!["http://a/1".into()], "1.bin".into());
        q.add(vec!["http://a/2".into()], "2.bin".into());
        q.add(vec!["http://a/3".into()], "3.bin".into());
        q
    }

    #[test]
    fn concurrency_ceiling_is_respected() {
        let mut q = q3();
        let start = q.to_start();
        assert_eq!(start.len(), 2, "max_active is 2");
        for id in &start {
            q.mark_running(*id);
        }
        assert_eq!(q.active(), 2);
        assert!(q.to_start().is_empty(), "no slot free");
        q.finish(start[0], None, None);
        assert_eq!(
            q.to_start(),
            vec![3],
            "a freed slot admits the next queued item"
        );
    }

    #[test]
    fn queue_order_is_insertion_order() {
        let mut q = Queue::new(1);
        q.add(vec!["http://a/x".into()], "x.bin".into());
        q.add(vec!["http://a/y".into()], "y.bin".into());
        assert_eq!(q.to_start(), vec![1], "first added starts first");
    }

    #[test]
    fn failure_retries_until_the_ceiling_then_stops() {
        let mut q = Queue::new(1);
        q.max_attempts = 3;
        let id = q.add(vec!["http://a/x".into()], "x.bin".into());
        for expected in [State::Queued, State::Queued, State::Failed] {
            q.mark_running(id);
            assert_eq!(q.fail(id, "timeout".into()), expected);
        }
        // Retrying forever is how a queue manager DoSes a mirror.
        assert_eq!(q.get(id).unwrap().attempts, 3);
        assert_eq!(q.get(id).unwrap().state, State::Failed);
        assert!(
            q.to_start().is_empty(),
            "a failed item must not restart on its own"
        );
    }

    #[test]
    fn explicit_resume_clears_the_attempt_count() {
        let mut q = Queue::new(1);
        q.max_attempts = 2;
        let id = q.add(vec!["http://a/x".into()], "x.bin".into());
        q.mark_running(id);
        q.fail(id, "e".into());
        q.mark_running(id);
        q.fail(id, "e".into());
        assert_eq!(q.get(id).unwrap().state, State::Failed);
        q.resume(id);
        assert_eq!(q.get(id).unwrap().state, State::Queued);
        assert_eq!(
            q.get(id).unwrap().attempts,
            0,
            "a deliberate user action must not be throttled by earlier automatic retries"
        );
    }

    #[test]
    fn pause_keeps_progress_and_frees_a_slot() {
        let mut q = q3();
        let ids = q.to_start();
        for id in &ids {
            q.mark_running(*id);
        }
        q.progress(ids[0], 5000, Some(10_000), 1.2e6);
        q.pause(ids[0]);
        let it = q.get(ids[0]).unwrap();
        assert_eq!(it.state, State::Paused);
        assert_eq!(it.done_bytes, 5000, "paused bytes must survive for resume");
        assert_eq!(it.rate, 0.0, "a paused item is not moving");
        assert_eq!(q.active(), 1);
        assert_eq!(
            q.to_start(),
            vec![3],
            "pausing frees a slot for the next item"
        );
    }

    #[test]
    fn progress_and_fraction_track_the_transfer() {
        let mut q = Queue::new(1);
        let id = q.add(vec!["http://a/x".into()], "x.bin".into());
        assert_eq!(
            q.get(id).unwrap().fraction(),
            None,
            "unknown size has no fraction"
        );
        q.progress(id, 2500, Some(10_000), 1.0e6);
        assert_eq!(q.get(id).unwrap().fraction(), Some(0.25));
        // Overshoot is clamped rather than reporting 110%.
        q.progress(id, 11_000, Some(10_000), 1.0e6);
        assert_eq!(q.get(id).unwrap().fraction(), Some(1.0));
    }

    #[test]
    fn finish_completes_the_byte_count() {
        let mut q = Queue::new(1);
        let id = q.add(vec!["http://a/x".into()], "x.bin".into());
        q.progress(id, 9_999, Some(10_000), 1.0e6);
        q.finish(id, Some("abc".into()), Some("archive".into()));
        let it = q.get(id).unwrap();
        assert_eq!(it.state, State::Done);
        assert_eq!(it.done_bytes, 10_000, "a done item must not show 99.99%");
        assert_eq!(it.category.as_deref(), Some("archive"));
    }

    #[test]
    fn reorder_moves_within_bounds() {
        let mut q = q3();
        q.reorder(3, -2);
        assert_eq!(q.items[0].id, 3);
        // Clamped rather than panicking at the edges.
        q.reorder(3, -5);
        assert_eq!(q.items[0].id, 3);
        q.reorder(3, 99);
        assert_eq!(q.items.last().unwrap().id, 3);
    }

    #[test]
    fn clear_finished_removes_only_terminal_items() {
        let mut q = q3();
        q.mark_running(1);
        q.finish(1, None, None);
        q.cancel(2);
        q.mark_running(3);
        q.fail(3, "x".into());
        assert_eq!(
            q.clear_finished(),
            2,
            "done and cancelled go; a failure still awaiting retry stays"
        );
        assert_eq!(q.items.len(), 1);
        assert_eq!(q.items[0].id, 3);
    }

    #[test]
    fn clear_finished_removes_items_that_exhausted_their_retries() {
        let mut q = q3();
        q.max_attempts = 1;
        q.mark_running(1);
        assert_eq!(q.fail(1, "404".into()), State::Failed);
        assert_eq!(q.clear_finished(), 1);
        assert!(q.items.iter().all(|i| i.id != 1));
    }

    #[test]
    fn idle_only_when_nothing_is_pending() {
        let mut q = q3();
        assert!(!q.is_idle());
        for id in [1u64, 2, 3] {
            q.mark_running(id);
            q.finish(id, None, None);
        }
        assert!(q.is_idle());
    }

    #[test]
    fn counts_and_total_rate_aggregate() {
        let mut q = q3();
        q.mark_running(1);
        q.mark_running(2);
        q.progress(1, 10, Some(100), 1.0e6);
        q.progress(2, 10, Some(100), 2.5e6);
        let (queued, running, done, failed) = q.counts();
        assert_eq!((queued, running, done, failed), (1, 2, 0, 0));
        assert_eq!(q.total_rate(), 3.5e6);
        q.pause(1);
        assert_eq!(q.total_rate(), 2.5e6, "a paused item contributes nothing");
    }

    /// A queue file that does not parse used to be silently replaced by an
    /// empty queue, losing every item in it.
    #[test]
    fn a_corrupt_queue_file_is_moved_aside_and_reported_not_replaced() {
        let dir = std::env::temp_dir().join(format!("hydra_q_corrupt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        std::fs::write(&path, b"{ this is not json").unwrap();
        let e = Queue::load(&path).unwrap_err();
        assert!(e.contains("queue.json") && e.contains("corrupt"), "{e}");
        assert!(
            !path.exists(),
            "the corrupt file must be moved, not left to fail again"
        );
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt"))
            .collect();
        assert_eq!(aside.len(), 1, "the original bytes are kept beside it");
        assert!(
            Queue::load(&dir.join("absent.json")).unwrap().is_none(),
            "no file at all is simply a fresh start"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #396: quitting mid-transfer put the item on hold, so the next session
    /// skipped it and started the one queued behind it.
    #[test]
    fn a_quit_mid_transfer_resumes_that_item_first_in_the_next_session() {
        let dir = std::env::temp_dir().join(format!("hydra_q_quit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        let mut q = Queue::new(1);
        for n in 1..=5 {
            q.add(vec![format!("http://a/{n}")], format!("{n}.bin").into());
        }
        for id in [1, 2] {
            q.mark_running(id);
            q.finish(id, None, None);
        }
        assert_eq!(q.to_start(), vec![3]);
        q.mark_running(3);
        q.progress(3, 4096, Some(8192), 1.0e6);

        q.interrupt(3);
        q.save(&path).unwrap();
        let back = Queue::load(&path).unwrap().unwrap();

        let item = back.get(3).unwrap();
        assert_eq!(item.state, State::Queued);
        assert_eq!(item.done_bytes, 4096, "the bytes already held are kept");
        assert_eq!(item.attempts, 0, "an interrupted run is not a failure");
        assert_eq!(item.owner_pid, None);
        assert_eq!(back.to_start(), vec![3], "#3 resumes; #4 keeps waiting");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupt_leaves_a_user_pause_alone() {
        let mut q = q3();
        q.mark_running(1);
        q.pause(1);
        q.interrupt(1);
        assert_eq!(q.get(1).unwrap().state, State::Paused);
    }

    #[test]
    fn a_reloaded_queue_demotes_running_items_only_when_the_owner_is_dead() {
        // Two cases that must NOT be conflated. A crash leaves Running items whose owner
        // is gone: those must be demoted or they occupy a slot forever. Backgrounding
        // leaves Running items whose owner is alive: demoting those would start a second
        // transfer into the same file.
        let dir = std::env::temp_dir().join(format!("hydra_q_pid_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Owner alive (this process): the item stays running.
        let live = dir.join("live.json");
        let mut q = q3();
        q.mark_running(1);
        q.progress(1, 4096, Some(8192), 1.0e6);
        assert_eq!(q.get(1).unwrap().owner_pid, Some(std::process::id()));
        q.save(&live).unwrap();
        let back = Queue::load(&live).unwrap().unwrap();
        assert_eq!(
            back.get(1).unwrap().state,
            State::Running,
            "a backgrounded transfer whose process is alive must keep its slot"
        );
        assert_eq!(back.get(1).unwrap().done_bytes, 4096, "bytes are preserved");

        // Owner dead: PID 0 is never a live user process, so this stands in for a crash.
        let dead = dir.join("dead.json");
        let mut q2 = q3();
        q2.mark_running(1);
        q2.progress(1, 4096, Some(8192), 1.0e6);
        if let Some(i) = q2.items.iter_mut().find(|i| i.id == 1) {
            i.owner_pid = Some(0);
        }
        q2.save(&dead).unwrap();
        let back2 = Queue::load(&dead).unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            back2.get(1).unwrap().state,
            State::Queued,
            "a Running item with no live owner is a phantom and must be requeued"
        );
        assert_eq!(
            back2.get(1).unwrap().rate,
            0.0,
            "a stale rate must not persist"
        );
        assert!(back2.get(1).unwrap().owner_pid.is_none());
        assert_eq!(
            back2.get(1).unwrap().done_bytes,
            4096,
            "requeueing must not discard the bytes already fetched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pid_zero_is_not_alive_and_our_own_pid_is() {
        // The demotion rule rests entirely on this probe, so assert it directly.
        assert!(
            pid_alive(std::process::id()),
            "our own process must read as alive"
        );
        assert!(!pid_alive(0), "pid 0 is not a live user process");
    }

    #[test]
    fn event_log_is_bounded_and_ordered() {
        let mut l = EventLog::new(3);
        assert!(l.is_empty());
        for i in 0..5 {
            l.push(format!("line {i}"));
        }
        assert_eq!(l.len(), 3, "the ring must not grow without bound");
        let got: Vec<&String> = l.recent(10).collect();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "line 2", "oldest retained first");
        assert_eq!(got[2], "line 4", "newest last");
    }
}

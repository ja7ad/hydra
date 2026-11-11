// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The ABI test suite.
//!
//! These tests call the exported symbols exactly as a C program does: through
//! raw pointers, with `CString` arguments, checking `hydra_error_code_t`
//! returns, and freeing everything hydra hands back. Nothing here reaches into
//! the crate's internals, because a test that did would stop being evidence
//! that the *interface* works.
//!
//! What they are for, in order of importance:
//!
//! * the memory contract — every allocation released through its own `*_free`;
//! * the life cycle — create, start, pause, resume, cancel, complete;
//! * hostile input — NULL, invalid UTF-8, unknown enum values, bad ids;
//! * persistence — a job that survives an engine being destroyed.

mod support;

use hydra::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::time::{Duration, Instant};

use support::{make_body, serve, Behaviour};

// ------------------------------------------------------------------- helpers

/// An engine with defaults, plus whatever `tweak` changes.
struct Harness {
    engine: *mut hydra_engine_t,
    /// Kept alive because the configuration borrowed a pointer into it.
    _state_path: Option<CString>,
    dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe { hydra_engine_destroy(self.engine) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydra-ffi-test-{}-{}-{}",
        name,
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn harness(name: &str, persist: bool, tweak: impl FnOnce(&mut hydra_engine_config_t)) -> Harness {
    let dir = scratch(name);
    let state = persist
        .then(|| CString::new(dir.join("state.json").to_string_lossy().into_owned()).unwrap());
    let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        hydra_engine_config_init(
            &mut cfg,
            std::mem::size_of::<hydra_engine_config_t>() as u32,
        )
    };
    assert_eq!(rc, hydra_error_code_t::HYDRA_OK);
    if let Some(s) = &state {
        cfg.state_path = s.as_ptr();
    }
    tweak(&mut cfg);
    let engine = unsafe { hydra_engine_create(&cfg) };
    assert!(
        !engine.is_null(),
        "engine creation failed: {}",
        last_error()
    );
    Harness {
        engine,
        _state_path: state,
        dir,
    }
}

/// The message behind the calling thread's last failure.
fn last_error() -> String {
    let mut e: hydra_error_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { hydra_last_error(&mut e) };
    if rc != hydra_error_code_t::HYDRA_OK || e.message.data.is_null() {
        return "(no detail)".into();
    }
    let s = unsafe { CStr::from_ptr(e.message.data) }
        .to_string_lossy()
        .into_owned();
    unsafe { hydra_error_free(&mut e) };
    s
}

/// Create a job from one URL and a destination, with `tweak` applied.
fn make_job(
    h: &Harness,
    url: &str,
    out: &std::path::Path,
    tweak: impl FnOnce(&mut hydra_job_config_t),
) -> hydra_job_id_t {
    let url_c = CString::new(url).unwrap();
    let urls: [*const c_char; 1] = [url_c.as_ptr()];
    let out_c = CString::new(out.to_string_lossy().into_owned()).unwrap();

    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32)
    };
    assert_eq!(rc, hydra_error_code_t::HYDRA_OK);
    cfg.urls = urls.as_ptr();
    cfg.url_count = 1;
    cfg.output_path = out_c.as_ptr();
    tweak(&mut cfg);

    let mut id: hydra_job_id_t = 0;
    let rc = unsafe { hydra_job_create(h.engine, &cfg, &mut id) };
    assert_eq!(
        rc,
        hydra_error_code_t::HYDRA_OK,
        "job creation failed: {}",
        last_error()
    );
    assert_ne!(id, 0, "0 is never a valid job id");
    id
}

/// Drain events until the job reaches a terminal state, or the deadline passes.
///
/// Returns the terminal event. Driving the tests off the queue rather than off
/// polled state is deliberate: the queue is the interface an application
/// actually uses, so a bug that only shows up there is one these tests should
/// see.
fn is_terminal_event(kind: hydra_event_type_t) -> bool {
    matches!(
        kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED
            | hydra_event_type_t::HYDRA_EVENT_FAILED
            | hydra_event_type_t::HYDRA_EVENT_CANCELLED
    )
}

/// Drain until every job in `jobs` has produced a terminal event.
///
/// Collecting them all rather than one is not fussiness: the queue has a single
/// consumer, so a loop that waits for job A and discards everything else throws
/// away job B's completion, and the next loop then waits for an event that has
/// already been delivered. That mistake cost an afternoon here, and it is
/// exactly the mistake a real application would make, so the helper models the
/// right shape.
fn await_all_terminal(
    h: &Harness,
    jobs: &[hydra_job_id_t],
    within: Duration,
) -> std::collections::HashMap<hydra_job_id_t, hydra_event_t> {
    let mut out = std::collections::HashMap::new();
    let deadline = Instant::now() + within;
    while Instant::now() < deadline && out.len() < jobs.len() {
        let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
        let left = deadline.saturating_duration_since(Instant::now());
        let rc = unsafe { hydra_event_wait(h.engine, left.as_millis().min(500) as u32, &mut ev) };
        if rc != hydra_error_code_t::HYDRA_OK {
            continue;
        }
        if jobs.contains(&ev.job_id) && is_terminal_event(ev.kind) {
            out.insert(ev.job_id, ev);
        }
    }
    assert_eq!(
        out.len(),
        jobs.len(),
        "only {:?} of {jobs:?} reached a terminal state within {within:?}",
        out.keys().collect::<Vec<_>>()
    );
    out
}

fn await_terminal(h: &Harness, job: hydra_job_id_t, within: Duration) -> hydra_event_t {
    await_all_terminal(h, &[job], within)
        .remove(&job)
        .expect("await_all_terminal asserts on absence")
}

/// Wait for a job to reach `want`, polling state rather than the queue.
fn await_state(h: &Harness, job: hydra_job_id_t, want: hydra_job_state_t, within: Duration) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let mut st = hydra_job_state_t::HYDRA_JOB_CREATED;
        let rc = unsafe { hydra_job_get_state(h.engine, job, &mut st) };
        assert_eq!(rc, hydra_error_code_t::HYDRA_OK);
        if st == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut st = hydra_job_state_t::HYDRA_JOB_CREATED;
    unsafe { hydra_job_get_state(h.engine, job, &mut st) };
    panic!("job {job} is in state {st:?}, expected {want:?}");
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

// ------------------------------------------------------------------- the tests

#[test]
fn abi_version_and_strings_are_available_without_an_engine() {
    assert_eq!(hydra_ffi_abi_version(), HYDRA_FFI_ABI_VERSION);
    let v = unsafe { CStr::from_ptr(hydra_ffi_version_string()) }
        .to_str()
        .unwrap();
    assert!(!v.is_empty(), "version string must not be empty");
    // Static storage: no free, and callable any number of times.
    for code in 0..25u32 {
        let n = unsafe { CStr::from_ptr(hydra_error_name(code)) }
            .to_str()
            .unwrap();
        assert!(n.starts_with("HYDRA_"), "{code} rendered as {n:?}");
    }
}

#[test]
fn config_init_stamps_size_and_version() {
    let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
    let n = std::mem::size_of::<hydra_engine_config_t>() as u32;
    assert_eq!(
        unsafe { hydra_engine_config_init(&mut cfg, n) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(cfg.size, n);
    assert_eq!(cfg.version, HYDRA_ENGINE_CONFIG_VERSION);
    assert!(cfg.max_jobs > 0 && cfg.max_connections > 0);
    assert_eq!(cfg.adaptive_concurrency, 1, "adaptive is the default");

    let mut jcfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    let n = std::mem::size_of::<hydra_job_config_t>() as u32;
    assert_eq!(
        unsafe { hydra_job_config_init(&mut jcfg, n) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(jcfg.size, n);
    assert_eq!(jcfg.resume, 1, "resume is the default");
    assert_eq!(jcfg.auto_start, 0, "creating a job must not start it");
}

#[test]
fn null_and_invalid_arguments_are_refused_rather_than_crashing() {
    use hydra_error_code_t as E;
    assert!(unsafe { hydra_engine_create(ptr::null()) }.is_null());
    assert_eq!(
        unsafe { hydra_engine_config_init(ptr::null_mut(), 8) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    // A destroyed-or-foreign handle must be rejected, not dereferenced.
    let mut junk = [0u8; 64];
    let fake = junk.as_mut_ptr() as *mut hydra_engine_t;
    assert_eq!(
        unsafe { hydra_engine_shutdown(fake, 0) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { hydra_job_start(ptr::null_mut(), 1) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    // Freeing nothing is defined behaviour, so a binding can free
    // unconditionally.
    unsafe { hydra_string_free(hydra_string_t::null()) };
    unsafe { hydra_error_free(ptr::null_mut()) };
    unsafe { hydra_job_snapshot_free(ptr::null_mut()) };
    unsafe { hydra_source_array_free(ptr::null_mut()) };
    unsafe { hydra_job_id_array_free(ptr::null_mut()) };
    unsafe { hydra_engine_destroy(ptr::null_mut()) };

    let h = harness("nulls", false, |_| {});
    assert_eq!(
        unsafe { hydra_job_start(h.engine, 12345) },
        E::HYDRA_ERR_NOT_FOUND
    );
    assert_eq!(
        unsafe { hydra_job_create(h.engine, ptr::null(), ptr::null_mut()) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    // An out-of-range enum is a refusal, never a silent fallback to a default.
    let dir = scratch("nulls-out");
    let id = make_job(&h, "http://127.0.0.1:1/x", &dir.join("f"), |_| {});
    assert_eq!(
        unsafe { hydra_job_cancel(h.engine, id, 99) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { hydra_engine_set_log_callback(h.engine, None, ptr::null_mut(), 99) },
        E::HYDRA_ERR_INVALID_ARGUMENT
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_bad_url_is_refused_at_creation_not_at_transfer_time() {
    let h = harness("badurl", false, |_| {});
    let dir = scratch("badurl-out");
    let out = CString::new(dir.join("f").to_string_lossy().into_owned()).unwrap();
    for bad in ["gopher://example.com/x", "not a url", ""] {
        let u = CString::new(bad).unwrap();
        let urls: [*const c_char; 1] = [u.as_ptr()];
        let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
        unsafe {
            hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32)
        };
        cfg.urls = urls.as_ptr();
        cfg.url_count = 1;
        cfg.output_path = out.as_ptr();
        let mut id = 0;
        let rc = unsafe { hydra_job_create(h.engine, &cfg, &mut id) };
        assert_eq!(
            rc,
            hydra_error_code_t::HYDRA_ERR_INVALID_URL,
            "{bad:?} should not have been accepted"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_ranged_download_completes_and_the_bytes_are_right() {
    let body = make_body(3 * 1024 * 1024 + 777);
    let origin = serve(body.clone(), Behaviour::default());
    let h = harness("ranged", false, |c| {
        c.max_connections = 4;
        // Fixed rather than adaptive, so the test exercises the multi-connection
        // scheduler path rather than settling on one stream.
        c.adaptive_concurrency = 0;
        c.progress_interval_ms = 20;
    });
    let out = h.dir.join("object.bin");
    let id = make_job(&h, &origin.url("/object.bin"), &out, |c| {
        c.adaptive = 0;
        c.max_connections = 4;
    });

    assert_eq!(
        unsafe { hydra_job_start(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "terminal event was {:?} ({}): {}",
        ev.kind,
        unsafe { CStr::from_ptr(hydra_error_name(ev.error as u32)) }.to_string_lossy(),
        last_error()
    );
    assert_eq!(ev.state, hydra_job_state_t::HYDRA_JOB_COMPLETED);
    assert_eq!(ev.progress.bytes_downloaded, body.len() as u64);
    assert_eq!(ev.progress.total_bytes, body.len() as u64);

    let got = std::fs::read(&out).expect("output file");
    assert_eq!(got.len(), body.len(), "wrong length");
    assert_eq!(
        sha256(&got),
        sha256(&body),
        "content differs from the origin"
    );

    // The snapshot is owned by the caller and must describe the finished job.
    let mut snap: hydra_job_snapshot_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_job_get_snapshot(h.engine, id, &mut snap) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(snap.state, hydra_job_state_t::HYDRA_JOB_COMPLETED);
    let name = unsafe { CStr::from_ptr(snap.file_name.data) }.to_string_lossy();
    assert_eq!(name, "object.bin");
    assert!(snap.finished_at_ms >= snap.created_at_ms);
    unsafe { hydra_job_snapshot_free(&mut snap) };
    // Freeing twice must be harmless, because a binding's destructor may run
    // after an explicit close.
    unsafe { hydra_job_snapshot_free(&mut snap) };

    let mut m: hydra_metrics_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_engine_get_metrics(h.engine, &mut m) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(m.jobs_completed, 1);
    assert!(m.bytes_written >= body.len() as u64);
}

#[test]
fn a_server_without_ranges_falls_back_to_one_stream() {
    let body = make_body(256 * 1024);
    let origin = serve(
        body.clone(),
        Behaviour {
            ranges: false,
            validator: false,
            ..Behaviour::default()
        },
    );
    let h = harness("noranges", false, |c| c.max_connections = 8);
    let out = h.dir.join("plain.bin");
    let id = make_job(&h, &origin.url("/plain.bin"), &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(std::fs::read(&out).unwrap(), body);
}

#[test]
fn a_matching_checksum_passes_and_a_wrong_one_fails_as_checksum() {
    let body = make_body(128 * 1024);
    let origin = serve(body.clone(), Behaviour::default());
    let h = harness("checksum", false, |_| {});

    let want = sha256(&body);
    let good = h.dir.join("good.bin");
    let id = make_job(&h, &origin.url("/o"), &good, |c| {
        c.checksum = hydra_checksum_t {
            algorithm: hydra_checksum_algorithm_t::HYDRA_CHECKSUM_SHA256 as u32,
            reserved: 0,
            digest: want.as_ptr(),
            digest_len: want.len(),
        };
    });
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_COMPLETED);

    let mut wrong = want;
    wrong[0] ^= 0xff;
    let bad = h.dir.join("bad.bin");
    let id2 = make_job(&h, &origin.url("/o"), &bad, |c| {
        c.checksum = hydra_checksum_t {
            algorithm: hydra_checksum_algorithm_t::HYDRA_CHECKSUM_SHA256 as u32,
            reserved: 0,
            digest: wrong.as_ptr(),
            digest_len: wrong.len(),
        };
    });
    unsafe { hydra_job_start(h.engine, id2) };
    let ev = await_terminal(&h, id2, Duration::from_secs(60));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_FAILED);
    assert_eq!(
        ev.error,
        hydra_error_code_t::HYDRA_ERR_CHECKSUM,
        "a mismatch must be reported as a checksum failure, not as I/O"
    );

    // A digest whose length does not match the algorithm is a caller mistake
    // and is refused at creation.
    let short = [0u8; 4];
    let url = CString::new(origin.url("/o")).unwrap();
    let urls: [*const c_char; 1] = [url.as_ptr()];
    let out = CString::new(h.dir.join("x").to_string_lossy().into_owned()).unwrap();
    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32) };
    cfg.urls = urls.as_ptr();
    cfg.url_count = 1;
    cfg.output_path = out.as_ptr();
    cfg.checksum = hydra_checksum_t {
        algorithm: hydra_checksum_algorithm_t::HYDRA_CHECKSUM_SHA256 as u32,
        reserved: 0,
        digest: short.as_ptr(),
        digest_len: short.len(),
    };
    let mut id3 = 0;
    assert_eq!(
        unsafe { hydra_job_create(h.engine, &cfg, &mut id3) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn pause_preserves_progress_and_resume_finishes_the_object() {
    let body = make_body(2 * 1024 * 1024);
    // Slow enough that a pause lands mid-transfer rather than racing the end.
    let origin = serve(
        body.clone(),
        Behaviour {
            delay_ms: 25,
            chunk: 32 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("pause", false, |c| {
        c.max_connections = 2;
        c.adaptive_concurrency = 0;
        c.progress_interval_ms = 20;
    });
    let out = h.dir.join("slow.bin");
    let id = make_job(&h, &origin.url("/slow.bin"), &out, |c| {
        c.adaptive = 0;
        c.max_connections = 2;
    });
    unsafe { hydra_job_start(h.engine, id) };

    // Wait for real progress, then pause.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut moved = 0u64;
    while Instant::now() < deadline {
        let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
        unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
        if p.bytes_downloaded > 64 * 1024 {
            moved = p.bytes_downloaded;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(moved > 0, "transfer never started moving");

    assert_eq!(
        unsafe { hydra_job_pause(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_PAUSED,
        Duration::from_secs(20),
    );

    let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
    let held = p.bytes_downloaded;
    assert!(
        held > 0,
        "a pause must not discard what was already fetched"
    );
    assert!(
        held < body.len() as u64,
        "the test paused too late to prove anything"
    );

    // Pausing something that is not running is a state error, not a crash.
    assert_eq!(
        unsafe { hydra_job_pause(h.engine, id) },
        hydra_error_code_t::HYDRA_ERR_INVALID_STATE
    );

    assert_eq!(
        unsafe { hydra_job_resume(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(120));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    let got = std::fs::read(&out).unwrap();
    assert_eq!(
        sha256(&got),
        sha256(&body),
        "a resumed transfer must reassemble the same object"
    );
}

#[test]
fn cancel_can_keep_or_remove_the_partial_file() {
    let body = make_body(1024 * 1024);
    let origin = serve(
        body.clone(),
        Behaviour {
            delay_ms: 25,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("cancel", false, |c| {
        c.max_connections = 1;
        c.adaptive_concurrency = 0;
    });

    let keep = h.dir.join("keep.bin");
    let a = make_job(&h, &origin.url("/a"), &keep, |c| c.adaptive = 0);
    unsafe { hydra_job_start(h.engine, a) };
    await_state(
        &h,
        a,
        hydra_job_state_t::HYDRA_JOB_DOWNLOADING,
        Duration::from_secs(20),
    );
    assert_eq!(
        unsafe {
            hydra_job_cancel(
                h.engine,
                a,
                hydra_cancel_mode_t::HYDRA_CANCEL_KEEP_PARTIAL as u32,
            )
        },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, a, Duration::from_secs(30));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_CANCELLED);
    assert!(keep.exists(), "KEEP_PARTIAL must leave the file behind");

    let gone = h.dir.join("gone.bin");
    let b = make_job(&h, &origin.url("/b"), &gone, |c| c.adaptive = 0);
    unsafe { hydra_job_start(h.engine, b) };
    await_state(
        &h,
        b,
        hydra_job_state_t::HYDRA_JOB_DOWNLOADING,
        Duration::from_secs(20),
    );
    assert_eq!(
        unsafe {
            hydra_job_cancel(
                h.engine,
                b,
                hydra_cancel_mode_t::HYDRA_CANCEL_REMOVE_PARTIAL as u32,
            )
        },
        hydra_error_code_t::HYDRA_OK
    );
    await_terminal(&h, b, Duration::from_secs(30));
    assert!(!gone.exists(), "REMOVE_PARTIAL must delete the file");

    // A terminal job cannot be cancelled again, and cannot be started again
    // either — but it can be forgotten.
    assert_eq!(
        unsafe { hydra_job_cancel(h.engine, b, 0) },
        hydra_error_code_t::HYDRA_ERR_INVALID_STATE
    );
    assert_eq!(
        unsafe { hydra_job_remove(h.engine, b) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(
        unsafe { hydra_job_get_state(h.engine, b, &mut hydra_job_state_t::HYDRA_JOB_CREATED) },
        hydra_error_code_t::HYDRA_ERR_NOT_FOUND
    );
}

#[test]
fn an_unreachable_origin_fails_with_a_transport_code() {
    let h = harness("unreachable", false, |c| c.max_retries = 1);
    let out = h.dir.join("never.bin");
    // Port 1 on loopback: nothing listens, and the refusal is immediate.
    let id = make_job(&h, "http://127.0.0.1:1/never", &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_FAILED);
    assert!(
        ev.error == hydra_error_code_t::HYDRA_ERR_CONNECTION
            || ev.error == hydra_error_code_t::HYDRA_ERR_NETWORK,
        "unexpected code {}",
        unsafe { CStr::from_ptr(hydra_error_name(ev.error as u32)) }.to_string_lossy()
    );

    let mut snap: hydra_job_snapshot_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_get_snapshot(h.engine, id, &mut snap) };
    let msg = unsafe { CStr::from_ptr(snap.error_message.data) }.to_string_lossy();
    assert!(!msg.is_empty(), "a failed job must carry a reason");
    unsafe { hydra_job_snapshot_free(&mut snap) };
}

#[test]
fn a_file_served_only_to_browsers_completes_through_the_abi() {
    let body = make_body(256 * 1024 + 5);
    let origin = serve(
        body.clone(),
        Behaviour {
            admit_only: Some("Chrome/".into()),
            ..Behaviour::default()
        },
    );
    let h = harness("gated", false, |c| c.max_retries = 1);
    let out = h.dir.join("gated.bin");
    let id = make_job(&h, &origin.url("/d/token/gated.bin"), &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };

    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(
        sha256(&std::fs::read(&out).expect("output file")),
        sha256(&body)
    );
}

#[test]
fn an_http_error_status_is_reported_with_the_status_attached() {
    let origin = serve(
        make_body(16),
        Behaviour {
            force_status: Some(404),
            ..Behaviour::default()
        },
    );
    let h = harness("status", false, |c| c.max_retries = 1);
    let out = h.dir.join("missing.bin");
    let id = make_job(&h, &origin.url("/missing"), &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_FAILED);
    assert_eq!(
        ev.http_status, 404,
        "the status belongs in the event, not only in the message"
    );
}

#[test]
fn state_survives_the_engine_that_created_it() {
    let body = make_body(1024 * 1024);
    let origin = serve(
        body.clone(),
        Behaviour {
            delay_ms: 25,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let dir = scratch("persist");
    let state = CString::new(dir.join("state.json").to_string_lossy().into_owned()).unwrap();
    let out = dir.join("resumable.bin");
    let url = origin.url("/resumable.bin");

    // ---- first engine: start, get some bytes, shut down --------------------
    let held = {
        let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
        unsafe {
            hydra_engine_config_init(
                &mut cfg,
                std::mem::size_of::<hydra_engine_config_t>() as u32,
            )
        };
        cfg.state_path = state.as_ptr();
        cfg.max_connections = 1;
        cfg.adaptive_concurrency = 0;
        let engine = unsafe { hydra_engine_create(&cfg) };
        assert!(!engine.is_null());
        let h = Harness {
            engine,
            _state_path: None,
            // Owned by this test, not by the harness: the second engine needs it.
            dir: std::env::temp_dir().join("hydra-ffi-unused"),
        };
        let id = make_job(&h, &url, &out, |c| c.adaptive = 0);
        unsafe { hydra_job_start(h.engine, id) };
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut moved = 0;
        while Instant::now() < deadline {
            let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
            unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
            if p.bytes_downloaded > 64 * 1024 {
                moved = p.bytes_downloaded;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(moved > 0, "nothing was fetched before the shutdown");
        assert_eq!(
            unsafe { hydra_engine_shutdown(h.engine, 5000) },
            hydra_error_code_t::HYDRA_OK
        );
        moved
        // `h` drops here, destroying the engine.
    };

    assert!(
        dir.join("state.json").exists(),
        "shutdown must write the state file"
    );

    // ---- second engine: restore and finish --------------------------------
    let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
    unsafe {
        hydra_engine_config_init(
            &mut cfg,
            std::mem::size_of::<hydra_engine_config_t>() as u32,
        )
    };
    cfg.state_path = state.as_ptr();
    cfg.max_connections = 1;
    cfg.adaptive_concurrency = 0;
    let engine = unsafe { hydra_engine_create(&cfg) };
    assert!(!engine.is_null());
    let h = Harness {
        engine,
        _state_path: None,
        dir: dir.clone(),
    };

    let mut restored: usize = 0;
    assert_eq!(
        unsafe { hydra_engine_restore(h.engine, &mut restored) },
        hydra_error_code_t::HYDRA_OK,
        "{}",
        last_error()
    );
    assert_eq!(restored, 1, "the job must come back");

    let mut ids: hydra_job_id_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_engine_list_jobs(h.engine, &mut ids) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(ids.len, 1);
    let id = unsafe { *ids.items };
    unsafe { hydra_job_id_array_free(&mut ids) };

    let mut st = hydra_job_state_t::HYDRA_JOB_CREATED;
    unsafe { hydra_job_get_state(h.engine, id, &mut st) };
    assert_eq!(
        st,
        hydra_job_state_t::HYDRA_JOB_PAUSED,
        "a restored job must not start itself"
    );
    let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
    // At least what was reported before the stop, because the transfer's
    // final range map is recorded on its way out rather than at the last
    // progress tick — and never the whole object, or the resume proves nothing.
    assert!(
        p.bytes_downloaded >= held && p.bytes_downloaded < body.len() as u64,
        "the restored range map should describe what was fetched \
         (restored {}, had {held} of {})",
        p.bytes_downloaded,
        body.len()
    );

    assert_eq!(
        unsafe { hydra_job_resume(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(180));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(
        sha256(&std::fs::read(&out).unwrap()),
        sha256(&body),
        "an object resumed across a process boundary must still be correct"
    );
}

/// Regression: `hydra_engine_destroy` used to reach its graceful stop by calling
/// the exported `hydra_engine_shutdown` on a handle it had already poisoned, so
/// the call rejected its own handle and did nothing. Jobs were not stopped and
/// no state was written — and because every other test called shutdown
/// explicitly, nothing noticed.
#[test]
fn destroying_without_an_explicit_shutdown_still_persists_state() {
    let dir = scratch("destroy-persists");
    let state_path = dir.join("state.json");
    let state = CString::new(state_path.to_string_lossy().into_owned()).unwrap();
    {
        let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
        unsafe {
            hydra_engine_config_init(
                &mut cfg,
                std::mem::size_of::<hydra_engine_config_t>() as u32,
            )
        };
        cfg.state_path = state.as_ptr();
        let engine = unsafe { hydra_engine_create(&cfg) };
        assert!(!engine.is_null());
        let h = Harness {
            engine,
            _state_path: None,
            dir: std::env::temp_dir().join("hydra-ffi-unused"),
        };
        let id = make_job(&h, "http://127.0.0.1:1/x", &dir.join("f.bin"), |_| {});
        assert_ne!(id, 0);
        // Deliberately NO hydra_engine_shutdown here: destroy alone must be a
        // correct, complete shutdown.
    }
    assert!(
        state_path.exists(),
        "destroy must write the state file even when shutdown was not called"
    );
    let body = std::fs::read_to_string(&state_path).unwrap();
    assert!(
        body.contains("127.0.0.1"),
        "the job was not recorded: {body}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn credentials_are_never_written_to_the_state_file() {
    let h = harness("secrets", true, |_| {});
    let out = h.dir.join("secret.bin");
    let user = CString::new("alice").unwrap();
    let pass = CString::new("hunter2-do-not-persist").unwrap();
    let name = CString::new("Authorization").unwrap();
    let value = CString::new("Bearer tok-do-not-persist").unwrap();
    let headers = [hydra_header_t {
        name: name.as_ptr(),
        value: value.as_ptr(),
    }];
    let id = make_job(&h, "http://127.0.0.1:1/x", &out, |c| {
        c.username = user.as_ptr();
        c.password = pass.as_ptr();
        c.headers = headers.as_ptr();
        c.header_count = 1;
    });
    assert_ne!(id, 0);
    assert_eq!(
        unsafe { hydra_engine_snapshot(h.engine) },
        hydra_error_code_t::HYDRA_OK,
        "{}",
        last_error()
    );
    let written = std::fs::read_to_string(h.dir.join("state.json")).unwrap();
    assert!(
        !written.contains("hunter2-do-not-persist"),
        "a password reached the state file"
    );
    assert!(
        !written.contains("tok-do-not-persist"),
        "an Authorization header reached the state file"
    );
    assert!(
        written.contains("withheld_headers") && written.contains("Authorization"),
        "the state file should record that a header was withheld: {written}"
    );
    // The username is not a secret and is kept, so a restored job can be
    // re-armed with just the password.
    assert!(written.contains("alice"));
}

/// Userinfo in a URL is a perfectly ordinary way to leak a password. It must be
/// stripped at creation, so that nothing downstream — the snapshot, the state
/// file, an error message naming the source — ever had it to leak.
#[test]
fn url_userinfo_never_reaches_a_snapshot_or_the_state_file() {
    let h = harness("userinfo", true, |_| {});
    let out = h.dir.join("secret.bin");
    let id = make_job(
        &h,
        "ftp://bob:hunter2-do-not-leak@files.invalid/pub/x.tar",
        &out,
        |_| {},
    );

    let mut snap: hydra_job_snapshot_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_job_get_snapshot(h.engine, id, &mut snap) },
        hydra_error_code_t::HYDRA_OK
    );
    let url = unsafe { CStr::from_ptr(snap.url.data) }
        .to_string_lossy()
        .into_owned();
    unsafe { hydra_job_snapshot_free(&mut snap) };
    assert!(
        !url.contains("hunter2-do-not-leak") && !url.contains("bob"),
        "the snapshot url leaked credentials: {url}"
    );
    assert_eq!(url, "ftp://files.invalid/pub/x.tar");

    assert_eq!(
        unsafe { hydra_engine_snapshot(h.engine) },
        hydra_error_code_t::HYDRA_OK
    );
    let written = std::fs::read_to_string(h.dir.join("state.json")).unwrap();
    assert!(
        !written.contains("hunter2-do-not-leak"),
        "a URL password reached the state file"
    );
    // The user half is not a secret and is kept, so a restored job needs only
    // the password put back.
    assert!(
        written.contains("bob"),
        "the username should survive: {written}"
    );
}

/// The rule documented on `hydra_job_start`: what "start" reuses is the range
/// map, so a cancel that cleared it restarts from zero and a cancel that kept
/// it does not.
#[test]
fn cancel_mode_decides_whether_a_restart_keeps_its_bytes() {
    let body = make_body(1024 * 1024);
    let origin = serve(
        body,
        Behaviour {
            delay_ms: 25,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("restart", false, |c| {
        c.max_connections = 1;
        c.adaptive_concurrency = 0;
    });

    for (mode, expect_zero) in [
        (hydra_cancel_mode_t::HYDRA_CANCEL_KEEP_PARTIAL, false),
        (hydra_cancel_mode_t::HYDRA_CANCEL_REMOVE_PARTIAL, true),
    ] {
        let out = h.dir.join(format!("m{}.bin", mode as u32));
        let id = make_job(&h, &origin.url("/m.bin"), &out, |c| c.adaptive = 0);
        unsafe { hydra_job_start(h.engine, id) };
        // Wait for real bytes, so "kept" has something to keep.
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
            unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
            if p.bytes_downloaded > 32 * 1024 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            unsafe { hydra_job_cancel(h.engine, id, mode as u32) },
            hydra_error_code_t::HYDRA_OK
        );
        await_terminal(&h, id, Duration::from_secs(30));

        let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
        unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
        if expect_zero {
            assert_eq!(
                p.bytes_downloaded, 0,
                "REMOVE_PARTIAL must clear the range map, so a restart begins at zero"
            );
            assert!(!out.exists());
        } else {
            assert!(
                p.bytes_downloaded > 0,
                "KEEP_PARTIAL must preserve the range map, so a restart resumes"
            );
            assert!(out.exists());
        }
    }
}

/// Moving a destination out from under a running transfer would leave the
/// finished ranges in the old file, the retried ranges in the new one, and a
/// range map claiming a single complete object. The call refuses instead.
#[test]
fn a_destination_cannot_be_moved_while_the_job_is_active() {
    let body = make_body(1024 * 1024);
    let origin = serve(
        body,
        Behaviour {
            delay_ms: 25,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("repath", false, |c| {
        c.max_connections = 1;
        c.adaptive_concurrency = 0;
    });
    let out = h.dir.join("start.bin");
    let moved = CString::new(h.dir.join("moved.bin").to_string_lossy().into_owned()).unwrap();
    let id = make_job(&h, &origin.url("/r.bin"), &out, |c| c.adaptive = 0);

    // Before it starts: allowed.
    assert_eq!(
        unsafe { hydra_job_set_output_path(h.engine, id, moved.as_ptr()) },
        hydra_error_code_t::HYDRA_OK
    );
    let back = CString::new(out.to_string_lossy().into_owned()).unwrap();
    unsafe { hydra_job_set_output_path(h.engine, id, back.as_ptr()) };

    unsafe { hydra_job_start(h.engine, id) };
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_DOWNLOADING,
        Duration::from_secs(20),
    );
    assert_eq!(
        unsafe { hydra_job_set_output_path(h.engine, id, moved.as_ptr()) },
        hydra_error_code_t::HYDRA_ERR_INVALID_STATE,
        "an active job must refuse to have its destination moved"
    );

    // Paused: allowed again, and that is the documented way to do it.
    unsafe { hydra_job_pause(h.engine, id) };
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_PAUSED,
        Duration::from_secs(20),
    );
    assert_eq!(
        unsafe { hydra_job_set_output_path(h.engine, id, moved.as_ptr()) },
        hydra_error_code_t::HYDRA_OK
    );
    unsafe { hydra_job_cancel(h.engine, id, 1) };
    await_terminal(&h, id, Duration::from_secs(30));
}

/// Removing a job must take its persisted metadata with it, or a later restore
/// resurrects something the application deliberately forgot.
#[test]
fn removing_a_job_also_removes_its_persisted_metadata() {
    let h = harness("removepersist", true, |_| {});
    let keep = make_job(&h, "http://127.0.0.1:1/keep", &h.dir.join("k.bin"), |_| {});
    let drop = make_job(&h, "http://127.0.0.1:1/drop", &h.dir.join("d.bin"), |_| {});
    assert_eq!(
        unsafe { hydra_engine_snapshot(h.engine) },
        hydra_error_code_t::HYDRA_OK
    );
    let before = std::fs::read_to_string(h.dir.join("state.json")).unwrap();
    assert!(before.contains("/drop") && before.contains("/keep"));

    assert_eq!(
        unsafe { hydra_job_remove(h.engine, drop) },
        hydra_error_code_t::HYDRA_OK
    );
    let after = std::fs::read_to_string(h.dir.join("state.json")).unwrap();
    assert!(
        !after.contains("/drop"),
        "a removed job stayed in the state file: {after}"
    );
    assert!(
        after.contains("/keep"),
        "removing one job must not disturb the others"
    );
    let _ = keep;
}

#[test]
fn events_coalesce_progress_but_never_lose_a_completion() {
    let body = make_body(1024 * 1024);
    let origin = serve(body, Behaviour::default());
    // A queue far too small for the progress traffic a transfer generates.
    let h = harness("events", false, |c| {
        c.event_queue_capacity = 8;
        c.progress_interval_ms = 10;
    });
    let out = h.dir.join("busy.bin");
    let id = make_job(&h, &origin.url("/busy.bin"), &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };

    // Deliberately drain slowly, so the queue is under pressure the whole time.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut saw_completed = false;
    let mut progress_events = 0;
    while Instant::now() < deadline && !saw_completed {
        let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
        if unsafe { hydra_event_wait(h.engine, 200, &mut ev) } != hydra_error_code_t::HYDRA_OK {
            continue;
        }
        if ev.kind == hydra_event_type_t::HYDRA_EVENT_PROGRESS {
            progress_events += 1;
            std::thread::sleep(Duration::from_millis(15));
        }
        if ev.kind == hydra_event_type_t::HYDRA_EVENT_COMPLETED {
            saw_completed = true;
        }
    }
    assert!(
        saw_completed,
        "the completion event was lost under queue pressure ({progress_events} progress events seen)"
    );
}

#[test]
fn an_event_callback_sees_the_same_events_as_the_queue() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEEN: AtomicU64 = AtomicU64::new(0);
    static COMPLETED: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn on_event(ev: *const hydra_event_t, user: *mut std::ffi::c_void) {
        assert!(!ev.is_null());
        assert_eq!(
            user as usize, 0xF00D,
            "user_data must be passed back verbatim"
        );
        SEEN.fetch_add(1, Ordering::Relaxed);
        if unsafe { (*ev).kind } == hydra_event_type_t::HYDRA_EVENT_COMPLETED {
            COMPLETED.fetch_add(1, Ordering::Relaxed);
        }
    }

    let body = make_body(64 * 1024);
    let origin = serve(body, Behaviour::default());
    let h = harness("callback", false, |_| {});
    assert_eq!(
        unsafe { hydra_event_set_callback(h.engine, Some(on_event), 0xF00D as *mut _) },
        hydra_error_code_t::HYDRA_OK
    );
    let out = h.dir.join("cb.bin");
    let id = make_job(&h, &origin.url("/cb.bin"), &out, |_| {});
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_COMPLETED);
    // The callback runs immediately AFTER the event is queued, which is what
    // `hydra_event_set_callback` documents. This thread was woken by that same
    // queue push, so on a loaded machine it can arrive here before the engine
    // thread has made the call. Wait for the callback rather than racing it:
    // the claim under test is that the callback sees the completion at all,
    // not that it sees it first.
    let deadline = Instant::now() + Duration::from_secs(10);
    while COMPLETED.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(SEEN.load(Ordering::Relaxed) > 0, "the callback never ran");
    assert_eq!(
        COMPLETED.load(Ordering::Relaxed),
        1,
        "the callback must see the completion too, not instead"
    );
    // Clearing must be safe while the engine is alive.
    assert_eq!(
        unsafe { hydra_event_set_callback(h.engine, None, ptr::null_mut()) },
        hydra_error_code_t::HYDRA_OK
    );
}

#[test]
fn a_shut_down_engine_refuses_work_and_releases_waiters() {
    let h = harness("shutdown", false, |_| {});
    assert_eq!(
        unsafe { hydra_engine_shutdown(h.engine, 1000) },
        hydra_error_code_t::HYDRA_OK
    );
    // Idempotent.
    assert_eq!(
        unsafe { hydra_engine_shutdown(h.engine, 1000) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(
        unsafe { hydra_job_start(h.engine, 1) },
        hydra_error_code_t::HYDRA_ERR_SHUTDOWN
    );
    // A wait after shutdown must return at once rather than sitting out its
    // timeout: a consumer thread has to be able to exit.
    let began = Instant::now();
    let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { hydra_event_wait(h.engine, 30_000, &mut ev) };
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "wait did not return promptly"
    );
    assert!(
        rc == hydra_error_code_t::HYDRA_ERR_SHUTDOWN || rc == hydra_error_code_t::HYDRA_OK,
        "unexpected code {rc:?}"
    );
}

#[test]
fn max_jobs_holds_the_extra_jobs_in_the_queue() {
    let body = make_body(512 * 1024);
    let origin = serve(
        body,
        Behaviour {
            delay_ms: 20,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("maxjobs", false, |c| {
        c.max_jobs = 1;
        c.max_connections = 1;
        c.adaptive_concurrency = 0;
    });
    let a = make_job(&h, &origin.url("/a"), &h.dir.join("a.bin"), |_| {});
    let b = make_job(&h, &origin.url("/b"), &h.dir.join("b.bin"), |_| {});
    unsafe { hydra_job_start(h.engine, a) };
    unsafe { hydra_job_start(h.engine, b) };
    await_state(
        &h,
        a,
        hydra_job_state_t::HYDRA_JOB_DOWNLOADING,
        Duration::from_secs(20),
    );

    let mut st = hydra_job_state_t::HYDRA_JOB_CREATED;
    unsafe { hydra_job_get_state(h.engine, b, &mut st) };
    assert_eq!(
        st,
        hydra_job_state_t::HYDRA_JOB_QUEUED,
        "the second job must wait for a slot, not run alongside"
    );

    // Both finish once the ceiling is lifted.
    assert_eq!(
        unsafe { hydra_engine_set_max_jobs(h.engine, 2) },
        hydra_error_code_t::HYDRA_OK
    );
    let done = await_all_terminal(&h, &[a, b], Duration::from_secs(120));
    for id in [a, b] {
        assert_eq!(
            done[&id].kind,
            hydra_event_type_t::HYDRA_EVENT_COMPLETED,
            "job {id}: {}",
            last_error()
        );
    }
}

#[test]
fn source_information_is_available_while_a_job_runs() {
    let body = make_body(512 * 1024);
    let origin = serve(
        body,
        Behaviour {
            delay_ms: 20,
            chunk: 16 * 1024,
            ..Behaviour::default()
        },
    );
    let h = harness("sources", false, |c| {
        c.max_connections = 2;
        c.adaptive_concurrency = 0;
        c.progress_interval_ms = 20;
    });
    let id = make_job(&h, &origin.url("/s.bin"), &h.dir.join("s.bin"), |c| {
        c.adaptive = 0;
        c.max_connections = 2;
    });
    unsafe { hydra_job_start(h.engine, id) };
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_DOWNLOADING,
        Duration::from_secs(20),
    );

    let mut arr: hydra_source_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_job_get_sources(h.engine, id, &mut arr) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(arr.len, 1, "one URL means one source");
    let first = unsafe { &*arr.items };
    let url = unsafe { CStr::from_ptr(first.url.data) }.to_string_lossy();
    assert!(url.contains("/s.bin"), "source url was {url:?}");
    unsafe { hydra_source_array_free(&mut arr) };
    // Freeing twice is harmless.
    unsafe { hydra_source_array_free(&mut arr) };

    unsafe { hydra_job_cancel(h.engine, id, 1) };
    await_terminal(&h, id, Duration::from_secs(30));
}

// ================================================================== metalink

/// Build a Metalink 4 document over live origins, so the mirrors in it are real.
fn meta4(
    name: &str,
    size: usize,
    digest_hex: &str,
    urls: &[String],
    pieces: Option<(u64, Vec<String>)>,
) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<metalink xmlns=\"urn:ietf:params:xml:ns:metalink\">\n");
    s.push_str("  <generator>hydra-ffi-test/1.0</generator>\n");
    s.push_str(&format!("  <file name=\"{name}\">\n"));
    s.push_str(&format!("    <size>{size}</size>\n"));
    s.push_str(&format!("    <hash type=\"sha-256\">{digest_hex}</hash>\n"));
    if let Some((len, hashes)) = pieces {
        s.push_str(&format!("    <pieces length=\"{len}\" type=\"sha-256\">\n"));
        for h in hashes {
            s.push_str(&format!("      <hash>{h}</hash>\n"));
        }
        s.push_str("    </pieces>\n");
    }
    for (i, u) in urls.iter().enumerate() {
        s.push_str(&format!("    <url priority=\"{}\">{u}</url>\n", i + 1));
    }
    // A scheme this build has no transport for, so the test also proves the
    // reader keeps it visible and the source list drops it.
    s.push_str("    <url priority=\"900\">rsync://rs.invalid/object.bin</url>\n");
    s.push_str("  </file>\n</metalink>\n");
    s
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    hya_net::digest::to_lower_hex(&h.finalize())
}

fn parse_doc(xml: &str) -> *mut hydra_metalink_t {
    let c = CString::new(xml).unwrap();
    let mut doc: *mut hydra_metalink_t = ptr::null_mut();
    let rc = unsafe { hydra_metalink_parse(c.as_ptr(), &mut doc) };
    assert_eq!(
        rc,
        hydra_error_code_t::HYDRA_OK,
        "parse failed: {}",
        last_error()
    );
    assert!(!doc.is_null());
    doc
}

#[test]
fn a_document_reports_its_files_and_its_ranked_mirrors_before_anything_is_fetched() {
    // The inspection layer exists so a host application can put the decision in
    // front of a user — which file, how large, is it verifiable — while the
    // object is still on the far side of a metered link.
    let body = make_body(1024);
    let a = serve(body.clone(), Behaviour::default());
    let b = serve(body.clone(), Behaviour::default());
    let urls = vec![a.url("/object.bin"), b.url("/object.bin")];
    let xml = meta4(
        "object.bin",
        body.len(),
        &sha256_hex(&body),
        &urls,
        Some((
            512,
            vec![sha256_hex(&body[..512]), sha256_hex(&body[512..])],
        )),
    );
    let doc = parse_doc(&xml);

    assert_eq!(
        unsafe { hydra_metalink_version(doc) },
        hydra_metalink_version_t::HYDRA_METALINK_V4
    );

    let mut files: hydra_metalink_file_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_metalink_files(doc, &mut files) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(files.len, 1);
    let f = unsafe { &*files.items };
    assert_eq!(
        unsafe { CStr::from_ptr(f.name.data) }.to_str().unwrap(),
        "object.bin"
    );
    assert_eq!(f.size, body.len() as u64);
    assert_eq!(f.name_usable, 1);
    assert_eq!(f.piece_count, 2);
    assert_eq!(f.piece_length, 512);
    assert_eq!(f.pieces_tile, 1, "512 x 2 tiles a 1024-byte object");
    assert_eq!(f.signed, 0);
    // The rsync mirror is VISIBLE in the count and absent from what can be
    // fetched: a list that silently shrank would be indistinguishable from one
    // the publisher wrote that way.
    assert_eq!(f.mirror_count, 3);
    assert_eq!(f.fetchable_count, 2);
    assert!(unsafe { CStr::from_ptr(f.digest.data) }
        .to_str()
        .unwrap()
        .starts_with("sha256:"));

    let mut mirrors: hydra_metalink_url_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_metalink_mirrors(doc, 0, &mut mirrors) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(mirrors.len, 2, "only the fetchable mirrors are sources");
    let ranks: Vec<u32> = (0..mirrors.len)
        .map(|i| unsafe { (*mirrors.items.add(i)).priority })
        .collect();
    assert_eq!(ranks, vec![1, 2], "dense, best first");
    assert_eq!(
        unsafe { CStr::from_ptr((*mirrors.items).url.data) }
            .to_str()
            .unwrap(),
        urls[0]
    );

    // A name that is not in the document is a miss, not entry zero.
    let mut idx: usize = 99;
    let want = CString::new("object.bin").unwrap();
    assert_eq!(
        unsafe { hydra_metalink_find_file(doc, want.as_ptr(), &mut idx) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(idx, 0);
    let missing = CString::new("not-in-here").unwrap();
    assert_eq!(
        unsafe { hydra_metalink_find_file(doc, missing.as_ptr(), &mut idx) },
        hydra_error_code_t::HYDRA_ERR_NOT_FOUND
    );
    assert_eq!(idx, 0, "a miss must not disturb the caller's variable");

    unsafe { hydra_metalink_url_array_free(&mut mirrors) };
    unsafe { hydra_metalink_file_array_free(&mut files) };
    unsafe { hydra_metalink_free(doc) };
}

#[test]
fn a_job_created_from_a_document_assembles_from_mirrors_that_share_no_validator() {
    // The claim the feature rests on. Two independent mirrors cannot produce the
    // same `ETag`, so the pairwise gate keeps exactly one of them — and with the
    // document's size as the admission test, both are used and the bytes are
    // still right, because the document's digest and pieces are checked after.
    let body = make_body(2 * 1024 * 1024 + 13);
    let no_validator = Behaviour {
        validator: false,
        ..Behaviour::default()
    };
    let a = serve(body.clone(), no_validator.clone());
    let b = serve(body.clone(), no_validator);
    let urls = vec![a.url("/object.bin"), b.url("/object.bin")];
    let chunk = 1 << 20;
    let pieces: Vec<String> = body.chunks(chunk).map(sha256_hex).collect();
    let xml = meta4(
        "object.bin",
        body.len(),
        &sha256_hex(&body),
        &urls,
        Some((chunk as u64, pieces)),
    );
    let doc = parse_doc(&xml);

    let h = harness("metalink", false, |c| {
        c.max_connections = 4;
        c.adaptive_concurrency = 0;
        c.progress_interval_ms = 20;
    });
    let out = h.dir.join("object.bin");
    let out_c = CString::new(out.to_string_lossy().into_owned()).unwrap();
    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32)
        },
        hydra_error_code_t::HYDRA_OK
    );
    // Deliberately NO urls: the document supplies them.
    cfg.output_path = out_c.as_ptr();
    cfg.adaptive = 0;
    cfg.max_connections = 4;

    let mut id: hydra_job_id_t = 0;
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, &cfg, &mut id) },
        hydra_error_code_t::HYDRA_OK,
        "creation failed: {}",
        last_error()
    );
    assert_ne!(id, 0);
    assert_eq!(
        unsafe { hydra_job_start(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "terminal event was {:?}: {}",
        ev.kind,
        last_error()
    );
    assert_eq!(
        std::fs::read(&out).unwrap(),
        body,
        "the assembled object must be byte-exact"
    );
    // Both mirrors served: that is the whole point, and it is only visible from
    // the request counts, because a one-source transfer produces the same file.
    let served = a.requests.load(std::sync::atomic::Ordering::Relaxed)
        + b.requests.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        served >= 2,
        "expected requests across both mirrors, got {served}"
    );

    unsafe { hydra_metalink_free(doc) };
}

#[test]
fn a_document_whose_mirrors_all_disagree_with_its_size_fails_with_a_reason() {
    // Reporting success here would hand back an object the publisher says is a
    // different size — which passes every length check this program makes.
    let body = make_body(4096);
    let o = serve(body.clone(), Behaviour::default());
    let xml = meta4(
        "object.bin",
        999_999,
        &sha256_hex(&body),
        &[o.url("/object.bin")],
        None,
    );
    let doc = parse_doc(&xml);

    let h = harness("metalink-size", false, |c| {
        c.progress_interval_ms = 20;
    });
    let out = h.dir.join("object.bin");
    let out_c = CString::new(out.to_string_lossy().into_owned()).unwrap();
    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32)
        },
        hydra_error_code_t::HYDRA_OK
    );
    cfg.output_path = out_c.as_ptr();
    let mut id: hydra_job_id_t = 0;
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, &cfg, &mut id) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(
        unsafe { hydra_job_start(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(30));
    assert_eq!(ev.kind, hydra_event_type_t::HYDRA_EVENT_FAILED);

    unsafe { hydra_metalink_free(doc) };
}

#[test]
fn metalink_handles_reject_null_and_stale_pointers_rather_than_crashing() {
    let mut doc: *mut hydra_metalink_t = ptr::null_mut();
    assert_eq!(
        unsafe { hydra_metalink_parse(ptr::null(), &mut doc) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    let junk = CString::new("<html>not a mirror list</html>").unwrap();
    assert_eq!(
        unsafe { hydra_metalink_parse(junk.as_ptr(), &mut doc) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    let missing = CString::new("/definitely/not/here.meta4").unwrap();
    assert_ne!(
        unsafe { hydra_metalink_open(missing.as_ptr(), &mut doc) },
        hydra_error_code_t::HYDRA_OK
    );

    let mut files: hydra_metalink_file_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_metalink_files(ptr::null_mut(), &mut files) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { hydra_metalink_version(ptr::null_mut()) },
        hydra_metalink_version_t::HYDRA_METALINK_UNKNOWN
    );

    // A freed handle is rejected rather than used: the magic is cleared on free,
    // so a double free and a use-after-free both land on the same check.
    let body = make_body(16);
    let d = parse_doc(&meta4(
        "x.bin",
        body.len(),
        &sha256_hex(&body),
        &["https://a.example/x.bin".to_string()],
        None,
    ));
    unsafe { hydra_metalink_free(d) };
    // Freeing twice is harmless.
    unsafe { hydra_metalink_free(d) };
    // Freeing NULL is harmless.
    unsafe { hydra_metalink_free(ptr::null_mut()) };
    unsafe { hydra_metalink_file_array_free(ptr::null_mut()) };
    unsafe { hydra_metalink_url_array_free(ptr::null_mut()) };
}

/// Live: a real Metalink document driven entirely through the C ABI.
///
/// Opt-in via `HYDRA_FFI_METALINK_LIVE=<path or url>`. The offline tests stop
/// at resolution; this is the only thing that exercises the ABI's own path —
/// `hydra_metalink_open` → `hydra_job_create_from_metalink` → the concurrent
/// mirror probe → the multi-source transfer → verification against the
/// document's digest — over real hosts.
#[test]
fn live_metalink_through_the_abi() {
    let Some(src) = std::env::var("HYDRA_FFI_METALINK_LIVE")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let c = CString::new(src).unwrap();
    let mut doc: *mut hydra_metalink_t = ptr::null_mut();
    assert_eq!(
        unsafe { hydra_metalink_open(c.as_ptr(), &mut doc) },
        hydra_error_code_t::HYDRA_OK,
        "open failed: {}",
        last_error()
    );

    let mut files: hydra_metalink_file_array_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_metalink_files(doc, &mut files) },
        hydra_error_code_t::HYDRA_OK
    );
    let f = unsafe { &*files.items };
    let want = f.size;
    eprintln!(
        "document: {} bytes, {} of {} mirrors fetchable, {} pieces",
        want, f.fetchable_count, f.mirror_count, f.piece_count
    );

    let h = harness("ffi-metalink-live", false, |c| {
        c.max_connections = 4;
        c.progress_interval_ms = 50;
    });
    let out = h.dir.join("object.bin");
    let out_c = CString::new(out.to_string_lossy().into_owned()).unwrap();
    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            hydra_job_config_init(&mut cfg, std::mem::size_of::<hydra_job_config_t>() as u32)
        },
        hydra_error_code_t::HYDRA_OK
    );
    cfg.output_path = out_c.as_ptr();
    cfg.max_connections = 4;

    let mut id: hydra_job_id_t = 0;
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, &cfg, &mut id) },
        hydra_error_code_t::HYDRA_OK,
        "creation failed: {}",
        last_error()
    );
    let started = Instant::now();
    assert_eq!(
        unsafe { hydra_job_start(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(300));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "terminal event was {:?}: {}",
        ev.kind,
        last_error()
    );
    let on_disk = std::fs::metadata(&out).expect("the file must exist").len();
    assert_eq!(on_disk, want, "size must match the document");
    eprintln!(
        "{on_disk} bytes in {:.2}s = {:.2} MB/s (digest verified by the engine)",
        started.elapsed().as_secs_f64(),
        on_disk as f64 / started.elapsed().as_secs_f64() / 1.048576e6
    );

    unsafe { hydra_metalink_file_array_free(&mut files) };
    unsafe { hydra_metalink_free(doc) };
}

// ============================================================ review fixes

/// A request head the origin received for `path`, or a panic naming what it did
/// receive.
fn head_for(origin: &support::Origin, path: &str) -> String {
    let heads = origin.heads.lock().unwrap();
    heads
        .iter()
        .find(|h| {
            h.lines()
                .next()
                .is_some_and(|l| l.split(' ').nth(1).is_some_and(|t| t.ends_with(path)))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no request for {path}; saw {heads:?}"))
}

fn has_header(head: &str, prefix: &str) -> bool {
    let want = prefix.to_ascii_lowercase();
    head.lines()
        .any(|l| l.to_ascii_lowercase().starts_with(&want))
}

/// A job configuration as a program built against an OLDER header would pass
/// it: `size` stops before `checksum`, so everything from there on — including
/// `auto_start` — is memory the caller never initialised and the library must
/// not read.
fn short_job_config(urls: *const *const c_char, out: *const c_char) -> hydra_job_config_t {
    let mut cfg: hydra_job_config_t = unsafe { std::mem::zeroed() };
    let short = std::mem::offset_of!(hydra_job_config_t, checksum) as u32;
    assert_eq!(
        unsafe { hydra_job_config_init(&mut cfg, short) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(cfg.size, short);
    cfg.urls = urls;
    cfg.url_count = 1;
    cfg.output_path = out;
    // What an old caller's stack might hold past the end of its struct.
    cfg.auto_start = 1;
    cfg.checksum.algorithm = 99;
    cfg.priority = 99;
    cfg
}

#[test]
fn a_short_job_config_is_read_as_a_prefix_and_never_auto_starts() {
    let h = harness("shortjob", false, |_| {});
    let url = CString::new("http://127.0.0.1:1/x").unwrap();
    let urls: [*const c_char; 1] = [url.as_ptr()];
    let out = CString::new(h.dir.join("x").to_string_lossy().into_owned()).unwrap();
    let cfg = short_job_config(urls.as_ptr(), out.as_ptr());

    let mut id: hydra_job_id_t = 0;
    assert_eq!(
        unsafe { hydra_job_create(h.engine, &cfg, &mut id) },
        hydra_error_code_t::HYDRA_OK,
        "{}",
        last_error()
    );
    let mut st = hydra_job_state_t::HYDRA_JOB_CANCELLED;
    unsafe { hydra_job_get_state(h.engine, id, &mut st) };
    assert_eq!(
        st,
        hydra_job_state_t::HYDRA_JOB_CREATED,
        "a field past the caller's `size` must not be read"
    );

    // The same rule on the Metalink path, which patches a copy of the struct.
    let body = make_body(16);
    let xml = meta4(
        "x.bin",
        body.len(),
        &sha256_hex(&body),
        &["http://127.0.0.1:1/x.bin".to_string()],
        None,
    );
    let doc = parse_doc(&xml);
    let mut id2: hydra_job_id_t = 0;
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, &cfg, &mut id2) },
        hydra_error_code_t::HYDRA_OK,
        "{}",
        last_error()
    );
    unsafe { hydra_job_get_state(h.engine, id2, &mut st) };
    assert_eq!(st, hydra_job_state_t::HYDRA_JOB_CREATED);

    // And NULL is a refusal there too, not a crash.
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, ptr::null(), &mut id2) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { hydra_job_create_from_metalink(h.engine, doc, 0, &cfg, ptr::null_mut()) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    unsafe { hydra_metalink_free(doc) };
}

#[test]
fn config_init_zeroes_the_tail_of_a_larger_struct() {
    // A program built against a NEWER header, whose struct has fields appended
    // past what this library knows: those fields must come back zero, which
    // is the value every appended field is defined to default from — not
    // whatever the caller's stack held.
    const EXTRA: usize = 64;
    let mut words = [u64::MAX; (std::mem::size_of::<hydra_engine_config_t>() + EXTRA) / 8];
    let total = std::mem::size_of_val(&words) as u32;
    let rc = unsafe { hydra_engine_config_init(words.as_mut_ptr() as *mut _, total) };
    assert_eq!(rc, hydra_error_code_t::HYDRA_OK);
    let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, total as usize) };
    let cfg = unsafe { &*(words.as_ptr() as *const hydra_engine_config_t) };
    assert_eq!(cfg.size, total, "size is the caller's, not this build's");
    assert!(
        bytes[std::mem::size_of::<hydra_engine_config_t>()..]
            .iter()
            .all(|&b| b == 0),
        "the appended tail must be zeroed"
    );

    let mut words = [u64::MAX; (std::mem::size_of::<hydra_job_config_t>() + EXTRA) / 8];
    let total = std::mem::size_of_val(&words) as u32;
    let rc = unsafe { hydra_job_config_init(words.as_mut_ptr() as *mut _, total) };
    assert_eq!(rc, hydra_error_code_t::HYDRA_OK);
    let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, total as usize) };
    assert!(bytes[std::mem::size_of::<hydra_job_config_t>()..]
        .iter()
        .all(|&b| b == 0));

    // A size no configuration struct will ever reach is a refusal, so a
    // garbage argument cannot make the library scribble kilobytes of zeros.
    let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_engine_config_init(&mut cfg, 1 << 20) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn the_initial_policy_in_the_engine_config_is_applied() {
    let h = harness("initpolicy", false, |c| {
        c.network_policy = hydra_network_policy_t::HYDRA_NETWORK_UNMETERED as u32;
        c.power_mode = hydra_power_mode_t::HYDRA_POWER_RESTRICTED as u32;
    });
    let mut p: hydra_runtime_policy_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_engine_get_policy(h.engine, &mut p) },
        hydra_error_code_t::HYDRA_OK
    );
    assert_eq!(
        p.network_policy,
        hydra_network_policy_t::HYDRA_NETWORK_UNMETERED as u32
    );
    assert_eq!(
        p.power_mode,
        hydra_power_mode_t::HYDRA_POWER_RESTRICTED as u32
    );

    let mut cfg: hydra_engine_config_t = unsafe { std::mem::zeroed() };
    unsafe {
        hydra_engine_config_init(
            &mut cfg,
            std::mem::size_of::<hydra_engine_config_t>() as u32,
        )
    };
    cfg.power_mode = 7;
    assert!(
        unsafe { hydra_engine_create(&cfg) }.is_null(),
        "an unknown power mode is a refusal, not a default"
    );
}

#[test]
fn wake_releases_a_forever_waiter_with_again() {
    let h = harness("wake", false, |_| {});
    let engine = h.engine as usize;
    let waiter = std::thread::spawn(move || {
        let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
        unsafe { hydra_event_wait(engine as *mut hydra_engine_t, HYDRA_WAIT_FOREVER, &mut ev) }
    });
    // Nothing observable says when the waiter has parked, so wake until it
    // returns; a wake that lands before the wait begins is meant to be lost.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !waiter.is_finished() {
        assert!(
            Instant::now() < deadline,
            "hydra_event_wake did not release the waiter"
        );
        assert_eq!(
            unsafe { hydra_event_wake(h.engine) },
            hydra_error_code_t::HYDRA_OK
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        waiter.join().unwrap(),
        hydra_error_code_t::HYDRA_ERR_AGAIN,
        "a woken wait reports nothing to do, not a failure"
    );
}

#[test]
fn a_jobs_events_are_ordered_and_its_terminal_event_is_its_last() {
    use hydra_event_type_t as T;
    let body = make_body(1024 * 1024);
    let origin = serve(body, Behaviour::default());
    let h = harness("ordering", false, |c| c.progress_interval_ms = 10);
    let id = make_job(&h, &origin.url("/o.bin"), &h.dir.join("o.bin"), |_| {});
    unsafe { hydra_job_start(h.engine, id) };

    let mut seen: Vec<T> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
        if unsafe { hydra_event_wait(h.engine, 500, &mut ev) } != hydra_error_code_t::HYDRA_OK {
            continue;
        }
        assert_eq!(ev.job_id, id);
        seen.push(ev.kind);
        if is_terminal_event(ev.kind) {
            break;
        }
    }
    assert_eq!(seen.last(), Some(&T::HYDRA_EVENT_COMPLETED), "{seen:?}");
    assert!(
        seen.starts_with(&[
            T::HYDRA_EVENT_JOB_CREATED,
            T::HYDRA_EVENT_JOB_QUEUED,
            T::HYDRA_EVENT_JOB_STARTED
        ]),
        "{seen:?}"
    );
    let resolved = seen
        .iter()
        .position(|k| *k == T::HYDRA_EVENT_RESOLVED)
        .expect("RESOLVED");
    assert!(
        seen.iter()
            .position(|k| *k == T::HYDRA_EVENT_PROGRESS)
            .is_none_or(|p| p > resolved),
        "PROGRESS before RESOLVED: {seen:?}"
    );

    // Nothing may follow the terminal event — in particular not a progress
    // sample that was pending when the completion was queued.
    for _ in 0..3 {
        let mut ev: hydra_event_t = unsafe { std::mem::zeroed() };
        let rc = unsafe { hydra_event_wait(h.engine, 100, &mut ev) };
        assert_eq!(
            rc,
            hydra_error_code_t::HYDRA_ERR_AGAIN,
            "an event ({:?}) was delivered after the job's terminal event",
            ev.kind
        );
    }
}

#[test]
fn a_resumed_job_discards_ranges_from_an_object_of_a_different_size() {
    let first = make_body(2 * 1024 * 1024);
    let origin = serve(
        first.clone(),
        Behaviour {
            delay_ms: 25,
            chunk: 32 * 1024,
            ..Behaviour::default()
        },
    );
    let port = origin.port;
    let h = harness("replaced", false, |c| {
        c.max_connections = 2;
        c.adaptive_concurrency = 0;
        c.progress_interval_ms = 20;
    });
    let out = h.dir.join("replaced.bin");
    let id = make_job(&h, &origin.url("/replaced.bin"), &out, |c| {
        c.adaptive = 0;
        c.max_connections = 2;
    });
    unsafe { hydra_job_start(h.engine, id) };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "transfer never started moving");
        let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
        unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
        if p.bytes_downloaded > 64 * 1024 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        unsafe { hydra_job_pause(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_PAUSED,
        Duration::from_secs(20),
    );
    let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_get_progress(h.engine, id, &mut p) };
    assert!(
        p.bytes_downloaded > 0,
        "nothing was held, so nothing can be discarded"
    );

    // The publisher replaces the object behind the same URL with a shorter,
    // different one. The spans on disk describe the OLD object: splicing them
    // into the new one would assemble a file of the right length and the wrong
    // bytes, reported as a success.
    drop(origin);
    let second = make_body(1024 * 1024 + 999);
    let replaced = support::serve_at(port, second.clone(), Behaviour::default());
    assert_eq!(replaced.port, port);

    assert_eq!(
        unsafe { hydra_job_resume(h.engine, id) },
        hydra_error_code_t::HYDRA_OK
    );
    let ev = await_terminal(&h, id, Duration::from_secs(120));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), second.len());
    assert_eq!(
        sha256(&got),
        sha256(&second),
        "held ranges from the old object leaked in"
    );
}

#[test]
fn cancelling_a_job_that_never_started_does_not_delete_a_pre_existing_file() {
    let h = harness("cancel-untouched", false, |_| {});
    let out = h.dir.join("precious.bin");
    std::fs::write(&out, b"the host's own file").unwrap();
    let id = make_job(&h, "http://127.0.0.1:1/x", &out, |_| {});
    assert_eq!(
        unsafe {
            hydra_job_cancel(
                h.engine,
                id,
                hydra_cancel_mode_t::HYDRA_CANCEL_REMOVE_PARTIAL as u32,
            )
        },
        hydra_error_code_t::HYDRA_OK
    );
    await_state(
        &h,
        id,
        hydra_job_state_t::HYDRA_JOB_CANCELLED,
        Duration::from_secs(5),
    );
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"the host's own file",
        "no attempt ever ran, so there is no partial file to remove"
    );
}

#[test]
fn an_http_proxy_login_is_sent_as_proxy_authorization() {
    let body = make_body(64 * 1024);
    // The origin stands in for a forward proxy: it ignores the absolute-form
    // request target and answers with the body, recording what it was sent.
    let proxy = serve(body.clone(), Behaviour::default());
    let h = harness("proxyauth", false, |_| {});
    let out = h.dir.join("via-proxy.bin");
    let host = CString::new("127.0.0.1").unwrap();
    let user = CString::new("pu").unwrap();
    let pass = CString::new("pw").unwrap();
    let pc = hydra_proxy_config_t {
        kind: hydra_proxy_type_t::HYDRA_PROXY_HTTP as u32,
        port: proxy.port,
        reserved: [0; 2],
        host: host.as_ptr(),
        username: user.as_ptr(),
        password: pass.as_ptr(),
    };
    let id = make_job(&h, "http://origin.invalid/via-proxy.bin", &out, |c| {
        c.proxy = &pc
    });
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(sha256(&std::fs::read(&out).unwrap()), sha256(&body));
    assert!(
        proxy.saw_header("Proxy-Authorization: Basic cHU6cHc="),
        "the proxy login never reached the proxy: {:?}",
        proxy.heads.lock().unwrap()
    );
    assert!(
        proxy.heads.lock().unwrap().iter().all(|h| h
            .lines()
            .next()
            .unwrap_or("")
            .contains("http://origin.invalid")),
        "a request through an HTTP proxy is sent in absolute form"
    );
}

#[test]
fn credentials_do_not_follow_a_cross_origin_redirect() {
    let body = make_body(64 * 1024);
    let far = serve(body.clone(), Behaviour::default());
    let near = serve(
        body.clone(),
        Behaviour {
            redirect: Some(("/away.bin".into(), far.url("/landed.bin"))),
            ..Behaviour::default()
        },
    );
    let h = harness("redirect-creds", false, |_| {});
    let user = CString::new("u").unwrap();
    let pass = CString::new("p").unwrap();

    let out = h.dir.join("away.bin");
    let id = make_job(&h, &near.url("/away.bin"), &out, |c| {
        c.username = user.as_ptr();
        c.password = pass.as_ptr();
    });
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(sha256(&std::fs::read(&out).unwrap()), sha256(&body));
    assert!(
        has_header(&head_for(&near, "/away.bin"), "Authorization: Basic"),
        "the origin the job named gets its login"
    );
    assert!(
        !far.saw_header("Authorization:"),
        "a login typed for one host followed a redirect to another: {:?}",
        far.heads.lock().unwrap()
    );

    // Same origin: the credential stays, because it was typed for this host.
    let same = serve(
        body.clone(),
        Behaviour {
            redirect: Some(("/moved.bin".into(), "/here.bin".into())),
            ..Behaviour::default()
        },
    );
    let out2 = h.dir.join("moved.bin");
    let id2 = make_job(&h, &same.url("/moved.bin"), &out2, |c| {
        c.username = user.as_ptr();
        c.password = pass.as_ptr();
    });
    unsafe { hydra_job_start(h.engine, id2) };
    let ev = await_terminal(&h, id2, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert!(
        has_header(&head_for(&same, "/here.bin"), "Authorization: Basic"),
        "a same-origin hop keeps the login"
    );
}

#[test]
fn a_corrupt_state_file_is_refused_and_a_hostile_one_is_sanitised() {
    let h = harness("hostile-state", true, |_| {});
    let path = h.dir.join("state.json");
    let mut n: usize = 0;

    std::fs::write(&path, b"{\"version\": 1, \"jobs\": [{\"id\": 1, \"ur").unwrap();
    assert_eq!(
        unsafe { hydra_engine_restore(h.engine, &mut n) },
        hydra_error_code_t::HYDRA_ERR_PROTOCOL,
        "a truncated file is refused, not resumed"
    );

    std::fs::write(&path, b"{\"version\": 99, \"jobs\": []}").unwrap();
    assert_eq!(
        unsafe { hydra_engine_restore(h.engine, &mut n) },
        hydra_error_code_t::HYDRA_ERR_UNSUPPORTED,
        "a file from a newer hydra is refused rather than guessed at"
    );

    // A record that claims to be mid-transfer, with a range map that overlaps,
    // runs backwards and reaches past the object.
    let out = h.dir.join("hostile.bin").to_string_lossy().into_owned();
    let record = format!(
        "{{\"version\": 1, \"jobs\": [{{\"id\": 7, \"urls\": [\"http://127.0.0.1:1/h\"], \
         \"output_path\": {out:?}, \"state\": 3, \"size\": 1000, \
         \"held\": [[50, 150], [0, 100], [900, 5000], [40, 30]], \
         \"max_retries\": 4000000000, \"max_connections\": 4000000000}}]}}"
    );
    std::fs::write(&path, record).unwrap();
    assert_eq!(
        unsafe { hydra_engine_restore(h.engine, &mut n) },
        hydra_error_code_t::HYDRA_OK,
        "{}",
        last_error()
    );
    assert_eq!(n, 1);
    let mut st = hydra_job_state_t::HYDRA_JOB_CREATED;
    unsafe { hydra_job_get_state(h.engine, 7, &mut st) };
    assert_eq!(
        st,
        hydra_job_state_t::HYDRA_JOB_PAUSED,
        "nothing is running after a restore, whatever the file says"
    );
    let mut p: hydra_progress_t = unsafe { std::mem::zeroed() };
    unsafe { hydra_job_get_progress(h.engine, 7, &mut p) };
    assert_eq!(
        p.bytes_downloaded, 250,
        "[0,150) and [900,1000): merged, clamped, counted once"
    );
    assert_eq!(p.completed_ranges, 2);
    // And it is a job the application can act on: not stuck "running".
    assert_eq!(
        unsafe { hydra_job_remove(h.engine, 7) },
        hydra_error_code_t::HYDRA_OK
    );
}

#[test]
fn a_small_object_is_fetched_over_one_connection() {
    let body = make_body(300 * 1024);
    let origin = serve(body.clone(), Behaviour::default());
    let h = harness("small", false, |c| {
        c.max_connections = 8;
        c.adaptive_concurrency = 0;
    });
    let out = h.dir.join("small.bin");
    let id = make_job(&h, &origin.url("/small.bin"), &out, |c| {
        c.max_connections = 8;
        c.adaptive = 0;
    });
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    assert_eq!(sha256(&std::fs::read(&out).unwrap()), sha256(&body));
    // Eight connections over 300 KiB would be eight requests for ranges that
    // start deep inside the object; one connection asks from the front only.
    let inner: Vec<String> = origin
        .heads
        .lock()
        .unwrap()
        .iter()
        .filter(|h| {
            h.lines().any(|l| {
                let l = l.to_ascii_lowercase();
                l.starts_with("range: bytes=") && !l.starts_with("range: bytes=0-")
            })
        })
        .cloned()
        .collect();
    assert!(inner.is_empty(), "the object was split: {inner:?}");
}

#[test]
fn a_url_cannot_name_a_file_outside_its_own_segment() {
    let body = make_body(1024);
    let origin = serve(body, Behaviour::default());
    let h = harness("filename", false, |_| {});
    let id = make_job(
        &h,
        &origin.url("/pub/dir%5C..%5Cevil%00.txt"),
        &h.dir.join("named.bin"),
        |_| {},
    );
    unsafe { hydra_job_start(h.engine, id) };
    let ev = await_terminal(&h, id, Duration::from_secs(60));
    assert_eq!(
        ev.kind,
        hydra_event_type_t::HYDRA_EVENT_COMPLETED,
        "{}",
        last_error()
    );
    let mut snap: hydra_job_snapshot_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { hydra_job_get_snapshot(h.engine, id, &mut snap) },
        hydra_error_code_t::HYDRA_OK
    );
    let name = unsafe { CStr::from_ptr(snap.file_name.data) }
        .to_string_lossy()
        .into_owned();
    unsafe { hydra_job_snapshot_free(&mut snap) };
    assert_eq!(
        name, "evil",
        "a decoded separator or NUL must not survive into the name"
    );
}

#[test]
fn an_oversized_metalink_file_is_refused_before_it_is_read() {
    let dir = scratch("bigdoc");
    let path = dir.join("huge.meta4");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(4 * 1024 * 1024 + 1).unwrap();
    drop(f);
    let c = CString::new(path.to_string_lossy().into_owned()).unwrap();
    let mut doc: *mut hydra_metalink_t = ptr::null_mut();
    assert_eq!(
        unsafe { hydra_metalink_open(c.as_ptr(), &mut doc) },
        hydra_error_code_t::HYDRA_ERR_INVALID_ARGUMENT
    );
    assert!(doc.is_null());
    let _ = std::fs::remove_dir_all(dir);
}

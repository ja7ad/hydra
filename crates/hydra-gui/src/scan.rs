// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Post-download virus scanning.
//!
//! When Options > Downloads > "Virus scanner program" names an executable,
//! every finished file is handed to it before the completion tail runs (sound,
//! progress window close, complete dialog). The scanner's console output is
//! streamed back line by line so the progress dialog can show it where the
//! per-connection table normally sits.
//!
//! The scanner runs in its own OS thread, not on the engine runtime: it is an
//! external process whose lifetime has nothing to do with a transfer, and a
//! blocking `wait()` must never sit on a tokio worker. Exit codes follow
//! clamscan's convention (0 = clean, 1 = infected, anything else = error),
//! which is also what most command-line scanners use.

use crate::model::DlId;
use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Console lines are capped per scan: a scanner pointed at a directory by a
/// bad `%1` substitution can print for a very long time, and the dialog only
/// ever shows the tail.
const MAX_LINES: usize = 2000;

#[derive(Clone, Debug)]
pub enum ScanEvent {
    /// One console line from the scanner, verbatim.
    Line {
        id: DlId,
        text: String,
        /// The line ended in a bare carriage return: a terminal would have
        /// drawn it over the previous one. clamscan's "Loading: 2s, ETA: 0s
        /// [====>]" bar is nothing but these, and appending each redraw would
        /// bury the real output under hundreds of near-identical rows.
        redraw: bool,
    },
    Done(DlId, Outcome),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    /// Signature name where the scanner named one, empty otherwise.
    Infected(String),
    /// The scanner could not be run, or exited with an error status.
    Failed(String),
}

// ------------------------------------------------------------------- bus

struct Bus {
    tx: UnboundedSender<ScanEvent>,
    rx: Mutex<Option<UnboundedReceiver<ScanEvent>>>,
}

static BUS: OnceLock<Bus> = OnceLock::new();

fn bus() -> &'static Bus {
    BUS.get_or_init(|| {
        let (tx, rx) = unbounded_channel();
        Bus {
            tx,
            rx: Mutex::new(Some(rx)),
        }
    })
}

/// The subscription side: take the event receiver (once).
pub fn take_events() -> Option<UnboundedReceiver<ScanEvent>> {
    bus().rx.lock().ok()?.take()
}

/// Running scanners, so Skip can kill one. Entries are removed by the
/// scanner thread when the process is reaped.
type Live = (DlId, Arc<Mutex<Option<Child>>>, Arc<AtomicBool>);
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------- argv

/// Split the "Command line parameters" box into argv, honouring double
/// quotes so a switch carrying a path with spaces survives.
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in args.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// The full argument list for one scan. `%1` is replaced
/// by the file path wherever it appears; without it the path is appended
/// last, which is what `clamscan <file>` and friends expect.
pub fn argv(args: &str, path: &str) -> Vec<String> {
    let mut argv = split_args(args);
    let mut substituted = false;
    for a in &mut argv {
        if a.contains("%1") {
            *a = a.replace("%1", path);
            substituted = true;
        }
    }
    if !substituted {
        argv.push(path.to_string());
    }
    argv
}

/// The signature name out of a clamscan hit line: `/path/f.zip: Eicar-Test-Signature FOUND`.
fn signature(line: &str) -> Option<String> {
    let head = line.trim_end().strip_suffix("FOUND")?.trim_end();
    let sig = match head.rfind(": ") {
        Some(i) => &head[i + 2..],
        None => head,
    };
    (!sig.is_empty()).then(|| sig.to_string())
}

// --------------------------------------------------------------- runner

/// Run `program` over `path` and stream its output back as [`ScanEvent`]s.
/// Returns immediately; exactly one [`ScanEvent::Done`] follows unless the
/// scan is [`skip`]ped.
pub fn start(id: DlId, program: &str, args: &str, path: &str) {
    let tx = bus().tx.clone();
    let program = program.to_string();
    let argv = argv(args, path);
    std::thread::spawn(move || {
        crate::log::debug(&format!("#{id} virus scan: {program} {}", argv.join(" ")));
        let spawned = Command::new(&program)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                crate::log::warn(&format!("#{id} virus scan: {program}: {e}"));
                let _ = tx.send(ScanEvent::Line {
                    id,
                    text: format!("{program}: {e}"),
                    redraw: false,
                });
                let _ = tx.send(ScanEvent::Done(id, Outcome::Failed(e.to_string())));
                return;
            }
        };
        let out = child.stdout.take();
        let err = child.stderr.take();
        let killed = Arc::new(AtomicBool::new(false));
        let handle = Arc::new(Mutex::new(Some(child)));
        if let Ok(mut live) = LIVE.lock() {
            live.push((id, handle.clone(), killed.clone()));
        }

        // stderr on its own thread: a scanner that fills one pipe while
        // nothing drains the other would deadlock on the write.
        let errt = {
            let tx = tx.clone();
            std::thread::spawn(move || pump(id, err, &tx))
        };
        let found = pump(id, out, &tx);
        let found = errt.join().ok().flatten().or(found);

        // Both pipes are at EOF, so the process has exited or is about to:
        // the wait cannot block the Skip path for any meaningful time.
        let status = handle
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().map(|c| c.wait()));
        if let Ok(mut live) = LIVE.lock() {
            live.retain(|(x, _, _)| *x != id);
        }
        if killed.load(Ordering::Relaxed) {
            // Skipped: the GUI already moved on when it killed the process.
            return;
        }
        let outcome = match status {
            Some(Ok(st)) => match st.code() {
                Some(0) => Outcome::Clean,
                Some(1) => Outcome::Infected(found.unwrap_or_default()),
                Some(c) => Outcome::Failed(format!("exit code {c}")),
                // Killed by a signal without Skip being pressed.
                None => Outcome::Failed("terminated".into()),
            },
            Some(Err(e)) => Outcome::Failed(e.to_string()),
            None => Outcome::Failed("no process".into()),
        };
        crate::log::debug(&format!("#{id} virus scan: {outcome:?}"));
        let _ = tx.send(ScanEvent::Done(id, outcome));
    });
}

/// Forward every line of one pipe, returning the signature name if the
/// scanner reported a hit on the way past.
///
/// Line breaking is done by hand rather than with [`BufRead::lines`]: a
/// progress bar redrawn with bare carriage returns carries no `\n` at all, so
/// `lines()` would hold the whole animation back as one enormous line and
/// release it only at the next real newline.
fn pump<R: Read>(id: DlId, pipe: Option<R>, tx: &UnboundedSender<ScanEvent>) -> Option<String> {
    let pipe = pipe?;
    let mut rdr = BufReader::new(pipe);
    let mut chunk = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    let mut found = None;
    let mut sent = 0usize;
    let mut cr = false;
    let mut alive = true;

    // Emitting is a closure over the counters so the CRLF fold-in below can
    // reach it from three places without repeating the bookkeeping.
    let mut emit = |pending: &mut Vec<u8>, redraw: bool, found: &mut Option<String>| -> bool {
        let text = String::from_utf8_lossy(pending).trim_end().to_string();
        pending.clear();
        if text.ends_with("FOUND") {
            *found = signature(&text).or(found.take());
        }
        sent += 1;
        if sent > MAX_LINES {
            return true;
        }
        tx.send(ScanEvent::Line { id, text, redraw }).is_ok()
    };

    while alive {
        let n = match rdr.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if cr {
                cr = false;
                // CRLF is one ordinary line ending, a lone CR is a redraw.
                if b == b'\n' {
                    alive = emit(&mut pending, false, &mut found);
                    continue;
                }
                alive = emit(&mut pending, true, &mut found);
            }
            match b {
                b'\r' => cr = true,
                // Blank lines are part of the scanner's own formatting;
                // keeping them preserves the layout the user knows.
                b'\n' => alive = emit(&mut pending, false, &mut found),
                _ => pending.push(b),
            }
            if !alive {
                break;
            }
        }
    }
    if alive && (cr || !pending.is_empty()) {
        emit(&mut pending, cr, &mut found);
    }
    found
}

/// Stop a running scan (the Skip button). No [`ScanEvent::Done`] follows —
/// the caller has already decided the download is finished.
pub fn skip(id: DlId) {
    let entry = LIVE
        .lock()
        .ok()
        .and_then(|live| live.iter().find(|(x, _, _)| *x == id).cloned());
    if let Some((_, handle, killed)) = entry {
        killed.store(true, Ordering::Relaxed);
        if let Ok(mut g) = handle.lock() {
            if let Some(c) = g.as_mut() {
                let _ = c.kill();
            }
        }
        crate::log::debug(&format!("#{id} virus scan skipped"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_appends_the_path_when_there_is_no_placeholder() {
        assert_eq!(argv("", "/a/b.zip"), vec!["/a/b.zip"]);
        assert_eq!(
            argv("--no-summary -i", "/a/b.zip"),
            vec!["--no-summary", "-i", "/a/b.zip"]
        );
    }

    #[test]
    fn argv_substitutes_the_placeholder_in_place() {
        assert_eq!(
            argv("--scan %1 --verbose", "/a/b.zip"),
            vec!["--scan", "/a/b.zip", "--verbose"]
        );
    }

    #[test]
    fn quoted_arguments_stay_one_token() {
        assert_eq!(
            argv("--db \"/opt/my defs\"", "/a/b.zip"),
            vec!["--db", "/opt/my defs", "/a/b.zip"]
        );
    }

    /// The whole runner over a stand-in scanner: streamed output, a bare
    /// carriage return marked as a redraw, and both verdicts the exit code
    /// carries. One test, because the event receiver can be taken only once.
    #[cfg(unix)]
    #[test]
    fn a_scanner_streams_its_console_and_its_verdict() {
        fn lines(rx: &mut UnboundedReceiver<ScanEvent>, n: usize) -> Vec<(String, bool)> {
            (0..n)
                .map(|_| match rx.blocking_recv().expect("event") {
                    ScanEvent::Line { text, redraw, .. } => (text, redraw),
                    ev => panic!("expected a line, got {ev:?}"),
                })
                .collect()
        }
        let mut rx = take_events().expect("the scan event receiver");

        start(
            7,
            "/bin/sh",
            r#"-c "printf 'load\rdone\n%1: OK\n'""#,
            "/tmp/x.zip",
        );
        assert_eq!(
            lines(&mut rx, 3),
            vec![
                ("load".to_string(), true),
                ("done".to_string(), false),
                ("/tmp/x.zip: OK".to_string(), false),
            ]
        );
        match rx.blocking_recv().expect("verdict") {
            ScanEvent::Done(7, Outcome::Clean) => {}
            ev => panic!("expected a clean verdict, got {ev:?}"),
        }

        start(
            8,
            "/bin/sh",
            r#"-c "printf '%1: Eicar-Test-Signature FOUND\n'; exit 1""#,
            "/tmp/x.zip",
        );
        assert_eq!(
            lines(&mut rx, 1),
            vec![("/tmp/x.zip: Eicar-Test-Signature FOUND".to_string(), false)]
        );
        match rx.blocking_recv().expect("verdict") {
            ScanEvent::Done(8, Outcome::Infected(sig)) => {
                assert_eq!(sig, "Eicar-Test-Signature")
            }
            ev => panic!("expected an infected verdict, got {ev:?}"),
        }
    }

    #[test]
    fn signature_reads_the_clamscan_hit_line() {
        assert_eq!(
            signature("/Users/j/Downloads/x.zip: Eicar-Test-Signature FOUND").as_deref(),
            Some("Eicar-Test-Signature")
        );
        assert_eq!(signature("/Users/j/Downloads/x.zip: OK"), None);
    }
}

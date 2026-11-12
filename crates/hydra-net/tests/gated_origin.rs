// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An origin that serves a file only to a request dressed like a browser's.
//!
//! File hosts and CDNs answer `403` to a good link when the request lacks a
//! `Referer` from their own site or a current browser's `User-Agent`. The
//! link is not the problem, so the probe asks again as a browser would have,
//! and the transfer that follows must wear what the origin accepted on every
//! range request, not only on the probe.

use std::sync::{Arc, Mutex};

use hya_core::{Scheduler, Source};
use hya_net::{
    probe_admitted, probe_resilient, run_transfer, Target, TcpConnector, BROWSER_USER_AGENT,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SIZE: u64 = 300_000;

fn byte_at(i: u64) -> u8 {
    (i.wrapping_mul(31) ^ (i >> 8)) as u8
}

/// Serve the object to requests whose head contains `pass`, `403` the rest.
/// Returns the port and every request head received.
async fn spawn(pass: impl Fn(u16) -> String) -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = l.local_addr().unwrap().port();
    let pass = pass(port);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                continue;
            };
            let (log, pass) = (log.clone(), pass.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).to_string();
                log.lock().unwrap().push(head.clone());
                if !head.contains(&pass) {
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                if head.starts_with("HEAD") {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {SIZE}\r\nAccept-Ranges: bytes\r\n\
                         ETag: \"gated\"\r\nConnection: close\r\n\r\n"
                    );
                    let _ = s.write_all(reply.as_bytes()).await;
                    return;
                }
                let (lo, hi) = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.trim().split_once('-'))
                    .map(|(lo, hi)| (lo.parse().unwrap(), hi.parse::<u64>().unwrap()))
                    .unwrap_or((0, SIZE - 1));
                let hi = hi.min(SIZE - 1);
                let body: Vec<u8> = (lo..=hi).map(byte_at).collect();
                let reply = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {lo}-{hi}/{SIZE}\r\n\
                     Content-Length: {}\r\nETag: \"gated\"\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
                let _ = s.write_all(&body).await;
            });
        }
    });
    (port, seen)
}

async fn download(t: Target, name: &str) -> Vec<u8> {
    let out = std::env::temp_dir().join(name);
    let outs = out.to_string_lossy().to_string();
    let source = Source {
        gamma_est: 2e6,
        delta_est: 0.01,
        ..Default::default()
    };
    let sched = Scheduler::new(SIZE, vec![source], &[2]);
    run_transfer(Arc::new(TcpConnector), vec![t], &[2], SIZE, &outs, sched)
        .await
        .expect("transfer");
    let got = std::fs::read(&out).expect("output");
    let _ = std::fs::remove_file(&out);
    got
}

fn is_the_object(got: &[u8]) -> bool {
    got.len() as u64 == SIZE && got.iter().enumerate().all(|(i, b)| *b == byte_at(i as u64))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_served_only_to_browsers_downloads_byte_exactly() {
    let (port, seen) = spawn(|_| format!("User-Agent: {BROWSER_USER_AGENT}")).await;
    let t = Target::direct("127.0.0.1", port, "/d/token/file.apk")
        .with_headers(Vec::new(), Some("hydra-test/1".into()));

    let (p, guise) = probe_admitted(
        &t,
        |t| async move { probe_resilient(&TcpConnector, &t).await },
    )
    .await
    .expect("probe");
    assert_eq!(p.refusal(), None);
    assert_eq!(p.stated_length(), Some(SIZE));

    let asked_before = seen.lock().unwrap().len();
    let got = download(t.in_guise(&guise), "hydra_gated_browser.bin").await;
    assert!(is_the_object(&got));
    let seen = seen.lock().unwrap();
    assert!(
        seen[asked_before..]
            .iter()
            .all(|r| r.contains(BROWSER_USER_AGENT)),
        "a range request went out undressed and was refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hotlink_protected_file_downloads_once_it_is_asked_for_from_its_own_site() {
    let (port, seen) = spawn(|port| format!("Referer: http://127.0.0.1:{port}/\r\n")).await;
    let t = Target::direct("127.0.0.1", port, "/7232/a_720p.mp4");

    let (p, guise) = probe_admitted(
        &t,
        |t| async move { probe_resilient(&TcpConnector, &t).await },
    )
    .await
    .expect("probe");
    assert_eq!(p.refusal(), None);
    assert_eq!(guise.user_agent, None, "the cheaper guise was enough");

    let got = download(t.in_guise(&guise), "hydra_gated_referer.bin").await;
    assert!(is_the_object(&got));
    assert!(seen.lock().unwrap()[0].starts_with("HEAD"));
}

#[tokio::test]
async fn a_link_that_is_refused_however_it_is_asked_is_reported_and_bounded() {
    let (port, seen) = spawn(|_| "X-Never: sent".into()).await;
    let t = Target::direct("127.0.0.1", port, "/expired");

    let (p, guise) = probe_admitted(
        &t,
        |t| async move { probe_resilient(&TcpConnector, &t).await },
    )
    .await
    .expect("probe");
    assert_eq!(
        p.refusal().as_deref(),
        Some("server answered 403 Forbidden")
    );
    assert_eq!(guise, hya_net::Guise::default());
    assert_eq!(seen.lock().unwrap().len(), 6, "HEAD and GET per guise");
}

// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! `hydra --preview <url>`: what is inside a ZIP archive, without the archive.
//!
//! The same peek the GUI's Preview button makes — `hya_net::zipdir` reads
//! the index off the file's tail — drawn as a table for a terminal. One
//! probe to find the object and its size, one small ranged GET, and the
//! listing is on screen whatever the archive weighs.

use hya_net::zipdir::{self, DosTime, Entry, PeekError};

pub async fn run(url: &str, args: &crate::cli::Cli) -> Result<(), String> {
    let Some(u) = crate::url::Url::parse(url) else {
        return Err(format!("{url} is not an http, https or ftp URL"));
    };
    let conn = hya_net::TlsCapableConnector::with_insecure(args.insecure)
        .map_err(|e| format!("tls setup failed: {e}"))?;
    let (probe, url) = crate::download::probe_public(&conn, &u, args).await?;
    if probe.status >= 400 {
        return Err(format!(
            "the server answered {} for {url}",
            hya_net::describe_status(probe.status)
        ));
    }
    let total = probe.size;
    if total == 0 {
        return Err(format!(
            "the server states no size for {url}, so its tail cannot be read"
        ));
    }
    let name = probe
        .suggested_filename()
        .unwrap_or_else(|| url.suggested_filename());

    let px = crate::download::proxy_for_public(&url, args.proxy.as_deref(), args.no_proxy);
    let target = url
        .to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p)))?
        .with_headers(args.headers.clone(), Some(args.user_agent.clone()));
    let entries = match zipdir::fetch_listing(&conn, &target, total).await {
        Ok(e) => e,
        Err(PeekError::Zip(zipdir::Error::NotZip)) => {
            return Err(format!(
                "{name} is not a ZIP archive; --preview lists ZIP archives only"
            ));
        }
        Err(e) => return Err(format!("could not list {name}: {e}")),
    };
    print_table(&name, total, &entries);
    Ok(())
}

/// four columns, with the variable-width name last so the numbers
/// line up. Files only: a directory entry says nothing the paths of the
/// files inside it do not already say.
fn print_table(name: &str, total: u64, entries: &[Entry]) {
    let files: Vec<&Entry> = entries.iter().filter(|e| !e.is_dir()).collect();
    println!(
        "{name}, {} ({total} bytes), {} file{}",
        crate::stream::human(total),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    if files.is_empty() {
        return;
    }
    println!("{:>10}  {:>10}  {:<16}  Name", "Size", "Packed", "Modified");
    for e in files {
        // WinRAR's convention: an encrypted entry's name carries a trailing `*`.
        let mark = if e.encrypted { "*" } else { "" };
        println!(
            "{:>10}  {:>10}  {:<16}  {}{mark}",
            crate::stream::human(e.size),
            crate::stream::human(e.packed),
            e.modified.map(stamp).unwrap_or_default(),
            e.name,
        );
    }
}

/// `2026-09-04 13:27`, as the archiver's clock recorded it: ZIP stamps carry
/// no zone, so there is nothing to convert.
fn stamp(t: DosTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_iso_like() {
        let t = DosTime {
            year: 2026,
            month: 9,
            day: 4,
            hour: 13,
            minute: 27,
            second: 30,
        };
        assert_eq!(stamp(t), "2026-09-04 13:27");
    }
}

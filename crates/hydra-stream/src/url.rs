// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! URL parsing and resolution, sized for what a manifest needs.
//!
//! Deliberately not a general URL library: a playlist references its
//! segments with absolute URLs, protocol-relative ones, root-relative paths,
//! and plain relative paths, and every one of those has to resolve the way a
//! browser would. That is the whole requirement, and it is small enough to
//! own outright rather than take a dependency for.

/// The pieces a connection needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedUrl {
    pub tls: bool,
    /// `ftp://` — served by a single-connection fetcher, never by a manifest.
    pub ftp: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// Split an absolute `http`, `https` or `ftp` URL.
///
/// Errors are plain English so a caller with a translation catalogue can map
/// them; this crate has no opinion about presentation.
pub fn parse_url(url: &str) -> Result<ParsedUrl, String> {
    let url = url.trim();
    let (tls, ftp, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, false, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, false, r)
    } else if let Some(r) = url.strip_prefix("ftp://") {
        (false, true, r)
    } else {
        return Err("Only http://, https:// and ftp:// addresses are supported".into());
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // Strip userinfo if pasted in the address.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h, p.parse::<u16>().map_err(|_| "bad port".to_string())?)
        }
        _ => (
            authority,
            if ftp {
                21
            } else if tls {
                443
            } else {
                80
            },
        ),
    };
    if host.is_empty() {
        return Err("Address has no host name".into());
    }
    Ok(ParsedUrl {
        tls,
        ftp,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// Resolve `loc` against `base`, the way a browser resolves an `href`.
pub fn join_url(base: &ParsedUrl, loc: &str) -> Option<String> {
    let loc = loc.trim();
    if loc.is_empty() {
        return None;
    }
    let scheme = if base.tls { "https" } else { "http" };
    let lower = loc.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(loc.to_string());
    }
    if let Some(rest) = loc.strip_prefix("//") {
        return Some(format!("{scheme}://{rest}"));
    }
    if loc.starts_with('/') {
        return Some(format!("{scheme}://{}:{}{}", base.host, base.port, loc));
    }
    // A scheme this client has no transport for. Not an address to chase.
    if lower
        .split('/')
        .next()
        .map(|h| h.contains(':'))
        .unwrap_or(false)
    {
        return None;
    }
    let dir = base
        .path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or("");
    Some(format!("{scheme}://{}:{}{dir}/{loc}", base.host, base.port))
}

/// Resolve a manifest's URI against the URL the manifest came from, and
/// return it in the form the rest of the world spells it.
///
/// This is the one callers should use: it normalises away the default port
/// and any `.`/`..` in the path, both of which [`join_url`] leaves in place.
pub fn join(base: &str, uri: &str) -> Option<String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return Some(normalize(uri));
    }
    let parsed = parse_url(base).ok()?;
    join_url(&parsed, uri).map(|u| normalize(&strip_default_port(&u)))
}

/// `join_url` always spells the port. Segment URLs are compared against what
/// a browser reported and are shown in logs, so the default port comes back
/// off: `https://h:443/p` and `https://h/p` are the same object and should
/// not read as two.
fn strip_default_port(url: &str) -> String {
    for (scheme, port) in [("https://", ":443"), ("http://", ":80")] {
        if let Some(rest) = url.strip_prefix(scheme) {
            let end = rest.find('/').unwrap_or(rest.len());
            if let Some(host) = rest[..end].strip_suffix(port) {
                return format!("{scheme}{host}{}", &rest[end..]);
            }
        }
    }
    url.to_string()
}

/// Resolve `.` and `..` in the path. A DASH `<BaseURL>./</BaseURL>` is
/// idiomatic and joins to a literal `/./` that some origins answer with a
/// 404, and a `..` left in place is both wrong and a way for a manifest to
/// aim a request somewhere its own directory does not cover.
fn normalize(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after = scheme_end + 3;
    let rest = &url[after..];
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (head, tail) = url.split_at(after + auth_end);
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, suffix) = tail.split_at(path_end);
    if !path.contains("/.") {
        return url.to_string();
    }

    let trailing = path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    let mut joined = String::from(head);
    for p in &out {
        joined.push('/');
        joined.push_str(p);
    }
    if trailing || out.is_empty() {
        joined.push('/');
    }
    joined.push_str(suffix);
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_splits_scheme_host_port_and_path() {
        let u = parse_url("https://cdn.example/a/b.m3u8?x=1").unwrap();
        assert!(u.tls && !u.ftp);
        assert_eq!((u.host.as_str(), u.port), ("cdn.example", 443));
        assert_eq!(u.path, "/a/b.m3u8?x=1");

        assert_eq!(parse_url("http://h/p").unwrap().port, 80);
        assert_eq!(parse_url("ftp://h/p").unwrap().port, 21);
        assert_eq!(parse_url("https://h:8443/p").unwrap().port, 8443);
        // Userinfo is not part of the address to connect to.
        assert_eq!(parse_url("https://u:pw@h/p").unwrap().host, "h");
        // A bare host has a root path.
        assert_eq!(parse_url("https://h").unwrap().path, "/");
        // The fragment is a client-side concern.
        assert_eq!(parse_url("https://h/p#frag").unwrap().path, "/p");

        assert!(parse_url("ws://h/p").is_err());
        assert!(parse_url("https:///p").is_err());
    }

    #[test]
    fn joining_resolves_every_form_a_manifest_uses() {
        let base = "https://cdn.example/hls/deep/index.m3u8";
        // Relative to the manifest's directory.
        assert_eq!(
            join(base, "seg1.ts").unwrap(),
            "https://cdn.example/hls/deep/seg1.ts"
        );
        // Root-relative.
        assert_eq!(
            join(base, "/top/seg.ts").unwrap(),
            "https://cdn.example/top/seg.ts"
        );
        // Protocol-relative.
        assert_eq!(
            join(base, "//other.example/s.ts").unwrap(),
            "https://other.example/s.ts"
        );
        // Absolute.
        assert_eq!(
            join(base, "https://x.example/s.ts").unwrap(),
            "https://x.example/s.ts"
        );
        // Dot segments resolve out.
        assert_eq!(
            join(base, "./s.ts").unwrap(),
            "https://cdn.example/hls/deep/s.ts"
        );
        assert_eq!(
            join(base, "../s.ts").unwrap(),
            "https://cdn.example/hls/s.ts"
        );
        // `..` cannot climb above the root.
        assert_eq!(
            join(base, "../../../../s.ts").unwrap(),
            "https://cdn.example/s.ts"
        );
        // A non-default port is part of the address and stays.
        assert_eq!(
            join("https://cdn.example:8443/a/m.m3u8", "s.ts").unwrap(),
            "https://cdn.example:8443/a/s.ts"
        );
        // A query string is not a path and is left alone.
        assert_eq!(
            join(base, "s.ts?t=./1&u=2").unwrap(),
            "https://cdn.example/hls/deep/s.ts?t=./1&u=2"
        );
        assert_eq!(join(base, "   "), None);
    }

    #[test]
    fn an_unsupported_scheme_is_not_chased() {
        let base = parse_url("https://cdn.example/a/m.m3u8").unwrap();
        assert_eq!(join_url(&base, "mailto:someone@example.com"), None);
        assert_eq!(join_url(&base, "data:text/plain,x"), None);
    }
}

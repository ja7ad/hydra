// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! RFC 6265 cookie storage, owned by one transfer.
//!
//! A [`CookieJar`] is consulted once per request head and updated from every
//! `Set-Cookie` the server answers with. That is all it takes to make a
//! login-gated redirect chain work: the first hop hands out a session cookie,
//! the jar stores it, and the hop that follows carries it back — which is the
//! case a bare `-H 'Cookie: …'` cannot express, because the header is fixed
//! before the chain starts.
//!
//! # The security boundary
//!
//! **A cookie never crosses an origin.** [`CookieJar::header_value`] selects by
//! the host of the request about to be sent, so a redirect from `evil.test` to
//! `bank.test` carries `bank.test`'s cookies and nothing else — there is no
//! path through this module by which a cookie set on one host reaches another.
//! That is structural rather than a check: the selector takes the destination
//! host as its argument and has no memory of where the chain has been.
//!
//! Two narrower rules hold the rest of the boundary:
//!
//! * A `Domain` attribute may only widen a cookie to a domain the setting host
//!   is itself under, and never to a [public suffix](is_public_suffix) — so
//!   `Set-Cookie: …; Domain=.co.uk` from `shop.co.uk` is dropped rather than
//!   handed to every other `.co.uk`.
//! * A `Secure` cookie is withheld from a plaintext request.
//!
//! The jar is per job. Two downloads running at once against different hosts
//! hold separate jars, so neither can observe the other's session.
//!
//! # What is deliberately not here
//!
//! `SameSite` is accepted and discarded rather than stored. Every request a
//! downloader makes is a top-level navigation to the URL the user named, so no
//! value of the attribute would change which cookies are sent; and the
//! Netscape interchange format has no column to write it back to. A field
//! nothing reads and nothing can persist is weight, not preservation.
//!
//! `HttpOnly` is stored, because [`netscape`] round-trips it and other tools
//! read it — but it does not gate anything here either. It bars scripts, and
//! this is not one.

pub mod browser;
mod chromium;
pub mod netscape;
mod safari;
mod sqlite;

use std::net::IpAddr;

/// One stored cookie.
///
/// Fields mirror RFC 6265 §5.3's storage model, with `expires` collapsing
/// `Max-Age` and `Expires` to the single instant the spec says they mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Canonicalized: lowercase, no leading dot, no trailing dot.
    pub domain: String,
    /// The response carried no `Domain`, so the cookie goes back only to the
    /// exact host that set it. RFC 6265 §5.3 step 6.
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Unix seconds at which the cookie stops being sent. `None` is a session
    /// cookie: it lives as long as the jar does and is not persisted unless
    /// `--keep-session-cookies` asks.
    pub expires: Option<u64>,
}

impl Cookie {
    /// A host-only cookie for `domain`, rooted at `/` and lasting the session.
    ///
    /// The shape a `--cookie 'name=value'` pair takes: the user named a host on
    /// the command line and no attributes, so the narrowest reading of what
    /// they asked for is the right one.
    pub fn new(name: &str, value: &str, domain: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
            domain: canonical_host(domain),
            host_only: true,
            path: "/".to_string(),
            secure: false,
            http_only: false,
            expires: None,
        }
    }

    /// The cookie has no expiry and dies with the process.
    pub fn is_session(&self) -> bool {
        self.expires.is_none()
    }

    /// This cookie may be sent on a request for `host` `path` at `now`.
    ///
    /// `secure` is whether the request goes over TLS, not a property of the
    /// cookie: a `Secure` cookie put on the wire in plaintext is the leak the
    /// attribute exists to prevent.
    fn matches(&self, host: &str, path: &str, secure: bool, now: u64) -> bool {
        if self.secure && !secure {
            return false;
        }
        if self.expires.is_some_and(|e| e <= now) {
            return false;
        }
        let domain_ok = if self.host_only {
            host == self.domain
        } else {
            domain_matches(host, &self.domain)
        };
        domain_ok && path_matches(path, &self.path)
    }
}

/// Cookies held for the life of one transfer.
///
/// Ordered by insertion, which is also creation order: RFC 6265 §5.4 breaks a
/// tie between two cookies of equal path length by which was created first, and
/// §5.3 step 11 says replacing a cookie keeps the original's creation time. A
/// `Vec` that replaces in place satisfies both without storing a timestamp.
///
/// A `Vec` rather than a map because a jar scoped to one job holds tens of
/// cookies at the very most, and the request path wants them in order anyway.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// Every cookie held, in creation order.
    pub fn iter(&self) -> impl Iterator<Item = &Cookie> {
        self.cookies.iter()
    }

    /// Store `c`, replacing any cookie with the same name, domain and path.
    ///
    /// The replacement keeps the original's POSITION, because position is what
    /// this jar uses for creation order (RFC 6265 §5.3 step 11).
    pub fn insert(&mut self, c: Cookie) {
        match self
            .cookies
            .iter_mut()
            .find(|e| e.name == c.name && e.domain == c.domain && e.path == c.path)
        {
            Some(slot) => *slot = c,
            None => self.cookies.push(c),
        }
    }

    /// The `Cookie:` header value for a request, or `None` when nothing matches.
    ///
    /// `host` is the host the request is ABOUT TO BE SENT TO. Passing the host
    /// of an earlier hop is what would leak a session across an origin, so
    /// callers following a redirect must call this again with the new host
    /// rather than reusing the header they built.
    ///
    /// `path` may carry a query string; it is cut at the first `?` or `#`.
    pub fn header_value(&self, host: &str, path: &str, secure: bool, now: u64) -> Option<String> {
        let host = canonical_host(host);
        let path = request_path(path);
        // Stable sort by descending path length: §5.4 orders longer paths
        // first, and a stable sort leaves equal-length cookies in creation
        // order, which is exactly the tie-break the section names.
        let mut hits: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.matches(&host, &path, secure, now))
            .collect();
        hits.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        if hits.is_empty() {
            return None;
        }
        let mut out = String::new();
        for c in hits {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&c.name);
            out.push('=');
            out.push_str(&c.value);
        }
        Some(out)
    }

    /// Apply every `Set-Cookie` in a raw response head, and report how many
    /// were accepted.
    ///
    /// `host` and `path` describe the request that produced the response, which
    /// is what the domain and path defaults are taken from. A `Set-Cookie` the
    /// requesting host is not allowed to set is dropped silently: it is a
    /// server's business, not a transfer error.
    pub fn store_response(&mut self, head: &str, host: &str, path: &str, now: u64) -> usize {
        let mut n = 0;
        for line in crate::http::header_all(head, "set-cookie") {
            if let Some(c) = parse_set_cookie(&line, host, path, now) {
                self.insert(c);
                n += 1;
            }
        }
        n
    }

    /// Add the pairs of a literal `name=value; name2=value2` string as
    /// host-only session cookies for `host` (curl's `-b`, wget's `--header`
    /// replacement).
    ///
    /// Host-only deliberately: the user named one address, and a pair with no
    /// attributes carries no permission to widen past it. Returns the number
    /// of pairs stored.
    pub fn add_pairs(&mut self, s: &str, host: &str) -> usize {
        let mut n = 0;
        for pair in s.split(';') {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name.is_empty() {
                continue;
            }
            self.insert(Cookie::new(name, value, host));
            n += 1;
        }
        n
    }

    /// Drop everything that could not be sent to `host`.
    ///
    /// What scopes a browser import: the whole profile is read, this keeps the
    /// handful of cookies for the host being downloaded from, and the rest is
    /// dropped before anything is written anywhere.
    pub fn retain_for_host(&mut self, host: &str) {
        let host = canonical_host(host);
        self.cookies.retain(|c| {
            if c.host_only {
                host == c.domain
            } else {
                domain_matches(&host, &c.domain)
            }
        });
    }

    /// Drop cookies whose expiry has passed, and session cookies too when
    /// `session` is set (wget's `--junk-session-cookies`).
    pub fn purge(&mut self, now: u64, session: bool) {
        self.cookies
            .retain(|c| !(c.expires.is_some_and(|e| e <= now) || (session && c.is_session())));
    }

    /// Merge `other` into this jar, `other` winning on a collision.
    pub fn extend(&mut self, other: CookieJar) {
        for c in other.cookies {
            self.insert(c);
        }
    }
}

impl FromIterator<Cookie> for CookieJar {
    fn from_iter<I: IntoIterator<Item = Cookie>>(iter: I) -> Self {
        let mut jar = Self::new();
        for c in iter {
            jar.insert(c);
        }
        jar
    }
}

/// Parse one `Set-Cookie` field value against the request that drew it.
///
/// Returns `None` when the line is malformed or when the requesting host is not
/// allowed to set it — a `Domain` naming another site or a public suffix, which
/// RFC 6265 §5.3 step 6 and the [suffix guard](is_public_suffix) reject.
pub fn parse_set_cookie(line: &str, req_host: &str, req_path: &str, now: u64) -> Option<Cookie> {
    let mut parts = line.split(';');
    let nv = parts.next()?.trim();
    // A value may be empty (`Set-Cookie: a=`), a name may not.
    let (name, value) = nv.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let host = canonical_host(req_host);

    let mut c = Cookie {
        name: name.to_string(),
        value: value.trim().trim_matches('"').to_string(),
        domain: host.clone(),
        host_only: true,
        path: default_path(req_path),
        secure: false,
        http_only: false,
        expires: None,
    };

    // `Max-Age` wins over `Expires` (§5.2.2), whatever order they arrive in,
    // so it is tracked separately and applied last.
    let mut max_age: Option<i64> = None;
    for attr in parts {
        let (k, v) = match attr.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attr.trim(), ""),
        };
        match k.to_ascii_lowercase().as_str() {
            "domain" => {
                let d = canonical_host(v);
                if d.is_empty() {
                    continue;
                }
                // §5.3 step 6: the host must be under the domain it is
                // widening to, and the domain must not be a registry.
                if is_public_suffix(&d) || !domain_matches(&host, &d) {
                    return None;
                }
                c.domain = d;
                c.host_only = false;
            }
            "path" if v.starts_with('/') => c.path = v.to_string(),
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            "max-age" => max_age = v.parse().ok(),
            "expires" => c.expires = crate::polite::parse_http_date(v),
            // `SameSite` lands here: accepted so it cannot break the parse,
            // discarded because nothing in a downloader reads it.
            _ => {}
        }
    }
    if let Some(age) = max_age {
        // A non-positive Max-Age means "expire now" (§5.2.2). Saturating so a
        // server's `Max-Age: 99999999999999` cannot wrap the clock.
        c.expires = Some(if age <= 0 {
            0
        } else {
            now.saturating_add(age as u64)
        });
    }
    Some(c)
}

/// Lowercase, with the leading dot of a `Domain` attribute and any root dot
/// removed, and an IPv6 literal's brackets stripped.
pub fn canonical_host(h: &str) -> String {
    h.trim()
        .trim_start_matches('.')
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
}

/// RFC 6265 §5.1.3 domain matching: identical, or a subdomain of it.
///
/// An IP literal matches only itself. Without that test `127.0.0.1` would be
/// read as a subdomain of `0.0.1`.
pub fn domain_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

/// RFC 6265 §5.1.4 path matching.
fn path_matches(request: &str, cookie: &str) -> bool {
    if request == cookie {
        return true;
    }
    if !request.starts_with(cookie) {
        return false;
    }
    cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/')
}

/// The request path with any query or fragment cut off, defaulting to `/`.
fn request_path(p: &str) -> String {
    let p = p.split(['?', '#']).next().unwrap_or("");
    if p.starts_with('/') {
        p.to_string()
    } else {
        "/".to_string()
    }
}

/// RFC 6265 §5.1.4 default-path: the request path up to its last `/`.
fn default_path(req_path: &str) -> String {
    let p = request_path(req_path);
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

/// Registry labels that make a two-label domain a public suffix.
///
/// Consulted only for a domain of exactly two labels, which is why a list this
/// size is enough: `example.co.uk` has three and is registrable whatever is in
/// here.
const REGISTRY_LABELS: &[&str] = &[
    "ac",
    "adm",
    "adv",
    "agro",
    "arts",
    "asn",
    "biz",
    "cc",
    "cem",
    "cng",
    "cnt",
    "co",
    "com",
    "coop",
    "cri",
    "ecn",
    "edu",
    "eng",
    "ernet",
    "esp",
    "etc",
    "eti",
    "firm",
    "fm",
    "fot",
    "fst",
    "g12",
    "geek",
    "gen",
    "go",
    "gob",
    "gov",
    "govt",
    "gr",
    "health",
    "hotel",
    "id",
    "idv",
    "ind",
    "inf",
    "info",
    "int",
    "iwi",
    "jor",
    "jus",
    "k12",
    "kiwi",
    "lel",
    "lg",
    "ltd",
    "mat",
    "med",
    "mil",
    "mod",
    "muni",
    "name",
    "nb",
    "ne",
    "net",
    "nhs",
    "nic",
    "nom",
    "not",
    "ntr",
    "odo",
    "off",
    "on",
    "or",
    "org",
    "parliament",
    "plc",
    "ppg",
    "presse",
    "priv",
    "pro",
    "psc",
    "psi",
    "qsl",
    "rec",
    "res",
    "sch",
    "school",
    "sci",
    "slg",
    "soc",
    "srv",
    "store",
    "tel",
    "tm",
    "tmp",
    "trd",
    "tur",
    "tv",
    "vet",
    "web",
    "zlg",
];

/// The domain is a registry under which unrelated organisations are
/// registered, so no site may set a cookie for it.
///
/// **Not the full Public Suffix List.** Shipping and refreshing ten thousand
/// rules to answer the one question asked here — "is this attribute broad
/// enough to reach a stranger's site?" — costs more than it buys for a jar
/// owned by a single transfer. Two rules stand in:
///
/// * any single-label domain is a suffix (`com`, a bare TLD, `localhost`);
/// * a two-label domain whose first label is a registry label is a suffix
///   (`co.uk`, `com.au`, `ne.jp`).
///
/// **The narrowing this leaves.** A deeper registry the PSL knows about and
/// this does not — `pvt.k12.ma.us`, `s3.amazonaws.com` — reads as registrable,
/// so a site under one could set a cookie its siblings would match. The reach
/// of that is bounded by everything else in this module: the jar belongs to one
/// transfer, so the only way to exercise it is a redirect from the attacker's
/// own host to a sibling of it, which is the same exposure as the
/// `-H 'Cookie: …'` this feature replaces and strictly less than handing over a
/// browser profile's whole jar. It errs toward refusing: `co.com` is
/// registrable in reality and is rejected here, costing a cookie rather than
/// leaking one.
pub fn is_public_suffix(domain: &str) -> bool {
    let d = canonical_host(domain);
    if d.is_empty() || d.parse::<IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = d.split('.').collect();
    match labels.len() {
        0 | 1 => true,
        2 => REGISTRY_LABELS.contains(&labels[0]),
        _ => false,
    }
}

/// Seconds since the Unix epoch, for callers with no clock of their own.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;

    fn head(lines: &[&str]) -> String {
        let mut s = String::from("HTTP/1.1 302 Found\r\n");
        for l in lines {
            s.push_str(l);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        s
    }

    #[test]
    fn a_cookie_set_on_one_host_goes_back_to_that_host() {
        let mut jar = CookieJar::new();
        assert_eq!(
            jar.store_response(
                &head(&["Set-Cookie: sid=abc"]),
                "files.example.org",
                "/a/b",
                NOW
            ),
            1
        );
        assert_eq!(
            jar.header_value("files.example.org", "/a/b", true, NOW)
                .as_deref(),
            Some("sid=abc")
        );
    }

    /// The whole point of the jar: a login that answers `Set-Cookie` + `302`
    /// expects the cookie back on the next hop, which a fixed `-H 'Cookie: …'`
    /// cannot do.
    #[test]
    fn a_redirect_within_the_domain_carries_the_cookie_it_was_handed() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: sid=abc; Domain=example.org; Path=/"]),
            "login.example.org",
            "/auth",
            NOW,
        );
        assert_eq!(
            jar.header_value("cdn.example.org", "/files/big.iso", true, NOW)
                .as_deref(),
            Some("sid=abc")
        );
    }

    #[test]
    fn a_cookie_never_crosses_an_origin_on_redirect() {
        let mut jar = CookieJar::new();
        jar.store_response(&head(&["Set-Cookie: sid=abc"]), "evil.test", "/", NOW);
        assert_eq!(jar.header_value("bank.test", "/", true, NOW), None);
        assert_eq!(
            jar.header_value("evil.test.attacker.test", "/", true, NOW),
            None
        );
        // Nor to a parent of the host that set it, without a Domain attribute.
        assert_eq!(jar.header_value("test", "/", true, NOW), None);
    }

    #[test]
    fn a_domain_attribute_may_not_name_a_public_suffix_or_a_stranger() {
        let mut jar = CookieJar::new();
        for line in [
            "Set-Cookie: a=1; Domain=.co.uk",
            "Set-Cookie: b=2; Domain=uk",
            "Set-Cookie: c=3; Domain=other.test",
            "Set-Cookie: d=4; Domain=hop.shop.co.uk",
        ] {
            jar.store_response(&head(&[line]), "shop.co.uk", "/", NOW);
        }
        assert!(jar.is_empty(), "{jar:?}");

        // The same host may widen to its own registrable domain.
        jar.store_response(
            &head(&["Set-Cookie: ok=5; Domain=shop.co.uk"]),
            "www.shop.co.uk",
            "/",
            NOW,
        );
        assert_eq!(
            jar.header_value("img.shop.co.uk", "/", true, NOW)
                .as_deref(),
            Some("ok=5")
        );
    }

    #[test]
    fn a_secure_cookie_is_withheld_from_a_plaintext_request() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: sid=abc; Secure"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(jar.header_value("example.org", "/", false, NOW), None);
        assert!(jar.header_value("example.org", "/", true, NOW).is_some());
    }

    #[test]
    fn path_scope_follows_rfc_6265_prefix_rules() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: p=1; Path=/files"]),
            "example.org",
            "/",
            NOW,
        );
        for (path, want) in [
            ("/files", true),
            ("/files/", true),
            ("/files/big.iso", true),
            ("/filestore", false),
            ("/", false),
        ] {
            assert_eq!(
                jar.header_value("example.org", path, true, NOW).is_some(),
                want,
                "{path}"
            );
        }
    }

    #[test]
    fn default_path_is_the_request_path_up_to_its_last_slash() {
        for (req, want) in [
            ("/a/b/c", "/a/b"),
            ("/a", "/"),
            ("/", "/"),
            ("/a/b/", "/a/b"),
            ("/a/b?q=1", "/a"),
            ("", "/"),
        ] {
            assert_eq!(default_path(req), want, "{req}");
        }
    }

    #[test]
    fn a_query_string_does_not_join_the_path() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: p=1"]),
            "example.org",
            "/files/x?a=/deep",
            NOW,
        );
        assert_eq!(jar.iter().next().unwrap().path, "/files");
        assert!(jar
            .header_value("example.org", "/files/y?z=1", true, NOW)
            .is_some());
    }

    /// RFC 6265 §5.4: longer paths first, then creation order.
    #[test]
    fn header_orders_by_path_length_then_creation() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&[
                "Set-Cookie: root=1; Path=/",
                "Set-Cookie: first=2; Path=/a/b",
                "Set-Cookie: second=3; Path=/a/b",
            ]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(
            jar.header_value("example.org", "/a/b", true, NOW)
                .as_deref(),
            Some("first=2; second=3; root=1")
        );
    }

    #[test]
    fn replacing_a_cookie_keeps_its_place_in_the_order() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: a=1", "Set-Cookie: b=2"]),
            "example.org",
            "/",
            NOW,
        );
        jar.store_response(&head(&["Set-Cookie: a=9"]), "example.org", "/", NOW);
        assert_eq!(
            jar.header_value("example.org", "/", true, NOW).as_deref(),
            Some("a=9; b=2")
        );
        assert_eq!(jar.len(), 2);
    }

    #[test]
    fn max_age_beats_expires_whatever_order_they_arrive_in() {
        let far = "Expires=Sat, 01 Jan 2033 00:00:00 GMT";
        for line in [
            format!("Set-Cookie: a=1; Max-Age=60; {far}"),
            format!("Set-Cookie: a=1; {far}; Max-Age=60"),
        ] {
            let mut jar = CookieJar::new();
            jar.store_response(&head(&[&line]), "example.org", "/", NOW);
            assert_eq!(jar.iter().next().unwrap().expires, Some(NOW + 60), "{line}");
        }
    }

    #[test]
    fn a_non_positive_max_age_expires_the_cookie_immediately() {
        let mut jar = CookieJar::new();
        jar.store_response(&head(&["Set-Cookie: a=1"]), "example.org", "/", NOW);
        jar.store_response(
            &head(&["Set-Cookie: a=1; Max-Age=0"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(jar.header_value("example.org", "/", true, NOW), None);
    }

    #[test]
    fn an_absurd_max_age_does_not_wrap_the_clock() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: a=1; Max-Age=9223372036854775807"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(
            jar.iter().next().unwrap().expires,
            Some(NOW + i64::MAX as u64)
        );
        assert!(jar.header_value("example.org", "/", true, NOW).is_some());
        // Larger than an i64 is not a number the attribute can hold, so the
        // cookie keeps the expiry it already had rather than gaining one.
        jar.store_response(
            &head(&["Set-Cookie: b=1; Max-Age=99999999999999999999"]),
            "example.org",
            "/",
            NOW,
        );
        assert!(jar.iter().any(|c| c.name == "b" && c.is_session()));
    }

    #[test]
    fn an_expired_cookie_is_not_sent_and_purge_drops_it() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: a=1; Expires=Sun, 06 Nov 1994 08:49:37 GMT"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(jar.len(), 1);
        assert_eq!(jar.header_value("example.org", "/", true, NOW), None);
        jar.purge(NOW, false);
        assert!(jar.is_empty());
    }

    #[test]
    fn a_pre_epoch_expires_deletes_the_cookie() {
        let mut jar = CookieJar::new();
        jar.store_response(&head(&["Set-Cookie: id=1"]), "example.org", "/", NOW);
        assert_eq!(
            jar.header_value("example.org", "/", true, NOW).as_deref(),
            Some("id=1")
        );
        jar.store_response(
            &head(&["Set-Cookie: id=; expires=Mon, 01 Jan 1900 00:00:00 GMT"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(jar.header_value("example.org", "/", true, NOW), None);
        jar.purge(NOW, false);
        assert!(jar.iter().all(|c| c.name != "id"));
    }

    #[test]
    fn purge_drops_session_cookies_only_when_asked() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: s=1", "Set-Cookie: p=2; Max-Age=600"]),
            "example.org",
            "/",
            NOW,
        );
        jar.purge(NOW, false);
        assert_eq!(jar.len(), 2);
        jar.purge(NOW, true);
        assert_eq!(jar.len(), 1);
        assert_eq!(jar.iter().next().unwrap().name, "p");
    }

    #[test]
    fn every_set_cookie_in_a_head_is_stored() {
        let mut jar = CookieJar::new();
        let n = jar.store_response(
            &head(&[
                "Content-Type: text/html",
                "Set-Cookie: a=1",
                "Location: /next",
                "set-cookie: b=2",
                "SET-COOKIE: c=3",
            ]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(n, 3);
        assert_eq!(
            jar.header_value("example.org", "/", true, NOW).as_deref(),
            Some("a=1; b=2; c=3")
        );
    }

    #[test]
    fn a_malformed_set_cookie_is_dropped_without_disturbing_the_rest() {
        let mut jar = CookieJar::new();
        let n = jar.store_response(
            &head(&["Set-Cookie: novalue", "Set-Cookie: =1", "Set-Cookie: ok=2"]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(n, 1);
        assert_eq!(jar.iter().next().unwrap().name, "ok");
    }

    #[test]
    fn an_empty_value_is_a_cookie_and_quotes_are_not_part_of_it() {
        let mut jar = CookieJar::new();
        jar.store_response(
            &head(&["Set-Cookie: a=", "Set-Cookie: b=\"quoted\""]),
            "example.org",
            "/",
            NOW,
        );
        assert_eq!(
            jar.header_value("example.org", "/", true, NOW).as_deref(),
            Some("a=; b=quoted")
        );
    }

    #[test]
    fn literal_pairs_are_host_only_for_the_host_named() {
        let mut jar = CookieJar::new();
        assert_eq!(
            jar.add_pairs("session=abc; csrf=def ; junk", "Example.ORG"),
            2
        );
        assert_eq!(
            jar.header_value("example.org", "/", false, NOW).as_deref(),
            Some("session=abc; csrf=def")
        );
        assert_eq!(jar.header_value("sub.example.org", "/", false, NOW), None);
    }

    #[test]
    fn retain_for_host_keeps_only_what_that_host_would_receive() {
        let mut jar = CookieJar::new();
        jar.store_response(&head(&["Set-Cookie: a=1"]), "files.example.org", "/", NOW);
        jar.store_response(
            &head(&["Set-Cookie: b=2; Domain=example.org"]),
            "www.example.org",
            "/",
            NOW,
        );
        jar.store_response(&head(&["Set-Cookie: c=3"]), "other.test", "/", NOW);
        jar.retain_for_host("files.example.org");
        assert_eq!(
            jar.header_value("files.example.org", "/", true, NOW)
                .as_deref(),
            Some("a=1; b=2")
        );
    }

    #[test]
    fn an_ip_literal_host_matches_only_itself() {
        assert!(domain_matches("127.0.0.1", "127.0.0.1"));
        assert!(!domain_matches("127.0.0.1", "0.0.1"));
        assert!(!domain_matches("10.0.0.1", "0.1"));
        assert!(domain_matches("a.example.org", "example.org"));
        assert!(!domain_matches("notexample.org", "example.org"));
        assert!(!domain_matches("example.org", "a.example.org"));
    }

    #[test]
    fn public_suffixes_are_recognised_at_one_and_two_labels() {
        for d in ["com", "uk", "co.uk", "com.au", "ne.jp", "localhost"] {
            assert!(is_public_suffix(d), "{d}");
        }
        for d in ["example.com", "example.co.uk", "a.b.c", "127.0.0.1"] {
            assert!(!is_public_suffix(d), "{d}");
        }
    }

    #[test]
    fn hosts_canonicalize_to_one_spelling() {
        for (raw, want) in [
            (".Example.ORG.", "example.org"),
            ("[::1]", "::1"),
            (" example.org ", "example.org"),
        ] {
            assert_eq!(canonical_host(raw), want, "{raw}");
        }
    }

    #[test]
    fn extend_lets_the_incoming_jar_win() {
        let mut a = CookieJar::new();
        a.add_pairs("k=old; keep=1", "example.org");
        let mut b = CookieJar::new();
        b.add_pairs("k=new", "example.org");
        a.extend(b);
        assert_eq!(
            a.header_value("example.org", "/", false, NOW).as_deref(),
            Some("k=new; keep=1")
        );
    }
}

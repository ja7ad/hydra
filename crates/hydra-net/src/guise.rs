// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Getting a request past an origin that refuses it for how it is dressed.
//!
//! File hosts and CDNs gate downloads on how a request presents itself: a
//! `Referer` from their own site, a current browser's `User-Agent` and
//! header set — or the opposite, refusing anything that names a browser but
//! does not handshake like one. A request on the wrong side is answered
//! `403` (some say `401`) although the link itself is good.
//! [`probe_admitted`] asks again each other way and reports the [`Guise`]
//! the origin accepted, so every later request of the transfer can wear it.

use std::future::Future;

use crate::http::{has_browser_profile, header_value, Probe};
use crate::{is_field, Target, DEFAULT_USER_AGENT};

/// The browser a refused request is retried as. A Chrome `User-Agent`, so
/// the request also carries Chrome's header set.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36";

/// What was changed about a request before the origin served it. The default
/// changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Guise {
    /// `Referer` to send, for a request that carried none.
    pub referer: Option<String>,
    /// `User-Agent` to send in place of the request's own.
    pub user_agent: Option<String>,
    /// Send no `Referer` at all, as a plain download tool does.
    pub no_referer: bool,
}

impl Guise {
    /// Take on what `later` changed, keeping the rest.
    pub fn absorb(&mut self, later: Guise) {
        if later.referer.is_some() || later.no_referer {
            self.referer = later.referer;
            self.no_referer = later.no_referer;
        }
        if later.user_agent.is_some() {
            self.user_agent = later.user_agent;
        }
    }
}

impl Target {
    /// This target, sending what `guise` changed.
    pub fn in_guise(mut self, guise: &Guise) -> Self {
        if guise.no_referer || guise.referer.is_some() {
            self.headers.retain(|h| !is_field(h, "referer"));
        }
        if let Some(referer) = &guise.referer {
            self.headers.push(format!("Referer: {referer}"));
        }
        if let Some(agent) = &guise.user_agent {
            self.agent = Some(agent.clone());
        }
        self
    }

    /// The root of this target's origin, as a link on its own site names it.
    fn origin_root(&self) -> String {
        let (host, port) = self.origin_endpoint();
        let scheme = if self.tls { "https" } else { "http" };
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        if port == if self.tls { 443 } else { 80 } {
            format!("{scheme}://{host}/")
        } else {
            format!("{scheme}://{host}:{port}/")
        }
    }
}

/// The origin turned the request away for what it looked like, not for what
/// it asked for. A `401` that names a scheme wants a login, which no guise is.
fn gated(p: &Probe) -> bool {
    match p.status {
        403 => true,
        401 => header_value(&p.raw_head, "www-authenticate").is_none(),
        _ => false,
    }
}

/// The guises worth trying for `t`, cheapest first. A `Referer` the caller
/// gave is never replaced by another, only left out when asking as a tool.
///
/// A request naming a browser is retried under this client's own name before
/// a current browser's: bot filters (Cloudflare in front of SourceForge)
/// refuse a browser `User-Agent` whose TLS handshake is not that browser's
/// and serve a download tool that says what it is.
fn guises(t: &Target) -> Vec<Guise> {
    let referer = (!t.headers.iter().any(|h| is_field(h, "referer"))).then(|| t.origin_root());
    let names_a_browser = t
        .agent
        .as_deref()
        .is_some_and(|a| a.starts_with("Mozilla/"));
    let mut out = Vec::new();
    if referer.is_some() {
        out.push(Guise {
            referer: referer.clone(),
            ..Guise::default()
        });
    }
    if names_a_browser {
        // Without any referer: the same hosts answer a tool that sends one
        // with their landing page instead of the file.
        out.push(Guise {
            user_agent: Some(DEFAULT_USER_AGENT.to_string()),
            no_referer: true,
            ..Guise::default()
        });
    }
    if !has_browser_profile(t) {
        out.push(Guise {
            referer,
            user_agent: Some(BROWSER_USER_AGENT.to_string()),
            ..Guise::default()
        });
    }
    out
}

/// Probe `t` with `ask`, and when the origin refuses the request as written,
/// ask again in each guise a browser would have had until one is served.
///
/// Returns the answer with the guise that earned it; apply that guise to
/// every later request for the same object. When no guise is served the first
/// answer is returned, because it is the one about the request as the caller
/// wrote it.
pub async fn probe_admitted<F, Fut, E>(t: &Target, ask: F) -> Result<(Probe, Guise), E>
where
    F: Fn(Target) -> Fut,
    Fut: Future<Output = Result<Probe, E>>,
{
    let first = ask(t.clone()).await?;
    if !gated(&first) {
        return Ok((first, Guise::default()));
    }
    for guise in guises(t) {
        if let Ok(p) = ask(t.clone().in_guise(&guise)).await {
            if p.is_redirect() || p.refusal().is_none() {
                return Ok((p, guise));
            }
        }
    }
    Ok((first, Guise::default()))
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use super::*;

    fn answered(status: u16, extra: &str) -> Probe {
        Probe {
            status,
            raw_head: format!("HTTP/1.1 {status} X\r\n{extra}\r\n"),
            ..Probe::default()
        }
    }

    /// An origin that serves a request only when `admits` says so, and
    /// answers `refusal` otherwise. Returns the answer and every request made.
    async fn ask_origin(
        t: &Target,
        refusal: Probe,
        admits: impl Fn(&Target) -> bool,
    ) -> ((Probe, Guise), Vec<Target>) {
        let seen = Mutex::new(Vec::new());
        let answer = probe_admitted(t, |t| {
            let p = if admits(&t) {
                answered(200, "")
            } else {
                refusal.clone()
            };
            seen.lock().unwrap().push(t);
            async move { Ok::<_, io::Error>(p) }
        })
        .await
        .unwrap();
        (answer, seen.into_inner().unwrap())
    }

    fn referer(t: &Target) -> Option<&str> {
        t.headers
            .iter()
            .find(|h| is_field(h, "referer"))
            .map(|h| h["referer:".len()..].trim())
    }

    fn pasted() -> Target {
        Target::direct_tls("v46.cdn.example", 443, "/7232/a_720p.mp4")
            .with_headers(Vec::new(), Some("hydra/1.0".into()))
    }

    #[tokio::test]
    async fn a_request_that_is_served_is_asked_once_and_left_as_written() {
        let ((p, guise), seen) = ask_origin(&pasted(), answered(403, ""), |_| true).await;
        assert_eq!(p.status, 200);
        assert_eq!(guise, Guise::default());
        assert_eq!(seen.len(), 1);
    }

    #[tokio::test]
    async fn a_hotlink_refusal_is_asked_again_as_linked_from_the_origins_own_site() {
        let ((p, guise), seen) = ask_origin(&pasted(), answered(403, ""), |t| {
            referer(t) == Some("https://v46.cdn.example/")
        })
        .await;
        assert_eq!(p.status, 200);
        assert_eq!(
            guise,
            Guise {
                referer: Some("https://v46.cdn.example/".into()),
                ..Guise::default()
            }
        );
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].agent.as_deref(), Some("hydra/1.0"));
    }

    #[tokio::test]
    async fn an_origin_that_serves_only_browsers_is_asked_as_one() {
        let ((p, guise), seen) = ask_origin(&pasted(), answered(403, ""), |t| {
            t.agent.as_deref().is_some_and(|a| a.contains("Chrome/"))
        })
        .await;
        assert_eq!(p.status, 200);
        assert_eq!(guise.user_agent.as_deref(), Some(BROWSER_USER_AGENT));
        assert_eq!(guise.referer.as_deref(), Some("https://v46.cdn.example/"));
        assert_eq!(seen.len(), 3);
        assert!(has_browser_profile(&seen[2]));
    }

    #[tokio::test]
    async fn a_referer_the_caller_gave_is_never_replaced() {
        let t = pasted().with_headers(
            vec!["referer: https://www.example/album".into()],
            Some("hydra/1.0".into()),
        );
        let ((_, guise), seen) = ask_origin(&t, answered(403, ""), |t| {
            t.agent.as_deref() == Some(BROWSER_USER_AGENT)
        })
        .await;
        assert_eq!(guise.referer, None);
        assert!(seen
            .iter()
            .all(|t| matches!(referer(t), Some("https://www.example/album"))));
    }

    /// #277: SourceForge answered `403` to the configured browser
    /// `User-Agent` and serves `hydra/…`, `curl/…` and `Wget/…`.
    #[tokio::test]
    async fn a_request_refused_for_posing_as_a_browser_is_asked_under_its_own_name() {
        const POSED: &str = "Mozilla/5.0 (Windows NT 6.1; Trident/7.0; rv:11.0) like Gecko";
        let t = pasted().with_headers(Vec::new(), Some(POSED.into()));
        let ((p, guise), seen) = ask_origin(&t, answered(403, ""), |t| {
            t.agent
                .as_deref()
                .is_some_and(|a| !a.starts_with("Mozilla/"))
        })
        .await;
        assert_eq!(p.status, 200);
        assert_eq!(guise.user_agent.as_deref(), Some(DEFAULT_USER_AGENT));
        let agents: Vec<_> = seen.iter().map(|t| t.agent.as_deref().unwrap()).collect();
        assert_eq!(agents, [POSED, POSED, DEFAULT_USER_AGENT]);
        assert_eq!(referer(&seen[2]), None, "a plain tool sends no referer");
        assert_eq!(guise.referer, None);
    }

    #[tokio::test]
    async fn a_captured_browser_request_has_only_its_own_name_left_to_try() {
        let t = pasted().with_headers(
            vec!["Referer: https://www.example/".into()],
            Some(BROWSER_USER_AGENT.into()),
        );
        let ((p, guise), seen) = ask_origin(&t, answered(403, ""), |_| false).await;
        assert_eq!(p.status, 403);
        assert_eq!(guise, Guise::default());
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].agent.as_deref(), Some(DEFAULT_USER_AGENT));
        assert_eq!(referer(&seen[1]), None, "a plain tool sends no referer");
    }

    #[tokio::test]
    async fn the_asking_is_bounded_however_the_request_was_dressed() {
        let posed = pasted().with_headers(Vec::new(), Some("Mozilla/5.0 Firefox/130.0".into()));
        let (_, seen) = ask_origin(&posed, answered(403, ""), |_| false).await;
        assert_eq!(seen.len(), 4, "as written, referer, own name, browser");
    }

    #[tokio::test]
    async fn a_link_no_guise_opens_reports_the_answer_to_the_request_as_written() {
        let refusal = answered(403, "X-Reason: expired\r\n");
        let ((p, guise), seen) = ask_origin(&pasted(), refusal, |_| false).await;
        assert!(p.raw_head.contains("X-Reason: expired"));
        assert_eq!(guise, Guise::default());
        assert_eq!(seen.len(), 3, "one ask per guise and no more");
    }

    #[tokio::test]
    async fn a_bare_401_is_a_gate_and_a_401_that_wants_a_login_is_not() {
        let (_, bare) = ask_origin(&pasted(), answered(401, ""), |_| false).await;
        assert_eq!(bare.len(), 3);

        let challenge = answered(401, "WWW-Authenticate: Basic realm=\"files\"\r\n");
        let ((p, _), login) = ask_origin(&pasted(), challenge, |_| false).await;
        assert_eq!(p.status, 401);
        assert_eq!(login.len(), 1);
    }

    #[tokio::test]
    async fn a_missing_object_is_not_asked_for_again() {
        for status in [404, 410, 416, 500, 503] {
            let ((p, _), seen) = ask_origin(&pasted(), answered(status, ""), |_| false).await;
            assert_eq!(p.status, status);
            assert_eq!(seen.len(), 1, "{status}");
        }
    }

    #[tokio::test]
    async fn a_guise_that_earns_a_redirect_is_kept_for_the_hop_it_points_to() {
        let moved = Probe {
            location: Some("https://edge.example/f".into()),
            ..answered(302, "")
        };
        let asks = Mutex::new(0);
        let (p, guise) = probe_admitted(&pasted(), |_| {
            let mut n = asks.lock().unwrap();
            *n += 1;
            let p = if *n == 1 {
                answered(403, "")
            } else {
                moved.clone()
            };
            async move { Ok::<_, io::Error>(p) }
        })
        .await
        .unwrap();
        assert!(p.is_redirect());
        assert!(guise.referer.is_some());
    }

    #[tokio::test]
    async fn a_guise_that_cannot_be_asked_does_not_hide_the_refusal() {
        let asks = Mutex::new(0);
        let (p, guise) = probe_admitted(&pasted(), |_| {
            let mut n = asks.lock().unwrap();
            *n += 1;
            let r = if *n == 1 {
                Ok(answered(403, ""))
            } else {
                Err(io::Error::other("reset"))
            };
            async move { r }
        })
        .await
        .unwrap();
        assert_eq!(p.status, 403);
        assert_eq!(guise, Guise::default());
    }

    #[tokio::test]
    async fn a_first_ask_that_fails_is_the_callers_error() {
        let r = probe_admitted(&pasted(), |_| async { Err::<Probe, _>("no route") }).await;
        assert_eq!(r.unwrap_err(), "no route");
    }

    #[test]
    fn the_origin_root_is_spelled_as_a_browser_spells_it() {
        let root = |t: Target| t.origin_root();
        assert_eq!(
            root(Target::direct_tls("cdn.example", 443, "/f")),
            "https://cdn.example/"
        );
        assert_eq!(
            root(Target::direct_tls("fs20.example", 183, "/d/tok/f")),
            "https://fs20.example:183/"
        );
        assert_eq!(
            root(Target::direct("cdn.example", 80, "/f")),
            "http://cdn.example/"
        );
        assert_eq!(
            root(Target::direct("::1", 8080, "/f")),
            "http://[::1]:8080/"
        );
        assert_eq!(
            root(Target::via_proxy(
                "proxy.lan",
                3128,
                "cdn.example:8080",
                "/f"
            )),
            "http://cdn.example:8080/"
        );
    }

    #[test]
    fn a_later_guise_overrides_only_what_it_changed() {
        let mut guise = Guise {
            referer: Some("https://a.example/".into()),
            ..Guise::default()
        };
        guise.absorb(Guise {
            user_agent: Some(BROWSER_USER_AGENT.into()),
            ..Guise::default()
        });
        assert_eq!(guise.referer.as_deref(), Some("https://a.example/"));
        assert_eq!(guise.user_agent.as_deref(), Some(BROWSER_USER_AGENT));

        let t = pasted()
            .with_headers(vec!["Referer: https://old.example/".into()], None)
            .in_guise(&guise);
        assert_eq!(t.headers, ["Referer: https://a.example/"]);
        assert_eq!(t.agent.as_deref(), Some(BROWSER_USER_AGENT));

        guise.absorb(Guise {
            user_agent: Some(DEFAULT_USER_AGENT.into()),
            no_referer: true,
            ..Guise::default()
        });
        assert_eq!(guise.referer, None);
        assert!(t.in_guise(&guise).headers.is_empty());
    }
}

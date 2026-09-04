// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Shared `reqwest::Client` construction for every networked subsystem.
//!
//! Every `viaduct` HTTP call goes through one of three clients (the feed
//! fetcher, the favicon/image cache, and the Reader View extractor). All of
//! them share the same baseline:
//!
//! - **`gzip` + `brotli`** decompression. Without these, servers that
//!   negotiate compressed encodings hand us binary garbage and the parser
//!   flags the result as `UnknownFormat` (see passionweiss.com,
//!   the-decoder.com, and many YouTube channel feeds — those all fail
//!   without compression even though NewsFlash works against them).
//! - **`rustls-tls`** for TLS — no system OpenSSL dependency.
//! - **Descriptive `User-Agent`** following NNW / NewsFlash convention:
//!   product name + version + contact URL. Some hosts (e.g. the-decoder)
//!   403 short / unrecognized UAs.
//! - **HTTP/2 auto-negotiation** via reqwest defaults.
//!
//! Each call site adds its own `Accept` header at request time — we don't
//! bake an Accept into the client because the three subsystems want
//! different MIME negotiation. See `ACCEPT_FEED`, `ACCEPT_IMAGE`,
//! `ACCEPT_HTML`.

use reqwest::Client;
use std::time::Duration;

const VIADUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// v2.6.11: cap idle connections per origin. Defaults to `usize::MAX`
/// in reqwest, which on a 130-feed corpus means we hold a TLS session
/// per host indefinitely (each rustls session retains certs +
/// session keys, several hundred KB easily). Four idle is enough to
/// pipeline a single user's hot paths (article images on a chosen
/// site, in-flight feed + favicon discovery against the same origin)
/// without unbounded growth.
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// v2.6.11: how long an idle connection sits in the pool before
/// being closed. reqwest's default is 90 s; we drop to 30 s so the
/// steady-state pool drains faster after a refresh cycle ends.
/// rustls session resumption tickets handle the cold-start cost on
/// the next cycle.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// v2.8.0: total per-request budget (connect + headers + body). reqwest
/// defaults to *no* timeout, so a host that accepts the connection but
/// stalls the body hangs the task until the OS TCP timeout (minutes).
/// The refresher fans feeds out under an 8-permit semaphore, so a handful
/// of dead hosts could otherwise wedge a whole refresh cycle and leave the
/// sync spinner stuck forever. 30 s clears any healthy feed / image / page
/// response with room to spare. Reader View layers its own tighter 15 s on
/// top via `client_builder` (last `.timeout` wins).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// v2.8.0: connect-phase budget. Fail fast on dead / unroutable hosts
/// rather than holding a semaphore slot for the full request timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Composed at build time so the User-Agent always tracks the package
/// version. Format mirrors NNW's `NetNewsWire/7.0.5 (Mac; +URL)` and
/// NewsFlash's equivalent.
fn user_agent() -> String {
    format!("Viaduct/{VIADUCT_VERSION} (RSS reader; +https://github.com/VirInvictus/Viaduct)")
}

/// `Accept` header for feed fetches. Lists every format we can parse, in
/// preference order. `*/*;q=0.5` is the catch-all for misconfigured
/// servers that respond `text/plain` to feed URLs.
pub const ACCEPT_FEED: &str = "application/rss+xml, application/atom+xml, application/feed+json, application/json;q=0.9, application/xml;q=0.8, text/xml;q=0.7, */*;q=0.5";

/// `Accept` header for inline images and favicons.
pub const ACCEPT_IMAGE: &str =
    "image/png, image/jpeg, image/webp, image/svg+xml, image/x-icon, image/*;q=0.9";

/// `Accept` header for HTML article pages (Reader View).
pub const ACCEPT_HTML: &str = "text/html, application/xhtml+xml, application/xml;q=0.9, */*;q=0.8";

/// The WebKit browser UA, resolved once by the UI at startup. Port of
/// NNW `UserAgent.browserUserAgent`, resolved in
/// `WebViewConfiguration.resolveBrowserUserAgent` (`75755af70`, #4868):
/// some hosts 403 non-browser agents on images, so the media downloaders
/// (favicons, images, video thumbs) and the favicon-discovery home-page
/// fetch identify as the same browser the article pane uses. Feed
/// fetches keep the viaduct UA. `None` until the UI resolves it (and on
/// headless test runs), which makes every media request fall back to
/// `user_agent()`.
static BROWSER_USER_AGENT: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Record the browser UA the article pane's WebKit reports. Accepts the
/// caller's judgment; upstream only forwards `Mozilla/`-prefixed values.
pub fn set_browser_user_agent(ua: Option<String>) {
    *BROWSER_USER_AGENT
        .write()
        .expect("browser UA lock poisoned") = ua;
}

/// UA for media and home-page downloads: the browser UA once resolved,
/// else the viaduct UA.
pub fn effective_media_user_agent() -> String {
    BROWSER_USER_AGENT
        .read()
        .expect("browser UA lock poisoned")
        .clone()
        .unwrap_or_else(user_agent)
}

/// Builds the baseline client. Used by the feed fetcher; the cache and
/// Reader View construct via similar paths in their own modules so they
/// can apply per-subsystem timeouts.
pub fn build_default_client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .user_agent(user_agent())
        .use_rustls_tls()
        .gzip(true)
        .brotli(true)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
}

/// Builder variant that lets the caller layer on a timeout or other
/// per-subsystem tweaks before `.build()`. Returns the same baseline
/// (UA + rustls + gzip + brotli).
pub fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .user_agent(user_agent())
        .use_rustls_tls()
        .gzip(true)
        .brotli(true)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NNW `75755af70`: media downloads ride the browser UA once the UI
    /// resolves it, falling back to the viaduct UA until then (headless
    /// runs, tests, a WebKit that reports nothing).
    #[test]
    fn media_user_agent_falls_back_then_overrides() {
        set_browser_user_agent(None);
        assert!(
            effective_media_user_agent().contains("Viaduct/"),
            "unset global falls back to the viaduct UA"
        );

        set_browser_user_agent(Some("Mozilla/5.0 Test UA".to_string()));
        assert_eq!(effective_media_user_agent(), "Mozilla/5.0 Test UA");

        // Restore for any parallel test asserting on the fallback.
        set_browser_user_agent(None);
    }
}

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

/// Stream a response body under a hard byte cap. Returns the bytes read
/// plus whether the cap truncated the body; callers decide whether
/// truncation is acceptable (head-scans accept a prefix, body consumers
/// reject it). The running chunk guard is the real enforcement, since
/// `Content-Length` can be absent or lie. A mid-body read failure errors
/// out rather than returning a short body, so a network failure can
/// never masquerade as an empty successful response.
pub async fn read_body_capped(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(max_bytes.min(256 * 1024));
    loop {
        match response.chunk().await? {
            Some(chunk) => {
                let remaining = max_bytes.saturating_sub(buf.len());
                if chunk.len() >= remaining {
                    if remaining > 0 {
                        buf.extend_from_slice(&chunk[..remaining]);
                    }
                    return Ok((buf, true));
                }
                buf.extend_from_slice(&chunk);
            }
            None => return Ok((buf, false)),
        }
    }
}

/// Upper bound on bytes read from an error response solely for the log
/// excerpt. Generous for a message a server writes about its own
/// failure, small enough that reading it never threatens the memory
/// budget.
pub const ERROR_BODY_EXCERPT_BYTES: usize = 8 * 1024;

/// Longest excerpt `response_body_excerpt` keeps (NNW `49dbebf67`
/// `prefix(500)`), in chars.
const EXCERPT_MAX_CHARS: usize = 500;

/// Port of NNW `String.collapsingWhitespace` (RSCore): runs of whitespace
/// collapse to a single space, and leading / trailing whitespace
/// disappears. Byte-level like upstream's implementation — the bytes it
/// treats as whitespace (space, 0x09..=0x0D) never occur inside a
/// multi-byte UTF-8 sequence, so non-ASCII passes through untouched.
fn collapsing_whitespace(s: &str) -> String {
    fn is_ws(b: u8) -> bool {
        b == b' ' || (0x09..=0x0D).contains(&b)
    }
    let mut out = Vec::with_capacity(s.len());
    let mut saw_non_space = false;
    let mut pending_space = false;
    for &b in s.as_bytes() {
        if is_ws(b) {
            if saw_non_space {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            out.push(b' ');
            pending_space = false;
        }
        saw_non_space = true;
        out.push(b);
    }
    // Trailing `pending_space` is discarded — the trim-trailing half.
    String::from_utf8(out).expect("byte-level whitespace pass preserves UTF-8")
}

/// Port of NNW `49dbebf67` `responseBodyForError`: a trimmed,
/// whitespace-collapsed prefix of an error response's body. The body
/// often says what the server didn't like. Strict UTF-8 decode, like
/// upstream's `String(data:encoding: .utf8)` — binary garbage yields
/// `None` rather than a replacement-character mess. `None` when the
/// collapsed excerpt would be empty.
pub fn response_body_excerpt(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let body = std::str::from_utf8(bytes).ok()?;
    let collapsed = collapsing_whitespace(body);
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(EXCERPT_MAX_CHARS).collect())
}

/// `response_body_excerpt` for bytes cut off at a read cap: a multi-byte
/// character split at the boundary is trimmed before the strict decode,
/// since an incomplete trailing sequence is a cap artifact, not
/// non-UTF-8 body content. An invalid sequence anywhere else still
/// yields `None`.
fn excerpt_from_capped_bytes(bytes: &[u8]) -> Option<String> {
    let usable = match std::str::from_utf8(bytes) {
        Ok(_) => bytes,
        Err(e) if e.error_len().is_none() => &bytes[..e.valid_up_to()],
        Err(_) => return None,
    };
    response_body_excerpt(usable)
}

/// Read at most `ERROR_BODY_EXCERPT_BYTES` of a non-success response
/// body and shape it for the log excerpt (NNW `49dbebf67`: the error
/// body rides the thrown error; we never buffer more than the excerpt
/// needs). A body read failure or a non-UTF-8 body yields `None` — the
/// status code still carries the failure.
pub async fn read_error_body_excerpt(response: reqwest::Response) -> Option<String> {
    let (bytes, _truncated) = read_body_capped(response, ERROR_BODY_EXCERPT_BYTES)
        .await
        .ok()?;
    excerpt_from_capped_bytes(&bytes)
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

    #[test]
    fn excerpt_collapses_whitespace_runs() {
        assert_eq!(
            response_body_excerpt(b"  error:\n\t  quota   exceeded\n"),
            Some("error: quota exceeded".to_string())
        );
        // Non-ASCII passes through the byte-level collapse untouched.
        assert_eq!(
            response_body_excerpt("déjà\t\tvu".as_bytes()),
            Some("déjà vu".to_string())
        );
    }

    #[test]
    fn excerpt_is_none_for_empty_or_blank_bodies() {
        assert_eq!(response_body_excerpt(b""), None);
        assert_eq!(response_body_excerpt(b"  \r\n\t "), None);
    }

    #[test]
    fn excerpt_rejects_non_utf8_bodies() {
        // Upstream's `String(data:encoding: .utf8)` returns nil for
        // binary error bodies; so do we, rather than logging
        // replacement-character mush.
        assert_eq!(response_body_excerpt(&[0xFF, 0xFE, b'<', b'>']), None);
        assert_eq!(response_body_excerpt(&[b'o', b'k', 0x80, b'!']), None);
    }

    #[test]
    fn excerpt_truncates_at_a_char_boundary() {
        // 600 three-byte chars: the 500-char cut must not split one.
        let body = "雨".repeat(600);
        let excerpt = response_body_excerpt(body.as_bytes()).expect("utf8 body");
        assert_eq!(excerpt.chars().count(), 500);
        assert!(excerpt.chars().all(|c| c == '雨'));
    }

    #[test]
    fn excerpt_from_capped_bytes_trims_a_split_trailing_char() {
        // "ok" plus the first byte of a two-byte char: the incomplete
        // sequence is a cap artifact, not body content.
        let mut bytes = b"ok".to_vec();
        bytes.extend_from_slice(&[0xC3]);
        assert_eq!(excerpt_from_capped_bytes(&bytes), Some("ok".to_string()));
        // An invalid sequence before the end is still rejected.
        assert_eq!(excerpt_from_capped_bytes(&[b'o', 0xFF, b'k']), None);
    }
}

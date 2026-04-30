// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use chrono::{DateTime, Duration, Utc};
use md5::{Digest, Md5};
use reqwest::{Client, StatusCode, header};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, error, warn};

use crate::database::accounts::Account;
use crate::error::{NetworkError, ParseError, Result};
use crate::models::{ArticleChanges, Feed, FeedSettings};

/// Port of NNW's `cacheControlMaxMaxAge` — cap `max-age` at 5 hours except for
/// well-behaved hosts like openrss.org.
const CACHE_CONTROL_MAX_MAX_AGE_SECS: i64 = 5 * 60 * 60;

/// Port of NNW's 8-day expiry on conditional GET info. Some servers always respond
/// 304 regardless of real state; dropping etag/last-modified periodically forces a
/// full re-sync. openrss.org and rachelbythebay.com are excluded.
const CONDITIONAL_GET_EXPIRY_DAYS: i64 = 8;

/// v2.6.9: cap on simultaneously-running per-feed pipelines inside a
/// single refresh cycle. Pre-v2.6.9 we `tokio::spawn`-ed every feed at
/// once, which made the cycle's peak RSS scale linearly with feed
/// count (one user reported `peak_delta_mb=124` for 130 feeds). 8 is
/// roughly the URLSession default global concurrency on macOS — enough
/// pipelining to keep network busy on a fast connection, low enough
/// that the in-flight HTTP bodies + parsed feeds + favicon-discovery
/// HTML stay bounded. NNW caps per-host at 1 via URLSession's
/// `httpMaximumConnectionsPerHost`; we don't replicate the per-host
/// shape (reqwest pools differently), but the global cap provides the
/// same memory-bound effect.
const REFRESH_PARALLELISM: usize = 8;

#[derive(Clone, Debug)]
pub struct FetchResult {
    pub status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub cache_control_max_age: Option<i64>,
}

type FetchSender = broadcast::Sender<std::result::Result<FetchResult, String>>;

#[derive(Clone)]
pub struct Fetcher {
    client: Client,
    active_requests: Arc<Mutex<HashMap<String, FetchSender>>>,
    cooldowns: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher {
    pub fn new() -> Self {
        let client =
            crate::network::http::build_default_client().expect("Failed to build reqwest client");

        Self {
            client,
            active_requests: Arc::new(Mutex::new(HashMap::new())),
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Borrow the underlying `reqwest::Client` so adjacent network work
    /// (favicon discovery, feed_discovery) can share the connection
    /// pool. `reqwest::Client` is cheap to clone — internally an `Arc`.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub async fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchResult> {
        let parsed_url = url::Url::parse(url).map_err(NetworkError::from)?;
        let host = parsed_url.host_str().unwrap_or("").to_string();

        {
            let cooldowns = self.cooldowns.lock().await;
            if let Some(retry_after) = cooldowns.get(&host)
                && Utc::now() < *retry_after
            {
                return Err(NetworkError::RateLimited {
                    retry_after_secs: (*retry_after - Utc::now()).num_seconds().max(0) as u64,
                }
                .into());
            }
        }

        let mut rx = {
            let mut active = self.active_requests.lock().await;
            if let Some(tx) = active.get(url) {
                tx.subscribe()
            } else {
                let (tx, rx) = broadcast::channel(1);
                active.insert(url.to_string(), tx.clone());

                let client = self.client.clone();
                let url_clone = url.to_string();
                let active_clone = self.active_requests.clone();
                let cooldowns_clone = self.cooldowns.clone();
                let host_clone = host.clone();

                let etag_clone = etag.map(|s| s.to_string());
                let last_modified_clone = last_modified.map(|s| s.to_string());

                tokio::spawn(async move {
                    let mut req = client
                        .get(&url_clone)
                        .header(header::ACCEPT, crate::network::http::ACCEPT_FEED);
                    if let Some(e) = &etag_clone {
                        req = req.header(header::IF_NONE_MATCH, e);
                    }
                    if let Some(l) = &last_modified_clone {
                        req = req.header(header::IF_MODIFIED_SINCE, l);
                    }
                    debug!(
                        url = %url_clone,
                        etag = ?etag_clone.is_some(),
                        if_modified_since = ?last_modified_clone.is_some(),
                        "fetch: GET"
                    );
                    let send_started = std::time::Instant::now();
                    let res = req.send().await;
                    let out = match res {
                        Ok(response) => {
                            let status = response.status();
                            let content_encoding = response
                                .headers()
                                .get(header::CONTENT_ENCODING)
                                .and_then(|v| v.to_str().ok().map(|s| s.to_string()));
                            if status == StatusCode::TOO_MANY_REQUESTS
                                && let Some(retry_after) =
                                    response.headers().get(header::RETRY_AFTER)
                                && let Ok(s) = retry_after.to_str()
                                && let Ok(secs) = s.parse::<i64>()
                            {
                                let mut cooldowns = cooldowns_clone.lock().await;
                                cooldowns.insert(
                                    host_clone.clone(),
                                    Utc::now() + Duration::seconds(secs),
                                );
                            }

                            if status == StatusCode::NOT_MODIFIED {
                                debug!(
                                    url = %url_clone,
                                    elapsed_ms = send_started.elapsed().as_millis() as u64,
                                    "fetch: 304 (cached)"
                                );
                                Ok(FetchResult {
                                    status: status.as_u16(),
                                    body: Vec::new(),
                                    etag: None,
                                    last_modified: None,
                                    cache_control_max_age: None,
                                })
                            } else {
                                let etag = response
                                    .headers()
                                    .get(header::ETAG)
                                    .and_then(|v| v.to_str().ok().map(|s| s.to_string()));
                                let last_modified = response
                                    .headers()
                                    .get(header::LAST_MODIFIED)
                                    .and_then(|v| v.to_str().ok().map(|s| s.to_string()));

                                let mut cache_control_max_age = None;
                                if let Some(cc) = response.headers().get(header::CACHE_CONTROL)
                                    && let Ok(cc_str) = cc.to_str()
                                {
                                    for part in cc_str.split(',') {
                                        let part = part.trim();
                                        if let Some(stripped) = part.strip_prefix("max-age=")
                                            && let Ok(secs) = stripped.parse::<i64>()
                                        {
                                            cache_control_max_age = Some(secs);
                                        }
                                    }
                                }

                                let body = response
                                    .bytes()
                                    .await
                                    .map(|b| b.to_vec())
                                    .unwrap_or_default();
                                debug!(
                                    url = %url_clone,
                                    status = status.as_u16(),
                                    body_bytes = body.len(),
                                    encoding = ?content_encoding,
                                    has_etag = etag.is_some(),
                                    max_age = ?cache_control_max_age,
                                    elapsed_ms = send_started.elapsed().as_millis() as u64,
                                    "fetch: response"
                                );
                                Ok(FetchResult {
                                    status: status.as_u16(),
                                    body,
                                    etag,
                                    last_modified,
                                    cache_control_max_age,
                                })
                            }
                        }
                        Err(e) => {
                            warn!(
                                url = %url_clone,
                                error = %e,
                                elapsed_ms = send_started.elapsed().as_millis() as u64,
                                "fetch: network error"
                            );
                            Err(e.to_string())
                        }
                    };

                    let mut active = active_clone.lock().await;
                    if let Some(tx) = active.remove(&url_clone) {
                        let _ = tx.send(out);
                    }
                });
                rx
            }
        };

        match rx.recv().await {
            Ok(Ok(res)) => Ok(res),
            // The downloader task stringifies any reqwest failure before
            // sending — rebuild as a parse-style error so the type at least
            // names the network failure rather than misclassifying as DB.
            Ok(Err(e)) => Err(ParseError::Malformed(format!("network: {}", e)).into()),
            // Sender dropped (task panicked). Surface as a generic rate-limit
            // with no retry hint so the caller backs off without retrying
            // immediately; misclassifying as a DB error was wrong.
            Err(_) => Err(NetworkError::RateLimited {
                retry_after_secs: 0,
            }
            .into()),
        }
    }
}

pub struct AccountRefresher {
    fetcher: Fetcher,
    account: Arc<Account>,
    changes_sender: tokio::sync::mpsc::UnboundedSender<ArticleChanges>,
    retention_days: i64,
    /// v2.6.10 progress counter — Some when the caller wants per-feed
    /// completion notifications (the GTK window polls this to update
    /// the bottom progress bar). None when we're refreshing in a
    /// context with no UI to drive (mem_check, internal sync).
    completion_counter: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl AccountRefresher {
    pub fn new(
        account: Arc<Account>,
        changes_sender: tokio::sync::mpsc::UnboundedSender<ArticleChanges>,
        retention_days: i64,
    ) -> Self {
        Self {
            fetcher: Fetcher::new(),
            account,
            changes_sender,
            retention_days,
            completion_counter: None,
        }
    }

    /// v2.6.10: install a per-feed completion counter so the GTK
    /// window can render a progress bar. The counter is incremented
    /// once per per-feed task on completion (regardless of whether
    /// the feed produced new articles, was 304, or failed); call
    /// before `refresh_feeds` / `refresh_feeds_forced`.
    pub fn with_completion_counter(mut self, counter: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        self.completion_counter = Some(counter);
        self
    }

    pub async fn refresh_feeds(&self, feeds: Vec<(Feed, FeedSettings)>) {
        self.refresh_feeds_inner(feeds, false).await
    }

    /// Force a refresh that bypasses the 29-minute timing throttle and the
    /// 5-hour Cache-Control freshness check. `etag` / `last_modified` are
    /// still sent (so a 304 response remains a fast no-op), but every feed
    /// gets a network round-trip. Use for explicit user clicks. Auto-refresh
    /// (when it lands in Phase 17 / Background portal) calls
    /// `refresh_feeds` instead.
    pub async fn refresh_feeds_forced(&self, feeds: Vec<(Feed, FeedSettings)>) {
        self.refresh_feeds_inner(feeds, true).await
    }

    async fn refresh_feeds_inner(&self, feeds: Vec<(Feed, FeedSettings)>, force: bool) {
        let special_case_cutoff_date = Utc::now() - Duration::hours(25);
        let total_input = feeds.len();
        let mut skipped = 0usize;

        // v2.6.9: cap in-flight per-feed pipelines to keep peak RSS
        // bounded. The pre-v2.6.9 path `tokio::spawn`-ed every feed
        // simultaneously, so a 130-feed cycle held 130 HTTP bodies +
        // 130 parsed feed trees + 130 favicon-discovery HTML pages
        // (v2.6.4) all in memory at the same time. Diagnostic from a
        // real run: `peak_delta_mb=124` for a single cycle. NNW caps
        // per-host concurrency at 1 via `URLSession.httpMaximum
        // ConnectionsPerHost`; reqwest pools differently, so we
        // bound at the pipeline level instead. Eight is roughly the
        // URLSession default global concurrency on macOS.
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(REFRESH_PARALLELISM));
        let mut futures = Vec::new();
        for (feed, settings) in feeds {
            if !force && Self::feed_should_be_skipped(&feed, &settings, special_case_cutoff_date) {
                debug!("Skipping feed: {}", feed.url);
                skipped += 1;
                // v2.6.10: count skipped feeds so the progress-bar
                // denominator (= paired.len()) matches and the bar
                // reaches 100% at cycle end. Otherwise an
                // auto-refresh that skips every feed in the 29-min
                // throttle would leave the bar stuck near 0.
                if let Some(c) = self.completion_counter.as_ref() {
                    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                continue;
            }

            let fetcher = self.fetcher.clone();
            let account = self.account.clone();
            let sender = self.changes_sender.clone();
            let retention_days = self.retention_days;
            let permit_source = semaphore.clone();
            let counter = self.completion_counter.clone();

            futures.push(tokio::spawn(async move {
                // Acquire blocks the per-feed task at the start of
                // the pipeline so the captured `feed` / `settings` /
                // `account` / `sender` / `fetcher` clones held while
                // queued are minimal — the heavy allocations (HTTP
                // body, parsed feed, favicon discovery) only land
                // once we hold a permit. `acquire_owned` returns
                // None only when the semaphore was closed; we never
                // close it, so the unwrap_or skip is defensive.
                let Ok(_permit) = permit_source.acquire_owned().await else {
                    return;
                };
                refresh_one_feed(
                    fetcher,
                    account,
                    sender,
                    feed,
                    settings,
                    retention_days,
                    force,
                )
                .await;
                // v2.6.10: bump the GTK-side progress counter on
                // every completion (success, 304, error — all count
                // toward "feeds we attempted"). The window's poll
                // loop reads through this and updates the bar.
                if let Some(c) = counter {
                    c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }));
        }

        debug!(
            total_input,
            skipped,
            attempted = total_input - skipped,
            force,
            parallelism = REFRESH_PARALLELISM,
            "refresh_feeds: dispatched"
        );

        for f in futures {
            let _ = f.await;
        }
    }

    fn feed_should_be_skipped(
        feed: &Feed,
        settings: &FeedSettings,
        special_case_cutoff_date: DateTime<Utc>,
    ) -> bool {
        // Disallowed hosts
        if let Ok(url) = url::Url::parse(&feed.url)
            && let Some(host) = url.host_str()
        {
            let host = host.to_lowercase();
            if host == "twitter.com"
                || host == "www.twitter.com"
                || host == "x.com"
                || host == "www.x.com"
            {
                return true;
            }
        }

        // Cache-control max age (already capped at 5h on persistence)
        if let (Some(last_check), Some(max_age)) = (settings.last_check_date, settings.max_age) {
            let elapsed = (Utc::now() - last_check).num_seconds();
            if elapsed < max_age {
                return true;
            }
        }

        // Timing logic — order matches NNW
        // `feedShouldBeSkippedForTimingReasons`.
        if let Some(last_check) = settings.last_check_date {
            // domainsWithNoMinimumTime short-circuits to false: these hosts
            // are checked on every refresh attempt regardless of timing.
            if is_no_minimum_time_domain(&feed.url) {
                return false;
            }
            if is_special_case_host(&feed.url) {
                if last_check > special_case_cutoff_date {
                    return true;
                }
            } else {
                let minutes_elapsed = (Utc::now() - last_check).num_minutes();
                if minutes_elapsed < 29 {
                    return true;
                }
            }
        }

        false
    }
}

/// Hosts that get the 25-hour `specialCaseCutoffDate` instead of the 29-minute
/// minimum, plus exemption from the 8-day conditional-GET expiry. These two
/// well-behaved hosts publish high-frequency feeds and their conditional-GET
/// is reliable. NNW's `SpecialCase.rachelByTheBayHostName` /
/// `SpecialCase.openRSSOrgHostName`.
const SPECIAL_CASE_DOMAINS: &[&str] = &["rachelbythebay.com", "openrss.org"];

/// Hosts that skip the 29-minute minimum entirely (every refresh attempt
/// hits them, modulo Cache-Control / 304). These are personal sites that
/// don't publish often but can update at unpredictable times. Synced from
/// NNW `LocalAccountRefresher.domainsWithNoMinimumTime` as of `4d594181f`.
const NO_MINIMUM_TIME_DOMAINS: &[&str] = &[
    "inessential.com",
    "ranchero.com",
    "netnewswire.blog",
    "daringfireball.net",
    "redsweater.com",
    "indiestack.com",
    "blog.plunkitup.com",
    "bitsplitting.org",
    "allenpike.com",
    "hypercritical.co",
    "micro.inessential.com",
    "discourse.netnewswire.com",
    "onefoottsunami.com",
    "manton.org",
    "randsinrepose.com",
    "micro.blog",
    "shapeof.com",
    "flyingmeat.com",
];

/// Returns true if the URL's host matches one of the supplied domains
/// exactly, ignoring case and an optional leading `www.`. Port of NNW
/// `SpecialCase.urlStringMatchesDomain`. Domains in `domains` must already
/// be lowercase and stripped of `www.`.
///
/// Substring matching (the previous behavior) false-positives on hosts
/// like `evilrachelbythebay.com` or `example.com.evil.com`, which is a
/// real attack surface for the conditional-GET / no-minimum-time bypass.
fn url_host_matches_domain(url: &str, domains: &[&str]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let lower = host.to_ascii_lowercase();
    let normalized = lower.strip_prefix("www.").unwrap_or(&lower);
    domains.contains(&normalized)
}

fn is_special_case_host(url: &str) -> bool {
    url_host_matches_domain(url, SPECIAL_CASE_DOMAINS)
}

fn is_openrss(url: &str) -> bool {
    url_host_matches_domain(url, &["openrss.org"])
}

fn is_no_minimum_time_domain(url: &str) -> bool {
    url_host_matches_domain(url, NO_MINIMUM_TIME_DOMAINS)
}

/// Drop conditional-GET info if it's older than 8 days (NNW behavior). Some
/// servers respond 304 to any conditional GET regardless of real state, which
/// would starve the feed. Special-case hosts are exempt.
fn maybe_expire_conditional_get_info(
    feed: &Feed,
    settings: &FeedSettings,
) -> (Option<String>, Option<String>) {
    if is_special_case_host(&feed.url) {
        return (settings.etag.clone(), settings.last_modified.clone());
    }
    if let Some(created) = settings.date_created
        && (Utc::now() - created) > Duration::days(CONDITIONAL_GET_EXPIRY_DAYS)
    {
        debug!(
            "Dropping conditional-GET info for {} — older than {} days",
            feed.url, CONDITIONAL_GET_EXPIRY_DAYS
        );
        return (None, None);
    }
    (settings.etag.clone(), settings.last_modified.clone())
}

async fn refresh_one_feed(
    fetcher: Fetcher,
    account: Arc<Account>,
    sender: tokio::sync::mpsc::UnboundedSender<ArticleChanges>,
    feed: Feed,
    settings: FeedSettings,
    retention_days: i64,
    force: bool,
) {
    // On force, drop conditional-GET headers entirely so the server can't
    // 304 us into a no-op when the local article store is missing.
    let (etag, last_modified) = if force {
        (None, None)
    } else {
        maybe_expire_conditional_get_info(&feed, &settings)
    };
    let mut new_settings = settings.clone();
    new_settings.last_check_date = Some(Utc::now());

    match fetcher
        .fetch(&feed.url, etag.as_deref(), last_modified.as_deref())
        .await
    {
        Ok(result) => {
            if result.status == 304 {
                debug!("Feed not modified (304): {}", feed.url);
                let _ = account.upsert_feed_settings(new_settings).await;
                return;
            }
            if result.status != 200 {
                warn!("Feed HTTP {}: {}", result.status, feed.url);
                let _ = account.upsert_feed_settings(new_settings).await;
                return;
            }

            // Refresh conditional-GET headers if the response provided new ones.
            let mut got_conditional = false;
            if result.etag.is_some() {
                new_settings.etag = result.etag.clone();
                got_conditional = true;
            }
            if result.last_modified.is_some() {
                new_settings.last_modified = result.last_modified.clone();
                got_conditional = true;
            }
            if got_conditional {
                new_settings.date_created = Some(Utc::now());
            }

            // Cap Cache-Control max-age (NNW's cacheControlMaxMaxAge) — many sites
            // misconfigure this and ship max-age values measured in months.
            if let Some(max_age) = result.cache_control_max_age {
                let capped = if is_openrss(&feed.url) {
                    max_age
                } else {
                    max_age.min(CACHE_CONTROL_MAX_MAX_AGE_SECS)
                };
                new_settings.max_age = Some(capped);
            }

            // Content-hash short-circuit: skip parsing if the body is
            // byte-identical to the last successful fetch. `force=true`
            // bypasses this so a manual refresh after a deleted articles
            // DB always re-parses and re-inserts.
            let mut hasher = Md5::new();
            hasher.update(&result.body);
            let hash = format!("{:x}", hasher.finalize());
            if !force && Some(&hash) == settings.content_hash.as_ref() {
                debug!("Feed content hash unchanged: {}", feed.url);
                let _ = account.upsert_feed_settings(new_settings).await;
                return;
            }
            new_settings.content_hash = Some(hash);

            // Parse → diff → emit changes.
            match crate::parser::parse(&result.body, &feed.url) {
                Ok(parsed) => {
                    // Pick up channel-level metadata from the parsed feed
                    // (Phase 11). NNW persists these on the feed; sidebar
                    // favicon fetch uses `icon_url` first.
                    if parsed.icon_url.is_some() {
                        new_settings.icon_url = parsed.icon_url.clone();
                    }
                    // Persist the home-page URL so favicon discovery
                    // (and any future home-link UI) has a base to work
                    // from. Was being dropped on the floor pre-v2.6.4.
                    if parsed.home_page_url.is_some() {
                        new_settings.home_page_url = parsed.home_page_url.clone();
                    }
                    // v2.6.4: most personal blogs don't ship a feed-level
                    // `<image>` / `<icon>`, so `parsed.icon_url` stays
                    // None and the sidebar shows the AdwAvatar fallback.
                    // Probe the home page HTML head for `<link rel="icon">`
                    // and fall back to `<origin>/favicon.ico`. Only runs
                    // when we don't already have a favicon — successful
                    // discoveries persist into `favicon_url`, so the
                    // probe is at most once per feed across the lifetime
                    // of the install.
                    if new_settings.favicon_url.is_none()
                        && let Some(home) = new_settings.home_page_url.as_deref()
                        && let Some(found) = crate::network::favicon_discovery::discover_favicon(
                            &fetcher.client,
                            home,
                        )
                        .await
                    {
                        new_settings.favicon_url = Some(found);
                    }
                    match account
                        .update_feed(feed.id.clone(), parsed.items, true, retention_days)
                        .await
                    {
                        Ok(changes) => {
                            debug!(
                                "Feed {}: {} new, {} updated, {} deleted",
                                feed.url,
                                changes.new_articles.len(),
                                changes.updated_articles.len(),
                                changes.deleted_article_ids.len(),
                            );
                            let _ = sender.send(changes);
                        }
                        Err(e) => {
                            error!("DB update failed for {}: {:?}", feed.url, e);
                        }
                    }
                }
                Err(e) => {
                    // Helpful preview when the parser rejects the body —
                    // most "UnknownFormat" failures are servers returning
                    // an HTML challenge page or unexpected MIME.
                    let preview_len = result.body.len().min(120);
                    let preview = String::from_utf8_lossy(&result.body[..preview_len]);
                    error!(
                        url = %feed.url,
                        body_bytes = result.body.len(),
                        preview = %preview,
                        error = ?e,
                        "Parse failed"
                    );
                }
            }

            let _ = account.upsert_feed_settings(new_settings).await;
        }
        Err(e) => {
            error!("Failed to fetch feed {}: {:?}", feed.url, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_host_matches_domain_handles_www_and_case() {
        let list = &["inessential.com", "openrss.org"];
        assert!(url_host_matches_domain(
            "https://inessential.com/feed.xml",
            list
        ));
        assert!(url_host_matches_domain(
            "https://www.inessential.com/feed.xml",
            list
        ));
        assert!(url_host_matches_domain(
            "https://INESSENTIAL.COM/feed.xml",
            list
        ));
        assert!(url_host_matches_domain(
            "https://openrss.org/some/path",
            list
        ));
    }

    #[test]
    fn url_host_matches_domain_rejects_substring_attacks() {
        // The previous substring-based implementation accepted these.
        let list = &["rachelbythebay.com", "openrss.org"];
        assert!(!url_host_matches_domain(
            "https://evilrachelbythebay.com/",
            list
        ));
        assert!(!url_host_matches_domain(
            "https://attacker.com/?u=rachelbythebay.com",
            list
        ));
        assert!(!url_host_matches_domain(
            "https://rachelbythebay.com.evil.com/",
            list
        ));
    }

    #[test]
    fn url_host_matches_domain_does_not_match_subdomains() {
        // NNW checks exact match (after www. strip), not suffix. A
        // sub-subdomain like `blog.example.com` does NOT match `example.com`.
        // Domains that need both forms must be listed explicitly (see
        // `micro.inessential.com` alongside `inessential.com`).
        let list = &["inessential.com"];
        assert!(!url_host_matches_domain(
            "https://blog.inessential.com/x",
            list
        ));
    }

    #[test]
    fn no_minimum_time_domains_match_real_world_urls() {
        assert!(is_no_minimum_time_domain(
            "https://daringfireball.net/feeds/main"
        ));
        assert!(is_no_minimum_time_domain(
            "https://www.flyingmeat.com/blog/feed.xml"
        ));
        assert!(is_no_minimum_time_domain(
            "https://discourse.netnewswire.com/latest.rss"
        ));
        assert!(!is_no_minimum_time_domain(
            "https://example.com/daringfireball.net"
        ));
    }
}

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
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static("Viaduct/1.0 (Linux; GTK4)"),
        );

        let client = Client::builder()
            .use_rustls_tls()
            // HTTP/2 is negotiated automatically by reqwest when rustls is used
            .default_headers(headers)
            .build()
            .expect("Failed to build reqwest client");

        Self {
            client,
            active_requests: Arc::new(Mutex::new(HashMap::new())),
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
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
                    let mut req = client.get(&url_clone);
                    if let Some(e) = etag_clone {
                        req = req.header(header::IF_NONE_MATCH, e);
                    }
                    if let Some(l) = last_modified_clone {
                        req = req.header(header::IF_MODIFIED_SINCE, l);
                    }

                    let res = req.send().await;
                    let out = match res {
                        Ok(response) => {
                            let status = response.status();
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
                                Ok(FetchResult {
                                    status: status.as_u16(),
                                    body,
                                    etag,
                                    last_modified,
                                    cache_control_max_age,
                                })
                            }
                        }
                        Err(e) => Err(e.to_string()),
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
        }
    }

    pub async fn refresh_feeds(&self, feeds: Vec<(Feed, FeedSettings)>) {
        let special_case_cutoff_date = Utc::now() - Duration::hours(25);

        let mut futures = Vec::new();
        for (feed, settings) in feeds {
            if Self::feed_should_be_skipped(&feed, &settings, special_case_cutoff_date) {
                debug!("Skipping feed: {}", feed.url);
                continue;
            }

            let fetcher = self.fetcher.clone();
            let account = self.account.clone();
            let sender = self.changes_sender.clone();
            let retention_days = self.retention_days;

            futures.push(tokio::spawn(async move {
                refresh_one_feed(fetcher, account, sender, feed, settings, retention_days).await;
            }));
        }

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

        // Timing logic
        if let Some(last_check) = settings.last_check_date {
            let is_special = is_special_case_host(&feed.url);
            if is_special {
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

fn is_special_case_host(url: &str) -> bool {
    url.contains("rachelbythebay.com") || url.contains("openrss.org")
}

fn is_openrss(url: &str) -> bool {
    url.contains("openrss.org")
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
) {
    let (etag, last_modified) = maybe_expire_conditional_get_info(&feed, &settings);
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

            // Content-hash short-circuit: skip parsing if the body is byte-identical.
            let mut hasher = Md5::new();
            hasher.update(&result.body);
            let hash = format!("{:x}", hasher.finalize());
            if Some(&hash) == settings.content_hash.as_ref() {
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
                    error!("Parse failed for {}: {:?}", feed.url, e);
                }
            }

            let _ = account.upsert_feed_settings(new_settings).await;
        }
        Err(e) => {
            error!("Failed to fetch feed {}: {:?}", feed.url, e);
        }
    }
}

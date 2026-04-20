use chrono::{DateTime, Duration, Utc};
use md5::{Digest, Md5};
use reqwest::{Client, StatusCode, header};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, error, warn};

use crate::error::{DatabaseError, NetworkError, ParseError, Result};
use crate::models::{ArticleChanges, Feed, FeedSettings};

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
            Ok(Err(e)) => Err(ParseError::Malformed(e).into()),
            Err(_) => Err(DatabaseError::WriterGone.into()), // Channel closed unexpectedly means task panicked or sender dropped
        }
    }
}

pub struct LocalAccountRefresher {
    fetcher: Fetcher,
    sender: tokio::sync::mpsc::UnboundedSender<ArticleChanges>,
}

impl LocalAccountRefresher {
    pub fn new(sender: tokio::sync::mpsc::UnboundedSender<ArticleChanges>) -> Self {
        Self {
            fetcher: Fetcher::new(),
            sender,
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
            let sender = self.sender.clone();

            futures.push(tokio::spawn(async move {
                let etag = settings.etag.as_deref();
                let last_modified = settings.last_modified.as_deref();

                match fetcher.fetch(&feed.url, etag, last_modified).await {
                    Ok(result) => {
                        if result.status == 304 {
                            debug!("Feed not modified (304): {}", feed.url);
                            return;
                        }
                        if result.status != 200 {
                            warn!("Feed HTTP {}: {}", result.status, feed.url);
                            return;
                        }

                        let mut hasher = Md5::new();
                        hasher.update(&result.body);
                        let hash = format!("{:x}", hasher.finalize());
                        if Some(&hash) == settings.content_hash.as_ref() {
                            debug!("Feed content hash unchanged: {}", feed.url);
                            return;
                        }

                        debug!("Feed downloaded successfully: {}", feed.url);

                        // Emit dummy article changes to the UI layer
                        let changes = ArticleChanges::default();
                        let _ = sender.send(changes);
                    }
                    Err(e) => {
                        error!("Failed to fetch feed {}: {:?}", feed.url, e);
                    }
                }
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

        // Cache-control max age
        if let (Some(last_check), Some(max_age)) = (settings.last_check_date, settings.max_age) {
            let elapsed = (Utc::now() - last_check).num_seconds();
            if elapsed < max_age {
                return true;
            }
        }

        // Timing logic
        if let Some(last_check) = settings.last_check_date {
            let is_special =
                feed.url.contains("rachelbythebay.com") || feed.url.contains("openrss.org");
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

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::{NetworkError, Result, ViaductError};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderAPIVariant {
    Inoreader,
}

impl ReaderAPIVariant {
    pub fn host(&self) -> &'static str {
        match self {
            Self::Inoreader => "https://www.inoreader.com",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPITag {
    pub id: String,
    pub sortid: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPITagContainer {
    pub tags: Vec<ReaderAPITag>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPISubscription {
    #[serde(rename = "id")]
    pub feed_id: String,
    pub title: String,
    pub categories: Vec<ReaderAPITag>,
    pub url: String,
    #[serde(rename = "htmlUrl")]
    pub html_url: Option<String>,
    #[serde(rename = "iconUrl")]
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPISubscriptionContainer {
    pub subscriptions: Vec<ReaderAPISubscription>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIQuickAddResult {
    #[serde(rename = "numResults")]
    pub num_results: i32,
    #[serde(rename = "streamId")]
    pub stream_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIReference {
    #[serde(rename = "id")]
    pub item_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIReferenceWrapper {
    #[serde(rename = "itemRefs")]
    pub item_refs: Vec<ReaderAPIReference>,
    pub continuation: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntry {
    #[serde(rename = "id")]
    pub article_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub summary: ReaderAPIEntrySummary,
    #[serde(rename = "published")]
    pub published_timestamp: Option<f64>,
    #[serde(rename = "alternate")]
    pub alternates: Option<Vec<ReaderAPIEntryAlternate>>,
    pub categories: Option<Vec<String>>,
    pub origin: ReaderAPIEntryOrigin,
}

impl ReaderAPIEntry {
    pub fn unique_id(&self) -> String {
        // Should look something like "tag:google.com,2005:reader/item/00058b10ce338909"
        let id_part = self
            .article_id
            .split('/')
            .next_back()
            .unwrap_or(&self.article_id);

        // Convert hex representation back to integer and then a string
        // representation. Parse unsigned, then reinterpret the bit
        // pattern as signed (NNW `1df4cf7d3`): the hex form is the
        // two's-complement encoding of a possibly-negative int64, and
        // the signed decimal is the canonical uniqueID every other
        // conversion starts from. A signed parse would return None on
        // high-bit IDs and leak the whole tag string through as the ID.
        if let Ok(id_number) = u64::from_str_radix(id_part, 16) {
            (id_number as i64).to_string()
        } else {
            id_part.to_string()
        }
    }
}

/// Long-form item parameter for an articleID —
/// `tag:google.com,2005:reader/item/000000000004c608`. The long form is
/// zero-padded 16-digit hex, two's complement for negative IDs. Returns
/// `None` for an articleID that can't be encoded, which callers skip
/// (NNW's `itemIDParameter`, `0833627ee`: the old encoding was
/// sign-blind and unpadded, sending negative IDs through raw).
fn item_id_parameter(article_id: &str) -> Option<String> {
    let id_value: i64 = article_id.parse().ok()?;
    Some(format!(
        "tag:google.com,2005:reader/item/{:016x}",
        id_value.cast_unsigned()
    ))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntrySummary {
    pub content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryOrigin {
    #[serde(rename = "streamId")]
    pub stream_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryAlternate {
    #[serde(rename = "href")]
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryWrapper {
    #[serde(rename = "items")]
    pub entries: Vec<ReaderAPIEntry>,
}

enum ReaderAPIEndpoints {
    Login,
    Token,
    DisableTag,
    RenameTag,
    TagList,
    SubscriptionList,
    SubscriptionEdit,
    SubscriptionAdd,
    Contents,
    ItemIds,
    EditTag,
    SubscriptionImport,
}

impl ReaderAPIEndpoints {
    fn path(&self) -> &'static str {
        match self {
            Self::Login => "/accounts/ClientLogin",
            Self::Token => "/reader/api/0/token",
            Self::DisableTag => "/reader/api/0/disable-tag",
            Self::RenameTag => "/reader/api/0/rename-tag",
            Self::TagList => "/reader/api/0/tag/list",
            Self::SubscriptionList => "/reader/api/0/subscription/list",
            Self::SubscriptionEdit => "/reader/api/0/subscription/edit",
            Self::SubscriptionAdd => "/reader/api/0/subscription/quickadd",
            Self::Contents => "/reader/api/0/stream/items/contents",
            Self::ItemIds => "/reader/api/0/stream/items/ids",
            Self::EditTag => "/reader/api/0/edit-tag",
            Self::SubscriptionImport => "/reader/api/0/subscription/import",
        }
    }
}

pub enum ItemIDType {
    Unread,
    Starred,
    AllForAccount,
    AllForFeed,
}

/// Inoreader's per-zone API usage, reported on most responses. Zone 1 is
/// reads. Port of NNW `ReaderAPIUsageLimits` (`2cd0e1781`).
/// <https://www.inoreader.com/developers/rate-limiting>
#[derive(Debug, Clone)]
pub struct ReaderAPIUsageLimits {
    pub zone1_usage: i64,
    pub zone1_limit: i64,
    /// When the daily limits reset: now + `X-Reader-Limits-Reset-After`
    /// (defaulting to 24 h when the header is absent, like upstream).
    pub reset_at: chrono::DateTime<chrono::Utc>,
}

/// Etag / Last-Modified pair sent as the account-level conditional GET on
/// the subscription and tag lists (the shape of NNW
/// `HTTPConditionalGetInfo`, keyed "subscriptions" / "tags" there; we
/// persist both under `db_info` keys).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalGetHeaders {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Result of a conditional-GET list call. `items: None` means 304 Not
/// Modified: the caller must skip the reconcile rather than treat the
/// list as empty, exactly why NNW's caller returns nil there.
pub struct ReaderAPIListResult<T> {
    pub items: Option<Vec<T>>,
    /// Headers to store for the next conditional GET. Unpopulated after
    /// a 304.
    pub conditional_get: ConditionalGetHeaders,
}

/// Inoreader's rate-limit response headers.
mod usage_limit_header {
    pub const ZONE1_USAGE: &str = "X-Reader-Zone1-Usage";
    pub const ZONE1_LIMIT: &str = "X-Reader-Zone1-Limit";
    pub const RESET_AFTER: &str = "X-Reader-Limits-Reset-After";
}

/// Upstream's fallback when `X-Reader-Limits-Reset-After` is absent
/// (NNW `defaultUsageLimitsResetAfter`): one day, the limits' natural
/// reset cadence.
const DEFAULT_USAGE_LIMITS_RESET_AFTER_SECS: i64 = 60 * 60 * 24;

fn header_string(
    headers: &reqwest::header::HeaderMap,
    name: impl reqwest::header::AsHeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn header_i64(
    headers: &reqwest::header::HeaderMap,
    name: impl reqwest::header::AsHeaderName,
) -> Option<i64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
}

/// Pure parse of the usage-limit header triple; `None` unless both usage
/// and a positive limit are present (upstream guards `limit > 0` too).
fn parse_usage_limits(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<ReaderAPIUsageLimits> {
    let zone1_usage = header_i64(headers, usage_limit_header::ZONE1_USAGE)?;
    let zone1_limit = header_i64(headers, usage_limit_header::ZONE1_LIMIT)?;
    if zone1_limit <= 0 {
        return None;
    }
    let reset_after = header_i64(headers, usage_limit_header::RESET_AFTER)
        .unwrap_or(DEFAULT_USAGE_LIMITS_RESET_AFTER_SECS)
        .max(0);
    Some(ReaderAPIUsageLimits {
        zone1_usage,
        zone1_limit,
        reset_at: now + chrono::Duration::seconds(reset_after),
    })
}

/// The sync pause's fallback when a 429 omits (or sends an unusable)
/// `Retry-After`: one hour, NNW `SyncRateLimiter.defaultRetryAfter`
/// (`fafd5bd80`). Deliberately longer than the feed fetcher's 10-minute
/// 429 cooldown (`c9bd65b1f`): this quota is shared per application, not
/// per feed, so backing off harder is the point.
pub(crate) const SYNC_DEFAULT_RETRY_AFTER_SECS: i64 = 60 * 60;

/// Parsed `Retry-After` for the sync pause; the 1-hour default applies
/// when the header is absent, unparseable, or non-positive. (An HTTP-date
/// form is legal but not parsed, matching the feed fetcher's handling.)
fn sync_retry_after_secs(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(SYNC_DEFAULT_RETRY_AFTER_SECS) as u64
}

/// Map a non-success response onto the error type, preserving a real 429's
/// identity and parsed Retry-After so the sync delegate can arm its pause.
/// Every other status becomes `HttpStatus`. The placeholder
/// `RateLimited { retry_after_secs: 0 }` values older call sites return
/// for unrelated failures are distinguishable by convention: this parser
/// always yields a positive number, so only a genuine 429 ever arms the
/// pause.
fn status_error(resp: &reqwest::Response) -> ViaductError {
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        ViaductError::Network(NetworkError::RateLimited {
            retry_after_secs: sync_retry_after_secs(resp.headers()),
        })
    } else {
        ViaductError::Network(NetworkError::HttpStatus(status.as_u16()))
    }
}

/// Attach stored conditional-GET markers to a list request.
fn apply_conditional_get(
    request: reqwest::RequestBuilder,
    conditional_get: &ConditionalGetHeaders,
) -> reqwest::RequestBuilder {
    let request = match &conditional_get.etag {
        Some(etag) => request.header(reqwest::header::IF_NONE_MATCH, etag),
        None => request,
    };
    match &conditional_get.last_modified {
        Some(last_modified) => request.header(reqwest::header::IF_MODIFIED_SINCE, last_modified),
        None => request,
    }
}

/// Extract the next round's markers from a list response. Missing headers
/// become `None`, which sends nothing next time (a full GET, like
/// upstream's `HTTPConditionalGetInfo` with absent values).
fn conditional_get_from_response(headers: &reqwest::header::HeaderMap) -> ConditionalGetHeaders {
    ConditionalGetHeaders {
        etag: header_string(headers, reqwest::header::ETAG),
        last_modified: header_string(headers, reqwest::header::LAST_MODIFIED),
    }
}

pub struct ReaderAPICaller {
    client: reqwest::Client,
    variant: ReaderAPIVariant,
    access_token: tokio::sync::RwLock<Option<String>>,
    /// Most recent Zone 1 usage report, refreshed from the rate-limit
    /// headers on every sync-path response. `None` for services that
    /// don't send them.
    usage_limits: tokio::sync::RwLock<Option<ReaderAPIUsageLimits>>,
}

impl ReaderAPICaller {
    pub fn new(variant: ReaderAPIVariant) -> Self {
        Self {
            client: reqwest::Client::new(),
            variant,
            access_token: tokio::sync::RwLock::new(None),
            usage_limits: tokio::sync::RwLock::new(None),
        }
    }

    /// Record the usage-limit headers of a response, if present. Mirrors
    /// NNW `noteUsageLimits(from:)`.
    fn note_usage_limits(&self, headers: &reqwest::header::HeaderMap) {
        if let Some(limits) = parse_usage_limits(headers, chrono::Utc::now())
            && let Ok(mut slot) = self.usage_limits.try_write()
        {
            *slot = Some(limits);
        }
    }

    /// The latest usage report, if any.
    pub async fn usage_limits(&self) -> Option<ReaderAPIUsageLimits> {
        self.usage_limits.read().await.clone()
    }

    fn api_base_url(&self) -> Url {
        Url::parse(self.variant.host()).expect("Invalid base URL")
    }

    fn add_variant_headers(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.variant == ReaderAPIVariant::Inoreader {
            // These come from compile-time environment variables.
            request = request.header("AppId", option_env!("INOREADER_APP_ID").unwrap_or(""));
            request = request.header("AppKey", option_env!("INOREADER_APP_KEY").unwrap_or(""));
        }
        request
    }

    pub async fn validate_credentials(&self, username: &str, password: &str) -> Result<String> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Login.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let mut request = self
            .client
            .post(url)
            .form(&[("Email", username), ("Passwd", password)]);

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })); // Simplified error
        }

        let body = resp
            .text()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        for line in body.lines() {
            if let Some(auth) = line.strip_prefix("Auth=") {
                return Ok(auth.to_string());
            }
        }

        Err(ViaductError::Network(NetworkError::RateLimited {
            retry_after_secs: 0,
        })) // Simplified error
    }

    pub async fn request_authorization_token(&self, auth_token: &str) -> Result<String> {
        {
            let token = self.access_token.read().await;
            if let Some(t) = &*token {
                return Ok(t.clone());
            }
        }

        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Token.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let mut request = self
            .client
            .get(url)
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        // Check status before caching: an error page (401 expired auth, 5xx)
        // must not be stored as the edit token, or every later write reuses
        // the garbage token until the process restarts. Mirrors the guard in
        // validate_credentials.
        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }

        let token = resp
            .text()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?
            .trim()
            .to_string();

        if token.is_empty() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }

        let mut write_token = self.access_token.write().await;
        *write_token = Some(token.clone());
        Ok(token)
    }

    /// Sends a token-authenticated request, dropping the cached edit token
    /// and retrying once on 401/403. Port of NNW `withWriteToken`.
    ///
    /// Inoreader's edit tokens are short-lived but we cache ours for the
    /// life of the process, so a token that expired mid-session used to
    /// fail every write (and the article-body fetch) until the app was
    /// restarted. `build` receives the token and returns the finished
    /// request; it runs a second time on the retry so the fresh token
    /// reaches the body, where these endpoints expect it as `T`.
    async fn with_write_token<F>(&self, auth_token: &str, build: F) -> Result<reqwest::Response>
    where
        F: Fn(String) -> reqwest::RequestBuilder,
    {
        let token = self.request_authorization_token(auth_token).await?;
        let resp = build(token)
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        let status = resp.status();
        if status != reqwest::StatusCode::UNAUTHORIZED && status != reqwest::StatusCode::FORBIDDEN {
            return Ok(resp);
        }

        *self.access_token.write().await = None;
        let fresh = self.request_authorization_token(auth_token).await?;
        build(fresh)
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))
    }

    pub async fn retrieve_subscriptions(
        &self,
        auth_token: &str,
        conditional_get: &ConditionalGetHeaders,
    ) -> Result<ReaderAPIListResult<ReaderAPISubscription>> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionList.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let request = self
            .client
            .get(url)
            .query(&[("output", "json")])
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        let request = apply_conditional_get(request, conditional_get);
        let request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        // A 304 comes back with an empty body: report it as no list so the
        // caller skips the reconcile (NNW returns nil for the same reason).
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(ReaderAPIListResult {
                items: None,
                conditional_get: ConditionalGetHeaders::default(),
            });
        }
        if !resp.status().is_success() {
            return Err(status_error(&resp));
        }
        self.note_usage_limits(resp.headers());
        let next_conditional_get = conditional_get_from_response(resp.headers());

        let container: ReaderAPISubscriptionContainer = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(ReaderAPIListResult {
            items: Some(container.subscriptions),
            conditional_get: next_conditional_get,
        })
    }

    pub async fn retrieve_tags(
        &self,
        auth_token: &str,
        conditional_get: &ConditionalGetHeaders,
    ) -> Result<ReaderAPIListResult<ReaderAPITag>> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::TagList.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let request = self
            .client
            .get(url)
            .query(&[("output", "json")])
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        let request = if self.variant == ReaderAPIVariant::Inoreader {
            request.query(&[("types", "1")])
        } else {
            request
        };

        let request = apply_conditional_get(request, conditional_get);
        let request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(ReaderAPIListResult {
                items: None,
                conditional_get: ConditionalGetHeaders::default(),
            });
        }
        if !resp.status().is_success() {
            return Err(status_error(&resp));
        }
        self.note_usage_limits(resp.headers());
        let next_conditional_get = conditional_get_from_response(resp.headers());

        let container: ReaderAPITagContainer = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(ReaderAPIListResult {
            items: Some(container.tags),
            conditional_get: next_conditional_get,
        })
    }

    pub async fn create_subscription(
        &self,
        auth_token: &str,
        url: &str,
    ) -> Result<ReaderAPISubscription> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionAdd.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[("T", token), ("quickadd", url.to_string())]);
                self.add_variant_headers(request)
            })
            .await?;

        let result: ReaderAPIQuickAddResult = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if result.num_results == 0 {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })); // Simplified error
        }

        let subscriptions = self
            .retrieve_subscriptions(auth_token, &ConditionalGetHeaders::default())
            .await?
            .items
            .unwrap_or_default();
        subscriptions
            .into_iter()
            .find(|s| s.feed_id == result.stream_id)
            .ok_or_else(|| {
                ViaductError::Network(NetworkError::RateLimited {
                    retry_after_secs: 0,
                })
            }) // Simplified error
    }

    pub async fn delete_subscription(&self, auth_token: &str, subscription_id: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionEdit.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[
                        ("T", token),
                        ("s", subscription_id.to_string()),
                        ("ac", "unsubscribe".to_string()),
                    ]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    async fn change_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        remove_tag_name: Option<&str>,
        add_tag_name: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionEdit.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T", token),
                    ("s", subscription_id.to_string()),
                    ("ac", "edit".to_string()),
                ];

                if let Some(name) = remove_tag_name {
                    params.push(("r", format!("user/-/label/{}", name)));
                }
                if let Some(name) = add_tag_name {
                    params.push(("a", format!("user/-/label/{}", name)));
                }
                if let Some(t) = title {
                    params.push(("t", t.to_string()));
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn rename_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        new_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, None, None, Some(new_name))
            .await
    }

    pub async fn create_tagging(
        &self,
        auth_token: &str,
        subscription_id: &str,
        tag_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, None, Some(tag_name), None)
            .await
    }

    pub async fn delete_tagging(
        &self,
        auth_token: &str,
        subscription_id: &str,
        tag_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, Some(tag_name), None, None)
            .await
    }

    pub async fn move_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        source_tag: &str,
        dest_tag: &str,
    ) -> Result<()> {
        self.change_subscription(
            auth_token,
            subscription_id,
            Some(source_tag),
            Some(dest_tag),
            None,
        )
        .await
    }

    pub async fn rename_tag(&self, auth_token: &str, old_name: &str, new_name: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::RenameTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[
                        ("T", token),
                        ("s", format!("user/-/label/{}", old_name)),
                        ("dest", format!("user/-/label/{}", new_name)),
                    ]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn delete_tag(&self, auth_token: &str, folder_external_id: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::DisableTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[("T", token), ("s", folder_external_id.to_string())]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn retrieve_entries(
        &self,
        auth_token: &str,
        article_ids: &[String],
    ) -> Result<Vec<ReaderAPIEntry>> {
        if article_ids.is_empty() {
            return Ok(Vec::new());
        }

        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Contents.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T".to_string(), token),
                    ("output".to_string(), "json".to_string()),
                ];

                for id in article_ids {
                    // Inoreader (and others) often want hex IDs for some reason in these calls.
                    // NNW converts decimal IDs to hex.
                    if let Some(param) = item_id_parameter(id) {
                        params.push(("i".to_string(), param));
                    }
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(status_error(&resp));
        }
        self.note_usage_limits(resp.headers());

        let wrapper: ReaderAPIEntryWrapper = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(wrapper.entries)
    }

    pub async fn retrieve_item_ids(
        &self,
        auth_token: &str,
        request_type: ItemIDType,
        feed_id: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut results = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut query = vec![("n", "1000".to_string()), ("output", "json".to_string())];

            match request_type {
                ItemIDType::AllForAccount => {
                    query.push(("s", "user/-/state/com.google/reading-list".to_string()));
                }
                ItemIDType::AllForFeed => {
                    if let Some(fid) = feed_id {
                        query.push(("s", fid.to_string()));
                    } else {
                        return Err(ViaductError::Network(NetworkError::RateLimited {
                            retry_after_secs: 0,
                        }));
                    }
                }
                ItemIDType::Unread => {
                    query.push(("s", "user/-/state/com.google/reading-list".to_string()));
                    query.push(("xt", "user/-/state/com.google/read".to_string()));
                }
                ItemIDType::Starred => {
                    query.push(("s", "user/-/state/com.google/starred".to_string()));
                }
            }

            if let Some(c) = &continuation {
                query.push(("c", c.clone()));
            }

            let url = self
                .api_base_url()
                .join(ReaderAPIEndpoints::ItemIds.path())
                .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
            let mut request = self
                .client
                .get(url)
                .query(&query)
                .header("Authorization", format!("GoogleLogin auth={}", auth_token));

            request = self.add_variant_headers(request);

            let resp = request
                .send()
                .await
                .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

            if !resp.status().is_success() {
                return Err(status_error(&resp));
            }
            self.note_usage_limits(resp.headers());

            let wrapper: ReaderAPIReferenceWrapper = resp
                .json()
                .await
                .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

            for reference in wrapper.item_refs {
                results.push(reference.item_id);
            }

            continuation = wrapper.continuation;
            if continuation.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn update_state_to_entries(
        &self,
        auth_token: &str,
        article_ids: &[String],
        state: &str,
        add: bool,
    ) -> Result<()> {
        if article_ids.is_empty() {
            return Ok(());
        }

        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::EditTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let action = if add { "a" } else { "r" };

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T".to_string(), token),
                    (action.to_string(), state.to_string()),
                ];

                for id in article_ids {
                    if let Some(param) = item_id_parameter(id) {
                        params.push(("i".to_string(), param));
                    }
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(status_error(&resp));
        }
        self.note_usage_limits(resp.headers());
        Ok(())
    }

    pub async fn import_opml(&self, auth_token: &str, opml_data: &[u8]) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionImport.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let mut request = self
            .client
            .post(endpoint)
            .header("Authorization", format!("GoogleLogin auth={}", auth_token))
            .header("Content-Type", "text/xml")
            .body(opml_data.to_vec());

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(article_id: &str) -> ReaderAPIEntry {
        ReaderAPIEntry {
            article_id: article_id.to_string(),
            title: None,
            author: None,
            summary: ReaderAPIEntrySummary { content: None },
            published_timestamp: None,
            alternates: None,
            categories: None,
            origin: ReaderAPIEntryOrigin {
                stream_id: None,
                title: None,
            },
        }
    }

    #[test]
    fn low_id_converts_to_decimal() {
        let entry = make_entry("tag:google.com,2005:reader/item/00058b10ce338909");
        assert_eq!(entry.unique_id(), "1560279178774793");
    }

    #[test]
    fn high_bit_id_does_not_overflow() {
        // The signed parse in NNW's original returned nil here, leaking
        // the whole tag string through as the ID; ours parsed unsigned
        // and stored a huge positive decimal where the canonical form
        // is the signed reinterpretation.
        let entry = make_entry("tag:google.com,2005:reader/item/ffffffffffffcdef");
        assert_eq!(entry.unique_id(), "-12817");
    }

    #[test]
    fn zero_id() {
        let entry = make_entry("tag:google.com,2005:reader/item/0000000000000000");
        assert_eq!(entry.unique_id(), "0");
    }

    #[test]
    fn non_hex_id_returns_raw_part() {
        let article_id = "tag:google.com,2005:reader/item/notavalidhexvalue";
        let entry = make_entry(article_id);
        assert_eq!(entry.unique_id(), "notavalidhexvalue");
    }

    #[test]
    fn decimal_round_trips_to_original_hex() {
        for hex in ["00058b10ce338909", "ffffffffffffcdef", "0000000000000000"] {
            let entry = make_entry(&format!("tag:google.com,2005:reader/item/{hex}"));
            let decimal = entry.unique_id();
            let value: i64 = decimal.parse().unwrap();
            assert_eq!(format!("{:016x}", value.cast_unsigned()), hex);
        }
    }

    #[test]
    fn item_id_parameter_zero_pads_and_handles_negatives() {
        assert_eq!(
            item_id_parameter("1560279178774793").unwrap(),
            "tag:google.com,2005:reader/item/00058b10ce338909"
        );
        // NNW `0833627ee`: negative IDs encode as two's complement, not
        // raw, and the form is always 16 digits.
        assert_eq!(
            item_id_parameter("-12817").unwrap(),
            "tag:google.com,2005:reader/item/ffffffffffffcdef"
        );
        assert_eq!(
            item_id_parameter("0").unwrap(),
            "tag:google.com,2005:reader/item/0000000000000000"
        );
        assert_eq!(item_id_parameter("feed/not-a-number"), None);
    }

    fn headers_from(pairs: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("valid name"),
                reqwest::header::HeaderValue::from_str(value).expect("valid value"),
            );
        }
        headers
    }

    /// NNW `fafd5bd80`: a 429 without a usable `Retry-After` still arms
    /// the pause, for one hour, not the feed fetcher's 10 minutes.
    #[test]
    fn sync_retry_after_defaults_to_one_hour_when_unusable() {
        assert_eq!(
            sync_retry_after_secs(&headers_from(&[("Retry-After", "120")])),
            120
        );
        assert_eq!(
            sync_retry_after_secs(&headers_from(&[])),
            SYNC_DEFAULT_RETRY_AFTER_SECS as u64
        );
        assert_eq!(
            sync_retry_after_secs(&headers_from(&[("Retry-After", "-5")])),
            SYNC_DEFAULT_RETRY_AFTER_SECS as u64
        );
        assert_eq!(
            sync_retry_after_secs(&headers_from(&[("Retry-After", " 90 ")])),
            90
        );
    }

    /// NNW `2cd0e1781`: the Zone 1 triple parses into the usage struct,
    /// with upstream's 24-hour default when the reset header is absent.
    #[test]
    fn usage_limits_parse_from_inoreader_headers() {
        let now = chrono::Utc::now();
        let full = parse_usage_limits(
            &headers_from(&[
                (usage_limit_header::ZONE1_USAGE, "950"),
                (usage_limit_header::ZONE1_LIMIT, "1000"),
                (usage_limit_header::RESET_AFTER, "3600"),
            ]),
            now,
        )
        .expect("parses");
        assert_eq!(full.zone1_usage, 950);
        assert_eq!(full.zone1_limit, 1000);
        assert_eq!(
            (full.reset_at - now).num_minutes(),
            60,
            "reset-after header wins over the default"
        );

        let defaulted = parse_usage_limits(
            &headers_from(&[
                (usage_limit_header::ZONE1_USAGE, "10"),
                (usage_limit_header::ZONE1_LIMIT, "200"),
            ]),
            now,
        )
        .expect("parses");
        assert!(
            (defaulted.reset_at - now).num_seconds() - DEFAULT_USAGE_LIMITS_RESET_AFTER_SECS <= 1,
            "absent reset header falls back to 24 h"
        );
    }

    /// Upstream guards `limit > 0` and requires both usage headers;
    /// a partial or degenerate triple reports nothing rather than a
    /// bogus limit.
    #[test]
    fn usage_limits_require_both_headers_and_positive_limit() {
        let now = chrono::Utc::now();
        assert!(parse_usage_limits(&headers_from(&[]), now).is_none());
        assert!(
            parse_usage_limits(
                &headers_from(&[(usage_limit_header::ZONE1_LIMIT, "100")]),
                now
            )
            .is_none()
        );
        assert!(
            parse_usage_limits(
                &headers_from(&[
                    (usage_limit_header::ZONE1_USAGE, "1"),
                    (usage_limit_header::ZONE1_LIMIT, "0"),
                ]),
                now
            )
            .is_none()
        );
        assert!(
            parse_usage_limits(
                &headers_from(&[
                    (usage_limit_header::ZONE1_USAGE, "1"),
                    (usage_limit_header::ZONE1_LIMIT, "not-a-number"),
                ]),
                now
            )
            .is_none()
        );
    }

    #[test]
    fn conditional_get_round_trips_through_headers() {
        let source = headers_from(&[
            (reqwest::header::ETAG.as_str(), "\"abc-123\""),
            (
                reqwest::header::LAST_MODIFIED.as_str(),
                "Mon, 24 Aug 2026 12:00:00 GMT",
            ),
        ]);
        let extracted = conditional_get_from_response(&source);
        assert_eq!(extracted.etag.as_deref(), Some("\"abc-123\""));
        assert_eq!(
            extracted.last_modified.as_deref(),
            Some("Mon, 24 Aug 2026 12:00:00 GMT")
        );

        // The markers ride the next request as the conditional-GET pair.
        let built = apply_conditional_get(
            reqwest::Client::new().get("http://localhost/reader/api/0/tag/list"),
            &extracted,
        )
        .build()
        .expect("builds");
        let sent = built.headers();
        assert_eq!(
            sent.get(reqwest::header::IF_NONE_MATCH).unwrap(),
            "\"abc-123\""
        );
        assert_eq!(
            sent.get(reqwest::header::IF_MODIFIED_SINCE).unwrap(),
            "Mon, 24 Aug 2026 12:00:00 GMT"
        );

        // No stored markers = no conditional headers = a full GET.
        let bare = apply_conditional_get(
            reqwest::Client::new().get("http://localhost/reader/api/0/tag/list"),
            &ConditionalGetHeaders::default(),
        )
        .build()
        .expect("builds");
        assert!(bare.headers().get(reqwest::header::IF_NONE_MATCH).is_none());
    }
}

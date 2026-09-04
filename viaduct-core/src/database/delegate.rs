use crate::database::accounts::Account;
use crate::error::{NetworkError, Result, ViaductError};
use crate::network::credentials::fetch_credentials;
use crate::network::inoreader::{
    ConditionalGetHeaders, ItemIDType, ReaderAPICaller, ReaderAPIListResult, ReaderAPIVariant,
};
use std::sync::Arc;

/// Fraction of the reported Zone 1 daily limit at which status downloads
/// stop until the limits reset (NNW `zone1UsageThreshold`, `2cd0e1781`).
const ZONE1_USAGE_THRESHOLD: f64 = 0.9;

/// Pure quota test so the threshold boundary is unit-tested without a
/// caller. True when `usage` is at or above `ZONE1_USAGE_THRESHOLD` of a
/// positive `limit`.
fn zone1_usage_near_limit(usage: i64, limit: i64) -> bool {
    limit > 0 && usage as f64 >= limit as f64 * ZONE1_USAGE_THRESHOLD
}

/// Pauses syncing after a rate-limit response until the server's
/// `Retry-After` (or the default) elapses. Port of NNW `SyncRateLimiter`
/// (`fafd5bd80`) in its ReaderAPI posture: only 429 arms the pause
/// (upstream passes `treatsForbiddenAsRateLimited: false`; a 403 here is
/// an auth/token problem that `with_write_token` already retries).
struct SyncRateLimiter {
    resume_at: tokio::sync::RwLock<Option<chrono::DateTime<chrono::Utc>>>,
}

impl SyncRateLimiter {
    fn new() -> Self {
        Self {
            resume_at: tokio::sync::RwLock::new(None),
        }
    }

    /// True while a rate-limit pause is in force; an expired pause
    /// clears itself so the next cycle runs normally.
    async fn should_skip(&self) -> bool {
        let mut guard = self.resume_at.write().await;
        match *guard {
            None => false,
            Some(resume_at) if resume_at > chrono::Utc::now() => true,
            Some(_) => {
                *guard = None;
                false
            }
        }
    }

    /// Arm (or extend) the pause. Returns the resume time for logging.
    async fn note_rate_limited(&self, retry_after_secs: u64) -> chrono::DateTime<chrono::Utc> {
        let resume_at = chrono::Utc::now() + chrono::Duration::seconds(retry_after_secs as i64);
        tracing::warn!(
            ?resume_at,
            "inoreader: rate limited; pausing syncing until then"
        );
        *self.resume_at.write().await = Some(resume_at);
        resume_at
    }

    /// The Retry-After of an error that arms a pause. The one shape that
    /// qualifies: a real 429 mapped by `inoreader::status_error`, whose
    /// `retry_after_secs` is always the parsed header or a positive
    /// default. The placeholder `RateLimited { retry_after_secs: 0 }`
    /// values older call sites return for unrelated failures (missing
    /// credentials, an unparseable login response) never pause, so a
    /// broken keyring can't stall sync.
    fn retry_after_of(err: &ViaductError) -> Option<u64> {
        match err {
            ViaductError::Network(NetworkError::RateLimited { retry_after_secs })
                if *retry_after_secs > 0 =>
            {
                Some(*retry_after_secs)
            }
            _ => None,
        }
    }
}

/// `db_info` key prefixes for the account-level conditional-GET markers
/// on the sync's two list calls (NNW keys these "subscriptions" /
/// "tags" in its account settings; our account-level store is the
/// articles `db_info` table). Each carries `-etag` and `-last-modified`
/// suffixes; an empty value means "absent".
const SYNC_CGET_SUBSCRIPTIONS: &str = "sync-cget-subscriptions";
const SYNC_CGET_TAGS: &str = "sync-cget-tags";

async fn load_conditional_get(account: &Account, key: &str) -> Result<ConditionalGetHeaders> {
    let etag = account
        .db_info_get(&format!("{key}-etag"))
        .await?
        .unwrap_or_default();
    let last_modified = account
        .db_info_get(&format!("{key}-last-modified"))
        .await?
        .unwrap_or_default();
    Ok(ConditionalGetHeaders {
        etag: (!etag.is_empty()).then_some(etag),
        last_modified: (!last_modified.is_empty()).then_some(last_modified),
    })
}

/// Store the next round's markers. Called only after the reconcile has
/// been applied (the v3.4.0 `5e33560da` ordering rule: never commit a
/// marker for content that wasn't ingested, or a 304 could hide the list
/// from a future sync).
async fn store_conditional_get(
    account: &Account,
    key: &str,
    headers: &ConditionalGetHeaders,
) -> Result<()> {
    account
        .db_info_set(
            &format!("{key}-etag"),
            headers.etag.as_deref().unwrap_or(""),
        )
        .await?;
    account
        .db_info_set(
            &format!("{key}-last-modified"),
            headers.last_modified.as_deref().unwrap_or(""),
        )
        .await
}

pub trait AccountDelegate: Send + Sync {
    fn refresh_all(
        &self,
        account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
    fn sync_article_status(
        &self,
        account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
    fn import_opml(
        &self,
        account: Arc<Account>,
        path: &std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>,
    >;
    /// Whether this delegate is the local-only one. Used by v2.6.5
    /// `cleanup_at_startup` to decide if `syncStatus` rows are by
    /// definition ghost (yes for local — the table is only used by
    /// remote-sync delegates, so any row here is leftover from a
    /// previous Inoreader session).
    fn is_local(&self) -> bool {
        false
    }
}

pub struct LocalAccountDelegate;

impl AccountDelegate for LocalAccountDelegate {
    fn refresh_all(
        &self,
        _account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn sync_article_status(
        &self,
        _account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }

    fn import_opml(
        &self,
        account: Arc<Account>,
        path: &std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>,
    > {
        let path = path.to_path_buf();
        Box::pin(async move { account.import_opml_internal(&path).await })
    }

    fn is_local(&self) -> bool {
        true
    }
}

pub struct InoreaderAccountDelegate {
    caller: ReaderAPICaller,
    rate_limiter: SyncRateLimiter,
}

impl Default for InoreaderAccountDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl InoreaderAccountDelegate {
    pub fn new() -> Self {
        Self {
            caller: ReaderAPICaller::new(ReaderAPIVariant::Inoreader),
            rate_limiter: SyncRateLimiter::new(),
        }
    }

    /// True when Inoreader's Zone 1 (read) usage is close enough to the
    /// daily limit that the status downloads should be skipped until the
    /// limits reset. Port of NNW `shouldSkipStatusDownloadsToConserveQuota`
    /// (`2cd0e1781`).
    async fn should_skip_status_downloads_to_conserve_quota(&self) -> bool {
        let Some(limits) = self.caller.usage_limits().await else {
            return false;
        };
        if limits.reset_at <= chrono::Utc::now() {
            return false;
        }
        if !zone1_usage_near_limit(limits.zone1_usage, limits.zone1_limit) {
            return false;
        }
        tracing::info!(
            usage = limits.zone1_usage,
            limit = limits.zone1_limit,
            "inoreader: skipping status downloads; Zone 1 API usage near the daily limit"
        );
        true
    }

    async fn get_auth_token(&self) -> Result<String> {
        let creds = fetch_credentials("inoreader").await?.ok_or_else(|| {
            ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })
        })?; // Simplified error

        let password = creds.password.ok_or_else(|| {
            ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })
        })?;
        self.caller
            .validate_credentials(&creds.username, &password)
            .await
    }

    /// The four-phase sync body. Kept separate from `refresh_all` so the
    /// rate-limit catch can wrap every phase without re-nesting the
    /// steps (NNW wraps `refreshAccount` the same way).
    async fn refresh_all_inner(&self, account: &Arc<Account>, auth_token: &str) -> Result<()> {
        // 1. Sync Folders and Feeds. Both lists ride account-level
        // conditional GET (NNW `e69cb835a`); a 304 skips the reconcile
        // rather than reconciling against a half-present server picture.
        let sub_cget = load_conditional_get(account, SYNC_CGET_SUBSCRIPTIONS).await?;
        let tags_cget = load_conditional_get(account, SYNC_CGET_TAGS).await?;
        let subscriptions = self
            .caller
            .retrieve_subscriptions(auth_token, &sub_cget)
            .await?;
        let tags = self.caller.retrieve_tags(auth_token, &tags_cget).await?;

        let ReaderAPIListResult {
            items: subscription_items,
            conditional_get: subscription_cget,
        } = subscriptions;
        let ReaderAPIListResult {
            items: tag_items,
            conditional_get: tag_cget,
        } = tags;

        match (subscription_items, tag_items) {
            (Some(subscriptions), Some(tags)) => {
                let existing_opml = account.load_opml().await?;
                let synced_opml = crate::database::opml::sync_inoreader_account(
                    &existing_opml,
                    subscriptions,
                    tags,
                );
                account.save_opml(synced_opml).await?;

                // Markers commit only after the reconcile is applied, so an
                // interrupted cycle can't 304 away its own retry (the
                // v3.4.0 `5e33560da` ordering rule, list-shaped).
                store_conditional_get(account, SYNC_CGET_SUBSCRIPTIONS, &subscription_cget).await?;
                store_conditional_get(account, SYNC_CGET_TAGS, &tag_cget).await?;
            }
            // A 304 proves the list is unchanged server-side, so skipping
            // loses nothing; the next cycle reconciles when a list changes.
            _ => {
                tracing::debug!(
                    "inoreader: subscription/tag lists unmodified (304); reconcile skipped"
                );
            }
        }

        // 2. Send Article Status (Local -> Remote)
        let sync_statuses = account.select_sync_statuses_for_processing(None).await?;
        if !sync_statuses.is_empty() {
            let mut read_ids = Vec::new();
            let mut unread_ids = Vec::new();
            let mut starred_ids = Vec::new();
            let mut unstarred_ids = Vec::new();

            for s in &sync_statuses {
                match (s.key.as_str(), s.flag) {
                    ("read", true) => read_ids.push(s.article_id.clone()),
                    ("read", false) => unread_ids.push(s.article_id.clone()),
                    ("starred", true) => starred_ids.push(s.article_id.clone()),
                    ("starred", false) => unstarred_ids.push(s.article_id.clone()),
                    _ => {}
                }
            }

            // A failed status send must not abort the refresh: the status
            // pull and article fetch below still need to run, or the
            // account silently stops updating until a send happens to
            // succeed. Failed rows stay selected and step 3's
            // reset-all re-arms them for the next cycle.
            // <https://discourse.netnewswire.com/t/no-feed-updates/336>
            // Each landed batch is deleted key-scoped (NNW
            // `e5171cbb0`): clearing a read row must not drop the
            // same article's queued starred row.
            let batches: [(&[String], &str, &str, bool); 4] = [
                (&read_ids, "read", "user/-/state/com.google/read", true),
                (&unread_ids, "read", "user/-/state/com.google/read", false),
                (
                    &starred_ids,
                    "starred",
                    "user/-/state/com.google/starred",
                    true,
                ),
                (
                    &unstarred_ids,
                    "starred",
                    "user/-/state/com.google/starred",
                    false,
                ),
            ];
            for (ids, key, state, flag) in batches {
                if ids.is_empty() {
                    continue;
                }
                match self
                    .caller
                    .update_state_to_entries(auth_token, ids, state, flag)
                    .await
                {
                    Ok(()) => {
                        let pairs: Vec<(String, String)> =
                            ids.iter().map(|id| (id.clone(), key.to_string())).collect();
                        if let Err(e) = account
                            .delete_sync_statuses_selected_for_processing(pairs)
                            .await
                        {
                            tracing::warn!(
                                ?e,
                                key,
                                "landed status rows could not be cleared; reset-all will re-arm them"
                            );
                        }
                    }
                    Err(e) => {
                        if let Some(retry_after_secs) = SyncRateLimiter::retry_after_of(&e) {
                            // The shared per-application quota is
                            // burning: arm the pause and stop sending
                            // further batches this cycle (NNW
                            // `fe285e217` pauses on the first 429).
                            self.rate_limiter.note_rate_limited(retry_after_secs).await;
                            break;
                        }
                        tracing::warn!(
                            ?e,
                            state,
                            flag,
                            count = ids.len(),
                            "status send failed; rows stay queued for the next cycle"
                        );
                    }
                }
            }
        }

        // 3. Refresh Article Status (Remote -> Local). The whole
        // reconcile is skipped when Inoreader reports Zone 1 usage
        // near the daily limit (NNW `2cd0e1781` + `e69cb835a`): these
        // item-ID downloads are what burn the read quota, and
        // upstream treats the mark-as-read + unread-download pair as
        // atomic, so we skip or run the whole step, never half.
        if self.should_skip_status_downloads_to_conserve_quota().await {
            tracing::debug!("inoreader: status reconcile skipped for quota");
        } else {
            let remote_unread_ids: std::collections::HashSet<String> = self
                .caller
                .retrieve_item_ids(auth_token, ItemIDType::Unread, None)
                .await?
                .into_iter()
                .collect();
            let remote_starred_ids: std::collections::HashSet<String> = self
                .caller
                .retrieve_item_ids(auth_token, ItemIDType::Starred, None)
                .await?
                .into_iter()
                .collect();

            let pending_statuses = account.select_sync_statuses_for_processing(None).await?;
            let mut pending_read_ids = std::collections::HashSet::new();
            let mut pending_starred_ids = std::collections::HashSet::new();
            for s in &pending_statuses {
                if s.key == "read" {
                    pending_read_ids.insert(s.article_id.clone());
                }
                if s.key == "starred" {
                    pending_starred_ids.insert(s.article_id.clone());
                }
            }
            // Reset them so they can be processed again later if needed (but they are technically processed now)
            account
                .reset_all_sync_statuses_selected_for_processing()
                .await?;

            let local_unread_ids = account.fetch_unread_article_ids().await?;
            let local_starred_ids = account.fetch_starred_article_ids().await?;

            // Updatable = Remote - Pending
            let updatable_remote_unread: Vec<String> = remote_unread_ids
                .iter()
                .filter(|id| !pending_read_ids.contains(*id))
                .cloned()
                .collect();
            let updatable_remote_starred: Vec<String> = remote_starred_ids
                .iter()
                .filter(|id| !pending_starred_ids.contains(*id))
                .cloned()
                .collect();

            // Delta Unread = UpdatableRemoteUnread - LocalUnread
            let delta_unread: Vec<String> = updatable_remote_unread
                .iter()
                .filter(|id| !local_unread_ids.contains(*id))
                .cloned()
                .collect();
            // Delta Read = LocalUnread - UpdatableRemoteUnread
            let delta_read: Vec<String> = local_unread_ids
                .iter()
                .filter(|id| !remote_unread_ids.contains(*id) && !pending_read_ids.contains(*id))
                .cloned()
                .collect();

            // Delta Starred = UpdatableRemoteStarred - LocalStarred
            let delta_starred: Vec<String> = updatable_remote_starred
                .iter()
                .filter(|id| !local_starred_ids.contains(*id))
                .cloned()
                .collect();
            // Delta Unstarred = LocalStarred - UpdatableRemoteStarred
            let delta_unstarred: Vec<String> = local_starred_ids
                .iter()
                .filter(|id| {
                    !remote_starred_ids.contains(*id) && !pending_starred_ids.contains(*id)
                })
                .cloned()
                .collect();

            if !delta_unread.is_empty() {
                account.update_statuses_read(delta_unread, false).await?;
            }
            if !delta_read.is_empty() {
                account.update_statuses_read(delta_read, true).await?;
            }
            if !delta_starred.is_empty() {
                account.update_statuses_starred(delta_starred, true).await?;
            }
            if !delta_unstarred.is_empty() {
                account
                    .update_statuses_starred(delta_unstarred, false)
                    .await?;
            }
        }

        // 4. Refresh Missing Articles
        let missing_ids = account.fetch_missing_article_ids().await?;
        if !missing_ids.is_empty() {
            for chunk in missing_ids.chunks(100) {
                let entries = self.caller.retrieve_entries(auth_token, chunk).await?;
                let mut articles = Vec::new();
                for entry in entries {
                    if let Some(ref stream_id) = entry.origin.stream_id {
                        let stream_id = stream_id.clone();
                        let unique_id = entry.unique_id();
                        let article_id =
                            crate::database::articles::article_id_for(&stream_id, &unique_id);

                        let date_published = entry
                            .published_timestamp
                            .and_then(|ts| chrono::DateTime::from_timestamp(ts as i64, 0));

                        let external_url = entry
                            .alternates
                            .as_ref()
                            .and_then(|a| a.first())
                            .and_then(|a| a.url.clone());

                        let content = entry.summary.content.clone();
                        let authors = entry
                            .author
                            .clone()
                            .map(|name| {
                                vec![crate::models::Author {
                                    name: Some(name),
                                    url: None,
                                    avatar_url: None,
                                    email: None,
                                }]
                            })
                            .unwrap_or_default();

                        articles.push(crate::models::Article {
                            article_id,
                            feed_id: stream_id,
                            title: entry.title,
                            content_html: content.clone(),
                            content_text: None,
                            url: None,
                            external_url,
                            summary: content,
                            image_url: None,
                            date_published,
                            date_modified: None,
                            authors,
                            attachments: Vec::new(),
                        });
                    }
                }
                if !articles.is_empty() {
                    account.batch_insert_articles(articles).await?;
                }
            }
        }

        Ok(())
    }
}

impl AccountDelegate for InoreaderAccountDelegate {
    fn refresh_all(
        &self,
        account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.rate_limiter.should_skip().await {
                // NNW `52e78b29c` logs this skip to its Activity Log; ours is
                // window-owned and unreachable from the delegate, so the
                // pause's warn line carries the resume time instead.
                tracing::info!("inoreader: skipping sync; rate-limit pause still in force");
                return Ok(());
            }
            let auth_token = self.get_auth_token().await?;

            // A 429 anywhere in the cycle pauses syncing instead of failing
            // the refresh, mirroring NNW's
            // `catch where rateLimiter.isRateLimitError(error)`.
            match self.refresh_all_inner(&account, &auth_token).await {
                Err(e) => match SyncRateLimiter::retry_after_of(&e) {
                    Some(retry_after_secs) => {
                        self.rate_limiter.note_rate_limited(retry_after_secs).await;
                        Ok(())
                    }
                    None => Err(e),
                },
                Ok(()) => Ok(()),
            }
        })
    }

    fn sync_article_status(
        &self,
        account: Arc<Account>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        self.refresh_all(account)
    }

    fn import_opml(
        &self,
        account: Arc<Account>,
        path: &std::path::Path,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>,
    > {
        let path = path.to_path_buf();
        Box::pin(async move {
            let auth_token = self.get_auth_token().await?;
            let xml = tokio::fs::read(&path).await?;
            self.caller.import_opml(&auth_token, &xml).await?;
            account.import_opml_internal(&path).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of NNW's SyncRateLimiter semantics: nothing pauses until a
    /// 429 lands; the pause blocks; expiry clears itself.
    #[tokio::test]
    async fn rate_limiter_blocks_until_resume_time_passes() {
        let limiter = SyncRateLimiter::new();
        assert!(
            !limiter.should_skip().await,
            "no pause before the first 429"
        );

        limiter
            .note_rate_limited(crate::network::inoreader::SYNC_DEFAULT_RETRY_AFTER_SECS as u64)
            .await;
        assert!(
            limiter.should_skip().await,
            "pause in force blocks the cycle"
        );

        // Force the pause into the past: an expired pause clears itself
        // and lets the next cycle through (NNW clears resumeDate too).
        *limiter.resume_at.write().await = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
        assert!(!limiter.should_skip().await);
        assert!(
            limiter.resume_at.read().await.is_none(),
            "expired pause is cleared"
        );
    }

    /// retry_after_secs: 0 is the placeholder convention, never a pause.
    /// A broken keyring surfaces as RateLimited { 0 } from
    /// `get_auth_token` and must not stall syncing for an hour.
    #[tokio::test]
    async fn placeholder_rate_limited_zero_never_pauses() {
        assert_eq!(
            SyncRateLimiter::retry_after_of(&ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0
            })),
            None
        );
        assert_eq!(
            SyncRateLimiter::retry_after_of(&ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 600
            })),
            Some(600)
        );
        assert_eq!(
            SyncRateLimiter::retry_after_of(&ViaductError::Network(NetworkError::HttpStatus(500))),
            None
        );

        let limiter = SyncRateLimiter::new();
        limiter.note_rate_limited(0).await;
        assert!(
            !limiter.should_skip().await,
            "a zero-second pause is no pause"
        );
    }

    /// NNW `zone1UsageThreshold` boundary: downloads stop at 90 percent
    /// of the reported limit, and a degenerate limit never trips.
    #[test]
    fn zone1_threshold_boundary() {
        assert!(!zone1_usage_near_limit(899, 1000));
        assert!(zone1_usage_near_limit(900, 1000));
        assert!(zone1_usage_near_limit(1000, 1000));
        assert!(zone1_usage_near_limit(1001, 1000), "over the limit trips");
        assert!(!zone1_usage_near_limit(5, 0), "zero limit never trips");
        assert!(
            !zone1_usage_near_limit(-1, 100),
            "negative usage never trips"
        );
    }
}

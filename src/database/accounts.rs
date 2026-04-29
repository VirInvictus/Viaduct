// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::sync::{mpsc, oneshot};

use crate::database::articles::{ArticlesDbOp, DEFAULT_RETENTION_DAYS, SmartFeedCounts};
use crate::database::opml::{
    OpmlFile, OpmlWriter, merge_opml, normalize_opml, parse_opml, serialize_account_opml,
};
use crate::database::settings::SettingsDbOp;
use crate::database::worker::DbOp;
use crate::error::{DatabaseError, Result, ViaductError};
use crate::models::{Article, ArticleChanges, ArticleStatus, Feed, FeedSettings, ParsedItem};
use crate::paths::opml_path;

use crate::database::delegate::{AccountDelegate, LocalAccountDelegate};
use crate::database::sync::SyncDbOp;
use std::sync::Arc;

pub struct Account {
    db_tx: mpsc::Sender<DbOp>,
    sync_tx: mpsc::Sender<SyncDbOp>,
    opml_writer: OpmlWriter,
    delegate: Arc<dyn AccountDelegate>,
}

impl Account {
    pub async fn new(db_tx: mpsc::Sender<DbOp>, sync_tx: mpsc::Sender<SyncDbOp>) -> Result<Self> {
        let opml_file_path = opml_path()?;
        let opml_writer = OpmlWriter::spawn(opml_file_path.clone());

        let delegate: Arc<dyn AccountDelegate> =
            if crate::network::credentials::fetch_credentials("inoreader")
                .await?
                .is_some()
            {
                Arc::new(crate::database::delegate::InoreaderAccountDelegate::new())
            } else {
                Arc::new(LocalAccountDelegate)
            };

        let account = Self {
            db_tx,
            sync_tx,
            opml_writer,
            delegate,
        };

        // Port of NNW `Account.init` startup cleanup chain
        // (`Account.swift:335-340`): prune settings for unsubscribed feeds,
        // drop articles for those same feeds, sweep stale status orphans,
        // then vacuum both databases. Runs on the worker thread so the GTK
        // main loop never blocks. Errors are logged but non-fatal —
        // cleanup is best-effort.
        if let Err(e) = account.cleanup_at_startup(DEFAULT_RETENTION_DAYS).await {
            tracing::warn!(?e, "startup cleanup failed");
        }

        Ok(account)
    }

    pub async fn load_opml(&self) -> Result<OpmlFile> {
        let path = opml_path()?;
        if !path.exists() {
            return Ok(OpmlFile {
                folders: Vec::new(),
                standalone_feeds: Vec::new(),
            });
        }
        let xml = tokio::fs::read_to_string(&path).await?;
        crate::database::opml::parse_opml(&xml)
    }

    pub async fn save_opml(&self, file: OpmlFile) -> Result<()> {
        self.opml_writer.save(file).await
    }

    // --- ArticlesDatabase API ---

    pub async fn batch_insert_articles(&self, articles: Vec<Article>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::BatchInsert(articles, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn upsert_statuses(&self, statuses: Vec<ArticleStatus>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UpsertStatuses(statuses, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_articles_by_feed(&self, feed_id: String) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchByFeed(feed_id, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_unread_articles(&self) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchUnread(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_starred_articles(&self) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchStarred(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_unread_article_ids(&self) -> Result<HashSet<String>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchUnreadArticleIds(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_starred_article_ids(&self) -> Result<HashSet<String>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchStarredArticleIds(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn update_statuses_read(&self, ids: Vec<String>, read: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UpdateStatusesRead(
                ids, read, tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn update_statuses_starred(&self, ids: Vec<String>, starred: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UpdateStatusesStarred(
                ids, starred, tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_missing_article_ids(&self) -> Result<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchMissingArticleIds(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_today_articles(&self) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchToday(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn search_articles(&self, query: String) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::Search(query, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_statuses_by_ids(
        &self,
        ids: Vec<String>,
    ) -> Result<HashMap<String, (bool, bool)>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchStatusesByIds(ids, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Per-feed unread totals for sidebar badges. Feeds with zero unread
    /// articles are absent from the result map.
    pub async fn unread_counts_by_feed(&self) -> Result<HashMap<String, i64>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UnreadCountsByFeed(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Counts for the three Smart Feed sidebar rows.
    pub async fn smart_feed_counts(&self) -> Result<SmartFeedCounts> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::SmartFeedCounts(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Search that returns a FTS5 `snippet()` fragment alongside each match.
    /// `feed_filter` is `Some(feed_id)` to scope to a single feed, `None` for
    /// the global index.
    pub async fn search_articles_with_snippets(
        &self,
        query: String,
        feed_filter: Option<String>,
    ) -> Result<Vec<(Article, String)>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::SearchWithSnippets(
                query,
                feed_filter,
                tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Update a feed with freshly parsed items. Diffs against DB state and returns
    /// new/updated/deleted deltas for UI coalescing (port of NNW `updateAsync`).
    /// `retention_days` controls the orphaned-article sweep when `delete_older`
    /// is true; pass `DEFAULT_RETENTION_DAYS` for NNW's hardcoded 30-day window.
    pub async fn update_feed(
        &self,
        feed_id: String,
        items: Vec<ParsedItem>,
        delete_older: bool,
        retention_days: i64,
    ) -> Result<ArticleChanges> {
        let (reply, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UpdateFeed {
                feed_id,
                items,
                delete_older,
                retention_days,
                reply,
            }))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    // --- FeedSettingsDatabase API ---

    pub async fn fetch_feed_settings(&self, feed_id: String) -> Result<Option<FeedSettings>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(SettingsDbOp::Fetch(feed_id, tx))))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn upsert_feed_settings(&self, settings: FeedSettings) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(SettingsDbOp::Upsert(
                Box::new(settings),
                tx,
            ))))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Import an OPML file from disk, merging into the current `local.opml`.
    /// Port of NNW `AccountDelegate.importOPML` + `Account.addOPMLItems`:
    ///
    /// 1. Read and parse the user-supplied file.
    /// 2. Normalize (strip nameless wrappers, flatten nested folders, dedup
    ///    by xmlUrl) — `OPMLNormalizer.normalize`.
    /// 3. Merge into the existing `OpmlFile`: union by xmlUrl, fold matching
    ///    folder names, never overwrite existing feeds.
    /// 4. Persist the merged file via the debounced `OpmlWriter`.
    /// 5. Return the list of newly-added feeds so the UI can kick a refresh
    ///    against just those.
    pub async fn import_opml(
        self: &std::sync::Arc<Self>,
        path: impl AsRef<Path>,
    ) -> Result<Vec<Feed>> {
        self.delegate
            .clone()
            .import_opml(self.clone(), path.as_ref())
            .await
    }

    pub async fn import_opml_internal(&self, path: impl AsRef<Path>) -> Result<Vec<Feed>> {
        let xml = tokio::fs::read_to_string(path.as_ref()).await?;
        let parsed = parse_opml(&xml)?;
        let normalized = normalize_opml(parsed);
        let existing = self.load_opml().await?;
        let (merged, added) = merge_opml(&existing, normalized);
        self.save_opml(merged).await?;
        Ok(added)
    }

    /// Export the current `OpmlFile` to a user-chosen path. Port of NNW
    /// `OPMLExporter.OPMLString` + the `ExportOPMLWindowController` write
    /// step. Uses the byte-shape-faithful `serialize_account_opml` writer
    /// rather than the serde-driven `serialize_opml` so the output matches
    /// NetNewsWire's formatting (attribute order, tab indent, version="RSS").
    pub async fn export_opml(&self, path: impl AsRef<Path>, title: &str) -> Result<()> {
        let opml = self.load_opml().await?;
        let body = serialize_account_opml(title, &opml);
        tokio::fs::write(path.as_ref(), body).await?;
        Ok(())
    }

    pub async fn cleanup_orphaned_settings(&self) -> Result<()> {
        let opml = self.load_opml().await?;
        let valid_urls: Vec<String> = opml_feed_urls(&opml);

        let removed = self.delete_settings_for_feeds_not_in(valid_urls).await?;
        if removed > 0 {
            tracing::info!("Cleaned up {} orphaned feed settings", removed);
        }
        Ok(())
    }

    /// Phase 14 startup cleanup. Mirrors NNW
    /// `ArticlesDatabase.cleanupDatabaseAtStartup` plus the
    /// `FeedSettingsDatabase` vacuum: prune settings + articles for feeds
    /// the user no longer subscribes to, sweep stale orphan statuses, then
    /// VACUUM both databases. All four ops are independent and non-fatal —
    /// each logs its own failure rather than aborting the chain.
    pub async fn cleanup_at_startup(&self, retention_days: i64) -> Result<()> {
        let opml = self.load_opml().await?;
        let valid_urls = opml_feed_urls(&opml);
        let valid_ids = opml_feed_ids(&opml);

        let removed_settings = self
            .delete_settings_for_feeds_not_in(valid_urls)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "delete_settings_for_feeds_not_in failed");
                0
            });
        if removed_settings > 0 {
            tracing::info!("Cleaned up {} orphaned feed settings", removed_settings);
        }

        let removed_articles = self
            .delete_articles_not_in_feeds(valid_ids)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "delete_articles_not_in_feeds failed");
                0
            });
        if removed_articles > 0 {
            tracing::info!(
                "Cleaned up {} articles for unsubscribed feeds",
                removed_articles
            );
        }

        let removed_statuses = self
            .delete_old_statuses(retention_days)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(?e, "delete_old_statuses failed");
                0
            });
        if removed_statuses > 0 {
            tracing::info!("Cleaned up {} orphan status rows", removed_statuses);
        }

        if let Err(e) = self.vacuum_databases().await {
            tracing::warn!(?e, "vacuum_databases failed");
        }

        Ok(())
    }

    async fn delete_settings_for_feeds_not_in(&self, feed_urls: Vec<String>) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(
                SettingsDbOp::DeleteSettingsForFeedsNotIn(feed_urls, tx),
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    async fn delete_articles_not_in_feeds(&self, feed_ids: Vec<String>) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::DeleteArticlesNotInFeeds(
                feed_ids, tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    async fn delete_old_statuses(&self, retention_days: i64) -> Result<usize> {
        let (reply, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::DeleteOldStatuses {
                retention_days,
                reply,
            }))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// VACUUM both SQLite files. The two ops fire serially through the same
    /// worker channel, so they don't contend with each other or with any
    /// other write.
    pub async fn vacuum_databases(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::Vacuum(tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))?;

        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(SettingsDbOp::Vacuum(tx))))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    // --- SyncDatabase API ---

    pub async fn insert_sync_statuses(
        &self,
        statuses: Vec<crate::database::sync::SyncStatus>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sync_tx
            .send(SyncDbOp::InsertStatuses(statuses, tx))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn select_sync_statuses_for_processing(
        &self,
        limit: Option<usize>,
    ) -> Result<Vec<crate::database::sync::SyncStatus>> {
        let (tx, rx) = oneshot::channel();
        self.sync_tx
            .send(SyncDbOp::SelectForProcessing(limit, tx))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn delete_sync_statuses_selected_for_processing(
        &self,
        ids: Vec<String>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sync_tx
            .send(SyncDbOp::DeleteSelectedForProcessing(ids, tx))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn reset_all_sync_statuses_selected_for_processing(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sync_tx
            .send(SyncDbOp::ResetAllSelectedForProcessing(tx))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }
}

fn opml_feed_urls(opml: &OpmlFile) -> Vec<String> {
    let mut out = Vec::new();
    for feed in &opml.standalone_feeds {
        out.push(feed.url.clone());
    }
    for folder in &opml.folders {
        for feed in &folder.feeds {
            out.push(feed.url.clone());
        }
    }
    out
}

fn opml_feed_ids(opml: &OpmlFile) -> Vec<String> {
    let mut out = Vec::new();
    for feed in &opml.standalone_feeds {
        out.push(feed.id.clone());
    }
    for folder in &opml.folders {
        for feed in &folder.feeds {
            out.push(feed.id.clone());
        }
    }
    out
}

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
use crate::models::{
    Article, ArticleChanges, ArticleStatus, Feed, FeedSettings, Folder, ParsedItem,
};
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

    pub async fn fetch_articles_by_feed(
        &self,
        feed_id: String,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchByFeed(feed_id, sort, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// Bulk fetch articles for many feeds at once. One SQL query (with
    /// an `IN (?, ?, …)` clause, chunked at 500 IDs to stay under
    /// SQLite's parameter limit) instead of N round-trips. Used by the
    /// folder-aggregate view (`fetch_folder_articles`); previously that
    /// fanned out N sequential single-feed queries.
    pub async fn fetch_articles_by_feeds(
        &self,
        feed_ids: Vec<String>,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchByFeeds(
                feed_ids, sort, tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_unread_articles(
        &self,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchUnread(sort, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    pub async fn fetch_starred_articles(
        &self,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchStarred(sort, tx)))
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

    pub async fn fetch_today_articles(
        &self,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchToday(sort, tx)))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// v2.7.0 — run a Smart Feed's rules against the article store.
    pub async fn fetch_smart_feed_articles(
        &self,
        rules: crate::smart_feeds::SmartFeedRules,
        sort: crate::database::articles::SortOrder,
    ) -> Result<Vec<Article>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::FetchSmartFeed(
                rules, sort, tx,
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// v2.7.0 — list all user-defined Smart Feeds. Reads
    /// `$XDG_DATA_HOME/viaduct/smart-feeds.json` on the tokio
    /// blocking pool. Missing file → empty list.
    pub async fn list_smart_feeds(&self) -> Result<Vec<crate::smart_feeds::SmartFeed>> {
        let path = crate::paths::smart_feeds_path()?;
        tokio::task::spawn_blocking(move || crate::smart_feeds::load(&path).map(|f| f.feeds))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?
    }

    /// v2.7.0 — append a Smart Feed and persist. ID-collision is the
    /// caller's problem (UI uses `uuid v4`). Returns the persisted feed.
    pub async fn add_smart_feed(
        &self,
        feed: crate::smart_feeds::SmartFeed,
    ) -> Result<crate::smart_feeds::SmartFeed> {
        let path = crate::paths::smart_feeds_path()?;
        tokio::task::spawn_blocking(move || -> Result<crate::smart_feeds::SmartFeed> {
            let mut file = crate::smart_feeds::load(&path)?;
            file.feeds.push(feed.clone());
            crate::smart_feeds::save(&path, &file)?;
            Ok(feed)
        })
        .await
        .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?
    }

    /// v2.7.0 — drop a Smart Feed by id. No-op if id isn't present.
    /// Returns `true` if a feed was actually removed.
    pub async fn delete_smart_feed(&self, id: String) -> Result<bool> {
        let path = crate::paths::smart_feeds_path()?;
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let mut file = crate::smart_feeds::load(&path)?;
            let len_before = file.feeds.len();
            file.feeds.retain(|f| f.id != id);
            let removed = file.feeds.len() != len_before;
            if removed {
                crate::smart_feeds::save(&path, &file)?;
            }
            Ok(removed)
        })
        .await
        .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?
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

    /// Add a single feed to the OPML hierarchy and persist. If
    /// `folder_name` is `Some`, place under that folder (creating it if
    /// it doesn't exist); else place as a standalone feed. Dedupes by
    /// `feed_url` — adding a feed that already exists at the same URL
    /// returns the existing entry rather than creating a duplicate row.
    /// Returns the `Feed` for immediate refresh by the caller.
    pub async fn add_feed(
        &self,
        feed_url: String,
        feed_name: Option<String>,
        home_page_url: Option<String>,
        folder_name: Option<String>,
    ) -> Result<Feed> {
        let mut opml = self.load_opml().await?;
        let feed = Feed {
            id: feed_url.clone(),
            url: feed_url.clone(),
            name: feed_name,
            edited_name: None,
            home_page_url,
        };

        let placed_feed = match folder_name {
            Some(folder) => {
                if let Some(existing_folder) = opml.folders.iter_mut().find(|f| f.name == folder) {
                    if let Some(existing) = existing_folder.feeds.iter().find(|x| x.url == feed.url)
                    {
                        existing.clone()
                    } else {
                        existing_folder.feeds.push(feed.clone());
                        feed
                    }
                } else {
                    opml.folders.push(Folder {
                        name: folder,
                        feeds: vec![feed.clone()],
                    });
                    feed
                }
            }
            None => {
                if let Some(existing) = opml.standalone_feeds.iter().find(|x| x.url == feed.url) {
                    existing.clone()
                } else {
                    opml.standalone_feeds.push(feed.clone());
                    feed
                }
            }
        };

        self.save_opml(opml).await?;
        Ok(placed_feed)
    }

    /// Remove a feed from the OPML hierarchy by URL. Sweeps both the
    /// standalone list and every folder. Empty folders left behind by
    /// the removal are preserved (NNW behaviour — folders are user-
    /// curated, not auto-pruned). Saves the OPML. Returns true if a
    /// feed was actually removed.
    ///
    /// Article rows for the removed feed are pruned by the next
    /// `cleanup_at_startup` cycle via `delete_articles_not_in_feeds`,
    /// or immediately if the caller fires that op manually.
    pub async fn remove_feed(&self, feed_url: &str) -> Result<bool> {
        let mut opml = self.load_opml().await?;
        let mut removed = false;

        let original_standalone = opml.standalone_feeds.len();
        opml.standalone_feeds.retain(|f| f.url != feed_url);
        if opml.standalone_feeds.len() != original_standalone {
            removed = true;
        }

        for folder in opml.folders.iter_mut() {
            let original_len = folder.feeds.len();
            folder.feeds.retain(|f| f.url != feed_url);
            if folder.feeds.len() != original_len {
                removed = true;
            }
        }

        if removed {
            self.save_opml(opml).await?;
        }
        Ok(removed)
    }

    /// v2.1.0: set the user-visible display name for a feed. Stored as
    /// `edited_name` on the OPML entry. The sidebar's display-name
    /// resolver uses the existing fallback chain `edited_name → name →
    /// URL host → raw URL`, so an empty `new_name` reverts to whatever
    /// the parsed feed reported. Saves the OPML; returns true if a feed
    /// was found and updated.
    pub async fn rename_feed(&self, feed_url: &str, new_name: String) -> Result<bool> {
        let mut opml = self.load_opml().await?;
        let trimmed = new_name.trim().to_string();
        let edited = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        let mut changed = false;

        for feed in opml.standalone_feeds.iter_mut() {
            if feed.url == feed_url {
                feed.edited_name = edited.clone();
                changed = true;
            }
        }
        for folder in opml.folders.iter_mut() {
            for feed in folder.feeds.iter_mut() {
                if feed.url == feed_url {
                    feed.edited_name = edited.clone();
                    changed = true;
                }
            }
        }

        if changed {
            self.save_opml(opml).await?;
        }
        Ok(changed)
    }

    /// v2.1.0: create an empty folder. No-ops + returns false when a
    /// folder with the same name already exists. Empty `name` (after
    /// trim) is rejected. Saves the OPML.
    pub async fn create_folder(&self, name: String) -> Result<bool> {
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Ok(false);
        }
        let mut opml = self.load_opml().await?;
        if opml.folders.iter().any(|f| f.name == trimmed) {
            return Ok(false);
        }
        opml.folders.push(Folder {
            name: trimmed,
            feeds: Vec::new(),
        });
        self.save_opml(opml).await?;
        Ok(true)
    }

    /// v2.1.0: relocate a feed between standalone-list and folders.
    /// `target_folder = None` moves to the standalone list; `Some(name)`
    /// moves into that folder, creating it if it doesn't exist. Sweeps
    /// every existing location for the feed first so the move is
    /// destination-only (no duplicates). Saves the OPML; returns true
    /// when the feed was found and the move actually changed something.
    pub async fn move_feed_to_folder(
        &self,
        feed_url: &str,
        target_folder: Option<String>,
    ) -> Result<bool> {
        let mut opml = self.load_opml().await?;

        // Locate + remove from current home, capturing the Feed value so
        // we can reinsert it at the destination.
        let mut current: Option<Feed> = None;
        let mut current_was_standalone = false;
        let mut current_folder: Option<String> = None;

        if let Some(idx) = opml.standalone_feeds.iter().position(|f| f.url == feed_url) {
            current = Some(opml.standalone_feeds.remove(idx));
            current_was_standalone = true;
        } else {
            for folder in opml.folders.iter_mut() {
                if let Some(idx) = folder.feeds.iter().position(|f| f.url == feed_url) {
                    current = Some(folder.feeds.remove(idx));
                    current_folder = Some(folder.name.clone());
                    break;
                }
            }
        }
        let Some(feed) = current else {
            return Ok(false);
        };

        // Detect a no-op: same destination as current location.
        let same_destination = match (&target_folder, &current_folder, current_was_standalone) {
            (None, _, true) => true,
            (Some(target), Some(current_name), false) => target == current_name,
            _ => false,
        };
        if same_destination {
            // Reinsert without saving.
            match target_folder {
                Some(ref name) => {
                    if let Some(folder) = opml.folders.iter_mut().find(|f| &f.name == name) {
                        folder.feeds.push(feed);
                    }
                }
                None => opml.standalone_feeds.push(feed),
            }
            return Ok(false);
        }

        match target_folder {
            None => opml.standalone_feeds.push(feed),
            Some(target) => {
                if let Some(folder) = opml.folders.iter_mut().find(|f| f.name == target) {
                    folder.feeds.push(feed);
                } else {
                    opml.folders.push(Folder {
                        name: target,
                        feeds: vec![feed],
                    });
                }
            }
        }

        self.save_opml(opml).await?;
        Ok(true)
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
    ///
    /// **v2.6.5** extends the chain with disk-cache sweeps (favicons /
    /// images / video-thumbs in `$XDG_CACHE_HOME/viaduct/`) and a
    /// local-only `syncStatus` wipe. The disk-cache sweeps are the only
    /// fix for the long-tail growth of `~/.cache/viaduct/` across many
    /// months of use; favicons use a targeted sweep against the live
    /// `feed_settings` URL set, images and video-thumbs use age-based
    /// pruning since neither has a clean live-set definition.
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

        let removed_authors = self.delete_orphaned_authors().await.unwrap_or_else(|e| {
            tracing::warn!(?e, "delete_orphaned_authors failed");
            0
        });
        if removed_authors > 0 {
            tracing::info!("Cleaned up {} orphan author rows", removed_authors);
        }

        // v2.6.5: disk-cache sweep. Targeted for favicons (we have the
        // live URL set in `feed_settings`); age-based for images +
        // video-thumbs (no clean live-set definition; rely on article
        // retention as a proxy — anything older than the retention
        // window is for a pruned article).
        let removed_caches = self.sweep_disk_caches(retention_days).await;
        if removed_caches > 0 {
            tracing::info!("Cleaned up {} orphan cache files", removed_caches);
        }

        // v2.6.5: when running in local-only mode (no Inoreader
        // credentials), every row in `syncStatus` is leftover ghost
        // from a previous remote session. Wipe wholesale. When
        // Inoreader is active, the rows are live state — leave alone.
        if self.is_local_account() {
            match self.wipe_sync_statuses().await {
                Ok(n) if n > 0 => {
                    tracing::info!("Cleaned up {} orphan syncStatus rows", n)
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(?e, "wipe_sync_statuses failed"),
            }
        }

        if let Err(e) = self.vacuum_databases().await {
            tracing::warn!(?e, "vacuum_databases failed");
        }

        Ok(())
    }

    /// v2.6.5: walk the three `~/.cache/viaduct/` subdirs and prune
    /// ghost files. Favicons use the `feed_settings`-derived live set
    /// (exact, no false positives). Images + video-thumbs use mtime
    /// against the article retention window — anything older is for
    /// an article that's been pruned out, so by definition orphan.
    /// Image threshold is `retention_days × 2` to give a buffer for
    /// re-bound articles; video-thumbs reuse `retention_days` directly
    /// (NNW's video-thumbnail TTL is the same).
    ///
    /// Returns the total number of files deleted across all three
    /// dirs. Errors per-file warn and continue; whole-dir errors warn
    /// and contribute zero to the sum.
    async fn sweep_disk_caches(&self, retention_days: i64) -> usize {
        let live_urls = self.collect_favicon_urls().await.unwrap_or_else(|e| {
            tracing::warn!(?e, "collect_favicon_urls failed; skipping favicon sweep");
            Vec::new()
        });
        let favicon_dir = match crate::paths::favicon_cache_dir() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(?e, "favicon_cache_dir resolution failed");
                return 0;
            }
        };
        let image_dir = match crate::paths::image_cache_dir() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(?e, "image_cache_dir resolution failed");
                return 0;
            }
        };
        let video_thumb_dir = match crate::paths::video_thumb_cache_dir() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(?e, "video_thumb_cache_dir resolution failed");
                return 0;
            }
        };

        // I/O happens on a blocking task — std::fs::read_dir +
        // remove_file are sync, and we don't want to stall the worker.
        let retention = retention_days.max(1) as u64;
        tokio::task::spawn_blocking(move || {
            use crate::network::cache_sweep::{live_filenames_for, sweep_by_age, sweep_targeted};
            let live_set = live_filenames_for(&live_urls);
            let removed_favicons = if live_set.is_empty() {
                // Empty live set means no settings exist yet (fresh
                // install) or we failed to collect. Don't sweep
                // wholesale — that would nuke favicons we just fetched
                // for feeds whose settings haven't persisted yet.
                0
            } else {
                sweep_targeted(&favicon_dir, &live_set)
            };
            let removed_images = sweep_by_age(&image_dir, retention.saturating_mul(2));
            let removed_thumbs = sweep_by_age(&video_thumb_dir, retention);
            removed_favicons + removed_images + removed_thumbs
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(?e, "sweep_disk_caches join failed");
            0
        })
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

    async fn delete_orphaned_authors(&self) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::DeleteOrphanedAuthors(tx)))
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

    /// v2.6.5: returns true when the active delegate is the
    /// local-only one (no Inoreader credentials at construction time).
    /// Used by `cleanup_at_startup` to gate the syncStatus wipe.
    pub fn is_local_account(&self) -> bool {
        self.delegate.is_local()
    }

    /// v2.6.5 favicon-sweep helper: returns every non-blank
    /// `favicon_url` + `icon_url` value from `feed_settings` so the
    /// cache sweep can compute the live md5 set.
    pub async fn collect_favicon_urls(&self) -> Result<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(SettingsDbOp::CollectFaviconUrls(
                tx,
            ))))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        rx.await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))
    }

    /// v2.6.5 sync-sweep helper: drop every row from `syncStatus`.
    /// Only safe to call from `cleanup_at_startup` when the active
    /// delegate is the local-only one; otherwise the rows are
    /// in-flight remote-sync state that the Inoreader delegate
    /// expects to consume.
    pub async fn wipe_sync_statuses(&self) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.sync_tx
            .send(SyncDbOp::WipeAll(tx))
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

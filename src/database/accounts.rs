// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

use crate::database::articles::ArticlesDbOp;
use crate::database::opml::{OpmlFile, OpmlWriter};
use crate::database::settings::SettingsDbOp;
use crate::database::worker::DbOp;
use crate::error::{DatabaseError, Result, ViaductError};
use crate::models::{Article, ArticleChanges, ArticleStatus, FeedSettings, ParsedItem};
use crate::paths::opml_path;

pub struct LocalAccount {
    db_tx: mpsc::Sender<DbOp>,
    opml_writer: OpmlWriter,
}

impl LocalAccount {
    pub async fn new(db_tx: mpsc::Sender<DbOp>) -> Result<Self> {
        let opml_file_path = opml_path()?;
        let opml_writer = OpmlWriter::spawn(opml_file_path.clone());

        let account = Self { db_tx, opml_writer };

        // Perform startup cleanup
        account.cleanup_orphaned_settings().await?;

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
    pub async fn update_feed(
        &self,
        feed_id: String,
        items: Vec<ParsedItem>,
        delete_older: bool,
    ) -> Result<ArticleChanges> {
        let (reply, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Articles(ArticlesDbOp::UpdateFeed {
                feed_id,
                items,
                delete_older,
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

    pub async fn cleanup_orphaned_settings(&self) -> Result<()> {
        let opml = self.load_opml().await?;
        let mut valid_urls = Vec::new();

        for feed in &opml.standalone_feeds {
            valid_urls.push(feed.url.clone());
        }
        for folder in &opml.folders {
            for feed in &folder.feeds {
                valid_urls.push(feed.url.clone());
            }
        }

        let (tx, rx) = oneshot::channel();
        self.db_tx
            .send(DbOp::Settings(Box::new(
                SettingsDbOp::DeleteSettingsForFeedsNotIn(valid_urls, tx),
            )))
            .await
            .map_err(|_| ViaductError::Database(DatabaseError::WriterGone))?;
        let removed_count = rx
            .await
            .unwrap_or_else(|_| Err(ViaductError::Database(DatabaseError::WriterGone)))?;

        if removed_count > 0 {
            tracing::info!("Cleaned up {} orphaned feed settings", removed_count);
        }

        Ok(())
    }
}

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use chrono::{Duration, TimeZone, Utc};
use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{HashMap, HashSet};
use tokio::sync::oneshot;

use crate::error::{DatabaseError, Result};
use crate::models::{Article, ArticleChanges, ArticleStatus, Attachment, Author, ParsedItem};

/// NNW's `ArticleStatus.staleIntervalInSeconds` — articles older than ~6 months default to read.
const STALE_INTERVAL_DAYS: i64 = 180;

/// Default `feedBased` retention used when callers don't override (matches
/// NNW's hardcoded 30 days in `ArticlesTable.deleteOldStatuses`). The
/// per-update sweep that runs from the refresher reads `retention-days`
/// from GSettings instead.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

/// v2.6.22: timeline sort direction. Drives the `ORDER BY` clause on
/// every timeline-feeding query (`FetchByFeed`, `FetchByFeeds`,
/// `FetchUnread`, `FetchStarred`, `FetchToday`). Search results
/// continue to sort by FTS5 `rank` regardless; relevance order is
/// always more useful than chronological for a search hit list.
///
/// v2.8.1: sorts on a **logical date** `COALESCE(date_published,
/// date_modified)` rather than `date_published` alone, porting the key
/// idea of NNW's `ArticleSorter` rewrite (NNW uses `datePublished ??
/// dateModified ?? dateArrived`). Atom entries that carry only
/// `<updated>` (no `<published>`) now sort by their modified date
/// instead of clustering at the NULL end of the list. We keep `rowid`
/// as the tiebreaker (arrival order) rather than NNW's `articleID`
/// hash order, which is more meaningful for a local-only store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    NewestFirst,
    OldestFirst,
}

impl SortOrder {
    /// Render to the SQL `ORDER BY` tail. Includes the `rowid`
    /// secondary key so two articles with the same logical date still
    /// have a deterministic order.
    pub fn order_by_clause(&self) -> &'static str {
        match self {
            SortOrder::NewestFirst => {
                "ORDER BY COALESCE(date_published, date_modified) DESC, rowid DESC"
            }
            SortOrder::OldestFirst => {
                "ORDER BY COALESCE(date_published, date_modified) ASC, rowid ASC"
            }
        }
    }

    /// Same as `order_by_clause` but with the `a.` table alias used by
    /// the smart-feed JOIN queries.
    pub fn order_by_clause_aliased(&self) -> &'static str {
        match self {
            SortOrder::NewestFirst => {
                "ORDER BY COALESCE(a.date_published, a.date_modified) DESC, a.rowid DESC"
            }
            SortOrder::OldestFirst => {
                "ORDER BY COALESCE(a.date_published, a.date_modified) ASC, a.rowid ASC"
            }
        }
    }
}

pub enum ArticlesDbOp {
    BatchInsert(Vec<Article>, oneshot::Sender<Result<()>>),
    UpsertStatuses(Vec<ArticleStatus>, oneshot::Sender<Result<()>>),
    FetchByFeed(String, SortOrder, oneshot::Sender<Result<Vec<Article>>>),
    /// Bulk variant of `FetchByFeed`. One SQL query with an `IN (?, ?, …)`
    /// clause replaces the previous N-round-trip fan-out used by folder
    /// aggregate views. Empty input is a no-op.
    FetchByFeeds(
        Vec<String>,
        SortOrder,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    FetchByArticleId(String, oneshot::Sender<Result<Option<Article>>>),
    FetchUnread(SortOrder, oneshot::Sender<Result<Vec<Article>>>),
    FetchStarred(SortOrder, oneshot::Sender<Result<Vec<Article>>>),
    FetchUnreadArticleIds(oneshot::Sender<Result<HashSet<String>>>),
    FetchStarredArticleIds(oneshot::Sender<Result<HashSet<String>>>),
    UpdateStatusesRead(Vec<String>, bool, oneshot::Sender<Result<()>>),
    UpdateStatusesStarred(Vec<String>, bool, oneshot::Sender<Result<()>>),
    FetchMissingArticleIds(oneshot::Sender<Result<Vec<String>>>),
    FetchToday(SortOrder, oneshot::Sender<Result<Vec<Article>>>),
    Search(String, oneshot::Sender<Result<Vec<Article>>>),
    SearchWithSnippets(
        String,
        Option<String>, // optional feed_id filter
        oneshot::Sender<Result<Vec<(Article, String)>>>,
    ),
    /// Bulk-fetch `(read, starred)` for the given article IDs. Missing rows
    /// are simply absent from the result map — callers treat absence as
    /// "not-yet-recorded, default false".
    FetchStatusesByIds(
        Vec<String>,
        oneshot::Sender<Result<HashMap<String, (bool, bool)>>>,
    ),
    /// Per-feed unread totals for sidebar badges. Returns a map keyed by
    /// `feed_id`; feeds with zero unread are absent from the map.
    UnreadCountsByFeed(oneshot::Sender<Result<HashMap<String, i64>>>),
    /// Counts for the three Smart Feed rows. Today / All Unread match the
    /// timeline-fetch queries; Starred narrows to starred-and-unread (NNW
    /// `BuiltinSmartFeed.unreadCount`).
    SmartFeedCounts(oneshot::Sender<Result<SmartFeedCounts>>),
    /// v2.7.0 — fetch articles matching a user-defined Smart Feed's
    /// rule list. The rules are AND-combined; the SQL builder lives
    /// in `crate::smart_feeds::build_where`.
    FetchSmartFeed(
        crate::smart_feeds::SmartFeedRules,
        SortOrder,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    UpdateFeed {
        feed_id: String,
        items: Vec<ParsedItem>,
        delete_older: bool,
        retention_days: i64,
        reply: oneshot::Sender<Result<ArticleChanges>>,
    },
    /// Drop article rows whose `feed_id` is not in the supplied list. Used
    /// by the startup cleanup to evict articles for unsubscribed feeds.
    /// Returns the count removed. Empty input is a no-op (matches NNW's
    /// `deleteArticlesNotInSubscribedToFeedIDs` early return).
    DeleteArticlesNotInFeeds(Vec<String>, oneshot::Sender<Result<usize>>),
    /// NNW's `deleteOldStatuses` (feedBased branch): prune statuses for
    /// articles that no longer exist, are not starred, and arrived before
    /// the cutoff. Returns the count removed.
    DeleteOldStatuses {
        retention_days: i64,
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Port of NNW `deleteOrphanedAuthorsLookupRows` (issue #5232 fix).
    /// Sweeps `authorsLookup` rows whose article no longer exists, then
    /// drops `authors` rows no longer referenced by any lookup. Catches
    /// the slow leak where an author table accumulates rows from
    /// long-deleted articles. Runs once at startup as part of
    /// `cleanup_at_startup`. Returns the count of orphan author rows
    /// removed (lookup rows are typically caught by the delete trigger;
    /// this op is the safety net for any that escaped).
    DeleteOrphanedAuthors(oneshot::Sender<Result<usize>>),
    /// Run `VACUUM` on the connection. Worker-thread-only; never call from
    /// inside another transaction.
    Vacuum(oneshot::Sender<Result<()>>),
    /// `PRAGMA wal_checkpoint(TRUNCATE)` only — flush the WAL into the main
    /// DB and truncate the WAL file. Cheap relative to `Vacuum` (no file
    /// rewrite); run every startup to bound the WAL even when nothing was
    /// pruned. See `cleanup_at_startup`.
    Checkpoint(oneshot::Sender<Result<()>>),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SmartFeedCounts {
    pub today_unread: i64,
    pub all_unread: i64,
    pub starred_unread: i64,
}

/// Matches NNW's `Article.calculatedArticleID(feedID:uniqueID:)`:
/// `md5("{feed_id} {unique_id}")`. Stable across builds, unlike `DefaultHasher`.
pub fn article_id_for(feed_id: &str, unique_id: &str) -> String {
    let mut h = Md5::new();
    h.update(feed_id.as_bytes());
    h.update(b" ");
    h.update(unique_id.as_bytes());
    format!("{:x}", h.finalize())
}

fn parsed_to_article(p: &ParsedItem, feed_id: &str) -> Article {
    // Truncate dates to second precision to match the DB's integer storage —
    // otherwise every refresh flags every article as "updated" on the round-trip.
    let trunc = |d: Option<chrono::DateTime<Utc>>| {
        d.and_then(|d| Utc.timestamp_opt(d.timestamp(), 0).single())
    };
    Article {
        article_id: article_id_for(feed_id, &p.id),
        feed_id: feed_id.to_string(),
        title: p.title.clone(),
        content_html: p.content_html.clone(),
        content_text: p.content_text.clone(),
        url: p.url.clone(),
        external_url: p.external_url.clone(),
        summary: p.summary.clone(),
        image_url: p.image_url.clone(),
        date_published: trunc(p.date_published),
        date_modified: trunc(p.date_modified),
        authors: p.authors.clone(),
        attachments: p.attachments.clone(),
    }
}

pub(crate) fn setup_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        -- v2.6.11: cap the file→RSS mmap. Pre-v2.6.11 we requested
        -- 30 GB (effectively 'map the whole DB + WAL') which made
        -- every page the refresher wrote to the WAL contribute
        -- directly to resident set size. With the WAL also uncapped
        -- (see `journal_size_limit` below) a 130-feed force-refresh
        -- cycle ballooned WAL to 149 MB and dragged ~150 MB into RSS
        -- per session via the mmap. 64 MB is plenty for SQLite's
        -- read-side optimizations on a database this size; the
        -- per-page page cache handles writes regardless.
        PRAGMA mmap_size = 67108864;
        -- v2.6.11: bound the WAL on disk + in mmap. SQLite's default
        -- behavior is to grow the WAL forever between full
        -- checkpoints (passive checkpoints sync but don't truncate).
        -- 64 MB cap means the periodic auto-checkpoint truncates the
        -- file once it crosses the threshold; the cap is far above
        -- our typical write-burst size (one refresh cycle = a few MB
        -- of WAL on a no-changes corpus, more during heavy ingest).
        PRAGMA journal_size_limit = 67108864;

        CREATE TABLE IF NOT EXISTS articles (
            article_id TEXT PRIMARY KEY,
            feed_id TEXT NOT NULL,
            title TEXT,
            content_html TEXT,
            content_text TEXT,
            url TEXT,
            external_url TEXT,
            summary TEXT,
            image_url TEXT,
            date_published INTEGER,
            date_modified INTEGER,
            authors JSON,
            attachments JSON
        );

        CREATE TABLE IF NOT EXISTS statuses (
            article_id TEXT PRIMARY KEY,
            read INTEGER NOT NULL DEFAULT 0,
            starred INTEGER NOT NULL DEFAULT 0,
            date_arrived INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS authors (
            author_id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT,
            url TEXT,
            avatar_url TEXT,
            email TEXT,
            UNIQUE(name, url, email)
        );

        CREATE TABLE IF NOT EXISTS authorsLookup (
            article_id TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            PRIMARY KEY(article_id, author_id)
        );

        -- Speeds up the delete-trigger and the orphan-cleanup sweep, both of
        -- which scan authorsLookup by article_id. Mirrors NNW's
        -- `authorsLookup_articleID` index added alongside issue #5232.
        CREATE INDEX IF NOT EXISTS authorsLookup_article_id_idx
            ON authorsLookup (article_id);

        CREATE VIRTUAL TABLE IF NOT EXISTS search USING fts5(
            article_id UNINDEXED,
            title,
            content_text,
            content='articles',
            content_rowid='rowid'
        );

        CREATE TRIGGER IF NOT EXISTS articles_ai AFTER INSERT ON articles BEGIN
            INSERT INTO search(rowid, article_id, title, content_text)
            VALUES (new.rowid, new.article_id, new.title, new.content_text);
        END;

        CREATE TRIGGER IF NOT EXISTS articles_ad AFTER DELETE ON articles BEGIN
            INSERT INTO search(search, rowid, article_id, title, content_text)
            VALUES ('delete', old.rowid, old.article_id, old.title, old.content_text);
        END;

        CREATE TRIGGER IF NOT EXISTS articles_au AFTER UPDATE ON articles BEGIN
            INSERT INTO search(search, rowid, article_id, title, content_text)
            VALUES ('delete', old.rowid, old.article_id, old.title, old.content_text);
            INSERT INTO search(rowid, article_id, title, content_text)
            VALUES (new.rowid, new.article_id, new.title, new.content_text);
        END;

        -- NNW cleans authorsLookup explicitly inside removeArticles. We get the
        -- same effect via a delete-cascade trigger so callers don't have to
        -- remember it. Status rows are deliberately NOT cascaded — NNW keeps
        -- them around in case the article reappears (idempotent feeds).
        CREATE TRIGGER IF NOT EXISTS articles_ad_lookup AFTER DELETE ON articles BEGIN
            DELETE FROM authorsLookup WHERE article_id = old.article_id;
        END;
        ",
    )?;

    // Idempotent column-add for existing DBs. New DBs get the column via the
    // CREATE TABLE above; pre-existing ones need an ALTER. SQLite raises
    // "duplicate column" when the column is already present — swallow that
    // specific error and propagate anything else.
    if let Err(e) = conn.execute("ALTER TABLE articles ADD COLUMN attachments JSON", [])
        && !e.to_string().contains("duplicate column")
    {
        return Err(e.into());
    }
    Ok(())
}

pub(crate) fn handle_op(conn: &mut Connection, op: ArticlesDbOp) {
    match op {
        ArticlesDbOp::BatchInsert(articles, tx) => {
            let res = batch_insert(conn, articles);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpsertStatuses(statuses, tx) => {
            let res = upsert_statuses(conn, statuses);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByFeed(feed_id, sort, tx) => {
            let res = fetch_by_feed(conn, &feed_id, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByFeeds(feed_ids, sort, tx) => {
            let res = fetch_by_feeds(conn, &feed_ids, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByArticleId(article_id, tx) => {
            let res = fetch_by_article_id(conn, &article_id);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchUnread(sort, tx) => {
            let res = fetch_unread(conn, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStarred(sort, tx) => {
            let res = fetch_starred(conn, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchUnreadArticleIds(tx) => {
            let res = fetch_unread_article_ids(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStarredArticleIds(tx) => {
            let res = fetch_starred_article_ids(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpdateStatusesRead(ids, read, tx) => {
            let res = update_statuses_read(conn, &ids, read);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpdateStatusesStarred(ids, starred, tx) => {
            let res = update_statuses_starred(conn, &ids, starred);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchMissingArticleIds(tx) => {
            let res = fetch_missing_article_ids(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchToday(sort, tx) => {
            let res = fetch_today(conn, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchSmartFeed(rules, sort, tx) => {
            let res = fetch_smart_feed(conn, &rules, sort);
            let _ = tx.send(res);
        }
        ArticlesDbOp::Search(query, tx) => {
            let res = search(conn, &query);
            let _ = tx.send(res);
        }
        ArticlesDbOp::SearchWithSnippets(query, feed_filter, tx) => {
            let res = search_with_snippets(conn, &query, feed_filter.as_deref());
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStatusesByIds(ids, tx) => {
            let res = fetch_statuses_by_ids(conn, &ids);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UnreadCountsByFeed(tx) => {
            let res = unread_counts_by_feed(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::SmartFeedCounts(tx) => {
            let res = smart_feed_counts(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpdateFeed {
            feed_id,
            items,
            delete_older,
            retention_days,
            reply,
        } => {
            let res = update_feed(conn, &feed_id, items, delete_older, retention_days);
            let _ = reply.send(res);
        }
        ArticlesDbOp::DeleteArticlesNotInFeeds(feed_ids, tx) => {
            let res = delete_articles_not_in_feeds(conn, &feed_ids);
            let _ = tx.send(res);
        }
        ArticlesDbOp::DeleteOldStatuses {
            retention_days,
            reply,
        } => {
            let res = delete_old_statuses(conn, retention_days);
            let _ = reply.send(res);
        }
        ArticlesDbOp::DeleteOrphanedAuthors(tx) => {
            let res = delete_orphaned_authors(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::Vacuum(tx) => {
            let res = vacuum(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::Checkpoint(tx) => {
            let res = checkpoint(conn);
            let _ = tx.send(res);
        }
    }
}

fn batch_insert(conn: &mut Connection, articles: Vec<Article>) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut article_stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO articles (
                article_id, feed_id, title, content_html, content_text,
                url, external_url, summary, image_url, date_published, date_modified,
                authors, attachments
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;

        let mut author_stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO authors (name, url, avatar_url, email) VALUES (?, ?, ?, ?)",
        )?;

        let mut author_id_stmt = tx.prepare_cached(
            "SELECT author_id FROM authors WHERE COALESCE(name, '') = COALESCE(?, '') 
             AND COALESCE(url, '') = COALESCE(?, '') 
             AND COALESCE(email, '') = COALESCE(?, '')",
        )?;

        let mut lookup_stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO authorsLookup (article_id, author_id) VALUES (?, ?)",
        )?;

        for article in articles {
            let authors_json = serde_json::to_string(&article.authors)
                .map_err(|e| DatabaseError::Migration(e.to_string()))?;
            let attachments_json = serde_json::to_string(&article.attachments)
                .map_err(|e| DatabaseError::Migration(e.to_string()))?;

            article_stmt.execute(params![
                article.article_id,
                article.feed_id,
                article.title,
                article.content_html,
                article.content_text,
                article.url,
                article.external_url,
                article.summary,
                article.image_url,
                article.date_published.map(|d| d.timestamp()),
                article.date_modified.map(|d| d.timestamp()),
                authors_json,
                attachments_json,
            ])?;

            for author in &article.authors {
                author_stmt.execute(params![
                    author.name,
                    author.url,
                    author.avatar_url,
                    author.email,
                ])?;

                let author_id: i64 = author_id_stmt
                    .query_row(params![author.name, author.url, author.email,], |row| {
                        row.get(0)
                    })?;

                lookup_stmt.execute(params![article.article_id, author_id])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn upsert_statuses(conn: &mut Connection, statuses: Vec<ArticleStatus>) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO statuses (article_id, read, starred, date_arrived) 
             VALUES (?, ?, ?, ?)
             ON CONFLICT(article_id) DO UPDATE SET 
             read=excluded.read, starred=excluded.starred",
        )?;

        for status in statuses {
            stmt.execute(params![
                status.article_id,
                status.read,
                status.starred,
                status.date_arrived.timestamp(),
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn row_to_article(row: &rusqlite::Row) -> rusqlite::Result<Article> {
    let authors_json: Option<String> = row.get("authors")?;
    let authors: Vec<Author> = if let Some(j) = authors_json {
        serde_json::from_str(&j).unwrap_or_default()
    } else {
        Vec::new()
    };
    // `attachments` was added after the initial schema so rows predating
    // the migration have it NULL. Treat that as an empty vec.
    let attachments_json: Option<String> = row.get("attachments").ok().flatten();
    let attachments: Vec<Attachment> = attachments_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();

    Ok(Article {
        article_id: row.get("article_id")?,
        feed_id: row.get("feed_id")?,
        title: row.get("title")?,
        content_html: row.get("content_html")?,
        content_text: row.get("content_text")?,
        url: row.get("url")?,
        external_url: row.get("external_url")?,
        summary: row.get("summary")?,
        image_url: row.get("image_url")?,
        date_published: row
            .get::<_, Option<i64>>("date_published")?
            .and_then(|t| Utc.timestamp_opt(t, 0).single()),
        date_modified: row
            .get::<_, Option<i64>>("date_modified")?
            .and_then(|t| Utc.timestamp_opt(t, 0).single()),
        authors,
        attachments,
    })
}

fn fetch_by_feed(conn: &mut Connection, feed_id: &str, sort: SortOrder) -> Result<Vec<Article>> {
    let sql = format!(
        "SELECT * FROM articles WHERE feed_id = ? {}",
        sort.order_by_clause()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([feed_id], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

/// Bulk variant of `fetch_by_feed` — `WHERE feed_id IN (?, ?, ?)`. Used
/// for folder-aggregate views (NNW's "show all articles from feeds in
/// this folder") which previously fanned out N sequential single-feed
/// queries through the worker mpsc. For a folder with 50 feeds that
/// was 50 round-trips of channel-send, `blocking_recv`, SQLite plan,
/// and reply-send. With the IN clause it's one round-trip and one
/// SQLite query plan.
///
/// Chunks at 500 IDs (SQLite's default `SQLITE_LIMIT_VARIABLE_NUMBER`
/// is 999; we leave headroom). Empty input is a no-op.
fn fetch_by_feeds(
    conn: &mut Connection,
    feed_ids: &[String],
    sort: SortOrder,
) -> Result<Vec<Article>> {
    if feed_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut articles: Vec<Article> = Vec::new();
    for chunk in feed_ids.chunks(500) {
        let placeholders: String = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT * FROM articles WHERE feed_id IN ({placeholders}) {}",
            sort.order_by_clause()
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_article)?;
        for row in rows {
            articles.push(row?);
        }
    }
    // Per-chunk results are each individually sorted; chunks need a
    // final merge-sort pass so the aggregate view honours the global
    // sort order. Comparator branches on `sort` since `Reverse` only
    // flips for newest-first. Keyed on the same logical date as the SQL
    // ORDER BY (v2.8.1): `date_published`, falling back to `date_modified`.
    match sort {
        SortOrder::NewestFirst => {
            articles.sort_by_key(|a| std::cmp::Reverse(a.date_published.or(a.date_modified)))
        }
        SortOrder::OldestFirst => articles.sort_by_key(|a| a.date_published.or(a.date_modified)),
    }
    Ok(articles)
}

fn fetch_by_article_id(conn: &mut Connection, article_id: &str) -> Result<Option<Article>> {
    let mut stmt = conn.prepare("SELECT * FROM articles WHERE article_id = ?")?;
    let article = stmt.query_row([article_id], row_to_article).optional()?;
    Ok(article)
}

fn fetch_unread(conn: &mut Connection, sort: SortOrder) -> Result<Vec<Article>> {
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE s.read = 0 {}",
        sort.order_by_clause_aliased()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_starred(conn: &mut Connection, sort: SortOrder) -> Result<Vec<Article>> {
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE s.starred = 1 {}",
        sort.order_by_clause_aliased()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_unread_article_ids(conn: &mut Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT article_id FROM statuses WHERE read = 0")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    let mut ids = HashSet::new();
    for row in rows {
        ids.insert(row?);
    }
    Ok(ids)
}

fn fetch_starred_article_ids(conn: &mut Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT article_id FROM statuses WHERE starred = 1")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    let mut ids = HashSet::new();
    for row in rows {
        ids.insert(row?);
    }
    Ok(ids)
}

fn update_statuses_read(conn: &mut Connection, ids: &[String], read: bool) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    const CHUNK: usize = 500;
    for chunk in ids.chunks(CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "UPDATE statuses SET read = ? WHERE article_id IN ({})",
            placeholders
        );
        let mut params = vec![rusqlite::types::Value::from(if read { 1i64 } else { 0i64 })];
        params.extend(
            chunk
                .iter()
                .map(|s| rusqlite::types::Value::from(s.clone())),
        );
        conn.execute(&sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

fn update_statuses_starred(conn: &mut Connection, ids: &[String], starred: bool) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    const CHUNK: usize = 500;
    for chunk in ids.chunks(CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "UPDATE statuses SET starred = ? WHERE article_id IN ({})",
            placeholders
        );
        let mut params = vec![rusqlite::types::Value::from(if starred {
            1i64
        } else {
            0i64
        })];
        params.extend(
            chunk
                .iter()
                .map(|s| rusqlite::types::Value::from(s.clone())),
        );
        conn.execute(&sql, rusqlite::params_from_iter(params))?;
    }
    Ok(())
}

fn fetch_missing_article_ids(conn: &mut Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT article_id FROM statuses WHERE article_id NOT IN (SELECT article_id FROM articles)",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Unix seconds for **local midnight today**, expressed in UTC. The
/// only correct way to ask "is this UTC-stored timestamp on the user's
/// local 'today'?" — naïvely calling `.and_utc()` on a local naïve
/// date gives midnight-UTC instead of midnight-local-converted-to-UTC,
/// which skews the boundary by the local offset (4 h on EDT, 5 h on
/// EST, etc.). Pre-v2.6.1 `smart_feed_counts` had that bug while
/// `fetch_today` was correct, so the Today badge count and the
/// fetch-on-click list disagreed by exactly the local offset's worth
/// of articles arriving in the boundary hours. Funneling both through
/// this helper prevents the drift.
///
/// Returns 0 if the local TZ DB lookup fails (DST gap on midnight,
/// extremely unlikely; standard transitions happen at 02:00 local).
fn local_midnight_utc_seconds() -> i64 {
    use chrono::TimeZone;
    let local_today = chrono::Local::now().date_naive();
    let Some(local_midnight) = local_today.and_hms_opt(0, 0, 0) else {
        return 0;
    };
    match chrono::Local.from_local_datetime(&local_midnight) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&chrono::Utc).timestamp(),
        // Ambiguous: pick the earlier instance — DST fall-back at
        // midnight is vanishingly rare but let the boundary be
        // permissive in that case rather than dropping articles.
        chrono::LocalResult::Ambiguous(early, _) => early.with_timezone(&chrono::Utc).timestamp(),
        // Gap: spring-forward at midnight isn't a real-world scenario
        // (transitions happen at 02:00) but guard anyway.
        chrono::LocalResult::None => 0,
    }
}

fn fetch_today(conn: &mut Connection, sort: SortOrder) -> Result<Vec<Article>> {
    let today_start = local_midnight_utc_seconds();
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE s.date_arrived >= ? OR a.date_published >= ? {}",
        sort.order_by_clause_aliased()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([today_start, today_start], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    // v2.6.1: log the window + result count so a user reporting "Today
    // doesn't show today's articles" can confirm what `today_start`
    // actually resolved to. Run with `RUST_LOG=viaduct=debug` to surface.
    tracing::debug!(
        today_start_unix_seconds = today_start,
        today_start_local = %chrono::DateTime::from_timestamp(today_start, 0)
            .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
            .unwrap_or_default(),
        result_count = articles.len(),
        "fetch_today"
    );
    Ok(articles)
}

/// v2.7.0 — execute a Smart Feed's rules against the articles store.
/// Compiles `rules` into a WHERE clause via `smart_feeds::build_where`,
/// LEFT-JOINs `statuses` so missing-status rows behave as unread/unstarred
/// (matches `UnreadCountsByFeed` semantics), and orders by the supplied
/// `SortOrder`.
fn fetch_smart_feed(
    conn: &mut Connection,
    rules: &crate::smart_feeds::SmartFeedRules,
    sort: SortOrder,
) -> Result<Vec<Article>> {
    let (where_clause, params) = crate::smart_feeds::build_where(rules);
    let sql = format!(
        "SELECT a.* FROM articles a \
         LEFT JOIN statuses s ON a.article_id = s.article_id \
         WHERE {where_clause} {}",
        sort.order_by_clause_aliased()
    );
    let mut stmt = conn.prepare(&sql)?;
    let bind_params: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(bind_params), row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn search(conn: &mut Connection, query: &str) -> Result<Vec<Article>> {
    let mut stmt = conn.prepare(
        "SELECT a.* FROM articles a
         INNER JOIN search s ON a.article_id = s.article_id
         WHERE search MATCH ?
         ORDER BY rank",
    )?;
    let rows = stmt.query_map([query], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

/// Search + FTS5 `snippet()` fragment. Optional `feed_filter` restricts matches
/// to a single feed (used by the "this feed" scope toggle in the UI).
///
/// Column index `-1` lets FTS5 pick the best-matching column (title or body);
/// empty start/end markers mean the snippet is plain text suitable for a
/// GtkLabel. The 10-token window is the NNW-ish default.
fn search_with_snippets(
    conn: &mut Connection,
    query: &str,
    feed_filter: Option<&str>,
) -> Result<Vec<(Article, String)>> {
    let sql = if feed_filter.is_some() {
        "SELECT a.*, snippet(search, -1, '', '', '…', 10) AS snip
         FROM search
         INNER JOIN articles a ON a.article_id = search.article_id
         WHERE search MATCH ?1 AND a.feed_id = ?2
         ORDER BY rank"
    } else {
        "SELECT a.*, snippet(search, -1, '', '', '…', 10) AS snip
         FROM search
         INNER JOIN articles a ON a.article_id = search.article_id
         WHERE search MATCH ?1
         ORDER BY rank"
    };

    let mut stmt = conn.prepare(sql)?;
    let mapper = |row: &rusqlite::Row| -> rusqlite::Result<(Article, String)> {
        let article = row_to_article(row)?;
        let snip: String = row.get("snip")?;
        Ok((article, snip))
    };

    let mut out = Vec::new();
    if let Some(feed_id) = feed_filter {
        let rows = stmt.query_map(rusqlite::params![query, feed_id], mapper)?;
        for row in rows {
            out.push(row?);
        }
    } else {
        let rows = stmt.query_map([query], mapper)?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

fn fetch_statuses_by_ids(
    conn: &mut Connection,
    ids: &[String],
) -> Result<HashMap<String, (bool, bool)>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    // Chunk the IN-list to stay well under SQLite's 999-parameter default.
    const CHUNK: usize = 500;
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT article_id, read, starred FROM statuses WHERE article_id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |row| {
            let id: String = row.get(0)?;
            let read: i64 = row.get(1)?;
            let starred: i64 = row.get(2)?;
            Ok((id, (read != 0, starred != 0)))
        })?;
        for row in rows {
            let (id, v) = row?;
            out.insert(id, v);
        }
    }
    Ok(out)
}

/// Per-feed unread totals for the sidebar. Articles without a `statuses`
/// row are treated as unread (NNW semantics: a missing status implies the
/// article was just inserted and hasn't been seen yet) — the LEFT JOIN +
/// COALESCE handles that without a separate insert path.
fn unread_counts_by_feed(conn: &mut Connection) -> Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT a.feed_id, COUNT(*)
         FROM articles a
         LEFT JOIN statuses s ON a.article_id = s.article_id
         WHERE COALESCE(s.read, 0) = 0
         GROUP BY a.feed_id",
    )?;
    let rows = stmt.query_map([], |row| {
        let feed_id: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((feed_id, count))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (feed_id, count) = row?;
        if count > 0 {
            out.insert(feed_id, count);
        }
    }
    Ok(out)
}

/// Counts for the three Smart Feed rows. Today and All Unread mirror the
/// existing `fetch_today` / `fetch_unread` queries; Starred narrows to
/// starred AND unread to match NNW's `BuiltinSmartFeed.unreadCount`.
fn smart_feed_counts(conn: &mut Connection) -> Result<SmartFeedCounts> {
    // v2.6.1: was previously `.and_utc()` here while `fetch_today` did
    // the correct local→UTC conversion — the badge count spanned a
    // window starting 4–5 hours before the actual local midnight, so
    // the count and the click-result disagreed.
    let today_start = local_midnight_utc_seconds();

    let today_unread: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM articles a
         INNER JOIN statuses s ON a.article_id = s.article_id
         WHERE s.read = 0
           AND (s.date_arrived >= ? OR a.date_published >= ?)",
        params![today_start, today_start],
        |row| row.get(0),
    )?;

    // v2.6.2: INNER JOIN against articles so **orphan status rows**
    // (status preserved when an article is deleted by retention or a
    // feed-removal sweep — NNW behaviour, see `articles_ad_lookup`
    // trigger comment) don't bloat the count. `fetch_unread` /
    // `fetch_starred` join the same way, so the badge count matches
    // the click result. Pre-v2.6.2 the bare `COUNT(*) FROM statuses`
    // counted orphans, producing the user-visible bug "Mark All as
    // Read leaves the All Unread badge at 1" when an orphan
    // unread-status existed.
    let all_unread: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM articles a
         INNER JOIN statuses s ON a.article_id = s.article_id
         WHERE s.read = 0",
        [],
        |row| row.get(0),
    )?;

    let starred_unread: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM articles a
         INNER JOIN statuses s ON a.article_id = s.article_id
         WHERE s.starred = 1 AND s.read = 0",
        [],
        |row| row.get(0),
    )?;

    Ok(SmartFeedCounts {
        today_unread,
        all_unread,
        starred_unread,
    })
}

/// Port of NNW `ArticlesTable.update(parsedItems, feedID, deleteOlder, ...)`.
///
/// Pipeline:
/// 1. Map parsed items to `Article`s (computes MD5 article_id from feed_id + unique_id).
/// 2. Fetch existing articles for the feed.
/// 3. New = incoming not in DB. Updated = incoming present but content differs.
/// 4. If `delete_older`: delete existing articles that are (!starred, date_arrived < retention_days, no longer in feed).
/// 5. Ensure a `statuses` row for every new article. Stale items (>6 months old) default to `read=1`.
/// 6. Emit `ArticleChanges` for the UI.
fn update_feed(
    conn: &mut Connection,
    feed_id: &str,
    items: Vec<ParsedItem>,
    delete_older: bool,
    retention_days: i64,
) -> Result<ArticleChanges> {
    if items.is_empty() {
        return Ok(ArticleChanges::default());
    }

    // 1. Map to Articles keyed by article_id.
    let mut incoming: HashMap<String, Article> = HashMap::with_capacity(items.len());
    for p in &items {
        let a = parsed_to_article(p, feed_id);
        incoming.insert(a.article_id.clone(), a);
    }

    // 2. Fetch existing for this feed.
    let existing: HashMap<String, Article> = {
        let mut stmt = conn.prepare("SELECT * FROM articles WHERE feed_id = ?")?;
        let mut map = HashMap::new();
        let rows = stmt.query_map([feed_id], row_to_article)?;
        for row in rows {
            let a = row?;
            map.insert(a.article_id.clone(), a);
        }
        map
    };

    // 3. Diff.
    let mut new_articles: Vec<Article> = Vec::new();
    let mut updated_articles: Vec<Article> = Vec::new();
    for (id, inc) in &incoming {
        match existing.get(id) {
            None => new_articles.push(inc.clone()),
            Some(cur) if cur != inc => updated_articles.push(inc.clone()),
            _ => {}
        }
    }

    // 4. Determine deletes (only if delete_older=true, NNW's feedBased retention).
    let mut deleted_ids: HashSet<String> = HashSet::new();
    if delete_older {
        let retention_cutoff = Utc::now() - Duration::days(retention_days);
        let orphans: Vec<&String> = existing
            .keys()
            .filter(|id| !incoming.contains_key(*id))
            .collect();
        if !orphans.is_empty() {
            let mut status_stmt =
                conn.prepare("SELECT starred, date_arrived FROM statuses WHERE article_id = ?")?;
            for id in orphans {
                let status: Option<(bool, i64)> = status_stmt
                    .query_row([id], |row| Ok((row.get::<_, i64>(0)? != 0, row.get(1)?)))
                    .optional()?;
                if let Some((starred, date_arrived)) = status
                    && !starred
                    && Utc
                        .timestamp_opt(date_arrived, 0)
                        .single()
                        .map(|t| t < retention_cutoff)
                        .unwrap_or(true)
                {
                    deleted_ids.insert(id.clone());
                }
            }
        }
    }

    // 5. Write everything in a single transaction.
    let tx = conn.transaction()?;
    {
        let stale_cutoff = Utc::now() - Duration::days(STALE_INTERVAL_DAYS);
        let now = Utc::now();

        let mut article_stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO articles (
                article_id, feed_id, title, content_html, content_text,
                url, external_url, summary, image_url, date_published, date_modified,
                authors, attachments
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;
        let mut status_stmt = tx.prepare_cached(
            "INSERT INTO statuses (article_id, read, starred, date_arrived)
             VALUES (?, ?, 0, ?)
             ON CONFLICT(article_id) DO NOTHING",
        )?;
        let mut delete_stmt = tx.prepare_cached("DELETE FROM articles WHERE article_id = ?")?;

        for a in new_articles.iter().chain(updated_articles.iter()) {
            let authors_json = serde_json::to_string(&a.authors)
                .map_err(|e| DatabaseError::Migration(e.to_string()))?;
            let attachments_json = serde_json::to_string(&a.attachments)
                .map_err(|e| DatabaseError::Migration(e.to_string()))?;
            article_stmt.execute(params![
                a.article_id,
                a.feed_id,
                a.title,
                a.content_html,
                a.content_text,
                a.url,
                a.external_url,
                a.summary,
                a.image_url,
                a.date_published.map(|d| d.timestamp()),
                a.date_modified.map(|d| d.timestamp()),
                authors_json,
                attachments_json,
            ])?;
        }

        for a in &new_articles {
            let is_stale = a.date_published.map(|d| d < stale_cutoff).unwrap_or(false);
            status_stmt.execute(params![
                a.article_id,
                if is_stale { 1 } else { 0 },
                now.timestamp(),
            ])?;
        }

        for id in &deleted_ids {
            delete_stmt.execute([id])?;
        }
    }
    tx.commit()?;

    Ok(ArticleChanges {
        new_articles,
        updated_articles,
        deleted_article_ids: deleted_ids,
        statuses: Vec::new(),
    })
}

/// Port of NNW `ArticlesTable.deleteArticlesNotInSubscribedToFeedIDs`.
/// Empty input is a no-op (NNW: `if feedIDs.isEmpty { return }`) so a
/// transient OPML-load failure can't blow away the user's article history.
/// The `articles_ad` and `articles_ad_lookup` triggers cascade FTS5 +
/// authorsLookup cleanup automatically.
fn delete_articles_not_in_feeds(conn: &mut Connection, feed_ids: &[String]) -> Result<usize> {
    if feed_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; feed_ids.len()].join(", ");
    let sql = format!(
        "DELETE FROM articles WHERE feed_id NOT IN ({})",
        placeholders
    );
    let count = conn.execute(&sql, rusqlite::params_from_iter(feed_ids))?;
    Ok(count)
}

/// Port of NNW `ArticlesTable.deleteOldStatuses` (`feedBased` branch):
///   `DELETE FROM statuses WHERE date_arrived < ? AND starred = 0
///    AND article_id NOT IN (SELECT article_id FROM articles)`
/// Status rows are intentionally retained when the article still exists
/// (so read/starred state survives idempotent feed reloads); this only
/// reaps the long tail of orphaned status rows after retention has
/// removed the underlying article.
fn delete_old_statuses(conn: &mut Connection, retention_days: i64) -> Result<usize> {
    let cutoff = (Utc::now() - Duration::days(retention_days)).timestamp();
    let count = conn.execute(
        "DELETE FROM statuses
         WHERE date_arrived < ?
           AND starred = 0
           AND article_id NOT IN (SELECT article_id FROM articles)",
        params![cutoff],
    )?;
    Ok(count)
}

/// `VACUUM` reclaims unused pages and rebuilds the file. Must run outside
/// any open transaction; the worker thread serializes ops so that holds
/// naturally. Cheap on small DBs, expensive after a large prune — fire
/// once per startup to amortize.
///
/// **v2.6.11**: also runs `PRAGMA wal_checkpoint(TRUNCATE)` first to
/// reclaim any WAL pages a prior session left lying around. Without
/// this, an existing 149-MB WAL (the diagnostic value that surfaced
/// the issue) sticks around even after the new `journal_size_limit`
/// would prevent fresh growth — SQLite truncates only at the next
/// checkpoint that crosses the limit.
fn vacuum(conn: &mut Connection) -> Result<()> {
    // PRAGMA returns rows; query_row would error on no-result, but
    // execute_batch tolerates the result set being discarded. The
    // checkpoint runs serially relative to other DB ops because the
    // worker holds the only connection.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// `PRAGMA wal_checkpoint(TRUNCATE)` without the `VACUUM`. Flushes the WAL
/// into the main database and truncates the WAL file. Run every startup so
/// the WAL stays bounded even on launches that prune nothing (the full
/// `VACUUM` is gated on prune activity in `cleanup_at_startup`).
fn checkpoint(conn: &mut Connection) -> Result<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

/// Port of NNW `deleteOrphanedAuthorsLookupRows` (issue #5232 fix). Wrapped
/// in a single transaction. The `articles_ad_lookup` delete-trigger handles
/// the live case where an article is deleted, but transactions that bypass
/// the trigger (or pre-trigger DBs from older builds) can leave dangling
/// rows. Returns the count of orphan author rows removed.
fn delete_orphaned_authors(conn: &mut Connection) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM authorsLookup
             WHERE article_id NOT IN (SELECT article_id FROM articles)",
        [],
    )?;
    let removed = tx.execute(
        "DELETE FROM authors
             WHERE author_id NOT IN (SELECT DISTINCT author_id FROM authorsLookup)",
        [],
    )?;
    tx.commit()?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory");
        setup_schema(&conn).expect("schema");
        conn
    }

    /// `local_midnight_utc_seconds()` must agree with a manual
    /// `Local::now()` → midnight → to_utc round-trip. The pre-v2.6.1
    /// `smart_feed_counts` had `.and_utc()` here which silently used
    /// midnight-UTC instead of midnight-local-converted-to-UTC; this
    /// test locks the helper to the correct semantic so a regression
    /// would fail loudly.
    #[test]
    fn local_midnight_helper_matches_explicit_local_midnight() {
        use chrono::{Local, NaiveDate, TimeZone};
        let helper = local_midnight_utc_seconds();
        let today_local: NaiveDate = Local::now().date_naive();
        let expected = match Local.from_local_datetime(
            &today_local
                .and_hms_opt(0, 0, 0)
                .expect("midnight is always representable"),
        ) {
            chrono::LocalResult::Single(dt) => dt.with_timezone(&chrono::Utc).timestamp(),
            chrono::LocalResult::Ambiguous(early, _) => {
                early.with_timezone(&chrono::Utc).timestamp()
            }
            chrono::LocalResult::None => 0,
        };
        assert_eq!(helper, expected);

        // Sanity: the helper's value matches the local clock — at any
        // moment of the day, the local clock should be on or after
        // local midnight, so `Local::now().timestamp() >= helper`.
        assert!(Local::now().timestamp() >= helper);
    }

    fn item(id: &str, title: &str, body: &str) -> ParsedItem {
        ParsedItem {
            id: id.to_string(),
            title: Some(title.to_string()),
            content_html: Some(body.to_string()),
            content_text: None,
            url: None,
            external_url: None,
            summary: None,
            image_url: None,
            date_published: Some(Utc::now()),
            date_modified: None,
            authors: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn article_id_is_md5_of_feed_and_unique() {
        // Regression test: the synthetic ID must be stable across builds.
        let a = article_id_for("https://example.com/feed", "guid-1");
        let b = article_id_for("https://example.com/feed", "guid-1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    /// v2.6.2 regression: orphan status rows (status row exists, no
    /// matching article) must NOT count toward the All Unread / Starred
    /// smart-feed badges. Pre-v2.6.2 the bare `COUNT(*) FROM statuses`
    /// included them, so "Mark All as Read" left the All Unread badge
    /// stuck at 1 because the orphan was unmarkable from the visible
    /// timeline.
    #[test]
    fn smart_feed_counts_excludes_orphan_statuses() {
        let mut conn = in_memory();

        // One real article + status (read=0).
        let feed_id = "https://example.com/feed";
        update_feed(
            &mut conn,
            feed_id,
            vec![item("guid-1", "Real article", "real body")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .expect("update_feed");

        // Orphan status: a status row whose article was deleted (mirrors
        // what happens when a feed is removed but the status outlives
        // the article via the `articles_ad_lookup` cascade-skip).
        conn.execute(
            "INSERT INTO statuses (article_id, read, starred, date_arrived) VALUES (?, 0, 0, ?)",
            params![
                "orphan-status-id-with-no-matching-article",
                Utc::now().timestamp(),
            ],
        )
        .expect("insert orphan status");

        // Sanity: bare statuses count includes the orphan…
        let bare: i64 = conn
            .query_row("SELECT COUNT(*) FROM statuses WHERE read = 0", [], |r| {
                r.get(0)
            })
            .expect("bare count");
        assert_eq!(
            bare, 2,
            "raw statuses table has 2 unread rows (real + orphan)"
        );

        // …but the smart-feed count doesn't.
        let counts = smart_feed_counts(&mut conn).expect("smart_feed_counts");
        assert_eq!(
            counts.all_unread, 1,
            "all_unread must INNER JOIN articles to exclude orphans"
        );

        // Also covers `starred_unread`: orphan with starred=1 must
        // similarly not count.
        conn.execute(
            "INSERT INTO statuses (article_id, read, starred, date_arrived) VALUES (?, 0, 1, ?)",
            params![
                "orphan-starred-id-with-no-matching-article",
                Utc::now().timestamp(),
            ],
        )
        .expect("insert orphan starred status");
        let counts = smart_feed_counts(&mut conn).expect("smart_feed_counts re-run");
        assert_eq!(
            counts.starred_unread, 0,
            "starred_unread must INNER JOIN articles too"
        );
    }

    #[test]
    fn update_feed_inserts_new_and_diffs_updated() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";

        let changes = update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "First", "body1"), item("b", "Second", "body2")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        assert_eq!(changes.new_articles.len(), 2);
        assert_eq!(changes.updated_articles.len(), 0);
        assert_eq!(changes.deleted_article_ids.len(), 0);

        // Re-run with one unchanged, one updated, one new.
        let changes = update_feed(
            &mut conn,
            feed_id,
            vec![
                item("a", "First", "body1"),            // unchanged
                item("b", "Second v2", "body2 edited"), // updated
                item("c", "Third", "body3"),            // new
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        assert_eq!(changes.new_articles.len(), 1);
        assert_eq!(changes.updated_articles.len(), 1);
        assert_eq!(changes.deleted_article_ids.len(), 0);
    }

    #[test]
    fn update_feed_deletes_orphans_when_flag_set() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";

        update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "A", "1"), item("b", "B", "2")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // Backdate the status of `a` so retention can sweep it.
        let a_id = article_id_for(feed_id, "a");
        conn.execute(
            "UPDATE statuses SET date_arrived = ? WHERE article_id = ?",
            params![
                (Utc::now() - Duration::days(DEFAULT_RETENTION_DAYS + 5)).timestamp(),
                a_id,
            ],
        )
        .unwrap();

        // Feed now only contains `b` — `a` should be deleted.
        let changes = update_feed(
            &mut conn,
            feed_id,
            vec![item("b", "B", "2")],
            true,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        assert!(changes.deleted_article_ids.contains(&a_id));
    }

    #[test]
    fn update_feed_honors_custom_retention_days() {
        // Regression for the GSettings-driven knob: a 7-day retention
        // should sweep an article whose status row arrived 10 days ago,
        // even though the default 30-day retention would keep it.
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";

        update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "A", "1"), item("b", "B", "2")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let a_id = article_id_for(feed_id, "a");
        conn.execute(
            "UPDATE statuses SET date_arrived = ? WHERE article_id = ?",
            params![(Utc::now() - Duration::days(10)).timestamp(), a_id],
        )
        .unwrap();

        let changes = update_feed(&mut conn, feed_id, vec![item("b", "B", "2")], true, 7).unwrap();
        assert!(changes.deleted_article_ids.contains(&a_id));
    }

    #[test]
    fn search_with_snippets_returns_excerpt_and_respects_feed_filter() {
        let mut conn = in_memory();
        let feed_a = "https://a.example/feed";
        let feed_b = "https://b.example/feed";

        let mut item_a = item(
            "1",
            "Rust memory safety",
            "Rust guarantees memory safety at compile time.",
        );
        item_a.content_text = item_a.content_html.clone();
        let mut item_b = item(
            "2",
            "Garbage collection",
            "Java uses a garbage collector for memory management.",
        );
        item_b.content_text = item_b.content_html.clone();

        update_feed(
            &mut conn,
            feed_a,
            vec![item_a],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        update_feed(
            &mut conn,
            feed_b,
            vec![item_b],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let results = search_with_snippets(&mut conn, "memory", None).unwrap();
        assert_eq!(results.len(), 2);
        for (_, snip) in &results {
            assert!(
                !snip.is_empty(),
                "snippet should never be empty for a match"
            );
        }

        let scoped = search_with_snippets(&mut conn, "memory", Some(feed_a)).unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].0.feed_id, feed_a);
    }

    #[test]
    fn stale_articles_default_to_read() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";

        let mut old = item("old", "Old", "body");
        old.date_published = Some(Utc::now() - Duration::days(STALE_INTERVAL_DAYS + 1));

        update_feed(&mut conn, feed_id, vec![old], false, DEFAULT_RETENTION_DAYS).unwrap();

        let old_id = article_id_for(feed_id, "old");
        let read: i64 = conn
            .query_row(
                "SELECT read FROM statuses WHERE article_id = ?",
                [&old_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(read, 1);
    }

    #[test]
    fn delete_articles_not_in_feeds_evicts_orphans_only() {
        let mut conn = in_memory();
        let feed_a = "https://a.example/feed";
        let feed_b = "https://b.example/feed";

        update_feed(
            &mut conn,
            feed_a,
            vec![item("1", "A1", "x")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        update_feed(
            &mut conn,
            feed_b,
            vec![item("2", "B1", "y")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // User unsubscribed from feed_b. feed_a survives.
        let removed = delete_articles_not_in_feeds(&mut conn, &[feed_a.to_string()]).unwrap();
        assert_eq!(removed, 1);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 1);

        let remaining_feed: String = conn
            .query_row("SELECT feed_id FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_feed, feed_a);
    }

    #[test]
    fn delete_articles_not_in_feeds_empty_input_is_noop() {
        // Regression mirror of the FeedSettingsDatabase early-return: a
        // transient OPML failure (yielding zero subscribed feed IDs) must
        // not trigger a wholesale article wipe.
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "A", "x")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let removed = delete_articles_not_in_feeds(&mut conn, &[]).unwrap();
        assert_eq!(removed, 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn delete_old_statuses_prunes_orphans_only() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        update_feed(
            &mut conn,
            feed_id,
            vec![
                item("live", "Live", "x"),
                item("orphan_recent", "Recent", "y"),
                item("orphan_old", "Old", "z"),
                item("orphan_starred", "Star", "s"),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // Mark `orphan_starred` starred, backdate two of the three orphans, and
        // drop the article rows so their statuses become true orphans.
        let starred_id = article_id_for(feed_id, "orphan_starred");
        conn.execute(
            "UPDATE statuses SET starred = 1 WHERE article_id = ?",
            [&starred_id],
        )
        .unwrap();

        let old_id = article_id_for(feed_id, "orphan_old");
        let starred_id_for_back = starred_id.clone();
        for id in [&old_id, &starred_id_for_back] {
            conn.execute(
                "UPDATE statuses SET date_arrived = ? WHERE article_id = ?",
                params![(Utc::now() - Duration::days(60)).timestamp(), id],
            )
            .unwrap();
        }

        for id in ["orphan_recent", "orphan_old", "orphan_starred"] {
            let aid = article_id_for(feed_id, id);
            conn.execute("DELETE FROM articles WHERE article_id = ?", [&aid])
                .unwrap();
        }

        let removed = delete_old_statuses(&mut conn, DEFAULT_RETENTION_DAYS).unwrap();
        assert_eq!(removed, 1, "only orphan_old should be pruned");

        // `live` (article still exists), `orphan_recent` (within retention),
        // and `orphan_starred` (starred=1) all survive.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM statuses", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn vacuum_succeeds_on_clean_db() {
        let mut conn = in_memory();
        // No transaction open; vacuum should run without error and leave
        // the schema intact.
        vacuum(&mut conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn delete_orphaned_authors_cleans_unreferenced_rows() {
        // Drop an authorsLookup row out from under the trigger by inserting
        // it manually after the article is deleted — simulates a pre-trigger
        // DB or a transaction that bypassed the cascade. The cleanup op
        // should sweep the lookup row AND the now-orphan author row.
        let mut conn = in_memory();
        // Seed an author + an orphan lookup row pointing at a non-existent
        // article. Authors with no lookup also count as orphans.
        conn.execute(
            "INSERT INTO authors (author_id, name, url, avatar_url, email)
             VALUES (1, 'Live Author', NULL, NULL, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO authors (author_id, name, url, avatar_url, email)
             VALUES (2, 'Orphan Author', NULL, NULL, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO authorsLookup (article_id, author_id) VALUES ('ghost-article', 2)",
            [],
        )
        .unwrap();

        let removed = delete_orphaned_authors(&mut conn).unwrap();
        // Orphan Author (2) and Live Author (1, because no lookup pointed
        // at it) both gone — neither is referenced by any live article.
        assert_eq!(removed, 2);

        let lookup_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM authorsLookup", [], |r| r.get(0))
            .unwrap();
        let author_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM authors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lookup_count, 0);
        assert_eq!(author_count, 0);
    }

    /// v2.8.1: the timeline sort keys on a logical date
    /// `COALESCE(date_published, date_modified)`. An article with only a
    /// modified date (no published date) must sort by that modified date,
    /// not get dumped at the NULL end of the list.
    #[test]
    fn timeline_sort_uses_logical_date_when_published_is_missing() {
        use chrono::Duration;
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let now = Utc::now();

        let mk = |guid: &str,
                  published: Option<chrono::DateTime<Utc>>,
                  modified: Option<chrono::DateTime<Utc>>| ParsedItem {
            id: guid.to_string(),
            title: Some(guid.to_string()),
            content_html: Some("body".to_string()),
            content_text: None,
            url: None,
            external_url: None,
            summary: None,
            image_url: None,
            date_published: published,
            date_modified: modified,
            authors: Vec::new(),
            attachments: Vec::new(),
        };

        update_feed(
            &mut conn,
            feed_id,
            vec![
                mk("recent", Some(now - Duration::hours(1)), None),
                mk("modified-only", None, Some(now - Duration::hours(3))),
                mk("oldest", Some(now - Duration::hours(5)), None),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .expect("update_feed");

        let titles: Vec<String> = fetch_by_feed(&mut conn, feed_id, SortOrder::NewestFirst)
            .expect("fetch")
            .into_iter()
            .filter_map(|a| a.title)
            .collect();
        // Logical-date order: 1h, 3h (via date_modified), 5h. Before the
        // coalesce, "modified-only" had a NULL key and sorted last.
        assert_eq!(titles, vec!["recent", "modified-only", "oldest"]);
    }
}

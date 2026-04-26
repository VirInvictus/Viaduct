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

/// NNW's `feedBased` retention cutoff — non-starred articles older than 30 days get pruned.
const RETENTION_CUTOFF_DAYS: i64 = 30;

pub enum ArticlesDbOp {
    BatchInsert(Vec<Article>, oneshot::Sender<Result<()>>),
    UpsertStatuses(Vec<ArticleStatus>, oneshot::Sender<Result<()>>),
    FetchByFeed(String, oneshot::Sender<Result<Vec<Article>>>),
    FetchByArticleId(String, oneshot::Sender<Result<Option<Article>>>),
    FetchUnread(oneshot::Sender<Result<Vec<Article>>>),
    FetchStarred(oneshot::Sender<Result<Vec<Article>>>),
    FetchToday(oneshot::Sender<Result<Vec<Article>>>),
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
    UpdateFeed {
        feed_id: String,
        items: Vec<ParsedItem>,
        delete_older: bool,
        reply: oneshot::Sender<Result<ArticleChanges>>,
    },
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
        PRAGMA mmap_size = 30000000000;

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
        ArticlesDbOp::FetchByFeed(feed_id, tx) => {
            let res = fetch_by_feed(conn, &feed_id);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByArticleId(article_id, tx) => {
            let res = fetch_by_article_id(conn, &article_id);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchUnread(tx) => {
            let res = fetch_unread(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStarred(tx) => {
            let res = fetch_starred(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchToday(tx) => {
            let res = fetch_today(conn);
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
            reply,
        } => {
            let res = update_feed(conn, &feed_id, items, delete_older);
            let _ = reply.send(res);
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
            .map(|t| Utc.timestamp_opt(t, 0).unwrap()),
        date_modified: row
            .get::<_, Option<i64>>("date_modified")?
            .map(|t| Utc.timestamp_opt(t, 0).unwrap()),
        authors,
        attachments,
    })
}

fn fetch_by_feed(conn: &mut Connection, feed_id: &str) -> Result<Vec<Article>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM articles WHERE feed_id = ? ORDER BY date_published DESC, rowid DESC",
    )?;
    let rows = stmt.query_map([feed_id], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_by_article_id(conn: &mut Connection, article_id: &str) -> Result<Option<Article>> {
    let mut stmt = conn.prepare("SELECT * FROM articles WHERE article_id = ?")?;
    let article = stmt.query_row([article_id], row_to_article).optional()?;
    Ok(article)
}

fn fetch_unread(conn: &mut Connection) -> Result<Vec<Article>> {
    let mut stmt = conn.prepare(
        "SELECT a.* FROM articles a 
         INNER JOIN statuses s ON a.article_id = s.article_id 
         WHERE s.read = 0 
         ORDER BY a.date_published DESC, a.rowid DESC",
    )?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_starred(conn: &mut Connection) -> Result<Vec<Article>> {
    let mut stmt = conn.prepare(
        "SELECT a.* FROM articles a 
         INNER JOIN statuses s ON a.article_id = s.article_id 
         WHERE s.starred = 1 
         ORDER BY a.date_published DESC, a.rowid DESC",
    )?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_today(conn: &mut Connection) -> Result<Vec<Article>> {
    let today_start = chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    let mut stmt = conn.prepare(
        "SELECT a.* FROM articles a 
         INNER JOIN statuses s ON a.article_id = s.article_id 
         WHERE s.date_arrived >= ? OR a.date_published >= ? 
         ORDER BY a.date_published DESC, a.rowid DESC",
    )?;
    let rows = stmt.query_map([today_start, today_start], row_to_article)?;
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
    let today_start = chrono::Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();

    let today_unread: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM articles a
         INNER JOIN statuses s ON a.article_id = s.article_id
         WHERE s.read = 0
           AND (s.date_arrived >= ? OR a.date_published >= ?)",
        params![today_start, today_start],
        |row| row.get(0),
    )?;

    let all_unread: i64 =
        conn.query_row("SELECT COUNT(*) FROM statuses WHERE read = 0", [], |row| {
            row.get(0)
        })?;

    let starred_unread: i64 = conn.query_row(
        "SELECT COUNT(*) FROM statuses WHERE starred = 1 AND read = 0",
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
/// 4. If `delete_older`: delete existing articles that are (!starred, date_arrived < 30d, no longer in feed).
/// 5. Ensure a `statuses` row for every new article. Stale items (>6 months old) default to `read=1`.
/// 6. Emit `ArticleChanges` for the UI.
fn update_feed(
    conn: &mut Connection,
    feed_id: &str,
    items: Vec<ParsedItem>,
    delete_older: bool,
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
        let retention_cutoff = Utc::now() - Duration::days(RETENTION_CUTOFF_DAYS);
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
                    && Utc.timestamp_opt(date_arrived, 0).unwrap() < retention_cutoff
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

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory");
        setup_schema(&conn).expect("schema");
        conn
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

    #[test]
    fn update_feed_inserts_new_and_diffs_updated() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";

        let changes = update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "First", "body1"), item("b", "Second", "body2")],
            false,
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
        )
        .unwrap();

        // Backdate the status of `a` so retention can sweep it.
        let a_id = article_id_for(feed_id, "a");
        conn.execute(
            "UPDATE statuses SET date_arrived = ? WHERE article_id = ?",
            params![
                (Utc::now() - Duration::days(RETENTION_CUTOFF_DAYS + 5)).timestamp(),
                a_id,
            ],
        )
        .unwrap();

        // Feed now only contains `b` — `a` should be deleted.
        let changes = update_feed(&mut conn, feed_id, vec![item("b", "B", "2")], true).unwrap();
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

        update_feed(&mut conn, feed_a, vec![item_a], false).unwrap();
        update_feed(&mut conn, feed_b, vec![item_b], false).unwrap();

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

        update_feed(&mut conn, feed_id, vec![old], false).unwrap();

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
}

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

/// Row cap on the timeline-feeding queries (`FetchByFeed`, `FetchByFeeds`,
/// `FetchUnread`, `FetchStarred`, `FetchToday`). Without it a folder
/// aggregate or an archive-hoarding feed materialized every row's full
/// `content_html` in one splice: hundreds of MB of RAM for a browse that
/// only ever shows the newest handful. 1000 newest is far past any
/// reading session; unread badges and mark-read paths are separate
/// queries and stay exact. `0` means unlimited (the mark-read callers).
pub const TIMELINE_FETCH_LIMIT: i64 = 1000;

/// ` LIMIT n` suffix for the timeline queries; `limit <= 0` appends
/// nothing. Internal constant only, never caller input.
fn limit_clause(limit: i64) -> String {
    if limit > 0 {
        format!(" LIMIT {limit}")
    } else {
        String::new()
    }
}

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
///
/// v4.1.0: the title variants port NNW `70c3ec809` ("Sort by the
/// displayed title text so untitled articles sort by their body
/// excerpt") as a SQL-level sort key: the trimmed `title`, falling
/// back to a 300-character `content_text` excerpt, then `summary`,
/// compared case-insensitively (`COLLATE NOCASE`). The excerpt tier
/// reads plain text derived from the HTML body at ingest
/// (`parser::html::strip_html_to_text`), the twin of upstream's
/// at-sort-time `strippingHTML` — without the ingest derivation the
/// tier is empty for RSS, whose items only ever carried `content_html`.
/// Divergences from upstream, accepted at the "SQL level where
/// practical" call: `NOCASE` folds ASCII case only (no locale-aware or
/// diacritic-insensitive collation exists in SQLite), entities stay as
/// the feed wrote them rather than being decoded after the strip, and
/// collation is byte-level `NOCASE` rather than locale-aware. Title
/// ties break newest-first-then-rowid in *both* directions, matching
/// upstream's hardcoded `.orderedDescending` date tiebreak. The
/// timeline cap applies in sort order (the first/last N titles), the
/// same shape `OldestFirst` has always had: the cap follows the
/// `ORDER BY`, it never re-scopes the visible set by date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    NewestFirst,
    OldestFirst,
    TitleAscending,
    TitleDescending,
}

/// v4.1.0 (NNW `70c3ec809`): the title sort key. The displayed title,
/// falling back to the body excerpt (`content_text`, capped at 300
/// characters like upstream's `truncatedSummary`), then `summary`.
/// `NULLIF(TRIM(...), '')` makes whitespace-only and empty values fall
/// through to the next source, so only an article with no title and no
/// readable text sorts as NULL. The `COLLATE NOCASE` rides the whole
/// expression: it is the collation SQLite uses to compare the key.
///
/// `TRIM` here takes an explicit charset — the six ASCII whitespace
/// characters — because one-argument `TRIM` strips only spaces, while
/// the Rust twin of this key (`title_sort_key`, used by the
/// `fetch_by_feeds` merge) trims the same six on both ends: keeping
/// the two sets identical is what makes a feed view and a folder view
/// order the same article identically. Trim also runs *before* the
/// 300-character cut on both sides.
///
/// Macros rather than consts because `concat!` only composes literals,
/// and every rendered clause must stay a `&'static str`.
macro_rules! title_sort_key {
    ($p:literal) => {
        concat!(
            "COALESCE(NULLIF(TRIM(",
            $p,
            "title, ' \t\n\u{0b}\u{0c}\r'), ''), NULLIF(SUBSTR(TRIM(",
            $p,
            "content_text, ' \t\n\u{0b}\u{0c}\r'), 1, 300), ''), NULLIF(TRIM(",
            $p,
            "summary, ' \t\n\u{0b}\u{0c}\r'), '')) COLLATE NOCASE"
        )
    };
}

/// The full `ORDER BY` tail for a title sort: the key in `$dir`, then
/// upstream's hardcoded tiebreak — logical date newest-first (both
/// directions), then rowid — so equal titles stay deterministic.
macro_rules! title_order_clause {
    ($p:literal, $dir:literal) => {
        concat!(
            "ORDER BY ",
            title_sort_key!($p),
            " ",
            $dir,
            ", COALESCE(",
            $p,
            "date_published, ",
            $p,
            "date_modified) DESC, ",
            $p,
            "rowid DESC"
        )
    };
}

impl SortOrder {
    /// Render to the SQL `ORDER BY` tail. Includes the `rowid`
    /// secondary key so two articles with the same logical date still
    /// have a deterministic order. Title sorts tie on the logical date
    /// newest-first in both directions (upstream `70c3ec809` hardcodes
    /// `.orderedDescending` for the title tiebreak), then rowid.
    pub fn order_by_clause(&self) -> &'static str {
        match self {
            SortOrder::NewestFirst => {
                "ORDER BY COALESCE(date_published, date_modified) DESC, rowid DESC"
            }
            SortOrder::OldestFirst => {
                "ORDER BY COALESCE(date_published, date_modified) ASC, rowid ASC"
            }
            SortOrder::TitleAscending => title_order_clause!("", "ASC"),
            SortOrder::TitleDescending => title_order_clause!("", "DESC"),
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
            SortOrder::TitleAscending => title_order_clause!("a.", "ASC"),
            SortOrder::TitleDescending => title_order_clause!("a.", "DESC"),
        }
    }
}

pub enum ArticlesDbOp {
    BatchInsert(Vec<Article>, oneshot::Sender<Result<()>>),
    UpsertStatuses(Vec<ArticleStatus>, oneshot::Sender<Result<()>>),
    FetchByFeed(
        String,
        SortOrder,
        i64,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    /// Read-filtered variant of `FetchByFeed` backing the timeline's
    /// "Show read articles" toggle. `include_read = false` hides articles
    /// whose status row marks them read; a missing status row counts as
    /// unread, per NNW's `defaultReadFilterType` semantics.
    FetchByFeedFiltered(
        String,
        SortOrder,
        i64,
        bool,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    /// Bulk variant of `FetchByFeed`. One SQL query with an `IN (?, ?, …)`
    /// clause replaces the previous N-round-trip fan-out used by folder
    /// aggregate views. Empty input is a no-op.
    FetchByFeeds(
        Vec<String>,
        SortOrder,
        i64,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    /// Read-filtered variant of `FetchByFeeds`; see `FetchByFeedFiltered`.
    FetchByFeedsFiltered(
        Vec<String>,
        SortOrder,
        i64,
        bool,
        oneshot::Sender<Result<Vec<Article>>>,
    ),
    FetchByArticleId(String, oneshot::Sender<Result<Option<Article>>>),
    FetchUnread(SortOrder, i64, oneshot::Sender<Result<Vec<Article>>>),
    FetchStarred(SortOrder, i64, oneshot::Sender<Result<Vec<Article>>>),
    FetchUnreadArticleIds(oneshot::Sender<Result<HashSet<String>>>),
    FetchStarredArticleIds(oneshot::Sender<Result<HashSet<String>>>),
    UpdateStatusesRead(Vec<String>, bool, oneshot::Sender<Result<()>>),
    UpdateStatusesStarred(Vec<String>, bool, oneshot::Sender<Result<()>>),
    FetchMissingArticleIds(oneshot::Sender<Result<Vec<String>>>),
    FetchToday(SortOrder, i64, oneshot::Sender<Result<Vec<Article>>>),
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
    /// Read `db_info.last_vacuum_date` (unix seconds), `None` if never
    /// stamped. Lets `cleanup_at_startup` throttle the full VACUUM.
    LastVacuumDate(oneshot::Sender<Result<Option<i64>>>),
    /// Read a generic `db_info` value by key. Account-level bookkeeping
    /// with no per-feed home; currently the Inoreader sync
    /// conditional-GET markers (`sync-cget-*` keys, delegate.rs).
    GetDbInfo {
        key: String,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
    /// Upsert a generic `db_info` value by key. An empty value is the
    /// "absent" encoding for readers (matches LastVacuumDate's parse).
    SetDbInfo {
        key: String,
        value: String,
        reply: oneshot::Sender<Result<()>>,
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

        -- v4.0.5 (NNW 19930aa3c): the Today queries (`fetch_today` /
        -- `smart_feed_counts`) OR two date windows across the articles ↔
        -- statuses join; before these indexes the planner full-scanned one
        -- side of the join on every Today click. Analog of upstream's
        -- `articles_feedID_datePublished_articleID` minus the feedID prefix
        -- (our Today queries are account-wide), plus the statuses twin
        -- upstream never needed (their fallback branch is gated on
        -- `datePublished is null` and stays seekable on the articles index
        -- alone; ours tests `date_arrived` as an independent window). The
        -- trailing `article_id` mirrors upstream's trailing `articleID`: it
        -- makes each index covering for the join key / the IN-subquery the
        -- rewritten WHERE feeds it.
        CREATE INDEX IF NOT EXISTS articles_date_published_idx
            ON articles (date_published, article_id);
        CREATE INDEX IF NOT EXISTS statuses_date_arrived_idx
            ON statuses (date_arrived, article_id);

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

        -- Per-database key/value metadata. Port of NNW `RSDatabaseInfoTable`
        -- (4c85c907f). Currently holds `last_vacuum_date` (unix seconds) so
        -- `cleanup_at_startup` can throttle the full VACUUM (see
        -- `last_vacuum_date` / `stamp_vacuum_date`).
        CREATE TABLE IF NOT EXISTS db_info (
            key TEXT PRIMARY KEY,
            value TEXT
        );
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
        ArticlesDbOp::FetchByFeed(feed_id, sort, limit, tx) => {
            let res = fetch_by_feed(conn, &feed_id, sort, limit, true);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByFeedFiltered(feed_id, sort, limit, include_read, tx) => {
            let res = fetch_by_feed(conn, &feed_id, sort, limit, include_read);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByFeeds(feed_ids, sort, limit, tx) => {
            let res = fetch_by_feeds(conn, &feed_ids, sort, limit, true);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByFeedsFiltered(feed_ids, sort, limit, include_read, tx) => {
            let res = fetch_by_feeds(conn, &feed_ids, sort, limit, include_read);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchByArticleId(article_id, tx) => {
            let res = fetch_by_article_id(conn, &article_id);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchUnread(sort, limit, tx) => {
            let res = fetch_unread(conn, sort, limit);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStarred(sort, limit, tx) => {
            let res = fetch_starred(conn, sort, limit);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchUnreadArticleIds(tx) => {
            let res = fetch_status_ids(conn, "read = 0");
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchStarredArticleIds(tx) => {
            let res = fetch_status_ids(conn, "starred = 1");
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpdateStatusesRead(ids, read, tx) => {
            let res = update_statuses_column(conn, &ids, "read", read);
            let _ = tx.send(res);
        }
        ArticlesDbOp::UpdateStatusesStarred(ids, starred, tx) => {
            let res = update_statuses_column(conn, &ids, "starred", starred);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchMissingArticleIds(tx) => {
            let res = fetch_missing_article_ids(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::FetchToday(sort, limit, tx) => {
            let res = fetch_today(conn, sort, limit);
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
        ArticlesDbOp::LastVacuumDate(tx) => {
            let res = last_vacuum_date(conn);
            let _ = tx.send(res);
        }
        ArticlesDbOp::GetDbInfo { key, reply } => {
            let res = db_info_get(conn, &key);
            let _ = reply.send(res);
        }
        ArticlesDbOp::SetDbInfo { key, value, reply } => {
            let res = db_info_set(conn, &key, &value);
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

/// Mark-as-read rows for `articles` that carry each article's existing
/// star over from `current` (a missing row means never starred). Needed
/// because `upsert_statuses` overwrites every column on conflict: a
/// mark-read built with a hardcoded `starred: false` silently unstars
/// the feed.
pub fn mark_read_statuses(
    articles: Vec<Article>,
    current: &HashMap<String, (bool, bool)>,
) -> Vec<ArticleStatus> {
    let now = Utc::now();
    articles
        .into_iter()
        .map(|a| {
            let starred = current
                .get(&a.article_id)
                .map(|(_, starred)| *starred)
                .unwrap_or(false);
            ArticleStatus {
                article_id: a.article_id,
                read: true,
                starred,
                date_arrived: now,
            }
        })
        .collect()
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

fn fetch_by_feed(
    conn: &mut Connection,
    feed_id: &str,
    sort: SortOrder,
    limit: i64,
    include_read: bool,
) -> Result<Vec<Article>> {
    // The read filter is a correlated subquery on the statuses PK (one
    // index seek per row, single-table — the multi-index-OR lesson), not a
    // join, so the existing SELECT * shape and row mapper stay untouched.
    // Missing status rows read as unread (COALESCE 0), matching NNW.
    let read_filter = if include_read {
        ""
    } else {
        " AND COALESCE((SELECT read FROM statuses WHERE statuses.article_id = articles.article_id), 0) = 0 "
    };
    let sql = format!(
        "SELECT * FROM articles WHERE feed_id = ?{read_filter}{}{}",
        sort.order_by_clause(),
        limit_clause(limit)
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
    limit: i64,
    include_read: bool,
) -> Result<Vec<Article>> {
    if feed_ids.is_empty() {
        return Ok(Vec::new());
    }
    let read_filter = if include_read {
        ""
    } else {
        " AND COALESCE((SELECT read FROM statuses WHERE statuses.article_id = articles.article_id), 0) = 0 "
    };
    let mut articles: Vec<Article> = Vec::new();
    for chunk in feed_ids.chunks(500) {
        let placeholders: String = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT * FROM articles WHERE feed_id IN ({placeholders}){read_filter}{}{}",
            sort.order_by_clause(),
            limit_clause(limit)
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            chunk.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_article)?;
        for row in rows {
            articles.push(row?);
        }
    }
    // Per-chunk results are each individually sorted (and each capped at
    // `limit`, so a 50-feed folder yields at most 50 × limit rows before
    // the merge); chunks need a final merge-sort pass so the aggregate
    // view honours the global sort order, then one truncate to the cap.
    // Comparator branches on `sort` since `Reverse` only flips for
    // newest-first. Keyed on the same logical date as the SQL ORDER BY
    // (v2.8.1): `date_published`, falling back to `date_modified`.
    // v4.1.0: title sorts merge on the Rust twin of the SQL title key;
    // the stable sort keeps SQL chunk order for exact ties, which is
    // already title-then-date-then-rowid within each chunk.
    // `sort_by_cached_key` because the title key allocates (trim +
    // lowercase + String); computing it once per element keeps the
    // merge O(n log n) comparisons over precomputed keys.
    match sort {
        SortOrder::NewestFirst => {
            articles.sort_by_key(|a| std::cmp::Reverse(a.date_published.or(a.date_modified)))
        }
        SortOrder::OldestFirst => articles.sort_by_key(|a| a.date_published.or(a.date_modified)),
        SortOrder::TitleAscending => articles.sort_by_cached_key(|a| {
            (
                title_sort_key(a),
                std::cmp::Reverse(a.date_published.or(a.date_modified)),
            )
        }),
        SortOrder::TitleDescending => articles.sort_by_cached_key(|a| {
            (
                std::cmp::Reverse(title_sort_key(a)),
                std::cmp::Reverse(a.date_published.or(a.date_modified)),
            )
        }),
    }
    if limit > 0 {
        articles.truncate(limit as usize);
    }
    Ok(articles)
}

/// v4.1.0: Rust twin of the SQL title sort key (`title_sort_key!`),
/// used by the `fetch_by_feeds` merge where the comparison happens in
/// memory. Same source precedence — trimmed `title`, `content_text`
/// excerpt capped at 300 chars, `summary`, then the empty string — with
/// `to_lowercase()` standing in for `COLLATE NOCASE` (Unicode-full
/// here, ASCII-only in SQLite; the merge re-sorts globally, so the
/// fold width only ever decides which of two case-variant equal keys
/// stays in SQL chunk order). The trim set is the same six ASCII
/// whitespace characters the SQL `TRIM(x, …)` charset names: keeping
/// the two identical is what makes a feed view and a folder view order
/// the same article identically.
fn title_sort_key(a: &Article) -> String {
    fn trim6(s: &str) -> &str {
        s.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r'))
    }
    fn nonempty(s: &str) -> Option<&str> {
        let trimmed = trim6(s);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
    fn excerpt(s: &str) -> String {
        let trimmed = trim6(s);
        match trimmed.char_indices().nth(300) {
            Some((idx, _)) => trimmed[..idx].to_string(),
            None => trimmed.to_string(),
        }
    }
    let key = a
        .title
        .as_deref()
        .and_then(nonempty)
        .map(str::to_string)
        .or_else(|| a.content_text.as_deref().and_then(nonempty).map(excerpt))
        .or_else(|| a.summary.as_deref().and_then(nonempty).map(str::to_string))
        .unwrap_or_default();
    key.to_lowercase()
}

fn fetch_by_article_id(conn: &mut Connection, article_id: &str) -> Result<Option<Article>> {
    let mut stmt = conn.prepare("SELECT * FROM articles WHERE article_id = ?")?;
    let article = stmt.query_row([article_id], row_to_article).optional()?;
    Ok(article)
}

fn fetch_unread(conn: &mut Connection, sort: SortOrder, limit: i64) -> Result<Vec<Article>> {
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE s.read = 0 {}{}",
        sort.order_by_clause_aliased(),
        limit_clause(limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

fn fetch_starred(conn: &mut Connection, sort: SortOrder, limit: i64) -> Result<Vec<Article>> {
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE s.starred = 1 {}{}",
        sort.order_by_clause_aliased(),
        limit_clause(limit)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_article)?;
    let mut articles = Vec::new();
    for row in rows {
        articles.push(row?);
    }
    Ok(articles)
}

/// Collect every `article_id` whose status row matches `where_clause`.
/// `where_clause` is a fixed internal literal (`"read = 0"` / `"starred = 1"`),
/// never caller/feed input, so there is no injection surface.
fn fetch_status_ids(conn: &mut Connection, where_clause: &str) -> Result<HashSet<String>> {
    let sql = format!("SELECT article_id FROM statuses WHERE {}", where_clause);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    let mut ids = HashSet::new();
    for row in rows {
        ids.insert(row?);
    }
    Ok(ids)
}

/// Set a boolean status `column` (`"read"` / `"starred"` — fixed internal
/// literals, no injection surface) on every id, chunked under SQLite's
/// parameter limit.
fn update_statuses_column(
    conn: &mut Connection,
    ids: &[String],
    column: &str,
    value: bool,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    const CHUNK: usize = 500;
    for chunk in ids.chunks(CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "UPDATE statuses SET {} = ? WHERE article_id IN ({})",
            column, placeholders
        );
        let mut params = vec![rusqlite::types::Value::from(if value {
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

/// The "Today" window filter shared by `fetch_today` and
/// `smart_feed_counts`. An article is "today" when EITHER its arrival OR
/// its publication timestamp falls on/after the local-midnight cutoff —
/// a wider union than NNW's `datePublished > ? or (datePublished is
/// null and dateArrived > ?)` fallback shape; that divergence predates
/// this port and the rewrite preserves it exactly.
///
/// NNW `19930aa3c` duplicated the `feedID in (...)` test into both
/// branches of the date OR so SQLite's multi-index OR seeks
/// `articles_feedID_datePublished_articleID` per branch instead of
/// scanning every article in the feeds. Our OR spans two tables, so a
/// plain textual duplication would still cross tables and scan; the
/// branch constraint rides the join key instead, as an `IN (subquery)`:
/// the arrival branch seeks `statuses_date_arrived_idx` inside the
/// subquery, the publication branch seeks `articles_date_published_idx`
/// directly (EXPLAIN QUERY PLAN: MULTI-INDEX OR with a SEARCH per
/// branch). `statuses.article_id` is the PK and the join is INNER, so
/// `a.article_id IN (today's arrivals)` is exactly `s.date_arrived >=
/// ?` on the joined row — same rows, seeks instead of scans.
fn today_window_where() -> &'static str {
    "(a.article_id IN (SELECT article_id FROM statuses WHERE date_arrived >= ?) \
     OR a.date_published >= ?)"
}

fn fetch_today(conn: &mut Connection, sort: SortOrder, limit: i64) -> Result<Vec<Article>> {
    let today_start = local_midnight_utc_seconds();
    let sql = format!(
        "SELECT a.* FROM articles a \
         INNER JOIN statuses s ON a.article_id = s.article_id \
         WHERE {} {}{}",
        today_window_where(),
        sort.order_by_clause_aliased(),
        limit_clause(limit)
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
        &format!(
            "SELECT COUNT(*)
             FROM articles a
             INNER JOIN statuses s ON a.article_id = s.article_id
             WHERE s.read = 0 AND {}",
            today_window_where()
        ),
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
    stamp_vacuum_date(conn)?;
    Ok(())
}

/// Record now (unix seconds) as `db_info.last_vacuum_date`. Read back by
/// `last_vacuum_date` to gate the 13-day VACUUM throttle in
/// `cleanup_at_startup`.
fn stamp_vacuum_date(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO db_info (key, value) VALUES ('last_vacuum_date', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![Utc::now().timestamp().to_string()],
    )?;
    Ok(())
}

/// Read `db_info.last_vacuum_date` as unix seconds. `None` when the row is
/// absent (DB created before this table existed, or never vacuumed) or the
/// stored value isn't a valid integer.
fn last_vacuum_date(conn: &Connection) -> Result<Option<i64>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM db_info WHERE key = 'last_vacuum_date'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|v| v.parse::<i64>().ok()))
}

/// Read a generic `db_info` value. `None` when the row is absent.
fn db_info_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM db_info WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

/// Upsert a generic `db_info` value.
fn db_info_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO db_info (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
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

    /// The generic `db_info` accessor the Inoreader sync conditional-GET
    /// markers ride on: set-then-get round-trips, a missing key reads as
    /// `None`, and an upsert overwrites.
    #[test]
    fn db_info_get_set_round_trip() {
        let conn = in_memory();
        assert_eq!(db_info_get(&conn, "sync-cget-tags-etag").unwrap(), None);
        db_info_set(&conn, "sync-cget-tags-etag", "\"abc\"").unwrap();
        assert_eq!(
            db_info_get(&conn, "sync-cget-tags-etag").unwrap(),
            Some("\"abc\"".to_string())
        );
        db_info_set(&conn, "sync-cget-tags-etag", "\"def\"").unwrap();
        assert_eq!(
            db_info_get(&conn, "sync-cget-tags-etag").unwrap(),
            Some("\"def\"".to_string())
        );
        // Distinct keys don't interfere (each list carries its own pair).
        assert_eq!(
            db_info_get(&conn, "sync-cget-tags-last-modified").unwrap(),
            None
        );
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

    /// Port of NNW `19930aa3c`'s `TodayQueriesTests` (v4.0.5): the
    /// per-branch index-seek rewrite of the Today window must select
    /// exactly the same rows as the old single-OR form. An article is
    /// "today" when EITHER its arrival OR its publication timestamp is
    /// on/after local midnight; `date_published` is NULL for some feeds,
    /// which must neither match the publication branch nor block the
    /// arrival branch; an article qualifying through BOTH branches
    /// counts once. 25 h ago is always before today's local midnight,
    /// and `Utc::now()` is always on/after it, so the window boundaries
    /// are deterministic regardless of when the test runs.
    #[test]
    fn today_queries_select_the_union_window_without_double_counting() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let now = Utc::now();
        let before_window = now - Duration::hours(25);

        // Four boundary articles (upstream's per-feed trio plus the
        // always-excluded one):
        // - "arrived-only": NULL date_published, arrived now.
        // - "published-only": published now, arrived 25 h ago.
        // - "both-windows": published now, arrived now.
        // - "neither-window": published 25 h ago, arrived 25 h ago.
        let mut arrived_only = item("arrived-only", "Arrived only", "body");
        arrived_only.date_published = None;
        let mut published_only = item("published-only", "Published only", "body");
        published_only.date_published = Some(now);
        let mut both_windows = item("both-windows", "Both windows", "body");
        both_windows.date_published = Some(now);
        let mut neither = item("neither-window", "Neither window", "body");
        neither.date_published = Some(before_window);

        update_feed(
            &mut conn,
            feed_id,
            vec![arrived_only, published_only, both_windows, neither],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .expect("update_feed");

        // Backdate the arrivals of "published-only" and "neither-window"
        // (update_feed stamps insert time, which is inside the window).
        for unique_id in ["published-only", "neither-window"] {
            conn.execute(
                "UPDATE statuses SET date_arrived = ? WHERE article_id = ?",
                params![
                    before_window.timestamp(),
                    article_id_for(feed_id, unique_id)
                ],
            )
            .expect("backdate arrival");
        }

        let titles = |articles: &[Article]| {
            let mut t: Vec<String> = articles
                .iter()
                .map(|a| a.title.clone().unwrap_or_default())
                .collect();
            t.sort();
            t
        };

        // The fetch returns exactly the three in-window articles (the
        // NULL-published one arrives via the arrival branch; "both-windows"
        // appears once despite matching both branches).
        let today = fetch_today(&mut conn, SortOrder::default(), 0).expect("fetch_today");
        assert_eq!(
            titles(&today),
            vec![
                "Arrived only".to_string(),
                "Both windows".to_string(),
                "Published only".to_string()
            ]
        );

        // All four are unread right after ingest, so the Today badge
        // counts the same three (no OR double-count in the COUNT form).
        let counts = smart_feed_counts(&mut conn).expect("smart_feed_counts");
        assert_eq!(counts.today_unread, 3);
        assert_eq!(counts.all_unread, 4);

        // Marking the publication-branch-only article read drops the
        // Today badge without touching the fetch window.
        conn.execute(
            "UPDATE statuses SET read = 1 WHERE article_id = ?",
            params![article_id_for(feed_id, "published-only")],
        )
        .expect("mark read");
        let counts = smart_feed_counts(&mut conn).expect("smart_feed_counts re-run");
        assert_eq!(counts.today_unread, 2);
        assert_eq!(counts.all_unread, 3);

        // LIMIT applies after the window filter (upstream's
        // todayArticlesWithLimit): fewer rows, all still in-window.
        let limited = fetch_today(&mut conn, SortOrder::default(), 2).expect("fetch_today limit");
        assert_eq!(limited.len(), 2);
        let window: std::collections::HashSet<&str> =
            ["Arrived only", "Both windows", "Published only"]
                .into_iter()
                .collect();
        assert!(
            limited
                .iter()
                .all(|a| window.contains(a.title.as_deref().unwrap_or_default()))
        );
    }

    /// v4.0.5: the Today-window indexes (`articles_date_published_idx`,
    /// `statuses_date_arrived_idx`) are created by the schema init and
    /// re-running the init on an existing DB is a no-op, not an error —
    /// the additive-migration contract every `CREATE INDEX IF NOT
    /// EXISTS` in `setup_schema` carries.
    #[test]
    fn schema_init_creates_today_window_indexes_idempotently() {
        let conn = Connection::open_in_memory().expect("open in-memory");
        setup_schema(&conn).expect("initial schema");
        let index_count = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN \
                 ('articles_date_published_idx', 'statuses_date_arrived_idx')",
                [],
                |r| r.get(0),
            )
            .expect("count indexes")
        };
        assert_eq!(index_count(&conn), 2, "both Today-window indexes exist");
        setup_schema(&conn).expect("re-run schema on existing DB");
        assert_eq!(
            index_count(&conn),
            2,
            "re-running the init must not duplicate or fail"
        );
    }

    /// v3.9.0 regression: "Mark Feed/Folder as Read" used to build its
    /// status rows with `starred: false` hardcoded, and since
    /// `upsert_statuses` overwrites the column on conflict, marking a
    /// feed or folder read silently unstarred every starred article in
    /// it. `mark_read_statuses` must carry the existing star through,
    /// and articles with no status row yet star as false.
    #[test]
    fn mark_read_statuses_preserves_existing_stars() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        update_feed(
            &mut conn,
            feed_id,
            vec![item("a", "First", "body1"), item("b", "Second", "body2")],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .expect("update_feed");

        // The user starred article "b" sometime in the past; "a" was
        // never touched, so it has no status row.
        let articles =
            fetch_by_feed(&mut conn, feed_id, SortOrder::default(), 0, true).expect("fetch");
        let starred_id = articles
            .iter()
            .find(|a| a.title.as_deref() == Some("Second"))
            .map(|a| a.article_id.clone())
            .expect("article b");
        let ids: Vec<String> = articles.iter().map(|a| a.article_id.clone()).collect();
        upsert_statuses(
            &mut conn,
            vec![ArticleStatus {
                article_id: starred_id.clone(),
                read: false,
                starred: true,
                date_arrived: Utc::now(),
            }],
        )
        .expect("star article b");

        // The mark-read path: gather current statuses, build rows, write.
        let current = fetch_statuses_by_ids(&mut conn, &ids).expect("statuses");
        let articles =
            fetch_by_feed(&mut conn, feed_id, SortOrder::default(), 0, true).expect("fetch");
        let rows = mark_read_statuses(articles, &current);
        assert_eq!(rows.len(), 2);
        upsert_statuses(&mut conn, rows).expect("upsert mark-read");

        for id in &ids {
            let (read, starred) = conn
                .query_row(
                    "SELECT read, starred FROM statuses WHERE article_id = ?",
                    params![id],
                    |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0)),
                )
                .expect("status row after mark-read");
            assert!(read, "article must end read");
            let expected_star = *id == starred_id;
            assert_eq!(starred, expected_star, "star state must survive mark-read");
        }
    }

    /// The timeline row cap: `limit > 0` returns the newest `limit`
    /// articles, `limit == 0` (the mark-read callers) returns every
    /// row. The folder aggregate caps after the merge, so the global
    /// newest-N wins, not per-feed newest-N.
    #[test]
    fn timeline_fetches_honour_the_row_cap() {
        let mut conn = in_memory();
        let feed_a = "https://example.com/a";
        let feed_b = "https://example.com/b";
        // Distinct publish dates so newest-first order is unambiguous.
        let mins_ago = |m: i64| Some(Utc::now() - chrono::Duration::minutes(m));
        let mk = |id: &str, title: &str, m: i64| {
            let mut it = item(id, title, "body");
            it.date_published = mins_ago(m);
            it
        };
        update_feed(
            &mut conn,
            feed_a,
            vec![mk("a1", "A1", 30), mk("a2", "A2", 20), mk("a3", "A3", 10)],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        update_feed(
            &mut conn,
            feed_b,
            vec![mk("b1", "B1", 5), mk("b2", "B2", 2)],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // Single feed: cap to the 2 newest; 0 means everything.
        let all = fetch_by_feed(&mut conn, feed_a, SortOrder::NewestFirst, 0, true).unwrap();
        assert_eq!(all.len(), 3);
        let capped = fetch_by_feed(&mut conn, feed_a, SortOrder::NewestFirst, 2, true).unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].title.as_deref(), Some("A3"));

        // Folder aggregate: 5 rows across two feeds, cap 3 applies to
        // the merged result.
        let ids = vec![feed_a.to_string(), feed_b.to_string()];
        let merged = fetch_by_feeds(&mut conn, &ids, SortOrder::NewestFirst, 0, true).unwrap();
        assert_eq!(merged.len(), 5);
        let capped_merge =
            fetch_by_feeds(&mut conn, &ids, SortOrder::NewestFirst, 3, true).unwrap();
        assert_eq!(capped_merge.len(), 3);
        assert_eq!(capped_merge[0].title.as_deref(), Some("B2"));

        // The smart-feed queries take the same cap. `update_feed` gives
        // every new article an unread status row, so all 5 are unread.
        assert_eq!(
            fetch_unread(&mut conn, SortOrder::NewestFirst, 0)
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            fetch_unread(&mut conn, SortOrder::NewestFirst, 1)
                .unwrap()
                .len(),
            1
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
    fn vacuum_stamps_last_vacuum_date() {
        let mut conn = in_memory();
        assert_eq!(last_vacuum_date(&conn).unwrap(), None);
        vacuum(&mut conn).unwrap();
        let stamped = last_vacuum_date(&conn).unwrap();
        assert!(stamped.is_some(), "vacuum should stamp last_vacuum_date");
        assert!(stamped.unwrap() > 0);
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

        let titles: Vec<String> =
            fetch_by_feed(&mut conn, feed_id, SortOrder::NewestFirst, 0, true)
                .expect("fetch")
                .into_iter()
                .filter_map(|a| a.title)
                .collect();
        // Logical-date order: 1h, 3h (via date_modified), 5h. Before the
        // coalesce, "modified-only" had a NULL key and sorted last.
        assert_eq!(titles, vec!["recent", "modified-only", "oldest"]);
    }

    // ---- v4.1.0: title sort (NNW 70c3ec809) ----

    /// Seed helper for the title-sort tests: full control over the four
    /// fields the sort key reads (title / content_text / summary / the
    /// logical date). `content_text` stays exactly what is given —
    /// unlike `item()`, nothing derives it from `content_html`.
    fn sort_item(
        id: &str,
        title: Option<&str>,
        content_text: Option<&str>,
        summary: Option<&str>,
        published: Option<chrono::DateTime<Utc>>,
    ) -> ParsedItem {
        ParsedItem {
            id: id.to_string(),
            title: title.map(str::to_string),
            content_html: None,
            content_text: content_text.map(str::to_string),
            url: None,
            external_url: None,
            summary: summary.map(str::to_string),
            image_url: None,
            date_published: published,
            date_modified: None,
            authors: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn title_sort_orders_displayed_titles_case_insensitively() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "z",
                    Some("  Zebra  "),
                    None,
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "a",
                    Some("apple"),
                    None,
                    None,
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "b",
                    Some("Banana"),
                    None,
                    None,
                    Some(base - Duration::hours(3)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // `COLLATE NOCASE` folds case for the comparison; the stored
        // title keeps its original spacing (TRIM shapes the key only).
        let titles_asc: Vec<String> =
            fetch_by_feed(&mut conn, feed_id, SortOrder::TitleAscending, 0, true)
                .unwrap()
                .into_iter()
                .filter_map(|a| a.title)
                .collect();
        assert_eq!(titles_asc, vec!["apple", "Banana", "  Zebra  "]);

        let titles_desc: Vec<String> =
            fetch_by_feed(&mut conn, feed_id, SortOrder::TitleDescending, 0, true)
                .unwrap()
                .into_iter()
                .filter_map(|a| a.title)
                .collect();
        assert_eq!(titles_desc, vec!["  Zebra  ", "Banana", "apple"]);
    }

    #[test]
    fn title_sort_falls_back_to_excerpt_then_summary() {
        // NNW 70c3ec809: an untitled article sorts by the start of its
        // body; a bodyless one by its summary; one with nothing readable
        // sorts as NULL (first ascending, last descending, like the
        // empty string does upstream).
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "n1",
                    None,
                    Some("walnut body"),
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "n2",
                    Some(""),
                    None,
                    Some("mango summary"),
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "n3",
                    None,
                    Some("   "),
                    Some("kiwi summary"),
                    Some(base - Duration::hours(3)),
                ),
                sort_item("n4", None, None, None, Some(base - Duration::hours(4))),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let mut labels = |sort: SortOrder| -> Vec<String> {
            fetch_by_feed(&mut conn, feed_id, sort, 0, true)
                .unwrap()
                .into_iter()
                .map(|a| {
                    a.summary
                        .or(a.content_text)
                        .unwrap_or_else(|| "none".to_string())
                })
                .collect()
        };
        assert_eq!(
            labels(SortOrder::TitleAscending),
            vec![
                "none".to_string(),
                "kiwi summary".to_string(),
                "mango summary".to_string(),
                "walnut body".to_string()
            ]
        );
        assert_eq!(
            labels(SortOrder::TitleDescending),
            vec!["walnut body", "mango summary", "kiwi summary", "none"]
        );
    }

    #[test]
    fn title_sort_excerpt_caps_at_300_characters_then_ties_by_date() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let base = Utc::now();
        // long1 and long2 share their first 300 characters, so the SQL
        // excerpt key cannot tell them apart: the tie falls to the
        // logical date, newest first. `other` differs inside the window
        // and sorts by its excerpt.
        let long1 = "x".repeat(320);
        let long2 = format!("{}{}", "x".repeat(300), "y".repeat(20));
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "long1",
                    None,
                    Some(&long1),
                    None,
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "long2",
                    None,
                    Some(&long2),
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "other",
                    None,
                    Some(&"a".repeat(50)),
                    None,
                    Some(base - Duration::hours(3)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let bodies: Vec<String> =
            fetch_by_feed(&mut conn, feed_id, SortOrder::TitleAscending, 0, true)
                .unwrap()
                .into_iter()
                .filter_map(|a| a.content_text)
                .collect();
        assert_eq!(bodies, vec!["a".repeat(50), long2, long1]);
    }

    #[test]
    fn title_sort_ties_break_newest_first_in_both_directions() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "old-same",
                    Some("Same"),
                    None,
                    None,
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "new-same",
                    Some("Same"),
                    None,
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "alpha",
                    Some("Alpha"),
                    None,
                    None,
                    Some(base - Duration::hours(3)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let mut order = |sort: SortOrder| -> Vec<i64> {
            fetch_by_feed(&mut conn, feed_id, sort, 0, true)
                .unwrap()
                .into_iter()
                .map(|a| a.date_published.unwrap().timestamp())
                .collect()
        };
        // Upstream hardcodes `.orderedDescending` for the title tiebreak,
        // so equal titles sit newest-first under A to Z and Z to A alike.
        assert_eq!(
            order(SortOrder::TitleAscending)[1],
            (base - Duration::hours(1)).timestamp()
        );
        assert_eq!(
            order(SortOrder::TitleAscending)[2],
            (base - Duration::hours(2)).timestamp()
        );
        assert_eq!(
            order(SortOrder::TitleDescending)[0],
            (base - Duration::hours(1)).timestamp()
        );
        assert_eq!(
            order(SortOrder::TitleDescending)[1],
            (base - Duration::hours(2)).timestamp()
        );
    }

    #[test]
    fn title_sort_runs_on_the_joined_queries() {
        // The unread / starred / today queries order through the `a.`
        // aliased clause across the articles ↔ statuses join; this pins
        // that the title key renders and executes there (an ambiguous or
        // misspelled column would fail the prepare, not silently sort).
        let mut conn = in_memory();
        let feed_id = "https://example.com/feed";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "c",
                    Some("Cherry"),
                    None,
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "a",
                    Some("Apple"),
                    None,
                    None,
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "b",
                    Some("banana"),
                    None,
                    None,
                    Some(base - Duration::hours(3)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let unread: Vec<String> = fetch_unread(&mut conn, SortOrder::TitleAscending, 0)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.title)
            .collect();
        assert_eq!(unread, vec!["Apple", "banana", "Cherry"]);

        conn.execute(
            "UPDATE statuses SET read = 1 WHERE article_id = ?",
            params![article_id_for(feed_id, "a")],
        )
        .unwrap();
        let unread_after: Vec<String> = fetch_unread(&mut conn, SortOrder::TitleAscending, 0)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.title)
            .collect();
        assert_eq!(unread_after, vec!["banana", "Cherry"]);

        conn.execute(
            "UPDATE statuses SET starred = 1 WHERE article_id = ?",
            params![article_id_for(feed_id, "c")],
        )
        .unwrap();
        let starred: Vec<String> = fetch_starred(&mut conn, SortOrder::TitleDescending, 0)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.title)
            .collect();
        assert_eq!(starred, vec!["Cherry"]);

        // Today shares the OR-window WHERE with the aliased title clause,
        // and like the Today smart feed generally it is not read-filtered,
        // so the read article still appears — ordered by title with the
        // rest.
        let today: Vec<String> = fetch_today(&mut conn, SortOrder::TitleAscending, 0)
            .unwrap()
            .into_iter()
            .filter_map(|a| a.title)
            .collect();
        assert_eq!(today, vec!["Apple", "banana", "Cherry"]);
    }

    #[test]
    fn fetch_by_feeds_merges_title_order_across_feeds() {
        let mut conn = in_memory();
        let feed_a = "https://example.com/a";
        let feed_b = "https://example.com/b";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_a,
            vec![
                sort_item(
                    "f",
                    Some("Fig"),
                    None,
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "c",
                    Some("Cherry"),
                    None,
                    None,
                    Some(base - Duration::hours(2)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        update_feed(
            &mut conn,
            feed_b,
            vec![
                sort_item(
                    "e",
                    Some("Elderberry"),
                    None,
                    None,
                    Some(base - Duration::hours(3)),
                ),
                sort_item(
                    "d",
                    Some("Date"),
                    None,
                    None,
                    Some(base - Duration::hours(4)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        let mut titles = |sort: SortOrder, limit: i64| -> Vec<String> {
            fetch_by_feeds(
                &mut conn,
                &[feed_a.to_string(), feed_b.to_string()],
                sort,
                limit,
                true,
            )
            .unwrap()
            .into_iter()
            .filter_map(|a| a.title)
            .collect()
        };
        // The merge comparator re-sorts the concatenated chunks globally,
        // so the interleaved per-feed rows come out in one title order.
        assert_eq!(
            titles(SortOrder::TitleAscending, 0),
            vec!["Cherry", "Date", "Elderberry", "Fig"]
        );
        assert_eq!(
            titles(SortOrder::TitleDescending, 0),
            vec!["Fig", "Elderberry", "Date", "Cherry"]
        );
        // The cap applies in sort order, as it always has for the date
        // sorts: the first/last N titles, not a date-scoped subset.
        assert_eq!(
            titles(SortOrder::TitleAscending, 3),
            vec!["Cherry", "Date", "Elderberry"]
        );
    }

    #[test]
    fn fetch_by_feed_honors_read_filter() {
        let mut conn = in_memory();
        let feed_id = "https://example.com/rss";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_id,
            vec![
                sort_item(
                    "unread-1",
                    Some("Still Unread"),
                    None,
                    None,
                    Some(base - Duration::hours(1)),
                ),
                sort_item(
                    "read-1",
                    Some("Already Read"),
                    None,
                    None,
                    Some(base - Duration::hours(2)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        // Mark the second article read. The id is the NNW-calculated
        // MD5 of "{feed_id} {unique_id}".
        let read_id = article_id_for(feed_id, "read-1");
        upsert_statuses(
            &mut conn,
            vec![ArticleStatus {
                article_id: read_id,
                read: true,
                starred: false,
                date_arrived: base,
            }],
        )
        .unwrap();

        let mut titles = |include_read: bool| -> Vec<String> {
            fetch_by_feed(&mut conn, feed_id, SortOrder::NewestFirst, 0, include_read)
                .unwrap()
                .into_iter()
                .filter_map(|a| a.title)
                .collect()
        };
        // Show-read (the timeline default): both, the read one dimmed
        // upstream. The article with no status row at all counts as
        // unread and survives the filter in both modes.
        assert_eq!(titles(true), vec!["Still Unread", "Already Read"]);
        assert_eq!(titles(false), vec!["Still Unread"]);
    }
    #[test]
    fn title_sort_key_rust_twin_matches_sql_precedence() {
        let mk = |title: Option<&str>, content_text: Option<&str>, summary: Option<&str>| Article {
            article_id: "id".to_string(),
            feed_id: "feed".to_string(),
            title: title.map(str::to_string),
            content_html: None,
            content_text: content_text.map(str::to_string),
            url: None,
            external_url: None,
            summary: summary.map(str::to_string),
            image_url: None,
            date_published: None,
            date_modified: None,
            authors: Vec::new(),
            attachments: Vec::new(),
        };

        // Title wins, trimmed and folded.
        assert_eq!(
            title_sort_key(&mk(Some("  Mixed Case "), None, None)),
            "mixed case"
        );
        // Untitled falls to the content_text excerpt, trimmed and capped.
        let body = format!("{}tail", "b".repeat(320));
        assert_eq!(
            title_sort_key(&mk(None, Some(&body), None)),
            "b".repeat(300)
        );
        // Whitespace-only content falls through to the summary.
        assert_eq!(
            title_sort_key(&mk(None, Some("   "), Some("Summary"))),
            "summary"
        );
        // The trim set is the six ASCII whitespace characters, matching
        // the SQL TRIM charset: leading newlines trim like spaces do.
        assert_eq!(
            title_sort_key(&mk(None, Some("\n\n spaced \r"), None)),
            "spaced"
        );
        // Nothing readable: the empty string, like the SQL NULL/'' key.
        assert_eq!(title_sort_key(&mk(None, None, None)), "");
        // The cap cuts on a char boundary, not mid-codepoint.
        let multibyte = "é".repeat(400);
        assert_eq!(
            title_sort_key(&mk(None, Some(&multibyte), None)),
            "é".repeat(300)
        );
    }

    #[test]
    fn title_sort_orders_identically_in_feed_and_folder_views() {
        // The SQL key (single-feed view) and the Rust twin (the
        // fetch_by_feeds merge) must trim the same whitespace set, or a
        // body with leading newlines orders differently depending on
        // which view shows it: pre-alignment, the one-argument SQL TRIM
        // stripped only spaces, so the raw key "\n\n alpha body" (0x0A
        // sorts before every letter) landed first in the feed view while
        // the merged folder view interleaved it alphabetically.
        let mut conn = in_memory();
        let feed_a = "https://example.com/a";
        let feed_b = "https://example.com/b";
        let base = Utc::now();
        update_feed(
            &mut conn,
            feed_a,
            vec![
                sort_item(
                    "x",
                    None,
                    Some("\n\n alpha body"),
                    None,
                    Some(base - Duration::hours(2)),
                ),
                sort_item(
                    "a2",
                    None,
                    Some("aardvark"),
                    None,
                    Some(base - Duration::hours(3)),
                ),
            ],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();
        update_feed(
            &mut conn,
            feed_b,
            vec![sort_item(
                "y",
                None,
                Some("alpha body"),
                None,
                Some(base - Duration::hours(1)),
            )],
            false,
            DEFAULT_RETENTION_DAYS,
        )
        .unwrap();

        // Single-feed view: the newline-prefixed body trims to
        // "alpha body", so it sorts after "aardvark" rather than first.
        let feed_view: Vec<Option<String>> =
            fetch_by_feed(&mut conn, feed_a, SortOrder::TitleAscending, 0, true)
                .unwrap()
                .into_iter()
                .map(|a| a.content_text)
                .collect();
        assert_eq!(
            feed_view,
            vec![
                Some("aardvark".to_string()),
                Some("\n\n alpha body".to_string())
            ]
        );

        // Folder view: the same pair lands in the same relative order.
        // Both x and y key as "alpha body", so the tie breaks
        // newest-first: y (1h) before x (2h), after "aardvark".
        let folder_view: Vec<Option<String>> = fetch_by_feeds(
            &mut conn,
            &[feed_a.to_string(), feed_b.to_string()],
            SortOrder::TitleAscending,
            0,
            true,
        )
        .unwrap()
        .into_iter()
        .map(|a| a.content_text)
        .collect();
        assert_eq!(
            folder_view,
            vec![
                Some("aardvark".to_string()),
                Some("alpha body".to_string()),
                Some("\n\n alpha body".to_string())
            ]
        );
    }
}

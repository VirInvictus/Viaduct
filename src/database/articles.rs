// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;

use crate::error::{DatabaseError, Result};
use crate::models::{Article, ArticleStatus, Author};

pub enum ArticlesDbOp {
    BatchInsert(Vec<Article>, oneshot::Sender<Result<()>>),
    UpsertStatuses(Vec<ArticleStatus>, oneshot::Sender<Result<()>>),
    FetchByFeed(String, oneshot::Sender<Result<Vec<Article>>>),
    FetchByArticleId(String, oneshot::Sender<Result<Option<Article>>>),
    FetchUnread(oneshot::Sender<Result<Vec<Article>>>),
    FetchStarred(oneshot::Sender<Result<Vec<Article>>>),
    FetchToday(oneshot::Sender<Result<Vec<Article>>>),
    Search(String, oneshot::Sender<Result<Vec<Article>>>),
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
            authors JSON
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
        ",
    )?;
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
    }
}

fn batch_insert(conn: &mut Connection, articles: Vec<Article>) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut article_stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO articles (
                article_id, feed_id, title, content_html, content_text,
                url, external_url, summary, image_url, date_published, date_modified, authors
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

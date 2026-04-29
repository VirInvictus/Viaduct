// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;

use crate::error::Result;
use crate::models::FeedSettings;

pub enum SettingsDbOp {
    Fetch(String, oneshot::Sender<Result<Option<FeedSettings>>>),
    Upsert(Box<FeedSettings>, oneshot::Sender<Result<()>>),
    DeleteSettingsForFeedsNotIn(Vec<String>, oneshot::Sender<Result<usize>>),
    /// Run `VACUUM`. NNW vacuums the FeedSettingsDatabase on every init
    /// (`FeedSettingsDatabase.swift:67`); we do the same once per startup.
    Vacuum(oneshot::Sender<Result<()>>),
}

pub(crate) fn setup_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;

        CREATE TABLE IF NOT EXISTS feed_settings (
            feed_id TEXT PRIMARY KEY,
            feed_url TEXT NOT NULL,
            home_page_url TEXT,
            icon_url TEXT,
            favicon_url TEXT,
            edited_name TEXT,
            content_hash TEXT,
            last_modified TEXT,
            etag TEXT,
            date_created INTEGER,
            max_age INTEGER,
            authors_json TEXT,
            folder_relationship_json TEXT,
            last_check_date INTEGER,
            reader_view_always_enabled INTEGER NOT NULL DEFAULT 0,
            new_article_notifications_enabled INTEGER NOT NULL DEFAULT 0
        );
        ",
    )?;
    // v2.4.0: idempotent ALTER for DBs created before
    // `new_article_notifications_enabled` existed. Same pattern as the
    // `attachments` JSON column add on `articles` from v0.6.x — try the
    // ALTER, ignore "duplicate column" errors so re-runs are no-ops.
    let _ = conn.execute_batch(
        "ALTER TABLE feed_settings
           ADD COLUMN new_article_notifications_enabled INTEGER NOT NULL DEFAULT 0;",
    );
    Ok(())
}

pub(crate) fn handle_op(conn: &mut Connection, op: SettingsDbOp) {
    match op {
        SettingsDbOp::Fetch(feed_id, tx) => {
            let res = fetch(conn, &feed_id);
            let _ = tx.send(res);
        }
        SettingsDbOp::Upsert(settings, tx) => {
            let res = upsert(conn, *settings);
            let _ = tx.send(res);
        }
        SettingsDbOp::DeleteSettingsForFeedsNotIn(feed_urls, tx) => {
            let res = delete_settings_for_feeds_not_in(conn, feed_urls);
            let _ = tx.send(res);
        }
        SettingsDbOp::Vacuum(tx) => {
            let res = vacuum(conn);
            let _ = tx.send(res);
        }
    }
}

fn vacuum(conn: &mut Connection) -> Result<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}

fn fetch(conn: &mut Connection, feed_id: &str) -> Result<Option<FeedSettings>> {
    let mut stmt = conn.prepare("SELECT * FROM feed_settings WHERE feed_id = ?")?;
    let settings = stmt
        .query_row([feed_id], |row| {
            Ok(FeedSettings {
                feed_id: row.get("feed_id")?,
                feed_url: row.get("feed_url")?,
                home_page_url: row.get("home_page_url")?,
                icon_url: row.get("icon_url")?,
                favicon_url: row.get("favicon_url")?,
                edited_name: row.get("edited_name")?,
                content_hash: row.get("content_hash")?,
                last_modified: row.get("last_modified")?,
                etag: row.get("etag")?,
                date_created: row
                    .get::<_, Option<i64>>("date_created")?
                    .and_then(|t| Utc.timestamp_opt(t, 0).single()),
                max_age: row.get("max_age")?,
                authors_json: row.get("authors_json")?,
                folder_relationship_json: row.get("folder_relationship_json")?,
                last_check_date: row
                    .get::<_, Option<i64>>("last_check_date")?
                    .and_then(|t| Utc.timestamp_opt(t, 0).single()),
                reader_view_always_enabled: row.get::<_, i64>("reader_view_always_enabled")? != 0,
                new_article_notifications_enabled: row
                    .get::<_, i64>("new_article_notifications_enabled")?
                    != 0,
            })
        })
        .optional()?;
    Ok(settings)
}

fn upsert(conn: &mut Connection, s: FeedSettings) -> Result<()> {
    conn.execute(
        "INSERT INTO feed_settings (
            feed_id, feed_url, home_page_url, icon_url, favicon_url,
            edited_name, content_hash, last_modified, etag,
            date_created, max_age, authors_json, folder_relationship_json,
            last_check_date, reader_view_always_enabled,
            new_article_notifications_enabled
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(feed_id) DO UPDATE SET
            feed_url=excluded.feed_url,
            home_page_url=excluded.home_page_url,
            icon_url=excluded.icon_url,
            favicon_url=excluded.favicon_url,
            edited_name=excluded.edited_name,
            content_hash=excluded.content_hash,
            last_modified=excluded.last_modified,
            etag=excluded.etag,
            date_created=excluded.date_created,
            max_age=excluded.max_age,
            authors_json=excluded.authors_json,
            folder_relationship_json=excluded.folder_relationship_json,
            last_check_date=excluded.last_check_date,
            reader_view_always_enabled=excluded.reader_view_always_enabled,
            new_article_notifications_enabled=excluded.new_article_notifications_enabled",
        params![
            s.feed_id,
            s.feed_url,
            s.home_page_url,
            s.icon_url,
            s.favicon_url,
            s.edited_name,
            s.content_hash,
            s.last_modified,
            s.etag,
            s.date_created.map(|d| d.timestamp()),
            s.max_age,
            s.authors_json,
            s.folder_relationship_json,
            s.last_check_date.map(|d| d.timestamp()),
            s.reader_view_always_enabled as i64,
            s.new_article_notifications_enabled as i64,
        ],
    )?;
    Ok(())
}

fn delete_settings_for_feeds_not_in(
    conn: &mut Connection,
    feed_urls: Vec<String>,
) -> Result<usize> {
    // Early return matches NNW's `guard !feedURLs.isEmpty else { return }`.
    // The previous implementation deleted *every* row when the input was empty,
    // which would wipe the user's settings DB during startup cleanup if the
    // OPML failed to load or wasn't there yet.
    if feed_urls.is_empty() {
        return Ok(0);
    }

    let placeholders = vec!["?"; feed_urls.len()].join(", ");
    let query = format!(
        "DELETE FROM feed_settings WHERE feed_url NOT IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&query)?;
    let count = stmt.execute(rusqlite::params_from_iter(feed_urls))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory");
        setup_schema(&conn).expect("schema");
        conn
    }

    #[test]
    fn empty_feed_list_does_not_wipe_settings() {
        // Regression: previously this nuked every row when the OPML happened
        // to be empty at startup.
        let mut conn = in_memory();
        conn.execute(
            "INSERT INTO feed_settings (feed_id, feed_url) VALUES (?, ?)",
            params!["fid", "https://example.com/feed"],
        )
        .unwrap();
        let removed = delete_settings_for_feeds_not_in(&mut conn, Vec::new()).unwrap();
        assert_eq!(removed, 0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM feed_settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn non_empty_feed_list_prunes_only_orphans() {
        let mut conn = in_memory();
        conn.execute(
            "INSERT INTO feed_settings (feed_id, feed_url) VALUES (?, ?)",
            params!["a", "https://a.example/feed"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feed_settings (feed_id, feed_url) VALUES (?, ?)",
            params!["b", "https://b.example/feed"],
        )
        .unwrap();
        let removed =
            delete_settings_for_feeds_not_in(&mut conn, vec!["https://a.example/feed".to_string()])
                .unwrap();
        assert_eq!(removed, 1);
    }
}

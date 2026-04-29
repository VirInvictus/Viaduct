// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The Database Worker module provides the single serialization point for all SQLite writes.
//!
//! As mandated by the architecture spec, this module avoids UI thread blocking by offloading
//! all synchronous `rusqlite` operations to a dedicated background thread. All communications
//! with the database layer occur via asynchronous `mpsc` channels.

use rusqlite::Connection;
use std::path::Path;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::paths::{articles_db_path, feed_settings_db_path, sync_db_path};

/// Operations that can be performed by the database worker.
pub enum DbOp {
    /// Operations related to the Articles database.
    Articles(crate::database::articles::ArticlesDbOp),
    /// Operations related to the Feed Settings database.
    Settings(Box<crate::database::settings::SettingsDbOp>),
}

pub fn spawn_sync_worker(mut rx: mpsc::Receiver<crate::database::sync::SyncDbOp>) -> Result<()> {
    let sync_path = sync_db_path()?;

    std::thread::spawn(move || {
        loop {
            let mut rx_ref = std::panic::AssertUnwindSafe(&mut rx);
            let sync_path_ref = std::panic::AssertUnwindSafe(&sync_path);

            let res = std::panic::catch_unwind(move || {
                let mut sync_conn = match Connection::open(*sync_path_ref) {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!("Failed to init sync db: {}", e);
                        return;
                    }
                };

                if let Err(e) = crate::database::sync::setup_schema(&sync_conn) {
                    tracing::error!("Failed to setup sync db schema: {}", e);
                    return;
                }

                while let Some(op) = rx_ref.blocking_recv() {
                    crate::database::sync::handle_op(&mut sync_conn, op);
                }
            });

            if let Err(err) = res {
                tracing::error!(?err, "Sync worker panicked; restarting loop...");
                std::thread::sleep(std::time::Duration::from_secs(1));
            } else {
                break;
            }
        }
    });

    Ok(())
}

/// Spawns a dedicated background thread to handle all database operations.
///
/// This worker manages both the `ArticlesDatabase` and `FeedSettingsDatabase` connections.
/// It listens for `DbOp` messages on the provided receiver and executes them sequentially,
/// ensuring thread-safe access to the underlying SQLite files in WAL mode.
pub fn spawn_db_worker(mut rx: mpsc::Receiver<DbOp>) -> Result<()> {
    let articles_path = articles_db_path()?;
    let settings_path = feed_settings_db_path()?;

    std::thread::spawn(move || {
        loop {
            let mut rx_ref = std::panic::AssertUnwindSafe(&mut rx);
            let articles_path_ref = std::panic::AssertUnwindSafe(&articles_path);
            let settings_path_ref = std::panic::AssertUnwindSafe(&settings_path);

            let res = std::panic::catch_unwind(move || {
                let mut articles_conn = match init_articles_db(&articles_path_ref) {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!("Failed to init articles db: {}", e);
                        return;
                    }
                };

                let mut settings_conn = match init_settings_db(&settings_path_ref) {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::error!("Failed to init settings db: {}", e);
                        return;
                    }
                };

                while let Some(op) = rx_ref.blocking_recv() {
                    let op_kind = match &op {
                        DbOp::Articles(a) => articles_op_label(a),
                        DbOp::Settings(_) => "settings",
                    };
                    let started = std::time::Instant::now();
                    match op {
                        DbOp::Articles(article_op) => {
                            crate::database::articles::handle_op(&mut articles_conn, article_op);
                        }
                        DbOp::Settings(settings_op) => {
                            crate::database::settings::handle_op(&mut settings_conn, *settings_op);
                        }
                    }
                    if crate::is_debug_mode() {
                        tracing::trace!(
                            op = op_kind,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "db: op handled"
                        );
                    }
                }
            });

            if let Err(err) = res {
                tracing::error!(?err, "Database worker panicked; restarting loop...");
                std::thread::sleep(std::time::Duration::from_secs(1));
            } else {
                break;
            }
        }
    });

    Ok(())
}

/// Compact label for tracing the article-op variant. Matches the variant
/// names so log filtering with `op="UpdateFeed"` works cleanly.
fn articles_op_label(op: &crate::database::articles::ArticlesDbOp) -> &'static str {
    use crate::database::articles::ArticlesDbOp::*;
    match op {
        BatchInsert(..) => "BatchInsert",
        UpsertStatuses(..) => "UpsertStatuses",
        FetchByFeed(..) => "FetchByFeed",
        FetchByArticleId(..) => "FetchByArticleId",
        FetchUnread(..) => "FetchUnread",
        FetchStarred(..) => "FetchStarred",
        FetchUnreadArticleIds(..) => "FetchUnreadArticleIds",
        FetchStarredArticleIds(..) => "FetchStarredArticleIds",
        UpdateStatusesRead(..) => "UpdateStatusesRead",
        UpdateStatusesStarred(..) => "UpdateStatusesStarred",
        FetchMissingArticleIds(..) => "FetchMissingArticleIds",
        FetchToday(..) => "FetchToday",
        Search(..) => "Search",
        SearchWithSnippets(..) => "SearchWithSnippets",
        FetchStatusesByIds(..) => "FetchStatusesByIds",
        UnreadCountsByFeed(..) => "UnreadCountsByFeed",
        SmartFeedCounts(..) => "SmartFeedCounts",
        UpdateFeed { .. } => "UpdateFeed",
        DeleteArticlesNotInFeeds(..) => "DeleteArticlesNotInFeeds",
        DeleteOldStatuses { .. } => "DeleteOldStatuses",
        Vacuum(..) => "Vacuum",
    }
}

fn init_articles_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    crate::database::articles::setup_schema(&conn)?;
    Ok(conn)
}

fn init_settings_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    crate::database::settings::setup_schema(&conn)?;
    Ok(conn)
}

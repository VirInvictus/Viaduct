// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! The Database Worker module provides the single serialization point for all SQLite writes.
//!
//! As mandated by the architecture spec, this module avoids UI thread blocking by offloading
//! all synchronous `rusqlite` operations to a dedicated background thread. All communications
//! with the database layer occur via asynchronous `mpsc` channels.

use crossbeam_channel as cbc;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use tokio::sync::mpsc;

use crate::database::articles::ArticlesDbOp;
use crate::error::Result;
use crate::paths::{articles_db_path, feed_settings_db_path, sync_db_path};

/// Number of read-only connections in the pool (v2.8.0). A single-user GUI
/// rarely issues more than one or two concurrent reads — a foreground
/// timeline fetch plus a background search or unread-count refresh — so a
/// small pool fully decouples reads from the writer without idle threads.
const READ_POOL_SIZE: usize = 3;

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
        FetchByFeeds(..) => "FetchByFeeds",
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
        FetchSmartFeed(..) => "FetchSmartFeed",
        Search(..) => "Search",
        SearchWithSnippets(..) => "SearchWithSnippets",
        FetchStatusesByIds(..) => "FetchStatusesByIds",
        UnreadCountsByFeed(..) => "UnreadCountsByFeed",
        SmartFeedCounts(..) => "SmartFeedCounts",
        UpdateFeed { .. } => "UpdateFeed",
        DeleteArticlesNotInFeeds(..) => "DeleteArticlesNotInFeeds",
        DeleteOldStatuses { .. } => "DeleteOldStatuses",
        DeleteOrphanedAuthors(..) => "DeleteOrphanedAuthors",
        Vacuum(..) => "Vacuum",
        Checkpoint(..) => "Checkpoint",
    }
}

fn init_articles_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    crate::database::articles::setup_schema(&conn)?;
    Ok(conn)
}

/// Create the channel feeding the read-only connection pool (v2.8.0). The
/// `Sender` is handed to `Account`; the `Receiver` to `spawn_read_workers`.
/// Unbounded because read ops are tiny and the GTK side already serializes
/// its own issuance, so a backlog can't realistically accumulate.
pub fn read_channel() -> (cbc::Sender<ArticlesDbOp>, cbc::Receiver<ArticlesDbOp>) {
    cbc::unbounded()
}

/// Spawn the read-only connection pool. Each worker owns its own read-only
/// SQLite connection to the articles DB and pulls `ArticlesDbOp`s off the
/// shared crossbeam receiver. WAL allows any number of concurrent readers
/// alongside the single writer, so timeline fetches / searches / counts no
/// longer queue behind a long write on the writer connection.
///
/// MUST be called after the writer has initialized the articles DB (i.e.
/// after `Account::new` has returned), so the read-only open finds an
/// existing, WAL-configured file. The pool only ever receives read ops
/// (Account routes writes to the single writer); a stray write would simply
/// fail against the read-only connection and reply with an error.
pub fn spawn_read_workers(read_rx: cbc::Receiver<ArticlesDbOp>) -> Result<()> {
    let articles_path = articles_db_path()?;

    for _ in 0..READ_POOL_SIZE {
        let rx = read_rx.clone();
        let path = articles_path.clone();

        std::thread::spawn(move || {
            loop {
                let rx_ref = std::panic::AssertUnwindSafe(&rx);
                let path_ref = std::panic::AssertUnwindSafe(&path);

                let res = std::panic::catch_unwind(move || {
                    let mut conn = match open_read_only(*path_ref) {
                        Ok(conn) => conn,
                        Err(e) => {
                            tracing::error!("Read worker failed to open articles db: {}", e);
                            return;
                        }
                    };
                    // Exits cleanly when every `Sender` has dropped.
                    while let Ok(op) = rx_ref.recv() {
                        crate::database::articles::handle_op(&mut conn, op);
                    }
                });

                if let Err(err) = res {
                    tracing::error!(?err, "Read worker panicked; restarting loop...");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                } else {
                    break;
                }
            }
        });
    }

    Ok(())
}

/// Open a read-only connection to an already-initialized SQLite file.
/// `query_only` is implied by the read-only flag; `busy_timeout` makes a
/// reader wait out the brief exclusive window of a `wal_checkpoint(TRUNCATE)`
/// instead of failing with `SQLITE_BUSY`. No `mmap_size` is set, so each
/// pool connection keeps a minimal resident footprint (reads come from the
/// shared page cache).
fn open_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn init_settings_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    crate::database::settings::setup_schema(&conn)?;
    Ok(conn)
}

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
use crate::paths::{articles_db_path, feed_settings_db_path};

/// Operations that can be performed by the database worker.
pub enum DbOp {
    /// Operations related to the Articles database.
    Articles(crate::database::articles::ArticlesDbOp),
    /// Operations related to the Feed Settings database.
    Settings(Box<crate::database::settings::SettingsDbOp>),
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
        let mut articles_conn = match init_articles_db(&articles_path) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!("Failed to init articles db: {}", e);
                return;
            }
        };

        let mut settings_conn = match init_settings_db(&settings_path) {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!("Failed to init settings db: {}", e);
                return;
            }
        };

        while let Some(op) = rx.blocking_recv() {
            match op {
                DbOp::Articles(article_op) => {
                    crate::database::articles::handle_op(&mut articles_conn, article_op);
                }
                DbOp::Settings(settings_op) => {
                    crate::database::settings::handle_op(&mut settings_conn, *settings_op);
                }
            }
        }
    });

    Ok(())
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

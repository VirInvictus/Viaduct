// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct SyncStatus {
    pub article_id: String,
    pub key: String,
    pub flag: bool,
    pub selected: bool,
}

pub enum SyncDbOp {
    InsertStatuses(Vec<SyncStatus>, oneshot::Sender<Result<()>>),
    SelectForProcessing(Option<usize>, oneshot::Sender<Result<Vec<SyncStatus>>>),
    DeleteSelectedForProcessing(Vec<String>, oneshot::Sender<Result<()>>),
    ResetAllSelectedForProcessing(oneshot::Sender<Result<()>>),
    /// v2.6.5: wipe every row in `syncStatus`. The table is only
    /// touched by remote-sync delegates (Inoreader); when the local
    /// delegate is active any row here is leftover ghost from a
    /// previous remote session. Returns count for the cleanup-summary
    /// log line.
    WipeAll(oneshot::Sender<Result<usize>>),
}

pub fn setup_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS syncStatus (
            articleID TEXT NOT NULL, 
            key TEXT NOT NULL, 
            flag BOOL NOT NULL DEFAULT 0, 
            selected BOOL NOT NULL DEFAULT 0, 
            PRIMARY KEY (articleID, key)
        );",
    )?;
    Ok(())
}

pub fn handle_op(conn: &mut Connection, op: SyncDbOp) {
    match op {
        SyncDbOp::InsertStatuses(statuses, reply) => {
            let res = (|| -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare("INSERT OR REPLACE INTO syncStatus (articleID, key, flag, selected) VALUES (?, ?, ?, ?)")?;
                    for s in &statuses {
                        stmt.execute(rusqlite::params![s.article_id, s.key, s.flag, s.selected])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })().map_err(Into::into);
            let _ = reply.send(res);
        }
        SyncDbOp::SelectForProcessing(limit, reply) => {
            let res = (|| -> rusqlite::Result<Vec<SyncStatus>> {
                let tx = conn.transaction()?;
                // Port of NNW SyncStatusTable.selectForProcessing: mark EVERY
                // row selected, then read them back. Marking all rows (not just
                // the ones returned, and not per-(articleID,key)) keeps the
                // selection grain aligned with DeleteSelectedForProcessing,
                // which deletes by articleID — so an article's read + starred
                // rows are always processed and cleared together, never split.
                tx.execute("UPDATE syncStatus SET selected = 1", [])?;
                let mut results = Vec::new();
                {
                    let query = if limit.is_some() {
                        "SELECT articleID, key, flag, selected FROM syncStatus WHERE selected = 1 LIMIT ?"
                    } else {
                        "SELECT articleID, key, flag, selected FROM syncStatus WHERE selected = 1"
                    };
                    let mut stmt = tx.prepare(query)?;
                    let mut rows = if let Some(l) = limit {
                        stmt.query(rusqlite::params![l])?
                    } else {
                        stmt.query([])?
                    };

                    while let Some(row) = rows.next()? {
                        results.push(SyncStatus {
                            article_id: row.get(0)?,
                            key: row.get(1)?,
                            flag: row.get(2)?,
                            selected: row.get(3)?,
                        });
                    }
                }
                tx.commit()?;

                Ok(results)
            })().map_err(Into::into);
            let _ = reply.send(res);
        }
        SyncDbOp::DeleteSelectedForProcessing(ids, reply) => {
            let res = (|| -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                {
                    let mut stmt =
                        tx.prepare("DELETE FROM syncStatus WHERE articleID = ? AND selected = 1")?;
                    for id in &ids {
                        stmt.execute(rusqlite::params![id])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })()
            .map_err(Into::into);
            let _ = reply.send(res);
        }
        SyncDbOp::ResetAllSelectedForProcessing(reply) => {
            let res = conn
                .execute("UPDATE syncStatus SET selected = 0", [])
                .map(|_| ())
                .map_err(Into::into);
            let _ = reply.send(res);
        }
        SyncDbOp::WipeAll(reply) => {
            let res = conn
                .execute("DELETE FROM syncStatus", [])
                .map_err(Into::into);
            let _ = reply.send(res);
        }
    }
}

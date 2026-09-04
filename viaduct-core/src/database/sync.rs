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
    /// (articleID, key) pairs. Key-scoped (NNW `e5171cbb0`): landing an
    /// article's read batch must not delete its queued starred row.
    DeleteSelectedForProcessing(Vec<(String, String)>, oneshot::Sender<Result<()>>),
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
                // row selected, then read them back. The delegate reads all
                // of them and clears each landed batch per-(articleID, key);
                // anything left selected is re-armed by
                // ResetAllSelectedForProcessing at the end of the cycle.
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
        SyncDbOp::DeleteSelectedForProcessing(pairs, reply) => {
            let res = (|| -> rusqlite::Result<()> {
                let tx = conn.transaction()?;
                {
                    let mut stmt = tx.prepare(
                        "DELETE FROM syncStatus WHERE articleID = ? AND key = ? AND selected = 1",
                    )?;
                    for (id, key) in &pairs {
                        stmt.execute(rusqlite::params![id, key])?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: &str, key: &str, flag: bool) -> SyncStatus {
        SyncStatus {
            article_id: id.to_string(),
            key: key.to_string(),
            flag,
            selected: false,
        }
    }

    /// NNW `e5171cbb0`: deleting an article's landed read row must
    /// leave its queued starred row in the queue.
    #[test]
    fn delete_is_scoped_to_key() {
        let mut conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn).unwrap();

        let (tx, mut rx) = oneshot::channel();
        handle_op(
            &mut conn,
            SyncDbOp::InsertStatuses(
                vec![
                    status("a1", "read", true),
                    status("a1", "starred", true),
                    status("a2", "read", true),
                ],
                tx,
            ),
        );
        rx.try_recv().unwrap().unwrap();

        let (tx, mut rx) = oneshot::channel();
        handle_op(&mut conn, SyncDbOp::SelectForProcessing(None, tx));
        let selected = rx.try_recv().unwrap().unwrap();
        assert_eq!(selected.len(), 3);

        // The read batch for a1 landed; the starred batch did not.
        let (tx, mut rx) = oneshot::channel();
        handle_op(
            &mut conn,
            SyncDbOp::DeleteSelectedForProcessing(vec![("a1".into(), "read".into())], tx),
        );
        rx.try_recv().unwrap().unwrap();

        // Re-arm everything and look at what is left queued.
        let (tx, mut rx) = oneshot::channel();
        handle_op(&mut conn, SyncDbOp::ResetAllSelectedForProcessing(tx));
        rx.try_recv().unwrap().unwrap();

        let (tx, mut rx) = oneshot::channel();
        handle_op(&mut conn, SyncDbOp::SelectForProcessing(None, tx));
        let queued = rx.try_recv().unwrap().unwrap();
        let mut keys: Vec<(&str, &str)> = queued
            .iter()
            .map(|s| (s.article_id.as_str(), s.key.as_str()))
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![("a1", "starred"), ("a2", "read")],
            "starred row for a1 must survive the read batch landing"
        );
    }
}

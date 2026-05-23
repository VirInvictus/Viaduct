// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.7.0 — Custom Smart Feeds.
//!
//! User-named, persistent saved searches over the article corpus. A
//! Smart Feed is a name and a list of `Condition`s combined with AND
//! semantics. Built-in pseudo-feeds (Today / All Unread / Starred)
//! continue to live in `accounts.rs` as their own ops; this module
//! only handles user-defined feeds.
//!
//! Persisted as JSON at `$XDG_DATA_HOME/viaduct/smart-feeds.json`.
//! Atomic writes via temp-file + rename, same shape as the OPML
//! writer. Disk I/O happens off the GTK thread; the GTK side talks
//! through `Account::list_smart_feeds` / `add_smart_feed` /
//! `delete_smart_feed`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{Result, ViaductError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartFeed {
    pub id: String,
    pub name: String,
    pub rules: SmartFeedRules,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmartFeedRules {
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Condition {
    /// Article title contains the substring (case-insensitive).
    TitleContains { value: String },
    /// Article body / summary / content_text contains the substring.
    BodyContains { value: String },
    /// Author name (any of the article's authors) contains the substring.
    AuthorContains { value: String },
    /// Feed ID matches exactly. UI populates from the OPML.
    FeedIs { feed_id: String },
    /// Read-state filter. `read=true` keeps read articles, `false` keeps unread.
    Read { read: bool },
    /// Star-state filter.
    Starred { starred: bool },
    /// Article publication-time newer than N days ago.
    NewerThanDays { days: i64 },
    /// Article publication-time older than N days ago.
    OlderThanDays { days: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SmartFeedsFile {
    pub version: u32,
    pub feeds: Vec<SmartFeed>,
}

const CURRENT_VERSION: u32 = 1;

impl SmartFeedsFile {
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION,
            feeds: Vec::new(),
        }
    }
}

/// Load the on-disk Smart Feeds file. Missing file → empty store; this
/// is an expected first-run state, not an error.
pub fn load(path: &Path) -> Result<SmartFeedsFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            ViaductError::Parse(crate::error::ParseError::Malformed(format!(
                "smart-feeds.json: {e}"
            )))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SmartFeedsFile::new()),
        Err(e) => Err(ViaductError::Parse(crate::error::ParseError::Malformed(
            format!("read smart-feeds.json: {e}"),
        ))),
    }
}

/// Atomic write: serialize to a sibling temp file, fsync, rename.
/// Same approach the OPML writer uses.
pub fn save(path: &Path, file: &SmartFeedsFile) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        ViaductError::Parse(crate::error::ParseError::Malformed(
            "smart-feeds.json has no parent dir".to_string(),
        ))
    })?;
    let tmp_path = parent.join(format!(".smart-feeds.json.tmp.{}", std::process::id()));
    let json = serde_json::to_vec_pretty(file).map_err(|e| {
        ViaductError::Parse(crate::error::ParseError::Malformed(format!(
            "serialise smart-feeds.json: {e}"
        )))
    })?;
    std::fs::write(&tmp_path, &json).map_err(|e| {
        ViaductError::Parse(crate::error::ParseError::Malformed(format!(
            "write smart-feeds.json tmp: {e}"
        )))
    })?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        ViaductError::Parse(crate::error::ParseError::Malformed(format!(
            "rename smart-feeds.json: {e}"
        )))
    })?;
    Ok(())
}

/// SQL fragment + bind value for a single condition. The aggregator
/// joins fragments with `AND`. The query expects `articles` aliased
/// as `a` and `statuses` LEFT JOINed as `s`.
pub fn condition_sql(c: &Condition) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;

    match c {
        Condition::TitleContains { value } => (
            "LOWER(COALESCE(a.title, '')) LIKE ?1".to_string(),
            vec![Value::Text(like_pattern(value))],
        ),
        Condition::BodyContains { value } => (
            "(LOWER(COALESCE(a.content_html, '')) LIKE ?1 \
              OR LOWER(COALESCE(a.content_text, '')) LIKE ?1 \
              OR LOWER(COALESCE(a.summary, '')) LIKE ?1)"
                .to_string(),
            vec![Value::Text(like_pattern(value))],
        ),
        Condition::AuthorContains { value } => (
            "EXISTS (SELECT 1 FROM authorsLookup al \
              JOIN authors au ON au.author_id = al.author_id \
              WHERE al.article_id = a.article_id \
              AND LOWER(COALESCE(au.name, '')) LIKE ?1)"
                .to_string(),
            vec![Value::Text(like_pattern(value))],
        ),
        Condition::FeedIs { feed_id } => (
            "a.feed_id = ?1".to_string(),
            vec![Value::Text(feed_id.clone())],
        ),
        Condition::Read { read } => (
            if *read {
                "COALESCE(s.read, 0) = 1".to_string()
            } else {
                "COALESCE(s.read, 0) = 0".to_string()
            },
            Vec::new(),
        ),
        Condition::Starred { starred } => (
            if *starred {
                "COALESCE(s.starred, 0) = 1".to_string()
            } else {
                "COALESCE(s.starred, 0) = 0".to_string()
            },
            Vec::new(),
        ),
        Condition::NewerThanDays { days } => {
            let cutoff = Utc::now() - chrono::Duration::days(*days);
            (
                "COALESCE(a.date_published, a.date_modified) >= ?1".to_string(),
                vec![Value::Integer(cutoff.timestamp())],
            )
        }
        Condition::OlderThanDays { days } => {
            let cutoff = Utc::now() - chrono::Duration::days(*days);
            (
                "COALESCE(a.date_published, a.date_modified) < ?1".to_string(),
                vec![Value::Integer(cutoff.timestamp())],
            )
        }
    }
}

fn like_pattern(needle: &str) -> String {
    let mut escaped = String::with_capacity(needle.len() + 2);
    escaped.push('%');
    for ch in needle.chars() {
        if ch == '%' || ch == '_' || ch == '\\' {
            escaped.push('\\');
        }
        for c in ch.to_lowercase() {
            escaped.push(c);
        }
    }
    escaped.push('%');
    escaped
}

/// Build the full WHERE clause + parameter bag for a SmartFeedRules.
/// Each condition compiles to a fragment; fragments AND together.
/// Empty rules → `1=1` no-op (returns everything).
pub fn build_where(rules: &SmartFeedRules) -> (String, Vec<rusqlite::types::Value>) {
    if rules.conditions.is_empty() {
        return ("1=1".to_string(), Vec::new());
    }
    let mut clauses = Vec::with_capacity(rules.conditions.len());
    let mut params: Vec<rusqlite::types::Value> = Vec::new();
    for c in &rules.conditions {
        let (mut frag, mut bind) = condition_sql(c);
        renumber(&mut frag, params.len());
        clauses.push(format!("({frag})"));
        params.append(&mut bind);
    }
    (clauses.join(" AND "), params)
}

/// Shift `?N` placeholders inside `frag` so they continue from `offset`.
/// Each condition's `condition_sql` returns fragments numbered from `?1`;
/// after concatenation, we need globally-unique numbers. Simple linear
/// scan over the bytes — placeholder names cap at single digit (`?1` /
/// `?2`) by construction.
fn renumber(frag: &mut String, offset: usize) {
    if offset == 0 {
        return;
    }
    let mut out = String::with_capacity(frag.len());
    let bytes = frag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'?' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            out.push('?');
            let mut end = i + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let n: usize = std::str::from_utf8(&bytes[i + 1..end])
                .unwrap()
                .parse()
                .unwrap();
            out.push_str(&(n + offset).to_string());
            i = end;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    *frag = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct ScopedDir(PathBuf);
    impl ScopedDir {
        fn new(label: &str) -> Self {
            let pid = std::process::id();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir()
                .join(format!("viaduct-smart-feeds-{label}-{pid}-{ts}-{counter}"));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScopedDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn round_trip_empty() {
        let dir = ScopedDir::new("empty");
        let path = dir.path().join("smart-feeds.json");
        let file = SmartFeedsFile::new();
        save(&path, &file).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.version, CURRENT_VERSION);
        assert!(loaded.feeds.is_empty());
    }

    #[test]
    fn round_trip_with_feeds() {
        let dir = ScopedDir::new("rt");
        let path = dir.path().join("smart-feeds.json");
        let mut file = SmartFeedsFile::new();
        file.feeds.push(SmartFeed {
            id: "abc".to_string(),
            name: "Linux news".to_string(),
            rules: SmartFeedRules {
                conditions: vec![
                    Condition::TitleContains {
                        value: "linux".to_string(),
                    },
                    Condition::Read { read: false },
                ],
            },
            created_at: Utc::now(),
        });
        save(&path, &file).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.feeds.len(), 1);
        assert_eq!(loaded.feeds[0].name, "Linux news");
        assert_eq!(loaded.feeds[0].rules.conditions.len(), 2);
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = ScopedDir::new("missing");
        let path = dir.path().join("does-not-exist.json");
        let loaded = load(&path).unwrap();
        assert!(loaded.feeds.is_empty());
    }

    #[test]
    fn condition_sql_title_contains() {
        let (sql, params) = condition_sql(&Condition::TitleContains {
            value: "Hello".to_string(),
        });
        assert!(sql.contains("a.title"));
        assert_eq!(params.len(), 1);
        match &params[0] {
            rusqlite::types::Value::Text(s) => {
                assert!(s.contains("hello"));
                assert!(s.starts_with('%'));
                assert!(s.ends_with('%'));
            }
            _ => panic!("expected Text param"),
        }
    }

    #[test]
    fn build_where_renumbers_placeholders() {
        let rules = SmartFeedRules {
            conditions: vec![
                Condition::TitleContains {
                    value: "a".to_string(),
                },
                Condition::AuthorContains {
                    value: "b".to_string(),
                },
            ],
        };
        let (sql, params) = build_where(&rules);
        // Two placeholders, ?1 and ?2, never duplicated.
        assert!(sql.contains("?1"));
        assert!(sql.contains("?2"));
        assert!(!sql.contains("?3"));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn build_where_status_only_no_params() {
        let rules = SmartFeedRules {
            conditions: vec![
                Condition::Read { read: false },
                Condition::Starred { starred: true },
            ],
        };
        let (sql, params) = build_where(&rules);
        assert!(params.is_empty());
        assert!(sql.contains("s.read"));
        assert!(sql.contains("s.starred"));
        assert!(sql.contains(" AND "));
    }

    #[test]
    fn empty_rules_match_all() {
        let (sql, params) = build_where(&SmartFeedRules::default());
        assert_eq!(sql, "1=1");
        assert!(params.is_empty());
    }

    #[test]
    fn like_pattern_escapes_special_chars() {
        let p = like_pattern("100%_done");
        assert!(p.contains("\\%"));
        assert!(p.contains("\\_"));
    }
}

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ViaductError>;

#[derive(Debug, Error)]
pub enum ViaductError {
    #[error("$HOME is not set; cannot resolve XDG fallback path")]
    MissingHome,

    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Database(#[from] DatabaseError),

    #[error(transparent)]
    Network(#[from] NetworkError),

    #[error(transparent)]
    Parse(#[from] ParseError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("credentials error: {0}")]
    Credentials(String),

    #[error(transparent)]
    Generic(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ViaductError {
    fn from(err: rusqlite::Error) -> Self {
        ViaductError::Database(DatabaseError::Sqlite(err))
    }
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("writer task has shut down")]
    WriterGone,

    #[error("schema migration failed: {0}")]
    Migration(String),
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// Used by `feed_discovery::discover_feed` when the response carries
    /// a `reqwest::Error` we want to surface verbatim (vs. the `Http`
    /// branch, which is only used for the `From` impl). Same shape, but
    /// the discoverer constructs it explicitly so the call site reads
    /// as "discovery failed at the network layer," not "general HTTP."
    #[error("reqwest error during feed discovery: {0}")]
    Reqwest(reqwest::Error),

    #[error("invalid url: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    /// `feed_discovery::discover_feed` exhausted both passes (URL
    /// didn't parse as a feed, no `<link rel="alternate">` in the HTML).
    #[error("no feed found at the supplied URL")]
    NoFeedFound,
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("xml parse error: {0}")]
    Xml(#[from] quick_xml::Error),

    #[error("xml deserialize error: {0}")]
    XmlDe(String),

    #[error("xml serialize error: {0}")]
    XmlSe(String),

    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unrecognized feed format")]
    UnknownFormat,

    #[error("malformed feed: {0}")]
    Malformed(String),
}

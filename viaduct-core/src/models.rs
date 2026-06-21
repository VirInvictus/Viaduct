// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct FeedSettings {
    pub feed_id: String,
    pub feed_url: String,
    pub home_page_url: Option<String>,
    pub icon_url: Option<String>,
    pub favicon_url: Option<String>,
    pub edited_name: Option<String>,
    pub content_hash: Option<String>,
    pub last_modified: Option<String>,
    pub etag: Option<String>,
    pub date_created: Option<DateTime<Utc>>,
    pub max_age: Option<i64>,
    pub authors_json: Option<String>,
    pub folder_relationship_json: Option<String>,
    pub last_check_date: Option<DateTime<Utc>>,
    pub reader_view_always_enabled: bool,
    /// v2.4.0: per-feed opt-in for desktop notifications. When `true`,
    /// new articles fetched from this feed during a refresh cycle
    /// trigger a per-feed `gio::Notification` (gated by the global
    /// `notifications-on-refresh` GSetting). Defaults to `false` so the
    /// schema upgrade is silent — users explicitly opt in via the feed
    /// inspector dialog.
    pub new_article_notifications_enabled: bool,
    /// HTTP status code from the most recent feed download attempt
    /// (NNW `4c85c907f` `FeedSettings.lastResponseCode`). `None` until
    /// the feed has been fetched at least once over HTTP; a network
    /// error that never reached the server leaves the prior value.
    pub last_response_code: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Author {
    pub name: Option<String>,
    pub url: Option<String>,
    pub avatar_url: Option<String>,
    pub email: Option<String>,
}

/// Port of NNW `ParsedAttachment`. Covers RSS `<enclosure>`, RSS
/// `<media:content>` / `<media:thumbnail>`, Atom `<link rel="enclosure">`,
/// and JSON Feed `attachments[]` entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Attachment {
    pub url: String,
    pub mime_type: Option<String>,
    pub title: Option<String>,
    pub size_in_bytes: Option<i64>,
    pub duration_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub id: String,
    pub url: String,
    pub name: Option<String>,
    pub edited_name: Option<String>,
    pub home_page_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub name: String,
    pub feeds: Vec<Feed>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Article {
    pub article_id: String,
    pub feed_id: String,
    pub title: Option<String>,
    pub content_html: Option<String>,
    pub content_text: Option<String>,
    pub url: Option<String>,
    pub external_url: Option<String>,
    pub summary: Option<String>,
    pub image_url: Option<String>,
    pub date_published: Option<DateTime<Utc>>,
    pub date_modified: Option<DateTime<Utc>>,
    pub authors: Vec<Author>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArticleStatus {
    pub article_id: String,
    pub read: bool,
    pub starred: bool,
    pub date_arrived: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedItem {
    pub id: String,
    pub title: Option<String>,
    pub content_html: Option<String>,
    pub content_text: Option<String>,
    pub url: Option<String>,
    pub external_url: Option<String>,
    pub summary: Option<String>,
    pub image_url: Option<String>,
    pub date_published: Option<DateTime<Utc>>,
    pub date_modified: Option<DateTime<Utc>>,
    pub authors: Vec<Author>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFeed {
    pub title: Option<String>,
    pub home_page_url: Option<String>,
    pub feed_url: Option<String>,
    /// RSS channel `<image><url>` or Atom `<icon>`/`<logo>`. Refresher
    /// persists into `FeedSettings.icon_url` for sidebar favicons.
    pub icon_url: Option<String>,
    /// RSS channel `<language>` or Atom `xml:lang` on the `<feed>` root.
    /// Not yet used for rendering direction; Phase 11 reserves for later.
    pub language: Option<String>,
    pub items: Vec<ParsedItem>,
}

#[derive(Debug, Clone, Default)]
pub struct ArticleChanges {
    pub new_articles: Vec<Article>,
    pub updated_articles: Vec<Article>,
    pub deleted_article_ids: HashSet<String>,
    pub statuses: Vec<ArticleStatus>,
}

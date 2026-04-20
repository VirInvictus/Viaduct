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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Author {
    pub name: Option<String>,
    pub url: Option<String>,
    pub avatar_url: Option<String>,
    pub email: Option<String>,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArticleStatus {
    pub article_id: String,
    pub read: bool,
    pub starred: bool,
    pub date_arrived: DateTime<Utc>,
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct ParsedFeed {
    pub title: Option<String>,
    pub home_page_url: Option<String>,
    pub feed_url: Option<String>,
    pub items: Vec<ParsedItem>,
}

#[derive(Debug, Clone, Default)]
pub struct ArticleChanges {
    pub new_articles: Vec<Article>,
    pub updated_articles: Vec<Article>,
    pub deleted_article_ids: HashSet<String>,
    pub statuses: Vec<ArticleStatus>,
}

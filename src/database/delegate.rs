use crate::database::accounts::Account;
use crate::error::{Result, ViaductError, NetworkError};
use std::sync::Arc;
use crate::network::inoreader::{ReaderAPICaller, ReaderAPIVariant, ItemIDType};
use crate::network::credentials::fetch_credentials;

pub trait AccountDelegate: Send + Sync {
    fn refresh_all(&self, account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
    fn sync_article_status(&self, account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
    fn import_opml(&self, account: Arc<Account>, path: &std::path::Path) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>>;
}

pub struct LocalAccountDelegate;

impl AccountDelegate for LocalAccountDelegate {
    fn refresh_all(&self, _account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            Ok(())
        })
    }

    fn sync_article_status(&self, _account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            Ok(())
        })
    }

    fn import_opml(&self, account: Arc<Account>, path: &std::path::Path) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>> {
        let path = path.to_path_buf();
        Box::pin(async move {
            account.import_opml_internal(&path).await
        })
    }
}

pub struct InoreaderAccountDelegate {
    caller: ReaderAPICaller,
}

impl InoreaderAccountDelegate {
    pub fn new() -> Self {
        Self {
            caller: ReaderAPICaller::new(ReaderAPIVariant::Inoreader),
        }
    }

    async fn get_auth_token(&self) -> Result<String> {
        let creds = fetch_credentials("inoreader").await?
            .ok_or_else(|| ViaductError::Network(NetworkError::RateLimited { retry_after_secs: 0 }))?; // Simplified error
        
        let password = creds.password.ok_or_else(|| ViaductError::Network(NetworkError::RateLimited { retry_after_secs: 0 }))?;
        self.caller.validate_credentials(&creds.username, &password).await
    }
}

impl AccountDelegate for InoreaderAccountDelegate {
    fn refresh_all(&self, account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let auth_token = self.get_auth_token().await?;
            
            // 1. Sync Folders and Feeds
            let subscriptions = self.caller.retrieve_subscriptions(&auth_token).await?;
            let tags = self.caller.retrieve_tags(&auth_token).await?;
            
            let existing_opml = account.load_opml().await?;
            let synced_opml = crate::database::opml::sync_inoreader_account(&existing_opml, subscriptions, tags);
            account.save_opml(synced_opml).await?;
            
            // 2. Send Article Status (Local -> Remote)
            let sync_statuses = account.select_sync_statuses_for_processing(None).await?;
            if !sync_statuses.is_empty() {
                let mut read_ids = Vec::new();
                let mut unread_ids = Vec::new();
                let mut starred_ids = Vec::new();
                let mut unstarred_ids = Vec::new();

                for s in &sync_statuses {
                    match (s.key.as_str(), s.flag) {
                        ("read", true) => read_ids.push(s.article_id.clone()),
                        ("read", false) => unread_ids.push(s.article_id.clone()),
                        ("starred", true) => starred_ids.push(s.article_id.clone()),
                        ("starred", false) => unstarred_ids.push(s.article_id.clone()),
                        _ => {}
                    }
                }

                if !read_ids.is_empty() {
                    self.caller.update_state_to_entries(&auth_token, &read_ids, "user/-/state/com.google/read", true).await?;
                }
                if !unread_ids.is_empty() {
                    self.caller.update_state_to_entries(&auth_token, &unread_ids, "user/-/state/com.google/read", false).await?;
                }
                if !starred_ids.is_empty() {
                    self.caller.update_state_to_entries(&auth_token, &starred_ids, "user/-/state/com.google/starred", true).await?;
                }
                if !unstarred_ids.is_empty() {
                    self.caller.update_state_to_entries(&auth_token, &unstarred_ids, "user/-/state/com.google/starred", false).await?;
                }

                let processed_ids: Vec<String> = sync_statuses.into_iter().map(|s| s.article_id).collect();
                account.delete_sync_statuses_selected_for_processing(processed_ids).await?;
            }

            // 3. Refresh Article Status (Remote -> Local)
            let remote_unread_ids: std::collections::HashSet<String> = self.caller.retrieve_item_ids(&auth_token, ItemIDType::Unread, None).await?.into_iter().collect();
            let remote_starred_ids: std::collections::HashSet<String> = self.caller.retrieve_item_ids(&auth_token, ItemIDType::Starred, None).await?.into_iter().collect();

            let pending_statuses = account.select_sync_statuses_for_processing(None).await?;
            let mut pending_read_ids = std::collections::HashSet::new();
            let mut pending_starred_ids = std::collections::HashSet::new();
            for s in &pending_statuses {
                if s.key == "read" { pending_read_ids.insert(s.article_id.clone()); }
                if s.key == "starred" { pending_starred_ids.insert(s.article_id.clone()); }
            }
            // Reset them so they can be processed again later if needed (but they are technically processed now)
            account.reset_all_sync_statuses_selected_for_processing().await?;

            let local_unread_ids = account.fetch_unread_article_ids().await?;
            let local_starred_ids = account.fetch_starred_article_ids().await?;

            // Updatable = Remote - Pending
            let updatable_remote_unread: Vec<String> = remote_unread_ids.iter().filter(|id| !pending_read_ids.contains(*id)).cloned().collect();
            let updatable_remote_starred: Vec<String> = remote_starred_ids.iter().filter(|id| !pending_starred_ids.contains(*id)).cloned().collect();

            // Delta Unread = UpdatableRemoteUnread - LocalUnread
            let delta_unread: Vec<String> = updatable_remote_unread.iter().filter(|id| !local_unread_ids.contains(*id)).cloned().collect();
            // Delta Read = LocalUnread - UpdatableRemoteUnread
            let delta_read: Vec<String> = local_unread_ids.iter().filter(|id| !remote_unread_ids.contains(*id) && !pending_read_ids.contains(*id)).cloned().collect();

            // Delta Starred = UpdatableRemoteStarred - LocalStarred
            let delta_starred: Vec<String> = updatable_remote_starred.iter().filter(|id| !local_starred_ids.contains(*id)).cloned().collect();
            // Delta Unstarred = LocalStarred - UpdatableRemoteStarred
            let delta_unstarred: Vec<String> = local_starred_ids.iter().filter(|id| !remote_starred_ids.contains(*id) && !pending_starred_ids.contains(*id)).cloned().collect();

            if !delta_unread.is_empty() { account.update_statuses_read(delta_unread, false).await?; }
            if !delta_read.is_empty() { account.update_statuses_read(delta_read, true).await?; }
            if !delta_starred.is_empty() { account.update_statuses_starred(delta_starred, true).await?; }
            if !delta_unstarred.is_empty() { account.update_statuses_starred(delta_unstarred, false).await?; }
            
            // 4. Refresh Missing Articles
            let missing_ids = account.fetch_missing_article_ids().await?;
            if !missing_ids.is_empty() {
                for chunk in missing_ids.chunks(100) {
                    let entries = self.caller.retrieve_entries(&auth_token, chunk).await?;
                    let mut articles = Vec::new();
                    for entry in entries {
                        if let Some(ref stream_id) = entry.origin.stream_id {
                            let stream_id = stream_id.clone();
                            let unique_id = entry.unique_id();
                            let article_id = crate::database::articles::article_id_for(&stream_id, &unique_id);
                            
                            let date_published = entry.published_timestamp.and_then(|ts| {
                                chrono::DateTime::from_timestamp(ts as i64, 0)
                            });

                            let external_url = entry.alternates.as_ref()
                                .and_then(|a| a.first())
                                .and_then(|a| a.url.clone());

                            let content = entry.summary.content.clone();
                            let authors = entry.author.clone().map(|name| {
                                vec![crate::models::Author {
                                    name: Some(name),
                                    url: None,
                                    avatar_url: None,
                                    email: None,
                                }]
                            }).unwrap_or_default();

                            articles.push(crate::models::Article {
                                article_id,
                                feed_id: stream_id,
                                title: entry.title,
                                content_html: content.clone(),
                                content_text: None,
                                url: None,
                                external_url,
                                summary: content,
                                image_url: None,
                                date_published,
                                date_modified: None,
                                authors,
                                attachments: Vec::new(),
                            });
                        }
                    }
                    if !articles.is_empty() {
                        account.batch_insert_articles(articles).await?;
                    }
                }
            }

            Ok(())
        })
    }

    fn sync_article_status(&self, account: Arc<Account>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        self.refresh_all(account)
    }

    fn import_opml(&self, account: Arc<Account>, path: &std::path::Path) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<crate::models::Feed>>> + Send + '_>> {
        let path = path.to_path_buf();
        Box::pin(async move {
            let auth_token = self.get_auth_token().await?;
            let xml = tokio::fs::read(&path).await?;
            self.caller.import_opml(&auth_token, &xml).await?;
            account.import_opml_internal(&path).await
        })
    }
}

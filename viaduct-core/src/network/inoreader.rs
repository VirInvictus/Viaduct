// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::error::{NetworkError, Result, ViaductError};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderAPIVariant {
    Inoreader,
}

impl ReaderAPIVariant {
    pub fn host(&self) -> &'static str {
        match self {
            Self::Inoreader => "https://www.inoreader.com",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPITag {
    pub id: String,
    pub sortid: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPITagContainer {
    pub tags: Vec<ReaderAPITag>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPISubscription {
    #[serde(rename = "id")]
    pub feed_id: String,
    pub title: String,
    pub categories: Vec<ReaderAPITag>,
    pub url: String,
    #[serde(rename = "htmlUrl")]
    pub html_url: Option<String>,
    #[serde(rename = "iconUrl")]
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPISubscriptionContainer {
    pub subscriptions: Vec<ReaderAPISubscription>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIQuickAddResult {
    #[serde(rename = "numResults")]
    pub num_results: i32,
    #[serde(rename = "streamId")]
    pub stream_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIReference {
    #[serde(rename = "id")]
    pub item_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIReferenceWrapper {
    #[serde(rename = "itemRefs")]
    pub item_refs: Vec<ReaderAPIReference>,
    pub continuation: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntry {
    #[serde(rename = "id")]
    pub article_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub summary: ReaderAPIEntrySummary,
    #[serde(rename = "published")]
    pub published_timestamp: Option<f64>,
    #[serde(rename = "alternate")]
    pub alternates: Option<Vec<ReaderAPIEntryAlternate>>,
    pub categories: Option<Vec<String>>,
    pub origin: ReaderAPIEntryOrigin,
}

impl ReaderAPIEntry {
    pub fn unique_id(&self) -> String {
        // Should look something like "tag:google.com,2005:reader/item/00058b10ce338909"
        let id_part = self
            .article_id
            .split('/')
            .next_back()
            .unwrap_or(&self.article_id);

        // Convert hex representation back to integer and then a string representation
        if let Ok(id_number) = u64::from_str_radix(id_part, 16) {
            id_number.to_string()
        } else {
            id_part.to_string()
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntrySummary {
    pub content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryOrigin {
    #[serde(rename = "streamId")]
    pub stream_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryAlternate {
    #[serde(rename = "href")]
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReaderAPIEntryWrapper {
    #[serde(rename = "items")]
    pub entries: Vec<ReaderAPIEntry>,
}

enum ReaderAPIEndpoints {
    Login,
    Token,
    DisableTag,
    RenameTag,
    TagList,
    SubscriptionList,
    SubscriptionEdit,
    SubscriptionAdd,
    Contents,
    ItemIds,
    EditTag,
    SubscriptionImport,
}

impl ReaderAPIEndpoints {
    fn path(&self) -> &'static str {
        match self {
            Self::Login => "/accounts/ClientLogin",
            Self::Token => "/reader/api/0/token",
            Self::DisableTag => "/reader/api/0/disable-tag",
            Self::RenameTag => "/reader/api/0/rename-tag",
            Self::TagList => "/reader/api/0/tag/list",
            Self::SubscriptionList => "/reader/api/0/subscription/list",
            Self::SubscriptionEdit => "/reader/api/0/subscription/edit",
            Self::SubscriptionAdd => "/reader/api/0/subscription/quickadd",
            Self::Contents => "/reader/api/0/stream/items/contents",
            Self::ItemIds => "/reader/api/0/stream/items/ids",
            Self::EditTag => "/reader/api/0/edit-tag",
            Self::SubscriptionImport => "/reader/api/0/subscription/import",
        }
    }
}

pub enum ItemIDType {
    Unread,
    Starred,
    AllForAccount,
    AllForFeed,
}

pub struct ReaderAPICaller {
    client: reqwest::Client,
    variant: ReaderAPIVariant,
    access_token: tokio::sync::RwLock<Option<String>>,
}

impl ReaderAPICaller {
    pub fn new(variant: ReaderAPIVariant) -> Self {
        Self {
            client: reqwest::Client::new(),
            variant,
            access_token: tokio::sync::RwLock::new(None),
        }
    }

    fn api_base_url(&self) -> Url {
        Url::parse(self.variant.host()).expect("Invalid base URL")
    }

    fn add_variant_headers(&self, mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.variant == ReaderAPIVariant::Inoreader {
            // These come from compile-time environment variables.
            request = request.header("AppId", option_env!("INOREADER_APP_ID").unwrap_or(""));
            request = request.header("AppKey", option_env!("INOREADER_APP_KEY").unwrap_or(""));
        }
        request
    }

    pub async fn validate_credentials(&self, username: &str, password: &str) -> Result<String> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Login.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let mut request = self
            .client
            .post(url)
            .form(&[("Email", username), ("Passwd", password)]);

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })); // Simplified error
        }

        let body = resp
            .text()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        for line in body.lines() {
            if let Some(auth) = line.strip_prefix("Auth=") {
                return Ok(auth.to_string());
            }
        }

        Err(ViaductError::Network(NetworkError::RateLimited {
            retry_after_secs: 0,
        })) // Simplified error
    }

    pub async fn request_authorization_token(&self, auth_token: &str) -> Result<String> {
        {
            let token = self.access_token.read().await;
            if let Some(t) = &*token {
                return Ok(t.clone());
            }
        }

        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Token.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let mut request = self
            .client
            .get(url)
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        // Check status before caching: an error page (401 expired auth, 5xx)
        // must not be stored as the edit token, or every later write reuses
        // the garbage token until the process restarts. Mirrors the guard in
        // validate_credentials.
        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }

        let token = resp
            .text()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?
            .trim()
            .to_string();

        if token.is_empty() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }

        let mut write_token = self.access_token.write().await;
        *write_token = Some(token.clone());
        Ok(token)
    }

    /// Sends a token-authenticated request, dropping the cached edit token
    /// and retrying once on 401/403. Port of NNW `withWriteToken`.
    ///
    /// Inoreader's edit tokens are short-lived but we cache ours for the
    /// life of the process, so a token that expired mid-session used to
    /// fail every write (and the article-body fetch) until the app was
    /// restarted. `build` receives the token and returns the finished
    /// request; it runs a second time on the retry so the fresh token
    /// reaches the body, where these endpoints expect it as `T`.
    async fn with_write_token<F>(&self, auth_token: &str, build: F) -> Result<reqwest::Response>
    where
        F: Fn(String) -> reqwest::RequestBuilder,
    {
        let token = self.request_authorization_token(auth_token).await?;
        let resp = build(token)
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        let status = resp.status();
        if status != reqwest::StatusCode::UNAUTHORIZED && status != reqwest::StatusCode::FORBIDDEN {
            return Ok(resp);
        }

        *self.access_token.write().await = None;
        let fresh = self.request_authorization_token(auth_token).await?;
        build(fresh)
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))
    }

    pub async fn retrieve_subscriptions(
        &self,
        auth_token: &str,
    ) -> Result<Vec<ReaderAPISubscription>> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionList.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let mut request = self
            .client
            .get(url)
            .query(&[("output", "json")])
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        let container: ReaderAPISubscriptionContainer = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(container.subscriptions)
    }

    pub async fn retrieve_tags(&self, auth_token: &str) -> Result<Vec<ReaderAPITag>> {
        let url = self
            .api_base_url()
            .join(ReaderAPIEndpoints::TagList.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
        let mut request = self
            .client
            .get(url)
            .query(&[("output", "json")])
            .header("Authorization", format!("GoogleLogin auth={}", auth_token));

        if self.variant == ReaderAPIVariant::Inoreader {
            request = request.query(&[("types", "1")]);
        }

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        let container: ReaderAPITagContainer = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(container.tags)
    }

    pub async fn create_subscription(
        &self,
        auth_token: &str,
        url: &str,
    ) -> Result<ReaderAPISubscription> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionAdd.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[("T", token), ("quickadd", url.to_string())]);
                self.add_variant_headers(request)
            })
            .await?;

        let result: ReaderAPIQuickAddResult = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if result.num_results == 0 {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            })); // Simplified error
        }

        let subscriptions = self.retrieve_subscriptions(auth_token).await?;
        subscriptions
            .into_iter()
            .find(|s| s.feed_id == result.stream_id)
            .ok_or_else(|| {
                ViaductError::Network(NetworkError::RateLimited {
                    retry_after_secs: 0,
                })
            }) // Simplified error
    }

    pub async fn delete_subscription(&self, auth_token: &str, subscription_id: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionEdit.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[
                        ("T", token),
                        ("s", subscription_id.to_string()),
                        ("ac", "unsubscribe".to_string()),
                    ]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    async fn change_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        remove_tag_name: Option<&str>,
        add_tag_name: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionEdit.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T", token),
                    ("s", subscription_id.to_string()),
                    ("ac", "edit".to_string()),
                ];

                if let Some(name) = remove_tag_name {
                    params.push(("r", format!("user/-/label/{}", name)));
                }
                if let Some(name) = add_tag_name {
                    params.push(("a", format!("user/-/label/{}", name)));
                }
                if let Some(t) = title {
                    params.push(("t", t.to_string()));
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn rename_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        new_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, None, None, Some(new_name))
            .await
    }

    pub async fn create_tagging(
        &self,
        auth_token: &str,
        subscription_id: &str,
        tag_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, None, Some(tag_name), None)
            .await
    }

    pub async fn delete_tagging(
        &self,
        auth_token: &str,
        subscription_id: &str,
        tag_name: &str,
    ) -> Result<()> {
        self.change_subscription(auth_token, subscription_id, Some(tag_name), None, None)
            .await
    }

    pub async fn move_subscription(
        &self,
        auth_token: &str,
        subscription_id: &str,
        source_tag: &str,
        dest_tag: &str,
    ) -> Result<()> {
        self.change_subscription(
            auth_token,
            subscription_id,
            Some(source_tag),
            Some(dest_tag),
            None,
        )
        .await
    }

    pub async fn rename_tag(&self, auth_token: &str, old_name: &str, new_name: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::RenameTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[
                        ("T", token),
                        ("s", format!("user/-/label/{}", old_name)),
                        ("dest", format!("user/-/label/{}", new_name)),
                    ]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn delete_tag(&self, auth_token: &str, folder_external_id: &str) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::DisableTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&[("T", token), ("s", folder_external_id.to_string())]);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn retrieve_entries(
        &self,
        auth_token: &str,
        article_ids: &[String],
    ) -> Result<Vec<ReaderAPIEntry>> {
        if article_ids.is_empty() {
            return Ok(Vec::new());
        }

        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::Contents.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T".to_string(), token),
                    ("output".to_string(), "json".to_string()),
                ];

                for id in article_ids {
                    // Inoreader (and others) often want hex IDs for some reason in these calls.
                    // NNW converts decimal IDs to hex.
                    if let Ok(val) = id.parse::<u64>() {
                        params.push((
                            "i".to_string(),
                            format!("tag:google.com,2005:reader/item/{:016x}", val),
                        ));
                    } else {
                        params.push((
                            "i".to_string(),
                            format!("tag:google.com,2005:reader/item/{}", id),
                        ));
                    }
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        let wrapper: ReaderAPIEntryWrapper = resp
            .json()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;
        Ok(wrapper.entries)
    }

    pub async fn retrieve_item_ids(
        &self,
        auth_token: &str,
        request_type: ItemIDType,
        feed_id: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut results = Vec::new();
        let mut continuation: Option<String> = None;

        loop {
            let mut query = vec![("n", "1000".to_string()), ("output", "json".to_string())];

            match request_type {
                ItemIDType::AllForAccount => {
                    query.push(("s", "user/-/state/com.google/reading-list".to_string()));
                }
                ItemIDType::AllForFeed => {
                    if let Some(fid) = feed_id {
                        query.push(("s", fid.to_string()));
                    } else {
                        return Err(ViaductError::Network(NetworkError::RateLimited {
                            retry_after_secs: 0,
                        }));
                    }
                }
                ItemIDType::Unread => {
                    query.push(("s", "user/-/state/com.google/reading-list".to_string()));
                    query.push(("xt", "user/-/state/com.google/read".to_string()));
                }
                ItemIDType::Starred => {
                    query.push(("s", "user/-/state/com.google/starred".to_string()));
                }
            }

            if let Some(c) = &continuation {
                query.push(("c", c.clone()));
            }

            let url = self
                .api_base_url()
                .join(ReaderAPIEndpoints::ItemIds.path())
                .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;
            let mut request = self
                .client
                .get(url)
                .query(&query)
                .header("Authorization", format!("GoogleLogin auth={}", auth_token));

            request = self.add_variant_headers(request);

            let resp = request
                .send()
                .await
                .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

            let wrapper: ReaderAPIReferenceWrapper = resp
                .json()
                .await
                .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

            for reference in wrapper.item_refs {
                results.push(reference.item_id);
            }

            continuation = wrapper.continuation;
            if continuation.is_none() {
                break;
            }
        }

        Ok(results)
    }

    pub async fn update_state_to_entries(
        &self,
        auth_token: &str,
        article_ids: &[String],
        state: &str,
        add: bool,
    ) -> Result<()> {
        if article_ids.is_empty() {
            return Ok(());
        }

        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::EditTag.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let action = if add { "a" } else { "r" };

        let resp = self
            .with_write_token(auth_token, |token| {
                let mut params = vec![
                    ("T".to_string(), token),
                    (action.to_string(), state.to_string()),
                ];

                for id in article_ids {
                    if let Ok(val) = id.parse::<u64>() {
                        params.push((
                            "i".to_string(),
                            format!("tag:google.com,2005:reader/item/{:016x}", val),
                        ));
                    } else {
                        params.push((
                            "i".to_string(),
                            format!("tag:google.com,2005:reader/item/{}", id),
                        ));
                    }
                }

                let request = self
                    .client
                    .post(endpoint.clone())
                    .header("Authorization", format!("GoogleLogin auth={}", auth_token))
                    .form(&params);
                self.add_variant_headers(request)
            })
            .await?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }

    pub async fn import_opml(&self, auth_token: &str, opml_data: &[u8]) -> Result<()> {
        let endpoint = self
            .api_base_url()
            .join(ReaderAPIEndpoints::SubscriptionImport.path())
            .map_err(|e| ViaductError::Network(NetworkError::InvalidUrl(e)))?;

        let mut request = self
            .client
            .post(endpoint)
            .header("Authorization", format!("GoogleLogin auth={}", auth_token))
            .header("Content-Type", "text/xml")
            .body(opml_data.to_vec());

        request = self.add_variant_headers(request);

        let resp = request
            .send()
            .await
            .map_err(|e| ViaductError::Network(NetworkError::Http(e)))?;

        if !resp.status().is_success() {
            return Err(ViaductError::Network(NetworkError::RateLimited {
                retry_after_secs: 0,
            }));
        }
        Ok(())
    }
}

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartFeedType {
    Today,
    Unread,
    Starred,
}

pub trait SmartFeedDelegate {
    fn fetch_articles(&self, account: &LocalAccount) -> impl std::future::Future<Output = Result<Vec<Article>, crate::error::ViaductError>> + Send;
}

pub struct TodayFeedDelegate;
impl SmartFeedDelegate for TodayFeedDelegate {
    async fn fetch_articles(&self, account: &LocalAccount) -> Result<Vec<Article>, crate::error::ViaductError> {
        account.fetch_today_articles().await
    }
}

pub struct UnreadFeedDelegate;
impl SmartFeedDelegate for UnreadFeedDelegate {
    async fn fetch_articles(&self, account: &LocalAccount) -> Result<Vec<Article>, crate::error::ViaductError> {
        account.fetch_unread_articles().await
    }
}

pub struct StarredFeedDelegate;
impl SmartFeedDelegate for StarredFeedDelegate {
    async fn fetch_articles(&self, account: &LocalAccount) -> Result<Vec<Article>, crate::error::ViaductError> {
        account.fetch_starred_articles().await
    }
}

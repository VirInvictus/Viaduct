// Network layer for fetching feeds and images
pub mod cache;
pub mod fetcher;

pub use fetcher::{Fetcher, LocalAccountRefresher};

pub fn init_network() {
    // Phase 2: Setup reqwest and Tokio async reactor
}

use chrono::Utc;
use tokio::sync::mpsc;
use viaduct_core::database::accounts::Account;
use viaduct_core::database::articles::SortOrder;
use viaduct_core::database::worker::{
    read_channel, spawn_db_worker, spawn_read_workers, spawn_sync_worker,
};
use viaduct_core::models::ParsedItem;

#[tokio::test]
async fn test_account_update_feed_integration() {
    let (db_tx, db_rx) = mpsc::channel(256);
    spawn_db_worker(db_rx).expect("Failed to spawn db worker");

    let (sync_tx, sync_rx) = mpsc::channel(256);
    spawn_sync_worker(sync_rx).expect("Failed to spawn sync worker");

    // v2.8.0: exercise the read-only connection pool end-to-end. The Sender
    // goes to Account; the reader threads start after Account::new so they
    // open onto an already-initialized articles DB.
    let (read_tx, read_rx) = read_channel();

    // Note: Account::new might try to create XDG directories.
    // In a test environment, we might want to override these,
    // but for now let's hope it works in the CI/Test environment.
    let account = Account::new(db_tx, Some(read_tx), sync_tx)
        .await
        .expect("Failed to create account");

    spawn_read_workers(read_rx).expect("Failed to spawn read workers");

    let feed_id = "test-feed".to_string();
    let items = vec![ParsedItem {
        id: "article-1".to_string(),
        title: Some("Article 1".to_string()),
        content_html: Some("<p>Hello</p>".to_string()),
        content_text: None,
        url: None,
        external_url: None,
        summary: None,
        image_url: None,
        date_published: Some(Utc::now()),
        date_modified: None,
        authors: Vec::new(),
        attachments: Vec::new(),
    }];

    let changes = account
        .update_feed(feed_id.clone(), items, false, 30)
        .await
        .expect("Failed to update feed");

    assert_eq!(changes.new_articles.len(), 1);
    assert_eq!(changes.new_articles[0].title, Some("Article 1".to_string()));

    // v2.8.0: read the just-written article back through the read pool. This
    // crosses the writer→reader boundary — the read-only connection must see
    // the committed write (WAL).
    let fetched = account
        .fetch_articles_by_feed(feed_id.clone(), SortOrder::NewestFirst)
        .await
        .expect("Failed to fetch articles through the read pool");
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].title, Some("Article 1".to_string()));

    // Second update with same item should result in 0 new articles
    let items2 = vec![ParsedItem {
        id: "article-1".to_string(),
        title: Some("Article 1".to_string()),
        content_html: Some("<p>Hello</p>".to_string()),
        content_text: None,
        url: None,
        external_url: None,
        summary: None,
        image_url: None,
        date_published: Some(Utc::now()),
        date_modified: None,
        authors: Vec::new(),
        attachments: Vec::new(),
    }];

    let changes2 = account
        .update_feed(feed_id, items2, false, 30)
        .await
        .expect("Failed to update feed again");
    assert_eq!(changes2.new_articles.len(), 0);
    assert_eq!(changes2.updated_articles.len(), 0);
}

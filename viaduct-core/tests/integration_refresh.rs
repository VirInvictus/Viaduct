use chrono::Utc;
use tokio::sync::mpsc;
use viaduct_core::database::accounts::Account;
use viaduct_core::database::articles::SortOrder;
use viaduct_core::database::worker::{
    read_channel, spawn_db_worker, spawn_read_workers, spawn_sync_worker,
};
use viaduct_core::models::ParsedItem;

/// Route the DBs into a tempdir, the same way `mem_check` does. Without
/// this the test wrote "test-feed" straight into the developer's real
/// `articles.sqlite`, and it only passed at all because that file already
/// existed: nothing here calls `ensure_dirs`, so on a fresh machine (a CI
/// container) the writer thread failed to open the DB and every op came
/// back `WriterGone`.
fn redirect_xdg_to_tempdir() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!("viaduct-itest-{}-{}", std::process::id(), ts));
    // SAFETY: this binary holds exactly one test, so no other thread is
    // reading the environment at this point.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", base.join("data"));
        std::env::set_var("XDG_CACHE_HOME", base.join("cache"));
    }
    viaduct_core::paths::ensure_dirs().expect("Failed to create XDG dirs");
}

#[tokio::test]
async fn test_account_update_feed_integration() {
    redirect_xdg_to_tempdir();

    let (db_tx, db_rx) = mpsc::channel(256);
    spawn_db_worker(db_rx).expect("Failed to spawn db worker");

    let (sync_tx, sync_rx) = mpsc::channel(256);
    spawn_sync_worker(sync_rx).expect("Failed to spawn sync worker");

    // v2.8.0: exercise the read-only connection pool end-to-end. The Sender
    // goes to Account; the reader threads start after Account::new so they
    // open onto an already-initialized articles DB.
    let (read_tx, read_rx) = read_channel();

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

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use viaduct_core::database::accounts::Account;
use viaduct_core::database::worker::{spawn_db_worker, spawn_sync_worker};
use viaduct_core::models::{Feed, FeedSettings};
use viaduct_core::network::fetcher::AccountRefresher;

/// Route the DBs into a fresh tempdir. This file is its own integration
/// binary (separate process from `integration_refresh.rs`), so the env
/// redirect can't race that test's.
fn redirect_xdg_to_tempdir() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base =
        std::env::temp_dir().join(format!("viaduct-ctest-cget-{}-{}", std::process::id(), ts));
    // SAFETY: single test in this binary, single-threaded setup before
    // any worker spawns.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", base.join("data"));
        std::env::set_var("XDG_CACHE_HOME", base.join("cache"));
    }
    viaduct_core::paths::ensure_dirs().expect("Failed to create XDG dirs");
}

/// Tiny in-process HTTP/1.1 server routing two paths: `/bad` serves an
/// HTML page no parser accepts, `/good` serves a valid RSS feed. Both
/// responses carry `ETag: "v2"`. Returns the bound port.
async fn spawn_feed_server() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => return,
            };
            tokio::spawn(async move {
                let mut req = [0u8; 4096];
                let n = sock.read(&mut req).await.unwrap_or(0);
                let is_good = n > 8 && req[..n.min(64)].windows(8).any(|w| w == b"GET /goo");
                let body: &str = if is_good {
                    "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>T</title><link>https://example.test/</link><description>d</description><item><title>A1</title><guid>g1</guid><description>body</description></item></channel></rss>"
                } else {
                    "<html><head><title>Not a feed</title></head><body>challenge page</body></html>"
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nETag: \"v2\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    if is_good {
                        "application/rss+xml"
                    } else {
                        "text/html"
                    },
                    body.len()
                );
                let _ = sock.write_all(header.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Ok(port)
}

fn blank_settings(feed_id: &str, url: &str) -> FeedSettings {
    FeedSettings {
        feed_id: feed_id.to_string(),
        feed_url: url.to_string(),
        home_page_url: None,
        icon_url: None,
        favicon_url: None,
        edited_name: None,
        content_hash: None,
        last_modified: None,
        etag: None,
        date_created: None,
        max_age: None,
        authors_json: None,
        folder_relationship_json: None,
        last_check_date: None,
        reader_view_always_enabled: false,
        new_article_notifications_enabled: false,
        last_response_code: None,
    }
}

fn feed_for(url: &str) -> Feed {
    Feed {
        id: url.to_string(),
        url: url.to_string(),
        name: Some(url.to_string()),
        edited_name: None,
        home_page_url: None,
    }
}

/// NNW `5e33560da`/`491df567a` port: a body that fails to parse must not
/// record its ETag — the old etag is what makes the server re-send the
/// body on the next cycle. A successful fetch records it as before.
#[tokio::test]
async fn failed_parse_does_not_store_conditional_get() {
    redirect_xdg_to_tempdir();

    let (db_tx, db_rx) = mpsc::channel(256);
    spawn_db_worker(db_rx).expect("Failed to spawn db worker");
    let (sync_tx, sync_rx) = mpsc::channel(256);
    spawn_sync_worker(sync_rx).expect("Failed to spawn sync worker");
    let account = std::sync::Arc::new(
        Account::new(db_tx, None, sync_tx)
            .await
            .expect("Failed to create account"),
    );

    let port = spawn_feed_server().await.expect("Failed to bind fixture");

    let (changes_tx, _changes_rx) = mpsc::unbounded_channel();
    let refresher = AccountRefresher::new(account.clone(), changes_tx, 30);

    // 1. Unparseable body: last_response_code is still recorded, but the
    //    ETag must not be, or the next cycle would 304 past content we
    //    never ingested.
    let bad_url = format!("http://127.0.0.1:{port}/bad");
    refresher
        .refresh_feeds(vec![(
            feed_for(&bad_url),
            blank_settings(&bad_url, &bad_url),
        )])
        .await;

    let settings = account
        .fetch_feed_settings(bad_url.clone())
        .await
        .expect("fetch settings")
        .expect("settings exist");
    assert_eq!(settings.last_response_code, Some(200));
    assert_eq!(settings.etag, None, "ETag stored despite failed parse");
    assert_eq!(
        settings.content_hash, None,
        "content hash stored despite failed parse"
    );
    assert!(settings.last_check_date.is_some());

    // 2. Parseable body: the ETag and hash are recorded as before.
    let good_url = format!("http://127.0.0.1:{port}/good");
    refresher
        .refresh_feeds(vec![(
            feed_for(&good_url),
            blank_settings(&good_url, &good_url),
        )])
        .await;

    let settings = account
        .fetch_feed_settings(good_url.clone())
        .await
        .expect("fetch settings")
        .expect("settings exist");
    assert_eq!(settings.etag.as_deref(), Some("\"v2\""));
    assert!(settings.content_hash.is_some());
    assert!(settings.date_created.is_some());

    let articles = account
        .fetch_articles_by_feed(
            good_url.clone(),
            viaduct_core::database::articles::SortOrder::NewestFirst,
        )
        .await
        .expect("fetch articles");
    assert_eq!(articles.len(), 1);
}

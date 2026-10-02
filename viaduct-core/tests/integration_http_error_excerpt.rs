use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use viaduct_core::database::accounts::Account;
use viaduct_core::database::worker::{spawn_db_worker, spawn_sync_worker};
use viaduct_core::models::{Feed, FeedSettings};
use viaduct_core::network::activity::{ActivityKind, ActivityLog};
use viaduct_core::network::fetcher::AccountRefresher;

/// Route the DBs into a fresh tempdir. This file is its own integration
/// binary (separate process from the other integration tests), so the env
/// redirect can't race theirs.
fn redirect_xdg_to_tempdir() {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "viaduct-ctest-excerpt-{}-{}",
        std::process::id(),
        ts
    ));
    // SAFETY: single test in this binary, single-threaded setup before
    // any worker spawns.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", base.join("data"));
        std::env::set_var("XDG_CACHE_HOME", base.join("cache"));
    }
    viaduct_core::paths::ensure_dirs().expect("Failed to create XDG dirs");
}

/// Tiny in-process HTTP/1.1 server with three failing routes: `/err`
/// answers 503 with a whitespace-heavy HTML error page, `/nil` answers
/// 500 with an empty body, and `/big` answers 503 with a body well over
/// the 8 KB excerpt-read cap. Returns the bound port.
async fn spawn_error_server() -> std::io::Result<u16> {
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
                let head = String::from_utf8_lossy(&req[..n.min(64)]).to_string();
                let (status, body): (&str, String) = if head.contains("/nil") {
                    ("500 Internal Server Error", String::new())
                } else if head.contains("/big") {
                    let mut body = String::from("<html><body>OVERFLOW  ");
                    body.push_str(&"x".repeat(20 * 1024));
                    body.push_str("</body></html>");
                    ("503 Service Unavailable", body)
                } else {
                    (
                        "503 Service Unavailable",
                        "<html>\r\n  <head>\r\n    <title>Upstream   error</title>\r\n  </head>\r\n  <body>\r\n    Gateway\t\ttimeout:\r\n    upstream   did   not   answer\r\n  </body>\r\n</html>\r\n".to_string(),
                    )
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        favicon_discovery_at: None,
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

/// NNW `49dbebf67` port: a non-success feed response carries a
/// whitespace-collapsed excerpt of its body into the Activity Log's
/// `HttpError` event; an empty error body carries none. The status
/// code is untouched.
#[tokio::test]
async fn http_error_events_carry_the_collapsed_body_excerpt() {
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

    let port = spawn_error_server().await.expect("Failed to bind fixture");

    let (changes_tx, _changes_rx) = mpsc::unbounded_channel();
    let log = ActivityLog::new();
    let refresher =
        AccountRefresher::new(account.clone(), changes_tx, 30).with_activity_log(log.clone());

    // 1. A 503 with an HTML error page: the excerpt is the collapsed
    //    body, the event status is untouched.
    let err_url = format!("http://127.0.0.1:{port}/err");
    refresher
        .refresh_feeds(vec![(
            feed_for(&err_url),
            blank_settings(&err_url, &err_url),
        )])
        .await;

    let snap = log.snapshot();
    let Some(ActivityKind::HttpError {
        status,
        response_body,
    }) = snap
        .iter()
        .find(|ev| ev.feed_url == err_url)
        .map(|ev| &ev.kind)
    else {
        panic!("no HttpError event for {err_url}");
    };
    assert_eq!(*status, 503);
    assert_eq!(
        response_body.as_deref(),
        Some(
            "<html> <head> <title>Upstream error</title> </head> <body> Gateway timeout: upstream did not answer </body> </html>"
        )
    );

    // The failure still records its response code on the feed.
    let settings = account
        .fetch_feed_settings(err_url.clone())
        .await
        .expect("fetch settings")
        .expect("settings exist");
    assert_eq!(settings.last_response_code, Some(503));

    // 2. An empty error body: the event carries no excerpt.
    let nil_url = format!("http://127.0.0.1:{port}/nil");
    refresher
        .refresh_feeds(vec![(
            feed_for(&nil_url),
            blank_settings(&nil_url, &nil_url),
        )])
        .await;

    let snap = log.snapshot();
    let Some(ActivityKind::HttpError {
        status,
        response_body,
    }) = snap
        .iter()
        .find(|ev| ev.feed_url == nil_url)
        .map(|ev| &ev.kind)
    else {
        panic!("no HttpError event for {nil_url}");
    };
    assert_eq!(*status, 500);
    assert_eq!(response_body.as_deref(), None);

    // 3. An error body well over the 8 KB excerpt-read cap: the status
    //    still surfaces as a completed check (never a network error)
    //    and the excerpt is the collapsed 500-char head of the body.
    let big_url = format!("http://127.0.0.1:{port}/big");
    refresher
        .refresh_feeds(vec![(
            feed_for(&big_url),
            blank_settings(&big_url, &big_url),
        )])
        .await;

    let snap = log.snapshot();
    let Some(ActivityKind::HttpError {
        status,
        response_body,
    }) = snap
        .iter()
        .find(|ev| ev.feed_url == big_url)
        .map(|ev| &ev.kind)
    else {
        panic!("no HttpError event for {big_url}");
    };
    assert_eq!(*status, 503);
    let excerpt = response_body.as_deref().expect("excerpt present");
    assert!(excerpt.starts_with("<html><body>OVERFLOW x"));
    assert_eq!(excerpt.chars().count(), 500);
    let settings = account
        .fetch_feed_settings(big_url.clone())
        .await
        .expect("fetch settings")
        .expect("settings exist");
    assert_eq!(settings.last_response_code, Some(503));
}

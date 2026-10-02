use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use viaduct_core::network::favicon_discovery::discover_favicon;
use viaduct_core::network::feed_discovery::discover_feed;

// NNW `7f004d968` ("Use redirected-to URL"): link resolution and feed
// identity must key on where the response actually came from, not the
// URL we entered. Three pins:
//   1. A URL that redirects straight to a feed discovers as the final
//      feed URL.
//   2. A redirected HTML page resolves its relative `rel=alternate`
//      href against the final page URL.
//   3. Favicon discovery resolves the head-scan candidate against the
//      final home-page URL.
const RSS: &str = "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Redirected Feed</title><link>https://example.test/</link><description>d</description></channel></rss>";

fn html_with_link(head_tags: &str) -> String {
    format!("<html><head>{head_tags}</head><body>page</body></html>")
}

async fn spawn_redirect_server() -> std::io::Result<u16> {
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
                let head = String::from_utf8_lossy(&req[..n.min(8192)]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();

                let (status, headers, body): (&str, String, String) = match path.as_str() {
                    "/hop" => (
                        "301 Moved Permanently",
                        "Location: /final.rss\r\n".to_string(),
                        String::new(),
                    ),
                    "/final.rss" => (
                        "200 OK",
                        "Content-Type: application/rss+xml\r\n".to_string(),
                        RSS.to_string(),
                    ),
                    "/site" => (
                        "301 Moved Permanently",
                        "Location: /en/\r\n".to_string(),
                        String::new(),
                    ),
                    "/en/" => (
                        "200 OK",
                        "Content-Type: text/html\r\n".to_string(),
                        html_with_link(
                            "<link rel=\"alternate\" type=\"application/rss+xml\" href=\"feed.xml\">",
                        ),
                    ),
                    "/en/feed.xml" => (
                        "200 OK",
                        "Content-Type: application/rss+xml\r\n".to_string(),
                        RSS.to_string(),
                    ),
                    "/favhop" => (
                        "301 Moved Permanently",
                        "Location: /fav/\r\n".to_string(),
                        String::new(),
                    ),
                    "/fav/" => (
                        "200 OK",
                        "Content-Type: text/html\r\n".to_string(),
                        html_with_link("<link rel=\"icon\" type=\"image/png\" href=\"icon.png\">"),
                    ),
                    "/fav/icon.png" => (
                        "200 OK",
                        "Content-Type: image/png\r\n".to_string(),
                        "png-not-real-but-nonzero".to_string(),
                    ),
                    _ => ("404 Not Found", String::new(), String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Ok(port)
}

#[tokio::test]
async fn redirected_url_discovers_as_the_final_feed_url() {
    let port = spawn_redirect_server()
        .await
        .expect("Failed to bind fixture");
    let client = viaduct_core::network::http::build_default_client().expect("client");
    let found = discover_feed(&client, &format!("http://127.0.0.1:{port}/hop"))
        .await
        .expect("discovery should succeed");
    assert_eq!(found.feed_url, format!("http://127.0.0.1:{port}/final.rss"));
    assert_eq!(found.title.as_deref(), Some("Redirected Feed"));
}

#[tokio::test]
async fn redirected_page_resolves_relative_alternate_against_final_url() {
    let port = spawn_redirect_server()
        .await
        .expect("Failed to bind fixture");
    let client = viaduct_core::network::http::build_default_client().expect("client");
    let found = discover_feed(&client, &format!("http://127.0.0.1:{port}/site"))
        .await
        .expect("discovery should succeed");
    assert_eq!(
        found.feed_url,
        format!("http://127.0.0.1:{port}/en/feed.xml")
    );
}

#[tokio::test]
async fn redirected_home_page_resolves_favicon_against_final_url() {
    let port = spawn_redirect_server()
        .await
        .expect("Failed to bind fixture");
    let client = viaduct_core::network::http::build_default_client().expect("client");
    let found = discover_favicon(&client, &format!("http://127.0.0.1:{port}/favhop")).await;
    let expected = format!("http://127.0.0.1:{port}/fav/icon.png");
    assert_eq!(found.as_deref(), Some(expected.as_str()));
}

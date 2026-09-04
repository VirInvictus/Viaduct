// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! End-to-end xml:base verification against a real-world Atom feed.
//!
//! The fixture is upstream NetNewsWire's `pappacoda.atom` (an RSParser
//! test resource, MIT), copied byte-for-byte: Andrea Pappacoda's blog
//! feed, the live case behind #5088. Port of upstream's
//! `xmlBaseResolvesRelativeURLsInXHTMLContent`, with assertions mapped
//! to our field shapes: our `ParsedFeed` carries one `icon_url`
//! (with `<icon>` preferred over `<logo>`) where upstream splits
//! icon/favicon across two fields, and `calculate_id` keeps the Atom
//! `<id>` verbatim when present.

use viaduct_core::parser::parse;

const PAPPACODA: &str = include_str!("fixtures/pappacoda.atom");

#[test]
fn pappacoda_xml_base_resolves_xhtml_content_and_links() {
    let feed = parse(
        PAPPACODA.as_bytes(),
        "https://andrea.pappacoda.it/blog/feed.atom",
    )
    .expect("pappacoda.atom parses as Atom");

    // Feed-level xml:lang rides through alongside xml:base.
    assert_eq!(feed.language.as_deref(), Some("it"));

    // Root-relative <icon> and <logo> resolve against the feed-level
    // xml:base. Our single icon_url field prefers <icon>, so this is the
    // author photo; upstream's split fields put the logo in iconURL and
    // the icon in faviconURL.
    assert_eq!(
        feed.icon_url.as_deref(),
        Some("https://andrea.pappacoda.it/andrea_pappacoda.jpg")
    );

    let item = feed
        .items
        .iter()
        .find(|i| i.title.as_deref() == Some("C su Windows"))
        .expect("C su Windows entry");

    // <id> is never resolved, even though it looks nothing like the base.
    assert_eq!(item.id, "tag:andrea.pappacoda.it,2023-06-09:c_su_windows");
    assert_eq!(
        item.url.as_deref(),
        Some("https://andrea.pappacoda.it/blog/c_su_windows/")
    );

    let html = item.content_html.as_deref().expect("xhtml content");
    assert!(html.contains(
        r#"src="https://andrea.pappacoda.it/blog/c_su_windows/c_su_windows_visual_studio_componenti.png""#
    ));
    assert!(html.contains(
        r#"srcset="https://andrea.pappacoda.it/blog/c_su_windows/c_su_windows_visual_studio_componenti_light.png""#
    ));
    assert!(
        html.contains(r#"src="https://andrea.pappacoda.it/blog/c_su_windows/clangd_path.png""#)
    );
    assert!(!html.contains(r#"src="clangd_path.png""#));

    // The xhtml wrapper still comes through verbatim.
    assert!(html.contains(r#"xmlns="http://www.w3.org/1999/xhtml""#));
}

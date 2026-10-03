// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlTagType {
    Link,
    Meta,
}

#[derive(Debug, Clone)]
pub struct HtmlTag {
    pub tag_type: HtmlTagType,
    pub attributes: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct HtmlMetadata {
    pub url_string: String,
    pub tags: Vec<HtmlTag>,
}

/// Extracts `<link>` and `<meta>` tags from an HTML document's head.
///
/// The scan is a hand-rolled HTML tokenizer, not quick-xml: unquoted
/// attribute values (`href=https://…`) are valid HTML5, and quick-xml's
/// attribute parser rejects them outright — a ranchero.com-style page
/// used to yield zero tags, killing feed and favicon discovery on it
/// (the viaduct twin of NNW `d1eaf7676`, whose HTMLScanner had the
/// inverse bug: `/` terminated unquoted values). The scanner follows
/// the HTML5 tokenizer rules instead: an unquoted value ends at
/// whitespace or `>` only, `/` is part of the value, quoted values end
/// at their matching quote, and `<script>`/`<style>` bodies are raw
/// text whose contents never produce tags. Keys keep their source
/// casing; consumers match the conventional lowercase names.
///
/// Stops at `<body>` unless the URL is YouTube (whose player pages
/// carry the interesting links below head), mirroring the previous
/// behavior.
pub fn extract_metadata(data: &[u8], url_string: &str) -> HtmlMetadata {
    let lower = url_string.to_lowercase();
    let scan_past_head = lower.contains("youtube.com") || lower.contains("youtu.be");

    let mut tags = Vec::new();
    let mut pos = 0usize;
    let end = data.len();

    while pos < end {
        if data[pos] != b'<' {
            pos += 1;
            continue;
        }
        if pos + 1 >= end {
            break;
        }
        let next = data[pos + 1];
        if next == b'!' {
            // Comment, CDATA, or doctype: skip verbatim.
            if data[pos..].starts_with(b"<!--") {
                pos = skip_past(data, pos, b"-->");
            } else if data[pos..].starts_with(b"<![CDATA[") {
                pos = skip_past(data, pos, b"]]>");
            } else {
                pos = skip_past(data, pos, b">");
            }
        } else if next == b'?' {
            pos = skip_past(data, pos, b"?>");
        } else if next == b'/' {
            pos = skip_past(data, pos, b">");
        } else if next.is_ascii_alphabetic() {
            let (name, attrs, after, aborted) = scan_start_tag(data, pos);
            pos = after;

            if aborted {
                // The tag's quoted value ran off the end of the input: it
                // is not well-formed, so it emits nothing (mirroring the
                // URL resolver's "replacements recorded so far were
                // well-formed and stay").
                break;
            }

            if name.eq_ignore_ascii_case(b"body") && !scan_past_head {
                break;
            }
            if name.eq_ignore_ascii_case(b"link") {
                if let Some(tag) = link_tag(attrs) {
                    tags.push(tag);
                }
            } else if name.eq_ignore_ascii_case(b"meta") {
                if !attrs.is_empty() {
                    tags.push(HtmlTag {
                        tag_type: HtmlTagType::Meta,
                        attributes: attrs,
                    });
                }
            } else if (name.eq_ignore_ascii_case(b"script") || name.eq_ignore_ascii_case(b"style"))
                && !attrs
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("self-closing"))
            {
                // Raw-text elements: nothing between here and the matching
                // close tag is markup, even if it looks like `<link>`.
                let lower_name: Vec<u8> = name.to_ascii_lowercase();
                pos = skip_raw_text(data, pos, &lower_name);
            }
        } else {
            pos += 1;
        }
    }

    HtmlMetadata {
        url_string: url_string.to_string(),
        tags,
    }
}

/// v4.1.0: cap applied when deriving `content_text` from an item's
/// HTML body at ingest (RSS / Atom items; JSON Feed carries a native
/// `content_text` and never goes through the stripper). 64 KiB of
/// plain text is far past any sort key, search snippet, or preview
/// that consumes the field, while bounding the per-article storage
/// the derivation adds to the database.
pub const MAX_CONTENT_TEXT_CHARS: usize = 65_536;

/// Derives the plain text of an HTML body: tags, comments, doctypes,
/// and `<script>`/`<style>` bodies are dropped; every tag boundary
/// emits one space so adjacent elements cannot fuse words
/// (`<p>Part 1</p><p>Part 2` reads "Part 1 Part 2", not
/// "Part 1Part 2"); whitespace runs collapse to single spaces and the
/// ends trim; output stops at `max_chars` characters. Entities are
/// left as they appear in the input (bodies arriving through
/// quick-xml text events are already entity-decoded; CDATA bodies
/// keep whatever the feed wrote) — stable keys are what the consumers
/// need, not decoded ones.
///
/// The scan follows the same tokenizer rules as `extract_metadata`:
/// quoted/unquoted attribute values, raw-text elements, and malformed
/// input are handled identically, and a tag whose quoted value runs
/// off the end of the input ends the scan with the text collected so
/// far. This is the ingest-time twin of upstream's at-sort-time
/// `ArticleStringFormatter.truncatedSummary` strip (NNW `70c3ec809`):
/// the plain text has to exist in the store for the SQL-level title
/// sort to excerpt it.
pub fn strip_html_to_text(html: &str, max_chars: usize) -> String {
    let data = html.as_bytes();
    let end = data.len();
    let mut out = String::new();
    let mut chars = 0usize;
    let mut pending_space = false;
    let mut pos = 0usize;

    while pos < end {
        if data[pos] == b'<' && pos + 1 < end {
            let next = data[pos + 1];
            let is_tag = next == b'/' || next == b'!' || next == b'?' || next.is_ascii_alphabetic();
            if is_tag {
                if data[pos..].starts_with(b"<!--") {
                    pos = skip_past(data, pos, b"-->");
                } else if next == b'!' || next == b'?' || next == b'/' {
                    // Comment remainder, doctype, processing
                    // instruction, or close tag: all end at `>`.
                    pos = skip_past(data, pos, b">");
                } else {
                    let (name, attrs, after, aborted) = scan_start_tag(data, pos);
                    pos = after;
                    if aborted {
                        break;
                    }
                    if (name.eq_ignore_ascii_case(b"script") || name.eq_ignore_ascii_case(b"style"))
                        && !attrs
                            .iter()
                            .any(|(k, _)| k.eq_ignore_ascii_case("self-closing"))
                    {
                        let lower_name: Vec<u8> = name.to_ascii_lowercase();
                        pos = skip_raw_text(data, pos, &lower_name);
                    }
                }
                pending_space = true;
                continue;
            }
        }

        // Text: ASCII whitespace collapses into a pending space; any
        // other byte is copied through on its char boundary.
        if data[pos].is_ascii_whitespace() {
            pending_space = true;
            pos += 1;
            continue;
        }
        if pending_space && !out.is_empty() {
            if chars >= max_chars {
                break;
            }
            out.push(' ');
            chars += 1;
        }
        pending_space = false;
        if chars >= max_chars {
            break;
        }
        let ch = html[pos..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        chars += 1;
        pos += ch.len_utf8();
    }
    out
}

/// Applies extract_metadata's link filters: a usable link tag has a
/// non-empty `rel` and an `href` or `src`.
fn link_tag(attrs: HashMap<String, String>) -> Option<HtmlTag> {
    let rel = attrs.get("rel")?;
    if rel.is_empty() {
        return None;
    }
    if !(attrs.contains_key("href") || attrs.contains_key("src")) {
        return None;
    }
    Some(HtmlTag {
        tag_type: HtmlTagType::Link,
        attributes: attrs,
    })
}

/// Scans one start tag beginning at `pos` (which points at `<`).
/// Returns the tag name, its attribute map, and the offset just past
/// the tag's `>`. Attribute syntax follows the HTML5 tokenizer: quoted
/// values end at their matching quote, unquoted values end at
/// whitespace or `>` (`/` is part of the value), a valueless attribute
/// records an empty string, and an unterminated quote consumes the
/// rest of the input.
fn scan_start_tag(data: &[u8], start: usize) -> (Vec<u8>, HashMap<String, String>, usize, bool) {
    let end = data.len();
    let mut pos = start + 1;
    let name_start = pos;
    while pos < end && is_tag_name_char(data[pos]) {
        pos += 1;
    }
    let name = data[name_start..pos].to_vec();

    let mut attrs: HashMap<String, String> = HashMap::new();
    let mut aborted = false;
    while pos < end {
        while pos < end && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= end {
            break;
        }
        let b = data[pos];
        if b == b'>' {
            pos += 1;
            break;
        }
        if b == b'/' {
            if pos + 1 < end && data[pos + 1] == b'>' {
                pos += 2;
                break;
            }
            pos += 1;
            continue;
        }

        // Attribute name.
        let name_start = pos;
        while pos < end {
            let b = data[pos];
            if b.is_ascii_whitespace() || b == b'=' || b == b'>' || b == b'/' {
                break;
            }
            pos += 1;
        }
        let attr_name = String::from_utf8_lossy(&data[name_start..pos]).into_owned();

        while pos < end && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= end || data[pos] != b'=' {
            // Valueless attribute: quick-xml recorded these with an
            // empty value, so the filter behavior is unchanged.
            if !attr_name.is_empty() {
                attrs.insert(attr_name, String::new());
            }
            continue;
        }
        pos += 1;
        while pos < end && data[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= end {
            if !attr_name.is_empty() {
                attrs.insert(attr_name, String::new());
            }
            break;
        }

        let quote = data[pos];
        let value;
        if quote == b'"' || quote == b'\'' {
            pos += 1;
            let value_start = pos;
            while pos < end && data[pos] != quote {
                pos += 1;
            }
            value = String::from_utf8_lossy(&data[value_start..pos]).into_owned();
            if pos < end {
                pos += 1; // past the closing quote
            } else {
                // Unterminated quote: the value runs to EOF and there is
                // nothing left to scan.
                aborted = true;
            }
        } else {
            let value_start = pos;
            while pos < end && !data[pos].is_ascii_whitespace() && data[pos] != b'>' {
                pos += 1;
            }
            value = String::from_utf8_lossy(&data[value_start..pos]).into_owned();
        }
        if !attr_name.is_empty() {
            attrs.insert(attr_name, value);
        }
        if aborted {
            break;
        }
    }
    (name, attrs, pos, aborted)
}

/// Skips a `<script>`/`<style>` body: raw text until the matching
/// case-insensitive close tag, then past its `>`. Keeps `</scripty>`
/// from being mistaken for `</script>`.
fn skip_raw_text(data: &[u8], mut pos: usize, close_tag_name: &[u8]) -> usize {
    let end = data.len();
    while pos < end {
        if data[pos] == b'<' && pos + 1 < end && data[pos + 1] == b'/' {
            let name_start = pos + 2;
            let name_end = name_start + close_tag_name.len();
            if name_end <= end
                && data[name_start..name_end].eq_ignore_ascii_case(close_tag_name)
                && match data.get(name_end) {
                    None => true,
                    Some(b) => *b == b'>' || *b == b'/' || b.is_ascii_whitespace(),
                }
            {
                return skip_past(data, name_end, b">");
            }
        }
        pos += 1;
    }
    end
}

fn skip_past(data: &[u8], mut pos: usize, literal: &[u8]) -> usize {
    let end = data.len();
    while pos + literal.len() <= end {
        if &data[pos..pos + literal.len()] == literal {
            return pos + literal.len();
        }
        pos += 1;
    }
    end
}

fn is_tag_name_char(b: u8) -> bool {
    b == b':' || b == b'_' || b == b'-' || b == b'.' || b.is_ascii_alphanumeric() || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NNW `d1eaf7676`'s ranchero.com case, 1:1: unquoted attribute
    /// values are valid HTML5, and a `/` is part of the value. Under the
    /// previous quick-xml implementation this page yielded ZERO tags —
    /// the unquoted attributes rejected every attribute of every tag —
    /// so neither feed nor favicon discovery could see it.
    #[test]
    fn unquoted_attribute_values_discover_ranchero_feeds() {
        let html = b"<html><head>\n<link rel=alternate type=application/rss+xml title=\"ranchero.com RSS feed\" href=https://ranchero.com/xml/rss.xml>\n<link rel=alternate type=application/json title=\"ranchero.com JSON feed\" href=https://ranchero.com/feed.json>\n</head><body></body></html>";
        let metadata = extract_metadata(html, "https://ranchero.com/");

        let feed_links: Vec<&HtmlTag> = metadata
            .tags
            .iter()
            .filter(|t| t.tag_type == HtmlTagType::Link)
            .collect();
        assert_eq!(feed_links.len(), 2);

        let rss = feed_links
            .iter()
            .find(|t| t.attributes.get("type").map(String::as_str) == Some("application/rss+xml"))
            .expect("rss link");
        assert_eq!(
            rss.attributes.get("href").map(String::as_str),
            Some("https://ranchero.com/xml/rss.xml"),
            "the unquoted href keeps its slashes"
        );
        assert_eq!(
            rss.attributes.get("title").map(String::as_str),
            Some("ranchero.com RSS feed"),
            "the quoted title keeps its spaces"
        );
    }

    /// Quoted-only input keeps working exactly as before the scanner
    /// swap (favicon_discovery's shape).
    #[test]
    fn quoted_attributes_parse_as_before() {
        let html = b"<html><head><link rel=\"icon\" href=\"/x.png\" />\n<meta name=\"generator\" content=\"hand\"></head><body></body></html>";
        let metadata = extract_metadata(html, "https://example.com/");
        assert_eq!(metadata.tags.len(), 2);
        let link = &metadata.tags[0];
        assert_eq!(link.tag_type, HtmlTagType::Link);
        assert_eq!(link.attributes.get("rel").map(String::as_str), Some("icon"));
        assert_eq!(
            link.attributes.get("href").map(String::as_str),
            Some("/x.png")
        );
        let meta = &metadata.tags[1];
        assert_eq!(meta.tag_type, HtmlTagType::Meta);
        assert_eq!(
            meta.attributes.get("content").map(String::as_str),
            Some("hand")
        );
    }

    /// The existing filters hold: a link needs a non-empty `rel` plus an
    /// `href` or `src`; valueless attributes record empty strings
    /// (quick-xml's shape) without breaking the tag.
    #[test]
    fn link_filters_and_valueless_attributes() {
        let html = b"<head>\n<link rel=\"\">\n<link type=\"application/rss+xml\">\n<link rel=\"alternate\" href=\"/feed.xml\" data-foo>\n</head>";
        let metadata = extract_metadata(html, "https://example.com/");
        let links: Vec<&HtmlTag> = metadata
            .tags
            .iter()
            .filter(|t| t.tag_type == HtmlTagType::Link)
            .collect();
        assert_eq!(links.len(), 1, "empty rel and href-less links are skipped");
        assert_eq!(
            links[0].attributes.get("href").map(String::as_str),
            Some("/feed.xml")
        );
        assert_eq!(
            links[0].attributes.get("data-foo").map(String::as_str),
            Some(""),
            "valueless attributes record an empty value"
        );
    }

    /// The scan stops at `<body>` for ordinary URLs, but YouTube pages
    /// keep scanning past it.
    #[test]
    fn body_stop_respects_the_youtube_exception() {
        let tail =
            b"<body><link rel=\"alternate\" href=\"https://after.example/feed.xml\"></body></html>";
        let head = b"<html><head><title>t</title></head>";
        let mut html = head.to_vec();
        html.extend_from_slice(tail);

        let ordinary = extract_metadata(&html, "https://example.com/");
        assert!(
            ordinary.tags.is_empty(),
            "links below <body> are not scanned"
        );

        let youtube = extract_metadata(&html, "https://www.youtube.com/watch?v=x");
        assert_eq!(youtube.tags.len(), 1, "YouTube scans past <body>");
    }

    /// Script and style bodies are raw text: a `<link>`-shaped string
    /// inside them produces no tag.
    #[test]
    fn script_and_style_bodies_produce_no_tags() {
        let html = b"<head>\n<script>var x = \"<link rel=alternate href=https://evil.example/x>\";</script>\n<style>a {{ content: \"<link rel=alternate href=https://evil.example/y>\" }}</style>\n<link rel=\"alternate\" href=\"https://real.example/feed.xml\">\n</head>";
        let metadata = extract_metadata(html, "https://example.com/");
        let links: Vec<&HtmlTag> = metadata
            .tags
            .iter()
            .filter(|t| t.tag_type == HtmlTagType::Link)
            .collect();
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].attributes.get("href").map(String::as_str),
            Some("https://real.example/feed.xml")
        );
    }

    /// An unterminated quoted value consumes the rest of the input
    /// without panicking; tags recorded before it survive.
    #[test]
    fn unterminated_quote_ends_the_scan_gracefully() {
        let html = b"<head><link rel=\"alternate\" href=\"https://good.example/feed.xml\"><link rel=\"alternate\" href=\"broken";
        let metadata = extract_metadata(html, "https://example.com/");
        let links: Vec<&HtmlTag> = metadata
            .tags
            .iter()
            .filter(|t| t.tag_type == HtmlTagType::Link)
            .collect();
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].attributes.get("href").map(String::as_str),
            Some("https://good.example/feed.xml")
        );
    }

    /// Comments, CDATA, and doctypes never produce tags.
    #[test]
    fn comments_cdata_and_doctype_are_skipped() {
        let html = b"<!DOCTYPE html>\n<!-- <link rel=alternate href=https://comment.example/x> -->\n<![CDATA[<link rel=alternate href=https://cdata.example/x>]]>\n<head><link rel=\"alternate\" href=\"https://real.example/feed.xml\"></head>";
        let metadata = extract_metadata(html, "https://example.com/");
        assert_eq!(metadata.tags.len(), 1);
        assert_eq!(
            metadata.tags[0].attributes.get("href").map(String::as_str),
            Some("https://real.example/feed.xml")
        );
    }

    // ---- v4.1.0: strip_html_to_text (ingest-time content_text) ----

    #[test]
    fn strips_tags_and_collapses_whitespace() {
        assert_eq!(
            strip_html_to_text("  <p>Hello <b>cruel</b> world.</p>\n\n<p>Second.</p>", 4096),
            "Hello cruel world. Second."
        );
    }

    #[test]
    fn tag_boundaries_never_fuse_words() {
        // Two adjacent block elements with no whitespace between them:
        // the tag boundary emits exactly one space, like upstream's
        // strip-with-space rather than strip-with-nothing.
        assert_eq!(
            strip_html_to_text("<p>Part 1</p><p>Part 2</p>", 4096),
            "Part 1 Part 2"
        );
        // Inline tags do not double up spaces with surrounding text.
        assert_eq!(strip_html_to_text("a <em>b</em> c", 4096), "a b c");
    }

    #[test]
    fn script_style_and_comment_bodies_are_dropped() {
        assert_eq!(
            strip_html_to_text(
                "<p>keep</p><script>var drop = \"this\";</script><style>.drop { x: y }</style><p>also</p>",
                4096
            ),
            "keep also"
        );
        assert_eq!(
            strip_html_to_text("a<!-- <b>gone</b> -->b", 4096),
            "a b",
            "comment bodies never leak and their boundary spaces collapse"
        );
    }

    #[test]
    fn text_like_markup_outside_tags_survives() {
        // A `<` that does not open a tag is text (the "a < b" shape);
        // entities are left exactly as the feed wrote them.
        assert_eq!(strip_html_to_text("a &lt; b", 4096), "a &lt; b");
        assert_eq!(
            strip_html_to_text("1 < 2 and 3 > 2", 4096),
            "1 < 2 and 3 > 2"
        );
    }

    #[test]
    fn output_stops_at_the_character_cap() {
        let html = "<p>".repeat(10) + &"x".repeat(100);
        let out = strip_html_to_text(&html, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.chars().all(|c| c == 'x'));

        // The cap counts characters, not bytes: multibyte text cuts on
        // a char boundary.
        let multibyte = "<p>".to_string() + &"é".repeat(50);
        let out = strip_html_to_text(&multibyte, 10);
        assert_eq!(out, "é".repeat(10));
    }

    #[test]
    fn empty_and_malformed_inputs_degrade_cleanly() {
        assert_eq!(strip_html_to_text("", 4096), "");
        assert_eq!(strip_html_to_text("<p></p>", 4096), "");
        // An unterminated tag ends the scan; text before it survives.
        assert_eq!(strip_html_to_text("keep <p class=\"broken", 4096), "keep");
        // Unterminated raw-text element swallows the rest.
        assert_eq!(
            strip_html_to_text("keep <script>gone forever", 4096),
            "keep"
        );
    }
}

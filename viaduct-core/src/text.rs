// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Article-title sanitization. Port of NNW
//! `Shared/Extensions/ArticleStringFormatter.sanitizedTitle` (commits
//! 3eac947dd, de464b6a3): a title may carry inline HTML, and the
//! detail pane wants the harmless subset rendered (`<em>`, `<b>`,
//! `<abbr>`) while everything else is neutralized.
//!
//! A tag is allowed or disallowed by its name alone (the leading run
//! of ASCII letters after any `/`); attributes are ignored, so
//! `<abbr title="…">` is recognized as `abbr`.
//!
//! Behavior per (allowed, for_html) combination:
//! - allowed tag, for_html=true: tag preserved (`<b>Bold</b>`).
//! - allowed tag, for_html=false: tag dropped, contents kept.
//! - disallowed tag, for_html=true: tag escaped as `&lt;...&gt;`.
//! - disallowed tag, for_html=false: tag preserved literally.
//!
//! Single pass over UTF-8 bytes; non-ASCII bytes pass through
//! unchanged. An unclosed tag never gains a synthesized `>` (NNW
//! issue #4742: a title like `<16s in UK…` must not grow one).

/// The NNW allowlist: inline, presentation-only tags.
const ALLOWED_TAGS: &[&str] = &[
    "abbr", "b", "bdi", "bdo", "cite", "code", "del", "dfn", "em", "i", "ins", "kbd", "mark", "q",
    "s", "samp", "small", "strong", "sub", "sup", "time", "u", "var",
];

fn is_ascii_letter(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_uppercase()
}

fn tag_is_allowed(name: &[u8]) -> bool {
    // Exact byte match, matching NNW's Set<[UInt8]> lookup: the
    // allowlist is lowercase and `<EM>` does not match it. Ported
    // as-is rather than "fixed" to ASCII-case-insensitive.
    ALLOWED_TAGS.iter().any(|t| t.as_bytes() == name)
}

/// Sanitize a title that may contain HTML. See the module docs for
/// the behavior matrix.
pub fn sanitized_title(title: &str, for_html: bool) -> String {
    let utf8 = title.as_bytes();
    let count = utf8.len();
    let mut out: Vec<u8> = Vec::with_capacity(count);

    const LT: u8 = b'<';
    const GT: u8 = b'>';
    const SLASH: u8 = b'/';
    const LT_ENTITY: &[u8] = b"&lt;";
    const GT_ENTITY: &[u8] = b"&gt;";

    let mut i = 0;
    while i < count {
        let b = utf8[i];
        if b != LT {
            out.push(b);
            i += 1;
            continue;
        }

        // Scan forward for `>` or end of input.
        let mut j = i + 1;
        while j < count && utf8[j] != GT {
            j += 1;
        }

        // Empty tag body: emit nothing, skip the `<`, let the next
        // iteration handle any trailing `>` as literal text.
        let tag_start = i + 1;
        let tag_end = j;
        if tag_start == tag_end {
            i += 1;
            continue;
        }

        // Extract the tag name: skip a leading `/` (closing tags),
        // then take the run of ASCII letters. Attributes are ignored.
        let mut name_start = tag_start;
        if name_start < tag_end && utf8[name_start] == SLASH {
            name_start += 1;
        }
        let mut name_end = name_start;
        while name_end < tag_end && is_ascii_letter(utf8[name_end]) {
            name_end += 1;
        }
        let is_allowed = tag_is_allowed(&utf8[name_start..name_end]);

        // Only emit a closing `>` if the input actually had one. An
        // unclosed tag must not gain a synthesized `>` (NNW #4742).
        let tag_was_closed = j < count;

        if is_allowed {
            if for_html {
                out.push(LT);
                out.extend_from_slice(&utf8[tag_start..tag_end]);
                if tag_was_closed {
                    out.push(GT);
                }
            }
        } else if for_html {
            out.extend_from_slice(LT_ENTITY);
            out.extend_from_slice(&utf8[tag_start..tag_end]);
            if tag_was_closed {
                out.extend_from_slice(GT_ENTITY);
            }
        } else {
            out.push(LT);
            out.extend_from_slice(&utf8[tag_start..tag_end]);
            if tag_was_closed {
                out.push(GT);
            }
        }

        i = if tag_was_closed { j + 1 } else { count };
    }

    // Byte-level surgery only ever copies whole input bytes or
    // inserts ASCII entities, so the output is valid UTF-8.
    String::from_utf8(out).expect("sanitized output is valid UTF-8")
}

/// HTML for a plain-text article body (NNW `d795983c9`,
/// `RSCore.String.convertingPlainTextToHTML`): escaped, blank lines as
/// paragraph breaks, other line breaks as `<br>`, and URLs as links.
/// Feeds whose items carry only plain text (no `content_html`) would
/// otherwise inject raw text into the HTML pipeline, unescaped and
/// unlinked.
///
/// Recorded divergence: upstream linkifies with `NSDataDetector` (ICU,
/// finds bare `www.` hosts and trailing punctuation intelligently);
/// we scan for `http://`, `https://`, and `mailto:` runs by hand (the
/// no-regex discipline) and trim common trailing punctuation from a
/// run. Same no-new-dependency trade the other scanners make.
pub fn plain_text_to_html(text: &str) -> String {
    fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                _ => out.push(c),
            }
        }
        out
    }

    /// Length of a URL starting at `i` (`http(s)://…` / `mailto:…`),
    /// stopping at whitespace or a closing bracket/quote. Trailing
    /// sentence punctuation (`.,;:!?)`]` + closing quotes) is left out
    /// of the run so "see https://x.example/a." links the address.
    fn url_run_len(bytes: &[u8], i: usize) -> Option<usize> {
        let rest = &bytes[i..];
        let starts = rest.starts_with(b"http://")
            || rest.starts_with(b"https://")
            || rest.starts_with(b"mailto:");
        if !starts {
            return None;
        }
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if b.is_ascii_whitespace()
                || matches!(b, b'<' | b'>' | b'"' | b'\'' | b'`' | b')' | b']' | b'}')
            {
                break;
            }
            j += 1;
        }
        // Trim trailing sentence punctuation off the run end.
        while j > i
            && matches!(
                bytes[j - 1],
                b'.' | b',' | b';' | b':' | b'!' | b'?' | b'"' | b'\'' | b'`'
            )
        {
            j -= 1;
        }
        if j <= i { None } else { Some(j - i) }
    }

    /// One line, escaped, with URL runs wrapped in anchors. The href
    /// and the text are both escaped (the href attribute needs quotes
    /// escaped too, which `escape` covers).
    fn linkified_escaped_line(line: &str) -> String {
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut i = 0usize;
        while i < line.len() {
            if let Some(len) = url_run_len(bytes, i) {
                let url = &line[i..i + len];
                out.push_str("<a href=\"");
                out.push_str(&escape(url));
                out.push_str("\">");
                out.push_str(&escape(url));
                out.push_str("</a>");
                i += len;
                continue;
            }
            // Copy one char (keeps UTF-8 whole; URL starts are ASCII,
            // so a non-match position just walks one char forward).
            let (off, c) = line[i..]
                .char_indices()
                .next()
                .map(|(off, c)| (i + off, c))
                .expect("i is a char boundary");
            let end = off + c.len_utf8();
            out.push_str(&escape(&line[off..end]));
            i = end;
        }
        out
    }

    let normalized = text.replace("\r\n", "\n");
    let mut paragraphs: Vec<String> = Vec::new();
    let mut paragraph_lines: Vec<String> = Vec::new();
    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            if !paragraph_lines.is_empty() {
                paragraphs.push(format!("<p>{}</p>", paragraph_lines.join("<br>\n")));
                paragraph_lines.clear();
            }
        } else {
            paragraph_lines.push(linkified_escaped_line(line));
        }
    }
    if !paragraph_lines.is_empty() {
        paragraphs.push(format!("<p>{}</p>", paragraph_lines.join("<br>\n")));
    }
    paragraphs.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_makes_paragraphs_and_brs() {
        // NNW d795983c9 shape: blank lines split paragraphs; single
        // newlines become <br>. CRLF normalizes first.
        assert_eq!(
            plain_text_to_html("one\ntwo\n\nthree"),
            "<p>one<br>\ntwo</p>\n<p>three</p>"
        );
        assert_eq!(
            plain_text_to_html("one\r\ntwo\r\n\r\nthree"),
            "<p>one<br>\ntwo</p>\n<p>three</p>"
        );
        // Leading/trailing blank lines produce no empty paragraphs.
        assert_eq!(plain_text_to_html("\n\nhello\n\n"), "<p>hello</p>");
        assert_eq!(plain_text_to_html(""), "");
    }

    #[test]
    fn plain_text_escapes_html() {
        assert_eq!(
            plain_text_to_html("a < b & c > d \"quoted\""),
            "<p>a &lt; b &amp; c &gt; d &quot;quoted&quot;</p>"
        );
    }

    #[test]
    fn plain_text_linkifies_urls_with_safe_trailing_punctuation() {
        // The address links; the sentence period stays outside.
        assert_eq!(
            plain_text_to_html("see https://example.com/a."),
            "<p>see <a href=\"https://example.com/a\">https://example.com/a</a>.</p>"
        );
        assert_eq!(
            plain_text_to_html("http://example.com/x and mailto:me@example.com"),
            "<p><a href=\"http://example.com/x\">http://example.com/x</a> and \
             <a href=\"mailto:me@example.com\">mailto:me@example.com</a></p>"
        );
        // A URL in parentheses: the closing paren ends the run.
        assert_eq!(
            plain_text_to_html("(https://example.com/y)"),
            "<p>(<a href=\"https://example.com/y\">https://example.com/y</a>)</p>"
        );
        // An ampersand inside the URL is escaped in both href and text.
        assert_eq!(
            plain_text_to_html("https://example.com/q?a=1&b=2"),
            "<p><a href=\"https://example.com/q?a=1&amp;b=2\">https://example.com/q?a=1&amp;b=2</a></p>"
        );
        // Bare hosts (no scheme) stay plain: the scanner's documented
        // divergence from NSDataDetector.
        assert_eq!(
            plain_text_to_html("visit example.com today"),
            "<p>visit example.com today</p>"
        );
    }

    #[test]
    fn plain_text_preserves_multibyte_text() {
        assert_eq!(
            plain_text_to_html("こんにちは世界"),
            "<p>こんにちは世界</p>"
        );
    }

    #[test]
    fn plain_title_passes_through() {
        assert_eq!(sanitized_title("A plain title", true), "A plain title");
        assert_eq!(sanitized_title("A plain title", false), "A plain title");
        assert_eq!(sanitized_title("", true), "");
    }

    #[test]
    fn allowed_tag_preserved_for_html() {
        assert_eq!(
            sanitized_title("<em>Word</em> up", true),
            "<em>Word</em> up"
        );
        assert_eq!(sanitized_title("<b>Bold</b>", true), "<b>Bold</b>");
    }

    #[test]
    fn allowed_tag_dropped_for_plain_text() {
        assert_eq!(sanitized_title("<em>Word</em> up", false), "Word up");
        assert_eq!(sanitized_title("<b>Bold</b>", false), "Bold");
    }

    #[test]
    fn disallowed_tag_escaped_for_html() {
        assert_eq!(
            sanitized_title("<script>x</script>", true),
            "&lt;script&gt;x&lt;/script&gt;"
        );
        assert_eq!(
            sanitized_title("a <div>b</div>", true),
            "a &lt;div&gt;b&lt;/div&gt;"
        );
    }

    #[test]
    fn disallowed_tag_preserved_literally_for_plain_text() {
        assert_eq!(sanitized_title("<div>b</div>", false), "<div>b</div>");
    }

    #[test]
    fn attributes_are_ignored_for_matching() {
        // <abbr title="…"> is recognized as abbr, and the preserved
        // form keeps its attributes verbatim.
        assert_eq!(
            sanitized_title("<abbr title=\"HyperText\">HT</abbr>", true),
            "<abbr title=\"HyperText\">HT</abbr>"
        );
        // Disallowed with attributes: whole body escaped.
        assert_eq!(
            sanitized_title("<a href=\"x\">y</a>", true),
            "&lt;a href=\"x\"&gt;y&lt;/a&gt;"
        );
    }

    #[test]
    fn unclosed_tag_gains_no_synthesized_gt() {
        // NNW #4742: `<16s in UK` must not grow a trailing `>`.
        assert_eq!(sanitized_title("<16s in UK", true), "&lt;16s in UK");
        assert_eq!(sanitized_title("<16s in UK", false), "<16s in UK");
        // Unclosed allowed tag, for_html: preserved without a `>`.
        assert_eq!(sanitized_title("before <em", true), "before <em");
        // Unclosed allowed tag, plain text: dropped entirely.
        assert_eq!(sanitized_title("before <em", false), "before ");
    }

    #[test]
    fn empty_tag_body_emits_nothing_and_keeps_trailing_gt() {
        // `<>`: the `<` is skipped, the `>` is next-iteration literal.
        assert_eq!(sanitized_title("a<>b", true), "a>b");
    }

    #[test]
    fn tag_matching_is_case_sensitive_like_nnw() {
        // NNW's byte-set lookup is exact: `<EM>` is not in the
        // lowercase allowlist, so it is treated as disallowed.
        // Deliberately ported as-is (match the weird edges).
        assert_eq!(
            sanitized_title("<EM>x</EM>", true),
            "&lt;EM&gt;x&lt;/EM&gt;"
        );
        assert_eq!(sanitized_title("<EM>x</EM>", false), "<EM>x</EM>");
    }

    #[test]
    fn non_ascii_content_passes_through() {
        assert_eq!(
            sanitized_title("Curiosité <em>déçue</em>", true),
            "Curiosité <em>déçue</em>"
        );
        assert_eq!(
            sanitized_title("Curiosité <em>déçue</em>", false),
            "Curiosité déçue"
        );
    }
}

// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Resolves relative URLs in HTML attribute values (`href`, `src`,
//! `poster`, `srcset`) against a base URL — used for Atom `xml:base`
//! support. Byte-level port of NNW `HTMLRelativeURLResolver`
//! (`1d54877be`, #5088).
//!
//! Replacement is surgical: everything outside a rewritten attribute
//! value is byte-identical to the input. Values are left alone when they
//! are empty, fragment-only (`#…` — same-document references), or
//! already have a scheme (`http:`, `data:`, `mailto:`, and so on).
//! Protocol-relative values (`//…`) do get resolved, taking the base
//! URL's scheme.
//!
//! Attribute values are not entity-decoded — the `url` crate passes
//! characters like `&` through untouched, so an `&amp;` in a relative
//! URL survives resolution byte-for-byte.
//!
//! Known divergences from the Swift original: Foundation's
//! `URL(string:relativeTo:)` and the `url` crate differ in strictness
//! and normalization (the crate percent-encodes garbage bytes where
//! Foundation rejects the URL outright; percent-encoding case and
//! dot-segment resolution can differ), so a rewritten value may differ
//! in encoding detail while resolving to the same URL. The
//! scan-and-replace structure, the attribute list, the value
//! classification, and the raw-text skips are ported 1:1.

/// Resolves relative URLs in `html` against `base`. Returns the input
/// unchanged when nothing needs resolving.
pub fn resolving_relative_urls(html: &str, base: &url::Url) -> String {
    let bytes = html.as_bytes();
    match rewritten_bytes(bytes, base) {
        Some(rewritten) => String::from_utf8(rewritten).unwrap_or_else(|_| html.to_string()),
        None => html.to_string(),
    }
}

/// Returns `None` when nothing needs resolving, so callers can keep the
/// input verbatim.
fn rewritten_bytes(bytes: &[u8], base: &url::Url) -> Option<Vec<u8>> {
    let scanner = UrlReplacementScanner::new(bytes, base);
    let replacements = scanner.scan();
    if replacements.is_empty() {
        return None;
    }

    let size_delta: usize = replacements
        .iter()
        .map(|r| r.bytes.len().saturating_sub(r.end - r.start))
        .sum();
    let mut result = Vec::with_capacity(bytes.len() + size_delta);

    let mut copy_start = 0usize;
    for replacement in replacements {
        result.extend_from_slice(&bytes[copy_start..replacement.start]);
        result.extend_from_slice(&replacement.bytes);
        copy_start = replacement.end;
    }
    result.extend_from_slice(&bytes[copy_start..]);
    Some(result)
}

struct Replacement {
    start: usize,
    end: usize,
    bytes: Vec<u8>,
}

/// Scans HTML bytes and collects the URL attribute-value replacements to
/// make.
struct UrlReplacementScanner<'a> {
    bytes: &'a [u8],
    base_url: &'a url::Url,
    pos: usize,
    replacements: Vec<Replacement>,
}

impl<'a> UrlReplacementScanner<'a> {
    fn new(bytes: &'a [u8], base_url: &'a url::Url) -> Self {
        Self {
            bytes,
            base_url,
            pos: 0,
            replacements: Vec::new(),
        }
    }

    fn scan(mut self) -> Vec<Replacement> {
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'<' {
                self.handle_less_than();
            } else {
                self.pos += 1;
            }
        }
        self.replacements
    }

    // MARK: Markup dispatch

    fn handle_less_than(&mut self) {
        if self.pos + 1 >= self.bytes.len() {
            self.pos = self.bytes.len();
            return;
        }

        let next = self.bytes[self.pos + 1];
        if next == b'!' {
            self.handle_declaration();
        } else if next == b'?' {
            self.skip_past(b"?>");
        } else if next == b'/' {
            self.skip_past_greater_than();
        } else if next.is_ascii_alphabetic() {
            self.scan_start_tag();
        } else {
            self.pos += 1;
        }
    }

    fn handle_declaration(&mut self) {
        if self.matches(b"<!--") {
            self.skip_past(b"-->");
        } else if self.matches(b"<![CDATA[") {
            self.skip_past(b"]]>");
        } else {
            self.skip_past_greater_than();
        }
    }

    // MARK: Start tags

    fn scan_start_tag(&mut self) {
        let name_start = self.pos + 1;
        let mut i = name_start;
        while i < self.bytes.len() && self.bytes[i].is_xml_name_char() {
            i += 1;
        }
        let tag_name = &self.bytes[name_start..i];
        self.pos = i;

        let mut self_closing = false;
        while self.pos < self.bytes.len() {
            self.skip_whitespace();
            if self.pos >= self.bytes.len() {
                return;
            }
            let b = self.bytes[self.pos];
            if b == b'>' {
                self.pos += 1;
                break;
            }
            if b == b'/' {
                if self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'>' {
                    self_closing = true;
                    self.pos += 2;
                    break;
                }
                self.pos += 1;
                continue;
            }
            if b.is_xml_name_start() {
                self.scan_attribute();
            } else {
                self.pos += 1;
            }
        }

        // Nothing inside script or style elements is a URL to resolve.
        if !self_closing {
            if tag_name.eq_ascii_case_insensitive(b"script") {
                self.skip_raw_text(b"script");
            } else if tag_name.eq_ascii_case_insensitive(b"style") {
                self.skip_raw_text(b"style");
            }
        }
    }

    fn scan_attribute(&mut self) {
        let name_start = self.pos;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_ws() || b == b'=' || b == b'>' || b == b'/' {
                break;
            }
            self.pos += 1;
        }
        let name = &self.bytes[name_start..self.pos];

        self.skip_whitespace();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'=' {
            return; // valueless attribute
        }
        self.pos += 1;
        self.skip_whitespace();
        if self.pos >= self.bytes.len() {
            return;
        }

        let value_start: usize;
        let value_end: usize;
        let quote = self.bytes[self.pos];
        if quote == b'"' || quote == b'\'' {
            let start = self.pos + 1;
            let mut i = start;
            while i < self.bytes.len() && self.bytes[i] != quote {
                i += 1;
            }
            if i >= self.bytes.len() {
                // Unterminated quote — abandon the rest of the scan.
                // Replacements recorded so far were well-formed and stay.
                self.pos = self.bytes.len();
                return;
            }
            value_start = start;
            value_end = i;
            self.pos = i + 1;
        } else {
            // Unquoted value: ends at whitespace or `>` (the WHATWG rule).
            // Deliberately not at `/`, so href=/foo/bar isn't truncated.
            let start = self.pos;
            let mut i = self.pos;
            while i < self.bytes.len() && !self.bytes[i].is_ascii_ws() && self.bytes[i] != b'>' {
                i += 1;
            }
            value_start = start;
            value_end = i;
            self.pos = i;
        }

        self.handle_attribute_value(name, value_start, value_end);
    }

    fn handle_attribute_value(&mut self, name: &[u8], value_start: usize, value_end: usize) {
        if name.eq_ascii_case_insensitive(b"href")
            || name.eq_ascii_case_insensitive(b"src")
            || name.eq_ascii_case_insensitive(b"poster")
        {
            self.add_replacement_if_needed(value_start, value_end);
        } else if name.eq_ascii_case_insensitive(b"srcset") {
            self.handle_srcset_value(value_start, value_end);
        }
    }

    // MARK: srcset

    fn handle_srcset_value(&mut self, value_start: usize, value_end: usize) {
        let mut i = value_start;

        while i < value_end {
            while i < value_end && (self.bytes[i].is_ascii_ws() || self.bytes[i] == b',') {
                i += 1;
            }
            if i >= value_end {
                return;
            }

            // URL token: up to whitespace. Trailing commas are separators,
            // not URL.
            let url_start = i;
            while i < value_end && !self.bytes[i].is_ascii_ws() {
                i += 1;
            }
            let mut url_end = i;
            while url_end > url_start && self.bytes[url_end - 1] == b',' {
                url_end -= 1;
            }
            let token_ended_with_comma = url_end < i;

            if url_end > url_start {
                self.add_replacement_if_needed(url_start, url_end);
            }

            // A trailing comma already ended this candidate. Otherwise skip
            // the descriptor — everything up to the next comma.
            if !token_ended_with_comma {
                while i < value_end && self.bytes[i] != b',' {
                    i += 1;
                }
            }
        }
    }

    // MARK: URL classification and resolution

    fn add_replacement_if_needed(&mut self, value_start: usize, value_end: usize) {
        // Browsers strip surrounding whitespace before processing a URL
        // value. Classify and replace only the trimmed part — the
        // whitespace stays.
        let (start, end) = self.trim_ascii_whitespace(value_start, value_end);
        if start >= end {
            return;
        }
        if let Some(resolved) = self.resolved_url_bytes(start, end) {
            self.replacements.push(Replacement {
                start,
                end,
                bytes: resolved,
            });
        }
    }

    fn trim_ascii_whitespace(&self, start: usize, end: usize) -> (usize, usize) {
        let mut lower = start;
        let mut upper = end;
        while lower < upper && self.bytes[lower].is_ascii_ws() {
            lower += 1;
        }
        while upper > lower && self.bytes[upper - 1].is_ascii_ws() {
            upper -= 1;
        }
        (lower, upper)
    }

    fn resolved_url_bytes(&self, start: usize, end: usize) -> Option<Vec<u8>> {
        let value = &self.bytes[start..end];
        if !should_resolve(value) {
            return None;
        }
        let s = std::str::from_utf8(value).ok()?;
        let resolved = self.base_url.join(s).ok()?;
        let resolved = resolved.as_str();
        if resolved == s {
            return None;
        }
        Some(resolved.as_bytes().to_vec())
    }
}

/// True for values worth resolving: non-empty, not a same-document
/// reference, and scheme-less.
fn should_resolve(value: &[u8]) -> bool {
    let Some(first) = value.first() else {
        return false; // empty
    };
    if *first == b'#' {
        return false; // same-document reference
    }
    !has_scheme(value)
}

/// True when the value starts with a URL scheme
/// (`[A-Za-z][A-Za-z0-9+.-]*:`).
fn has_scheme(value: &[u8]) -> bool {
    let Some(first) = value.first() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    for &b in &value[1..] {
        if b == b':' {
            return true;
        }
        if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.') {
            return false;
        }
    }
    false
}

// MARK: Byte classification

trait ResolverByte {
    /// HTML ASCII whitespace: tab, LF, FF, CR, space (WHATWG).
    fn is_ascii_ws(&self) -> bool;
    /// XML NameStartChar, byte-wise: the ASCII ranges per the spec
    /// (`:`, letters, `_`), and any high byte (leading or continuation
    /// of a multibyte character) counts as a name character so Unicode
    /// tag names scan intact.
    fn is_xml_name_start(&self) -> bool;
    fn is_xml_name_char(&self) -> bool;
}

impl ResolverByte for u8 {
    fn is_ascii_ws(&self) -> bool {
        matches!(self, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
    }

    fn is_xml_name_start(&self) -> bool {
        let b = *self;
        b == b':' || b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
    }

    fn is_xml_name_char(&self) -> bool {
        let b = *self;
        b.is_xml_name_start() || b.is_ascii_digit() || b == b'-' || b == b'.'
    }
}

trait SliceExt {
    fn eq_ascii_case_insensitive(&self, lowercase: &[u8]) -> bool;
}

impl SliceExt for [u8] {
    fn eq_ascii_case_insensitive(&self, lowercase: &[u8]) -> bool {
        self.len() == lowercase.len()
            && self
                .iter()
                .zip(lowercase)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    }
}

impl<'a> UrlReplacementScanner<'a> {
    fn skip_whitespace(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_ws() {
            self.pos += 1;
        }
    }

    fn skip_past_greater_than(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'>' {
            self.pos += 1;
        }
        if self.pos < self.bytes.len() {
            self.pos += 1;
        }
    }

    fn matches(&self, literal: &[u8]) -> bool {
        self.pos + literal.len() <= self.bytes.len()
            && &self.bytes[self.pos..self.pos + literal.len()] == literal
    }

    fn skip_past(&mut self, literal: &[u8]) {
        while self.pos + literal.len() <= self.bytes.len() {
            if &self.bytes[self.pos..self.pos + literal.len()] == literal {
                self.pos += literal.len();
                return;
            }
            self.pos += 1;
        }
        self.pos = self.bytes.len();
    }

    /// Keeps `</scripty>` from being mistaken for `</script>`.
    fn skip_raw_text(&mut self, close_tag_name: &[u8]) {
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'<'
                && self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos + 1] == b'/'
            {
                let name_start = self.pos + 2;
                let name_end = name_start + close_tag_name.len();
                if name_end <= self.bytes.len()
                    && self.bytes[name_start..name_end].eq_ascii_case_insensitive(close_tag_name)
                    && is_close_tag_name_boundary(self.bytes, name_end)
                {
                    self.pos = name_end;
                    self.skip_past_greater_than();
                    return;
                }
            }
            self.pos += 1;
        }
    }
}

fn is_close_tag_name_boundary(bytes: &[u8], index: usize) -> bool {
    match bytes.get(index) {
        None => true,
        Some(b) => *b == b'>' || *b == b'/' || b.is_ascii_ws(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> url::Url {
        url::Url::parse("https://example.com/blog/entries/").expect("base parses")
    }

    /// The headline behavior: relative href/src rewrite against the
    /// base; everything outside the rewritten value is byte-identical.
    #[test]
    fn relative_hrefs_and_srcs_resolve_against_base() {
        let html = r#"<p><a href="post/1.html">One</a> <img src="../img/x.png"></p>"#;
        let out = resolving_relative_urls(html, &base());
        assert_eq!(
            out,
            r#"<p><a href="https://example.com/blog/entries/post/1.html">One</a> <img src="https://example.com/blog/img/x.png"></p>"#
        );
    }

    /// Fragment-only references stay; values with any scheme stay
    /// (http, data, mailto); protocol-relative takes the base scheme.
    #[test]
    fn fragments_schemed_and_protocol_relative_classify_correctly() {
        let html = r##"<a href="#note">n</a><a href="mailto:a@b.c">m</a><img src="data:image/gif;base64,AAAA"><img src="//cdn.example.com/x.png"><a href="https://other.example/y">y</a>"##;
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains(r##"href="#note""##));
        assert!(out.contains(r#"href="mailto:a@b.c""#));
        assert!(out.contains(r#"src="data:image/gif;base64,AAAA""#));
        assert!(out.contains(r#"src="https://cdn.example.com/x.png""#));
        assert!(out.contains(r#"href="https://other.example/y""#));
    }

    /// NNW's srcset handling: each URL candidate resolves, descriptors
    /// stay attached, trailing commas are separators not URL bytes.
    #[test]
    fn srcset_candidates_resolve_individually() {
        let html = r#"<img srcset="a-1x.png 1x, b-2x.png 2x, ,c-3x.png 3x">"#;
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains("https://example.com/blog/entries/a-1x.png 1x"));
        assert!(out.contains("https://example.com/blog/entries/b-2x.png 2x"));
        assert!(out.contains("https://example.com/blog/entries/c-3x.png 3x"));
    }

    /// Unquoted values end at whitespace or `>`, deliberately not at
    /// `/` (the inverse of the old HTMLScanner bug upstream called out).
    #[test]
    fn unquoted_values_keep_slashes() {
        let html = r#"<a href=/blog/post/2>two</a>"#;
        let out = resolving_relative_urls(html, &base());
        // The replacement covers only the value bytes: an unquoted value
        // stays unquoted in the output.
        assert!(out.contains(r#"href=https://example.com/blog/post/2"#));
    }

    /// Values are not entity-decoded: `&amp;` survives resolution
    /// byte-for-byte.
    #[test]
    fn entities_in_values_survive() {
        let html = r#"<a href="list?page=2&amp;sort=asc">l</a>"#;
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains("list?page=2&amp;sort=asc"));
        assert!(!out.contains("page=2&sort=asc\""));
    }

    /// Script and style bodies are raw text; nothing inside them
    /// resolves, including things shaped like tags.
    #[test]
    fn script_and_style_bodies_are_skipped() {
        let html = r#"<style>a { background: url(bg.png) }</style><script>var x = "<a href='z.html'>";</script><a href="real.html">r</a>"#;
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains("url(bg.png)"));
        assert!(out.contains("href='z.html'"));
        assert!(out.contains(r#"href="https://example.com/blog/entries/real.html""#));
    }

    /// Comments and CDATA sections pass through unscanned.
    #[test]
    fn comments_and_cdata_are_skipped() {
        let html =
            r#"<!-- <a href="no.html"> --><![CDATA[<b href="no.html">]]><a href="yes.html">y</a>"#;
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains(r#"href="no.html""#));
        assert!(out.contains(r#"href="https://example.com/blog/entries/yes.html""#));
    }

    /// Single-quoted values resolve; surrounding whitespace in a value
    /// stays while only the trimmed part rewrites.
    #[test]
    fn single_quotes_and_padded_values() {
        let html = "<a href = ' x.html ' >x</a>";
        let out = resolving_relative_urls(html, &base());
        assert!(out.contains("' https://example.com/blog/entries/x.html '"));
    }

    /// Nothing to resolve: the output is the input, byte for byte.
    #[test]
    fn nothing_to_resolve_returns_input_unchanged() {
        let html = "<p>plain text</p><a href=\"https://abs.example/x\">a</a><a href=\"#f\">f</a>";
        assert_eq!(resolving_relative_urls(html, &base()), html);
    }

    /// An unterminated quote abandons the rest of the scan; replacements
    /// recorded before it were well-formed and stay.
    #[test]
    fn unterminated_quote_stops_the_scan() {
        let html = r#"<a href="good.html">g</a><a href="unclosed"#;
        let out = resolving_relative_urls(html, &base());
        // The well-formed attribute before the broken one still rewrote.
        assert!(out.contains(r#"href="https://example.com/blog/entries/good.html""#));
        // The unterminated quote abandons everything after it, verbatim.
        assert!(out.ends_with(r#"<a href="unclosed"#));
    }
}

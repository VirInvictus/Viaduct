// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

pub mod date;
pub mod html;
pub mod json;
pub mod xml;

use crate::error::{ParseError, Result};
use crate::models::ParsedFeed;
pub use html::{HtmlMetadata, HtmlTag, HtmlTagType, extract_metadata};
pub use json::parse as parse_json;
pub use xml::{OpmlDocument, OpmlItem, parse_feed as parse_xml, parse_opml};

pub fn parse(data: &[u8], feed_url: &str) -> Result<ParsedFeed> {
    // Try JSON first, if it fails, try XML
    if let Ok(feed) = json::parse(data, feed_url) {
        return Ok(feed);
    }

    if let Ok(feed) = xml::parse_feed(data, feed_url) {
        return Ok(feed);
    }

    Err(ParseError::UnknownFormat.into())
}

pub fn init_parser() {
    // Phase 3: Setup quick-xml and serde_json parsers
}

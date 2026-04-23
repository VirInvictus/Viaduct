// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use quick_xml::Reader;
use quick_xml::events::Event;
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

pub fn extract_metadata(data: &[u8], url_string: &str) -> HtmlMetadata {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut tags = Vec::new();
    let scan_past_head = url_string.to_lowercase().contains("youtube.com")
        || url_string.to_lowercase().contains("youtu.be");
    let mut finished = false;

    loop {
        if finished {
            break;
        }
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = e.local_name();
                let name_ref = name.as_ref();

                if name_ref.eq_ignore_ascii_case(b"body") && !scan_past_head {
                    finished = true;
                    continue;
                }

                if name_ref.eq_ignore_ascii_case(b"link") {
                    let mut attributes = HashMap::new();
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        attributes.insert(key, val);
                    }
                    if let Some(rel) = attributes.get("rel")
                        && !rel.is_empty()
                        && (attributes.contains_key("href") || attributes.contains_key("src"))
                    {
                        tags.push(HtmlTag {
                            tag_type: HtmlTagType::Link,
                            attributes,
                        });
                    }
                } else if name_ref.eq_ignore_ascii_case(b"meta") {
                    let mut attributes = HashMap::new();
                    for attr in e.attributes().filter_map(|a| a.ok()) {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        attributes.insert(key, val);
                    }
                    if !attributes.is_empty() {
                        tags.push(HtmlTag {
                            tag_type: HtmlTagType::Meta,
                            attributes,
                        });
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // Ignore parse errors (HTML can be messy)
            _ => (),
        }
        buf.clear();
    }

    HtmlMetadata {
        url_string: url_string.to_string(),
        tags,
    }
}

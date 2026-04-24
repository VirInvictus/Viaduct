// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Native HTML renderer. No WebKit.
//!
//! Pipeline:
//!   1. `ammonia::clean` strips scripts / iframes / event handlers / inline styles.
//!   2. `quick-xml` walks the sanitized markup as a SAX stream.
//!   3. Structural tags map to `GtkTextTag` ranges in a `GtkTextBuffer` rendered
//!      by the article-pane `GtkTextView`.
//!   4. `<a>` clicks are routed through `gio::AppInfo::launch_default_for_uri`
//!      (which is `xdg-open` on Linux). Per the spec, Enter on a focused article
//!      should open the article URL — wire that at the window level, not here.
//!
//! Each link gets a uniquely-named tag of the form `link:<href>` so the click
//! handler can recover the URL by reading the tag name. There's no per-tag user
//! data on `GtkTextTag` without subclassing, and per-link tags keep the model
//! flat without paying that cost.

use crate::network::ImageCache;
use gtk::glib;
use gtk::glib::translate::IntoGlib;
use gtk::pango;
use gtk::prelude::*;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

const LINK_TAG_PREFIX: &str = "link:";

/// Hard cap on inline image display width. Articles assume a reading column
/// width — letting images render at their natural pixel size produces 4000px
/// monsters from raw camera shots.
const INLINE_IMAGE_MAX_WIDTH: i32 = 600;

/// Render sanitized HTML into the given `GtkTextView`. Replaces any prior
/// content. Idempotent — calling repeatedly with the same TextView works.
///
/// `image_cache` is used to async-fetch inline `<img>` payloads. Pass `None`
/// to render with text placeholders only (useful for tests / preview contexts
/// that don't want the network).
pub fn render_html(text_view: &gtk::TextView, html: &str, image_cache: Option<Arc<ImageCache>>) {
    let buffer = text_view.buffer();
    buffer.set_text("");
    ensure_base_tags(&buffer);

    let sanitized = ammonia::clean(html);
    walk(text_view, &buffer, &sanitized, image_cache);

    install_link_handler_once(text_view);
}

fn ensure_base_tags(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    let add_if_missing = |name: &str, build: &dyn Fn() -> gtk::TextTag| {
        if table.lookup(name).is_none() {
            let tag = build();
            tag.set_property("name", name);
            table.add(&tag);
        }
    };

    add_if_missing("bold", &|| {
        gtk::TextTag::builder()
            .weight(pango::Weight::Bold.into_glib())
            .build()
    });
    add_if_missing("italic", &|| {
        gtk::TextTag::builder().style(pango::Style::Italic).build()
    });
    add_if_missing("monospace", &|| {
        gtk::TextTag::builder().family("monospace").build()
    });
    add_if_missing("code-block", &|| {
        gtk::TextTag::builder()
            .family("monospace")
            .left_margin(16)
            .pixels_above_lines(6)
            .pixels_below_lines(6)
            .build()
    });
    add_if_missing("blockquote", &|| {
        gtk::TextTag::builder()
            .left_margin(24)
            .style(pango::Style::Italic)
            .build()
    });
    for (i, scale) in [
        ("heading-1", 1.8),
        ("heading-2", 1.5),
        ("heading-3", 1.3),
        ("heading-4", 1.15),
        ("heading-5", 1.05),
        ("heading-6", 1.0),
    ] {
        add_if_missing(i, &|| {
            gtk::TextTag::builder()
                .scale(scale)
                .weight(pango::Weight::Bold.into_glib())
                .pixels_above_lines(8)
                .pixels_below_lines(4)
                .build()
        });
    }
    add_if_missing("link", &|| {
        gtk::TextTag::builder()
            .underline(pango::Underline::Single)
            .foreground("#3584e4")
            .build()
    });
}

fn walk(
    text_view: &gtk::TextView,
    buffer: &gtk::TextBuffer,
    sanitized_html: &str,
    image_cache: Option<Arc<ImageCache>>,
) {
    // Wrap in a synthetic root so the walker handles fragments cleanly. ammonia
    // output is well-formed enough for quick-xml's permissive mode to chew on.
    let wrapped = format!("<root>{}</root>", sanitized_html);
    let mut reader = Reader::from_str(&wrapped);
    reader.config_mut().trim_text(false);

    // Stack of (tag_name, start_offset_in_buffer) for inline tags.
    let mut inline_stack: Vec<(String, i32)> = Vec::new();
    // Stack of currently-open block tag names ("p", "blockquote", "pre", etc.)
    // to know whether to emit pre-formatted whitespace.
    let mut block_stack: Vec<String> = Vec::new();
    // List context for <li> bullets/numbering.
    let mut list_stack: Vec<ListKind> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                let name = std::str::from_utf8(local.as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "blockquote" | "div"
                    | "section" | "article" | "header" | "footer" | "main" | "aside" | "figure"
                    | "figcaption" => {
                        ensure_block_break(buffer);
                        block_stack.push(name.clone());
                        inline_stack.push((name, mark_offset(buffer)));
                    }
                    "pre" => {
                        ensure_block_break(buffer);
                        block_stack.push(name.clone());
                        inline_stack.push(("code-block".to_string(), mark_offset(buffer)));
                    }
                    "ul" => {
                        ensure_block_break(buffer);
                        list_stack.push(ListKind::Unordered);
                    }
                    "ol" => {
                        ensure_block_break(buffer);
                        list_stack.push(ListKind::Ordered(1));
                    }
                    "li" => {
                        ensure_block_break(buffer);
                        let prefix = match list_stack.last_mut() {
                            Some(ListKind::Unordered) => "• ".to_string(),
                            Some(ListKind::Ordered(n)) => {
                                let s = format!("{}. ", *n);
                                *n += 1;
                                s
                            }
                            None => String::new(),
                        };
                        if !prefix.is_empty() {
                            insert_text(buffer, &prefix);
                        }
                    }
                    "strong" | "b" => {
                        inline_stack.push(("bold".to_string(), mark_offset(buffer)));
                    }
                    "em" | "i" => {
                        inline_stack.push(("italic".to_string(), mark_offset(buffer)));
                    }
                    "code" => {
                        inline_stack.push(("monospace".to_string(), mark_offset(buffer)));
                    }
                    "a" => {
                        let href = attr(e, b"href").unwrap_or_default();
                        let tag_name = format!("{}{}", LINK_TAG_PREFIX, href);
                        ensure_link_tag(buffer, &tag_name);
                        inline_stack.push((tag_name, mark_offset(buffer)));
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                let name = std::str::from_utf8(local.as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    "br" => insert_text(buffer, "\n"),
                    "hr" => {
                        ensure_block_break(buffer);
                        insert_text(buffer, "──────\n");
                    }
                    "img" => {
                        let src = attr(e, b"src").unwrap_or_default();
                        let alt = attr(e, b"alt").unwrap_or_default();
                        if let Some(cache) = image_cache.clone()
                            && !src.is_empty()
                            && src.to_ascii_lowercase().starts_with("http")
                        {
                            insert_image_anchor(text_view, buffer, cache, src);
                        } else if !alt.is_empty() {
                            insert_text(buffer, &format!("[image: {}]", alt));
                        } else {
                            insert_text(buffer, "[image]");
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = e.local_name();
                let name = std::str::from_utf8(local.as_ref())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    "ul" | "ol" => {
                        list_stack.pop();
                        ensure_block_break(buffer);
                    }
                    "li" => {
                        insert_text(buffer, "\n");
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if let Some((stacked, start)) = inline_stack.pop() {
                            let heading =
                                format!("heading-{}", stacked.chars().last().unwrap_or('1'));
                            apply_tag(buffer, &heading, start);
                        }
                        block_stack.pop();
                        ensure_block_break(buffer);
                    }
                    "p" | "blockquote" | "div" | "section" | "article" | "header" | "footer"
                    | "main" | "aside" | "figure" | "figcaption" => {
                        if let Some((stacked, start)) = inline_stack.pop()
                            && stacked == "blockquote"
                        {
                            apply_tag(buffer, "blockquote", start);
                        }
                        block_stack.pop();
                        ensure_block_break(buffer);
                    }
                    "pre" => {
                        if let Some((tag, start)) = inline_stack.pop() {
                            apply_tag(buffer, &tag, start);
                        }
                        block_stack.pop();
                        ensure_block_break(buffer);
                    }
                    "strong" | "b" | "em" | "i" | "code" | "a" => {
                        if let Some((tag, start)) = inline_stack.pop() {
                            apply_tag(buffer, &tag, start);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                let raw = e.unescape().unwrap_or_default();
                let text: String = if block_stack.last().map(String::as_str) == Some("pre") {
                    // Preserve <pre> whitespace verbatim.
                    raw.to_string()
                } else {
                    collapse_whitespace(&raw)
                };
                if !text.is_empty() {
                    insert_text(buffer, &text);
                }
            }
            Ok(Event::CData(c)) => {
                let bytes = c.into_inner();
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    insert_text(buffer, s);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

enum ListKind {
    Unordered,
    Ordered(u32),
}

fn mark_offset(buffer: &gtk::TextBuffer) -> i32 {
    buffer.end_iter().offset()
}

fn insert_text(buffer: &gtk::TextBuffer, s: &str) {
    let mut iter = buffer.end_iter();
    buffer.insert(&mut iter, s);
}

fn ensure_block_break(buffer: &gtk::TextBuffer) {
    let end = buffer.end_iter();
    if end.offset() == 0 {
        return;
    }
    let mut iter_back = end;
    if iter_back.backward_char() && iter_back.char() == '\n' {
        return;
    }
    insert_text(buffer, "\n");
}

fn apply_tag(buffer: &gtk::TextBuffer, tag_name: &str, start_offset: i32) {
    let table = buffer.tag_table();
    let Some(tag) = table.lookup(tag_name) else {
        return;
    };
    let start = buffer.iter_at_offset(start_offset);
    let end = buffer.end_iter();
    buffer.apply_tag(&tag, &start, &end);
}

fn ensure_link_tag(buffer: &gtk::TextBuffer, tag_name: &str) {
    let table = buffer.tag_table();
    if table.lookup(tag_name).is_some() {
        return;
    }
    let tag = gtk::TextTag::builder()
        .underline(pango::Underline::Single)
        .foreground("#3584e4")
        .build();
    tag.set_property("name", tag_name);
    table.add(&tag);
}

fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    for attr in e.attributes().filter_map(|a| a.ok()) {
        if attr.key.as_ref().eq_ignore_ascii_case(key) {
            return std::str::from_utf8(attr.value.as_ref())
                .ok()
                .map(String::from);
        }
    }
    None
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Anchor a `gtk::Picture` widget in the buffer at the current end and kick off
/// an async fetch of the image bytes. On success the texture is decoded on the
/// main thread and assigned to the picture's paintable. Failures leave a blank
/// space — port-first; we don't promote them to an error widget.
fn insert_image_anchor(
    text_view: &gtk::TextView,
    buffer: &gtk::TextBuffer,
    image_cache: Arc<ImageCache>,
    src: String,
) {
    ensure_block_break(buffer);
    let mut iter = buffer.end_iter();
    let anchor = buffer.create_child_anchor(&mut iter);

    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::ScaleDown);
    picture.set_size_request(-1, 1); // give it a non-zero height until loaded
    picture.set_hexpand(false);
    picture.set_halign(gtk::Align::Start);

    text_view.add_child_at_anchor(&picture, &anchor);
    insert_text(buffer, "\n");

    let picture_weak = picture.downgrade();
    glib::spawn_future_local(async move {
        let Some(bytes) = image_cache.image(&src).await else {
            return;
        };
        let Some(picture) = picture_weak.upgrade() else {
            return;
        };
        let glib_bytes = glib::Bytes::from(&bytes);
        match gtk::gdk::Texture::from_bytes(&glib_bytes) {
            Ok(texture) => {
                let intrinsic = texture.width().min(INLINE_IMAGE_MAX_WIDTH);
                picture.set_paintable(Some(&texture));
                picture.set_size_request(intrinsic, -1);
            }
            Err(e) => tracing::debug!(?e, "inline image decode failed: {}", src),
        }
    });
}

/// Install a single click controller on the `TextView` that resolves the click
/// to a `link:<href>` tag and launches the URL via xdg-open. Idempotent —
/// stashes a marker on the widget so re-renders don't stack handlers.
fn install_link_handler_once(text_view: &gtk::TextView) {
    static LINK_HANDLER_INSTALLED: &str = "viaduct-link-handler-installed";
    unsafe {
        if text_view.data::<bool>(LINK_HANDLER_INSTALLED).is_some() {
            return;
        }
        text_view.set_data(LINK_HANDLER_INSTALLED, true);
    }

    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let tv = text_view.clone();
    // Clicks fire fast and we don't want to spawn dozens of xdg-open processes
    // for the same tap; use a tiny re-entrancy guard.
    let opening = Rc::new(Cell::new(false));
    let opening_inner = opening.clone();
    click.connect_released(move |gesture, _n_press, x, y| {
        if opening_inner.get() {
            return;
        }
        let (bx, by) = tv.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        if let Some(iter) = tv.iter_at_location(bx, by) {
            for tag in iter.tags() {
                if let Some(name) = tag.name()
                    && let Some(href) = name.strip_prefix(LINK_TAG_PREFIX)
                {
                    opening_inner.set(true);
                    let opening_clone = opening_inner.clone();
                    let href_owned = href.to_string();
                    glib::idle_add_local_once(move || {
                        let _ = gtk::gio::AppInfo::launch_default_for_uri(
                            &href_owned,
                            None::<&gtk::gio::AppLaunchContext>,
                        );
                        opening_clone.set(false);
                    });
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    return;
                }
            }
        }
    });
    text_view.add_controller(click);
}

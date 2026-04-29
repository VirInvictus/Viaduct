// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::models::Article;
use crate::network::ImageCache;
use crate::network::video_thumbs::{VideoSource, detect_video};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

glib::wrapper! {
    /// A GObject wrapper around the domain `Article` so it can be used in `gio::ListModel`.
    pub struct ArticleNode(ObjectSubclass<imp::ArticleNode>);
}

pub mod imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ArticleNode)]
    pub struct ArticleNode {
        pub article: RefCell<Option<Article>>,
        /// Optional FTS5 snippet for search-result rows. When set, the timeline
        /// row renders this in the preview area instead of the article summary.
        pub snippet: RefCell<Option<String>>,
        /// Cached status from the `statuses` table. Exposed as glib properties so
        /// the row factory can subscribe to `notify::read` and re-style the title
        /// without waiting for a recycle.
        #[property(get, set)]
        pub read: Cell<bool>,
        #[property(get, set)]
        pub starred: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArticleNode {
        const NAME: &'static str = "ViaductArticleNode";
        type Type = super::ArticleNode;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ArticleNode {}
}

impl ArticleNode {
    pub fn new(article: Article) -> Self {
        let node: Self = glib::Object::builder().build();
        node.imp().article.replace(Some(article));
        node
    }

    pub fn with_snippet(article: Article, snippet: String) -> Self {
        let node: Self = glib::Object::builder().build();
        node.imp().article.replace(Some(article));
        node.imp().snippet.replace(Some(snippet));
        node
    }

    pub fn article(&self) -> Option<Article> {
        self.imp().article.borrow().clone()
    }

    pub fn snippet(&self) -> Option<String> {
        self.imp().snippet.borrow().clone()
    }

    pub fn is_read(&self) -> bool {
        self.read()
    }

    pub fn is_starred(&self) -> bool {
        self.starred()
    }

    pub fn set_status(&self, read: bool, starred: bool) {
        if self.read() != read {
            self.set_read(read);
        }
        if self.starred() != starred {
            self.set_starred(starred);
        }
    }
}

/// Resolver from `feed_id` to display name. Built off the OPML tree once at
/// load time and rebuilt on every OPML mutation. The timeline row factory
/// reads through it on each bind, falling back to the feed_id (URL) when the
/// feed isn't in the map yet.
pub type FeedNameMap = Rc<RefCell<HashMap<String, String>>>;

/// Sets up the Timeline ListView with models and the row factory.
/// Returns the `SingleSelection` so the caller can drive article rendering.
pub fn setup_timeline_list_view(
    list_view: &gtk::ListView,
    list_store: &gtk::gio::ListStore,
    feed_names: FeedNameMap,
    image_cache: Arc<ImageCache>,
) -> gtk::SingleSelection {
    let selection_model = gtk::SingleSelection::new(Some(list_store.clone()));
    selection_model.set_autoselect(false);
    selection_model.set_can_unselect(true);
    list_view.set_model(Some(&selection_model));

    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        // Row layout (v1.2.0-pre4.3 — restructured to fix date getting
        // pushed off-screen in smart-feed views; v1.3.0 added a leading
        // thumbnail column hidden when the article has no detected video):
        //
        //   row_hbox:
        //     thumb_picture (visible only when the article has a video)
        //     content_vbox (hexpand=true):
        //       top_hbox: title (hexpand) | media icon + count
        //       feed_name
        //       preview (2 lines)
        //     date_label  ← fixed-width, top-aligned, RIGHT column
        //
        // Date sits as a sibling of the entire content column instead of
        // sharing an hbox with the title. Long aggregated titles in
        // smart feeds (Today / All Unread / Starred) can't push it off
        // because hbox layout allocates the date its natural width
        // before letting hexpand=true children take the rest.
        let row_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_hbox.set_margin_start(8);
        row_hbox.set_margin_end(8);
        row_hbox.set_margin_top(8);
        row_hbox.set_margin_bottom(8);

        // Video thumbnail column. Hidden by default; populated and shown
        // during bind when `detect_video` finds a YouTube / Vimeo URL.
        // 80×45 = 16:9 at compact size — small enough not to dominate
        // text-only rows when present, large enough to read.
        let thumb_picture = gtk::Picture::new();
        thumb_picture.set_size_request(80, 45);
        thumb_picture.set_content_fit(gtk::ContentFit::Cover);
        thumb_picture.set_can_shrink(true);
        thumb_picture.set_valign(gtk::Align::Start);
        thumb_picture.add_css_class("viaduct-timeline-thumb");
        thumb_picture.set_visible(false);

        let content_vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content_vbox.set_hexpand(true);

        let top_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let title_label = gtk::Label::new(None);
        title_label.set_hexpand(true);
        title_label.set_halign(gtk::Align::Start);
        title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // Cap the title's natural width (≈ 32 average glyph widths) so a
        // row containing a 100-character smart-feed title can't request
        // a 900 px natural width and inflate the timeline pane through
        // the AdwNavigationSplitView's sidebar-width-fraction up to its
        // max. Title still ellipsizes via EllipsizeMode::End at the cap.
        title_label.set_max_width_chars(32);
        title_label.set_width_chars(20);

        // Media indicator: small icon when the article has attachments
        // (podcast/video enclosures, MRSS media). Count badge appears when
        // the article carries more than one — bind logic decides the digits.
        let media_icon = gtk::Image::from_icon_name("audio-x-generic-symbolic");
        media_icon.set_pixel_size(12);
        media_icon.add_css_class("dim-label");
        media_icon.set_visible(false);
        let media_count = gtk::Label::new(None);
        media_count.add_css_class("numeric");
        media_count.add_css_class("dim-label");
        media_count.set_visible(false);

        top_hbox.append(&title_label);
        top_hbox.append(&media_icon);
        top_hbox.append(&media_count);

        let feed_name_label = gtk::Label::new(None);
        feed_name_label.set_halign(gtk::Align::Start);
        feed_name_label.add_css_class("dim-label");
        feed_name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // Same natural-width cap rationale as title_label — long feed
        // names ("Deutsche Welle: DW.com - Top Stories") would otherwise
        // inflate the pane.
        feed_name_label.set_max_width_chars(32);
        feed_name_label.set_width_chars(20);

        let preview_label = gtk::Label::new(None);
        preview_label.set_halign(gtk::Align::Start);
        preview_label.set_wrap(true);
        preview_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        preview_label.set_lines(2);
        preview_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview_label.add_css_class("dim-label");
        // Wrap=true makes natural width potentially huge (full prose
        // length). Cap so the row's total natural width stays bounded
        // regardless of how long the preview text is.
        preview_label.set_max_width_chars(48);
        preview_label.set_width_chars(20);

        content_vbox.append(&top_hbox);
        content_vbox.append(&feed_name_label);
        content_vbox.append(&preview_label);

        let date_label = gtk::Label::new(None);
        date_label.add_css_class("dim-label");
        date_label.add_css_class("numeric");
        date_label.set_valign(gtk::Align::Start);
        date_label.set_xalign(1.0);
        // Hard-floor width so the date column always shows. Title
        // ellipsizes around it.
        date_label.set_size_request(80, -1);

        row_hbox.append(&thumb_picture);
        row_hbox.append(&content_vbox);
        row_hbox.append(&date_label);

        item.set_child(Some(&row_hbox));
    });

    let feed_names_for_bind = feed_names.clone();
    let cache_for_bind = image_cache.clone();
    factory.connect_bind(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let node = item.item().and_downcast::<ArticleNode>().unwrap();
        // Tree from v1.3.0 (thumbnail column added in front):
        //   row_hbox
        //   ├── thumb_picture (first_child, hidden when no video)
        //   ├── content_vbox
        //   │   ├── top_hbox (title + media)
        //   │   ├── feed_name_label
        //   │   └── preview_label
        //   └── date_label  (last_child)
        let row_hbox = item.child().and_downcast::<gtk::Box>().unwrap();
        let thumb_picture = row_hbox
            .first_child()
            .and_downcast::<gtk::Picture>()
            .unwrap();
        let content_vbox = thumb_picture
            .next_sibling()
            .and_downcast::<gtk::Box>()
            .unwrap();
        let date_label = row_hbox.last_child().and_downcast::<gtk::Label>().unwrap();

        let top_hbox = content_vbox
            .first_child()
            .and_downcast::<gtk::Box>()
            .unwrap();
        let title_label = top_hbox.first_child().and_downcast::<gtk::Label>().unwrap();
        let media_icon = title_label
            .next_sibling()
            .and_downcast::<gtk::Image>()
            .unwrap();
        let media_count = media_icon
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();

        let feed_name_label = top_hbox
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();
        let preview_label = feed_name_label
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();

        if let Some(article) = node.article() {
            let title = article.title.as_deref().unwrap_or("Untitled");
            title_label.set_text(title);

            apply_read_styling(&title_label, node.read());

            // Media indicator. Pick a roughly-correct icon based on the first
            // attachment's MIME type so podcasts and videos look distinct.
            let n = article.attachments.len();
            if n > 0 {
                let first_type = article.attachments[0].mime_type.as_deref().unwrap_or("");
                let icon_name = if first_type.starts_with("video/") {
                    "video-x-generic-symbolic"
                } else if first_type.starts_with("image/") {
                    "image-x-generic-symbolic"
                } else {
                    "audio-x-generic-symbolic"
                };
                media_icon.set_icon_name(Some(icon_name));
                media_icon.set_visible(true);
                if n > 1 {
                    media_count.set_text(&n.to_string());
                    media_count.set_visible(true);
                } else {
                    media_count.set_visible(false);
                }
            } else {
                media_icon.set_visible(false);
                media_count.set_visible(false);
            }

            let date_str = article
                .date_published
                .map(format_relative_date)
                .unwrap_or_default();
            date_label.set_text(&date_str);

            // Resolve display name through the feed-name map. Falls back to
            // the feed_id (which is the URL) when the feed isn't loaded yet
            // — startup race when the timeline beats the OPML load.
            let display_name = feed_names_for_bind
                .borrow()
                .get(&article.feed_id)
                .cloned()
                .unwrap_or_else(|| article.feed_id.clone());
            feed_name_label.set_text(&display_name);

            // Search results carry an FTS5 snippet; prefer that over the
            // generic summary/content preview so the user sees why the row
            // matched.
            let preview_source = node.snippet().unwrap_or_else(|| {
                article
                    .summary
                    .as_deref()
                    .or(article.content_text.as_deref())
                    .or(article.content_html.as_deref())
                    .unwrap_or("")
                    .to_string()
            });
            let clean_preview = strip_html_for_preview(&preview_source);
            preview_label.set_text(&clean_preview);

            // Sharpen the read/unread visual hierarchy beyond the title:
            // when read, the entire row dims; when unread, feed-name +
            // preview pop a touch above default to draw the eye.
            apply_row_read_styling(&feed_name_label, &preview_label, &date_label, node.read());

            // Video thumbnail. Hide by default; if the article carries a
            // detectable YouTube/Vimeo URL, async-load the thumbnail and
            // assign to the picture. The article_id is stamped onto the
            // widget as a stale-row guard so a recycled row doesn't show
            // the previous article's thumb.
            thumb_picture.set_paintable(gtk::gdk::Paintable::NONE);
            thumb_picture.set_visible(false);
            thumb_picture.set_widget_name(&article.article_id);
            spawn_video_thumbnail_fetch(&article, &thumb_picture, cache_for_bind.clone());
        }

        // Re-style the title whenever the node's read flag flips. Stored on
        // the list_item so connect_unbind can disconnect cleanly when the
        // row recycles to a different node.
        let title_for_notify = title_label.downgrade();
        let id = node.connect_notify_local(Some("read"), move |node, _| {
            if let Some(label) = title_for_notify.upgrade() {
                apply_read_styling(&label, node.read());
            }
        });
        unsafe {
            item.set_data("viaduct-read-handler", id);
        }
    });

    factory.connect_unbind(|_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");
        let Some(node) = item.item().and_downcast::<ArticleNode>() else {
            return;
        };
        unsafe {
            if let Some(id) = item.steal_data::<glib::SignalHandlerId>("viaduct-read-handler") {
                node.disconnect(id);
            }
        }
    });

    list_view.set_factory(Some(&factory));
    selection_model
}

/// Toggle bold/dim-label classes on the title to reflect read state. NNW
/// renders unread titles in bold full color and read titles in regular
/// weight + slight gray.
fn apply_read_styling(title: &gtk::Label, read: bool) {
    if read {
        title.remove_css_class("heading");
        title.add_css_class("dim-label");
    } else {
        title.remove_css_class("dim-label");
        title.add_css_class("heading");
    }
}

/// Sharpen the unread/read visual hierarchy beyond the title — once an
/// article is read, every label in the row gets a notch dimmer; while
/// unread, the supporting labels (feed name, preview) sit at default
/// brightness so the title doesn't fight a low-contrast row.
fn apply_row_read_styling(
    feed_name: &gtk::Label,
    preview: &gtk::Label,
    date: &gtk::Label,
    read: bool,
) {
    let extra_dim = "viaduct-row-read";
    if read {
        feed_name.add_css_class(extra_dim);
        preview.add_css_class(extra_dim);
        date.add_css_class(extra_dim);
    } else {
        feed_name.remove_css_class(extra_dim);
        preview.remove_css_class(extra_dim);
        date.remove_css_class(extra_dim);
    }
}

/// NNW-style relative date for the timeline row: "5m ago" within the
/// hour, "3h ago" within the day, "Yesterday" for ~24-48 h, weekday
/// name for the past week, "Mar 19" within the year, "Mar 19, 2025"
/// for older. Stays compact so long titles don't get squeezed.
fn format_relative_date(d: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let local_now = now.with_timezone(&chrono::Local);
    let local = d.with_timezone(&chrono::Local);
    let elapsed = now.signed_duration_since(d);

    if elapsed.num_seconds() < 0 {
        // Future-dated article (clock skew or feed bug). Fall back to
        // the absolute medium format rather than "in 3h".
        return local.format("%b %-d").to_string();
    }
    let mins = elapsed.num_minutes();
    let hours = elapsed.num_hours();
    let days = elapsed.num_days();
    if mins < 1 {
        return "Just now".to_string();
    }
    if hours < 1 {
        return format!("{}m ago", mins);
    }
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    // Today / Yesterday checks key off LOCAL calendar dates so a feed
    // post at 23:30 local time and read at 02:00 the next day is
    // "Yesterday" rather than "3h ago".
    use chrono::Datelike;
    let today_ord = local_now.num_days_from_ce();
    let pub_ord = local.num_days_from_ce();
    let day_diff = today_ord - pub_ord;
    if day_diff == 1 {
        return "Yesterday".to_string();
    }
    if (2..7).contains(&day_diff) {
        return local.format("%A").to_string();
    }
    if local.year() == local_now.year() {
        return local.format("%b %-d").to_string();
    }
    if days < 365 {
        return local.format("%b %-d").to_string();
    }
    local.format("%b %-d, %Y").to_string()
}

/// Drop tags, decode the most common HTML entities, and collapse whitespace
/// so a feed-supplied HTML preview lands as a clean two-line string in
/// the timeline row. Skipped when the input is already plain text.
fn strip_html_for_preview(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut last_was_space = false;
    for ch in input.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        out.push(ch);
        last_was_space = false;
    }
    decode_common_entities(out.trim())
}

fn decode_common_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&hellip;", "…")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&rsquo;", "’")
        .replace("&lsquo;", "‘")
        .replace("&rdquo;", "”")
        .replace("&ldquo;", "“")
}

/// Detect a video URL on the article and async-load its thumbnail through
/// `ImageCache`. The widget's `name` carries the article_id as a stale-row
/// guard — when the row gets recycled to a different article before the
/// thumbnail finishes downloading, we skip applying the texture.
///
/// IMPORTANT (v1.5.6): every reqwest call goes through `spawn_on_runtime`
/// so DNS resolution lands on the tokio reactor. The previous version
/// called `thumbnail_url(&client, …)` directly inside
/// `glib::spawn_future_local`; for the Vimeo path that triggered
/// `client.get(oembed_url).send().await` from the GLib executor, panicking
/// with "there is no reactor running, must be called from the context of
/// a Tokio 1.x runtime". The panic cascaded into a frozen
/// AdwNavigationSplitView whenever a Vimeo-bearing article got bound while
/// the window was in collapsed adaptive layout.
fn spawn_video_thumbnail_fetch(article: &Article, picture: &gtk::Picture, cache: Arc<ImageCache>) {
    let Some(source) = detect_video(article) else {
        return;
    };
    let expected_id = article.article_id.clone();
    let picture_weak = picture.downgrade();

    // All reqwest work happens on the tokio runtime; the resolved bytes
    // come back to the GTK thread via oneshot for `GdkTexture::from_bytes`.
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<Vec<u8>>>();
    let cache_for_runtime = cache.clone();
    crate::spawn_on_runtime(async move {
        let bytes = match source {
            VideoSource::YouTube { id } => {
                cache_for_runtime
                    .video_thumbnail(&crate::network::video_thumbs::youtube_thumbnail_url(&id))
                    .await
            }
            VideoSource::Vimeo { .. } => {
                let client = cache_for_runtime.client().await;
                let url_opt = crate::network::video_thumbs::thumbnail_url(&client, &source).await;
                match url_opt {
                    Some(u) => cache_for_runtime.video_thumbnail(&u).await,
                    None => None,
                }
            }
        };
        let _ = tx.send(bytes);
    });

    glib::spawn_future_local(async move {
        let Ok(Some(bytes)) = rx.await else { return };
        let Some(picture) = picture_weak.upgrade() else {
            return;
        };
        if picture.widget_name() != expected_id {
            return;
        }
        let glib_bytes = glib::Bytes::from_owned(bytes);
        match gtk::gdk::Texture::from_bytes(&glib_bytes) {
            Ok(texture) => {
                picture.set_paintable(Some(&texture));
                picture.set_visible(true);
            }
            Err(e) => {
                tracing::debug!(?e, "failed to decode video thumbnail bytes");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_drops_tags_and_collapses_whitespace() {
        let input = "<p>Hello <strong>world</strong>!</p>\n<p>How are\nyou?</p>";
        let out = strip_html_for_preview(input);
        assert_eq!(out, "Hello world ! How are you?");
    }

    #[test]
    fn strip_html_decodes_common_entities() {
        let input = "Tom &amp; Jerry &mdash; Mary&#39;s &ldquo;cat&rdquo;";
        let out = strip_html_for_preview(input);
        assert_eq!(out, "Tom & Jerry — Mary's “cat”");
    }

    #[test]
    fn strip_html_handles_plain_text() {
        let input = "  Already plain  text  ";
        let out = strip_html_for_preview(input);
        assert_eq!(out, "Already plain text");
    }

    #[test]
    fn relative_date_just_now_under_minute() {
        let d = chrono::Utc::now() - chrono::Duration::seconds(15);
        assert_eq!(format_relative_date(d), "Just now");
    }

    #[test]
    fn relative_date_minutes_within_hour() {
        let d = chrono::Utc::now() - chrono::Duration::minutes(7);
        assert_eq!(format_relative_date(d), "7m ago");
    }

    #[test]
    fn relative_date_hours_within_day() {
        let d = chrono::Utc::now() - chrono::Duration::hours(5);
        assert_eq!(format_relative_date(d), "5h ago");
    }
}

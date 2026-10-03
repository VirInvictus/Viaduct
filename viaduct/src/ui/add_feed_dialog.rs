// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Add-Feed dialog. Port of NetNewsWire's "Add Feed" window.
//!
//! UX: paste a URL — feed URL OR website URL — optionally override the
//! name, optionally pick a folder. On submit, run two-pass discovery
//! (feed-first, HTML `<link rel="alternate">` fallback) on the tokio
//! runtime, add the result to the OPML, refresh the sidebar, fire a
//! one-shot refresh of just the new feed so its articles appear.
//!
//! All network work goes through `crate::spawn_on_runtime`; the GTK
//! side awaits a `tokio::sync::oneshot` for the result. Same pattern
//! as the rest of the app — never run reqwest directly off the GLib
//! executor (panics — see CLAUDE.md gotchas section).

use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use viaduct_core::network::feed_discovery;

use crate::database::opml::OpmlFile;
use crate::models::Feed;
use crate::ui::rows;
use crate::ui::window::ViaductWindow;

/// Build and present the Add Feed dialog modal to `parent`.
pub fn present(parent: &ViaductWindow) {
    // Phase 20c: plain modal window. The Add button rides a `GtkHeaderBar`;
    // the rows are `ui::rows` in a `.boxed-list`.
    let add_btn = gtk::Button::with_label("Add");
    add_btn.add_css_class("suggested-action");
    add_btn.set_sensitive(false);
    let header = gtk::HeaderBar::new();
    header.pack_end(&add_btn);

    let group = rows::group(
        None,
        Some("Paste a feed URL or a website URL. Viaduct will look up the feed automatically."),
    );

    let (url_row, url_entry) = rows::entry_row(Some("Feed or website URL"), None, None, None);
    let (name_row, name_entry) = rows::entry_row(Some("Name (optional)"), None, None, None);

    let folder_names = list_folder_names(parent);
    let mut combo_labels: Vec<String> = vec!["None".to_string()];
    combo_labels.extend(folder_names.iter().cloned());
    let combo_strs: Vec<&str> = combo_labels.iter().map(|s| s.as_str()).collect();
    let (folder_row, folder_drop_down) = rows::combo_row(
        "Folder",
        Some("Where the feed will live in the sidebar"),
        &combo_strs,
    );
    // NNW `b4361413f` (#4221): the initial folder mirrors the current
    // sidebar selection (selected feed → its folder; folder → itself;
    // otherwise top-level). Index +1 skips the "None" row.
    let initial_folder_index = parent
        .selected_add_feed_folder_public()
        .and_then(|name| folder_names.iter().position(|n| *n == name))
        .map_or(0, |idx| (idx + 1) as u32);
    folder_drop_down.set_selected(initial_folder_index);

    // NewsFlash #905 analog: reader-on-by-default per feed at creation
    // time. Same row copy as the feed-settings dialog so the two
    // surfaces read as one setting.
    let (reader_row, reader_switch) = rows::switch_row(
        "Always use Reader View",
        Some("Open every article from this feed in extracted-text mode."),
    );

    group.add(&url_row);
    group.add(&name_row);
    group.add(&folder_row);
    group.add(&reader_row);

    // Status row at the bottom — shows discovery progress + error
    // messages without taking the user out of the dialog. Uses
    // dim-label styling so empty state is invisible.
    let status_label = gtk::Label::new(None);
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);
    status_label.add_css_class("caption");
    status_label.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.append(group.widget());
    content.append(&status_label);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&header);
    outer.append(&content);

    let dialog = gtk::Window::builder()
        .title("Add Feed")
        .transient_for(parent)
        .modal(true)
        .default_width(440)
        .child(&outer)
        .build();
    crate::ui::close_on_escape(&dialog);

    // Tracks whether a discovery is in flight — prevents double-submits
    // and disables the Add button while we're working.
    let busy = Rc::new(RefCell::new(false));

    // Bind Add-button sensitivity to "URL non-empty AND not currently busy."
    let add_btn_for_text = add_btn.clone();
    let busy_for_text = busy.clone();
    url_entry.connect_changed(move |entry| {
        let has_text = !entry.text().trim().is_empty();
        let idle = !*busy_for_text.borrow();
        add_btn_for_text.set_sensitive(has_text && idle);
    });

    // Submit when the user activates the Add button or hits Enter in
    // the URL field.
    let parent_weak = parent.downgrade();
    let dialog_weak = dialog.downgrade();
    let submit = {
        let url_entry = url_entry.clone();
        let name_entry = name_entry.clone();
        let folder_drop_down = folder_drop_down.clone();
        let reader_switch = reader_switch.clone();
        let combo_labels = combo_labels.clone();
        let status_label = status_label.clone();
        let add_btn = add_btn.clone();
        let busy = busy.clone();
        move || {
            let Some(parent) = parent_weak.upgrade() else {
                return;
            };
            if *busy.borrow() {
                return;
            }
            let url_input = url_entry.text().to_string();
            if url_input.trim().is_empty() {
                return;
            }
            let name_input = {
                let s = name_entry.text().to_string();
                if s.trim().is_empty() { None } else { Some(s) }
            };
            let folder_idx = folder_drop_down.selected() as usize;
            let folder_name = if folder_idx == 0 {
                None
            } else {
                combo_labels.get(folder_idx).cloned()
            };
            let reader_on = reader_switch.is_active();

            // NNW `7ea15d7f7` (#3758), site 1: an exact match on the
            // entered URL short-circuits before discovery — no network
            // round trip for a feed that's already subscribed.
            if let Some(opml) = parent.opml_snapshot_public()
                && let Some(existing) = existing_feed_with_url(&opml, url_input.trim())
            {
                let names = containing_folder_names(&opml, &existing.url);
                status_label.add_css_class("error");
                status_label.set_text(&already_subscribed_error_text(&names));
                return;
            }

            *busy.borrow_mut() = true;
            add_btn.set_sensitive(false);
            status_label.set_text("Looking up the feed…");
            status_label.remove_css_class("error");

            let dialog_inner = dialog_weak.clone();
            let status_inner = status_label.clone();
            let busy_inner = busy.clone();
            let add_btn_inner = add_btn.clone();
            let parent_for_task = parent.downgrade();
            glib::spawn_future_local(async move {
                let cache = match parent_for_task.upgrade().map(|w| w.image_cache()) {
                    Some(c) => c,
                    None => return,
                };
                let client = cache.client().await;

                let (tx, rx) = tokio::sync::oneshot::channel();
                let url_for_task = url_input.clone();
                crate::spawn_on_runtime(async move {
                    let result = feed_discovery::discover_feed(&client, &url_for_task).await;
                    let _ = tx.send(result);
                });

                let discovered = match rx.await {
                    Ok(Ok(d)) => d,
                    Ok(Err(e)) => {
                        tracing::warn!(?e, url = %url_input, "feed discovery failed");
                        status_inner.add_css_class("error");
                        status_inner.set_text(
                            "No feed found at that URL. Check the address and try again.",
                        );
                        *busy_inner.borrow_mut() = false;
                        add_btn_inner.set_sensitive(true);
                        return;
                    }
                    Err(_) => {
                        status_inner.add_css_class("error");
                        status_inner.set_text("Discovery task crashed.");
                        *busy_inner.borrow_mut() = false;
                        add_btn_inner.set_sensitive(true);
                        return;
                    }
                };

                let final_name = name_input.or(discovered.title.clone());
                let display_name = final_name
                    .clone()
                    .unwrap_or_else(|| discovered.feed_url.clone());

                let Some(parent) = parent_for_task.upgrade() else {
                    return;
                };
                // NNW `7ea15d7f7` (#3758), site 2: discovery may
                // canonicalize the URL (redirects, HTML link
                // resolution), so re-check the resolved feed URL before
                // the add — the entered URL is not the only key a
                // subscription can exist under.
                if let Some(opml) = parent.opml_snapshot_public()
                    && let Some(existing) = existing_feed_with_url(&opml, &discovered.feed_url)
                {
                    let names = containing_folder_names(&opml, &existing.url);
                    status_inner.add_css_class("error");
                    status_inner.set_text(&already_subscribed_error_text(&names));
                    *busy_inner.borrow_mut() = false;
                    add_btn_inner.set_sensitive(true);
                    return;
                }
                let account = parent.account();
                let feed_url = discovered.feed_url.clone();
                let home_page_url = discovered.home_page_url.clone();
                let folder_for_task = folder_name.clone();
                let (add_tx, add_rx) = tokio::sync::oneshot::channel();
                crate::spawn_on_runtime(async move {
                    let _ = add_tx.send(
                        account
                            .add_feed(feed_url, final_name, home_page_url, folder_for_task)
                            .await,
                    );
                });
                match add_rx.await {
                    Ok(Ok(feed)) => {
                        // Persist the reader-view default BEFORE the
                        // first refresh of the feed, so the refresher's
                        // own settings writes can't race a fresh row.
                        // Same fetch-or-default shape as
                        // act_feed_settings' save path.
                        if reader_on {
                            let account = parent.account();
                            let feed_for_settings = feed.clone();
                            let (set_tx, set_rx) = tokio::sync::oneshot::channel();
                            crate::spawn_on_runtime(async move {
                                let existing = account
                                    .fetch_feed_settings(feed_for_settings.id.clone())
                                    .await
                                    .ok()
                                    .flatten();
                                let mut s =
                                    existing.unwrap_or_else(|| crate::models::FeedSettings {
                                        feed_id: feed_for_settings.id.clone(),
                                        feed_url: feed_for_settings.url.clone(),
                                        home_page_url: feed_for_settings.home_page_url.clone(),
                                        icon_url: None,
                                        favicon_url: None,
                                        edited_name: feed_for_settings.edited_name.clone(),
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
                                    });
                                s.reader_view_always_enabled = true;
                                let result = account.upsert_feed_settings(s).await;
                                let _ = set_tx.send(result);
                            });
                            if let Ok(Err(e)) = set_rx.await {
                                tracing::warn!(?e, "reader-view default upsert failed");
                            }
                        }
                        parent.show_toast_public(&format!("Added “{display_name}”."));
                        parent.reload_sidebar_after_opml_change();
                        parent.refresh_specific_feeds_public(vec![feed]);
                        if let Some(d) = dialog_inner.upgrade() {
                            d.close();
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "add_feed failed");
                        status_inner.add_css_class("error");
                        status_inner
                            .set_text("Failed to save the feed list. See the log for details.");
                        *busy_inner.borrow_mut() = false;
                        add_btn_inner.set_sensitive(true);
                    }
                    Err(_) => {
                        status_inner.add_css_class("error");
                        status_inner.set_text("Save task crashed.");
                        *busy_inner.borrow_mut() = false;
                        add_btn_inner.set_sensitive(true);
                    }
                }
            });
        }
    };

    let submit_for_btn = submit.clone();
    add_btn.connect_clicked(move |_| submit_for_btn());
    url_entry.connect_activate(move |_| submit());

    dialog.present();
}

/// Read the parent window's OPML and return the folder names the user
/// can pick from in the dialog. Doesn't hit the network or the DB —
/// we just walk the in-memory sidebar tree the window already has.
fn list_folder_names(parent: &ViaductWindow) -> Vec<String> {
    parent.list_folder_names_public()
}

/// Port of NNW `Account.existingFeed(withURL:)` (`7ea15d7f7`): the
/// already-subscribed lookup. Exact URL match; standalone feeds first,
/// then every folder — the same walk `Account::remove_feed` uses.
fn existing_feed_with_url<'a>(opml: &'a OpmlFile, url: &str) -> Option<&'a Feed> {
    opml.standalone_feeds
        .iter()
        .find(|f| f.url == url)
        .or_else(|| {
            opml.folders
                .iter()
                .flat_map(|folder| folder.feeds.iter())
                .find(|f| f.url == url)
        })
}

/// Port of NNW `Account.existingContainers(withFeed:)`, collapsed to
/// the part the message can show: the names of the folders containing
/// the feed, sorted. Top-level membership contributes no name (NNW's
/// `compactMap { ($0 as? Folder)?.nameForDisplay }` drops the account
/// the same way), so a feed that lives only at top level yields an
/// empty list and the generic message. Sorted with byte-order `sort`:
/// NNW uses `localizedStandardCompare`, which has no stdlib-only Rust
/// equivalent.
fn containing_folder_names(opml: &OpmlFile, feed_url: &str) -> Vec<String> {
    let mut names: Vec<String> = opml
        .folders
        .iter()
        .filter(|folder| folder.feeds.iter().any(|f| f.url == feed_url))
        .map(|folder| folder.name.clone())
        .collect();
    names.sort();
    names
}

/// NNW's `quotedNames.formatted(.list(type: .and))` for the en-US
/// shape the app ships: `“A”`, `“A” and “B”`, `“A”, “B”, and “C”`.
fn format_name_list(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| format!("“{n}”")).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {}", rest.join(", "), last),
    }
}

/// The already-subscribed status text (NNW `alreadySubscribedErrorText`,
/// with the follow-up `8795b7926` wording: "added", not "subscribed").
/// No containing folder resolves to the generic text, exactly upstream.
fn already_subscribed_error_text(folder_names: &[String]) -> String {
    if folder_names.is_empty() {
        return "Can’t add this feed because you’ve already added it.".to_string();
    }
    format!(
        "Can’t add this feed because you’ve already added it in {}.",
        format_name_list(folder_names)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Folder;

    fn feed(id: &str, url: &str) -> Feed {
        Feed {
            id: id.to_string(),
            url: url.to_string(),
            name: None,
            edited_name: None,
            home_page_url: None,
        }
    }

    /// Folders deliberately in reverse alphabetical order so the
    /// sorted result is distinguishable from OPML order. The same feed
    /// URL can legitimately live in two containers (`add_feed`'s
    /// dedupe is container-scoped), so both names must appear.
    fn opml() -> OpmlFile {
        OpmlFile {
            folders: vec![
                Folder {
                    name: "Zeta".to_string(),
                    feeds: vec![feed("f1", "https://example.com/feed.xml")],
                },
                Folder {
                    name: "alpha".to_string(),
                    feeds: vec![
                        feed("f2", "https://example.org/atom.xml"),
                        feed("f3", "https://example.com/feed.xml"),
                    ],
                },
            ],
            standalone_feeds: vec![feed("f4", "https://standalone.example/rss.xml")],
        }
    }

    #[test]
    fn existing_feed_lookup_finds_standalone_folder_nested_and_nothing() {
        let opml = opml();
        let found = existing_feed_with_url(&opml, "https://standalone.example/rss.xml")
            .expect("standalone feed should match");
        assert_eq!(found.id, "f4");
        let found = existing_feed_with_url(&opml, "https://example.org/atom.xml")
            .expect("folder-nested feed should match");
        assert_eq!(found.id, "f2");
        assert!(existing_feed_with_url(&opml, "https://example.com/other.xml").is_none());
    }

    #[test]
    fn containing_folders_list_every_copy_sorted() {
        let opml = opml();
        // Same URL subscribed in two folders: both names, sorted
        // (byte-order sorts "Zeta" before lowercase "alpha").
        assert_eq!(
            containing_folder_names(&opml, "https://example.com/feed.xml"),
            vec!["Zeta".to_string(), "alpha".to_string()]
        );
        // Top-level-only membership contributes no folder name — the
        // generic message path.
        assert!(containing_folder_names(&opml, "https://standalone.example/rss.xml").is_empty());
        assert!(containing_folder_names(&opml, "https://example.com/other.xml").is_empty());
    }

    #[test]
    fn name_list_matches_nnw_list_formatting() {
        assert_eq!(format_name_list(&[]), "");
        assert_eq!(format_name_list(&["News".to_string()]), "“News”");
        assert_eq!(
            format_name_list(&["News".to_string(), "Reading".to_string()]),
            "“News” and “Reading”"
        );
        assert_eq!(
            format_name_list(&[
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string()
            ]),
            "“A”, “B”, “C”, and “D”"
        );
    }

    #[test]
    fn already_subscribed_text_names_folders_only_when_known() {
        assert_eq!(
            already_subscribed_error_text(&[]),
            "Can’t add this feed because you’ve already added it."
        );
        assert_eq!(
            already_subscribed_error_text(&["News".to_string(), "Reading".to_string()]),
            "Can’t add this feed because you’ve already added it in “News” and “Reading”."
        );
    }
}

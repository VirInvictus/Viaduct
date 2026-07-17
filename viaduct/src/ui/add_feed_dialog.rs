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

    let (group, list) = rows::group(
        None,
        Some("Paste a feed URL or a website URL. Viaduct will look up the feed automatically."),
    );

    let (url_row, url_entry) = rows::entry_row("Feed or website URL", None, None);
    let (name_row, name_entry) = rows::entry_row("Name (optional)", None, None);

    let folder_names = list_folder_names(parent);
    let mut combo_labels: Vec<String> = vec!["None".to_string()];
    combo_labels.extend(folder_names.iter().cloned());
    let combo_strs: Vec<&str> = combo_labels.iter().map(|s| s.as_str()).collect();
    let (folder_row, folder_drop_down) = rows::combo_row(
        "Folder",
        Some("Where the feed will live in the sidebar"),
        &combo_strs,
    );

    list.append(&url_row);
    list.append(&name_row);
    list.append(&folder_row);

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
    content.append(&group);
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

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.6.23 — first-launch welcome dialog.
//!
//! Pre-v2.6.23 a fresh viaduct install opened to an empty sidebar with
//! no clear path forward. NewsFlash's `welcome_screen` is the
//! comparable surface. We don't have a remote-services picker — local
//! account is the default and Inoreader credentials are the only
//! optional path — so the dialog is leaner: a couple of suggested feeds,
//! shortcuts to Add Feed and Import OPML, and a visible promise that
//! viaduct stops bothering you after the first one.
//!
//! Visibility: shown once when (a) the OPML at startup is empty AND
//! (b) the `welcome-shown` GSetting is false. Setting the GSetting to
//! true on dismiss closes the loop. dconf-editor flip-back to false is
//! the supported "show me the welcome dialog again" path.

use gtk::glib;
use gtk::prelude::*;

use crate::ui::rows;
use crate::ui::window::ViaductWindow;

/// Curated suggested-feed list. Each entry seeds the OPML on click.
/// Picked for diversity (tech blog, mainstream news, daily comic,
/// long-form, technology magazine) and for being likely to still be
/// alive in 5 years. Keep the list small — a wall of buttons is
/// indistinguishable from "we don't know what to recommend".
const SUGGESTED_FEEDS: &[(&str, &str, &str)] = &[
    (
        "Daring Fireball",
        "Apple commentary by John Gruber.",
        "https://daringfireball.net/feeds/main",
    ),
    (
        "NPR News",
        "Top stories from NPR.",
        "https://feeds.npr.org/1001/rss.xml",
    ),
    (
        "xkcd",
        "A webcomic of romance, sarcasm, math, and language.",
        "https://xkcd.com/atom.xml",
    ),
    (
        "Hacker News",
        "Front-page links from news.ycombinator.com.",
        "https://hnrss.org/frontpage",
    ),
    (
        "Ars Technica",
        "Technology news and analysis.",
        "https://feeds.arstechnica.com/arstechnica/index",
    ),
];

/// Build and present the welcome dialog modal to `parent`.
pub fn present(parent: &ViaductWindow) {
    // Phase 20c: plain modal window. The `adw::Dialog` closed on
    // click-outside; a modal `gtk::Window` does not, so Escape and the
    // close button are the dismiss paths, both routed through
    // `close-request` below.
    let dialog = gtk::Window::builder()
        .title("Welcome to viaduct")
        .transient_for(parent)
        .modal(true)
        .default_width(520)
        .build();
    crate::ui::close_on_escape(&dialog);

    let header = gtk::HeaderBar::new();

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 18);
    outer.set_margin_top(24);
    outer.set_margin_bottom(24);
    outer.set_margin_start(24);
    outer.set_margin_end(24);

    let intro = gtk::Label::new(Some(
        "viaduct is an RSS reader. Subscribe to feeds, read in your own pace, no ads, no remote tracking. Start with a feed of your own or pick one below to try.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("body");
    outer.append(&intro);

    // Action buttons row.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    actions.set_homogeneous(true);

    let add_feed_btn = gtk::Button::with_label("Add a feed…");
    add_feed_btn.add_css_class("suggested-action");
    add_feed_btn.add_css_class("pill");
    let weak = parent.downgrade();
    add_feed_btn.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            if let Some(window) = weak.upgrade() {
                mark_welcome_shown();
                dialog.close();
                crate::ui::add_feed_dialog::present(&window);
            }
        }
    ));
    actions.append(&add_feed_btn);

    let import_btn = gtk::Button::with_label("Import OPML…");
    import_btn.add_css_class("pill");
    let weak = parent.downgrade();
    import_btn.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            if let Some(window) = weak.upgrade() {
                mark_welcome_shown();
                dialog.close();
                window.act_import_opml();
            }
        }
    ));
    actions.append(&import_btn);

    outer.append(&actions);

    // Suggested-feeds list.
    let suggested_label = gtk::Label::new(Some("Or try one of these:"));
    suggested_label.set_xalign(0.0);
    suggested_label.add_css_class("heading");
    outer.append(&suggested_label);

    let feed_list = gtk::ListBox::new();
    feed_list.add_css_class("boxed-list");
    feed_list.set_selection_mode(gtk::SelectionMode::None);

    for (name, subtitle, url) in SUGGESTED_FEEDS {
        let plus = gtk::Button::from_icon_name("list-add-symbolic");
        plus.set_valign(gtk::Align::Center);
        plus.add_css_class("flat");
        plus.set_tooltip_text(Some(&format!("Subscribe to {name}")));
        let row = rows::button_row(name, Some(subtitle), &plus);
        let parent_weak = parent.downgrade();
        let url_owned = (*url).to_string();
        let name_owned = (*name).to_string();
        plus.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| {
                let Some(window) = parent_weak.upgrade() else {
                    return;
                };
                row.set_activatable(false);
                let url_clone = url_owned.clone();
                let name_clone = name_owned.clone();
                let account = window.account();
                let weak_window = window.downgrade();
                // GTK side: spawn_future_local owns the result hop;
                // the tokio side does the DB write. Same pattern as
                // add_feed_dialog — never run an async fn that uses
                // tokio I/O directly inside spawn_future_local; never
                // run glib::spawn_future_local inside spawn_on_runtime.
                let (tx, rx) = tokio::sync::oneshot::channel();
                crate::spawn_on_runtime(async move {
                    let _ = tx.send(
                        account
                            .add_feed(url_clone, Some(name_clone), None, None)
                            .await,
                    );
                });
                glib::spawn_future_local(async move {
                    let Some(window) = weak_window.upgrade() else {
                        return;
                    };
                    match rx.await {
                        Ok(Ok(feed)) => {
                            window.show_toast_public(&format!("Added {}", feed.url));
                            window.reload_sidebar_after_opml_change();
                            window.refresh_specific_feeds_public(vec![feed]);
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(?e, "welcome: add_feed failed");
                            window.show_toast_public("Couldn't add feed — see log");
                        }
                        Err(_) => {
                            tracing::warn!("welcome: add_feed task aborted");
                        }
                    }
                });
                mark_welcome_shown();
            }
        ));
        feed_list.append(&row);
    }
    outer.append(&feed_list);

    let footer = gtk::Label::new(Some(
        "viaduct stops showing this dialog after you add your first feed.",
    ));
    footer.set_wrap(true);
    footer.set_xalign(0.0);
    footer.add_css_class("caption");
    footer.add_css_class("dim-label");
    outer.append(&footer);

    scroller.set_child(Some(&outer));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&scroller);
    dialog.set_child(Some(&content));

    // Always mark as shown when the dialog closes by any path — close
    // button or escape key (a modal `gtk::Window` doesn't close on an
    // outside click, so that former path is simply gone). Without this a
    // cautious user who closes without picking would see it every launch.
    dialog.connect_close_request(|_| {
        mark_welcome_shown();
        glib::Propagation::Proceed
    });

    dialog.present();
}

fn mark_welcome_shown() {
    if let Some(s) = crate::preferences::settings()
        && !s.boolean(crate::preferences::keys::WELCOME_SHOWN)
        && let Err(e) = s.set_boolean(crate::preferences::keys::WELCOME_SHOWN, true)
    {
        tracing::warn!(?e, "failed to write welcome-shown");
    }
}

/// Returns true if the welcome dialog should fire on this launch:
/// `welcome-shown` is false. The OPML-empty check is the caller's
/// concern — `wire_models` already has the loaded OPML in hand.
pub fn should_present() -> bool {
    crate::preferences::settings()
        .map(|s| !s.boolean(crate::preferences::keys::WELCOME_SHOWN))
        .unwrap_or(false)
}

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.6.24 — Activity Log dialog.
//!
//! NetNewsWire's "Activity Log" surface; NewsFlash has the same idea
//! under a different name. Reads a snapshot of the process-wide
//! `ActivityLog` ring buffer (most-recent first) and renders one row
//! per event grouped into AdwActionRows. Each entry says **what feed**,
//! **what happened** (success / 304 / HTTP error / network error /
//! parse error / DB error / skipped), and **when**. Closes the gap
//! between "did my refresh run?" and "why didn't this feed update?"
//! that previously required tailing `RUST_LOG=debug` output.

use gtk::prelude::*;

use chrono::{DateTime, Local, Utc};
use std::sync::Arc;

use crate::network::activity::{ActivityEvent, ActivityKind, ActivityLog, SkipReason};
use crate::ui::rows;
use crate::ui::window::ViaductWindow;

pub fn present(parent: &ViaductWindow) {
    let log = parent.activity_log();

    // Phase 20c: plain modal window. Was `adw::Dialog` + `ToolbarView` +
    // `HeaderBar`; the Clear button rides a `gtk::HeaderBar` set as the
    // window titlebar.
    let clear_btn = gtk::Button::with_label("Clear");
    clear_btn.add_css_class("flat");
    let header = gtk::HeaderBar::new();
    header.pack_end(&clear_btn);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_vexpand(true);

    let (group, list_box) = rows::group(
        Some("Recent feed activity"),
        Some("Newest first. Most recent 500 events are kept; clear to start fresh."),
    );
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 18);
    outer.set_margin_top(18);
    outer.set_margin_bottom(18);
    outer.set_margin_start(18);
    outer.set_margin_end(18);
    outer.append(&group);
    scroller.set_child(Some(&outer));

    let empty = status_page(
        "document-open-recent-symbolic",
        "No activity yet",
        "Refresh a feed to see what happens here. Successes, 304 not-modified, HTTP and network errors all show up.",
    );

    stack.add_named(&scroller, Some("content"));
    stack.add_named(&empty, Some("empty"));

    populate(&list_box, &stack, &log);

    let log_for_clear = log.clone();
    let list_for_clear = list_box.downgrade();
    let stack_for_clear = stack.downgrade();
    clear_btn.connect_clicked(move |_| {
        log_for_clear.clear();
        if let (Some(list), Some(stack)) = (list_for_clear.upgrade(), stack_for_clear.upgrade()) {
            populate(&list, &stack, &log_for_clear);
        }
    });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);
    content.append(&stack);

    let window = gtk::Window::builder()
        .title("Activity Log")
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(560)
        .child(&content)
        .build();
    crate::ui::close_on_escape(&window);
    window.present();
}

/// The `adw::StatusPage` replacement: a centred icon, title, and dim
/// wrapping description. No adwaita-specific behaviour to preserve, so this
/// is a plain composite rather than an owned widget.
fn status_page(icon: &str, title: &str, description: &str) -> gtk::Box {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .hexpand(true)
        .margin_start(24)
        .margin_end(24)
        .build();
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(48);
    image.add_css_class("dim-label");
    outer.append(&image);
    outer.append(
        &gtk::Label::builder()
            .label(title)
            .css_classes(["title-2"])
            .build(),
    );
    outer.append(
        &gtk::Label::builder()
            .label(description)
            .justify(gtk::Justification::Center)
            .wrap(true)
            .max_width_chars(48)
            .css_classes(["dim-label"])
            .build(),
    );
    outer
}

fn populate(list: &gtk::ListBox, stack: &gtk::Stack, log: &Arc<ActivityLog>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let snapshot = log.snapshot();
    if snapshot.is_empty() {
        stack.set_visible_child_name("empty");
        return;
    }
    stack.set_visible_child_name("content");
    for ev in snapshot {
        list.append(&row_for(&ev));
    }
}

fn row_for(ev: &ActivityEvent) -> gtk::ListBoxRow {
    let stamp = gtk::Label::new(Some(&format_when(ev.at)));
    stamp.add_css_class("dim-label");
    stamp.add_css_class("caption");
    rows::row(
        &display_title(ev),
        Some(&display_subtitle(ev)),
        Some(stamp.upcast_ref()),
    )
}

fn display_title(ev: &ActivityEvent) -> String {
    ev.feed_name.clone().unwrap_or_else(|| ev.feed_url.clone())
}

fn display_subtitle(ev: &ActivityEvent) -> String {
    match &ev.kind {
        ActivityKind::Success {
            new,
            updated,
            deleted,
        } => {
            if *new == 0 && *updated == 0 && *deleted == 0 {
                "Updated · no changes".to_string()
            } else {
                let mut parts = Vec::with_capacity(3);
                if *new > 0 {
                    parts.push(format!("{new} new"));
                }
                if *updated > 0 {
                    parts.push(format!("{updated} updated"));
                }
                if *deleted > 0 {
                    parts.push(format!("{deleted} removed"));
                }
                format!("Updated · {}", parts.join(", "))
            }
        }
        ActivityKind::NotModified => "Not modified (304)".to_string(),
        ActivityKind::HttpError { status } => format!("HTTP {status}"),
        ActivityKind::NetworkError { detail } => format!("Network error · {}", trim(detail)),
        ActivityKind::ParseError { detail } => format!("Parse error · {}", trim(detail)),
        ActivityKind::DbError { detail } => format!("Database error · {}", trim(detail)),
        ActivityKind::Skipped(reason) => match reason {
            SkipReason::DisallowedHost => "Skipped · disallowed host".to_string(),
            SkipReason::CacheControl => "Skipped · still fresh per Cache-Control".to_string(),
            SkipReason::Throttled => "Skipped · refreshed within the last 9 minutes".to_string(),
        },
    }
}

fn trim(s: &str) -> String {
    let one_line: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if one_line.chars().count() > 140 {
        let truncated: String = one_line.chars().take(137).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

fn format_when(at: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now.signed_duration_since(at);
    if delta.num_seconds() < 60 {
        return "Just now".to_string();
    }
    if delta.num_minutes() < 60 {
        let m = delta.num_minutes();
        return format!("{m} min ago");
    }
    if delta.num_hours() < 24 {
        let h = delta.num_hours();
        return format!("{h}h ago");
    }
    at.with_timezone(&Local).format("%b %-d, %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn ev(kind: ActivityKind) -> ActivityEvent {
        ActivityEvent {
            at: Utc::now(),
            feed_id: "f1".to_string(),
            feed_url: "https://example.com/feed".to_string(),
            feed_name: Some("Example".to_string()),
            kind,
        }
    }

    #[test]
    fn success_with_only_new_articles() {
        let row = display_subtitle(&ev(ActivityKind::Success {
            new: 3,
            updated: 0,
            deleted: 0,
        }));
        assert_eq!(row, "Updated · 3 new");
    }

    #[test]
    fn success_no_changes() {
        let row = display_subtitle(&ev(ActivityKind::Success {
            new: 0,
            updated: 0,
            deleted: 0,
        }));
        assert_eq!(row, "Updated · no changes");
    }

    #[test]
    fn skip_reason_strings() {
        assert!(
            display_subtitle(&ev(ActivityKind::Skipped(SkipReason::DisallowedHost)))
                .contains("disallowed")
        );
        assert!(
            display_subtitle(&ev(ActivityKind::Skipped(SkipReason::Throttled)))
                .contains("9 minutes")
        );
        assert!(
            display_subtitle(&ev(ActivityKind::Skipped(SkipReason::CacheControl)))
                .contains("Cache-Control")
        );
    }

    #[test]
    fn relative_time_just_now() {
        assert_eq!(format_when(Utc::now()), "Just now");
    }

    #[test]
    fn relative_time_hours() {
        let t = Utc::now() - Duration::hours(3);
        assert_eq!(format_when(t), "3h ago");
    }

    #[test]
    fn detail_trimming() {
        let long = "x".repeat(200);
        let out = trim(&long);
        assert!(out.chars().count() <= 138);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn fallback_title_uses_url_when_name_missing() {
        let mut e = ev(ActivityKind::NotModified);
        e.feed_name = None;
        assert_eq!(display_title(&e), "https://example.com/feed");
    }
}

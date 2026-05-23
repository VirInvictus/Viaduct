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

use adw::prelude::*;
use chrono::{DateTime, Local, Utc};
use std::sync::Arc;

use crate::network::activity::{ActivityEvent, ActivityKind, ActivityLog, SkipReason};
use crate::ui::window::ViaductWindow;

pub fn present(parent: &ViaductWindow) {
    let log = parent.activity_log();
    let dialog = adw::Dialog::new();
    dialog.set_title("Activity Log");
    dialog.set_content_width(640);
    dialog.set_content_height(560);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(true);

    let clear_btn = gtk::Button::with_label("Clear");
    clear_btn.add_css_class("flat");
    header.pack_end(&clear_btn);
    toolbar.add_top_bar(&header);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(false);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 18);
    outer.set_margin_top(18);
    outer.set_margin_bottom(18);
    outer.set_margin_start(18);
    outer.set_margin_end(18);

    let group = adw::PreferencesGroup::new();
    group.set_title("Recent feed activity");
    group.set_description(Some(
        "Newest first. Most recent 500 events are kept; clear to start fresh.",
    ));
    let list_box = gtk::ListBox::new();
    list_box.add_css_class("boxed-list");
    list_box.set_selection_mode(gtk::SelectionMode::None);
    group.add(&list_box);
    outer.append(&group);
    scroller.set_child(Some(&outer));

    let empty = adw::StatusPage::new();
    empty.set_icon_name(Some("document-open-recent-symbolic"));
    empty.set_title("No activity yet");
    empty.set_description(Some(
        "Refresh a feed to see what happens here. Successes, 304 not-modified, HTTP and network errors all show up.",
    ));

    stack.add_named(&scroller, Some("content"));
    stack.add_named(&empty, Some("empty"));
    toolbar.set_content(Some(&stack));
    dialog.set_child(Some(&toolbar));

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

    dialog.present(Some(parent));
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

fn row_for(ev: &ActivityEvent) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&display_title(ev));
    row.set_subtitle(&display_subtitle(ev));
    let stamp = gtk::Label::new(Some(&format_when(ev.at)));
    stamp.add_css_class("dim-label");
    stamp.add_css_class("caption");
    stamp.set_valign(gtk::Align::Center);
    row.add_suffix(&stamp);
    row
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
            SkipReason::Throttled => "Skipped · refreshed within the last 29 minutes".to_string(),
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
        assert!(display_subtitle(&ev(ActivityKind::Skipped(SkipReason::Throttled))).contains("29"));
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

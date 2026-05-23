// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! v2.7.0 — New Smart Feed dialog.
//!
//! Builds a Smart Feed by accepting a name and a list of rules. Rules
//! are AND-combined; the rule list grows dynamically with an Add Rule
//! button. Each rule row exposes a field combo, an entry / value
//! widget appropriate to the field, and a remove button.
//!
//! Out of scope for v2.7.0: OR / NOT, nested groups, edit-existing
//! (the sidebar right-click delete + recreate covers that for now).

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use chrono::Utc;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::smart_feeds::{Condition, SmartFeed, SmartFeedRules};
use crate::ui::window::ViaductWindow;

const FIELD_OPTIONS: &[(&str, &str)] = &[
    ("title_contains", "Title contains"),
    ("body_contains", "Body contains"),
    ("author_contains", "Author contains"),
    ("feed_is", "Feed is"),
    ("read", "Read"),
    ("starred", "Starred"),
    ("newer_than_days", "Newer than (days)"),
    ("older_than_days", "Older than (days)"),
];

const READ_STATE_OPTIONS: &[(&str, bool)] = &[("read", true), ("unread", false)];
const STAR_STATE_OPTIONS: &[(&str, bool)] = &[("starred", true), ("not starred", false)];

struct RuleRow {
    container: gtk::Box,
    field_combo: gtk::DropDown,
    value_stack: gtk::Stack,
    text_entry: gtk::Entry,
    feed_combo: gtk::DropDown,
    read_combo: gtk::DropDown,
    star_combo: gtk::DropDown,
    days_spin: gtk::SpinButton,
    feed_pairs: Vec<(String, String)>, // (feed_id, display_name)
}

impl RuleRow {
    fn new(feed_pairs: Vec<(String, String)>) -> Rc<Self> {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        container.set_margin_top(4);
        container.set_margin_bottom(4);

        let field_strings: Vec<String> = FIELD_OPTIONS
            .iter()
            .map(|(_, l)| (*l).to_string())
            .collect();
        let field_strs: Vec<&str> = field_strings.iter().map(|s| s.as_str()).collect();
        let field_combo = gtk::DropDown::from_strings(&field_strs);
        field_combo.set_valign(gtk::Align::Center);
        container.append(&field_combo);

        let value_stack = gtk::Stack::new();
        value_stack.set_hexpand(true);

        let text_entry = gtk::Entry::new();
        text_entry.set_placeholder_text(Some("Search text"));
        value_stack.add_named(&text_entry, Some("text"));

        let feed_strings: Vec<String> = feed_pairs.iter().map(|(_, n)| n.clone()).collect();
        let feed_strs: Vec<&str> = feed_strings.iter().map(|s| s.as_str()).collect();
        let feed_combo = if feed_pairs.is_empty() {
            gtk::DropDown::from_strings(&["(no feeds yet)"])
        } else {
            gtk::DropDown::from_strings(&feed_strs)
        };
        feed_combo.set_valign(gtk::Align::Center);
        value_stack.add_named(&feed_combo, Some("feed"));

        let read_strings: Vec<String> = READ_STATE_OPTIONS
            .iter()
            .map(|(l, _)| (*l).to_string())
            .collect();
        let read_strs: Vec<&str> = read_strings.iter().map(|s| s.as_str()).collect();
        let read_combo = gtk::DropDown::from_strings(&read_strs);
        read_combo.set_valign(gtk::Align::Center);
        value_stack.add_named(&read_combo, Some("read"));

        let star_strings: Vec<String> = STAR_STATE_OPTIONS
            .iter()
            .map(|(l, _)| (*l).to_string())
            .collect();
        let star_strs: Vec<&str> = star_strings.iter().map(|s| s.as_str()).collect();
        let star_combo = gtk::DropDown::from_strings(&star_strs);
        star_combo.set_valign(gtk::Align::Center);
        value_stack.add_named(&star_combo, Some("star"));

        let days_spin = gtk::SpinButton::with_range(1.0, 365.0, 1.0);
        days_spin.set_value(7.0);
        days_spin.set_valign(gtk::Align::Center);
        value_stack.add_named(&days_spin, Some("days"));

        container.append(&value_stack);

        let row = Rc::new(Self {
            container,
            field_combo: field_combo.clone(),
            value_stack: value_stack.clone(),
            text_entry,
            feed_combo,
            read_combo,
            star_combo,
            days_spin,
            feed_pairs,
        });

        let weak_row = Rc::downgrade(&row);
        field_combo.connect_selected_notify(move |combo| {
            let Some(r) = weak_row.upgrade() else {
                return;
            };
            r.show_value_for(combo.selected() as usize);
        });
        row.show_value_for(0);
        row
    }

    fn show_value_for(&self, idx: usize) {
        let key = FIELD_OPTIONS[idx].0;
        let stack_name = match key {
            "title_contains" | "body_contains" | "author_contains" => "text",
            "feed_is" => "feed",
            "read" => "read",
            "starred" => "star",
            "newer_than_days" | "older_than_days" => "days",
            _ => "text",
        };
        self.value_stack.set_visible_child_name(stack_name);
    }

    fn to_condition(&self) -> Option<Condition> {
        let key = FIELD_OPTIONS[self.field_combo.selected() as usize].0;
        match key {
            "title_contains" => {
                let v = self.text_entry.text().to_string();
                if v.trim().is_empty() {
                    None
                } else {
                    Some(Condition::TitleContains { value: v })
                }
            }
            "body_contains" => {
                let v = self.text_entry.text().to_string();
                if v.trim().is_empty() {
                    None
                } else {
                    Some(Condition::BodyContains { value: v })
                }
            }
            "author_contains" => {
                let v = self.text_entry.text().to_string();
                if v.trim().is_empty() {
                    None
                } else {
                    Some(Condition::AuthorContains { value: v })
                }
            }
            "feed_is" => {
                if self.feed_pairs.is_empty() {
                    return None;
                }
                let idx = self.feed_combo.selected() as usize;
                let (feed_id, _) = self.feed_pairs.get(idx)?;
                Some(Condition::FeedIs {
                    feed_id: feed_id.clone(),
                })
            }
            "read" => {
                let idx = self.read_combo.selected() as usize;
                let (_, read) = READ_STATE_OPTIONS.get(idx)?;
                Some(Condition::Read { read: *read })
            }
            "starred" => {
                let idx = self.star_combo.selected() as usize;
                let (_, starred) = STAR_STATE_OPTIONS.get(idx)?;
                Some(Condition::Starred { starred: *starred })
            }
            "newer_than_days" => Some(Condition::NewerThanDays {
                days: self.days_spin.value() as i64,
            }),
            "older_than_days" => Some(Condition::OlderThanDays {
                days: self.days_spin.value() as i64,
            }),
            _ => None,
        }
    }
}

pub fn present(parent: &ViaductWindow) {
    let feed_pairs = collect_feed_pairs(parent);

    let dialog = adw::Dialog::new();
    dialog.set_title("New Smart Feed");
    dialog.set_content_width(560);
    dialog.set_content_height(520);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(true);
    let cancel_btn = gtk::Button::with_label("Cancel");
    cancel_btn.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    header.pack_start(&cancel_btn);
    let save_btn = gtk::Button::with_label("Save");
    save_btn.add_css_class("suggested-action");
    header.pack_end(&save_btn);
    toolbar.add_top_bar(&header);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 12);
    outer.set_margin_top(18);
    outer.set_margin_bottom(18);
    outer.set_margin_start(18);
    outer.set_margin_end(18);

    let name_group = adw::PreferencesGroup::new();
    let name_row = adw::EntryRow::new();
    name_row.set_title("Name");
    name_group.add(&name_row);
    outer.append(&name_group);

    let rules_group = adw::PreferencesGroup::new();
    rules_group.set_title("Conditions");
    rules_group.set_description(Some("All conditions must match (AND). Add at least one."));
    let rules_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let rules_holder = adw::PreferencesRow::new();
    rules_holder.set_activatable(false);
    rules_holder.set_child(Some(&rules_box));
    rules_group.add(&rules_holder);

    let add_btn = gtk::Button::with_label("Add Condition");
    add_btn.add_css_class("flat");
    add_btn.set_halign(gtk::Align::Start);
    outer.append(&rules_group);
    outer.append(&add_btn);

    let footer = gtk::Label::new(Some(
        "Smart Feeds appear in the sidebar under \"My Smart Feeds\". They re-evaluate every time you click them.",
    ));
    footer.set_wrap(true);
    footer.set_xalign(0.0);
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");
    outer.append(&footer);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_child(Some(&outer));
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    let rule_rows: Rc<RefCell<Vec<Rc<RuleRow>>>> = Rc::new(RefCell::new(Vec::new()));

    let push_row = {
        let rule_rows = rule_rows.clone();
        let rules_box = rules_box.clone();
        let feed_pairs = feed_pairs.clone();
        move || {
            let row = RuleRow::new(feed_pairs.clone());
            let remove_btn = gtk::Button::from_icon_name("user-trash-symbolic");
            remove_btn.set_valign(gtk::Align::Center);
            remove_btn.add_css_class("flat");
            remove_btn.set_tooltip_text(Some("Remove this condition"));
            row.container.append(&remove_btn);
            rules_box.append(&row.container);

            let weak_box = rules_box.downgrade();
            let weak_container = row.container.downgrade();
            let rule_rows_for_remove = rule_rows.clone();
            let row_for_remove = row.clone();
            remove_btn.connect_clicked(move |_| {
                if let (Some(b), Some(c)) = (weak_box.upgrade(), weak_container.upgrade()) {
                    b.remove(&c);
                }
                rule_rows_for_remove
                    .borrow_mut()
                    .retain(|r| !std::ptr::eq(Rc::as_ptr(r), Rc::as_ptr(&row_for_remove)));
            });

            rule_rows.borrow_mut().push(row);
        }
    };

    push_row();

    let push_row_clone = push_row.clone();
    add_btn.connect_clicked(move |_| push_row_clone());

    let weak_window = parent.downgrade();
    let rule_rows_for_save = rule_rows.clone();
    let weak_dialog = dialog.downgrade();
    save_btn.connect_clicked(move |_| {
        let Some(window) = weak_window.upgrade() else {
            return;
        };
        let name = name_row.text().trim().to_string();
        if name.is_empty() {
            window.show_toast_public("Give your Smart Feed a name first.");
            return;
        }
        let conditions: Vec<Condition> = rule_rows_for_save
            .borrow()
            .iter()
            .filter_map(|r| r.to_condition())
            .collect();
        if conditions.is_empty() {
            window.show_toast_public("Add at least one non-empty condition.");
            return;
        }
        let feed = SmartFeed {
            id: new_smart_feed_id(),
            name,
            rules: SmartFeedRules { conditions },
            created_at: Utc::now(),
        };
        let account = window.account();
        let weak_window_inner = window.downgrade();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::spawn_on_runtime(async move {
            let _ = tx.send(account.add_smart_feed(feed).await);
        });
        glib::spawn_future_local(async move {
            let Some(window) = weak_window_inner.upgrade() else {
                return;
            };
            match rx.await {
                Ok(Ok(saved)) => {
                    window.show_toast_public(&format!("Smart Feed “{}” added.", saved.name));
                    window.reload_custom_smart_feeds();
                }
                Ok(Err(e)) => {
                    tracing::warn!(?e, "add_smart_feed failed");
                    window.show_toast_public("Couldn't save the Smart Feed.");
                }
                Err(_) => {
                    tracing::warn!("add_smart_feed task aborted");
                }
            }
        });
        if let Some(d) = weak_dialog.upgrade() {
            d.close();
        }
    });

    dialog.present(Some(parent));
}

fn collect_feed_pairs(window: &ViaductWindow) -> Vec<(String, String)> {
    let names = window.imp().sidebar_view.get().feed_names();
    let mut pairs: Vec<(String, String)> = names
        .borrow()
        .iter()
        .map(|(id, name)| (id.clone(), name.clone()))
        .collect();
    pairs.sort_by_key(|a| a.1.to_lowercase());
    pairs
}

fn new_smart_feed_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sf-{:032x}", nanos)
}

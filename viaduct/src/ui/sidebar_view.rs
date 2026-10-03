// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Phase 18 / v2.0.0-pre3 — `ViaductSidebarView`. Owns the sidebar list
//! view, the OPML-derived tree (delegate / controller / data source),
//! the per-feed display-name resolver consumed by the timeline factory,
//! the right-click context menus for feed and folder rows, and the
//! sidebar header bar with its mark-all-read / sync / search / menu
//! buttons. Lifted out of `ViaductWindow` so the god-object shrinks one
//! pane at a time. Window-side action bodies (`act_*_feed` / `_folder`,
//! `act_mark_all_read`, `act_refresh`) read context through the
//! accessors here; the cross-pane orchestration (sidebar selection
//! drives timeline fetch, status mutations refresh unread counts) stays
//! in the window.
//!
//! NetNewsWire counterpart: `Mac/MainWindow/Sidebar/SidebarViewController.swift`.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::database::accounts::Account;
use crate::network::ImageCache;
use crate::ui::sidebar::{
    SidebarDataSource, SidebarItem, SidebarTreeControllerDelegate, setup_sidebar_list_view,
};
use crate::ui::timeline::FeedNameMap;

/// Twin of [`FeedNameMap`]: feed id → feed URL, rebuilt on every OPML
/// apply. Consumed by the article pane's per-feed rendering special
/// cases (NNW #5460).
pub type FeedUrlMap = Rc<RefCell<HashMap<String, String>>>;
use crate::ui::tree::{TreeController, TreeNode};

/// Single-flight + dirty flag for unread-count rounds. Distilled from
/// NNW `04e1a054a` (`Account._fetchAllUnreadCounts`) and `655883214`
/// (`SmartFeed.fetchUnreadCounts`): status changes arrive continuously
/// while a count round runs, so only one round is ever in flight, and
/// a request landing mid-flight just raises the dirty flag instead of
/// starting a second query. Completion releases the flight and reports
/// whether exactly one follow-up round must run. Kept as a plain value
/// type so the collapse semantics are unit-testable without GTK; the
/// `RefCell` holding it lives on the sidebar imp and never crosses a
/// thread.
#[derive(Default)]
pub(crate) struct UnreadCountFlight {
    in_flight: bool,
    needs_refetch: bool,
}

impl UnreadCountFlight {
    /// A request arrived: `true` when the caller must start a query
    /// round, `false` when a round is already running and this request
    /// collapsed into its follow-up.
    fn request(&mut self) -> bool {
        if self.in_flight {
            self.needs_refetch = true;
            return false;
        }
        self.in_flight = true;
        true
    }

    /// A round finished: releases the flight and reports whether at
    /// most one follow-up round must run (something changed while this
    /// one ran). Never arms more than one, so a burst of N overlapping
    /// requests costs one round plus one follow-up, not N rounds.
    fn complete(&mut self) -> bool {
        self.in_flight = false;
        std::mem::take(&mut self.needs_refetch)
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(file = "sidebar_view.ui")]
    pub struct SidebarView {
        #[template_child]
        pub mark_all_read_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub sync_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub sync_btn_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub sync_btn_spinner: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub sidebar_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub primary_menu: TemplateChild<gio::Menu>,

        pub delegate: OnceCell<Rc<RefCell<SidebarTreeControllerDelegate>>>,
        pub controller: OnceCell<Rc<TreeController>>,
        pub data_source: OnceCell<Rc<SidebarDataSource>>,
        pub selection: OnceCell<gtk::SingleSelection>,
        pub feed_names: OnceCell<FeedNameMap>,
        pub feed_urls: OnceCell<FeedUrlMap>,
        pub feed_popover: OnceCell<gtk::PopoverMenu>,
        pub folder_popover: OnceCell<gtk::PopoverMenu>,
        pub smart_feed_popover: OnceCell<gtk::PopoverMenu>,
        /// Right-click context. The gesture handler stashes the model
        /// object before showing the popover; action bodies on
        /// `ViaductWindow` take it via the accessors below.
        pub right_clicked_feed: RefCell<Option<crate::models::Feed>>,
        pub right_clicked_folder: RefCell<Option<crate::models::Folder>>,
        /// Single-flight state for unread-count rounds. GTK-main-thread
        /// only, like every other field here; see [`UnreadCountFlight`].
        pub(crate) unread_counts_flight: RefCell<UnreadCountFlight>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SidebarView {
        const NAME: &'static str = "ViaductSidebarView";
        type Type = super::SidebarView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Phase 20c: what `adw::Bin` was for. BinLayout gives the same
            // "size to my one child" behaviour with no libadwaita.
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SidebarView {
        // `adw::Bin` unparented its child for us; plain `gtk::Widget`
        // does not, and GTK warns about surviving children at finalize.
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for SidebarView {}
}

glib::wrapper! {
    pub struct SidebarView(ObjectSubclass<imp::SidebarView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for SidebarView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SidebarView {
    /// Construct the tree (delegate → controller → data source), bind
    /// it to the list view via `setup_sidebar_list_view`, build the two
    /// right-click popover menus, and attach the gesture controller
    /// that resolves the clicked row to a feed / folder model object
    /// and stashes it for the action handlers. Run once from
    /// `ViaductWindow::wire_models`.
    pub fn bootstrap(&self, account: Arc<Account>, image_cache: Arc<ImageCache>) {
        use gtk::gdk;

        let imp = self.imp();

        // Tree primitives. Same construction order NNW uses.
        let delegate = Rc::new(RefCell::new(SidebarTreeControllerDelegate::new()));
        let controller = Rc::new(TreeController::new_with_generic_root(
            Rc::downgrade(&delegate) as _,
        ));
        let data_source = Rc::new(SidebarDataSource::new());
        data_source.set_tree_controller(controller.clone());

        let selection = setup_sidebar_list_view(
            &imp.sidebar_list_view,
            &data_source,
            account.clone(),
            image_cache,
        );

        let _ = imp.delegate.set(delegate);
        let _ = imp.controller.set(controller);
        let _ = imp.data_source.set(data_source);
        let _ = imp.selection.set(selection);

        // Feed-name resolver: starts empty, populated by `apply_opml`
        // on every OPML load / import. The timeline factory clones this
        // Rc and reads through it on every row bind.
        let feed_names: FeedNameMap = Rc::new(RefCell::new(HashMap::new()));
        let _ = imp.feed_names.set(feed_names);
        // Twin resolver (feed id → feed URL) for the article pane's
        // per-feed rendering special cases (NNW #5460).
        let feed_urls: FeedUrlMap = Rc::new(RefCell::new(HashMap::new()));
        let _ = imp.feed_urls.set(feed_urls);

        // ---- Sidebar feed popover ----
        let feed_menu = gio::Menu::new();
        let read_section = gio::Menu::new();
        read_section.append(Some("Mark All as Read"), Some("win.mark-feed-read"));
        feed_menu.append_section(None, &read_section);
        let net_section = gio::Menu::new();
        net_section.append(Some("Refresh"), Some("win.refresh-feed"));
        net_section.append(Some("Copy Feed URL"), Some("win.copy-feed-url"));
        feed_menu.append_section(None, &net_section);
        // v2.1.0 + v2.4.0: feed organization + settings
        let edit_section = gio::Menu::new();
        edit_section.append(Some("Rename Feed…"), Some("win.rename-feed"));
        edit_section.append(Some("Move to Folder…"), Some("win.move-feed"));
        edit_section.append(Some("Feed Settings…"), Some("win.feed-settings"));
        feed_menu.append_section(None, &edit_section);
        let danger_section = gio::Menu::new();
        danger_section.append(Some("Delete Feed"), Some("win.delete-feed"));
        feed_menu.append_section(None, &danger_section);

        let feed_popover = gtk::PopoverMenu::from_model(Some(&feed_menu));
        feed_popover.set_has_arrow(false);
        feed_popover.set_parent(&imp.sidebar_list_view.get());
        let _ = imp.feed_popover.set(feed_popover);

        // ---- Sidebar folder popover (smaller — just mark-read) ----
        let folder_menu = gio::Menu::new();
        folder_menu.append(Some("Mark All as Read"), Some("win.mark-folder-read"));
        let folder_popover = gtk::PopoverMenu::from_model(Some(&folder_menu));
        folder_popover.set_has_arrow(false);
        folder_popover.set_parent(&imp.sidebar_list_view.get());
        let _ = imp.folder_popover.set(folder_popover);

        // ---- Custom Smart Feed popover (v2.7.0) — just delete ----
        let sf_menu = gio::Menu::new();
        sf_menu.append(Some("Delete Smart Feed"), Some("win.delete-smart-feed"));
        let sf_popover = gtk::PopoverMenu::from_model(Some(&sf_menu));
        sf_popover.set_has_arrow(false);
        sf_popover.set_parent(&imp.sidebar_list_view.get());
        let _ = imp.smart_feed_popover.set(sf_popover);

        let sidebar_gesture = gtk::GestureClick::new();
        sidebar_gesture.set_button(gdk::BUTTON_SECONDARY);
        let weak = self.downgrade();
        sidebar_gesture.connect_pressed(move |_, _n_press, x, y| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            let listview = view.imp().sidebar_list_view.get();
            let Some(item) = pick_sidebar_item_at(listview.upcast_ref::<gtk::Widget>(), x, y)
            else {
                return;
            };
            match item {
                SidebarItem::Feed(feed) => {
                    *view.imp().right_clicked_feed.borrow_mut() = Some(feed);
                    view.show_feed_popover(x, y);
                }
                SidebarItem::Folder(folder) => {
                    *view.imp().right_clicked_folder.borrow_mut() = Some(folder);
                    view.show_folder_popover(x, y);
                }
                SidebarItem::CustomSmartFeed(sf) => {
                    if let Some(window) = view
                        .root()
                        .and_then(|r| r.dynamic_cast::<crate::ui::window::ViaductWindow>().ok())
                    {
                        *window.imp().right_clicked_smart_feed.borrow_mut() = Some(sf);
                        view.show_smart_feed_popover(x, y);
                    }
                }
                // Built-in smart feeds + group rows have no destructive
                // actions to expose — skip the popover entirely.
                _ => {}
            }
        });
        imp.sidebar_list_view.add_controller(sidebar_gesture);
    }

    // -------------------- Public accessors --------------------

    pub fn list_view(&self) -> gtk::ListView {
        self.imp().sidebar_list_view.get()
    }

    pub fn selection(&self) -> gtk::SingleSelection {
        self.imp()
            .selection
            .get()
            .cloned()
            .expect("SidebarView used before bootstrap")
    }

    pub fn search_btn(&self) -> gtk::ToggleButton {
        self.imp().search_btn.get()
    }

    pub fn mark_all_read_btn(&self) -> gtk::Button {
        self.imp().mark_all_read_btn.get()
    }

    pub fn sync_btn(&self) -> gtk::Button {
        self.imp().sync_btn.get()
    }

    pub fn primary_menu(&self) -> gio::Menu {
        self.imp().primary_menu.get()
    }

    pub fn feed_names(&self) -> FeedNameMap {
        self.imp()
            .feed_names
            .get()
            .cloned()
            .expect("SidebarView used before bootstrap")
    }

    pub fn feed_urls(&self) -> FeedUrlMap {
        self.imp()
            .feed_urls
            .get()
            .cloned()
            .expect("SidebarView used before bootstrap")
    }

    /// Read & clear the right-clicked feed cell. Action bodies on
    /// `ViaductWindow` (`act_refresh_clicked_feed`, `act_copy_clicked_feed_url`,
    /// `act_delete_clicked_feed`, `act_mark_clicked_feed_read`) call this so
    /// a stale value can't bleed into a later keyboard activation.
    pub fn take_right_clicked_feed(&self) -> Option<crate::models::Feed> {
        self.imp().right_clicked_feed.borrow_mut().take()
    }

    pub fn take_right_clicked_folder(&self) -> Option<crate::models::Folder> {
        self.imp().right_clicked_folder.borrow_mut().take()
    }

    pub fn controller(&self) -> Option<Rc<TreeController>> {
        self.imp().controller.get().cloned()
    }

    pub fn delegate(&self) -> Option<Rc<RefCell<SidebarTreeControllerDelegate>>> {
        self.imp().delegate.get().cloned()
    }

    pub fn data_source(&self) -> Option<Rc<SidebarDataSource>> {
        self.imp().data_source.get().cloned()
    }

    /// Snapshot of the current OPML's folder names — used by the Add
    /// Feed dialog to populate its destination dropdown.
    pub fn list_folder_names(&self) -> Vec<String> {
        let Some(delegate) = self.imp().delegate.get() else {
            return Vec::new();
        };
        let delegate = delegate.borrow();
        let Some(opml) = delegate.opml_file.borrow().clone() else {
            return Vec::new();
        };
        opml.folders.iter().map(|f| f.name.clone()).collect()
    }

    /// The whole in-memory OPML tree, for callers that need more than
    /// the folder-name list — the Add Feed dialog's already-subscribed
    /// check (NNW `7ea15d7f7`) walks folders and standalone feeds.
    /// `None` before the startup OPML load lands.
    pub fn opml_snapshot(&self) -> Option<Rc<crate::database::opml::OpmlFile>> {
        let delegate = self.imp().delegate.get()?;
        delegate.borrow().opml_file.borrow().clone()
    }

    /// NNW `b4361413f` (#4221): the folder the Add Feed dialog
    /// preselects mirrors the current sidebar selection. A selected
    /// folder selects itself; a selected feed selects its containing
    /// folder (`containerForNode`: feed → `node.parent`); a
    /// standalone feed, a smart feed, a group row, or no selection
    /// means top-level (`None`).
    pub fn selected_add_feed_folder(&self) -> Option<String> {
        let selection = self.selection();
        let item = selection.selected_item()?;
        let row = item.downcast_ref::<gtk::TreeListRow>()?;
        let node = row.item().and_downcast::<TreeNode>()?;
        let selected = node
            .represented_object()?
            .downcast_ref::<SidebarItem>()?
            .clone();
        let parent = node
            .parent()
            .and_then(|p| p.represented_object())
            .and_then(|obj| obj.downcast_ref::<SidebarItem>().cloned());
        initial_add_feed_folder(&selected, parent.as_ref())
    }

    /// Flip the sync button between its icon and an in-progress spinner.
    /// Paired with refresh start / completion in `ViaductWindow::act_refresh`.
    pub fn set_refresh_in_progress(&self, on: bool) {
        let imp = self.imp();
        if on {
            imp.sync_btn_spinner.start();
            imp.sync_btn_stack.set_visible_child_name("spinner");
        } else {
            imp.sync_btn_spinner.stop();
            imp.sync_btn_stack.set_visible_child_name("icon");
        }
    }

    /// Fully apply a freshly-loaded `OpmlFile`: rebuild the feed-name
    /// resolver, push the OPML into the delegate, kick the controller
    /// to rebuild its tree nodes, and refresh the data-source root.
    /// Consumes the OpmlFile because `SidebarTreeControllerDelegate::set_opml_file`
    /// takes it by value (and it isn't Clone).
    pub fn apply_opml(&self, opml: crate::database::opml::OpmlFile) {
        self.rebuild_feed_names_from(&opml);
        if let Some(delegate) = self.imp().delegate.get() {
            delegate.borrow().set_opml_file(opml);
        }
        if let Some(controller) = self.imp().controller.get() {
            controller.rebuild();
        }
        if let Some(data_source) = self.imp().data_source.get() {
            data_source.refresh_root();
        }
    }

    /// v2.7.0 — replace the user-defined Smart Feed list and rebuild
    /// the sidebar tree. Called from `wire_models` on startup after
    /// `Account::list_smart_feeds`, and from the new-smart-feed /
    /// delete-smart-feed action bodies.
    pub fn apply_custom_smart_feeds(&self, feeds: Vec<crate::smart_feeds::SmartFeed>) {
        if let Some(delegate) = self.imp().delegate.get() {
            delegate.borrow().set_custom_smart_feeds(feeds);
        }
        if let Some(controller) = self.imp().controller.get() {
            controller.rebuild();
        }
        if let Some(data_source) = self.imp().data_source.get() {
            data_source.refresh_root();
        }
    }

    /// Walk the tree and set leaf-level unread counts. **v2.0.0-pre5**:
    /// folder and smart-feed-group totals auto-aggregate via the
    /// `notify::unread-count` subscriptions wired in
    /// `TreeNode::set_child_nodes`, so the imperative parent-sum
    /// bookkeeping is gone — we just touch leaves and let the cascade
    /// propagate. Triggered after every status mutation, refresh-cycle
    /// completion, OPML load, and OPML import.
    ///
    /// Port of NNW `04e1a054a` / `655883214` (unread-count single-flight
    /// coalescing): the triggers fire per status mutation, so holding
    /// Down through a timeline used to start one pooled
    /// `UnreadCountsByFeed` query plus one full tree walk per row
    /// opened, all overlapping. Now: a request arriving while a round
    /// runs only raises the dirty flag, and each finished round runs at
    /// most one follow-up, so a burst collapses to one round plus one.
    /// Chosen mechanism: NNW's literal flag pair, not a glib
    /// debounce — a `timeout_add` would delay every badge update by its
    /// interval (the single-flight pair adds none; a round is at most
    /// one query behind) and still needs re-arm bookkeeping to collapse
    /// a burst, while the flags are the exact shape upstream proved.
    /// All state is GTK-main-thread (`RefCell` on the imp; the round
    /// future runs on the main loop via `spawn_future_local`, with no
    /// borrow held across an await), the same place NNW pins the flags
    /// on `Account` / `SmartFeed`.
    pub fn refresh_unread_counts(&self, account: Arc<Account>) {
        let Some(controller) = self.imp().controller.get().cloned() else {
            return;
        };
        if !self.imp().unread_counts_flight.borrow_mut().request() {
            return;
        }

        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let view = weak.upgrade();
            let per_feed = match account.unread_counts_by_feed().await {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::debug!(?e, "unread_counts_by_feed failed");
                    None
                }
            };
            let smart = if per_feed.is_some() {
                account.smart_feed_counts().await.ok()
            } else {
                None
            };

            if let Some(per_feed) = per_feed {
                let to_u32 = |n: i64| n.max(0).min(u32::MAX as i64) as u32;
                let count_for_feed = |id: &str| to_u32(per_feed.get(id).copied().unwrap_or(0));

                for top in controller.root_node.child_nodes() {
                    let Some(rep) = top.represented_object() else {
                        continue;
                    };
                    let Some(item) = rep.downcast_ref::<SidebarItem>() else {
                        continue;
                    };
                    match item {
                        SidebarItem::Feed(feed) => {
                            // Standalone feed (not in a folder).
                            top.set_unread_count(count_for_feed(&feed.id));
                        }
                        SidebarItem::Folder(_)
                        | SidebarItem::SmartFeedGroup
                        | SidebarItem::CustomSmartFeedsGroup => {
                            // Container — only set leaves; total auto-sums.
                            for child in top.child_nodes() {
                                let Some(c_rep) = child.represented_object() else {
                                    continue;
                                };
                                let Some(c_item) = c_rep.downcast_ref::<SidebarItem>() else {
                                    continue;
                                };
                                match c_item {
                                    SidebarItem::Feed(feed) => {
                                        child.set_unread_count(count_for_feed(&feed.id));
                                    }
                                    SidebarItem::SmartFeed(name) => {
                                        let count = match (name.as_str(), smart) {
                                            ("Today", Some(s)) => to_u32(s.today_unread),
                                            ("All Unread", Some(s)) => to_u32(s.all_unread),
                                            ("Starred", Some(s)) => to_u32(s.starred_unread),
                                            _ => 0,
                                        };
                                        child.set_unread_count(count);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        SidebarItem::SmartFeed(_) | SidebarItem::CustomSmartFeed(_) => {}
                    }
                }
            }

            // Round complete: release the flight, then honor at most one
            // re-run if any request collapsed while this round ran. Runs
            // on the query-error path too, so a failed round doesn't
            // swallow the follow-up and strand the badges stale.
            if let Some(view) = view {
                let rerun = view.imp().unread_counts_flight.borrow_mut().complete();
                if rerun {
                    view.refresh_unread_counts(account);
                }
            }
        });
    }

    /// Tear down the right-click popovers before the listview finalizes.
    /// Call from `ViaductWindow::connect_close_request`'s quit branch.
    /// Without this GTK emits a "Finalizing GtkListView, but it still
    /// has children left: GtkPopoverMenu" warning at exit. Non-fatal
    /// but ugly in the logs.
    pub fn unparent_popovers(&self) {
        let imp = self.imp();
        if let Some(p) = imp.feed_popover.get() {
            p.unparent();
        }
        if let Some(p) = imp.folder_popover.get() {
            p.unparent();
        }
        if let Some(p) = imp.smart_feed_popover.get() {
            p.unparent();
        }
    }

    // -------------------- Internal helpers --------------------

    fn show_feed_popover(&self, x: f64, y: f64) {
        let Some(popover) = self.imp().feed_popover.get() else {
            return;
        };
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }

    fn show_folder_popover(&self, x: f64, y: f64) {
        let Some(popover) = self.imp().folder_popover.get() else {
            return;
        };
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }

    fn show_smart_feed_popover(&self, x: f64, y: f64) {
        let Some(popover) = self.imp().smart_feed_popover.get() else {
            return;
        };
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    }

    fn rebuild_feed_names_from(&self, opml: &crate::database::opml::OpmlFile) {
        let Some(map_rc) = self.imp().feed_names.get() else {
            return;
        };
        let mut map = map_rc.borrow_mut();
        map.clear();
        for feed in &opml.standalone_feeds {
            map.insert(feed.id.clone(), display_name_for_feed(feed));
        }
        for folder in &opml.folders {
            for feed in &folder.feeds {
                map.insert(feed.id.clone(), display_name_for_feed(feed));
            }
        }
        // The twin map the article pane's rendering special cases key on
        // (Slashdot paragraph separation, NNW #5460): feed id → feed URL.
        if let Some(url_map_rc) = self.imp().feed_urls.get() {
            let mut url_map = url_map_rc.borrow_mut();
            url_map.clear();
            for feed in &opml.standalone_feeds {
                url_map.insert(feed.id.clone(), feed.url.clone());
            }
            for folder in &opml.folders {
                for feed in &folder.feeds {
                    url_map.insert(feed.id.clone(), feed.url.clone());
                }
            }
        }
    }
}

/// Resolve a friendly display name for a feed. Mirrors NNW's
/// `WebFeed.nameForDisplay` semantics for the local account: edited
/// override → parsed name → URL host → raw URL.
fn display_name_for_feed(feed: &crate::models::Feed) -> String {
    if let Some(edited) = feed.edited_name.as_deref()
        && !edited.is_empty()
    {
        return edited.to_string();
    }
    if let Some(name) = feed.name.as_deref()
        && !name.is_empty()
    {
        return name.to_string();
    }
    if let Ok(parsed) = url::Url::parse(&feed.url)
        && let Some(host) = parsed.host_str()
    {
        return host.to_string();
    }
    feed.url.clone()
}

/// Pure decision table behind `selected_add_feed_folder` (NNW
/// `b4361413f`: `containerForNode` plus
/// `AddFeedDefaultContainer.substituteContainerIfNeeded`). `parent`
/// is the tree parent's item, already flattened: the generic root row
/// downcasts to `None`, which is also what NNW's account container
/// maps to here — viaduct's account model has no
/// `.disallowFeedInRootFolder` behavior (local and Inoreader both
/// allow top-level feeds), so `substituteContainerIfNeeded` never
/// substitutes and the account means top-level. NNW falls back to its
/// last-used default container for smart-feed rows; viaduct's dialog
/// has no last-used memory, so they fall back to top-level like every
/// other non-container selection.
fn initial_add_feed_folder(selected: &SidebarItem, parent: Option<&SidebarItem>) -> Option<String> {
    match selected {
        SidebarItem::Folder(folder) => Some(folder.name.clone()),
        SidebarItem::Feed(_) => match parent {
            Some(SidebarItem::Folder(folder)) => Some(folder.name.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Walk the sidebar list view's child widget tree from the click
/// coordinates up to the first ancestor that has `viaduct-sidebar-item`
/// data attached during the row factory's `connect_bind`. Used by the
/// right-click gesture handler to recover the clicked SidebarItem.
fn pick_sidebar_item_at(listview: &gtk::Widget, x: f64, y: f64) -> Option<SidebarItem> {
    let leaf = listview.pick(x, y, gtk::PickFlags::DEFAULT)?;
    let mut walker: Option<gtk::Widget> = Some(leaf);
    while let Some(w) = walker {
        unsafe {
            if let Some(ptr) = w.data::<SidebarItem>("viaduct-sidebar-item") {
                return Some(ptr.as_ref().clone());
            }
        }
        walker = w.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Feed, Folder};

    fn feed(id: &str) -> Feed {
        Feed {
            id: id.to_string(),
            url: format!("https://example.com/{id}.xml"),
            name: None,
            edited_name: None,
            home_page_url: None,
        }
    }

    fn folder(name: &str, feed_ids: &[&str]) -> Folder {
        Folder {
            name: name.to_string(),
            feeds: feed_ids.iter().map(|id| feed(id)).collect(),
        }
    }

    #[test]
    fn initial_folder_selected_folder_selects_itself() {
        let selected = SidebarItem::Folder(folder("News", &["f1"]));
        assert_eq!(
            initial_add_feed_folder(&selected, None).as_deref(),
            Some("News")
        );
        // NNW ignores the parent for container rows; a folder row
        // selects itself regardless of what sits above it.
        let parent = Some(SidebarItem::Folder(folder("Other", &[])));
        assert_eq!(
            initial_add_feed_folder(&selected, parent.as_ref()).as_deref(),
            Some("News")
        );
    }

    #[test]
    fn initial_folder_feed_selects_its_containing_folder() {
        let selected = SidebarItem::Feed(feed("f1"));
        let parent = Some(SidebarItem::Folder(folder("News", &["f1"])));
        assert_eq!(
            initial_add_feed_folder(&selected, parent.as_ref()).as_deref(),
            Some("News")
        );
    }

    #[test]
    fn initial_folder_non_container_selections_fall_back_to_top_level() {
        // Standalone feed: the tree parent is the generic root, which
        // downcasts to no SidebarItem (NNW: parent container is the
        // account, and our accounts allow top-level feeds).
        let selected = SidebarItem::Feed(feed("f1"));
        assert_eq!(initial_add_feed_folder(&selected, None), None);
        // Smart feeds and group rows have no container in the tree.
        let smart = SidebarItem::SmartFeed("Today".to_string());
        assert_eq!(initial_add_feed_folder(&smart, None), None);
        let group = SidebarItem::SmartFeedGroup;
        assert_eq!(initial_add_feed_folder(&group, None), None);
        let custom = SidebarItem::CustomSmartFeed(crate::smart_feeds::SmartFeed {
            id: "sf-1".to_string(),
            name: "SF".to_string(),
            rules: Default::default(),
            created_at: chrono::Utc::now(),
        });
        assert_eq!(initial_add_feed_folder(&custom, None), None);
        // A feed whose parent is a non-folder row is not a real
        // containing folder either.
        let group_parent = Some(SidebarItem::SmartFeedGroup);
        assert_eq!(
            initial_add_feed_folder(&selected, group_parent.as_ref()),
            None
        );
    }

    #[test]
    fn unread_count_flight_collapses_a_burst_into_one_round_plus_followup() {
        let mut f = UnreadCountFlight::default();
        // The first request starts the single in-flight round...
        assert!(f.request());
        // ...and every request landing while it runs collapses; none
        // starts a second query.
        for _ in 0..25 {
            assert!(!f.request());
        }
        // Completion releases the flight and runs exactly one follow-up
        // (NNW needsRefetch), no matter how many requests collapsed.
        assert!(f.complete());
        assert!(f.request());
        // A clean follow-up completes without arming another round.
        assert!(!f.complete());
    }

    #[test]
    fn unread_count_flight_clean_round_runs_no_followup() {
        let mut f = UnreadCountFlight::default();
        assert!(f.request());
        // Nothing changed while the round ran: no re-run armed.
        assert!(!f.complete());
        // And the machine is idle-reusable for the next burst.
        assert!(f.request());
        assert!(!f.request());
        assert!(f.complete());
        assert!(!f.complete());
    }

    #[test]
    fn unread_count_flight_collapsed_requests_are_never_lost() {
        // The badge may lag a burst by one round, never by the burst
        // size: a follow-up round starts after completion even when the
        // completion was for a round whose query failed.
        let mut f = UnreadCountFlight::default();
        assert!(f.request());
        assert!(!f.request());
        assert!(!f.request());
        assert!(f.complete());
        assert!(f.request());
        assert!(!f.request());
        assert!(f.complete());
        assert!(f.request());
        assert!(!f.complete());
    }
}

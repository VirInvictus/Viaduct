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
use crate::ui::tree::TreeController;

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
        pub feed_popover: OnceCell<gtk::PopoverMenu>,
        pub folder_popover: OnceCell<gtk::PopoverMenu>,
        pub smart_feed_popover: OnceCell<gtk::PopoverMenu>,
        /// Right-click context. The gesture handler stashes the model
        /// object before showing the popover; action bodies on
        /// `ViaductWindow` take it via the accessors below.
        pub right_clicked_feed: RefCell<Option<crate::models::Feed>>,
        pub right_clicked_folder: RefCell<Option<crate::models::Folder>>,
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
    pub fn refresh_unread_counts(&self, account: Arc<Account>) {
        let Some(controller) = self.imp().controller.get().cloned() else {
            return;
        };
        glib::spawn_future_local(async move {
            let per_feed = match account.unread_counts_by_feed().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(?e, "unread_counts_by_feed failed");
                    return;
                }
            };
            let smart = account.smart_feed_counts().await.ok();

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

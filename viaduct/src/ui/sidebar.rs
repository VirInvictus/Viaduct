// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use crate::database::accounts::Account;
use crate::database::opml::OpmlFile;
use crate::models::{Feed, Folder};
use crate::network::ImageCache;
use crate::ui::tree::{TreeController, TreeControllerDelegate, TreeNode};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Enum representing the items that can appear in the sidebar.
/// This acts as the `representedObject` in the `TreeNode`.
#[derive(Clone)]
pub enum SidebarItem {
    SmartFeedGroup,
    SmartFeed(String), // e.g., "Today", "All Unread", "Starred"
    Folder(Folder),
    Feed(Feed),
}

/// Port of `SidebarTreeControllerDelegate`.
pub struct SidebarTreeControllerDelegate {
    pub opml_file: RefCell<Option<Rc<OpmlFile>>>,
    pub is_read_filtered: std::cell::Cell<bool>,
}

impl Default for SidebarTreeControllerDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarTreeControllerDelegate {
    pub fn new() -> Self {
        Self {
            opml_file: RefCell::new(None),
            is_read_filtered: std::cell::Cell::new(false),
        }
    }

    pub fn set_opml_file(&self, opml_file: OpmlFile) {
        self.opml_file.replace(Some(Rc::new(opml_file)));
    }

    fn child_nodes_for_root(&self, root_node: &TreeNode) -> Vec<TreeNode> {
        let mut top_level_nodes = Vec::new();

        // Smart Feeds group
        let smart_feeds_obj = Rc::new(SidebarItem::SmartFeedGroup);
        // We recreate or find existing - for simplicity, recreate.
        let smart_feeds_node = TreeNode::new(smart_feeds_obj, Some(root_node));
        smart_feeds_node.set_can_have_child_nodes(true);
        smart_feeds_node.set_is_group_item(true);
        top_level_nodes.push(smart_feeds_node);

        // Feeds and Folders from OPML
        if let Some(opml) = self.opml_file.borrow().as_ref() {
            for folder in &opml.folders {
                let folder_obj = Rc::new(SidebarItem::Folder(folder.clone()));
                let folder_node = TreeNode::new(folder_obj, Some(root_node));
                folder_node.set_can_have_child_nodes(true);
                top_level_nodes.push(folder_node);
            }
            for feed in &opml.standalone_feeds {
                let feed_obj = Rc::new(SidebarItem::Feed(feed.clone()));
                let feed_node = TreeNode::new(feed_obj, Some(root_node));
                top_level_nodes.push(feed_node);
            }
        }

        top_level_nodes
    }

    fn child_nodes_for_smart_feeds(&self, parent_node: &TreeNode) -> Vec<TreeNode> {
        let smart_feeds = vec![
            "Today".to_string(),
            "All Unread".to_string(),
            "Starred".to_string(),
        ];

        smart_feeds
            .into_iter()
            .map(|name| {
                let obj = Rc::new(SidebarItem::SmartFeed(name));
                TreeNode::new(obj, Some(parent_node))
            })
            .collect()
    }

    fn child_nodes_for_folder(&self, parent_node: &TreeNode, folder: &Folder) -> Vec<TreeNode> {
        folder
            .feeds
            .iter()
            .map(|feed| {
                let obj = Rc::new(SidebarItem::Feed(feed.clone()));
                TreeNode::new(obj, Some(parent_node))
            })
            .collect()
    }
}

impl TreeControllerDelegate for SidebarTreeControllerDelegate {
    fn child_nodes_for(&self, _tree_controller: &TreeController, node: &TreeNode) -> Vec<TreeNode> {
        if node.is_root() {
            return self.child_nodes_for_root(node);
        }

        if let Some(obj) = node.represented_object()
            && let Some(sidebar_item) = obj.downcast_ref::<SidebarItem>()
        {
            match sidebar_item {
                SidebarItem::SmartFeedGroup => {
                    return self.child_nodes_for_smart_feeds(node);
                }
                SidebarItem::Folder(folder) => {
                    return self.child_nodes_for_folder(node, folder);
                }
                _ => {}
            }
        }

        Vec::new()
    }
}

/// Port of `SidebarOutlineDataSource`.
/// This bridges the `TreeController` domain logic into a GTK `GtkTreeListModel`.
pub struct SidebarDataSource {
    tree_controller: RefCell<Option<Rc<TreeController>>>,
    root_store: gio::ListStore,
}

impl Default for SidebarDataSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SidebarDataSource {
    pub fn new() -> Self {
        let root_store = gio::ListStore::new::<TreeNode>();
        Self {
            tree_controller: RefCell::new(None),
            root_store,
        }
    }

    /// Connects the tree controller to the data source and initializes the root store.
    pub fn set_tree_controller(&self, controller: Rc<TreeController>) {
        self.tree_controller.replace(Some(controller.clone()));
        self.sync_root_nodes(&controller.root_node);
    }

    /// Returns the root `gio::ListModel` to be wrapped by a `GtkTreeListModel`.
    pub fn root_model(&self) -> gio::ListStore {
        self.root_store.clone()
    }

    /// The callback used by `GtkTreeListModel` to fetch children for a node.
    /// It returns a `gio::ListModel` containing the child `TreeNode`s, or `None` if it's a leaf.
    pub fn create_child_model(item: &glib::Object) -> Option<gio::ListModel> {
        if let Some(node) = item.downcast_ref::<TreeNode>() {
            if !node.can_have_child_nodes() {
                return None;
            }

            let store = gio::ListStore::new::<TreeNode>();
            let children = node.child_nodes();
            for child in children {
                store.append(&child);
            }

            Some(store.upcast())
        } else {
            None
        }
    }

    /// Syncs the root `TreeNode`'s children into the GTK root `ListStore`.
    fn sync_root_nodes(&self, root_node: &TreeNode) {
        self.root_store.remove_all();
        for child in root_node.child_nodes() {
            self.root_store.append(&child);
        }
    }

    /// Re-syncs the root from the controller's current state. Call after the
    /// delegate's underlying data (OPML) has changed and the controller has
    /// rebuilt.
    pub fn refresh_root(&self) {
        if let Some(controller) = self.tree_controller.borrow().clone() {
            self.sync_root_nodes(&controller.root_node);
        }
    }
}

/// Sets up the Sidebar ListView with models and the row factory.
/// Returns the `SingleSelection` so the caller can connect selection-changed.
pub fn setup_sidebar_list_view(
    list_view: &gtk::ListView,
    data_source: &SidebarDataSource,
    account: Arc<Account>,
    image_cache: Arc<ImageCache>,
) -> gtk::SingleSelection {
    let tree_model = gtk::TreeListModel::new(
        data_source.root_model(),
        false,
        true, // auto-expand
        SidebarDataSource::create_child_model,
    );

    let selection_model = gtk::SingleSelection::new(Some(tree_model));
    list_view.set_model(Some(&selection_model));

    let factory = gtk::SignalListItemFactory::new();

    let account_for_setup = account.clone();
    factory.connect_setup(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let expander = gtk::TreeExpander::new();

        let box_widget = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        box_widget.set_margin_start(6);
        box_widget.set_margin_end(6);
        box_widget.set_margin_top(2);
        box_widget.set_margin_bottom(2);

        // v2.6.0 drag-and-drop. Drag-source: feed rows expose their URL
        // as a string; drop-target: folder rows accept a feed URL and
        // call `Account::move_feed_to_folder`. Both controllers read
        // the bound `SidebarItem` via the `viaduct-sidebar-item`
        // set_data attached during `connect_bind`. Non-Feed rows
        // suppress the drag (return None from prepare); non-Folder
        // rows reject the drop.
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gtk::gdk::DragAction::MOVE);
        let box_for_drag = box_widget.downgrade();
        drag_source.connect_prepare(move |_, _x, _y| {
            let bound = box_for_drag.upgrade()?;
            let item = unsafe { bound.data::<SidebarItem>("viaduct-sidebar-item")? };
            // SAFETY: `viaduct-sidebar-item` is set in `connect_bind`
            // and lives until the next bind / unbind on this widget.
            // Reading through the pointer here is fine for the
            // synchronous duration of `prepare`.
            let item = unsafe { item.as_ref() };
            match item {
                SidebarItem::Feed(feed) => {
                    Some(gtk::gdk::ContentProvider::for_value(&feed.url.to_value()))
                }
                _ => None,
            }
        });
        box_widget.add_controller(drag_source);

        let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
        let box_for_drop = box_widget.downgrade();
        let account_for_drop = account_for_setup.clone();
        drop_target.connect_drop(move |target, value, _x, _y| {
            let Some(bound) = box_for_drop.upgrade() else {
                return false;
            };
            let item = unsafe { bound.data::<SidebarItem>("viaduct-sidebar-item") };
            let folder_name = match item.map(|p| unsafe { p.as_ref() }) {
                Some(SidebarItem::Folder(folder)) => folder.name.clone(),
                _ => return false,
            };
            let Ok(url) = value.get::<String>() else {
                return false;
            };
            let account = account_for_drop.clone();
            let target_widget = target.widget();
            // Run the DB op on tokio (Send-only future); deliver the
            // result back to the GTK thread via a oneshot. The widget
            // ref is `!Send` so it stays here in the local future.
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::spawn_on_runtime(async move {
                let _ = tx.send(account.move_feed_to_folder(&url, Some(folder_name)).await);
            });
            glib::spawn_future_local(async move {
                match rx.await {
                    Ok(Ok(_)) => {
                        if let Some(w) = target_widget {
                            let _ = w.activate_action("win.reload-sidebar", None);
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(?e, "drag-and-drop: move_feed_to_folder failed");
                    }
                    Err(_) => {}
                }
            });
            true
        });
        box_widget.add_controller(drop_target);

        // Icon slot is a Stack with two pages — a symbolic GtkImage for groups
        // and folders, an AdwAvatar for feeds and smart feeds. Same row factory
        // reused across the whole sidebar, switched at bind time.
        let icon_stack = gtk::Stack::new();
        icon_stack.set_transition_type(gtk::StackTransitionType::None);

        let icon_image = gtk::Image::new();
        icon_image.set_pixel_size(16);
        icon_image.set_valign(gtk::Align::Center);
        icon_stack.add_named(&icon_image, Some("icon"));

        let avatar = adw::Avatar::new(24, None, true);
        avatar.set_valign(gtk::Align::Center);
        icon_stack.add_named(&avatar, Some("avatar"));

        let label = gtk::Label::new(None);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_valign(gtk::Align::Center);

        let badge = gtk::Label::new(None);
        badge.add_css_class("numeric");
        // Custom class flagged via apply_sidebar_styling — pill-shaped
        // background, dimmed when count is low. Doesn't affect non-badge
        // GtkLabels because of the class scope.
        badge.add_css_class("viaduct-unread-badge");
        badge.set_valign(gtk::Align::Center);

        box_widget.append(&icon_stack);
        box_widget.append(&label);
        box_widget.append(&badge);

        expander.set_child(Some(&box_widget));
        item.set_child(Some(&expander));
    });

    let account_for_bind = account;
    let image_cache_for_bind = image_cache;
    factory.connect_bind(move |_factory, list_item| {
        let account = account_for_bind.clone();
        let image_cache = image_cache_for_bind.clone();

        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
        let row = item.item().and_downcast::<gtk::TreeListRow>().unwrap();
        expander.set_list_row(Some(&row));

        let node = row.item().and_downcast::<TreeNode>().unwrap();
        let box_widget = expander.child().and_downcast::<gtk::Box>().unwrap();

        let icon_stack = box_widget
            .first_child()
            .and_downcast::<gtk::Stack>()
            .unwrap();
        let icon_image = icon_stack
            .child_by_name("icon")
            .and_downcast::<gtk::Image>()
            .unwrap();
        let avatar = icon_stack
            .child_by_name("avatar")
            .and_downcast::<adw::Avatar>()
            .unwrap();
        let label = icon_stack
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();
        let badge = label.next_sibling().and_downcast::<gtk::Label>().unwrap();

        // Reset avatar state so reused rows don't bleed favicons across feeds.
        avatar.set_custom_image(None::<&gtk::gdk::Paintable>);

        // Extract domain data to bind
        let rep_obj = node.represented_object();

        if let Some(obj) = rep_obj {
            // Reset bind-time style classes so a recycled row binding to a
            // different SidebarItem doesn't keep the previous header class.
            label.remove_css_class("viaduct-sidebar-heading");
            label.remove_css_class("heading");
            label.remove_css_class("dim-label");
            if let Some(sidebar_item) = obj.downcast_ref::<SidebarItem>() {
                // Attach the bound SidebarItem to the row's content box
                // so the right-click context menu (v1.7.1) can recover
                // it via a parent-walk from the picked leaf widget.
                // Overwrites cleanly on rebind — no explicit unbind
                // needed, mirrors the timeline.rs pattern.
                unsafe {
                    box_widget
                        .set_data::<SidebarItem>("viaduct-sidebar-item", sidebar_item.clone());
                }
                match sidebar_item {
                    SidebarItem::SmartFeedGroup => {
                        label.set_text("Smart Feeds");
                        // Section-header styling — bold, slightly smaller,
                        // dimmed. The icon stays as a folder for now since
                        // GtkListView still wants something in the slot.
                        label.add_css_class("viaduct-sidebar-heading");
                        icon_image.set_icon_name(Some("emblem-symbolic-link-symbolic"));
                        icon_stack.set_visible_child_name("icon");
                    }
                    SidebarItem::SmartFeed(name) => {
                        label.set_text(name);
                        icon_image.set_icon_name(Some(smart_feed_icon(name)));
                        icon_stack.set_visible_child_name("icon");
                    }
                    SidebarItem::Folder(folder) => {
                        label.set_text(&folder.name);
                        icon_image.set_icon_name(Some("folder-symbolic"));
                        icon_stack.set_visible_child_name("icon");
                    }
                    SidebarItem::Feed(feed) => {
                        let name = feed
                            .edited_name
                            .as_deref()
                            .or(feed.name.as_deref())
                            .unwrap_or("Unnamed Feed");
                        label.set_text(name);
                        // AdwAvatar auto-derives a stable accent color from the
                        // displayed text — semantically equivalent to NNW's
                        // ColorHash(feed.url) for our purposes.
                        avatar.set_text(Some(name));
                        avatar.set_show_initials(true);
                        icon_stack.set_visible_child_name("avatar");
                        spawn_favicon_fetch(
                            account.clone(),
                            image_cache.clone(),
                            feed.id.clone(),
                            name.to_string(),
                            avatar.clone(),
                        );
                    }
                }
            } else {
                label.set_text("Unknown");
                icon_image.set_icon_name(Some("dialog-question-symbolic"));
                icon_stack.set_visible_child_name("icon");
            }
        } else {
            label.set_text("Root");
            icon_image.set_icon_name(Some("folder-symbolic"));
            icon_stack.set_visible_child_name("icon");
        }

        apply_unread_badge(&badge, node.unread_count());

        // Re-render the badge whenever the node's unread count flips. Same
        // pattern as ArticleNode/notify::read in the timeline factory: id
        // stashed on the list_item, disconnected by `connect_unbind` when
        // the row recycles to a different node.
        let badge_for_notify = badge.downgrade();
        let id = node.connect_notify_local(Some("unread-count"), move |node, _| {
            if let Some(badge) = badge_for_notify.upgrade() {
                apply_unread_badge(&badge, node.unread_count());
            }
        });
        unsafe {
            item.set_data("viaduct-unread-handler", id);
        }
    });

    factory.connect_unbind(|_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(node) = row.item().and_downcast::<TreeNode>() else {
            return;
        };
        unsafe {
            if let Some(id) = item.steal_data::<glib::SignalHandlerId>("viaduct-unread-handler") {
                node.disconnect(id);
            }
        }
    });

    list_view.set_factory(Some(&factory));
    selection_model
}

fn apply_unread_badge(badge: &gtk::Label, count: u32) {
    if count > 0 {
        badge.set_text(&count.to_string());
        badge.set_visible(true);
    } else {
        badge.set_text("");
        badge.set_visible(false);
    }
}

/// Async-fetch the favicon for a feed and apply it to the row's avatar.
///
/// The flow is settings DB → favicon URL → ImageCache (in-memory → disk → net) →
/// `gdk::Texture::from_bytes` → `adw::Avatar::set_custom_image`. Stale-row guard:
/// the row factory recycles widgets as the user scrolls, so by the time the bytes
/// arrive the row may have been re-bound to a different feed. We compare the
/// avatar's currently-displayed text to the name we kicked off with and bail if
/// it changed.
fn spawn_favicon_fetch(
    account: Arc<Account>,
    image_cache: Arc<ImageCache>,
    feed_id: String,
    expected_name: String,
    avatar: adw::Avatar,
) {
    glib::spawn_future_local(async move {
        let settings = match account.fetch_feed_settings(feed_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(?e, "favicon: settings fetch failed");
                return;
            }
        };
        let Some(favicon_url) = settings.favicon_url.or(settings.icon_url) else {
            return;
        };
        let Some(bytes) = image_cache.favicon(&favicon_url).await else {
            return;
        };
        // Stale-row guard.
        if avatar.text().as_deref() != Some(expected_name.as_str()) {
            return;
        }
        let glib_bytes = glib::Bytes::from(&bytes);
        match gtk::gdk::Texture::from_bytes(&glib_bytes) {
            Ok(texture) => avatar.set_custom_image(Some(&texture)),
            Err(e) => tracing::debug!(?e, "favicon: texture decode failed"),
        }
    });
}

fn smart_feed_icon(name: &str) -> &'static str {
    match name {
        "Today" => "x-office-calendar-symbolic",
        "All Unread" => "mail-unread-symbolic",
        "Starred" => "starred-symbolic",
        _ => "view-pin-symbolic",
    }
}

/// The currently-selected sidebar row, decoded from a `gtk::SingleSelection`.
pub fn selected_sidebar_item(selection: &gtk::SingleSelection) -> Option<SidebarItem> {
    let item = selection.selected_item()?;
    let row = item.downcast_ref::<gtk::TreeListRow>()?;
    let node = row.item().and_downcast::<TreeNode>()?;
    let obj = node.represented_object()?;
    let item = obj.downcast_ref::<SidebarItem>()?;
    Some(item.clone())
}

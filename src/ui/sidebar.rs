use crate::database::opml::OpmlFile;
use crate::models::{Feed, Folder};
use crate::ui::tree::{TreeController, TreeControllerDelegate, TreeNode};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

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
}

/// Sets up the Sidebar ListView with models and the row factory.
pub fn setup_sidebar_list_view(list_view: &gtk::ListView, data_source: &SidebarDataSource) {
    let tree_model = gtk::TreeListModel::new(
        data_source.root_model(),
        false,
        true, // auto-expand
        SidebarDataSource::create_child_model,
    );

    let selection_model = gtk::SingleSelection::new(Some(tree_model));
    list_view.set_model(Some(&selection_model));

    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let expander = gtk::TreeExpander::new();

        let box_widget = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        box_widget.set_margin_start(4);
        box_widget.set_margin_end(4);

        let icon = gtk::Image::new();
        icon.set_pixel_size(16);

        let label = gtk::Label::new(None);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);

        let badge = gtk::Label::new(None);
        badge.add_css_class("numeric"); // standard libadwaita styling

        box_widget.append(&icon);
        box_widget.append(&label);
        box_widget.append(&badge);

        expander.set_child(Some(&box_widget));
        item.set_child(Some(&expander));
    });

    factory.connect_bind(move |_factory, list_item| {
        let item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("Needs to be ListItem");

        let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
        let row = item.item().and_downcast::<gtk::TreeListRow>().unwrap();
        expander.set_list_row(Some(&row));

        let node = row.item().and_downcast::<TreeNode>().unwrap();
        let box_widget = expander.child().and_downcast::<gtk::Box>().unwrap();

        let icon = box_widget
            .first_child()
            .and_downcast::<gtk::Image>()
            .unwrap();
        let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let badge = label.next_sibling().and_downcast::<gtk::Label>().unwrap();

        // Extract domain data to bind
        let rep_obj = node.represented_object();

        if let Some(obj) = rep_obj {
            if let Some(sidebar_item) = obj.downcast_ref::<SidebarItem>() {
                match sidebar_item {
                    SidebarItem::SmartFeedGroup => {
                        label.set_text("Smart Feeds");
                        icon.set_icon_name(Some("folder-symbolic"));
                    }
                    SidebarItem::SmartFeed(name) => {
                        label.set_text(name);
                        icon.set_icon_name(Some("emblem-favorite-symbolic")); // Placeholder
                    }
                    SidebarItem::Folder(folder) => {
                        label.set_text(&folder.name);
                        icon.set_icon_name(Some("folder-symbolic"));
                    }
                    SidebarItem::Feed(feed) => {
                        let name = feed
                            .edited_name
                            .as_deref()
                            .or(feed.name.as_deref())
                            .unwrap_or("Unnamed Feed");
                        label.set_text(name);
                        icon.set_icon_name(Some("text-html-symbolic")); // Placeholder for favicon
                    }
                }
            } else {
                label.set_text("Unknown");
                icon.set_icon_name(Some("dialog-question-symbolic"));
            }
        } else {
            label.set_text("Root");
            icon.set_icon_name(Some("folder-symbolic"));
        }

        let count = node.unread_count();
        if count > 0 {
            badge.set_text(&count.to_string());
            badge.set_visible(true);
        } else {
            badge.set_text("");
            badge.set_visible(false);
        }
    });

    list_view.set_factory(Some(&factory));
}

// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// A trait porting `TreeControllerDelegate` from `TreeController.swift`.
pub trait TreeControllerDelegate {
    fn child_nodes_for(&self, tree_controller: &TreeController, node: &TreeNode) -> Vec<TreeNode>;
}

glib::wrapper! {
    /// Port of `Node.swift`. Must be a `glib::Object` so it can be used in `gio::ListModel`.
    pub struct TreeNode(ObjectSubclass<imp::TreeNode>);
}

pub mod imp {
    use super::*;
    use gtk::gio;
    use std::cell::Cell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::TreeNode)]
    pub struct TreeNode {
        pub represented_object: RefCell<Option<Rc<dyn std::any::Any>>>,
        pub can_have_child_nodes: Cell<bool>,
        pub is_group_item: Cell<bool>,
        /// Unread count exposed as a glib property so the sidebar row factory
        /// can subscribe to `notify::unread-count` and update badges in
        /// place when counts change (status flips, refresh completion).
        ///
        /// **v2.0.0-pre5**: parents auto-aggregate from their children.
        /// `set_child_nodes` connects `notify::unread-count` on each new
        /// child to recompute self = sum(children); the cascade then
        /// propagates upward via the parent's own `notify::unread-count`.
        /// Callers should only set this directly on **leaf** nodes
        /// (`SidebarItem::Feed` / `SidebarItem::SmartFeed`); folder and
        /// smart-feed-group totals derive automatically.
        #[property(get, set)]
        pub unread_count: Cell<u32>,
        pub child_nodes: RefCell<Vec<super::TreeNode>>,
        /// `(child_weak, handler_id)` pairs for the per-child
        /// `notify::unread-count` subscriptions installed in
        /// `set_child_nodes`. Re-set on every `set_child_nodes` call so a
        /// rebuild doesn't leak handlers from the old children.
        pub child_unread_handlers:
            RefCell<Vec<(glib::WeakRef<super::TreeNode>, glib::SignalHandlerId)>>,
        pub parent: RefCell<Option<glib::WeakRef<super::TreeNode>>>,
        // The list store exposed to GTK for this node's children
        pub list_store: RefCell<Option<gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TreeNode {
        const NAME: &'static str = "ViaductTreeNode";
        type Type = super::TreeNode;
    }

    #[glib::derived_properties]
    impl ObjectImpl for TreeNode {}
}

impl TreeNode {
    pub fn new(represented_object: Rc<dyn std::any::Any>, parent: Option<&TreeNode>) -> Self {
        let node: Self = glib::Object::builder().build();
        node.imp()
            .represented_object
            .replace(Some(represented_object));

        if let Some(p) = parent {
            node.imp().parent.replace(Some(p.downgrade()));
        }
        node
    }

    pub fn generic_root_node() -> Self {
        let node: Self = glib::Object::builder().build();
        // TopLevelRepresentedObject analog
        node.imp().represented_object.replace(Some(Rc::new(())));
        node.imp().can_have_child_nodes.set(true);
        node
    }

    pub fn parent(&self) -> Option<TreeNode> {
        self.imp()
            .parent
            .borrow()
            .as_ref()
            .and_then(|w| w.upgrade())
    }

    pub fn represented_object(&self) -> Option<Rc<dyn std::any::Any>> {
        self.imp().represented_object.borrow().clone()
    }

    pub fn can_have_child_nodes(&self) -> bool {
        self.imp().can_have_child_nodes.get()
    }

    pub fn set_can_have_child_nodes(&self, val: bool) {
        self.imp().can_have_child_nodes.set(val)
    }

    pub fn is_group_item(&self) -> bool {
        self.imp().is_group_item.get()
    }

    pub fn set_is_group_item(&self, val: bool) {
        self.imp().is_group_item.set(val)
    }

    pub fn child_nodes(&self) -> Vec<TreeNode> {
        self.imp().child_nodes.borrow().clone()
    }

    pub fn set_child_nodes(&self, new_nodes: Vec<TreeNode>) {
        // Disconnect any existing notify::unread-count subscriptions
        // from the previous child set. Idempotent: weak refs that fail
        // to upgrade (children already finalized) are silently skipped.
        {
            let mut handlers = self.imp().child_unread_handlers.borrow_mut();
            for (weak, id) in handlers.drain(..) {
                if let Some(child) = weak.upgrade() {
                    child.disconnect(id);
                }
            }
            // Wire fresh subscriptions for the new child set.
            let weak_self = self.downgrade();
            for child in &new_nodes {
                let cb_self = weak_self.clone();
                let id = child.connect_notify_local(Some("unread-count"), move |_, _| {
                    if let Some(this) = cb_self.upgrade() {
                        this.recompute_aggregate_unread();
                    }
                });
                handlers.push((child.downgrade(), id));
            }
        }
        *self.imp().child_nodes.borrow_mut() = new_nodes;
        // Compute the initial total so the parent's badge reflects the
        // current children even before any of them changes.
        self.recompute_aggregate_unread();
    }

    /// Recompute `unread_count` as the sum of children's `unread_count`.
    /// Called from `set_child_nodes` (initial sum) and from each child's
    /// `notify::unread-count` handler (incremental update). The cascade
    /// to grandparents happens naturally through `set_unread_count`'s own
    /// `notify::unread-count` emission.
    fn recompute_aggregate_unread(&self) {
        let total: u32 = self
            .imp()
            .child_nodes
            .borrow()
            .iter()
            .map(|c| c.unread_count())
            .sum();
        if self.unread_count() != total {
            self.set_unread_count(total);
        }
    }

    pub fn number_of_child_nodes(&self) -> usize {
        self.imp().child_nodes.borrow().len()
    }

    pub fn is_root(&self) -> bool {
        self.parent().is_none()
    }

    pub fn is_leaf(&self) -> bool {
        self.number_of_child_nodes() == 0
    }

    // Methods mimicking Swift's `childAtIndex`, `indexOfChild`, etc. could be added here.
}

/// Port of `TreeController.swift`.
pub struct TreeController {
    pub root_node: TreeNode,
    delegate: Weak<RefCell<dyn TreeControllerDelegate>>,
}

impl TreeController {
    pub fn new(delegate: Weak<RefCell<dyn TreeControllerDelegate>>, root_node: TreeNode) -> Self {
        let controller = Self {
            root_node,
            delegate,
        };
        controller.rebuild();
        controller
    }

    pub fn new_with_generic_root(delegate: Weak<RefCell<dyn TreeControllerDelegate>>) -> Self {
        Self::new(delegate, TreeNode::generic_root_node())
    }

    pub fn rebuild(&self) -> bool {
        self.rebuild_child_nodes(&self.root_node)
    }

    fn rebuild_child_nodes(&self, node: &TreeNode) -> bool {
        if !node.can_have_child_nodes() {
            return false;
        }

        let mut child_nodes_did_change = false;

        let new_child_nodes = if let Some(delegate_rc) = self.delegate.upgrade() {
            delegate_rc.borrow().child_nodes_for(self, node)
        } else {
            Vec::new()
        };

        // Note: simplified equality check based on pointers/identity could be done here.
        // For now, assume if rebuilt, it might change.
        // A complete equality would check identity of glib::Object.
        let old_nodes = node.child_nodes();

        let are_equal = old_nodes.len() == new_child_nodes.len()
            && old_nodes
                .iter()
                .zip(new_child_nodes.iter())
                .all(|(a, b)| a == b);

        if !are_equal {
            node.set_child_nodes(new_child_nodes.clone());
            child_nodes_did_change = true;
        }

        for child in new_child_nodes {
            if self.rebuild_child_nodes(&child) {
                child_nodes_did_change = true;
            }
        }

        child_nodes_did_change
    }
}

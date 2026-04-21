use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

/// A struct for batch updating, porting `BatchUpdate.swift`.
/// Main thread only.
#[derive(Clone)]
pub struct BatchUpdate {
    count: Rc<RefCell<usize>>,
}

impl Default for BatchUpdate {
    fn default() -> Self {
        Self {
            count: Rc::new(RefCell::new(0)),
        }
    }
}

impl BatchUpdate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_performing(&self) -> bool {
        assert!(
            glib::MainContext::default().is_owner(),
            "BatchUpdate must be used from the main thread"
        );
        *self.count.borrow() > 0
    }

    pub fn perform<F>(&self, mut batch_update_block: F)
    where
        F: FnMut(),
    {
        assert!(
            glib::MainContext::default().is_owner(),
            "BatchUpdate must be used from the main thread"
        );
        self.increment_count();
        batch_update_block();
        self.decrement_count();
    }

    pub fn start(&self) {
        assert!(
            glib::MainContext::default().is_owner(),
            "BatchUpdate must be used from the main thread"
        );
        self.increment_count();
    }

    pub fn end(&self) {
        assert!(
            glib::MainContext::default().is_owner(),
            "BatchUpdate must be used from the main thread"
        );
        self.decrement_count();
    }

    fn increment_count(&self) {
        *self.count.borrow_mut() += 1;
    }

    fn decrement_count(&self) {
        let mut count = self.count.borrow_mut();
        *count -= 1;
        if *count < 1 {
            *count = 0;
            self.post_batch_update_did_perform();
        }
    }

    fn post_batch_update_did_perform(&self) {
        // In Rust/GTK, we emit signals or custom events. For now we will rely on
        // passing this state to necessary UI elements or using GObject properties/signals
        // when fully integrated with the models.
        // TODO: NotificationCenter analog
    }
}

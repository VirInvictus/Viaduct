use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::task::JoinHandle;

/// A queue that ensures only the latest fetch request is active,
/// porting `FetchRequestQueue.swift`.
#[derive(Default, Clone)]
pub struct FetchRequestQueue {
    current_task: Rc<RefCell<Option<JoinHandle<()>>>>,
}

impl FetchRequestQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit<F>(&self, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        assert!(
            glib::MainContext::default().is_owner(),
            "FetchRequestQueue must be used from the main thread"
        );

        let mut lock = self.current_task.borrow_mut();

        if let Some(handle) = lock.take() {
            handle.abort();
        }

        let new_handle = tokio::task::spawn(task);
        *lock = Some(new_handle);
    }
}

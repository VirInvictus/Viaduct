// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// CoalescingQueue, porting `CoalescingQueue.swift`.
/// Main thread only.
pub struct CoalescingQueue<T, F>
where
    T: Eq + Clone + 'static,
    F: FnMut(Vec<T>) + 'static,
{
    interval: Duration,
    max_interval: Duration,
    last_call_time: Rc<RefCell<Instant>>,
    timer: Rc<RefCell<Option<glib::SourceId>>>,
    calls: Rc<RefCell<Vec<T>>>,
    perform_fn: Rc<RefCell<F>>,
}

impl<T, F> CoalescingQueue<T, F>
where
    T: Eq + Clone + 'static,
    F: FnMut(Vec<T>) + 'static,
{
    pub fn new(interval: Duration, max_interval: Duration, perform_fn: F) -> Self {
        Self {
            interval,
            max_interval,
            last_call_time: Rc::new(RefCell::new(Instant::now())),
            timer: Rc::new(RefCell::new(None)),
            calls: Rc::new(RefCell::new(Vec::new())),
            perform_fn: Rc::new(RefCell::new(perform_fn)),
        }
    }

    pub fn add(&self, call: T) {
        assert!(
            glib::MainContext::default().is_owner(),
            "CoalescingQueue must be used from the main thread"
        );

        self.restart_timer();

        if !self.calls.borrow().contains(&call) {
            self.calls.borrow_mut().push(call);
        }

        if self.last_call_time.borrow().elapsed() > self.max_interval {
            self.perform_calls_immediately();
        }
    }

    pub fn perform_calls_immediately(&self) {
        assert!(
            glib::MainContext::default().is_owner(),
            "CoalescingQueue must be used from the main thread"
        );

        let calls_to_make: Vec<T> = {
            let mut calls_ref = self.calls.borrow_mut();
            let copy = calls_ref.clone();
            calls_ref.clear();
            copy
        };

        self.invalidate_timer();
        *self.last_call_time.borrow_mut() = Instant::now();

        if !calls_to_make.is_empty() {
            (self.perform_fn.borrow_mut())(calls_to_make);
        }
    }

    fn restart_timer(&self) {
        self.invalidate_timer();

        let calls_ref = self.calls.clone();
        let last_call_time_ref = self.last_call_time.clone();
        let perform_fn_ref = self.perform_fn.clone();

        let timer_id = glib::timeout_add_local_once(self.interval, move || {
            let calls_to_make: Vec<T> = {
                let mut calls_mut = calls_ref.borrow_mut();
                let copy = calls_mut.clone();
                calls_mut.clear();
                copy
            };

            *last_call_time_ref.borrow_mut() = Instant::now();

            if !calls_to_make.is_empty() {
                (perform_fn_ref.borrow_mut())(calls_to_make);
            }
        });

        *self.timer.borrow_mut() = Some(timer_id);
    }

    fn invalidate_timer(&self) {
        if let Some(timer_id) = self.timer.borrow_mut().take() {
            timer_id.remove();
        }
    }
}

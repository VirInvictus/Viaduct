// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

use gtk::glib;
pub fn test_channel() {
    let (tx, rx) = glib::MainContext::channel::<i32>(glib::Priority::DEFAULT);
    let _: () = tx; // force type error
}

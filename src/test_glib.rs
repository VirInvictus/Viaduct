use gtk::glib;
pub fn test_channel() {
    let (tx, rx) = glib::MainContext::channel::<i32>(glib::Priority::DEFAULT);
    let _: () = tx; // force type error
}

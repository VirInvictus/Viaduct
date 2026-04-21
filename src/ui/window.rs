use adw::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(file = "window.ui")]
    pub struct ViaductWindow {
        #[template_child]
        pub outer_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub inner_split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub sidebar_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub timeline_list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub article_text_view: TemplateChild<gtk::TextView>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViaductWindow {
        const NAME: &'static str = "ViaductWindow";
        type Type = super::ViaductWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ViaductWindow {
        fn constructed(&self) {
            self.parent_constructed();

            let sidebar_ds = crate::ui::sidebar::SidebarDataSource::new();
            crate::ui::sidebar::setup_sidebar_list_view(&self.sidebar_list_view, &sidebar_ds);

            let timeline_store = gio::ListStore::new::<crate::ui::timeline::ArticleNode>();
            crate::ui::timeline::setup_timeline_list_view(
                &self.timeline_list_view,
                &timeline_store,
            );

            // TODO: store models somewhere if they need to be updated dynamically later.
        }
    }
    impl WidgetImpl for ViaductWindow {}
    impl WindowImpl for ViaductWindow {}
    impl ApplicationWindowImpl for ViaductWindow {}
    impl adw::subclass::prelude::AdwApplicationWindowImpl for ViaductWindow {}
}

glib::wrapper! {
    pub struct ViaductWindow(ObjectSubclass<imp::ViaductWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl ViaductWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }
}

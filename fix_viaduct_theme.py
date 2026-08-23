import re
with open("viaduct/src/theme.rs", "r") as f:
    text = f.read()

# We will completely rewrite theme.rs, keeping only STRUCTURE, apply_stylesheet, etc.
# Wait, it's easier to just parse the STRUCTURE string out.
match = re.search(r'const STRUCTURE: &str = "(.*?)";\n\nfn stylesheet_css', text, re.DOTALL)
if match:
    structure = match.group(1)
    
new_content = f"""
use gtk::prelude::*;
use std::cell::RefCell;

pub fn system_is_dark() -> bool {{ vir_gtk::portal::system_is_dark() }}
pub fn is_dark() -> bool {{ vir_gtk::portal::is_dark() }}
pub fn connect_dark_changed<F>(owner: &impl IsA<gtk::glib::Object>, f: F) where F: Fn(bool) + 'static {{
    vir_gtk::portal::connect_dark_changed(owner, f)
}}

pub fn init(settings: Option<gtk::gio::Settings>) {{
    vir_gtk::portal::init(settings, Some("color-scheme"), false);
}}

const STRUCTURE: &str = "{structure}";

thread_local! {{
    static SHEET_PROVIDER: RefCell<Option<gtk::CssProvider>> = const {{ RefCell::new(None) }};
}}

pub fn install_stylesheet() {{
    apply_stylesheet();
    connect_dark_changed(&global_owner(), |_| apply_stylesheet());
}}

fn apply_stylesheet() {{
    let Some(display) = gtk::gdk::Display::default() else {{ return; }};
    let palette = if is_dark() {{ vir_gtk::theme::Palette::dragon() }} else {{ vir_gtk::theme::Palette::lotus() }};
    
    let css = format!("{{}}{{}}", palette.to_css_custom_properties(), STRUCTURE);
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);

    SHEET_PROVIDER.with(|slot| {{
        if let Some(old) = slot.borrow_mut().take() {{
            gtk::style_context_remove_provider_for_display(&display, &old);
        }}
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        *slot.borrow_mut() = Some(provider);
    }});
}}

fn global_owner() -> gtk::Settings {{
    gtk::Settings::default().expect("GtkSettings requires a display")
}}
"""
with open("viaduct/src/theme.rs", "w") as f:
    f.write(new_content)

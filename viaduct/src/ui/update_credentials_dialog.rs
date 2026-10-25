// Copyright (c) 2002-2026 Brent Simmons, Ranchero Software
// Copyright (c) 2026 Brandon LaRocque
// Licensed under the MIT License. See LICENSE in the project root for details.

//! Update Inoreader Credentials dialog (NNW `263ec2683`'s update path for
//! username/password accounts). The app's only credentials surface until
//! now was external provisioning; this verifies a token against the
//! Reader API before storing it, so a typo can't silently kill sync.

use gtk::glib;
use gtk::prelude::*;

use crate::ui::rows;
use crate::ui::window::ViaductWindow;

/// Build and present the dialog. Fields pre-fill from the Secret Service
/// where credentials already exist; Save verifies against the Reader API
/// and only then stores, so the previously-working token survives a
/// failed attempt untouched.
pub fn present(parent: &ViaductWindow) {
    let group = rows::group(
        Some("Inoreader account"),
        Some(
            "The email and app password (token) viaduct syncs with. Saving verifies the token against Inoreader first; a failed verification leaves the stored credentials untouched.",
        ),
    );
    let (email_row, email_entry) =
        rows::entry_row(Some("Email"), None, None, Some("you@example.com"));
    let (token_row, token_entry) =
        rows::entry_row(Some("App password"), None, None, Some("App password token"));
    group.add(&email_row);
    group.add(&token_row);

    let status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .visible(false)
        .build();

    // Pre-fill from the stored credentials (best-effort; a locked or
    // empty keyring just leaves the fields blank). spawn_future_local:
    // the entries cross the keyring await, so the future stays on the
    // main loop.
    let email_for_prefill = email_entry.clone();
    let token_for_prefill = token_entry.clone();
    glib::spawn_future_local(async move {
        if let Some(creds) = crate::network::credentials::fetch_credentials("inoreader")
            .await
            .ok()
            .flatten()
        {
            email_for_prefill.set_text(&creds.username);
            token_for_prefill.set_text(creds.password.as_deref().unwrap_or_default());
        }
    });

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::End)
        .spacing(12)
        .build();
    let cancel = gtk::Button::builder().label("Cancel").build();
    let save = gtk::Button::builder()
        .label("Verify and Save")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&cancel);
    buttons.append(&save);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    content.append(group.widget());
    content.append(&status);
    content.append(&buttons);

    let window = gtk::Window::builder()
        .title("Update Inoreader Credentials")
        .transient_for(parent)
        .modal(true)
        .default_width(480)
        .default_height(320)
        .child(&content)
        .build();
    crate::ui::close_on_escape(&window);

    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| {
        window_for_cancel.close();
    });

    let window_for_save = window.clone();
    let status_for_save = status.clone();
    let save_for_save = save.clone();
    save.connect_clicked(move |_| {
        let email = email_entry.text().trim().to_string();
        let token = token_entry.text().trim().to_string();
        let window = window_for_save.clone();
        let status = status_for_save.clone();
        let save_btn = save_for_save.clone();
        if email.is_empty() || token.is_empty() {
            status.set_text("Both the email and the app password are required.");
            status.set_visible(true);
            return;
        }
        // Verify against the Reader API; store only on success. The
        // button disables for the round trip so a slow token check
        // can't be double-submitted. Runs on the main loop via
        // spawn_future_local (the widgets cross the awaits, so the
        // Send-bounded runtime spawner can't carry them).
        save_btn.set_sensitive(false);
        status.set_text("Verifying against Inoreader…");
        status.set_visible(true);
        glib::spawn_future_local(async move {
            let caller = crate::network::inoreader::ReaderAPICaller::new(
                crate::network::inoreader::ReaderAPIVariant::Inoreader,
            );
            match caller.validate_credentials(&email, &token).await {
                Ok(_) => {
                    let creds = crate::network::credentials::Credentials {
                        username: email,
                        password: Some(token),
                    };
                    match crate::network::credentials::store_credentials("inoreader", &creds).await
                    {
                        Ok(()) => {
                            window.close();
                            if let Some(parent) = window
                                .transient_for()
                                .and_then(|p| p.downcast::<crate::ui::window::ViaductWindow>().ok())
                            {
                                parent.show_toast_public("Inoreader credentials updated.");
                            }
                        }
                        Err(e) => {
                            save_btn.set_sensitive(true);
                            status.set_text(&format!("Verified, but storing failed: {e}"));
                            status.set_visible(true);
                        }
                    }
                }
                Err(e) => {
                    save_btn.set_sensitive(true);
                    status.set_text(&format!("Inoreader rejected these credentials: {e}"));
                    status.set_visible(true);
                }
            }
        });
    });

    window.present();
}

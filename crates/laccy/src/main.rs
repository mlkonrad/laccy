//! laccy is the clipboard history popup. The GNOME Shell extension's shortcut
//! launches it, and it exits as soon as it closes, so it uses no memory while
//! hidden.

mod popup;

use adw::prelude::*;
use gtk::glib;

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(laccy_core::APP_ID).build();
    app.connect_activate(|app| match app.active_window() {
        // Launching again while the popup is open closes it.
        Some(window) => window.close(),
        None => popup::Popup::present(app),
    });
    app.run()
}

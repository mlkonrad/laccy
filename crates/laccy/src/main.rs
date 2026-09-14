//! laccy is the clipboard history popup. The GNOME Shell extension's shortcut
//! launches it, and it exits as soon as it closes, so it uses no memory while
//! hidden.

mod popup;

use adw::prelude::*;
use gtk::glib;

fn main() -> glib::ExitCode {
    // A short list doesn't need the GPU. With Cairo the popup uses about a
    // third of the memory of the Vulkan and OpenGL renderers and starts faster
    // (see README). Setting GSK_RENDERER still overrides it.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: GTK isn't initialized yet and no other threads exist.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
    }

    let app = adw::Application::builder().application_id(laccy_core::APP_ID).build();
    app.connect_activate(|app| match app.active_window() {
        // Launching again while the popup is open closes it.
        Some(window) => window.close(),
        None => popup::Popup::present(app),
    });
    app.run()
}

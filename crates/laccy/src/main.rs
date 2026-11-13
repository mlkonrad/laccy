//! laccy is the clipboard history popup and its preferences window. The GNOME
//! Shell extension's shortcut launches it, and it exits once its windows
//! close, so it uses no memory while hidden. The exception: after a text item
//! is picked with its formatting, it keeps offering it until something else is
//! copied (see `popup::offer_formatted`).

mod popup;
mod preferences;
mod thumbnail;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    // Run by laccyd for copied images; needs no display.
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "--thumbnail") {
        return thumbnail::run(std::env::args().nth(2).as_deref());
    }

    // A short list doesn't need the GPU. With Cairo the popup uses about a
    // third of the memory of the Vulkan and OpenGL renderers and starts faster
    // (see README). Setting GSK_RENDERER still overrides it.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: GTK isn't initialized yet and no other threads exist.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
    }

    let app = adw::Application::builder()
        .application_id(laccy_core::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.add_main_option(
        "preferences",
        glib::Char::from(b'p'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Open the preferences window",
        None,
    );
    // Every launch ends up here in the running instance, including the shortcut's.
    app.connect_command_line(|app, command_line| {
        if command_line.options_dict().contains("preferences") {
            preferences::present(app);
        } else {
            popup::toggle(app);
        }
        glib::ExitCode::SUCCESS
    });
    app.run()
}

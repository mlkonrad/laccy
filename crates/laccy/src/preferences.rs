//! The preferences window. Every control writes straight to GSettings, which
//! the popup, the daemon and the GNOME Shell extension all read.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use laccy_core::config::{
    self, HISTORY_SIZE_KEY, IGNORED_APPS_KEY, PASTE_AUTOMATICALLY_KEY, SHOW_INDICATOR_KEY, TOGGLE_SHORTCUT_KEY,
};

const WINDOW_NAME: &str = "laccy-preferences";

/// Shows the preferences window, or raises it if it's already open.
pub fn present(app: &adw::Application) {
    if let Some(window) = app.windows().into_iter().find(|window| window.widget_name() == WINDOW_NAME) {
        window.present();
        return;
    }

    let content: gtk::Widget = match config::settings() {
        Some(settings) => preferences_page(&settings).upcast(),
        None => adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title("Extension Not Installed")
            .description("Laccy keeps its settings with its GNOME Shell extension. Install it with scripts/install-dev.sh.")
            .build()
            .upcast(),
    };

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Preferences")
        .default_width(560)
        .default_height(640)
        .content(&toolbar)
        .build();
    window.set_widget_name(WINDOW_NAME);

    let shortcuts = gtk::ShortcutController::new();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::NamedAction::new("window.close")),
    ));
    window.add_controller(shortcuts);

    window.present();
}

fn preferences_page(settings: &gio::Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.add(&general_group(settings));
    page.add(&history_group(settings));
    page.add(&ignored_apps_group(settings));
    page
}

fn general_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();

    let shortcut_label = gtk::ShortcutLabel::builder()
        .disabled_text("Disabled")
        .valign(gtk::Align::Center)
        .build();
    let show_shortcut = |settings: &gio::Settings, label: &gtk::ShortcutLabel| {
        let accelerators = settings.strv(TOGGLE_SHORTCUT_KEY);
        label.set_accelerator(accelerators.first().map_or("", |accelerator| accelerator.as_str()));
    };
    show_shortcut(settings, &shortcut_label);
    settings.connect_changed(
        Some(TOGGLE_SHORTCUT_KEY),
        glib::clone!(
            #[weak]
            shortcut_label,
            move |settings, _| show_shortcut(settings, &shortcut_label)
        ),
    );

    let shortcut_row = adw::ActionRow::builder()
        .title("Open Laccy")
        .subtitle("Opens and closes the clipboard history")
        .activatable(true)
        .build();
    shortcut_row.add_suffix(&shortcut_label);
    shortcut_row.connect_activated(glib::clone!(
        #[strong]
        settings,
        move |row| shortcut_dialog(&settings).present(Some(row))
    ));
    group.add(&shortcut_row);

    let paste_row = adw::SwitchRow::builder()
        .title("Paste Automatically")
        .subtitle("Paste into the focused app after picking an item, instead of only copying it. Alt+Enter does the opposite")
        .build();
    settings.bind(PASTE_AUTOMATICALLY_KEY, &paste_row, "active").build();
    group.add(&paste_row);

    let indicator_row = adw::SwitchRow::builder()
        .title("Show Icon in Top Bar")
        .subtitle("Click it to open the clipboard history, right-click it for preferences")
        .build();
    settings.bind(SHOW_INDICATOR_KEY, &indicator_row, "active").build();
    group.add(&indicator_row);

    group
}

fn history_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("History").build();

    let size_row = adw::SpinRow::with_range(1.0, 10_000.0, 10.0);
    size_row.set_title("History Size");
    size_row.set_subtitle("Unpinned items to keep. Pinned items are always kept");
    settings.bind(HISTORY_SIZE_KEY, &size_row, "value").build();
    group.add(&size_row);

    group
}

fn ignored_apps_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Ignored Apps")
        .description(
            "Copies from these apps are never recorded. Copies that password managers mark as secret are always ignored.",
        )
        .build();

    let add_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Ignore an App")
        .valign(gtk::Align::Center)
        .build();
    add_button.add_css_class("flat");
    add_button.connect_clicked(glib::clone!(
        #[strong]
        settings,
        move |button| app_picker(&settings).present(Some(button))
    ));
    group.set_header_suffix(Some(&add_button));

    let placeholder = gtk::Label::builder()
        .label("No ignored apps")
        .margin_top(12)
        .margin_bottom(12)
        .build();
    placeholder.add_css_class("dim-label");
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    list.set_placeholder(Some(&placeholder));
    group.add(&list);

    fill_ignored_apps(&list, settings);
    settings.connect_changed(
        Some(IGNORED_APPS_KEY),
        glib::clone!(
            #[weak]
            list,
            move |settings, _| fill_ignored_apps(&list, settings)
        ),
    );

    group
}

fn fill_ignored_apps(list: &gtk::ListBox, settings: &gio::Settings) {
    list.remove_all();
    let installed = gio::AppInfo::all();
    for app_id in ignored_apps(settings) {
        let info = installed.iter().find(|app| app.id().is_some_and(|id| id == app_id.as_str()));
        let row = adw::ActionRow::builder().use_markup(false).build();
        match &info {
            Some(info) => {
                row.set_title(&info.display_name());
                row.set_subtitle(&app_id);
            }
            None => row.set_title(&app_id),
        }
        row.add_prefix(&app_icon(info.and_then(|info| info.icon())));

        let remove_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Stop Ignoring")
            .valign(gtk::Align::Center)
            .build();
        remove_button.add_css_class("flat");
        remove_button.connect_clicked(glib::clone!(
            #[strong]
            settings,
            move |_| {
                let remaining: Vec<String> =
                    ignored_apps(&settings).into_iter().filter(|ignored| *ignored != app_id).collect();
                set_ignored_apps(&settings, remaining);
            }
        ));
        row.add_suffix(&remove_button);
        list.append(&row);
    }
}

/// A dialog listing installed apps; picking one adds it to the ignored apps.
fn app_picker(settings: &gio::Settings) -> adw::Dialog {
    let ignored = ignored_apps(settings);
    let mut apps: Vec<(String, gio::AppInfo)> = gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show())
        .filter_map(|app| Some((app.id()?.to_string(), app)))
        .filter(|(id, _)| !ignored.contains(id))
        .collect();
    apps.sort_by_cached_key(|(_, app)| app.display_name().to_lowercase());

    let dialog = adw::Dialog::builder()
        .title("Ignore App")
        .content_width(420)
        .content_height(560)
        .build();

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .valign(gtk::Align::Start)
        .build();
    list.add_css_class("boxed-list");
    let names: Vec<String> = apps.iter().map(|(_, app)| app.display_name().to_lowercase()).collect();

    for (app_id, app) in apps {
        let row = adw::ActionRow::builder()
            .title(app.display_name())
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_prefix(&app_icon(app.icon()));
        row.connect_activated(glib::clone!(
            #[strong]
            settings,
            #[weak]
            dialog,
            move |_| {
                let mut ignored = ignored_apps(&settings);
                ignored.push(app_id.clone());
                set_ignored_apps(&settings, ignored);
                dialog.close();
            }
        ));
        list.append(&row);
    }

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search apps")
        .hexpand(true)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    list.set_filter_func(glib::clone!(
        #[weak]
        search,
        #[upgrade_or]
        true,
        move |row| {
            let query = search.text().to_lowercase();
            names.get(row.index() as usize).is_none_or(|name| name.contains(&query))
        }
    ));
    search.connect_search_changed(glib::clone!(
        #[weak]
        list,
        move |_| list.invalidate_filter()
    ));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.add_top_bar(&search);
    toolbar.set_content(Some(&scrolled));
    dialog.set_child(Some(&toolbar));
    search.set_key_capture_widget(Some(&dialog));

    dialog
}

/// Records a new "Open Laccy" shortcut from the next key combination pressed.
fn shortcut_dialog(settings: &gio::Settings) -> adw::Dialog {
    let status = adw::StatusPage::builder()
        .icon_name("preferences-desktop-keyboard-shortcuts-symbolic")
        .title("Press the New Shortcut")
        .description("Esc to cancel, Backspace to disable")
        .build();
    status.add_css_class("compact");
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));

    let dialog = adw::Dialog::builder()
        .title("Open Laccy")
        .content_width(400)
        .child(&toolbar)
        .build();

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[strong]
        settings,
        #[weak]
        dialog,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| {
            use gdk::ModifierType as Mod;

            let mods = state & gtk::accelerator_get_default_mod_mask();
            let key = key.to_lower();
            let accelerators = match key {
                gdk::Key::Escape if mods.is_empty() => {
                    dialog.close();
                    return glib::Propagation::Stop;
                }
                gdk::Key::BackSpace if mods.is_empty() => Vec::new(),
                // Wait for a non-modifier key, and require Ctrl, Alt or Super so
                // the shortcut can't swallow ordinary typing.
                _ if is_modifier(key) || !mods.intersects(Mod::CONTROL_MASK | Mod::ALT_MASK | Mod::SUPER_MASK) => {
                    return glib::Propagation::Stop;
                }
                _ => vec![gtk::accelerator_name(key, mods).to_string()],
            };
            if let Err(err) = settings.set_strv(TOGGLE_SHORTCUT_KEY, accelerators) {
                eprintln!("laccy: cannot save shortcut: {err}");
            }
            dialog.close();
            glib::Propagation::Stop
        }
    ));
    dialog.add_controller(keys);

    dialog
}

fn is_modifier(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::Shift_L
            | gdk::Key::Shift_R
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Alt_L
            | gdk::Key::Alt_R
            | gdk::Key::Meta_L
            | gdk::Key::Meta_R
            | gdk::Key::Super_L
            | gdk::Key::Super_R
            | gdk::Key::Hyper_L
            | gdk::Key::Hyper_R
            | gdk::Key::ISO_Level3_Shift
            | gdk::Key::ISO_Level5_Shift
            | gdk::Key::Caps_Lock
            | gdk::Key::Num_Lock
    )
}

fn app_icon(icon: Option<gio::Icon>) -> gtk::Image {
    let image = match icon {
        Some(icon) => gtk::Image::from_gicon(&icon),
        None => gtk::Image::from_icon_name("application-x-executable-symbolic"),
    };
    image.set_icon_size(gtk::IconSize::Large);
    image
}

fn ignored_apps(settings: &gio::Settings) -> Vec<String> {
    settings.strv(IGNORED_APPS_KEY).iter().map(ToString::to_string).collect()
}

fn set_ignored_apps(settings: &gio::Settings, apps: Vec<String>) {
    if let Err(err) = settings.set_strv(IGNORED_APPS_KEY, apps) {
        eprintln!("laccy: cannot save ignored apps: {err}");
    }
}

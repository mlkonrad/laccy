//! The preferences window. Every control writes straight to GSettings, which
//! the popup, the daemon and the GNOME Shell extension all read.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use laccy_core::config::{
    self, HIGHLIGHT_MATCHES_KEY, HISTORY_SIZE_KEY, Highlight, IGNORED_APPS_KEY, IGNORED_PATTERNS_KEY,
    PASTE_AUTOMATICALLY_KEY, PASTE_WITHOUT_FORMATTING_KEY, PAUSED_KEY, PREVIEW_AUTOMATICALLY_KEY, PREVIEW_DELAY_KEY,
    RECORD_FILES_KEY, RECORD_IMAGES_KEY,
    RECORD_TEXT_KEY, SEARCH_MODE_KEY, SHOW_APP_ICONS_KEY, SHOW_COLOR_SWATCHES_KEY, SHOW_INDICATOR_KEY,
    PIN_TO_KEY, SHOW_LATEST_COPY_KEY, SHOW_SPECIAL_SYMBOLS_KEY, SORT_BY_KEY, TOGGLE_SHORTCUT_KEY,
};
use laccy_core::history::{PinPosition, SortOrder};
use laccy_core::search;

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
    page.add(&appearance_group(settings));
    page.add(&preview_group(settings));
    page.add(&history_group(settings));
    page.add(&ignored_apps_group(settings));
    page.add(&ignored_patterns_group(settings));
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
        .subtitle("The keyboard shortcut that shows your clipboard history")
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
        .subtitle("Paste the item you pick right away instead of only copying it. Alt+Enter does the opposite")
        .build();
    settings.bind(PASTE_AUTOMATICALLY_KEY, &paste_row, "active").build();
    group.add(&paste_row);

    let formatting_row = adw::SwitchRow::builder()
        .title("Paste Without Formatting")
        .subtitle("Paste text without bold, links or other styling. Alt+Shift+Enter does the opposite")
        .build();
    settings.bind(PASTE_WITHOUT_FORMATTING_KEY, &formatting_row, "active").build();
    group.add(&formatting_row);

    let search_row = choice_row(
        settings,
        SEARCH_MODE_KEY,
        search::Mode::ALL.map(|mode| {
            let label = match mode {
                search::Mode::Exact => "Exact",
                search::Mode::Fuzzy => "Fuzzy",
                search::Mode::Regex => "Regular Expression",
                search::Mode::Mixed => "Mixed",
            };
            (mode.name(), label)
        }),
    );
    search_row.set_title("Search Mode");
    // Only the chosen mode is described, so the row stays short enough to show its value.
    let describe = |row: &adw::ComboRow| {
        let description = match search::Mode::ALL.get(row.selected() as usize) {
            Some(search::Mode::Exact) => "Finds items that contain every word you type",
            Some(search::Mode::Fuzzy) => "Also finds words with letters left out, so “gco” finds “git checkout”",
            Some(search::Mode::Regex) => "Finds patterns, like “^https?://” for links",
            Some(search::Mode::Mixed) => "Tries Exact, then Regular Expression, then Fuzzy, until something matches",
            None => "",
        };
        row.set_subtitle(description);
    };
    describe(&search_row);
    search_row.connect_selected_notify(describe);
    group.add(&search_row);

    let pause_row = adw::SwitchRow::builder()
        .title("Pause Recording")
        .subtitle("Stop saving new copies for a while. Alt-click the top bar icon to switch it quickly")
        .build();
    settings.bind(PAUSED_KEY, &pause_row, "active").build();
    group.add(&pause_row);

    group
}

fn appearance_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Appearance").build();

    let highlight_row = choice_row(
        settings,
        HIGHLIGHT_MATCHES_KEY,
        Highlight::ALL.map(|style| {
            let label = match style {
                Highlight::Bold => "Bold",
                Highlight::Italic => "Italic",
                Highlight::Underline => "Underline",
                Highlight::Color => "Color",
            };
            (style.name(), label)
        }),
    );
    highlight_row.set_title("Highlight Matches");
    group.add(&highlight_row);

    for (key, title, subtitle) in [
        (SHOW_SPECIAL_SYMBOLS_KEY, "Show Special Symbols", "Make spaces at the ends, line breaks and tabs visible as ·, ⏎ and ⇥"),
        (SHOW_COLOR_SWATCHES_KEY, "Show Color Swatches", "Show a small square of color next to color codes like #3584e4"),
        (SHOW_APP_ICONS_KEY, "Show App Icons", "Show which app each item was copied from"),
        (SHOW_INDICATOR_KEY, "Show Icon in Top Bar", "Click it to open your clipboard history, right-click it for more options"),
    ] {
        let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).build();
        settings.bind(key, &row, "active").build();
        group.add(&row);
    }

    let latest_row = adw::SwitchRow::builder()
        .title("Show Latest Copy in Top Bar")
        .subtitle("Show the beginning of your most recent copy next to the icon")
        .build();
    settings.bind(SHOW_LATEST_COPY_KEY, &latest_row, "active").build();
    settings.bind(SHOW_INDICATOR_KEY, &latest_row, "sensitive").flags(gio::SettingsBindFlags::GET).build();
    group.add(&latest_row);

    group
}

fn preview_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Preview")
        .description("Press Ctrl+Space to see the selected item in full, along with where and when you copied it.")
        .build();

    let automatic_row = adw::SwitchRow::builder()
        .title("Open Automatically")
        .subtitle("Show the preview when you pause on an item")
        .build();
    settings.bind(PREVIEW_AUTOMATICALLY_KEY, &automatic_row, "active").build();
    group.add(&automatic_row);

    let delay_row = adw::SpinRow::with_range(200.0, 100_000.0, 100.0);
    delay_row.set_title("Delay");
    delay_row.set_subtitle("How long to wait before showing it, in milliseconds");
    settings.bind(PREVIEW_DELAY_KEY, &delay_row, "value").build();
    automatic_row.bind_property("active", &delay_row, "sensitive").sync_create().build();
    group.add(&delay_row);

    group
}

fn history_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("History").build();

    let size_row = adw::SpinRow::with_range(1.0, 10_000.0, 10.0);
    size_row.set_title("History Size");
    size_row.set_subtitle("How many items to remember. Pinned items are extra and always kept");
    settings.bind(HISTORY_SIZE_KEY, &size_row, "value").build();
    group.add(&size_row);

    let sort_row = choice_row(
        settings,
        SORT_BY_KEY,
        SortOrder::ALL.map(|order| {
            let label = match order {
                SortOrder::LastCopied => "Last Copied",
                SortOrder::FirstCopied => "First Copied",
                SortOrder::CopyCount => "Number of Copies",
            };
            (order.name(), label)
        }),
    );
    sort_row.set_title("Sort By");
    sort_row.set_subtitle("The order of items that aren't pinned");
    group.add(&sort_row);

    let pin_row = choice_row(
        settings,
        PIN_TO_KEY,
        PinPosition::ALL.map(|position| {
            let label = match position {
                PinPosition::Top => "Top",
                PinPosition::Bottom => "Bottom",
            };
            (position.name(), label)
        }),
    );
    pin_row.set_title("Pinned Items");
    pin_row.set_subtitle("Where pinned items are listed");
    group.add(&pin_row);

    for (key, title) in [(RECORD_TEXT_KEY, "Save Text"), (RECORD_IMAGES_KEY, "Save Images"), (RECORD_FILES_KEY, "Save Files")] {
        let row = adw::SwitchRow::builder().title(title).build();
        settings.bind(key, &row, "active").build();
        group.add(&row);
    }

    group
}

fn ignored_patterns_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Ignored Patterns")
        .description(
            "Copied text and files matching any of these regular expressions are never saved, like “^sk-” to skip \
             API keys. Upper and lower case must match unless a pattern starts with (?i).",
        )
        .build();

    let add_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Ignore a Pattern")
        .valign(gtk::Align::Center)
        .build();
    add_button.add_css_class("flat");
    add_button.connect_clicked(glib::clone!(
        #[strong]
        settings,
        move |button| pattern_dialog(&settings).present(Some(button))
    ));
    group.set_header_suffix(Some(&add_button));

    let placeholder = gtk::Label::builder()
        .label("No ignored patterns")
        .margin_top(12)
        .margin_bottom(12)
        .build();
    placeholder.add_css_class("dim-label");
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    list.set_placeholder(Some(&placeholder));
    group.add(&list);

    fill_ignored_patterns(&list, settings);
    settings.connect_changed(
        Some(IGNORED_PATTERNS_KEY),
        glib::clone!(
            #[weak]
            list,
            move |settings, _| fill_ignored_patterns(&list, settings)
        ),
    );

    group
}

fn fill_ignored_patterns(list: &gtk::ListBox, settings: &gio::Settings) {
    list.remove_all();
    for pattern in strings(settings, IGNORED_PATTERNS_KEY) {
        let row = adw::ActionRow::builder().title(&pattern).use_markup(false).build();
        row.add_css_class("monospace");

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
                    strings(&settings, IGNORED_PATTERNS_KEY).into_iter().filter(|other| *other != pattern).collect();
                set_strings(&settings, IGNORED_PATTERNS_KEY, remaining);
            }
        ));
        row.add_suffix(&remove_button);
        list.append(&row);
    }
}

/// A dialog that adds a regular expression to the ignored patterns once it's valid.
fn pattern_dialog(settings: &gio::Settings) -> adw::Dialog {
    let entry = adw::EntryRow::builder().title("Regular Expression").build();
    entry.add_css_class("monospace");
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    list.add_css_class("boxed-list");
    list.append(&entry);

    let add_button = gtk::Button::builder().label("_Add").use_underline(true).sensitive(false).build();
    add_button.add_css_class("suggested-action");
    let header = adw::HeaderBar::builder().show_end_title_buttons(false).build();
    header.pack_end(&add_button);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&list));
    let dialog = adw::Dialog::builder()
        .title("Ignore Pattern")
        .content_width(420)
        .child(&toolbar)
        .build();

    entry.connect_changed(glib::clone!(
        #[weak]
        add_button,
        move |entry| {
            let text = entry.text();
            // An empty entry isn't an error yet, just not addable.
            let error = (!text.is_empty()).then(|| config::pattern_error(&text)).flatten();
            if error.is_some() {
                entry.add_css_class("error");
            } else {
                entry.remove_css_class("error");
            }
            entry.set_tooltip_text(error.as_deref());
            add_button.set_sensitive(!text.is_empty() && error.is_none());
        }
    ));
    add_button.connect_clicked(glib::clone!(
        #[strong]
        settings,
        #[weak]
        dialog,
        #[weak]
        entry,
        move |_| {
            let pattern = entry.text().to_string();
            if config::pattern_error(&pattern).is_some() {
                return;
            }
            let mut patterns = strings(&settings, IGNORED_PATTERNS_KEY);
            if !patterns.contains(&pattern) {
                patterns.push(pattern);
                set_strings(&settings, IGNORED_PATTERNS_KEY, patterns);
            }
            dialog.close();
        }
    ));
    entry.connect_entry_activated(glib::clone!(
        #[weak]
        add_button,
        move |_| {
            if add_button.is_sensitive() {
                add_button.emit_clicked();
            }
        }
    ));

    dialog
}

fn ignored_apps_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Ignored Apps")
        .description(
            "Copies from these apps are never saved. Passwords copied from password managers are always skipped.",
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
    for app_id in strings(settings, IGNORED_APPS_KEY) {
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
                    strings(&settings, IGNORED_APPS_KEY).into_iter().filter(|ignored| *ignored != app_id).collect();
                set_strings(&settings, IGNORED_APPS_KEY,remaining);
            }
        ));
        row.add_suffix(&remove_button);
        list.append(&row);
    }
}

/// A dialog listing installed apps; picking one adds it to the ignored apps.
fn app_picker(settings: &gio::Settings) -> adw::Dialog {
    let ignored = strings(settings, IGNORED_APPS_KEY);
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
                let mut ignored = strings(&settings, IGNORED_APPS_KEY);
                ignored.push(app_id.clone());
                set_strings(&settings, IGNORED_APPS_KEY,ignored);
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

/// A combo row for a string setting whose values are the names in `choices`,
/// listed by their labels.
fn choice_row<const N: usize>(
    settings: &gio::Settings,
    key: &str,
    choices: [(&'static str, &'static str); N],
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| *label).collect();
    let row = adw::ComboRow::builder().model(&gtk::StringList::new(&labels)).build();
    let names: Vec<&'static str> = choices.iter().map(|(name, _)| *name).collect();
    let set_names = names.clone();
    settings
        .bind(key, &row, "selected")
        .mapping(move |variant, _| {
            let position = names.iter().position(|name| Some(*name) == variant.str())?;
            Some((position as u32).to_value())
        })
        .set_mapping(move |value, _| Some(set_names.get(value.get::<u32>().ok()? as usize)?.to_variant()))
        .build();
    row
}

fn strings(settings: &gio::Settings, key: &str) -> Vec<String> {
    settings.strv(key).iter().map(ToString::to_string).collect()
}

fn set_strings(settings: &gio::Settings, key: &str, values: Vec<String>) {
    if let Err(err) = settings.set_strv(key, values) {
        eprintln!("laccy: cannot save {key}: {err}");
    }
}

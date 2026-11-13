//! The preferences window. Every control writes straight to GSettings, which
//! the popup, the daemon and the GNOME Shell extension all read.
//!
//! Settings are split over tabs (General, Appearance, History, About), and the
//! ignored apps and patterns get subpages of their own, so a long list never
//! pushes the other settings out of view.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use laccy_core::config::{
    self, CLEAR_ON_LOGOUT_KEY, CLEAR_SYSTEM_CLIPBOARD_KEY, HIGHLIGHT_MATCHES_KEY, HISTORY_SIZE_KEY, Highlight, IGNORED_APPS_KEY, IGNORED_PATTERNS_KEY,
    PASTE_AUTOMATICALLY_KEY, PASTE_WITHOUT_FORMATTING_KEY, PAUSED_KEY, PREVIEW_AUTOMATICALLY_KEY, PREVIEW_DELAY_KEY,
    RECORD_FILES_KEY, RECORD_IMAGES_KEY,
    RECORD_TEXT_KEY, SEARCH_MODE_KEY, SHOW_APP_ICONS_KEY, SHOW_COLOR_SWATCHES_KEY, SHOW_INDICATOR_KEY,
    PIN_TO_KEY, SHOW_LATEST_COPY_KEY, SHOW_SPECIAL_SYMBOLS_KEY, SORT_BY_KEY, TOGGLE_SHORTCUT_KEY,
};
use laccy_core::history::{PinPosition, SortOrder};
use laccy_core::search;

const WINDOW_NAME: &str = "laccy-preferences";

const REPOSITORY_URL: &str = "https://github.com/mlkonrad/laccy";
const ISSUES_URL: &str = "https://github.com/mlkonrad/laccy/issues";
const DONATION_URL: &str = "https://buymeacoffee.com/mlkonrad";

/// Navigation tags of the main page and the subpages pushed over it.
const MAIN_PAGE: &str = "main";
const IGNORED_APPS_PAGE: &str = "ignored-apps";
const IGNORED_PATTERNS_PAGE: &str = "ignored-patterns";

/// Shows the preferences window, or raises it if it's already open.
pub fn present(app: &adw::Application) {
    if let Some(window) = app.windows().into_iter().find(|window| window.widget_name() == WINDOW_NAME) {
        window.present();
        return;
    }

    let navigation = adw::NavigationView::new();
    let stack = adw::ViewStack::new();
    let settings = config::settings();
    match &settings {
        Some(settings) => {
            stack.add_titled_with_icon(&general_page(settings), Some("general"), "General", "preferences-system-symbolic");
            stack.add_titled_with_icon(
                &appearance_page(settings),
                Some("appearance"),
                "Appearance",
                "preferences-desktop-appearance-symbolic",
            );
            stack.add_titled_with_icon(
                &history_page(settings, &navigation),
                Some("history"),
                "History",
                "document-open-recent-symbolic",
            );
        }
        None => {
            let missing = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .title("Extension Not Installed")
                .description("Laccy keeps its settings with its GNOME Shell extension. Install it with scripts/install-dev.sh.")
                .build();
            stack.add_titled_with_icon(&missing, Some("general"), "General", "preferences-system-symbolic");
        }
    }
    stack.add_titled_with_icon(&about_page(), Some("about"), "About", "help-about-symbolic");

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::builder().title_widget(&switcher).build());
    toolbar.set_content(Some(&stack));

    // The first page added is the one shown; subpages wait to be pushed.
    navigation.add(&adw::NavigationPage::builder().title("Preferences").tag(MAIN_PAGE).child(&toolbar).build());
    if let Some(settings) = &settings {
        navigation.add(&subpage(IGNORED_APPS_PAGE, "Ignored Apps", ignored_apps_group(settings)));
        navigation.add(&subpage(IGNORED_PATTERNS_PAGE, "Ignored Patterns", ignored_patterns_group(settings)));
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Laccy Preferences")
        .default_width(620)
        .default_height(680)
        .width_request(360)
        .height_request(400)
        .content(&navigation)
        .build();
    window.set_widget_name(WINDOW_NAME);

    // Escape leaves a subpage first, and closes the window from the tabs.
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new(glib::clone!(
            #[weak]
            navigation,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |widget, _| {
                if !navigation.pop()
                    && let Some(window) = widget.downcast_ref::<gtk::Window>()
                {
                    window.close();
                }
                glib::Propagation::Stop
            }
        ))),
    ));
    window.add_controller(shortcuts);

    window.present();
}

/// A navigation page showing `group` under its own header bar, which has a
/// back button and `add_button`.
fn subpage(tag: &str, title: &str, (group, add_button): (adw::PreferencesGroup, gtk::Button)) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let header = adw::HeaderBar::new();
    header.pack_end(&add_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    adw::NavigationPage::builder().tag(tag).title(title).child(&toolbar).build()
}

/// A row that opens the subpage tagged `tag` and counts the entries of the
/// string list setting `key` as its subtitle.
fn subpage_row(
    settings: &gio::Settings,
    navigation: &adw::NavigationView,
    key: &'static str,
    tag: &'static str,
    title: &str,
    count: fn(usize) -> String,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let show_count = move |settings: &gio::Settings, row: &adw::ActionRow| row.set_subtitle(&count(settings.strv(key).len()));
    show_count(settings, &row);
    settings.connect_changed(
        Some(key),
        glib::clone!(
            #[weak]
            row,
            move |settings, _| show_count(settings, &row)
        ),
    );
    row.connect_activated(glib::clone!(
        #[weak]
        navigation,
        move |_| navigation.push_by_tag(tag)
    ));
    row
}

fn general_page(settings: &gio::Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();

    let group = adw::PreferencesGroup::new();

    let shortcut_label = gtk::ShortcutLabel::builder()
        .disabled_text("Not Set")
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
        .subtitle("Click to change")
        .activatable(true)
        .build();
    shortcut_row.add_suffix(&shortcut_label);
    shortcut_row.connect_activated(glib::clone!(
        #[strong]
        settings,
        move |row| shortcut_dialog(&settings).present(Some(row))
    ));
    group.add(&shortcut_row);

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
            Some(search::Mode::Exact) => "Items containing every word you type",
            Some(search::Mode::Fuzzy) => "Letters may be left out, so “gco” finds “git checkout”",
            Some(search::Mode::Regex) => "Patterns, like “^https?://” for links",
            Some(search::Mode::Mixed) => "Exact, then regular expression, then fuzzy",
            None => "",
        };
        row.set_subtitle(description);
    };
    describe(&search_row);
    search_row.connect_selected_notify(describe);
    group.add(&search_row);
    page.add(&group);

    let paste_group = adw::PreferencesGroup::builder()
        .title("Pasting")
        .description("Alt+Enter and Alt+Shift+Enter do the opposite for a single item.")
        .build();
    for (key, title, subtitle) in [
        (PASTE_AUTOMATICALLY_KEY, "Paste Automatically", "Paste the item you pick instead of only copying it"),
        (PASTE_WITHOUT_FORMATTING_KEY, "Paste Without Formatting", "Leave out bold, links and other styling"),
    ] {
        let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).build();
        settings.bind(key, &row, "active").build();
        paste_group.add(&row);
    }
    page.add(&paste_group);

    page
}

fn appearance_page(settings: &gio::Settings) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();

    let list_group = adw::PreferencesGroup::builder().title("List").build();
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
    list_group.add(&highlight_row);
    for (key, title, subtitle) in [
        (SHOW_SPECIAL_SYMBOLS_KEY, "Special Symbols", "Show edge spaces, line breaks and tabs as ·, ⏎ and ⇥"),
        (SHOW_COLOR_SWATCHES_KEY, "Color Swatches", "Show a square of color next to codes like #3584e4"),
        (SHOW_APP_ICONS_KEY, "App Icons", "Show which app each item was copied from"),
    ] {
        let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).build();
        settings.bind(key, &row, "active").build();
        list_group.add(&row);
    }
    page.add(&list_group);

    let top_bar_group = adw::PreferencesGroup::builder().title("Top Bar").build();
    let indicator_row = adw::SwitchRow::builder()
        .title("Show Icon")
        .subtitle("Click it for your history, right-click it for more")
        .build();
    settings.bind(SHOW_INDICATOR_KEY, &indicator_row, "active").build();
    top_bar_group.add(&indicator_row);
    let latest_row = adw::SwitchRow::builder()
        .title("Show Latest Copy")
        .subtitle("The beginning of your last copy, next to the icon")
        .build();
    settings.bind(SHOW_LATEST_COPY_KEY, &latest_row, "active").build();
    settings.bind(SHOW_INDICATOR_KEY, &latest_row, "sensitive").flags(gio::SettingsBindFlags::GET).build();
    top_bar_group.add(&latest_row);
    page.add(&top_bar_group);

    let preview_group = adw::PreferencesGroup::builder()
        .title("Preview")
        .description("Ctrl+Space shows the selected item in full, with where and when you copied it.")
        .build();
    let automatic_row = adw::SwitchRow::builder()
        .title("Open Automatically")
        .subtitle("When you pause on an item")
        .build();
    settings.bind(PREVIEW_AUTOMATICALLY_KEY, &automatic_row, "active").build();
    preview_group.add(&automatic_row);
    let delay_row = adw::SpinRow::with_range(200.0, 100_000.0, 100.0);
    delay_row.set_title("Delay");
    delay_row.set_subtitle("In milliseconds");
    settings.bind(PREVIEW_DELAY_KEY, &delay_row, "value").build();
    automatic_row.bind_property("active", &delay_row, "sensitive").sync_create().build();
    preview_group.add(&delay_row);
    page.add(&preview_group);

    page
}

fn history_page(settings: &gio::Settings, navigation: &adw::NavigationView) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();

    let list_group = adw::PreferencesGroup::new();
    let size_row = adw::SpinRow::with_range(1.0, 10_000.0, 10.0);
    size_row.set_title("History Size");
    size_row.set_subtitle("Pinned items come on top of this");
    settings.bind(HISTORY_SIZE_KEY, &size_row, "value").build();
    list_group.add(&size_row);

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
    list_group.add(&sort_row);

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
    pin_row.set_title("Pinned Items Go To");
    list_group.add(&pin_row);
    page.add(&list_group);

    let save_group = adw::PreferencesGroup::builder().title("Save").build();
    for (key, title) in [(RECORD_TEXT_KEY, "Text"), (RECORD_IMAGES_KEY, "Images"), (RECORD_FILES_KEY, "Files")] {
        let row = adw::SwitchRow::builder().title(title).build();
        settings.bind(key, &row, "active").build();
        save_group.add(&row);
    }
    page.add(&save_group);

    let privacy_group = adw::PreferencesGroup::builder().title("Privacy").build();
    let pause_row = adw::SwitchRow::builder()
        .title("Pause Recording")
        .subtitle("Alt-click the top bar icon to switch it quickly")
        .build();
    settings.bind(PAUSED_KEY, &pause_row, "active").build();
    privacy_group.add(&pause_row);
    privacy_group.add(&subpage_row(settings, navigation, IGNORED_APPS_KEY, IGNORED_APPS_PAGE, "Ignored Apps", |count| {
        match count {
            0 => "None".to_owned(),
            1 => "1 app".to_owned(),
            count => format!("{count} apps"),
        }
    }));
    privacy_group.add(&subpage_row(
        settings,
        navigation,
        IGNORED_PATTERNS_KEY,
        IGNORED_PATTERNS_PAGE,
        "Ignored Patterns",
        |count| match count {
            0 => "None".to_owned(),
            1 => "1 pattern".to_owned(),
            count => format!("{count} patterns"),
        },
    ));
    // Laccy clears when it next starts after a login, see laccy_core::clear_if_new_login.
    let logout_row = adw::SwitchRow::builder()
        .title("Clear History on Logout")
        .subtitle("Start each login with only your pinned items")
        .build();
    settings.bind(CLEAR_ON_LOGOUT_KEY, &logout_row, "active").build();
    privacy_group.add(&logout_row);
    let clipboard_row = adw::SwitchRow::builder()
        .title("Clear the Clipboard Too")
        .subtitle("Clearing the history also empties what's copied now")
        .build();
    settings.bind(CLEAR_SYSTEM_CLIPBOARD_KEY, &clipboard_row, "active").build();
    privacy_group.add(&clipboard_row);
    page.add(&privacy_group);

    page
}

fn about_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .build();
    let icon = gtk::Image::builder().icon_name(laccy_core::APP_ID).pixel_size(128).margin_bottom(6).build();
    icon.add_css_class("icon-dropshadow");
    header.append(&icon);
    let name = gtk::Label::new(Some("Laccy"));
    name.add_css_class("title-1");
    header.append(&name);
    let tagline = gtk::Label::builder()
        .label("A lightweight clipboard manager for GNOME, inspired by Maccy")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    tagline.add_css_class("dim-label");
    header.append(&tagline);
    let version = gtk::Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
    version.add_css_class("dim-label");
    version.add_css_class("caption");
    header.append(&version);
    let header_group = adw::PreferencesGroup::new();
    header_group.add(&header);
    page.add(&header_group);

    let project_group = adw::PreferencesGroup::new();
    project_group.add(&link_row("Source Code", "github.com/mlkonrad/laccy", REPOSITORY_URL));
    project_group.add(&link_row("Report an Issue", "Found a bug or missing a feature? Let me know", ISSUES_URL));
    page.add(&project_group);

    let support_group = adw::PreferencesGroup::builder()
        .title("Support Laccy")
        .description("Laccy is free and open source. If it saves you time, a coffee helps keep it going.")
        .build();
    let coffee_button = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().icon_name("emote-love-symbolic").label("Buy Me a Coffee").build())
        .tooltip_text(DONATION_URL)
        .halign(gtk::Align::Center)
        .margin_top(6)
        .build();
    coffee_button.add_css_class("pill");
    coffee_button.add_css_class("suggested-action");
    coffee_button.connect_clicked(|button| open_uri(button, DONATION_URL));
    support_group.add(&coffee_button);
    page.add(&support_group);

    page
}

/// A row that opens `uri` in the browser.
fn link_row(title: &str, subtitle: &str, uri: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .tooltip_text(uri)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row.connect_activated(move |row| open_uri(row, uri));
    row
}

fn open_uri(widget: &impl IsA<gtk::Widget>, uri: &str) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
        if let Err(err) = result {
            eprintln!("laccy: cannot open link: {err}");
        }
    });
}

/// The ignored patterns list, and the button for its header bar that adds one.
fn ignored_patterns_group(settings: &gio::Settings) -> (adw::PreferencesGroup, gtk::Button) {
    let group = adw::PreferencesGroup::builder()
        .description(
            "Copied text and files matching any of these regular expressions are never saved, like “^sk-” to skip \
             API keys. Upper and lower case must match unless a pattern starts with (?i).",
        )
        .build();

    let add_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Ignore a Pattern")
        .build();
    add_button.connect_clicked(glib::clone!(
        #[strong]
        settings,
        move |button| pattern_dialog(&settings).present(Some(button))
    ));

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

    (group, add_button)
}

fn fill_ignored_patterns(list: &gtk::ListBox, settings: &gio::Settings) {
    remove_rows(list);
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

/// The ignored apps list, and the button for its header bar that adds one.
fn ignored_apps_group(settings: &gio::Settings) -> (adw::PreferencesGroup, gtk::Button) {
    let group = adw::PreferencesGroup::builder()
        .description(
            "Copies from these apps are never saved. Passwords copied from password managers are always skipped.",
        )
        .build();

    let add_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Ignore an App")
        .build();
    add_button.connect_clicked(glib::clone!(
        #[strong]
        settings,
        move |button| app_picker(&settings).present(Some(button))
    ));

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

    (group, add_button)
}

fn fill_ignored_apps(list: &gtk::ListBox, settings: &gio::Settings) {
    remove_rows(list);
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
                set_strings(&settings, IGNORED_APPS_KEY, remaining);
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
                set_strings(&settings, IGNORED_APPS_KEY, ignored);
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
        .description("Esc to cancel, Backspace to remove the shortcut")
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

/// Empties `list` but keeps its placeholder, which `ListBox::remove_all` drops too.
fn remove_rows(list: &gtk::ListBox) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
}

fn strings(settings: &gio::Settings, key: &str) -> Vec<String> {
    settings.strv(key).iter().map(ToString::to_string).collect()
}

fn set_strings(settings: &gio::Settings, key: &str, values: Vec<String>) {
    if let Err(err) = settings.set_strv(key, values) {
        eprintln!("laccy: cannot save {key}: {err}");
    }
}

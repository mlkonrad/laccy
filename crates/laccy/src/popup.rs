use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use laccy_core::config::Config;
use laccy_core::history::{Content, History, Item, Kind};
use laccy_core::{
    DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH, HISTORY_CHANGED_SIGNAL, SHELL_BUS_NAME, SHELL_INTERFACE,
    SHELL_OBJECT_PATH, THUMBNAIL_SCALE, files, search,
};

const PREVIEW_CHARS: usize = 100;
/// Items reachable with Ctrl+1 … Ctrl+9.
const NUMBERED_ITEMS: u32 = 9;
/// Tells the popup apart from the preferences window.
const WINDOW_NAME: &str = "laccy-popup";

/// Opens the popup, or closes it if it's already open.
pub fn toggle(app: &adw::Application) {
    match app.windows().into_iter().find(|window| window.widget_name() == WINDOW_NAME) {
        Some(window) => {
            let _ = WidgetExt::activate_action(&window, "popup.close", None);
        }
        None => Popup::present(app),
    }
}

pub struct Popup {
    window: adw::ApplicationWindow,
    search: gtk::SearchEntry,
    banner: adw::Banner,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    list: gtk::ListView,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    /// Shared with the rows, which load image thumbnails as they're shown.
    history: Rc<History>,
    config: Config,
    /// Every history item, in history order.
    objects: RefCell<Vec<glib::BoxedAnyObject>>,
    /// Normalized search text for each entry in `objects`.
    haystacks: RefCell<Vec<String>>,
    /// The last query that was a valid pattern, whose results stay listed
    /// while a regular expression is being typed.
    valid_query: RefCell<String>,
    /// Reloads when laccyd stores a copy; unsubscribes when the popup is gone.
    history_changed: RefCell<Option<gio::SignalSubscription>>,
    was_active: Cell<bool>,
    closing: Cell<bool>,
}

impl Popup {
    pub fn present(app: &adw::Application) {
        let history = match History::open_default() {
            Ok(history) => history,
            Err(err) => {
                eprintln!("laccy: cannot open history: {err}");
                return;
            }
        };
        let popup = Rc::new(Self::build(app, Rc::new(history), Config::load()));
        // Signal handlers only hold weak references, so the window has to keep
        // the popup alive; otherwise every handler silently does nothing.
        let keep_alive = RefCell::new(Some(Rc::clone(&popup)));
        popup.window.connect_destroy(move |_| drop(keep_alive.take()));
        popup.connect_signals();
        popup.reload();
        popup.check_extension();
        popup.window.present();
    }

    fn build(app: &adw::Application, history: Rc<History>, config: Config) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        let list = gtk::ListView::new(Some(selection.clone()), Some(row_factory(Rc::clone(&history))));
        list.set_single_click_activate(true);
        list.add_css_class("navigation-sidebar");

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let empty = adw::StatusPage::builder().icon_name("edit-paste-symbolic").build();
        empty.add_css_class("compact");

        let stack = gtk::Stack::new();
        stack.add_named(&scrolled, Some("items"));
        stack.add_named(&empty, Some("empty"));

        let search = gtk::SearchEntry::builder()
            .placeholder_text(match config.search_mode {
                search::Mode::Regex => "Search clipboard history with a regular expression",
                search::Mode::Exact | search::Mode::Fuzzy => "Search clipboard history",
            })
            .search_delay(0)
            .margin_top(8)
            .margin_bottom(4)
            .margin_start(8)
            .margin_end(8)
            .build();

        let banner = adw::Banner::new("Laccy's GNOME Shell extension isn't running, so picked items can't be copied");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(&search);
        content.append(&stack);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Laccy")
            .default_width(480)
            .default_height(540)
            .resizable(false)
            .content(&content)
            .build();
        search.set_key_capture_widget(Some(&window));
        window.set_widget_name(WINDOW_NAME);

        Self {
            window,
            search,
            banner,
            stack,
            empty,
            list,
            store,
            selection,
            history,
            config,
            objects: RefCell::default(),
            haystacks: RefCell::default(),
            valid_query: RefCell::default(),
            history_changed: RefCell::default(),
            was_active: Cell::new(false),
            closing: Cell::new(false),
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        // Lets `toggle` close the popup through the same path as Esc.
        let close = gio::SimpleAction::new("close", None);
        close.connect_activate(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_, _| popup.close()
        ));
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&close);
        self.window.insert_action_group("popup", Some(&actions));

        self.search.connect_search_changed(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| popup.apply_filter()
        ));

        self.list.connect_activate(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_, position| popup.pick(position, popup.config.paste_automatically)
        ));

        // Like Maccy, clicking anywhere else dismisses the popup.
        self.window.connect_is_active_notify(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |window| {
                if window.is_active() {
                    popup.was_active.set(true);
                } else if popup.was_active.get() && window.visible_dialog().is_none() {
                    popup.close();
                }
            }
        ));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| popup.on_key(key, state)
        ));
        self.window.add_controller(keys);

        // Copies made while the popup is open, like one from a script or Ctrl+C
        // in the search entry, show up without reopening it.
        if let Some(connection) = self.window.application().and_then(|app| app.dbus_connection()) {
            let subscription = connection.subscribe_to_signal(
                Some(DAEMON_BUS_NAME),
                Some(DAEMON_INTERFACE),
                Some(HISTORY_CHANGED_SIGNAL),
                Some(DAEMON_OBJECT_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                glib::clone!(
                    #[weak(rename_to = popup)]
                    self,
                    move |_| popup.reload()
                ),
            );
            self.history_changed.replace(Some(subscription));
        }
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        use gdk::ModifierType as Mod;

        if self.window.visible_dialog().is_some() {
            return glib::Propagation::Proceed;
        }
        let mods = state & gtk::accelerator_get_default_mod_mask();
        let paste = self.config.paste_automatically;
        match key {
            gdk::Key::Escape => self.close(),
            gdk::Key::Down if mods.is_empty() => self.move_selection(1),
            gdk::Key::Up if mods.is_empty() => self.move_selection(-1),
            gdk::Key::Return | gdk::Key::KP_Enter if mods.is_empty() => self.pick(self.selection.selected(), paste),
            gdk::Key::Return | gdk::Key::KP_Enter if mods == Mod::ALT_MASK => {
                self.pick(self.selection.selected(), !paste)
            }
            gdk::Key::p | gdk::Key::P if mods == Mod::ALT_MASK => self.toggle_pin(),
            gdk::Key::Delete | gdk::Key::KP_Delete if mods == Mod::ALT_MASK => self.delete_selected(),
            gdk::Key::Delete | gdk::Key::KP_Delete if mods == Mod::CONTROL_MASK | Mod::SHIFT_MASK => {
                self.confirm_clear()
            }
            gdk::Key::comma if mods == Mod::CONTROL_MASK => self.open_preferences(),
            _ if mods == Mod::CONTROL_MASK => match key.to_unicode().and_then(|c| c.to_digit(10)) {
                Some(digit @ 1..=NUMBERED_ITEMS) => self.pick(digit - 1, paste),
                _ => return glib::Propagation::Proceed,
            },
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// Reads the history again. The selected item stays selected if it's still
    /// listed, so a copy arriving meanwhile doesn't change what Enter picks.
    fn reload(&self) {
        let selected = self.item_at(self.selection.selected(), |item| item.id);
        let items = self.history.items().unwrap_or_else(|err| {
            eprintln!("laccy: cannot read history: {err}");
            Vec::new()
        });
        *self.haystacks.borrow_mut() = items.iter().map(|item| haystack(item, self.config.search_mode)).collect();
        *self.objects.borrow_mut() = items.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.apply_filter();
        let position = selected
            .and_then(|id| (0..self.store.n_items()).find(|&p| self.item_at(p, |item| item.id) == Some(id)));
        if let Some(position) = position {
            self.select(position);
        }
    }

    fn apply_filter(&self) {
        let objects = self.objects.borrow();
        let haystacks = self.haystacks.borrow();
        let query = self.search.text();
        let mode = self.config.search_mode;
        let ranked = match search::rank(&haystacks, &query, mode) {
            Ok(ranked) => {
                self.valid_query.replace(query.to_string());
                self.search.remove_css_class("error");
                self.search.set_tooltip_text(None);
                ranked
            }
            // An unfinished pattern, like "(a", keeps the last valid results.
            Err(err) => {
                self.search.add_css_class("error");
                self.search.set_tooltip_text(Some(err.message()));
                search::rank(&haystacks, &self.valid_query.borrow(), mode).unwrap_or_default()
            }
        };
        let matches: Vec<glib::BoxedAnyObject> = ranked
            .into_iter()
            .map(|index| objects[index].clone())
            .collect();
        self.store.splice(0, self.store.n_items(), &matches);

        if objects.is_empty() {
            self.empty.set_title("No Clipboard History");
            self.empty.set_description(Some("Copied text, images and files will show up here"));
        } else {
            self.empty.set_title("No Results");
            self.empty.set_description(None);
        }
        self.stack.set_visible_child_name(if matches.is_empty() { "empty" } else { "items" });
        if !matches.is_empty() {
            self.select(0);
        }
    }

    fn select(&self, position: u32) {
        self.selection.set_selected(position);
        self.list.scroll_to(position, gtk::ListScrollFlags::NONE, None);
    }

    fn move_selection(&self, delta: i64) {
        let count = self.store.n_items();
        if count == 0 {
            return;
        }
        let current = match self.selection.selected() {
            gtk::INVALID_LIST_POSITION => 0,
            position => i64::from(position) + delta,
        };
        self.select(current.clamp(0, i64::from(count) - 1) as u32);
    }

    fn item_at<T>(&self, position: u32, f: impl FnOnce(&Item) -> T) -> Option<T> {
        let object = self.store.item(position).and_downcast::<glib::BoxedAnyObject>()?;
        Some(f(&object.borrow::<Item>()))
    }

    /// Copies the item at `position` and closes the popup. The extension sets
    /// the clipboard so it outlives this process, then pastes if asked.
    fn pick(&self, position: u32, paste: bool) {
        let Some(id) = self.item_at(position, |item| item.id) else {
            return;
        };
        let content = match self.history.content(id) {
            Ok(Some(content)) => content,
            Ok(None) => return,
            Err(err) => {
                eprintln!("laccy: cannot read item: {err}");
                return;
            }
        };
        let Some(app) = self.window.application() else {
            return;
        };
        let Some(connection) = app.dbus_connection() else {
            eprintln!("laccy: not connected to the session bus");
            return;
        };

        let (method, parameters) = match content {
            Content::Text(text) => ("SetClipboard", (text, paste).to_variant()),
            Content::Files(uris) => {
                ("SetContent", set_content_parameters(files::MIME_TYPE, files::uri_list(&uris).as_bytes(), paste))
            }
            Content::Image { mime_type, data } => ("SetContent", set_content_parameters(&mime_type, &data, paste)),
        };
        // The extension doesn't record clipboard changes it makes itself, so
        // move the item to the top here.
        if let Err(err) = self.history.touch(id) {
            eprintln!("laccy: cannot update item: {err}");
        }

        // Stay alive until the extension replies, even though the window is gone.
        let hold = app.hold();
        // Closing first returns focus to the previous window before the paste.
        self.close();
        connection.call(
            Some(SHELL_BUS_NAME),
            SHELL_OBJECT_PATH,
            SHELL_INTERFACE,
            method,
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    eprintln!("laccy: the GNOME Shell extension could not set the clipboard: {err}");
                }
                drop(hold);
            },
        );
    }

    fn toggle_pin(&self) {
        let Some((id, pinned)) = self.item_at(self.selection.selected(), |item| (item.id, item.pinned)) else {
            return;
        };
        if let Err(err) = self.history.set_pinned(id, !pinned) {
            eprintln!("laccy: cannot update item: {err}");
            return;
        }
        // Keeps the item selected at its new position.
        self.reload();
    }

    fn delete_selected(&self) {
        let position = self.selection.selected();
        let Some(id) = self.item_at(position, |item| item.id) else {
            return;
        };
        if let Err(err) = self.history.delete(id) {
            eprintln!("laccy: cannot delete item: {err}");
            return;
        }
        self.reload();
        if let Some(last) = self.store.n_items().checked_sub(1) {
            self.select(position.min(last));
        }
    }

    fn confirm_clear(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Clear History?"),
            Some("All unpinned items will be removed. Pinned items are kept."),
        );
        dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear")]);
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            Some("clear"),
            glib::clone!(
                #[weak(rename_to = popup)]
                self,
                move |_, _| {
                    if let Err(err) = popup.history.clear(false) {
                        eprintln!("laccy: cannot clear history: {err}");
                    }
                    popup.reload();
                }
            ),
        );
        dialog.present(Some(&self.window));
    }

    fn open_preferences(&self) {
        if let Some(app) = self.window.application().and_downcast::<adw::Application>() {
            crate::preferences::present(&app);
        }
        self.close();
    }

    /// Shows a banner when the extension isn't there to copy picked items.
    fn check_extension(self: &Rc<Self>) {
        let Some(connection) = self.window.application().and_then(|app| app.dbus_connection()) else {
            return;
        };
        connection.call(
            Some(SHELL_BUS_NAME),
            SHELL_OBJECT_PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            Some(&(SHELL_INTERFACE,).to_variant()),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            1000,
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = popup)]
                self,
                move |result| popup.banner.set_revealed(result.is_err())
            ),
        );
    }

    fn close(&self) {
        if !self.closing.replace(true) {
            self.window.close();
        }
    }
}

/// What the search looks through: the text itself, or the paths of copied
/// files. Images have nothing to search, so they only show without a query.
fn haystack(item: &Item, mode: search::Mode) -> String {
    match item.kind {
        Kind::Text => search::normalize(&item.excerpt, mode),
        Kind::Files => {
            search::normalize(&item.excerpt.lines().map(files::display_path).collect::<Vec<_>>().join("\n"), mode)
        }
        Kind::Image => String::new(),
    }
}

/// Arguments of the extension's `SetContent` method.
fn set_content_parameters(mime_type: &str, data: &[u8], paste: bool) -> glib::Variant {
    glib::Variant::tuple_from_iter([
        mime_type.to_variant(),
        glib::Variant::array_from_fixed_array(data),
        paste.to_variant(),
    ])
}

fn thumbnail_texture(history: &History, id: i64) -> Option<gdk::Texture> {
    let png = history
        .thumbnail(id)
        .inspect_err(|err| eprintln!("laccy: cannot read thumbnail: {err}"))
        .ok()??;
    gdk::Texture::from_bytes(&glib::Bytes::from_owned(png))
        .inspect_err(|err| eprintln!("laccy: cannot load thumbnail: {err}"))
        .ok()
}

/// The icon of a copied file's type, or a generic one for several files.
fn files_icon(uris: &[&str]) -> gio::Icon {
    match uris {
        [uri] => gio::content_type_get_symbolic_icon(&gio::content_type_guess(Some(files::display_name(uri)), None).0),
        _ => gio::ThemedIcon::new("folder-documents-symbolic").upcast(),
    }
}

fn row_factory(history: Rc<History>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        let pin = gtk::Image::from_icon_name("view-pin-symbolic");
        let icon = gtk::Image::new();
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::ScaleDown)
            .alternative_text("Image")
            .build();
        // A picture asks to be as wide as its image has pixels; the clamp keeps
        // it to the width it's shown at.
        let thumbnail = adw::Clamp::builder()
            .child(&picture)
            .tightening_threshold(0)
            .halign(gtk::Align::Start)
            .build();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let shortcut = gtk::Label::new(None);
        shortcut.add_css_class("dim-label");
        shortcut.add_css_class("numeric");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(2);
        row.set_margin_bottom(2);
        row.append(&pin);
        row.append(&icon);
        row.append(&thumbnail);
        row.append(&label);
        row.append(&shortcut);
        list_item.set_child(Some(&row));

        // Items can move without being rebound, so follow the position itself.
        list_item.connect_position_notify(move |list_item| {
            let position = list_item.position();
            shortcut.set_text(&match position {
                p if p < NUMBERED_ITEMS => format!("Ctrl+{}", p + 1),
                _ => String::new(),
            });
        });
    });

    factory.connect_bind(move |_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        let object = list_item.item().and_downcast::<glib::BoxedAnyObject>().expect("store holds items");
        let item = object.borrow::<Item>();
        let row = list_item.child().expect("row was set up");
        let pin = row.first_child().expect("row has a pin icon");
        let icon = pin.next_sibling().and_downcast::<gtk::Image>().expect("row has a kind icon");
        let thumbnail = icon.next_sibling().and_downcast::<adw::Clamp>().expect("row has a thumbnail");
        let picture = thumbnail.child().and_downcast::<gtk::Picture>().expect("thumbnail has a picture");
        let label = thumbnail.next_sibling().and_downcast::<gtk::Label>().expect("row has a label");
        let shortcut = label.next_sibling().and_downcast::<gtk::Label>().expect("row has a shortcut label");

        pin.set_visible(item.pinned);

        // Loaded per row, so only the thumbnails on screen are ever decoded.
        let texture = (item.kind == Kind::Image).then(|| thumbnail_texture(&history, item.id)).flatten();
        if let Some(texture) = &texture {
            let (width, height) = (texture.width() / THUMBNAIL_SCALE, texture.height() / THUMBNAIL_SCALE);
            picture.set_size_request(width.max(1), height.max(1));
            thumbnail.set_maximum_size(width.max(1));
        }
        picture.set_paintable(texture.as_ref());
        thumbnail.set_visible(texture.is_some());

        let (kind_icon, text) = match item.kind {
            Kind::Text => (None, laccy_core::preview(&item.excerpt, PREVIEW_CHARS)),
            Kind::Files => {
                let uris: Vec<&str> = item.excerpt.lines().collect();
                let names: Vec<String> = uris.iter().map(|uri| files::display_name(uri)).collect();
                (Some(files_icon(&uris)), laccy_core::preview(&names.join(", "), PREVIEW_CHARS))
            }
            Kind::Image if texture.is_some() => (None, String::new()),
            // Without a thumbnail, at least say what it is.
            Kind::Image => (Some(gio::ThemedIcon::new("image-x-generic-symbolic").upcast()), "Image".to_owned()),
        };
        icon.set_visible(kind_icon.is_some());
        if let Some(kind_icon) = &kind_icon {
            icon.set_from_gicon(kind_icon);
        }
        label.set_text(&text);
        shortcut.set_text(&match list_item.position() {
            p if p < NUMBERED_ITEMS => format!("Ctrl+{}", p + 1),
            _ => String::new(),
        });
    });

    factory
}

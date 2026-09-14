use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use laccy_core::config::Config;
use laccy_core::history::{History, Item};
use laccy_core::{SHELL_BUS_NAME, SHELL_INTERFACE, SHELL_OBJECT_PATH, search};

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
    history: History,
    config: Config,
    /// Every history item, in history order.
    objects: RefCell<Vec<glib::BoxedAnyObject>>,
    /// Normalized search text for each entry in `objects`.
    haystacks: RefCell<Vec<String>>,
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
        let popup = Rc::new(Self::build(app, history, Config::load()));
        // Signal handlers only hold weak references, so the window has to keep
        // the popup alive; otherwise every handler silently does nothing.
        let keep_alive = RefCell::new(Some(Rc::clone(&popup)));
        popup.window.connect_destroy(move |_| drop(keep_alive.take()));
        popup.connect_signals();
        popup.reload();
        popup.check_extension();
        popup.window.present();
    }

    fn build(app: &adw::Application, history: History, config: Config) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        let list = gtk::ListView::new(Some(selection.clone()), Some(row_factory()));
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
            .placeholder_text("Search clipboard history")
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

    fn reload(&self) {
        let items = self.history.items().unwrap_or_else(|err| {
            eprintln!("laccy: cannot read history: {err}");
            Vec::new()
        });
        *self.haystacks.borrow_mut() = items.iter().map(|item| search::normalize(&item.excerpt)).collect();
        *self.objects.borrow_mut() = items.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.apply_filter();
    }

    fn apply_filter(&self) {
        let objects = self.objects.borrow();
        let matches: Vec<glib::BoxedAnyObject> = search::rank(&self.haystacks.borrow(), &self.search.text(), self.config.search_mode)
            .into_iter()
            .map(|index| objects[index].clone())
            .collect();
        self.store.splice(0, self.store.n_items(), &matches);

        if objects.is_empty() {
            self.empty.set_title("No Clipboard History");
            self.empty.set_description(Some("Copied text will show up here"));
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
        let text = match self.history.text(id) {
            Ok(Some(text)) => text,
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

        // Stay alive until the extension replies, even though the window is gone.
        let hold = app.hold();
        // Closing first returns focus to the previous window before the paste.
        self.close();
        connection.call(
            Some(SHELL_BUS_NAME),
            SHELL_OBJECT_PATH,
            SHELL_INTERFACE,
            "SetClipboard",
            Some(&(text, paste).to_variant()),
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
        self.reload();
        // Keep the item selected at its new position.
        if let Some(position) = (0..self.store.n_items()).find(|&p| self.item_at(p, |item| item.id) == Some(id)) {
            self.select(position);
        }
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

fn row_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        let pin = gtk::Image::from_icon_name("view-pin-symbolic");
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

    factory.connect_bind(|_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        let object = list_item.item().and_downcast::<glib::BoxedAnyObject>().expect("store holds items");
        let item = object.borrow::<Item>();
        let row = list_item.child().expect("row was set up");
        let pin = row.first_child().expect("row has a pin icon");
        let label = pin.next_sibling().and_downcast::<gtk::Label>().expect("row has a label");
        let shortcut = label.next_sibling().and_downcast::<gtk::Label>().expect("row has a shortcut label");

        pin.set_visible(item.pinned);
        label.set_text(&laccy_core::preview(&item.excerpt, PREVIEW_CHARS));
        shortcut.set_text(&match list_item.position() {
            p if p < NUMBERED_ITEMS => format!("Ctrl+{}", p + 1),
            _ => String::new(),
        });
    });

    factory
}

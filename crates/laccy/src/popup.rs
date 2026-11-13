use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use laccy_core::config::{Config, Highlight};
use laccy_core::history::{Content, Format, History, Item, Kind};
use laccy_core::{
    DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH, HISTORY_CHANGED_SIGNAL, PICKED_MIME_TYPE, SHELL_BUS_NAME,
    SHELL_INTERFACE, SHELL_OBJECT_PATH, THUMBNAIL_SCALE, files, search,
};

const PREVIEW_CHARS: usize = 100;
/// Unpinned items reachable with Ctrl+1 to Ctrl+9. Pinned items have their own keys.
const NUMBERED_ITEMS: usize = 9;
/// Tells the popup apart from the preferences window.
const WINDOW_NAME: &str = "laccy-popup";
const POPUP_WIDTH: i32 = 480;
const POPUP_HEIGHT: i32 = 540;
/// How much wider the popup gets while the preview is open.
const PREVIEW_WIDTH: i32 = 400;
/// More rows than fit in the popup, listed before it first shows (see `Popup::apply_filter`).
const INITIAL_ROWS: usize = 20;

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
    /// Clear, Preferences…, About and Quit below the list, like Maccy's menu.
    footer: gtk::ListBox,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    /// Shared with the rows, which load image thumbnails as they're shown.
    history: Rc<History>,
    rows: Rc<RowState>,
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
    /// Set while a dialog is open. libadwaita can show a dialog as its own
    /// window (it does for the non-resizable popup), which takes focus from
    /// the popup without it counting as a click elsewhere.
    dialog_open: Cell<bool>,
    closing: Cell<bool>,
    preview: Preview,
    /// Opens the preview once the selection rests, see [`Popup::schedule_preview`].
    preview_timeout: RefCell<Option<glib::SourceId>>,
    /// Updates the preview once, however many times the selection moved meanwhile.
    preview_update: RefCell<Option<glib::SourceId>>,
    /// Set when the preview is closed by hand, so it doesn't open by itself again.
    preview_dismissed: Cell<bool>,
    /// Matches past the first `INITIAL_ROWS`, listed once the first frame is drawn.
    deferred_rows: RefCell<Vec<glib::BoxedAnyObject>>,
    first_frame_drawn: Cell<bool>,
}

impl Popup {
    pub fn present(app: &adw::Application) {
        // GTK fades the text cursor in and out by redrawing the whole popup every
        // frame, about 12% of a core with the Cairo renderer, so it stays solid.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_cursor_blink(false);
        }
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
        // Otherwise GTK can start with the footer focused, where Enter would activate Clear.
        popup.search.grab_focus();
        popup.window.present();
        popup.list_deferred_rows_after_first_frame();
    }

    fn list_deferred_rows_after_first_frame(self: &Rc<Self>) {
        let Some(clock) = self.window.frame_clock() else {
            self.first_frame_drawn.set(true);
            self.list_deferred_rows();
            return;
        };
        let handler = Rc::new(RefCell::new(None));
        let id = clock.connect_after_paint(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            #[strong]
            handler,
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                popup.first_frame_drawn.set(true);
                // Pending input goes first: a query typed meanwhile lists everything itself.
                glib::idle_add_local_once(glib::clone!(
                    #[weak]
                    popup,
                    move || popup.list_deferred_rows()
                ));
            }
        ));
        handler.replace(Some(id));
    }

    fn list_deferred_rows(&self) {
        let deferred = self.deferred_rows.take();
        if !deferred.is_empty() {
            self.store.splice(self.store.n_items(), 0, &deferred);
        }
    }

    fn build(app: &adw::Application, history: Rc<History>, config: Config) -> Self {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(store.clone()));
        let rows = Rc::new(RowState {
            history: Rc::clone(&history),
            config: config.clone(),
            query: RefCell::default(),
            mode: Cell::new(config.search_mode),
            apps: OnceCell::new(),
            list_items: RefCell::default(),
            numbered: RefCell::default(),
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(row_factory(Rc::clone(&rows))));
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
                search::Mode::Exact | search::Mode::Fuzzy | search::Mode::Mixed => "Search clipboard history",
            })
            .search_delay(0)
            .margin_top(8)
            .margin_bottom(4)
            .margin_start(8)
            .margin_end(8)
            .build();

        let banner = adw::Banner::new("Laccy's GNOME Shell extension isn't running, so picked items can't be copied");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_width_request(POPUP_WIDTH);
        content.set_hexpand(true);
        content.append(&banner);
        content.append(&search);
        content.append(&stack);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let footer = footer();
        content.append(&footer);

        let preview = Preview::new();
        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.append(&content);
        columns.append(&preview.separator);
        columns.append(&preview.pane);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Laccy")
            .default_width(POPUP_WIDTH)
            .default_height(POPUP_HEIGHT)
            .resizable(false)
            .content(&columns)
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
            footer,
            store,
            selection,
            history,
            rows,
            config,
            objects: RefCell::default(),
            haystacks: RefCell::default(),
            valid_query: RefCell::default(),
            history_changed: RefCell::default(),
            was_active: Cell::new(false),
            dialog_open: Cell::new(false),
            closing: Cell::new(false),
            preview,
            preview_timeout: RefCell::default(),
            preview_update: RefCell::default(),
            preview_dismissed: Cell::new(false),
            deferred_rows: RefCell::default(),
            first_frame_drawn: Cell::new(false),
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

        self.selection.connect_selected_notify(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| {
                if popup.preview.is_open() {
                    popup.queue_preview_update();
                } else {
                    popup.schedule_preview();
                }
            }
        ));

        self.list.connect_activate(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_, position| popup.pick(position, popup.config.paste_automatically, popup.config.paste_without_formatting)
        ));

        // Both act on the selected item, which the preview shows.
        self.preview.pin.connect_clicked(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| popup.toggle_pin()
        ));
        self.preview.delete.connect_clicked(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| popup.delete_selected()
        ));

        self.footer.connect_row_activated(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_, row| match row.widget_name().as_str() {
                "clear" => popup.confirm_clear(false),
                "preferences" => popup.open_preferences(),
                "about" => popup.show_about(),
                "quit" => popup.quit(),
                name => unreachable!("footer has no {name} row"),
            }
        ));

        // Like Maccy, clicking anywhere else dismisses the popup.
        self.window.connect_is_active_notify(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |window| {
                if window.is_active() {
                    popup.was_active.set(true);
                } else if popup.was_active.get() && window.visible_dialog().is_none() && !popup.dialog_open.get() {
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
        // In the footer or the preview's buttons, reached with Tab, arrows move
        // between them and Enter activates one instead of picking the selected item.
        if (self.footer.focus_child().is_some() || self.preview.toolbar.focus_child().is_some())
            && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Up | gdk::Key::Down)
        {
            return glib::Propagation::Proceed;
        }
        let paste = self.config.paste_automatically;
        let plain = self.config.paste_without_formatting;
        match key {
            gdk::Key::Escape => self.close(),
            gdk::Key::Down if mods.is_empty() => self.move_selection(1),
            gdk::Key::Up if mods.is_empty() => self.move_selection(-1),
            gdk::Key::Return | gdk::Key::KP_Enter if mods.is_empty() => {
                self.pick(self.selection.selected(), paste, plain)
            }
            gdk::Key::Return | gdk::Key::KP_Enter if mods == Mod::ALT_MASK => {
                self.pick(self.selection.selected(), !paste, plain)
            }
            // Like Maccy's Option+Shift+Enter: always pastes, with the other formatting choice.
            gdk::Key::Return | gdk::Key::KP_Enter if mods == Mod::ALT_MASK | Mod::SHIFT_MASK => {
                self.pick(self.selection.selected(), true, !plain)
            }
            gdk::Key::p | gdk::Key::P if mods == Mod::ALT_MASK => self.toggle_pin(),
            gdk::Key::Delete | gdk::Key::KP_Delete if mods == Mod::ALT_MASK => self.delete_selected(),
            gdk::Key::Delete | gdk::Key::KP_Delete if mods == Mod::CONTROL_MASK | Mod::SHIFT_MASK => {
                self.confirm_clear(false)
            }
            // Like Maccy's Shift+Option+Command+Delete.
            gdk::Key::Delete | gdk::Key::KP_Delete if mods == Mod::CONTROL_MASK | Mod::ALT_MASK | Mod::SHIFT_MASK => {
                self.confirm_clear(true)
            }
            gdk::Key::comma if mods == Mod::CONTROL_MASK => self.open_preferences(),
            gdk::Key::q | gdk::Key::Q if mods == Mod::CONTROL_MASK => self.quit(),
            // Maccy's default preview shortcut.
            gdk::Key::space if mods == Mod::CONTROL_MASK => self.toggle_preview(),
            _ if mods == Mod::CONTROL_MASK => {
                let Some(c) = key.to_lower().to_unicode() else {
                    return glib::Propagation::Proceed;
                };
                let position = match c.to_digit(10) {
                    Some(digit @ 1..) => self.rows.numbered.borrow().get(digit as usize - 1).copied(),
                    Some(0) => None,
                    None => self.pinned_position(c),
                };
                match position {
                    Some(position) => self.pick(position, paste, plain),
                    // Ctrl+A, Ctrl+C and the like stay with the search field.
                    None => return glib::Propagation::Proceed,
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// Reads the history again. The selected item stays selected if it's still
    /// listed, so a copy arriving meanwhile doesn't change what Enter picks.
    fn reload(&self) {
        let selected = self.item_at(self.selection.selected(), |item| item.id);
        let items = self.history.items(self.config.sort_by).unwrap_or_else(|err| {
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
        // The item may be the same while its copy count and times changed.
        self.update_preview();
    }

    fn apply_filter(&self) {
        let objects = self.objects.borrow();
        let haystacks = self.haystacks.borrow();
        let query = self.search.text();
        let mode = self.config.search_mode;
        let (ranked, matched_mode) = match search::rank(&haystacks, &query, mode) {
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
                search::rank(&haystacks, &self.valid_query.borrow(), mode).unwrap_or((Vec::new(), mode))
            }
        };
        let mut matches: Vec<glib::BoxedAnyObject> = ranked
            .into_iter()
            .map(|index| objects[index].clone())
            .collect();
        // GTK builds a row for each of the first ~200 items, on screen or not.
        // Building them all before the popup first shows delays it by about
        // 40 ms, so until then only a screenful is listed.
        let deferred = if self.first_frame_drawn.get() {
            Vec::new()
        } else {
            matches.split_off(matches.len().min(INITIAL_ROWS))
        };
        self.deferred_rows.replace(deferred);
        // Rows bound by the splice highlight this query and show these
        // shortcuts; rows GTK keeps bound are updated after.
        self.rows.query.replace(self.valid_query.borrow().clone());
        self.rows.mode.set(matched_mode);
        self.rows.numbered.replace(
            matches
                .iter()
                .chain(self.deferred_rows.borrow().iter())
                .enumerate()
                .filter(|(_, object)| !object.borrow::<Item>().pinned)
                .map(|(position, _)| position as u32)
                .take(NUMBERED_ITEMS)
                .collect(),
        );
        self.store.splice(0, self.store.n_items(), &matches);
        self.rows.refresh_rows();

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

    /// Where the listed item pinned with `key` is, if it's listed.
    fn pinned_position(&self, key: char) -> Option<u32> {
        (0..self.store.n_items()).find(|&position| self.item_at(position, |item| item.pin_key) == Some(Some(key)))
    }

    /// Copies the item at `position` and closes the popup. The extension sets
    /// the clipboard so it outlives this process, then pastes if asked. Text
    /// with formatting, unless `plain`, is offered by this process instead
    /// (see [`offer_formatted`]).
    fn pick(&self, position: u32, paste: bool, plain: bool) {
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

        let formats = match &content {
            Content::Text(_) if !plain => self.history.formats(id).unwrap_or_else(|err| {
                eprintln!("laccy: cannot read formats: {err}");
                Vec::new()
            }),
            _ => Vec::new(),
        };

        let call = match content {
            Content::Text(text) if !formats.is_empty() => {
                // While the window still has focus, which Wayland requires to set the clipboard.
                offer_formatted(&app, &WidgetExt::display(&self.window), &text, formats);
                paste.then_some(("Paste", None))
            }
            Content::Text(text) => Some(("SetClipboard", Some((text, paste).to_variant()))),
            Content::Files(uris) => Some((
                "SetContent",
                Some(set_content_parameters(files::MIME_TYPE, files::uri_list(&uris).as_bytes(), paste)),
            )),
            Content::Image { mime_type, data } => {
                Some(("SetContent", Some(set_content_parameters(&mime_type, &data, paste))))
            }
        };
        // The extension doesn't record clipboard changes it or this process
        // makes, so move the item to the top here.
        if let Err(err) = self.history.touch(id) {
            eprintln!("laccy: cannot update item: {err}");
        }

        // Stay alive until the extension replies, even though the window is gone.
        let hold = app.hold();
        // Closing first returns focus to the previous window before the paste.
        self.close();
        let Some((method, parameters)) = call else {
            return;
        };
        connection.call(
            Some(SHELL_BUS_NAME),
            SHELL_OBJECT_PATH,
            SHELL_INTERFACE,
            method,
            parameters.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(err) = result {
                    eprintln!("laccy: the GNOME Shell extension's {method} call failed: {err}");
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

    /// Asks before removing all unpinned items, or every item when `include_pinned` is set.
    fn confirm_clear(self: &Rc<Self>, include_pinned: bool) {
        let dialog = if include_pinned {
            let dialog = adw::AlertDialog::new(
                Some("Clear All History?"),
                Some("All items, including pinned ones, will be removed."),
            );
            dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear All")]);
            dialog
        } else {
            let dialog = adw::AlertDialog::new(
                Some("Clear History?"),
                Some("All unpinned items will be removed. Pinned items are kept."),
            );
            dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear")]);
            dialog
        };
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            Some("clear"),
            glib::clone!(
                #[weak(rename_to = popup)]
                self,
                move |_, _| {
                    if let Err(err) = popup.history.clear(include_pinned) {
                        eprintln!("laccy: cannot clear history: {err}");
                    }
                    popup.reload();
                }
            ),
        );
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| popup.dialog_open.set(false)
        ));
        self.dialog_open.set(true);
        dialog.present(Some(&self.window));
    }

    fn open_preferences(&self) {
        if let Some(app) = self.window.application().and_downcast::<adw::Application>() {
            crate::preferences::present(&app);
        }
        self.close();
    }

    fn show_about(self: &Rc<Self>) {
        let dialog = adw::AboutDialog::builder()
            .application_name("Laccy")
            .application_icon("edit-paste-symbolic")
            .comments("A lightweight clipboard manager for GNOME, inspired by Maccy")
            .version(env!("CARGO_PKG_VERSION"))
            .website("https://github.com/mlkonrad/laccy")
            .license_type(gtk::License::MitX11)
            .copyright("© 2026 The Laccy contributors")
            .build();
        dialog.connect_closed(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move |_| popup.dialog_open.set(false)
        ));
        self.dialog_open.set(true);
        dialog.present(Some(&self.window));
    }

    /// Exits laccy, which also stops offering formatted text it picked earlier.
    /// Recording goes on: that's the extension and laccyd.
    fn quit(&self) {
        self.close();
        if let Some(app) = self.window.application() {
            app.quit();
        }
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
            for source in [self.preview_timeout.take(), self.preview_update.take()].into_iter().flatten() {
                source.remove();
            }
            self.window.close();
        }
    }

    fn toggle_preview(&self) {
        let open = !self.preview.is_open();
        if !open {
            self.preview_dismissed.set(true);
        }
        if let Some(source) = self.preview_timeout.take() {
            source.remove();
        }
        self.set_preview_open(open);
    }

    /// Shows or hides the preview, growing or shrinking the popup to fit.
    fn set_preview_open(&self, open: bool) {
        if self.preview.is_open() == open {
            return;
        }
        self.preview.pane.set_visible(open);
        self.preview.separator.set_visible(open);
        let width = if open { POPUP_WIDTH + PREVIEW_WIDTH } else { POPUP_WIDTH };
        self.window.set_default_size(width, POPUP_HEIGHT);
        if open {
            self.update_preview();
        } else {
            // Drops the full text or image.
            self.preview.show(&self.rows, None, None);
        }
    }

    /// Opens the preview once the selection has rested for the configured delay.
    fn schedule_preview(self: &Rc<Self>) {
        if let Some(source) = self.preview_timeout.take() {
            source.remove();
        }
        if !self.config.preview_automatically || self.preview_dismissed.get() || self.store.n_items() == 0 {
            return;
        }
        let source = glib::timeout_add_local_once(
            self.config.preview_delay,
            glib::clone!(
                #[weak(rename_to = popup)]
                self,
                move || {
                    // It has run, so it must not be removed later.
                    popup.preview_timeout.take();
                    popup.set_preview_open(true);
                }
            ),
        );
        self.preview_timeout.replace(Some(source));
    }

    fn queue_preview_update(self: &Rc<Self>) {
        if self.preview_update.borrow().is_some() {
            return;
        }
        let source = glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = popup)]
            self,
            move || {
                popup.preview_update.take();
                popup.update_preview();
            }
        ));
        self.preview_update.replace(Some(source));
    }

    /// Shows the selected item in the preview, if it's open.
    fn update_preview(&self) {
        if !self.preview.is_open() {
            return;
        }
        let item = self.item_at(self.selection.selected(), Item::clone);
        let content = item.as_ref().and_then(|item| {
            self.history
                .content(item.id)
                .inspect_err(|err| eprintln!("laccy: cannot read item: {err}"))
                .ok()
                .flatten()
        });
        self.preview.show(&self.rows, item.as_ref(), content);
    }
}

/// The pane next to the list that shows the selected item in full, like
/// Maccy's preview. Hidden until Ctrl+Space or the selection rests.
struct Preview {
    separator: gtk::Separator,
    pane: gtk::Box,
    text: gtk::TextView,
    text_scroll: gtk::ScrolledWindow,
    picture: gtk::Picture,
    app_title: gtk::Label,
    app: gtk::Box,
    app_icon: gtk::Image,
    app_name: gtk::Label,
    size_title: gtk::Label,
    size: gtk::Label,
    first_copied: gtk::Label,
    last_copied: gtk::Label,
    copies: gtk::Label,
    /// Pin and Delete buttons for the item, like Maccy's preview toolbar.
    toolbar: gtk::Box,
    /// Pressed in while the item is pinned, as GNOME has no unpin icon.
    pin: gtk::ToggleButton,
    delete: gtk::Button,
}

impl Preview {
    fn new() -> Self {
        let text = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .build();
        let text_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&text)
            .build();
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .vexpand(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();

        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(6)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let mut next_row = 0;
        let mut add_row = |title: &str, value: &gtk::Widget| {
            let title = gtk::Label::builder().label(title).xalign(0.0).build();
            title.add_css_class("dim-label");
            grid.attach(&title, 0, next_row, 1, 1);
            grid.attach(value, 1, next_row, 1, 1);
            next_row += 1;
            title
        };
        let value_label = || gtk::Label::builder().xalign(0.0).hexpand(true).ellipsize(pango::EllipsizeMode::End).build();

        let app_icon = gtk::Image::new();
        let app_name = value_label();
        let app = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        app.append(&app_icon);
        app.append(&app_name);
        let app_title = add_row("Application", app.upcast_ref());
        let size = value_label();
        let size_title = add_row("Size", size.upcast_ref());
        let first_copied = value_label();
        add_row("First Copied", first_copied.upcast_ref());
        let last_copied = value_label();
        add_row("Last Copied", last_copied.upcast_ref());
        let copies = value_label();
        add_row("Copies", copies.upcast_ref());

        // Clicks leave the focus in the search field, where typing goes on searching.
        let pin = gtk::ToggleButton::builder()
            .icon_name("view-pin-symbolic")
            .focus_on_click(false)
            .build();
        pin.add_css_class("flat");
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete (Alt+Delete)")
            .focus_on_click(false)
            .build();
        delete.add_css_class("flat");
        let toolbar = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .margin_bottom(6)
            .margin_end(6)
            .build();
        toolbar.append(&pin);
        toolbar.append(&delete);

        let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pane.set_width_request(PREVIEW_WIDTH);
        pane.set_visible(false);
        pane.append(&text_scroll);
        pane.append(&picture);
        pane.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        pane.append(&grid);
        pane.append(&toolbar);

        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        separator.set_visible(false);

        Self {
            separator,
            pane,
            text,
            text_scroll,
            picture,
            app_title,
            app,
            app_icon,
            app_name,
            size_title,
            size,
            first_copied,
            last_copied,
            copies,
            toolbar,
            pin,
            delete,
        }
    }

    fn is_open(&self) -> bool {
        self.pane.is_visible()
    }

    /// Shows `item` with its full `content`, or empties the pane.
    fn show(&self, rows: &RowState, item: Option<&Item>, content: Option<Content>) {
        let (text, texture) = match content {
            Some(Content::Text(text)) => (Some(text), None),
            Some(Content::Files(uris)) => {
                (Some(uris.iter().map(|uri| files::display_path(uri)).collect::<Vec<_>>().join("\n")), None)
            }
            Some(Content::Image { data, .. }) => (
                None,
                gdk::Texture::from_bytes(&glib::Bytes::from_owned(data))
                    .inspect_err(|err| eprintln!("laccy: cannot load image: {err}"))
                    .ok(),
            ),
            None => (None, None),
        };
        self.text_scroll.set_visible(texture.is_none());
        self.text.buffer().set_text(text.as_deref().unwrap_or(""));
        self.picture.set_visible(texture.is_some());
        self.size_title.set_visible(texture.is_some());
        self.size.set_visible(texture.is_some());
        if let Some(texture) = &texture {
            self.size.set_text(&format!("{} × {}", texture.width(), texture.height()));
        }
        self.picture.set_paintable(texture.as_ref());

        let source_app = item.and_then(|item| item.source_app.as_deref());
        let app = source_app.and_then(|app_id| rows.app(app_id));
        self.app_title.set_visible(source_app.is_some());
        self.app.set_visible(source_app.is_some());
        self.app_icon.set_visible(app.is_some());
        match &app {
            Some((icon, name)) => {
                self.app_icon.set_from_gicon(icon);
                self.app_name.set_text(name);
            }
            // Uninstalled since, or without an icon.
            None => self.app_name.set_text(source_app.unwrap_or("")),
        }
        self.first_copied.set_text(&item.map(|item| format_time(item.first_copied)).unwrap_or_default());
        self.last_copied.set_text(&item.map(|item| format_time(item.last_copied)).unwrap_or_default());
        self.copies.set_text(&item.map(|item| item.copy_count.to_string()).unwrap_or_default());

        let pinned = item.is_some_and(|item| item.pinned);
        self.pin.set_active(pinned);
        self.pin.set_tooltip_text(Some(if pinned { "Unpin (Alt+P)" } else { "Pin (Alt+P)" }));
        self.pin.set_sensitive(item.is_some());
        self.delete.set_sensitive(item.is_some());
    }
}

/// The footer's rows, each named after what it does, with its shortcut.
fn footer() -> gtk::ListBox {
    let footer = gtk::ListBox::new();
    footer.set_selection_mode(gtk::SelectionMode::None);
    footer.add_css_class("navigation-sidebar");
    for (name, title, shortcut) in [
        ("clear", "Clear", "Ctrl+Shift+Delete"),
        ("preferences", "Preferences…", "Ctrl+,"),
        ("about", "About", ""),
        ("quit", "Quit", "Ctrl+Q"),
    ] {
        let title = gtk::Label::builder().label(title).xalign(0.0).hexpand(true).build();
        let shortcut = gtk::Label::new(Some(shortcut));
        shortcut.add_css_class("dim-label");
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row_box.append(&title);
        row_box.append(&shortcut);
        let row = gtk::ListBoxRow::builder().child(&row_box).name(name).build();
        footer.append(&row);
    }
    footer
}

/// A local date and time for `millis` since the Unix epoch, in the locale's format.
fn format_time(millis: i64) -> String {
    glib::DateTime::from_unix_local(millis / 1000)
        .and_then(|time| time.format("%x %X"))
        .map(|time| time.to_string())
        .unwrap_or_default()
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

/// Puts a text item on the clipboard with its formatted versions. The
/// extension can only offer one format, and GNOME keeps only the plain text
/// when a clipboard owner quits, so this process offers them and keeps running,
/// without a window, until something else is copied.
fn offer_formatted(app: &gtk::Application, display: &gdk::Display, text: &str, formats: Vec<Format>) {
    // A string value is offered in every plain text type apps ask for.
    let mut providers = vec![gdk::ContentProvider::for_value(&text.to_value())];
    providers.extend(
        formats
            .into_iter()
            .map(|format| gdk::ContentProvider::for_bytes(&format.mime_type, &glib::Bytes::from_owned(format.data))),
    );
    providers.push(gdk::ContentProvider::for_bytes(PICKED_MIME_TYPE, &glib::Bytes::from_static(b"")));

    let clipboard = display.clipboard();
    if let Err(err) = clipboard.set_content(Some(&gdk::ContentProvider::new_union(&providers))) {
        eprintln!("laccy: cannot set the clipboard: {err}");
        return;
    }
    let hold = RefCell::new(Some(app.hold()));
    clipboard.connect_local_notify(move |clipboard| {
        if !clipboard.is_local() {
            drop(hold.take());
        }
    });
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

/// What rows show besides their item. Shared by the popup and its row factory.
struct RowState {
    history: Rc<History>,
    config: Config,
    /// The last valid query, whose matches rows highlight.
    query: RefCell<String>,
    /// The mode that matched it, which Mixed picks per query.
    mode: Cell<search::Mode>,
    /// Installed apps' icons and names by desktop ID, read when the first row needs one.
    apps: OnceCell<HashMap<String, (gio::Icon, String)>>,
    /// Every row's list item. GTK can keep rows bound when the search changes,
    /// so they're highlighted again through these.
    list_items: RefCell<Vec<glib::WeakRef<gtk::ListItem>>>,
    /// The positions of the listed items Ctrl+1 to Ctrl+9 pick: the first
    /// unpinned ones, so pinning doesn't renumber them.
    numbered: RefCell<Vec<u32>>,
}

impl RowState {
    /// The icon and name of the app with the desktop ID `app_id`, if it's installed and has an icon.
    fn app(&self, app_id: &str) -> Option<(gio::Icon, String)> {
        self.apps
            .get_or_init(|| {
                gio::AppInfo::all()
                    .into_iter()
                    .filter_map(|info| Some((info.id()?.to_string(), (info.icon()?, info.display_name().to_string()))))
                    .collect()
            })
            .get(app_id)
            .cloned()
    }

    /// Marks what the query matches in a row's label.
    fn highlight(&self, label: &gtk::Label) {
        let ranges = search::highlights(&label.text(), &self.query.borrow(), self.mode.get());
        label.set_attributes(highlight_attributes(&ranges, self.config.highlight_matches).as_ref());
    }

    /// The shortcut that picks `item` at `position`: its pin key, or its number.
    fn shortcut(&self, position: u32, item: &Item) -> String {
        if item.pinned {
            return item.pin_key.map(|key| format!("Ctrl+{}", key.to_ascii_uppercase())).unwrap_or_default();
        }
        match self.numbered.borrow().iter().position(|&numbered| numbered == position) {
            Some(index) => format!("Ctrl+{}", index + 1),
            None => String::new(),
        }
    }

    /// Updates a bound row's shortcut.
    fn update_shortcut(&self, list_item: &gtk::ListItem) {
        let (Some(object), Some(row)) = (list_item.item().and_downcast::<glib::BoxedAnyObject>(), list_item.child())
        else {
            return;
        };
        let shortcut: gtk::Label = named_child(&row, "shortcut");
        shortcut.set_text(&self.shortcut(list_item.position(), &object.borrow::<Item>()));
    }

    /// Highlights and shortcuts of the rows GTK kept bound through a new search.
    fn refresh_rows(&self) {
        self.list_items.borrow_mut().retain(|list_item| {
            let Some(list_item) = list_item.upgrade() else {
                return false;
            };
            if let (Some(_), Some(row)) = (list_item.item(), list_item.child()) {
                self.highlight(&named_child(&row, "text"));
                self.update_shortcut(&list_item);
            }
            true
        });
    }
}

/// Pango attributes that mark `ranges` (byte ranges of a label's text) in `style`.
fn highlight_attributes(ranges: &[Range<usize>], style: Highlight) -> Option<pango::AttrList> {
    if ranges.is_empty() {
        return None;
    }
    let attributes = pango::AttrList::new();
    for range in ranges {
        let marks: Vec<pango::Attribute> = match style {
            Highlight::Bold => vec![pango::AttrInt::new_weight(pango::Weight::Bold).into()],
            Highlight::Italic => vec![pango::AttrInt::new_style(pango::Style::Italic).into()],
            Highlight::Underline => vec![pango::AttrInt::new_underline(pango::Underline::Single).into()],
            // GNOME's yellow, see-through so the text stays readable in dark mode.
            Highlight::Color => vec![
                pango::AttrColor::new_background(0xf6f6, 0xd3d3, 0x2d2d).into(),
                pango::AttrInt::new_background_alpha(u16::MAX / 2).into(),
            ],
        };
        for mut mark in marks {
            mark.set_start_index(range.start as u32);
            mark.set_end_index(range.end as u32);
            attributes.insert(mark);
        }
    }
    Some(attributes)
}

/// The child of a row set up by [`row_factory`] with the widget name `name`.
fn named_child<T: IsA<gtk::Widget>>(row: &gtk::Widget, name: &str) -> T {
    let mut child = row.first_child();
    while let Some(widget) = child {
        if widget.widget_name() == name {
            return widget.downcast().expect("row child has the expected type");
        }
        child = widget.next_sibling();
    }
    panic!("row has no {name}");
}

fn row_factory(state: Rc<RowState>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    let setup_state = Rc::clone(&state);
    factory.connect_setup(move |_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        setup_state.list_items.borrow_mut().push(list_item.downgrade());
        let pin = gtk::Image::from_icon_name("view-pin-symbolic");
        pin.set_widget_name("pin");
        let app_icon = gtk::Image::new();
        app_icon.set_widget_name("app-icon");
        let icon = gtk::Image::new();
        icon.set_widget_name("kind-icon");
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
            .name("thumbnail")
            .build();
        let swatch = gtk::DrawingArea::builder()
            .content_width(14)
            .content_height(14)
            .valign(gtk::Align::Center)
            .name("swatch")
            .build();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .single_line_mode(true)
            .ellipsize(pango::EllipsizeMode::End)
            .name("text")
            .build();
        let shortcut = gtk::Label::builder().name("shortcut").build();
        shortcut.add_css_class("dim-label");
        shortcut.add_css_class("numeric");

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_margin_top(2);
        row.set_margin_bottom(2);
        for child in [pin.upcast_ref::<gtk::Widget>(), app_icon.upcast_ref(), icon.upcast_ref(), thumbnail.upcast_ref(), swatch.upcast_ref(), label.upcast_ref(), shortcut.upcast_ref()] {
            row.append(child);
        }
        list_item.set_child(Some(&row));

        // Items can move without being rebound, so follow the position itself.
        let position_state = Rc::clone(&setup_state);
        list_item.connect_position_notify(move |list_item| position_state.update_shortcut(list_item));
    });

    factory.connect_bind(move |_, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().expect("factory creates list items");
        let object = list_item.item().and_downcast::<glib::BoxedAnyObject>().expect("store holds items");
        let item = object.borrow::<Item>();
        let row = list_item.child().expect("row was set up");
        let pin: gtk::Image = named_child(&row, "pin");
        let app_icon: gtk::Image = named_child(&row, "app-icon");
        let icon: gtk::Image = named_child(&row, "kind-icon");
        let thumbnail: adw::Clamp = named_child(&row, "thumbnail");
        let picture = thumbnail.child().and_downcast::<gtk::Picture>().expect("thumbnail has a picture");
        let swatch: gtk::DrawingArea = named_child(&row, "swatch");
        let label: gtk::Label = named_child(&row, "text");
        let shortcut: gtk::Label = named_child(&row, "shortcut");

        pin.set_visible(item.pinned);

        let app = item
            .source_app
            .as_deref()
            .filter(|_| state.config.show_app_icons)
            .and_then(|app_id| state.app(app_id));
        app_icon.set_visible(app.is_some());
        if let Some((app_gicon, name)) = &app {
            app_icon.set_from_gicon(app_gicon);
            app_icon.set_tooltip_text(Some(name));
        }

        // Loaded per row, so only the thumbnails on screen are ever decoded.
        let texture = (item.kind == Kind::Image).then(|| thumbnail_texture(&state.history, item.id)).flatten();
        if let Some(texture) = &texture {
            let (width, height) = (texture.width() / THUMBNAIL_SCALE, texture.height() / THUMBNAIL_SCALE);
            picture.set_size_request(width.max(1), height.max(1));
            thumbnail.set_maximum_size(width.max(1));
        }
        picture.set_paintable(texture.as_ref());
        thumbnail.set_visible(texture.is_some());

        let (kind_icon, text) = match item.kind {
            Kind::Text => {
                (None, laccy_core::display_text(&item.excerpt, PREVIEW_CHARS, state.config.show_special_symbols))
            }
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
        let color = (state.config.show_color_swatches && item.kind == Kind::Text)
            .then(|| laccy_core::hex_color(&item.excerpt))
            .flatten();
        swatch.set_visible(color.is_some());
        if let Some([red, green, blue, alpha]) = color {
            swatch.set_draw_func(move |_, cr, width, height| {
                let channel = |value: u8| f64::from(value) / 255.0;
                cr.rectangle(0.5, 0.5, f64::from(width) - 1.0, f64::from(height) - 1.0);
                cr.set_source_rgba(channel(red), channel(green), channel(blue), channel(alpha));
                let _ = cr.fill_preserve();
                // An outline, so white shows on a light row and black on a dark one.
                cr.set_source_rgba(0.5, 0.5, 0.5, 0.5);
                cr.set_line_width(1.0);
                let _ = cr.stroke();
            });
        }

        label.set_text(&text);
        state.highlight(&label);
        shortcut.set_text(&state.shortcut(list_item.position(), &item));
    });

    factory
}

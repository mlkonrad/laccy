// Laccy's GNOME Shell extension.
//
// GNOME doesn't let regular apps watch the clipboard, register global
// shortcuts or type keys, so this extension does exactly those three things.
// Storage and UI live in the laccyd daemon and the laccy popup (see README.md).

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const APP_DESKTOP_ID = 'io.github.mlkonrad.Laccy.desktop';
// The popup window, told apart from the preferences window by its title.
const APP_ID = 'io.github.mlkonrad.Laccy';
const POPUP_TITLE = 'Laccy';

// Keybinding names are shared by all extensions (Caffeine also has a
// "toggle-shortcut"), so ours needs a prefix or GNOME refuses to add it.
const TOGGLE_SHORTCUT_KEY = 'laccy-toggle-shortcut';

const DAEMON_BUS_NAME = 'io.github.mlkonrad.Laccy.Daemon';
const DAEMON_OBJECT_PATH = '/io/github/mlkonrad/Laccy/Daemon';
const DAEMON_INTERFACE = 'io.github.mlkonrad.Laccy.Daemon1';

const SHELL_OBJECT_PATH = '/io/github/mlkonrad/Laccy/Shell';
const SHELL_INTERFACE_XML = `
<node>
  <interface name="io.github.mlkonrad.Laccy.Shell1">
    <method name="SetClipboard">
      <arg name="text" type="s" direction="in"/>
      <arg name="paste" type="b" direction="in"/>
    </method>
    <method name="SetContent">
      <arg name="mime_type" type="s" direction="in"/>
      <arg name="data" type="ay" direction="in"/>
      <arg name="paste" type="b" direction="in"/>
    </method>
    <method name="Paste"/>
  </interface>
</node>`;

// Password managers (KeePassXC, KDE apps) mark copied secrets with this type.
const SECRET_MIME_TYPE = 'x-kde-passwordManagerHint';
// Copied files, as file:// URIs.
const URI_LIST_MIME_TYPE = 'text/uri-list';
// The types St.Clipboard.get_text() reads.
const TEXT_MIME_TYPES = ['text/plain;charset=utf-8', 'UTF8_STRING', 'text/plain', 'STRING'];
// Formatted versions of copied text kept next to the plain text (FORMAT_MIME_TYPES in laccy-core).
const FORMAT_MIME_TYPES = ['text/html', 'text/rtf', 'application/rtf'];
// Offered by the laccy popup next to a formatted item it put back (PICKED_MIME_TYPE in laccy-core).
const PICKED_MIME_TYPE = 'application/x-laccy-picked';

// Pasting waits until the popup has given focus back, since Ctrl+V goes to the
// focused window, and until keys still held from picking (Alt+Enter,
// Alt+Shift+Enter) are released, since they would turn Ctrl+V into another
// shortcut. It checks this often, for up to this long.
const PASTE_POLL_MS = 10;
const PASTE_WAIT_MS = 5000;
const HELD_KEYS_MASK = Clutter.ModifierType.SHIFT_MASK | Clutter.ModifierType.MOD1_MASK |
    Clutter.ModifierType.SUPER_MASK | Clutter.ModifierType.HYPER_MASK | Clutter.ModifierType.META_MASK;

// Any program on the session bus can call SetClipboard. To keep that from
// becoming a way to type into other apps, pasting is only allowed once, and
// only within this long after the user opened the popup with the shortcut.
const PASTE_ALLOWED_US = 2 * 60 * GLib.USEC_PER_SEC;

const SHOW_INDICATOR_KEY = 'show-indicator';
const MAX_ITEM_BYTES_KEY = 'max-item-bytes';
const MAX_IMAGE_BYTES_KEY = 'max-image-bytes';
const PAUSED_KEY = 'paused';
const RECORD_TEXT_KEY = 'record-text';
const RECORD_IMAGES_KEY = 'record-images';
const RECORD_FILES_KEY = 'record-files';
const SHOW_LATEST_COPY_KEY = 'show-latest-copy';
const HISTORY_CHANGED_SIGNAL = 'HistoryChanged';
const INDICATOR_ICON = 'edit-paste-symbolic';
// The icon's opacity while recording is paused.
const PAUSED_OPACITY = 128;

// Asks before clearing the history, like Maccy's Clear (unpinned items) and
// Option+Clear (every item). `clear(includePinned)` does it.
const ClearHistoryDialog = GObject.registerClass(
class ClearHistoryDialog extends ModalDialog.ModalDialog {
    _init(clear) {
        super._init();
        this.contentLayout.add_child(new Dialog.MessageDialogContent({
            title: 'Clear History?',
            description: 'Clear Unpinned keeps pinned items. Clear All removes every item.',
        }));
        this.setButtons([{
            label: 'Cancel',
            action: () => this.close(),
            key: Clutter.KEY_Escape,
        }, {
            label: 'Clear All',
            action: () => {
                this.close();
                clear(true);
            },
        }, {
            label: 'Clear Unpinned',
            action: () => {
                this.close();
                clear(false);
            },
            default: true,
        }]);
    }
});

// Top bar icon: left-click opens the clipboard history, right-click shows a
// menu. Like Option-clicking Maccy's icon, Alt-click pauses or resumes
// recording and Alt+Shift-click ignores the next copy.
const LaccyIndicator = GObject.registerClass(
class LaccyIndicator extends PanelMenu.Button {
    // `actions` has openHistory(), openPreferences(), toggleIgnoreNext() and clearHistory().
    _init(settings, actions) {
        super._init(0.5, 'Laccy');
        this._settings = settings;

        const box = new St.BoxLayout({style_class: 'panel-status-menu-box'});
        this._icon = new St.Icon({
            icon_name: INDICATOR_ICON,
            style_class: 'system-status-icon',
        });
        box.add_child(this._icon);
        this._latestCopyLabel = new St.Label({y_align: Clutter.ActorAlign.CENTER, visible: false});
        box.add_child(this._latestCopyLabel);
        this.add_child(box);

        this.menu.addAction('Open Clipboard History', actions.openHistory);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._pauseItem = new PopupMenu.PopupSwitchMenuItem('Pause Recording', false);
        this._pauseItem.connect('toggled', (_item, state) => settings.set_boolean(PAUSED_KEY, state));
        this.menu.addMenuItem(this._pauseItem);
        this._ignoreNextItem = this.menu.addAction('Ignore Next Copy', actions.toggleIgnoreNext);
        this.menu.addAction('Clear History…', actions.clearHistory);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addAction('Preferences', actions.openPreferences);

        // PanelMenu.Button opens its menu on any click; split clicks by button instead.
        this._clickGesture.set_enabled(false);

        const leftClick = new Clutter.ClickGesture({required_button: Clutter.BUTTON_PRIMARY});
        leftClick.set_recognize_on_press(true);
        leftClick.connect('recognize', () => {
            const [, , mods] = global.get_pointer();
            if (!(mods & Clutter.ModifierType.MOD1_MASK))
                actions.openHistory();
            else if (mods & Clutter.ModifierType.SHIFT_MASK)
                actions.toggleIgnoreNext();
            else
                settings.set_boolean(PAUSED_KEY, !settings.get_boolean(PAUSED_KEY));
        });
        this.add_action(leftClick);

        const rightClick = new Clutter.ClickGesture({required_button: Clutter.BUTTON_SECONDARY});
        rightClick.set_recognize_on_press(true);
        rightClick.connect('recognize', () => this.menu.toggle());
        this.add_action(rightClick);

        settings.connectObject(`changed::${PAUSED_KEY}`, () => this._syncPaused(), this);
        this._syncPaused();
        this.setIgnoreNext(false);
    }

    _syncPaused() {
        const paused = this._settings.get_boolean(PAUSED_KEY);
        this._pauseItem.setToggleState(paused);
        this._icon.opacity = paused ? PAUSED_OPACITY : 255;
    }

    setIgnoreNext(pending) {
        this._ignoreNextItem.setOrnament(pending ? PopupMenu.Ornament.CHECK : PopupMenu.Ornament.HIDDEN);
    }

    // Shows `label` next to the icon, or nothing when it's empty.
    setLatestCopy(label) {
        this._latestCopyLabel.text = label;
        this._latestCopyLabel.visible = label !== '';
    }
});

export default class LaccyExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._pasteAllowedUntil = 0;
        this._pasteTimeoutId = 0;
        this._ignoreNext = false;
        this._settingClipboard = false;
        this._restoreIdleId = 0;

        this._clipboard = St.Clipboard.get_default();
        this._selection = global.display.get_selection();
        this._ownerChangedId = this._selection.connect('owner-changed',
            (_selection, type, source) => this._onOwnerChanged(type, source));

        this._windowCreatedId = global.display.connect('window-created',
            (_display, window) => this._onWindowCreated(window));

        this._keyboard = global.stage.context.get_backend().get_default_seat()
            .create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);

        this._dbus = Gio.DBusExportedObject.wrapJSObject(SHELL_INTERFACE_XML, this);
        this._dbus.export(Gio.DBus.session, SHELL_OBJECT_PATH);

        Main.wm.addKeybinding(TOGGLE_SHORTCUT_KEY, this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._togglePopup());

        this._indicator = null;
        this._historyChangedId = 0;
        this._showIndicatorChangedId = this._settings.connect(`changed::${SHOW_INDICATOR_KEY}`,
            () => this._syncIndicator());
        this._showLatestCopyChangedId = this._settings.connect(`changed::${SHOW_LATEST_COPY_KEY}`,
            () => this._syncLatestCopy());
        this._syncIndicator();
    }

    disable() {
        Main.wm.removeKeybinding(TOGGLE_SHORTCUT_KEY);

        this._settings.disconnect(this._showIndicatorChangedId);
        this._settings.disconnect(this._showLatestCopyChangedId);
        this._indicator?.destroy();
        this._indicator = null;
        this._syncLatestCopy();

        this._dbus.unexport();
        this._dbus = null;

        global.display.disconnect(this._windowCreatedId);
        for (const actor of global.get_window_actors())
            actor.meta_window.disconnectObject(this);
        this._selection.disconnect(this._ownerChangedId);
        this._selection = null;
        this._clipboard = null;
        this._stopWatchingForRestore();

        if (this._pasteTimeoutId) {
            GLib.Source.remove(this._pasteTimeoutId);
            this._pasteTimeoutId = 0;
        }
        this._keyboard.run_dispose();
        this._keyboard = null;
        this._settings = null;
    }

    _onOwnerChanged(type, source) {
        if (type !== Meta.SelectionType.SELECTION_CLIPBOARD)
            return;
        if (!source) {
            this._watchForRestore();
            return;
        }
        // When the app that owns the clipboard quits, GNOME keeps what it
        // copied by offering a copy from memory. That was recorded already.
        const restored = this._restoreIdleId !== 0 && source instanceof Meta.SelectionSourceMemory;
        this._stopWatchingForRestore();
        // Picked in the popup, which has already moved the item to the top.
        if (restored || this._settingClipboard)
            return;
        const mimeTypes = source.get_mimetypes();
        if (mimeTypes.includes(SECRET_MIME_TYPE))
            return;
        // A formatted item picked in the popup, which offers it itself and has
        // already moved it to the top.
        if (mimeTypes.includes(PICKED_MIME_TYPE))
            return;
        if (this._settings.get_boolean(PAUSED_KEY))
            return;
        if (this._ignoreNext) {
            this._setIgnoreNext(false);
            return;
        }

        // Windows without a desktop file get made-up IDs like "window:3"; don't record those.
        const focusAppId = Shell.WindowTracker.get_default().focus_app?.get_id() ?? '';
        const sourceApp = focusAppId.startsWith('window:') ? '' : focusAppId;

        // File managers also offer the paths as text, so files come first. Apps
        // that offer text next to an image (like spreadsheets) are recorded as text.
        const imageType = mimeTypes.includes('image/png')
            ? 'image/png' : mimeTypes.find(mimeType => mimeType.startsWith('image/'));
        if (mimeTypes.includes(URI_LIST_MIME_TYPE))
            this._recordFiles(sourceApp);
        else if (imageType && !mimeTypes.some(mimeType => TEXT_MIME_TYPES.includes(mimeType)))
            this._recordImage(imageType, sourceApp);
        else
            this._recordText(sourceApp, mimeTypes);
    }

    // GNOME offers its copy before the current main loop dispatch ends, if it
    // kept one. Anything else from memory, like a screenshot, comes later.
    _watchForRestore() {
        this._stopWatchingForRestore();
        this._restoreIdleId = GLib.idle_add(GLib.PRIORITY_HIGH, () => {
            this._restoreIdleId = 0;
            return GLib.SOURCE_REMOVE;
        });
    }

    _stopWatchingForRestore() {
        if (this._restoreIdleId) {
            GLib.Source.remove(this._restoreIdleId);
            this._restoreIdleId = 0;
        }
    }

    _setIgnoreNext(pending) {
        this._ignoreNext = pending;
        this._indicator?.setIgnoreNext(pending);
    }

    // Records the copied text, with its formatted versions if `mimeTypes` offers any.
    _recordText(sourceApp, mimeTypes = []) {
        // The daemon checks the record-* settings too; this avoids reading what it would drop.
        if (!this._settings.get_boolean(RECORD_TEXT_KEY))
            return;
        const formatTypes = FORMAT_MIME_TYPES.filter(mimeType => mimeTypes.includes(mimeType));
        this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clipboard, text) => {
            if (!this._selection || !text)
                return;
            if (formatTypes.length === 0) {
                this._callDaemon('AddText', new GLib.Variant('(ss)', [text, sourceApp]));
                return;
            }
            // The daemon would drop larger ones anyway; don't send them over D-Bus.
            const maxBytes = this._settings.get_uint(MAX_ITEM_BYTES_KEY);
            Promise.all(formatTypes.map(mimeType => this._readContent(mimeType, maxBytes))).then(contents => {
                if (!this._selection)
                    return;
                const formats = formatTypes
                    .map((mimeType, i) => [mimeType, contents[i]])
                    .filter(([, data]) => data)
                    .map(([mimeType, data]) => GLib.Variant.new_dict_entry(new GLib.Variant('s', mimeType), data));
                this._callDaemon('AddFormattedText', GLib.Variant.new_tuple([
                    new GLib.Variant('s', text),
                    GLib.Variant.new_array(new GLib.VariantType('{say}'), formats),
                    new GLib.Variant('s', sourceApp),
                ]));
            });
        });
    }

    // The clipboard's content in `mimeType` as an `ay` variant, or null if there
    // is none or it's larger than `maxBytes`.
    _readContent(mimeType, maxBytes) {
        return new Promise(resolve => {
            this._clipboard.get_content(St.ClipboardType.CLIPBOARD, mimeType, (_clipboard, bytes) => {
                // St frees the bytes when this callback returns (using them later
                // crashes gnome-shell), so the variant has to take its reference here.
                const size = bytes?.get_size() ?? 0;
                resolve(size > 0 && size <= maxBytes
                    ? GLib.Variant.new_from_bytes(new GLib.VariantType('ay'), bytes, true)
                    : null);
            });
        });
    }

    _recordFiles(sourceApp) {
        this._clipboard.get_content(St.ClipboardType.CLIPBOARD, URI_LIST_MIME_TYPE, (_clipboard, bytes) => {
            if (!this._selection)
                return;
            const uris = bytes?.get_size()
                ? new TextDecoder().decode(bytes.toArray()).split(/\r?\n/)
                    .map(line => line.trim())
                    .filter(line => line && !line.startsWith('#'))
                : [];
            // Links can come as a URI list too; those are recorded as text.
            if (uris.length > 0 && uris.every(uri => uri.startsWith('file://'))) {
                // Not recorded as their paths either.
                if (this._settings.get_boolean(RECORD_FILES_KEY))
                    this._callDaemon('AddFiles', new GLib.Variant('(ass)', [uris, sourceApp]));
            } else {
                this._recordText(sourceApp);
            }
        });
    }

    _recordImage(mimeType, sourceApp) {
        if (!this._settings.get_boolean(RECORD_IMAGES_KEY))
            return;
        this._clipboard.get_content(St.ClipboardType.CLIPBOARD, mimeType, (_clipboard, bytes) => {
            if (!this._selection || !bytes?.get_size())
                return;
            // The daemon would drop it anyway; don't send it over D-Bus.
            if (bytes.get_size() > this._settings.get_uint(MAX_IMAGE_BYTES_KEY))
                return;
            this._callDaemon('AddImage', GLib.Variant.new_tuple([
                GLib.Variant.new_from_bytes(new GLib.VariantType('ay'), bytes, true),
                new GLib.Variant('s', mimeType),
                new GLib.Variant('s', sourceApp),
            ]));
        });
    }

    // `what` finishes the warning logged when the call fails.
    _callDaemon(method, parameters, what = 'record clipboard item') {
        Gio.DBus.session.call(
            DAEMON_BUS_NAME, DAEMON_OBJECT_PATH, DAEMON_INTERFACE, method, parameters,
            null, Gio.DBusCallFlags.NONE, -1, null,
            (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch (e) {
                    console.warn(`Laccy: could not ${what}: ${e.message}`);
                }
            });
    }

    // The popup opens and closes without GNOME's window animations, which take
    // 150 ms each way to settle. A Wayland window gets its title and app ID
    // after it's created but before it's shown, so check again as they arrive.
    _onWindowCreated(window) {
        const actor = window.get_compositor_private();
        if (!actor)
            return;
        const skipIfPopup = () => {
            if (!this._isPopup(window))
                return;
            window.disconnectObject(this);
            // Pins, deletes and clears in the popup don't go through laccyd.
            window.connectObject('unmanaged', () => this._refreshLatestCopy(), this);
            Main.wm.skipNextEffect(actor);
            // Mapping uses up that skip; this one is for closing.
            const firstFrameId = actor.connect('first-frame', () => {
                actor.disconnect(firstFrameId);
                Main.wm.skipNextEffect(actor);
            });
        };
        window.connectObject(
            'notify::title', skipIfPopup,
            'notify::wm-class', skipIfPopup,
            'shown', () => window.disconnectObject(this),
            this);
        skipIfPopup();
    }

    _isPopup(window) {
        return window?.get_wm_class() === APP_ID && window.get_title() === POPUP_TITLE;
    }

    _syncIndicator() {
        const show = this._settings.get_boolean(SHOW_INDICATOR_KEY);
        if (show && !this._indicator) {
            this._indicator = new LaccyIndicator(this._settings, {
                openHistory: () => this._togglePopup(),
                openPreferences: () => this._openPreferences(),
                toggleIgnoreNext: () => this._setIgnoreNext(!this._ignoreNext),
                clearHistory: () => new ClearHistoryDialog(includePinned =>
                    this._callDaemon('Clear', new GLib.Variant('(b)', [includePinned]), 'clear the history')).open(),
            });
            this._indicator.setIgnoreNext(this._ignoreNext);
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        } else if (!show && this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
        this._syncLatestCopy();
    }

    // The latest copy's label is only asked for, and laccyd's HistoryChanged only
    // followed, while the top bar icon shows it.
    _syncLatestCopy() {
        const show = this._indicator !== null && this._settings?.get_boolean(SHOW_LATEST_COPY_KEY);
        if (show && !this._historyChangedId) {
            this._historyChangedId = Gio.DBus.session.signal_subscribe(
                DAEMON_BUS_NAME, DAEMON_INTERFACE, HISTORY_CHANGED_SIGNAL, DAEMON_OBJECT_PATH, null,
                Gio.DBusSignalFlags.NONE, () => this._refreshLatestCopy());
            this._refreshLatestCopy();
        } else if (!show && this._historyChangedId) {
            Gio.DBus.session.signal_unsubscribe(this._historyChangedId);
            this._historyChangedId = 0;
            this._indicator?.setLatestCopy('');
        }
    }

    _refreshLatestCopy() {
        if (!this._historyChangedId)
            return;
        Gio.DBus.session.call(
            DAEMON_BUS_NAME, DAEMON_OBJECT_PATH, DAEMON_INTERFACE, 'Latest', null,
            new GLib.VariantType('(s)'), Gio.DBusCallFlags.NONE, -1, null,
            (connection, result) => {
                let label = '';
                try {
                    [label] = connection.call_finish(result).deepUnpack();
                } catch (e) {
                    console.warn(`Laccy: could not read the latest copy: ${e.message}`);
                }
                // Turned off, or the extension disabled, meanwhile.
                if (this._historyChangedId)
                    this._indicator?.setLatestCopy(label);
            });
    }

    _lookupApp() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        if (!app) {
            Main.notifyError('Laccy is not installed',
                `${APP_DESKTOP_ID} was not found. Run scripts/install-dev.sh.`);
        }
        return app;
    }

    _openPreferences() {
        const app = this._lookupApp();
        if (!app)
            return;

        try {
            app.launch_action('preferences', global.get_current_time(), -1);
        } catch (e) {
            Main.notifyError('Laccy could not open preferences', e.message);
        }
    }

    _togglePopup() {
        const app = this._lookupApp();
        if (!app)
            return;

        this._pasteAllowedUntil = GLib.get_monotonic_time() + PASTE_ALLOWED_US;
        // Launch instead of activating: activating a running app only raises
        // its window (such as the preferences). The running laccy decides
        // whether to open or close the popup.
        try {
            app.launch(0, -1, Shell.AppLaunchGpu.APP_PREF);
        } catch (e) {
            Main.notifyError('Laccy could not start', e.message);
        }
    }

    // D-Bus method, called by the popup when a text item is picked.
    SetClipboard(text, paste) {
        this._setClipboard(() => this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text));
        this._pasteIfAllowed(paste);
        // The popup has moved the item to the top already.
        this._refreshLatestCopy();
    }

    // D-Bus method, called by the popup when an image or files are picked.
    SetContent(mimeType, data, paste) {
        this._setClipboard(() =>
            this._clipboard.set_content(St.ClipboardType.CLIPBOARD, mimeType, new GLib.Bytes(data)));
        this._pasteIfAllowed(paste);
        this._refreshLatestCopy();
    }

    // D-Bus method, called by the popup after it put a formatted item on the
    // clipboard itself.
    Paste() {
        this._pasteIfAllowed(true);
    }

    // Setting the clipboard emits owner-changed right away, which this flag
    // tells apart from copies made in other apps.
    _setClipboard(set) {
        this._settingClipboard = true;
        try {
            set();
        } finally {
            this._settingClipboard = false;
        }
    }

    _pasteIfAllowed(paste) {
        if (!paste)
            return;

        if (GLib.get_monotonic_time() > this._pasteAllowedUntil) {
            console.warn('Laccy: ignoring a paste request that did not follow the shortcut');
            return;
        }
        this._pasteAllowedUntil = 0;

        if (this._pasteTimeoutId)
            GLib.Source.remove(this._pasteTimeoutId);
        const start = GLib.get_monotonic_time();
        this._pasteTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, PASTE_POLL_MS, () => {
            const popupFocused = this._isPopup(global.display.focus_window);
            const [, , mods] = global.get_pointer();
            const held = (mods & HELD_KEYS_MASK) !== 0;
            if ((popupFocused || held) && GLib.get_monotonic_time() - start < PASTE_WAIT_MS * 1000)
                return GLib.SOURCE_CONTINUE;

            this._pasteTimeoutId = 0;
            if (popupFocused || held) {
                const reason = held ? 'modifier keys are still held' : 'the popup still has focus';
                console.warn(`Laccy: not pasting, because ${reason}`);
            } else {
                this._sendPasteKeys();
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    _sendPasteKeys() {
        const time = GLib.get_monotonic_time();
        this._keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.PRESSED);
        this._keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.PRESSED);
        this._keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.RELEASED);
        this._keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);    }
}

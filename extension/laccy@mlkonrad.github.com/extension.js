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
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';

const APP_DESKTOP_ID = 'io.github.mlkonrad.Laccy.desktop';

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
  </interface>
</node>`;

// Password managers (KeePassXC, KDE apps) mark copied secrets with this type.
const SECRET_MIME_TYPE = 'x-kde-passwordManagerHint';

// Gives focus time to return to the previous window after the popup closes.
const PASTE_DELAY_MS = 100;

// Any program on the session bus can call SetClipboard. To keep that from
// becoming a way to type into other apps, pasting is only allowed once, and
// only within this long after the user opened the popup with the shortcut.
const PASTE_ALLOWED_US = 2 * 60 * GLib.USEC_PER_SEC;

const SHOW_INDICATOR_KEY = 'show-indicator';
const INDICATOR_ICON = 'edit-paste-symbolic';

// Top bar icon: left-click opens the clipboard history, right-click shows a menu.
const LaccyIndicator = GObject.registerClass(
class LaccyIndicator extends PanelMenu.Button {
    _init(openHistory, openPreferences) {
        super._init(0.5, 'Laccy');

        this.add_child(new St.Icon({
            icon_name: INDICATOR_ICON,
            style_class: 'system-status-icon',
        }));

        this.menu.addAction('Open Clipboard History', openHistory);
        this.menu.addAction('Preferences', openPreferences);

        // PanelMenu.Button opens its menu on any click; split clicks by button instead.
        this._clickGesture.set_enabled(false);

        const leftClick = new Clutter.ClickGesture({required_button: Clutter.BUTTON_PRIMARY});
        leftClick.set_recognize_on_press(true);
        leftClick.connect('recognize', () => openHistory());
        this.add_action(leftClick);

        const rightClick = new Clutter.ClickGesture({required_button: Clutter.BUTTON_SECONDARY});
        rightClick.set_recognize_on_press(true);
        rightClick.connect('recognize', () => this.menu.toggle());
        this.add_action(rightClick);
    }
});

export default class LaccyExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._pasteAllowedUntil = 0;
        this._pasteTimeoutId = 0;
        this._ownText = null;

        this._selection = global.display.get_selection();
        this._ownerChangedId = this._selection.connect('owner-changed',
            (_selection, type, source) => this._onOwnerChanged(type, source));

        this._keyboard = global.stage.context.get_backend().get_default_seat()
            .create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);

        this._dbus = Gio.DBusExportedObject.wrapJSObject(SHELL_INTERFACE_XML, this);
        this._dbus.export(Gio.DBus.session, SHELL_OBJECT_PATH);

        Main.wm.addKeybinding(TOGGLE_SHORTCUT_KEY, this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._togglePopup());

        this._indicator = null;
        this._showIndicatorChangedId = this._settings.connect(`changed::${SHOW_INDICATOR_KEY}`,
            () => this._syncIndicator());
        this._syncIndicator();
    }

    disable() {
        Main.wm.removeKeybinding(TOGGLE_SHORTCUT_KEY);

        this._settings.disconnect(this._showIndicatorChangedId);
        this._indicator?.destroy();
        this._indicator = null;

        this._dbus.unexport();
        this._dbus = null;

        this._selection.disconnect(this._ownerChangedId);
        this._selection = null;

        if (this._pasteTimeoutId) {
            GLib.Source.remove(this._pasteTimeoutId);
            this._pasteTimeoutId = 0;
        }
        this._keyboard.run_dispose();
        this._keyboard = null;
        this._settings = null;
        this._ownText = null;
    }

    _onOwnerChanged(type, source) {
        if (type !== Meta.SelectionType.SELECTION_CLIPBOARD || !source)
            return;
        if (source.get_mimetypes().includes(SECRET_MIME_TYPE))
            return;

        // Windows without a desktop file get made-up IDs like "window:3"; don't record those.
        const focusAppId = Shell.WindowTracker.get_default().focus_app?.get_id() ?? '';
        const sourceApp = focusAppId.startsWith('window:') ? '' : focusAppId;
        St.Clipboard.get_default().get_text(St.ClipboardType.CLIPBOARD, (_clipboard, text) => {
            if (!this._selection || !text)
                return;

            // Text picked in the popup: moves to the top, but it wasn't copied
            // from whichever app has focus now.
            const fromLaccy = text === this._ownText;
            this._ownText = null;

            Gio.DBus.session.call(
                DAEMON_BUS_NAME, DAEMON_OBJECT_PATH, DAEMON_INTERFACE, 'AddText',
                new GLib.Variant('(ss)', [text, fromLaccy ? '' : sourceApp]),
                null, Gio.DBusCallFlags.NONE, -1, null,
                (connection, result) => {
                    try {
                        connection.call_finish(result);
                    } catch (e) {
                        console.warn(`Laccy: could not record clipboard item: ${e.message}`);
                    }
                });
        });
    }

    _syncIndicator() {
        const show = this._settings.get_boolean(SHOW_INDICATOR_KEY);
        if (show && !this._indicator) {
            this._indicator = new LaccyIndicator(
                () => this._togglePopup(),
                () => this._openPreferences());
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        } else if (!show && this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
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

    // D-Bus method, called by the popup when an item is picked.
    SetClipboard(text, paste) {
        this._ownText = text;
        St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text);
        if (!paste)
            return;

        if (GLib.get_monotonic_time() > this._pasteAllowedUntil) {
            console.warn('Laccy: ignoring a paste request that did not follow the shortcut');
            return;
        }
        this._pasteAllowedUntil = 0;

        if (this._pasteTimeoutId)
            GLib.Source.remove(this._pasteTimeoutId);
        this._pasteTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, PASTE_DELAY_MS, () => {
            this._pasteTimeoutId = 0;
            this._sendPasteKeys();
            return GLib.SOURCE_REMOVE;
        });
    }

    _sendPasteKeys() {
        const time = GLib.get_monotonic_time();
        this._keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.PRESSED);
        this._keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.PRESSED);
        this._keyboard.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.RELEASED);
        this._keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);
    }
}

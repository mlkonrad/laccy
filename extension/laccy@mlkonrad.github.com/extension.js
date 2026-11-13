// Laccy's GNOME Shell extension. It watches the clipboard, adds the global
// shortcut and types the paste keys; the Laccy app does the rest.

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
const INSTALL_URL = 'https://github.com/mlkonrad/laccy#build-and-install';
// The popup has this title; the preferences window doesn't.
const APP_ID = 'io.github.mlkonrad.Laccy';
const POPUP_TITLE = 'Laccy';

// Keybinding names are global, so this one is prefixed.
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
    <method name="ClearClipboard"/>
  </interface>
</node>`;

// Set by password managers on copied secrets.
const SECRET_MIME_TYPE = 'x-kde-passwordManagerHint';
// Most preferred first. Keep in sync with laccy-core.
const IMAGE_MIME_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp', 'image/bmp', 'image/tiff'];
const URI_LIST_MIME_TYPE = 'text/uri-list';
// What St.Clipboard.get_text() reads.
const TEXT_MIME_TYPES = ['text/plain;charset=utf-8', 'UTF8_STRING', 'text/plain', 'STRING'];
// Keep in sync with laccy-core.
const FORMAT_MIME_TYPES = ['text/html', 'text/rtf', 'application/rtf'];
// Offered by the popup when it puts a formatted item back itself.
const PICKED_MIME_TYPE = 'application/x-laccy-picked';

// Before pasting, wait for the popup to give focus back and for modifier
// keys to be released, or Ctrl+V goes to the wrong place.
const PASTE_POLL_MS = 10;
const PASTE_WAIT_MS = 5000;
const HELD_KEYS_MASK = Clutter.ModifierType.SHIFT_MASK | Clutter.ModifierType.MOD1_MASK |
    Clutter.ModifierType.SUPER_MASK | Clutter.ModifierType.HYPER_MASK | Clutter.ModifierType.META_MASK;

// Pasting is only allowed once, shortly after the user opened the popup.
const PASTE_ALLOWED_US = 2 * 60 * GLib.USEC_PER_SEC;

// Pressing the shortcut again while the popup is still starting closes it
// once it shows.
const LAUNCH_WAIT_MS = 3000;
// Closing waits for pending startup notices, so no busy cursor is left over.
const CLOSE_WAIT_MS = 500;
const CLOSE_POLL_MS = 20;
// Pressing it while the popup is closing reopens it afterwards.
const CLOSING_WAIT_US = 2 * GLib.USEC_PER_SEC;

const SHOW_INDICATOR_KEY = 'show-indicator';
const MAX_ITEM_BYTES_KEY = 'max-item-bytes';
const MAX_IMAGE_BYTES_KEY = 'max-image-bytes';
const PAUSED_KEY = 'paused';
const RECORD_TEXT_KEY = 'record-text';
const RECORD_IMAGES_KEY = 'record-images';
const RECORD_FILES_KEY = 'record-files';
const SHOW_LATEST_COPY_KEY = 'show-latest-copy';
const CLEAR_SYSTEM_CLIPBOARD_KEY = 'clear-system-clipboard';
const HISTORY_CHANGED_SIGNAL = 'HistoryChanged';
const INDICATOR_ICON = 'edit-paste-symbolic';
const PAUSED_OPACITY = 128;

// "device:inode" of the file at `path`, or null.
function fileIdentity(path) {
    return new Promise(resolve => {
        Gio.File.new_for_path(path).query_info_async('unix::device,unix::inode',
            Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_DEFAULT, null, (file, result) => {
                try {
                    const info = file.query_info_finish(result);
                    resolve(`${info.get_attribute_uint32('unix::device')}:${info.get_attribute_uint64('unix::inode')}`);
                } catch {
                    resolve(null);
                }
            });
    });
}

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

// Shown when the Laccy app isn't installed.
const MissingAppDialog = GObject.registerClass(
class MissingAppDialog extends ModalDialog.ModalDialog {
    constructor() {
        super();
        this.contentLayout.add_child(new Dialog.MessageDialogContent({
            title: 'Laccy App Needed',
            description: 'This extension keeps your clipboard history with the Laccy app, ' +
                'which isn\'t installed. Until it is, nothing you copy is recorded.',
        }));
        this.setButtons([{
            label: 'Close',
            action: () => this.close(),
            key: Clutter.KEY_Escape,
        }, {
            label: 'How to Install',
            action: () => {
                this.close();
                try {
                    Gio.AppInfo.launch_default_for_uri(INSTALL_URL, global.create_app_launch_context(0, -1));
                } catch (e) {
                    Main.notifyError('Laccy could not open the install instructions', e.message);
                }
            },
            default: true,
        }]);
    }
});

// Left-click opens the history, right-click the menu. Alt-click toggles
// pause, Alt+Shift-click ignores the next copy.
const LaccyIndicator = GObject.registerClass(
class LaccyIndicator extends PanelMenu.Button {
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

        // The default gesture opens the menu on any button.
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
        this._launchTimeoutId = 0;
        this._closeWhenShown = false;
        this._closeTimeoutId = 0;
        this._shownPopup = null;
        this._closingPopup = null;
        this._closeRequestedAt = 0;
        this._openWhenClosed = false;
        this._settingClipboard = false;
        this._restoreIdleId = 0;
        this._missingAppDialog = null;
        this._clearDialog = null;

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
        this._missingAppDialog?.destroy();
        this._missingAppDialog = null;
        this._clearDialog?.destroy();
        this._clearDialog = null;

        this._dbus.unexport();
        this._dbus = null;

        global.display.disconnect(this._windowCreatedId);
        for (const actor of global.get_window_actors()) {
            actor.disconnectObject(this);
            actor.meta_window.disconnectObject(this);
        }
        this._selection.disconnect(this._ownerChangedId);
        this._selection = null;
        this._clipboard = null;
        this._stopWatchingForRestore();

        if (this._pasteTimeoutId) {
            GLib.Source.remove(this._pasteTimeoutId);
            this._pasteTimeoutId = 0;
        }
        this._endLaunch();
        this._cancelClose();
        this._shownPopup = null;
        this._closingPopup = null;
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
        // GNOME's own copy after the owning app quit; already recorded.
        const restored = this._restoreIdleId !== 0 && source instanceof Meta.SelectionSourceMemory;
        this._stopWatchingForRestore();
        if (restored || this._settingClipboard)
            return;
        const mimeTypes = source.get_mimetypes();
        if (mimeTypes.includes(SECRET_MIME_TYPE))
            return;
        if (mimeTypes.includes(PICKED_MIME_TYPE))
            return;
        if (this._settings.get_boolean(PAUSED_KEY))
            return;
        if (this._ignoreNext) {
            this._setIgnoreNext(false);
            return;
        }
        // Without the app, don't read the clipboard at all.
        if (!this._appInstalled())
            return;

        // Windows without a desktop file get IDs like "window:3".
        const focusAppId = Shell.WindowTracker.get_default().focus_app?.get_id() ?? '';
        const sourceApp = focusAppId.startsWith('window:') ? '' : focusAppId;

        // File managers also offer paths as text, so check files first.
        const imageType = IMAGE_MIME_TYPES.find(mimeType => mimeTypes.includes(mimeType));
        if (mimeTypes.includes(URI_LIST_MIME_TYPE))
            this._recordFiles(sourceApp);
        else if (imageType && !mimeTypes.some(mimeType => TEXT_MIME_TYPES.includes(mimeType)))
            this._recordImage(imageType, sourceApp);
        else
            this._recordText(sourceApp, mimeTypes);
    }

    // GNOME restores its copy within the current main loop iteration.
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

    _recordText(sourceApp, mimeTypes = []) {
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

    // The clipboard content as an `ay` variant, or null if empty or too large.
    _readContent(mimeType, maxBytes) {
        return new Promise(resolve => {
            this._clipboard.get_content(St.ClipboardType.CLIPBOARD, mimeType, (_clipboard, bytes) => {
                // St frees `bytes` after this callback, so wrap it right away.
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
            // Non-file URIs, like links, are recorded as text.
            if (uris.length > 0 && uris.every(uri => uri.startsWith('file://'))) {
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
            if (bytes.get_size() > this._settings.get_uint(MAX_IMAGE_BYTES_KEY))
                return;
            this._callDaemon('AddImage', GLib.Variant.new_tuple([
                GLib.Variant.new_from_bytes(new GLib.VariantType('ay'), bytes, true),
                new GLib.Variant('s', mimeType),
                new GLib.Variant('s', sourceApp),
            ]));
        });
    }

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

    // Skips the window animations for the popup. Wayland windows get their
    // title and app ID after creation, so check again when they change.
    _onWindowCreated(window) {
        const actor = window.get_compositor_private();
        if (!actor)
            return;
        let skipped = false;
        const skipIfPopup = () => {
            if (skipped || !this._isPopup(window))
                return;
            skipped = true;
            Main.wm.skipNextEffect(actor);
            // Mapping uses up the first skip; this one is for closing.
            actor.connectObject('first-frame', () => {
                actor.disconnectObject(this);
                Main.wm.skipNextEffect(actor);
            }, this);
        };
        window.connectObject(
            'notify::title', skipIfPopup,
            'notify::wm-class', skipIfPopup,
            'shown', () => {
                window.disconnectObject(this);
                if (window.get_wm_class() !== APP_ID)
                    return;
                if (this._isPopup(window)) {
                    const closeNow = this._closeWhenShown;
                    this._endLaunch();
                    this._shownPopup = window;
                    window.connectObject('unmanaged', () => {
                        if (this._shownPopup === window)
                            this._shownPopup = null;
                        this._cancelClose();
                        // The popup may have changed the history directly.
                        this._refreshLatestCopy();
                        if (this._closingPopup === window) {
                            this._closingPopup = null;
                            if (this._openWhenClosed) {
                                this._openWhenClosed = false;
                                this._togglePopup();
                            }
                        }
                    }, this);
                    if (closeNow) {
                        this._closePopup(window);
                        return;
                    }
                }
                // Without startup notification GNOME may not focus it.
                window.activate(global.get_current_time());
            },
            this);
        skipIfPopup();
    }

    _endLaunch() {
        if (this._launchTimeoutId) {
            GLib.Source.remove(this._launchTimeoutId);
            this._launchTimeoutId = 0;
        }
        this._closeWhenShown = false;
    }

    _closePopup(window) {
        this._cancelClose();
        this._closingPopup = window;
        this._closeRequestedAt = GLib.get_monotonic_time();
        this._openWhenClosed = false;
        const tracker = Shell.WindowTracker.get_default();
        const start = GLib.get_monotonic_time();
        const close = () => {
            const waited = GLib.get_monotonic_time() - start;
            if (tracker.get_startup_sequences().length > 0 && waited < CLOSE_WAIT_MS * 1000)
                return GLib.SOURCE_CONTINUE;
            this._closeTimeoutId = 0;
            window.delete(global.get_current_time());
            return GLib.SOURCE_REMOVE;
        };
        if (close() === GLib.SOURCE_CONTINUE)
            this._closeTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, CLOSE_POLL_MS, close);
    }

    _cancelClose() {
        if (this._closeTimeoutId) {
            GLib.Source.remove(this._closeTimeoutId);
            this._closeTimeoutId = 0;
        }
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
                clearHistory: () => this._confirmClear(),
            });
            this._indicator.setIgnoreNext(this._ignoreNext);
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        } else if (!show && this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
        this._syncLatestCopy();
    }

    _confirmClear() {
        if (this._clearDialog || !this._lookupApp())
            return;
        this._clearDialog = new ClearHistoryDialog(includePinned => {
            this._callDaemon('Clear', new GLib.Variant('(b)', [includePinned]), 'clear the history');
            if (this._settings.get_boolean(CLEAR_SYSTEM_CLIPBOARD_KEY))
                this._clearClipboard();
        });
        this._clearDialog.connect('destroy', () => {
            this._clearDialog = null;
        });
        this._clearDialog.open();
    }

    // Only follows laccyd's HistoryChanged while the label is shown.
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
        if (!this._historyChangedId || !this._appInstalled())
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
                if (this._historyChangedId)
                    this._indicator?.setLatestCopy(label);
            });
    }

    _appInstalled() {
        return Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID) !== null;
    }

    // The Laccy app, or null after telling the user it's missing.
    _lookupApp() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        if (!app && !this._missingAppDialog) {
            this._missingAppDialog = new MissingAppDialog();
            this._missingAppDialog.connect('destroy', () => {
                this._missingAppDialog = null;
            });
            this._missingAppDialog.open();
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
        if (this._closingPopup && GLib.get_monotonic_time() - this._closeRequestedAt < CLOSING_WAIT_US) {
            this._openWhenClosed = !this._openWhenClosed;
            return;
        }
        this._closingPopup = null;
        if (this._shownPopup) {
            this._closePopup(this._shownPopup);
            return;
        }
        const popupComing = global.get_window_actors().some(actor => this._isPopup(actor.meta_window));
        if (this._launchTimeoutId || popupComing) {
            this._closeWhenShown = !this._closeWhenShown;
            return;
        }

        const app = this._lookupApp();
        if (!app)
            return;
        this._launchTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, LAUNCH_WAIT_MS, () => {
            this._launchTimeoutId = 0;
            this._closeWhenShown = false;
            return GLib.SOURCE_REMOVE;
        });

        this._pasteAllowedUntil = GLib.get_monotonic_time() + PASTE_ALLOWED_US;
        // Launch, not activate: activating only raises an open window.
        try {
            app.launch(0, -1, Shell.AppLaunchGpu.APP_PREF);
        } catch (e) {
            Main.notifyError('Laccy could not start', e.message);
        }
    }

    // D-Bus methods. The *Async names make GJS pass the invocation, so the
    // caller can be checked.

    SetClipboardAsync([text, paste], invocation) {
        this._fromLaccy(invocation, () => {
            this._setClipboard(() => this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text));
            this._pasteIfAllowed(paste);
            this._refreshLatestCopy();
        });
    }

    SetContentAsync([mimeType, data, paste], invocation) {
        this._fromLaccy(invocation, () => {
            this._setClipboard(() =>
                this._clipboard.set_content(St.ClipboardType.CLIPBOARD, mimeType, new GLib.Bytes(data)));
            this._pasteIfAllowed(paste);
            this._refreshLatestCopy();
        });
    }

    PasteAsync(_params, invocation) {
        this._fromLaccy(invocation, () => this._pasteIfAllowed(true));
    }

    ClearClipboardAsync(_params, invocation) {
        this._fromLaccy(invocation, () => this._clearClipboard());
    }

    // Empty text, because removing the owner would make GNOME restore its copy.
    _clearClipboard() {
        this._setClipboard(() => this._clipboard.set_text(St.ClipboardType.CLIPBOARD, ''));
    }

    // Only the Laccy app may call the D-Bus methods; otherwise any program on
    // the session bus could replace the clipboard or paste into other apps.
    _fromLaccy(invocation, run) {
        const sender = invocation.get_sender();
        this._isLaccy(sender).then(isLaccy => {
            if (!isLaccy) {
                console.warn(`Laccy: refused ${invocation.get_method_name()} from ${sender}, which is not laccy`);
                invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied',
                    'Only the Laccy popup may call this method');
                return;
            }
            if (this._clipboard)
                run();
            invocation.return_value(null);
        }).catch(e => {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.Failed', e.message);
        });
    }

    // Whether `sender` runs the same binary file as the desktop file's Exec.
    async _isLaccy(sender) {
        const executable = this._laccyExecutable();
        if (!sender || !executable)
            return false;
        const pid = await new Promise((resolve, reject) => {
            Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                'GetConnectionUnixProcessID', new GLib.Variant('(s)', [sender]), new GLib.VariantType('(u)'),
                Gio.DBusCallFlags.NONE, -1, null, (connection, result) => {
                    try {
                        resolve(connection.call_finish(result).deepUnpack()[0]);
                    } catch (e) {
                        reject(e);
                    }
                });
        });
        const [caller, expected] = await Promise.all([
            fileIdentity(`/proc/${pid}/exe`),
            fileIdentity(executable),
        ]);
        return caller !== null && caller === expected;
    }

    _laccyExecutable() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        const executable = app?.get_app_info()?.get_executable();
        if (!executable)
            return null;
        return GLib.path_is_absolute(executable) ? executable : GLib.find_program_in_path(executable);
    }

    // Marks our own clipboard changes so they aren't recorded as copies.
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
        this._keyboard.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);
    }
}

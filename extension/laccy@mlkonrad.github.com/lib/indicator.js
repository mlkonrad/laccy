// The top bar button with its menu and the optional latest-copy label.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import {DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH, HISTORY_CHANGED_SIGNAL} from './daemon.js';
import {ClearHistoryDialog} from './dialogs.js';
import {PAUSED_KEY, SHOW_LATEST_COPY_KEY} from './keys.js';

const INDICATOR_ICON = 'edit-paste-symbolic';
const PAUSED_OPACITY = 128;

// Left-click opens the history, right-click the menu. Alt-click toggles
// pause, Alt+Shift-click ignores the next copy.
export const LaccyIndicator = GObject.registerClass(
class LaccyIndicator extends PanelMenu.Button {
    _init(settings, app, actions) {
        super._init(0.5, 'Laccy');
        this._settings = settings;
        this._app = app;
        this._clearHistory = actions.clearHistory;
        this._clearDialog = null;
        this._historyChangedId = 0;

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
        this.menu.addAction('Clear History…', () => this._confirmClear());
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this.menu.addAction('Preferences', () => app.openPreferences());

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

        settings.connectObject(
            `changed::${PAUSED_KEY}`, () => this._syncPaused(),
            `changed::${SHOW_LATEST_COPY_KEY}`, () => this._syncLatestCopy(),
            this);
        this._syncPaused();
        this._syncLatestCopy();
        this.setIgnoreNext(false);
    }

    _onDestroy() {
        this._settings.disconnectObject(this);
        if (this._historyChangedId) {
            Gio.DBus.session.signal_unsubscribe(this._historyChangedId);
            this._historyChangedId = 0;
        }
        this._clearDialog?.disconnectObject(this);
        this._clearDialog?.destroy();
        this._clearDialog = null;
        super._onDestroy();
    }

    setIgnoreNext(pending) {
        this._ignoreNextItem.setOrnament(pending ? PopupMenu.Ornament.CHECK : PopupMenu.Ornament.HIDDEN);
    }

    _syncPaused() {
        const paused = this._settings.get_boolean(PAUSED_KEY);
        this._pauseItem.setToggleState(paused);
        this._icon.opacity = paused ? PAUSED_OPACITY : 255;
    }

    _confirmClear() {
        if (this._clearDialog || !this._app.lookup())
            return;
        this._clearDialog = new ClearHistoryDialog(this._clearHistory);
        this._clearDialog.connectObject('destroy', () => {
            this._clearDialog = null;
        }, this);
        this._clearDialog.open();
    }

    // Only follows laccyd's HistoryChanged while the label is shown.
    _syncLatestCopy() {
        const show = this._settings.get_boolean(SHOW_LATEST_COPY_KEY);
        if (show && !this._historyChangedId) {
            this._historyChangedId = Gio.DBus.session.signal_subscribe(
                DAEMON_BUS_NAME, DAEMON_INTERFACE, HISTORY_CHANGED_SIGNAL, DAEMON_OBJECT_PATH, null,
                Gio.DBusSignalFlags.NONE, () => this.refreshLatestCopy());
            this.refreshLatestCopy();
        } else if (!show && this._historyChangedId) {
            Gio.DBus.session.signal_unsubscribe(this._historyChangedId);
            this._historyChangedId = 0;
            this._setLatestCopy('');
        }
    }

    refreshLatestCopy() {
        if (!this._historyChangedId || !this._app.isInstalled())
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
                // Skip it if the label was turned off or the button destroyed meanwhile.
                if (this._historyChangedId)
                    this._setLatestCopy(label);
            });
    }

    _setLatestCopy(label) {
        this._latestCopyLabel.text = label;
        this._latestCopyLabel.visible = label !== '';
    }
});

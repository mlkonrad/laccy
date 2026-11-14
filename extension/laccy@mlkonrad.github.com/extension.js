// Laccy's GNOME Shell extension. It watches the clipboard, adds the global
// shortcut and types the paste keys; the Laccy app does the rest. Each part
// lives in its own module under lib/ and cleans up after itself.

import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {LaccyApp} from './lib/app.js';
import {ClipboardWatcher} from './lib/clipboard.js';
import {callDaemon} from './lib/daemon.js';
import {LaccyIndicator} from './lib/indicator.js';
import {CLEAR_SYSTEM_CLIPBOARD_KEY, SHOW_INDICATOR_KEY, TOGGLE_SHORTCUT_KEY} from './lib/keys.js';
import {Paster} from './lib/paster.js';
import {PopupLauncher} from './lib/popup.js';
import {ShellService} from './lib/service.js';

export default class LaccyExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._indicator = null;
        this._app = new LaccyApp();
        this._clipboard = new ClipboardWatcher(this._settings, this._app,
            pending => this._indicator?.setIgnoreNext(pending));
        this._paster = new Paster();
        this._popup = new PopupLauncher(this._app, {
            onLaunch: () => this._paster.allow(),
            // The popup may have changed the history directly.
            onClosed: () => this._indicator?.refreshLatestCopy(),
        });
        this._service = new ShellService(this._app, this._clipboard, this._paster,
            () => this._indicator?.refreshLatestCopy());

        Main.wm.addKeybinding(TOGGLE_SHORTCUT_KEY, this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._popup.toggle());

        this._showIndicatorChangedId = this._settings.connect(`changed::${SHOW_INDICATOR_KEY}`,
            () => this._syncIndicator());
        this._syncIndicator();
    }

    disable() {
        Main.wm.removeKeybinding(TOGGLE_SHORTCUT_KEY);
        this._settings.disconnect(this._showIndicatorChangedId);
        this._indicator?.destroy();
        this._indicator = null;

        this._service.destroy();
        this._service = null;
        this._popup.destroy();
        this._popup = null;
        this._paster.destroy();
        this._paster = null;
        this._clipboard.destroy();
        this._clipboard = null;
        this._app.destroy();
        this._app = null;
        this._settings = null;
    }

    _syncIndicator() {
        const show = this._settings.get_boolean(SHOW_INDICATOR_KEY);
        if (show && !this._indicator) {
            this._indicator = new LaccyIndicator(this._settings, this._app, {
                openHistory: () => this._popup.toggle(),
                toggleIgnoreNext: () => {
                    this._clipboard.ignoreNext = !this._clipboard.ignoreNext;
                },
                clearHistory: includePinned => this._clearHistory(includePinned),
            });
            this._indicator.setIgnoreNext(this._clipboard.ignoreNext);
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        } else if (!show && this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
    }

    _clearHistory(includePinned) {
        callDaemon('Clear', new GLib.Variant('(b)', [includePinned]), 'clear the history');
        if (this._settings.get_boolean(CLEAR_SYSTEM_CLIPBOARD_KEY))
            this._clipboard.clear();
    }
}

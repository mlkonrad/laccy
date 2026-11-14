// Finding and starting the Laccy app.

import GLib from 'gi://GLib';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {MissingAppDialog} from './dialogs.js';

const APP_DESKTOP_ID = 'io.github.mlkonrad.Laccy.desktop';
export const APP_ID = 'io.github.mlkonrad.Laccy';
// The popup has this title; the preferences window doesn't.
const POPUP_TITLE = 'Laccy';

export function isPopup(window) {
    return window?.get_wm_class() === APP_ID && window.get_title() === POPUP_TITLE;
}

export class LaccyApp {
    constructor() {
        this._missingAppDialog = null;
    }

    destroy() {
        this._missingAppDialog?.disconnectObject(this);
        this._missingAppDialog?.destroy();
        this._missingAppDialog = null;
    }

    isInstalled() {
        return Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID) !== null;
    }

    // The Laccy app, or null after telling the user it's missing.
    lookup() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        if (!app && !this._missingAppDialog) {
            this._missingAppDialog = new MissingAppDialog();
            this._missingAppDialog.connectObject('destroy', () => {
                this._missingAppDialog = null;
            }, this);
            this._missingAppDialog.open();
        }
        return app;
    }

    // The absolute path of the desktop file's Exec, or null.
    executable() {
        const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
        const executable = app?.get_app_info()?.get_executable();
        if (!executable)
            return null;
        return GLib.path_is_absolute(executable) ? executable : GLib.find_program_in_path(executable);
    }

    openPreferences() {
        const app = this.lookup();
        if (!app)
            return;
        try {
            app.launch_action('preferences', global.get_current_time(), -1);
        } catch (e) {
            Main.notifyError('Laccy could not open preferences', e.message);
        }
    }
}

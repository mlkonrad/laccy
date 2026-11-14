// Opens and closes the Laccy popup window.

import GLib from 'gi://GLib';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {APP_ID, isPopup} from './app.js';

// Pressing the shortcut again while the popup is still starting closes it
// once it shows.
const LAUNCH_WAIT_MS = 3000;
// Closing waits for pending startup notices, so no busy cursor is left over.
const CLOSE_WAIT_MS = 500;
const CLOSE_POLL_MS = 20;
// Pressing it while the popup is closing reopens it afterwards.
const CLOSING_WAIT_US = 2 * GLib.USEC_PER_SEC;

export class PopupLauncher {
    // `onLaunch()` runs when the popup is started, `onClosed()` after it closed.
    constructor(app, {onLaunch, onClosed}) {
        this._app = app;
        this._onLaunch = onLaunch;
        this._onClosed = onClosed;
        this._launchTimeoutId = 0;
        this._closeWhenShown = false;
        this._closeTimeoutId = 0;
        this._shownPopup = null;
        this._closingPopup = null;
        this._closeRequestedAt = 0;
        this._openWhenClosed = false;

        this._windowCreatedId = global.display.connect('window-created',
            (_display, window) => this._onWindowCreated(window));
    }

    destroy() {
        global.display.disconnect(this._windowCreatedId);
        for (const actor of global.get_window_actors()) {
            actor.disconnectObject(this);
            actor.meta_window.disconnectObject(this);
        }
        this._endLaunch();
        this._cancelClose();
        this._shownPopup = null;
        this._closingPopup = null;
    }

    toggle() {
        if (this._closingPopup && GLib.get_monotonic_time() - this._closeRequestedAt < CLOSING_WAIT_US) {
            this._openWhenClosed = !this._openWhenClosed;
            return;
        }
        this._closingPopup = null;
        if (this._shownPopup) {
            this._closePopup(this._shownPopup);
            return;
        }
        const popupComing = global.get_window_actors().some(actor => isPopup(actor.meta_window));
        if (this._launchTimeoutId || popupComing) {
            this._closeWhenShown = !this._closeWhenShown;
            return;
        }

        const app = this._app.lookup();
        if (!app)
            return;
        this._endLaunch();
        this._launchTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, LAUNCH_WAIT_MS, () => {
            this._launchTimeoutId = 0;
            this._closeWhenShown = false;
            return GLib.SOURCE_REMOVE;
        });

        this._onLaunch();
        // Launch, not activate: activating only raises an open window.
        try {
            app.launch(0, -1, Shell.AppLaunchGpu.APP_PREF);
        } catch (e) {
            Main.notifyError('Laccy could not start', e.message);
        }
    }

    // Skips the window animations for the popup. Wayland windows get their
    // title and app ID after creation, so check again when they change.
    _onWindowCreated(window) {
        const actor = window.get_compositor_private();
        if (!actor)
            return;
        let skipped = false;
        const skipIfPopup = () => {
            if (skipped || !isPopup(window))
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
                if (isPopup(window)) {
                    const closeNow = this._closeWhenShown;
                    this._endLaunch();
                    this._shownPopup = window;
                    window.connectObject('unmanaged', () => this._onPopupUnmanaged(window), this);
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

    _onPopupUnmanaged(window) {
        if (this._shownPopup === window)
            this._shownPopup = null;
        this._cancelClose();
        this._onClosed();
        if (this._closingPopup === window) {
            this._closingPopup = null;
            if (this._openWhenClosed) {
                this._openWhenClosed = false;
                this.toggle();
            }
        }
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
}

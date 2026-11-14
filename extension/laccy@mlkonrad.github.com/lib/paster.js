// Types Ctrl+V after the user picks an item in the popup.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';

import {isPopup} from './app.js';

// Before pasting, wait for the popup to give focus back and for modifier
// keys to be released, or Ctrl+V goes to the wrong place.
const PASTE_POLL_MS = 10;
const PASTE_WAIT_MS = 5000;
const HELD_KEYS_MASK = Clutter.ModifierType.SHIFT_MASK | Clutter.ModifierType.MOD1_MASK |
    Clutter.ModifierType.SUPER_MASK | Clutter.ModifierType.HYPER_MASK | Clutter.ModifierType.META_MASK;

// Pasting is only allowed once, shortly after the user opened the popup.
const PASTE_ALLOWED_US = 2 * 60 * GLib.USEC_PER_SEC;

export class Paster {
    constructor() {
        this._pasteAllowedUntil = 0;
        this._pasteTimeoutId = 0;
        this._keyboard = global.stage.context.get_backend().get_default_seat()
            .create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
    }

    destroy() {
        if (this._pasteTimeoutId) {
            GLib.Source.remove(this._pasteTimeoutId);
            this._pasteTimeoutId = 0;
        }
        this._keyboard = null;
    }

    // Called when the user opens the popup.
    allow() {
        this._pasteAllowedUntil = GLib.get_monotonic_time() + PASTE_ALLOWED_US;
    }

    pasteIfAllowed(paste) {
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
            const popupFocused = isPopup(global.display.focus_window);
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

// Moves the Laccy popup to where the popup-position setting asks for: at the
// mouse pointer or centered on the pointer's monitor. With 'automatic' the
// popup stays where Mutter placed it.

import {POPUP_POSITION_KEY} from './keys.js';

export class PopupPlacer {
    constructor(settings) {
        this._settings = settings;
        this._window = null;
        // Where the popup was opened: the pointer and its monitor.
        this._anchor = null;
    }

    destroy() {
        this._forget();
        this._settings = null;
    }

    // Call when the popup is shown, before its first frame is drawn, so it
    // doesn't visibly jump.
    place(window) {
        this._forget();
        const position = this._settings.get_string(POPUP_POSITION_KEY);
        if (position !== 'pointer' && position !== 'center')
            return;

        const [x, y] = global.get_pointer();
        this._window = window;
        this._anchor = {position, x, y, monitor: global.display.get_current_monitor()};
        // The preview makes the popup wider, which could push it off screen.
        window.connectObject(
            'size-changed', () => this._move(),
            'unmanaged', () => this._forget(),
            this);
        this._move();
    }

    _move() {
        const {position, x, y, monitor} = this._anchor;
        const area = this._window.get_work_area_for_monitor(monitor);
        const frame = this._window.get_frame_rect();
        let left, top;
        if (position === 'center') {
            left = area.x + Math.floor((area.width - frame.width) / 2);
            top = area.y + Math.floor((area.height - frame.height) / 2);
        } else {
            left = x;
            top = y;
        }
        // Keep it inside the work area, preferring the top-left edge if it
        // doesn't fit at all.
        left = Math.max(area.x, Math.min(left, area.x + area.width - frame.width));
        top = Math.max(area.y, Math.min(top, area.y + area.height - frame.height));
        if (left !== frame.x || top !== frame.y)
            this._window.move_frame(true, left, top);
    }

    _forget() {
        this._window?.disconnectObject(this);
        this._window = null;
        this._anchor = null;
    }
}

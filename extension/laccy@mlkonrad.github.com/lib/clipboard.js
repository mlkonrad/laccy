// Watches the clipboard, sends each copy to laccyd and sets the clipboard for
// the Laccy app.

import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {callDaemon} from './daemon.js';
import {
    MAX_IMAGE_BYTES_KEY, MAX_ITEM_BYTES_KEY, PAUSED_KEY,
    RECORD_FILES_KEY, RECORD_IMAGES_KEY, RECORD_TEXT_KEY,
} from './keys.js';

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

export class ClipboardWatcher {
    // `onIgnoreNextChanged(pending)` is called when "ignore next copy" changes.
    constructor(settings, app, onIgnoreNextChanged) {
        this._settings = settings;
        this._app = app;
        this._onIgnoreNextChanged = onIgnoreNextChanged;
        this._ignoreNext = false;
        this._settingClipboard = false;
        this._restoreIdleId = 0;

        this._clipboard = St.Clipboard.get_default();
        this._selection = global.display.get_selection();
        this._ownerChangedId = this._selection.connect('owner-changed',
            (_selection, type, source) => this._onOwnerChanged(type, source));
    }

    destroy() {
        this._selection.disconnect(this._ownerChangedId);
        this._selection = null;
        this._clipboard = null;
        this._stopWatchingForRestore();
        this._settings = null;
    }

    get ignoreNext() {
        return this._ignoreNext;
    }

    set ignoreNext(pending) {
        this._ignoreNext = pending;
        this._onIgnoreNextChanged(pending);
    }

    setText(text) {
        this._setClipboard(() => this._clipboard?.set_text(St.ClipboardType.CLIPBOARD, text));
    }

    setContent(mimeType, data) {
        this._setClipboard(() =>
            this._clipboard?.set_content(St.ClipboardType.CLIPBOARD, mimeType, new GLib.Bytes(data)));
    }

    // Empty text, because removing the owner would make GNOME restore its copy.
    clear() {
        this.setText('');
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
            this.ignoreNext = false;
            return;
        }
        // Without the app, don't read the clipboard at all.
        if (!this._app.isInstalled())
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

    _recordText(sourceApp, mimeTypes = []) {
        if (!this._settings.get_boolean(RECORD_TEXT_KEY))
            return;
        const formatTypes = FORMAT_MIME_TYPES.filter(mimeType => mimeTypes.includes(mimeType));
        this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clipboard, text) => {
            if (!this._selection || !text)
                return;
            if (formatTypes.length === 0) {
                callDaemon('AddText', new GLib.Variant('(ss)', [text, sourceApp]));
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
                callDaemon('AddFormattedText', GLib.Variant.new_tuple([
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
                    callDaemon('AddFiles', new GLib.Variant('(ass)', [uris, sourceApp]));
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
            callDaemon('AddImage', GLib.Variant.new_tuple([
                GLib.Variant.new_from_bytes(new GLib.VariantType('ay'), bytes, true),
                new GLib.Variant('s', mimeType),
                new GLib.Variant('s', sourceApp),
            ]));
        });
    }
}

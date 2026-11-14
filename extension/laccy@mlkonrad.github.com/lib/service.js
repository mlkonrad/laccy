// The D-Bus object the Laccy app calls to set the clipboard and paste.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const OBJECT_PATH = '/org/gnome/Shell/Extensions/Laccy';
const INTERFACE_XML = `
<node>
  <interface name="org.gnome.Shell.Extensions.Laccy">
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

export class ShellService {
    // `onClipboardSet()` runs after the app put an item on the clipboard.
    constructor(app, clipboard, paster, onClipboardSet) {
        this._app = app;
        this._clipboard = clipboard;
        this._paster = paster;
        this._onClipboardSet = onClipboardSet;
        this._dbus = Gio.DBusExportedObject.wrapJSObject(INTERFACE_XML, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
    }

    destroy() {
        this._dbus.unexport();
        this._dbus = null;
    }

    // D-Bus methods. The *Async names make GJS pass the invocation, so the
    // caller can be checked.

    SetClipboardAsync([text, paste], invocation) {
        this._fromLaccy(invocation, () => {
            this._clipboard.setText(text);
            this._paster.pasteIfAllowed(paste);
            this._onClipboardSet();
        });
    }

    SetContentAsync([mimeType, data, paste], invocation) {
        this._fromLaccy(invocation, () => {
            this._clipboard.setContent(mimeType, data);
            this._paster.pasteIfAllowed(paste);
            this._onClipboardSet();
        });
    }

    PasteAsync(_params, invocation) {
        this._fromLaccy(invocation, () => this._paster.pasteIfAllowed(true));
    }

    ClearClipboardAsync(_params, invocation) {
        this._fromLaccy(invocation, () => this._clipboard.clear());
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
            // Skip it if the extension was disabled during the check.
            if (this._dbus)
                run();
            invocation.return_value(null);
        }).catch(e => {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.Failed', e.message);
        });
    }

    // Whether `sender` runs the same binary file as the desktop file's Exec.
    async _isLaccy(sender) {
        const executable = this._app.executable();
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
}

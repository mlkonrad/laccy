// Calls to laccyd, the Laccy app's daemon that stores the history.

import Gio from 'gi://Gio';

export const DAEMON_BUS_NAME = 'io.github.mlkonrad.Laccy.Daemon';
export const DAEMON_OBJECT_PATH = '/io/github/mlkonrad/Laccy/Daemon';
export const DAEMON_INTERFACE = 'io.github.mlkonrad.Laccy.Daemon1';
export const HISTORY_CHANGED_SIGNAL = 'HistoryChanged';

// Calls `method` and only logs a failure. `what` completes "could not ...".
export function callDaemon(method, parameters, what = 'record clipboard item') {
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

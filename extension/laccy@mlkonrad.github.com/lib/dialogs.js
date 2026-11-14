// The extension's modal dialogs.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GObject from 'gi://GObject';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';

const INSTALL_URL = 'https://github.com/mlkonrad/laccy#build-and-install';

export const ClearHistoryDialog = GObject.registerClass(
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
export const MissingAppDialog = GObject.registerClass(
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

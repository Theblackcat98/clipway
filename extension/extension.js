import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Pango from 'gi://Pango';
import Shell from 'gi://Shell';
import St from 'gi://St';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const DAEMON_NAME = 'io.clipway.ClipboardManager';
const DAEMON_PATH = '/io/clipway/ClipboardManager';
const DAEMON_IFACE = 'io.clipway.ClipboardManager1';
const EXTENSION_NAME = 'io.clipway.Extension';
const EXTENSION_PATH = '/io/clipway/Extension';
const APP_NAME = 'io.clipway.Clipway';
const APP_PATH = '/io/clipway/Clipway';
const APP_ACTIONS_IFACE = 'org.gtk.Actions';

const MIME = {
    text: ['text/plain;charset=utf-8', 'text/plain', 'UTF8_STRING', 'STRING'],
    image: ['image/png', 'image/jpeg'],
    files: ['x-special/gnome-copied-files'],
    sensitive: ['x-kde-passwordManagerHint'],
};

const KIND_ICONS = ['format-text-symbolic', 'image-x-generic-symbolic', 'folder-symbolic'];
const RECENT_LIMIT = 10;
const PRIMARY_DEBOUNCE_MS = 500;
const READ_TIMEOUT_MS = 2000;

export default class ClipwayExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._clipboard = St.Clipboard.get_default();
        this._global = Shell.Global.get();
        this._selection = this._global.get_display().get_selection();
        this._cachedRecent = null;

        this._exportClipboardApi();

        this._ownerChangedId = this._selection.connect('owner-changed', (_selection, type) => {
            this._onOwnerChanged(type);
        });

        this._hotkeyId = Main.wm.addKeybinding(
            'clipway-popup',
            this._settings,
            'popup-keybinding',
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.ALL,
            () => this._activateAppAction('popup'),
        );

        this._buildIndicator();
    }

    disable() {
        if (this._hotkeyId) {
            Main.wm.removeKeybinding(this._hotkeyId);
            this._hotkeyId = 0;
        }
        if (this._ownerChangedId) {
            this._selection.disconnect(this._ownerChangedId);
            this._ownerChangedId = 0;
        }
        if (this._primaryDebounceId) {
            GLib.Source.remove(this._primaryDebounceId);
            this._primaryDebounceId = 0;
        }
        this._destroyIndicator();
        if (this._exported) {
            this._exported.unexport();
            this._exported = null;
        }
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        this._settings = null;
        this._clipboard = null;
        this._global = null;
        this._selection = null;
    }

    _exportClipboardApi() {
        const file = Gio.File.new_for_path(`${this.path}/io.clipway.Extension1.xml`);
        const [ok, contents] = file.load_contents(null);
        if (!ok) {
            throw new Error('clipway: cannot read io.clipway.Extension1.xml');
        }
        const xml = new TextDecoder().decode(contents);
        this._exported = Gio.DBusExportedObject.wrapJSObject(xml, this);
        this._exported.export(Gio.DBus.session, EXTENSION_PATH);
        this._nameId = Gio.bus_own_name_on_connection(
            Gio.DBus.session,
            EXTENSION_NAME,
            Gio.BusNameOwnerFlags.REPLACE,
            null,
            null,
        );
    }

    SetClipboard(mime, data) {
        const bytes = new GLib.Bytes(data);
        this._clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, bytes);
        if (this._settings.get_boolean('sync-primary')) {
            this._clipboard.set_content(St.ClipboardType.PRIMARY, mime, bytes);
        }
    }

    setClipboard(mime, data) {
        this.SetClipboard(mime, data);
    }

    _onOwnerChanged(type) {
        if (!this._settings || this._settings.get_boolean('incognito')) {
            return;
        }

        const sourceApp = this._focusedAppClass();
        if (sourceApp && this._isExcluded(sourceApp)) {
            return;
        }

        if (Meta.SelectionType && type === Meta.SelectionType.SELECTION_PRIMARY) {
            if (this._settings.get_boolean('sync-primary')) {
                this._schedulePrimaryCapture(sourceApp);
            }
            return;
        }

        this._capture(St.ClipboardType.CLIPBOARD, sourceApp);
    }

    _schedulePrimaryCapture(sourceApp) {
        if (this._primaryDebounceId) {
            GLib.Source.remove(this._primaryDebounceId);
        }
        this._primaryDebounceId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            PRIMARY_DEBOUNCE_MS,
            () => {
                this._primaryDebounceId = 0;
                this._capture(St.ClipboardType.PRIMARY, sourceApp);
                return GLib.SOURCE_REMOVE;
            },
        );
    }

    _capture(clipboardType, sourceApp) {
        const mimes = this._clipboard.get_mimetypes(clipboardType) ?? [];
        if (mimes.includes(MIME.sensitive[0])) {
            return;
        }
        const mime = this._pickMime(mimes);
        if (!mime) {
            return;
        }
        this._readContent(clipboardType, mime).then(data => {
            if (!data || data.length === 0) {
                return;
            }
            this._callDaemon(
                'AddEntry',
                new GLib.Variant('(says)', [mime, data, sourceApp ?? '']),
            );
        }).catch(() => {});
    }

    _pickMime(mimes) {
        for (const group of [MIME.text, MIME.image, MIME.files]) {
            for (const candidate of group) {
                if (mimes.includes(candidate)) {
                    return candidate;
                }
            }
        }
        return null;
    }

    _readContent(clipboardType, mime) {
        return new Promise(resolve => {
            let done = false;
            let timeoutId = 0;
            const finish = data => {
                if (done) {
                    return;
                }
                done = true;
                if (timeoutId) {
                    GLib.Source.remove(timeoutId);
                }
                resolve(data);
            };
            const callback = (_clipboard, bytes) => {
                if (!bytes) {
                    finish(null);
                    return;
                }
                finish(bytes instanceof GLib.Bytes ? bytes.get_data() : bytes);
            };
            timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, READ_TIMEOUT_MS, () => {
                finish(null);
                return GLib.SOURCE_REMOVE;
            });
            const withCancellable = this._getContentTakesCancellable();
            try {
                if (withCancellable) {
                    this._clipboard.get_content(clipboardType, mime, null, callback);
                } else {
                    this._clipboard.get_content(clipboardType, mime, callback);
                }
            } catch (error) {
                finish(null);
            }
        });
    }

    _getContentTakesCancellable() {
        const proto = St.Clipboard.prototype;
        if (!proto || typeof proto.get_content !== 'function') {
            return false;
        }
        return proto.get_content.length >= 4;
    }

    _focusedAppClass() {
        const window = this._global.get_display().focus_window;
        if (!window) {
            return '';
        }
        return window.get_wm_class() ?? '';
    }

    _isExcluded(wmClass) {
        const needle = wmClass.toLowerCase();
        return this._settings.get_strv('excluded-apps').some(app => {
            const candidate = app.trim().toLowerCase();
            return candidate !== '' && candidate === needle;
        });
    }

    _callDaemon(method, parameters) {
        Gio.DBus.session.call(
            DAEMON_NAME,
            DAEMON_PATH,
            DAEMON_IFACE,
            method,
            parameters,
            null,
            Gio.DBusCallFlags.NO_REPLY_EXPECTED,
            1000,
            null,
            (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch (error) {
                    if (!error.matches?.(Gio.io_error_quark(), Gio.IOErrorEnum.CANCELLED)) {
                        logError(error, `clipway: ${method} failed`);
                    }
                }
            },
        );
    }

    _activateAppAction(action) {
        Gio.DBus.session.call(
            APP_NAME,
            APP_PATH,
            APP_ACTIONS_IFACE,
            'Activate',
            new GLib.Variant('(sava{sv})', [
                action,
                new GLib.Variant('av', []),
                {},
            ]),
            null,
            Gio.DBusCallFlags.NONE,
            5000,
            null,
            (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch (error) {
                    logError(error, `clipway: could not activate ${action}`);
                }
            },
        );
    }

    _pasteEntry(objectPath) {
        Gio.DBus.session.call(
            DAEMON_NAME,
            DAEMON_PATH,
            DAEMON_IFACE,
            'PasteEntry',
            new GLib.Variant('(o)', [objectPath]),
            new GLib.VariantType('(b)'),
            Gio.DBusCallFlags.NONE,
            5000,
            null,
            (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch (error) {
                    logError(error, 'clipway: paste failed');
                }
            },
        );
    }

    _loadRecent(callback) {
        Gio.DBus.session.call(
            DAEMON_NAME,
            DAEMON_PATH,
            DAEMON_IFACE,
            'GetRecent',
            new GLib.Variant('(u)', [RECENT_LIMIT]),
            new GLib.VariantType('(a(oa{sv}))'),
            Gio.DBusCallFlags.NONE,
            5000,
            null,
            (connection, result) => {
                try {
                    const reply = connection.call_finish(result).deepUnpack();
                    callback(reply[0].map(([path, fields]) => ({
                        path,
                        kind: Number(fields.kind),
                        preview: fields.preview ?? '',
                        pinned: Boolean(fields.pinned),
                    })));
                } catch (error) {
                    callback([]);
                }
            },
        );
    }

    _buildIndicator() {
        this._indicator = new St.Button({
            child: new St.Icon({
                icon_name: 'edit-paste-symbolic',
                style_class: 'system-status-icon',
            }),
            y_align: 0.5,
        });
        this._menu = new PopupMenu.PopupMenu(this._indicator, 0.5, 'clipway-menu');
        Main.panel.addToStatusArea(this.uuid, this._indicator, 0, 'right');
        this._indicator.connect('button-press-event', () => {
            return Clutter.EVENT_PROPAGATE;
        });
        this._menu.connect('open-state-changed', (_menu, open) => {
            if (open) {
                this._loadRecent(entries => {
                    if (!this._menu) {
                        return;
                    }
                    this._cachedRecent = entries;
                    this._rebuildMenu();
                });
            }
        });
        this._historyChangedId = Gio.DBus.session.signal_subscribe(
            null,
            DAEMON_IFACE,
            'HistoryChanged',
            DAEMON_PATH,
            null,
            Gio.DBusSignalFlags.NONE,
            () => {
                if (this._menu && this._menu.isOpen) {
                    this._loadRecent(entries => {
                        this._cachedRecent = entries;
                        this._rebuildMenu();
                    });
                }
            },
        );
    }

    _rebuildMenu() {
        if (!this._menu) {
            return;
        }
        this._menu.removeAll();

        const open = new PopupMenu.PopupMenuItem('Open Clipway');
        open.connect('activate', () => this._activateAppAction('popup'));
        this._menu.addMenuItem(open);

        const incognito = new PopupMenu.PopupMenuItem('Incognito Mode');
        if (this._settings.get_boolean('incognito')) {
            incognito.setOrnament(PopupMenu.Ornament.CHECK);
        }
        incognito.connect('activate', () => {
            const current = this._settings.get_boolean('incognito');
            this._settings.set_boolean('incognito', !current);
            this._rebuildMenu();
        });
        this._menu.addMenuItem(incognito);

        this._menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const recent = this._cachedRecent;
        if (!recent || recent.length === 0) {
            const empty = new PopupMenu.PopupMenuItem('Clipboard history is empty');
            empty.reactive = false;
            this._menu.addMenuItem(empty);
        } else {
            for (const entry of recent) {
                const item = new PopupMenu.PopupBaseMenuItem({ activate: false });
                const icon = new St.Icon({
                    icon_name: KIND_ICONS[entry.kind] ?? KIND_ICONS[0],
                    style_class: 'popup-menu-icon',
                });
                const label = new St.Label({ text: entry.preview, y_align: 0.5 });
                label.clutter_text.ellipsize = Pango.EllipsizeMode.END;
                item.add_child(icon);
                item.add_child(label);
                if (entry.pinned) {
                    item.add_child(new St.Icon({
                        icon_name: 'starred-symbolic',
                        style_class: 'popup-menu-icon',
                    }));
                }
                const path = entry.path;
                item.connect('activate', () => this._pasteEntry(path));
                this._menu.addMenuItem(item);
            }
        }

        this._menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const clear = new PopupMenu.PopupMenuItem('Clear History');
        clear.connect('activate', () => {
            this._callDaemon('ClearHistory', null);
            this._cachedRecent = [];
            this._rebuildMenu();
        });
        this._menu.addMenuItem(clear);

        const preferences = new PopupMenu.PopupMenuItem('Settings');
        preferences.connect('activate', () => this._activateAppAction('preferences'));
        this._menu.addMenuItem(preferences);
    }

    _destroyIndicator() {
        if (this._historyChangedId) {
            Gio.DBus.session.signal_unsubscribe(this._historyChangedId);
            this._historyChangedId = 0;
        }
        if (this._menu) {
            this._menu.destroy();
            this._menu = null;
        }
        if (this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
        this._cachedRecent = null;
    }
}

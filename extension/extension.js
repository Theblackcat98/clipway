// Clipway capture extension.
//
// Watches the compositor's selection, forwards new clipboard content to the
// Clipway app over D-Bus, restores entries the app asks for, owns the popup
// shortcut and shows a small panel menu. Storage, search and all other
// policy live in the app (clipway-daemon).
//
// Settings are read from the app's own schema (io.clipway.Clipway). The
// extension ships no schema: without the app it has nothing to do.

import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const APP_ID = 'io.clipway.Clipway';
const APP_PATH = '/io/clipway/Clipway';
const APP_SCHEMA = 'io.clipway.Clipway';
const SHORTCUT_KEY = 'popup-keybinding';

const DAEMON_NAME = 'io.clipway.ClipboardManager';
const DAEMON_PATH = '/io/clipway/ClipboardManager';
const DAEMON_IFACE = 'io.clipway.ClipboardManager1';

const EXTENSION_NAME = 'io.clipway.Extension';
const EXTENSION_PATH = '/io/clipway/Extension';
const EXTENSION_IFACE_XML = `
<node>
  <interface name="io.clipway.Extension1">
    <method name="SetClipboard">
      <arg type="s" name="mime" direction="in"/>
      <arg type="ay" name="data" direction="in"/>
    </method>
  </interface>
</node>`;

// Checked in order. Text comes first, so a Nautilus copy (which also offers
// the paths as text) is recorded as text. `x-special/gnome-copied-files` is
// never read: restoring a stale "cut" marker would move files on paste.
const CAPTURE_MIMES = [
    'text/plain;charset=utf-8',
    'text/plain',
    'UTF8_STRING',
    'STRING',
    'image/png',
    'image/jpeg',
    'text/uri-list',
];

// The only types the app stores entries as, and so the only ones restored.
const RESTORABLE_MIMES = [
    'text/plain;charset=utf-8',
    'image/png',
    'image/jpeg',
    'text/uri-list',
];

// Password managers (KeePassXC and others) offer this type to ask clipboard
// managers not to record the content.
const SECRET_HINT = 'x-kde-passwordManagerHint';

const FILE_LIST_MAX_BYTES = 64 * 1024;
const PRIMARY_DEBOUNCE_MS = 500;
const MENU_RECENT = 10;

const {SELECTION_CLIPBOARD, SELECTION_PRIMARY} = Meta.SelectionType;

function dbusCall(destination, path, iface, method, parameters, replyType) {
    return new Promise((resolve, reject) => {
        Gio.DBus.session.call(
            destination, path, iface, method, parameters,
            replyType ? new GLib.VariantType(replyType) : null,
            Gio.DBusCallFlags.NONE, -1, null,
            (connection, result) => {
                try {
                    resolve(connection.call_finish(result));
                } catch (error) {
                    reject(error);
                }
            });
    });
}

async function peerPid(uniqueName) {
    const reply = await dbusCall(
        'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
        'GetConnectionUnixProcessID', new GLib.Variant('(s)', [uniqueName]), '(u)');
    return reply.deepUnpack()[0];
}

// Where `make install` and distribution packages put the app. A process
// that merely claims the daemon's bus name gets no clipboard data.
function isTrustedDaemon(pid) {
    let exe;
    try {
        exe = GLib.file_read_link(`/proc/${pid}/exe`);
    } catch {
        return false;
    }
    exe = exe.replace(/ \(deleted\)$/, '');
    return [
        '/usr/bin/clipway-daemon',
        '/usr/local/bin/clipway-daemon',
        '/usr/libexec/clipway-daemon',
        GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'clipway-daemon']),
    ].includes(exe);
}

function normalizeAppId(id) {
    return id.trim().toLowerCase().replace(/\.desktop$/, '');
}

const ClipwayIndicator = GObject.registerClass(
class ClipwayIndicator extends PanelMenu.Button {
    _init(extension) {
        super._init(0.5, extension.metadata.name, false);
        this._extension = extension;
        this.add_child(new St.Icon({
            icon_name: 'edit-paste-symbolic',
            style_class: 'system-status-icon',
        }));
        this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._extension.reloadMenu();
        });
    }
});

export default class ClipwayExtension extends Extension {
    enable() {
        try {
            this._enable();
        } catch (error) {
            // Undo whatever was set up before the failure, so a broken
            // extension never keeps capturing in the background.
            this.disable();
            throw error;
        }
    }

    _enable() {
        this._logger = this.getLogger();
        this._settings = this._lookupAppSettings();
        this._clipboard = St.Clipboard.get_default();
        this._selection = global.display.get_selection();
        this._cancellable = new Gio.Cancellable();
        this._generation = {[SELECTION_CLIPBOARD]: 0, [SELECTION_PRIMARY]: 0};
        this._selfWrite = false;
        this._daemonOwner = null;
        this._daemonPid = 0;
        this._daemonCheck = 0;
        this._recent = [];

        this._dbusImpl = Gio.DBusExportedObject.wrapJSObject(EXTENSION_IFACE_XML, {
            SetClipboardAsync: (params, invocation) => this._onSetClipboard(params, invocation),
        });
        this._dbusImpl.export(Gio.DBus.session, EXTENSION_PATH);
        this._nameId = Gio.bus_own_name_on_connection(Gio.DBus.session,
            EXTENSION_NAME, Gio.BusNameOwnerFlags.NONE, null, null);

        this._daemonWatchId = Gio.bus_watch_name_on_connection(Gio.DBus.session,
            DAEMON_NAME, Gio.BusNameWatcherFlags.AUTO_START,
            (_connection, _name, owner) => this._onDaemonAppeared(owner),
            () => {
                this._daemonOwner = null;
                this._daemonCheck++;
            });

        this._historyChangedId = Gio.DBus.session.signal_subscribe(null,
            DAEMON_IFACE, 'HistoryChanged', DAEMON_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_connection, sender) => {
                if (sender === this._daemonOwner && this._indicator?.menu.isOpen)
                    this.reloadMenu();
            });

        this._ownerChangedId = this._selection.connect('owner-changed',
            (_selection, type, source) => this._onOwnerChanged(type, source));

        this._indicator = new ClipwayIndicator(this);
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        if (this._settings) {
            this._bindShortcut();
            this._shortcutChangedId = this._settings.connect(`changed::${SHORTCUT_KEY}`,
                () => this._bindShortcut());
        }
    }

    disable() {
        // Must stay synchronous. Runs on logout, when the extension is
        // switched off or updated, and on every screen lock (only the
        // default 'user' session mode is used).
        this._cancellable?.cancel();
        this._cancellable = null;

        if (this._primaryTimeoutId) {
            GLib.Source.remove(this._primaryTimeoutId);
            this._primaryTimeoutId = 0;
        }
        this._stopFocusWatch();
        if (this._shortcutChangedId) {
            this._settings.disconnect(this._shortcutChangedId);
            this._shortcutChangedId = 0;
        }
        Main.wm.removeKeybinding(SHORTCUT_KEY);

        if (this._ownerChangedId) {
            this._selection.disconnect(this._ownerChangedId);
            this._ownerChangedId = 0;
        }
        if (this._historyChangedId) {
            Gio.DBus.session.signal_unsubscribe(this._historyChangedId);
            this._historyChangedId = 0;
        }
        if (this._daemonWatchId) {
            Gio.bus_unwatch_name(this._daemonWatchId);
            this._daemonWatchId = 0;
        }
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        this._dbusImpl?.unexport();
        this._dbusImpl = null;

        this._indicator?.destroy();
        this._indicator = null;

        this._settings = null;
        this._clipboard = null;
        this._selection = null;
        this._daemonOwner = null;
        this._daemonPid = 0;
        this._daemonCheck++; // ignore daemon checks still in flight
        this._recent = [];
        this._logger = null;
    }

    _lookupAppSettings() {
        const schema = Gio.SettingsSchemaSource.get_default()?.lookup(APP_SCHEMA, true);
        if (!schema) {
            this._logger.warn(`schema ${APP_SCHEMA} not found; install the Clipway app`);
            return null;
        }
        return new Gio.Settings({settings_schema: schema});
    }

    _bindShortcut() {
        Main.wm.removeKeybinding(SHORTCUT_KEY);
        Main.wm.addKeybinding(SHORTCUT_KEY, this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
            () => this.activateAppAction('popup'));
    }

    // Capture

    _onOwnerChanged(type, source) {
        if (type !== SELECTION_CLIPBOARD && type !== SELECTION_PRIMARY)
            return; // drag-and-drop
        const generation = ++this._generation[type];
        if (this._selfWrite || !source || !this._settings)
            return;
        if (this._settings.get_boolean('incognito'))
            return;
        if (type === SELECTION_PRIMARY && !this._settings.get_boolean('sync-primary'))
            return;

        const sourceApp = this._focusedAppIds();
        if (this._isExcluded(sourceApp))
            return;

        if (type === SELECTION_CLIPBOARD) {
            this._capture(type, generation, sourceApp);
            return;
        }
        // Primary changes on every selection drag; wait until it settles.
        if (this._primaryTimeoutId)
            GLib.Source.remove(this._primaryTimeoutId);
        this._primaryTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
            PRIMARY_DEBOUNCE_MS, () => {
                this._primaryTimeoutId = 0;
                this._capture(type, generation, sourceApp);
                return GLib.SOURCE_REMOVE;
            });
    }

    _capture(type, generation, sourceApp) {
        const offered = this._selection.get_mimetypes(type);
        if (offered.includes(SECRET_HINT))
            return;
        const mime = CAPTURE_MIMES.find(candidate => offered.includes(candidate));
        if (!mime)
            return;

        // Read at most cap + 1 bytes, so oversized content is recognised
        // without ever pulling all of it into the Shell.
        const cap = this._capFor(mime);
        const stream = Gio.MemoryOutputStream.new_resizable();
        this._selection.transfer_async(type, mime, cap + 1, stream, this._cancellable,
            (selection, result) => {
                try {
                    selection.transfer_finish(result);
                } catch (error) {
                    if (!error.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        this._logger?.debug(`reading the clipboard failed: ${error.message}`);
                    return;
                }
                if (!this._cancellable || generation !== this._generation[type])
                    return; // disabled, or a newer copy has already arrived
                stream.close(null);
                const bytes = stream.steal_as_bytes();
                const size = bytes.get_size();
                if (size > 0 && size <= cap)
                    this._sendEntry(mime, bytes, sourceApp);
            });
    }

    _capFor(mime) {
        if (mime === 'text/uri-list')
            return FILE_LIST_MAX_BYTES;
        const key = mime.startsWith('image/') ? 'max-image-bytes' : 'max-text-bytes';
        return this._settings.get_uint(key);
    }

    // App ID (from the .desktop file) first, then the window class. Copies
    // from a terminal (pass, wl-copy) are attributed to the terminal.
    _focusedAppIds() {
        const window = global.display.focus_window;
        if (!window)
            return '';
        const ids = [];
        const appId = Shell.WindowTracker.get_default().get_window_app(window)?.get_id();
        if (appId && !appId.startsWith('window:'))
            ids.push(appId.replace(/\.desktop$/, ''));
        for (const id of [window.get_wm_class(), window.get_wm_class_instance()]) {
            if (id && !ids.includes(id))
                ids.push(id);
        }
        return ids.join('|');
    }

    _isExcluded(sourceApp) {
        if (!sourceApp)
            return false;
        const excluded = this._settings.get_strv('excluded-apps').map(normalizeAppId);
        return sourceApp.split('|').some(id => excluded.includes(normalizeAppId(id)));
    }

    _sendEntry(mime, bytes, sourceApp) {
        if (!this._daemonOwner)
            return; // app not running, or not the real app
        dbusCall(this._daemonOwner, DAEMON_PATH, DAEMON_IFACE, 'AddEntry',
            new GLib.Variant('(says)', [mime, bytes.get_data() ?? new Uint8Array(), sourceApp]),
            null)
            .catch(error => this._logger?.warn(`AddEntry failed: ${error.message}`));
    }

    async _onDaemonAppeared(owner) {
        const check = ++this._daemonCheck;
        this._daemonOwner = null;
        try {
            const pid = await peerPid(owner);
            if (check !== this._daemonCheck)
                return;
            if (!isTrustedDaemon(pid)) {
                this._logger?.warn(`${DAEMON_NAME} is owned by an unexpected program; not sending clipboard data`);
                return;
            }
            this._daemonOwner = owner;
            this._daemonPid = pid;
        } catch (error) {
            this._logger?.warn(`could not verify ${DAEMON_NAME}: ${error.message}`);
        }
    }

    // Restore

    // The app calls this while its popup has focus. Like any Wayland client,
    // only the focused application may set the clipboard: a background
    // process cannot use this method to replace what the user copied.
    async _onSetClipboard([mime, data], invocation) {
        try {
            if (!RESTORABLE_MIMES.includes(mime))
                throw new Error(`unsupported MIME type ${mime}`);
            const pid = await peerPid(invocation.get_sender());
            const focus = global.display.focus_window;
            if (!this._clipboard || !focus || focus.get_pid() !== pid) {
                invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied',
                    'Only the focused application may set the clipboard');
                return;
            }
            this._setClipboard(mime, new GLib.Bytes(data));
            invocation.return_value(null);
        } catch (error) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.Failed', error.message);
        }
    }

    _setClipboard(mime, bytes) {
        // owner-changed fires synchronously inside set_content(); the flag
        // stops the extension from recording its own write.
        this._selfWrite = true;
        try {
            this._clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, bytes);
            if (mime.startsWith('text/plain') && this._settings?.get_boolean('sync-primary'))
                this._clipboard.set_content(St.ClipboardType.PRIMARY, mime, bytes);
        } finally {
            this._selfWrite = false;
        }
    }

    async _pasteFromMenu(path) {
        if (!this._daemonOwner)
            return;
        try {
            const reply = await dbusCall(this._daemonOwner, DAEMON_PATH, DAEMON_IFACE,
                'GetEntry', new GLib.Variant('(o)', [path]), '(say)');
            const [mime, data] = reply.deepUnpack();
            if (this._clipboard && RESTORABLE_MIMES.includes(mime))
                this._setClipboard(mime, new GLib.Bytes(data));
        } catch (error) {
            this._logger?.warn(`could not restore entry: ${error.message}`);
        }
    }

    // App and panel menu

    activateAppAction(action) {
        const platformData = {};
        const token = this._activationToken();
        if (token) {
            // Lets the app's window take focus despite focus-stealing
            // prevention.
            platformData['activation-token'] = new GLib.Variant('s', token);
            platformData['desktop-startup-id'] = new GLib.Variant('s', token);
        }
        this._focusAppWindow();
        dbusCall(APP_ID, APP_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [action, [], platformData]), null)
            .catch(error => this._logger?.warn(`could not open Clipway (${action}): ${error.message}`));
    }

    // The user asked for the app's window, so give it focus: raise an
    // existing one, or the next one it maps within a few seconds. This does
    // not depend on the activation token being honoured.
    _focusAppWindow() {
        this._stopFocusWatch();
        const isAppWindow = window => this._daemonPid && window.get_pid() === this._daemonPid;
        const existing = global.display.list_all_windows().find(isAppWindow);
        if (existing && !existing.minimized && existing.showing_on_its_workspace()) {
            Main.activateWindow(existing);
            return;
        }
        this._windowCreatedId = global.display.connect('window-created', (_display, window) => {
            if (!isAppWindow(window))
                return;
            this._stopFocusWatch();
            // Focus once the window is actually on screen.
            this._pendingShown = [window, window.connect('shown', () => {
                this._stopFocusWatch();
                Main.activateWindow(window);
            })];
        });
        this._focusTimeoutId = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 5, () => {
            this._focusTimeoutId = 0;
            this._stopFocusWatch();
            return GLib.SOURCE_REMOVE;
        });
    }

    _stopFocusWatch() {
        if (this._pendingShown) {
            const [window, id] = this._pendingShown;
            window.disconnect(id);
            this._pendingShown = null;
        }
        if (this._windowCreatedId) {
            global.display.disconnect(this._windowCreatedId);
            this._windowCreatedId = 0;
        }
        if (this._focusTimeoutId) {
            GLib.Source.remove(this._focusTimeoutId);
            this._focusTimeoutId = 0;
        }
    }

    _activationToken() {
        const app = Shell.AppSystem.get_default().lookup_app(`${APP_ID}.desktop`);
        if (!app)
            return null;
        try {
            const context = global.create_app_launch_context(global.get_current_time(), -1);
            return context.get_startup_notify_id(app.get_app_info(), []);
        } catch (error) {
            this._logger?.debug(`no activation token: ${error.message}`);
            return null;
        }
    }

    async reloadMenu() {
        this._rebuildMenu();
        if (!this._daemonOwner)
            return;
        try {
            const reply = await dbusCall(this._daemonOwner, DAEMON_PATH, DAEMON_IFACE,
                'GetRecent', new GLib.Variant('(u)', [MENU_RECENT]), '(a(oa{sv}))');
            this._recent = reply.deepUnpack()[0].map(([path, fields]) => ({
                path,
                kind: fields.kind?.deepUnpack() ?? 0,
                preview: fields.preview?.deepUnpack() ?? '',
                pinned: fields.pinned?.deepUnpack() ?? false,
            }));
        } catch (error) {
            this._logger?.warn(`could not load recent entries: ${error.message}`);
            this._recent = [];
        }
        this._rebuildMenu();
    }

    _rebuildMenu() {
        const menu = this._indicator?.menu;
        if (!menu)
            return;
        menu.removeAll();

        if (!this._settings) {
            const missing = new PopupMenu.PopupMenuItem('Clipway app is not installed');
            missing.setSensitive(false);
            menu.addMenuItem(missing);
            return;
        }

        const open = new PopupMenu.PopupMenuItem('Open Clipway');
        open.connect('activate', () => this.activateAppAction('popup'));
        menu.addMenuItem(open);

        const incognito = new PopupMenu.PopupSwitchMenuItem('Incognito Mode',
            this._settings.get_boolean('incognito'));
        incognito.connect('toggled', (_item, state) =>
            this._settings.set_boolean('incognito', state));
        menu.addMenuItem(incognito);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        if (!this._daemonOwner) {
            const down = new PopupMenu.PopupMenuItem('Clipway is not running');
            down.setSensitive(false);
            menu.addMenuItem(down);
        } else if (this._recent.length === 0) {
            const empty = new PopupMenu.PopupMenuItem('Clipboard history is empty');
            empty.setSensitive(false);
            menu.addMenuItem(empty);
        } else {
            const icons = ['format-text-symbolic', 'image-x-generic-symbolic', 'folder-symbolic'];
            for (const entry of this._recent) {
                const item = new PopupMenu.PopupImageMenuItem(entry.preview,
                    icons[entry.kind] ?? icons[0]);
                item.label.add_style_class_name('clipway-menu-label');
                if (entry.pinned) {
                    item.add_child(new St.Icon({
                        icon_name: 'starred-symbolic',
                        style_class: 'popup-menu-icon',
                    }));
                }
                item.connect('activate', () => this._pasteFromMenu(entry.path));
                menu.addMenuItem(item);
            }
        }

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const clear = new PopupMenu.PopupMenuItem('Clear History…');
        clear.connect('activate', () => this.activateAppAction('clear-history'));
        menu.addMenuItem(clear);

        const preferences = new PopupMenu.PopupMenuItem('Settings');
        preferences.connect('activate', () => this.activateAppAction('preferences'));
        menu.addMenuItem(preferences);
    }
}

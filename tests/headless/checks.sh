UUID=clipway@clipway.dev
EXT="Main.extensionManager.lookup('$UUID').stateObj"
PRE="const {GLib, Meta, St} = imports.gi;"
# Simulates another application copying: a new selection owner with one MIME type.
copy() { eval_js "$PRE Meta.SelectionType; global.display.get_selection().set_owner(Meta.SelectionType.SELECTION_CLIPBOARD, Meta.SelectionSourceMemory.new('$1', new GLib.Bytes(new TextEncoder().encode('$2')))); 'copied'" >/dev/null; sleep 0.8; }
copy_file() { eval_js "$PRE const [, data] = GLib.file_get_contents('$2'); global.display.get_selection().set_owner(Meta.SelectionType.SELECTION_CLIPBOARD, Meta.SelectionSourceMemory.new('$1', new GLib.Bytes(data))); 'copied'" >/dev/null; sleep 1.2; }
clip() { eval_js "$PRE global._clip = null; St.Clipboard.get_default().get_text(St.ClipboardType.CLIPBOARD, (c, t) => { global._clip = t; }); 'ok'" >/dev/null; sleep 0.5; eval_js "String(global._clip).slice(0, 60)"; }
recent() { eval_js "$EXT.reloadMenu(); 'ok'" >/dev/null; sleep 0.7; eval_js "JSON.stringify($EXT._recent.map(e => [e.kind, e.preview, e.pinned]))"; }

eval_js "global._kbd = (global.stage.context?.get_backend() ?? imports.gi.Clutter.get_default_backend()).get_default_seat().create_virtual_device(imports.gi.Clutter.InputDeviceType.KEYBOARD_DEVICE); 'kbd'" >/dev/null
echo "== daemon verified by extension (expect a unique name)"
eval_js "String($EXT._daemonOwner)"
echo "== text copy (X11-style UTF8_STRING) is captured and normalised"
copy UTF8_STRING "hello from x11"
recent
echo "== same text as text/plain;charset=utf-8 is deduplicated; then a second text"
copy "text/plain;charset=utf-8" "hello from x11"
copy "text/plain;charset=utf-8" "second copy"
recent
echo "== image"
copy_file image/png $TEST_DIR/test.png
recent
echo "== Nautilus cut marker is ignored (never captured)"
copy x-special/gnome-copied-files 'cut\nfile:///tmp/a.txt'
recent
echo "== incognito: nothing recorded"
gsettings set io.clipway.Clipway incognito true; sleep 0.3
copy "text/plain;charset=utf-8" "copied while incognito"
gsettings set io.clipway.Clipway incognito false
recent
echo "== oversized text (cap 1024 bytes) not recorded"
gsettings set io.clipway.Clipway max-text-bytes 1024; sleep 0.3
copy "text/plain;charset=utf-8" "$(head -c 5000 /dev/zero | tr '\0' 'x')"
gsettings reset io.clipway.Clipway max-text-bytes
recent
echo "== D-Bus from a non-Shell process is refused"
gdbus call --session --dest io.clipway.ClipboardManager --object-path /io/clipway/ClipboardManager --method io.clipway.ClipboardManager1.GetRecent 10 2>&1 | head -1
gdbus call --session --dest io.clipway.ClipboardManager --object-path /io/clipway/ClipboardManager --method io.clipway.ClipboardManager1.AddEntry text/plain "[104,105]" "x" 2>&1 | head -1
gdbus call --session --dest io.clipway.Extension --object-path /io/clipway/Extension --method io.clipway.Extension1.SetClipboard "text/plain;charset=utf-8" "[104,105]" 2>&1 | head -1
gdbus call --session --dest io.clipway.Extension --object-path /io/clipway/Extension --method io.clipway.Extension1.SetClipboard "x-special/gnome-copied-files" "[104,105]" 2>&1 | head -1
echo "== panel menu restores the oldest text entry, without re-capturing it"
eval_js "const r = $EXT._recent.filter(e => e.kind === 0); $EXT._pasteFromMenu(r[r.length-1].path); 'ok'" >/dev/null; sleep 1
echo "clipboard now: $(clip)"
recent
echo "== shortcut binding"
gsettings set io.clipway.Clipway popup-keybinding "['<Super><Shift>v']"; sleep 0.5
eval_js "String(Main.wm._allowedKeybindings['popup-keybinding'])"
echo "== popup via app action; Down then Enter pastes the second row through SetClipboard"
eval_js "$EXT.activateAppAction('popup'); 'ok'" >/dev/null; sleep 3
eval_js "JSON.stringify(global.get_window_actors().map(a => [a.meta_window.get_title(), a.meta_window.has_focus()]))"
eval_js "const C = imports.gi.Clutter; const d = global._kbd; for (const k of [C.KEY_Down, C.KEY_Return]) { d.notify_keyval(C.get_current_event_time()*1000, k, C.KeyState.PRESSED); d.notify_keyval(C.get_current_event_time()*1000, k, C.KeyState.RELEASED); } 'sent'" >/dev/null
sleep 2
echo "clipboard now: $(clip)"
eval_js "JSON.stringify(global.get_window_actors().map(a => [a.meta_window.get_title(), a.visible]))"
recent
echo "== disable: capture stops and the D-Bus name is released"
eval_js "Main.extensionManager.disableExtension('$UUID')" >/dev/null; sleep 1
gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner io.clipway.Extension
copy "text/plain;charset=utf-8" "copied while disabled"
eval_js "Main.extensionManager.enableExtension('$UUID')" >/dev/null; sleep 3
eval_js "String(Main.extensionManager.lookup('$UUID').state)"
recent
echo "== Ctrl+P in the popup pins the highlighted (top) entry"
eval_js "$EXT.activateAppAction('popup'); 'ok'" >/dev/null; sleep 2
eval_js "const C = imports.gi.Clutter; const d = global._kbd; const t = () => C.get_current_event_time()*1000; d.notify_keyval(t(), C.KEY_Control_L, C.KeyState.PRESSED); d.notify_keyval(t(), C.KEY_p, C.KeyState.PRESSED); d.notify_keyval(t(), C.KEY_p, C.KeyState.RELEASED); d.notify_keyval(t(), C.KEY_Control_L, C.KeyState.RELEASED); d.notify_keyval(t(), C.KEY_Escape, C.KeyState.PRESSED); d.notify_keyval(t(), C.KEY_Escape, C.KeyState.RELEASED); 'sent'" >/dev/null; sleep 1.5
recent
echo "== daemon restart in the same session keeps history (clear-on-logout marker)"
pkill -x clipway-daemon; sleep 1
$HOME/.local/bin/clipway-daemon --daemon >> /tmp/daemon.log 2>&1 &
sleep 3
recent
echo "== a daemon started from an untrusted path gets no clipboard data"
pkill -x clipway-daemon; sleep 1
mkdir -p /tmp/fake && cp $HOME/.local/bin/clipway-daemon /tmp/fake/clipway-daemon
CLIPWAY_DB_PATH=/tmp/fake/history.db /tmp/fake/clipway-daemon --daemon > /tmp/fake.log 2>&1 &
sleep 3
copy "text/plain;charset=utf-8" "must not reach the fake daemon"
eval_js "String($EXT._daemonOwner)"
grep -c "unexpected program" /tmp/shell.log
pkill -x clipway-daemon
echo "== shell log (clipway / JS errors)"
grep -iE "clipway|JS ERROR|TypeError|JS WARNING" /tmp/shell.log | head -30
echo "== daemon log"
grep -v "libEGL\|MESA\|^$\|Broken pipe" /tmp/daemon.log | head -30

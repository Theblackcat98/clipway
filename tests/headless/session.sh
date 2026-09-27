#!/bin/bash
# Runs inside dbus-run-session: starts the shell and the daemon, then checks.
gsettings set org.gnome.shell disable-extension-version-validation true
gsettings reset org.gnome.shell disabled-extensions
gsettings set org.gnome.shell enabled-extensions "['clipway-test@local', 'clipway@clipway.dev']"
gnome-shell --headless --wayland --no-x11 --virtual-monitor 1280x800 > /tmp/shell.log 2>&1 &
SHELL_PID=$!
for _ in $(seq 1 60); do [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && break; sleep 0.5; done
sleep 4
export WAYLAND_DISPLAY=wayland-0
eval_js() { timeout 10 gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Eval "$1"; }
echo "== extension error (expect empty)"
eval_js "Main.extensionManager.lookup('clipway@clipway.dev').error ?? ''"
echo "== panel indicator present"
eval_js "String(Main.panel.statusArea['clipway@clipway.dev']?.constructor?.name)"
# Debug builds accept a key from the environment; release builds need the keyring.
export CLIPWAY_DB_KEY=00112233445566778899aabbccddeeff00112233445566778899aabbccddee
export CLIPWAY_DB_PATH=/tmp/clipway-headless/history.db
rm -rf /tmp/clipway-headless
"$HOME/.local/bin/clipway-daemon" --daemon > /tmp/daemon.log 2>&1 &
sleep 3
. "$TEST_DIR/checks.sh"
kill $SHELL_PID 2>/dev/null; sleep 1

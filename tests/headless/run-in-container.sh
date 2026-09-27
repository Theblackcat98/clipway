#!/bin/bash
# End-to-end test of the extension and daemon inside a headless GNOME Shell.
#
# Run it in a disposable container or VM, never on your desktop: it installs
# a *debug* clipway-daemon to ~/.local/bin (the path the extension trusts),
# rewrites GNOME Shell settings, and may start a system bus.
#
# Needs: gnome-shell, dbus, gsettings, gdbus, a debug build (cargo build).
set -euo pipefail
TEST_DIR=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$TEST_DIR/../.." && pwd)
EXT_ROOT=$HOME/.local/share/gnome-shell/extensions
UUID=clipway@clipway.dev

(cd "$REPO" && cargo build)
install -Dm755 "$REPO/target/debug/clipway-daemon" "$HOME/.local/bin/clipway-daemon"
install -Dm644 "$REPO/data/io.clipway.Clipway.gschema.xml" "$HOME/.local/share/glib-2.0/schemas/io.clipway.Clipway.gschema.xml"
glib-compile-schemas "$HOME/.local/share/glib-2.0/schemas"
install -Dm644 "$REPO/data/io.clipway.Clipway.desktop" "$HOME/.local/share/applications/io.clipway.Clipway.desktop"

rm -rf "$EXT_ROOT/$UUID" "$EXT_ROOT/clipway-test@local"
mkdir -p "$EXT_ROOT/$UUID"
cp "$REPO"/extension/{metadata.json,extension.js,stylesheet.css} "$EXT_ROOT/$UUID/"
shell_major=$(gnome-shell --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')
if [ "$shell_major" -lt 48 ]; then
    # Extension.getLogger() exists from GNOME 48; shim it for older test shells.
    sed -i 's/this._logger = this.getLogger();/this._logger = this.getLogger?.() ?? console;/' "$EXT_ROOT/$UUID/extension.js"
fi
cp -r "$TEST_DIR/helper-extension" "$EXT_ROOT/clipway-test@local"
sed -i "s/\"46\"/\"$shell_major\"/" "$EXT_ROOT/clipway-test@local/metadata.json"

# GNOME Shell needs a system bus. Without logind it must not believe systemd
# manages seats, or it waits for org.freedesktop.login1.
if [ ! -S /run/dbus/system_bus_socket ]; then
    mkdir -p /run/dbus && dbus-daemon --system --fork
fi
if [ -d /run/systemd/seats ] && ! busctl --system status org.freedesktop.login1 >/dev/null 2>&1; then
    echo "note: hiding /run/systemd/seats (no logind in this container)"
    mv /run/systemd/seats /run/systemd/seats.hidden
fi

export TEST_DIR
export XDG_RUNTIME_DIR=/tmp/clipway-headless-rt
rm -rf "$XDG_RUNTIME_DIR"; mkdir -m 700 -p "$XDG_RUNTIME_DIR"
exec dbus-run-session -- "$TEST_DIR/session.sh"

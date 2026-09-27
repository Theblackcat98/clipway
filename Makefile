UUID := clipway@clipway.dev
EXT_DIR := extension
PREFIX ?= $(HOME)/.local
DATA_DIR := $(PREFIX)/share
BIN_DIR := $(PREFIX)/bin
EXT_INSTALL_DIR := $(DATA_DIR)/gnome-shell/extensions/$(UUID)
SCHEMA_DIR := $(DATA_DIR)/glib-2.0/schemas
APPS_DIR := $(DATA_DIR)/applications
UNIT_DIR := $(HOME)/.config/systemd/user
DBUS_SERVICE_DIR := $(DATA_DIR)/dbus-1/services
SCHEMA := io.clipway.Clipway.gschema.xml
EXT_FILES := metadata.json extension.js stylesheet.css

.PHONY: all build run nested headless-test check fmt fmt-check clippy clippy-gui test lint-extension \
	install install-schemas install-daemon install-desktop install-extension \
	install-service install-dbus-services dev-install extension-zip uninstall

all: build

build:
	cargo build --release

# Runs the installed binary: the extension only sends clipboard data to a
# daemon started from a trusted path (see extension.js, isTrustedDaemon).
run: install-daemon install-schemas
	-systemctl --user stop clipway-daemon.service
	$(BIN_DIR)/clipway-daemon

# A nested GNOME Shell with its own session bus, for testing the extension
# without logging out. Start clipway-daemon inside it from its terminal.
nested: dev-install
	dbus-run-session -- gnome-shell --devkit --wayland

# End-to-end test in a headless GNOME Shell. Container/VM only: see
# tests/headless/run-in-container.sh.
headless-test:
	tests/headless/run-in-container.sh

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

clippy:
	cargo clippy --all-targets --no-default-features -- -D warnings

clippy-gui:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test --no-default-features

# ESLint catches undefined names and banned imports; it cannot catch GNOME
# Shell API misuse. Run `make nested` before every release as well.
lint-extension:
	eslint $(EXT_DIR)/
	@python3 -m json.tool $(EXT_DIR)/metadata.json > /dev/null
	@glib-compile-schemas --strict --dry-run data
	@echo "extension checks passed"

check: fmt-check clippy test lint-extension

install-schemas:
	mkdir -p $(SCHEMA_DIR)
	cp data/$(SCHEMA) $(SCHEMA_DIR)/$(SCHEMA)
	glib-compile-schemas $(SCHEMA_DIR)

install-daemon: build
	mkdir -p $(BIN_DIR)
	install -m 0755 target/release/clipway-daemon $(BIN_DIR)/clipway-daemon

install-desktop:
	mkdir -p $(APPS_DIR)
	install -m 0644 data/io.clipway.Clipway.desktop $(APPS_DIR)/io.clipway.Clipway.desktop

install-extension:
	mkdir -p $(EXT_INSTALL_DIR)
	cp $(addprefix $(EXT_DIR)/,$(EXT_FILES)) $(EXT_INSTALL_DIR)/

install-service:
	mkdir -p $(UNIT_DIR)
	sed 's|@BINDIR@|$(BIN_DIR)|g' data/clipway-daemon.service.in > $(UNIT_DIR)/clipway-daemon.service
	systemctl --user daemon-reload
	systemctl --user enable clipway-daemon.service
	systemctl --user restart clipway-daemon.service

# Both bus names activate the same systemd unit, so D-Bus activation and the
# unit never start two copies.
install-dbus-services:
	mkdir -p $(DBUS_SERVICE_DIR)
	for name in io.clipway.Clipway io.clipway.ClipboardManager; do \
		sed -e "s|@NAME@|$$name|g" -e 's|@BINDIR@|$(BIN_DIR)|g' data/dbus-service.in \
			> $(DBUS_SERVICE_DIR)/$$name.service; \
	done

install: install-schemas install-daemon install-desktop install-extension install-dbus-services install-service
	@echo "Installed. Log out and back in, then: gnome-extensions enable $(UUID)"
	@echo "Choose a shortcut in Clipway's settings (none is set by default)."

dev-install: install-schemas install-extension
	@echo "Installed schemas and extension for development."

# What gets uploaded to extensions.gnome.org: the extension only. The app
# (daemon, schema, desktop file, services) is packaged separately.
extension-zip:
	@rm -f clipway-extension.zip
	@if command -v zip > /dev/null 2>&1; then \
		cd $(EXT_DIR) && zip -q ../clipway-extension.zip $(EXT_FILES); \
	else \
		cd $(EXT_DIR) && python3 -m zipfile -c ../clipway-extension.zip $(EXT_FILES); \
	fi
	@echo "built clipway-extension.zip"

uninstall:
	-systemctl --user disable --now clipway-daemon.service
	-gnome-extensions disable $(UUID)
	rm -rf $(EXT_INSTALL_DIR)
	rm -f $(BIN_DIR)/clipway-daemon
	rm -f $(UNIT_DIR)/clipway-daemon.service
	rm -f $(DBUS_SERVICE_DIR)/io.clipway.Clipway.service $(DBUS_SERVICE_DIR)/io.clipway.ClipboardManager.service
	rm -f $(APPS_DIR)/io.clipway.Clipway.desktop
	rm -f $(SCHEMA_DIR)/$(SCHEMA)
	-systemctl --user daemon-reload
	-glib-compile-schemas $(SCHEMA_DIR)
	@echo "Your history is kept in ~/.local/share/clipway; delete it yourself if you want it gone."

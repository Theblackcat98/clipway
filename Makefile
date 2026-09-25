UUID := clipway@clipway.dev
EXT_DIR := extension
PREFIX ?= $(HOME)/.local
DATA_DIR := $(PREFIX)/share
BIN_DIR := $(PREFIX)/bin
EXT_INSTALL_DIR := $(DATA_DIR)/gnome-shell/extensions/$(UUID)
SCHEMA_DIR := $(DATA_DIR)/glib-2.0/schemas
UNIT_DIR := $(HOME)/.config/systemd/user
DBUS_SERVICE_DIR := $(DATA_DIR)/dbus-1/services
EXT_FILES := metadata.json extension.js stylesheet.css io.clipway.Extension1.xml
CHECK_FILE := /tmp/clipway-extension-check.mjs

.PHONY: all build run check fmt fmt-check clippy clippy-gui test lint-extension install install-schemas install-daemon install-extension install-service install-dbus-service dev-install extension-zip uninstall

all: build

build:
	cargo build --release

run:
	GSETTINGS_SCHEMA_DIR=$(SCHEMA_DIR) cargo run --release

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

clippy:
	cargo clippy --all-targets --no-default-features

clippy-gui:
	cargo clippy --all-targets

test:
	cargo test --no-default-features

lint-extension:
	@cp $(EXT_DIR)/extension.js $(CHECK_FILE)
	@node --check $(CHECK_FILE)
	@rm -f $(CHECK_FILE)
	@python3 -m json.tool $(EXT_DIR)/metadata.json > /dev/null
	@glib-compile-schemas --strict --dry-run data
	@echo "extension checks passed"

check: fmt-check clippy test lint-extension

install-schemas:
	mkdir -p $(SCHEMA_DIR)
	cp data/org.gnome.clipway.gschema.xml $(SCHEMA_DIR)/org.gnome.clipway.gschema.xml
	glib-compile-schemas $(SCHEMA_DIR)

install-daemon: build
	mkdir -p $(BIN_DIR)
	cp target/release/clipway-daemon $(BIN_DIR)/clipway-daemon

install-extension:
	mkdir -p $(EXT_INSTALL_DIR)/gschemas
	cp $(addprefix $(EXT_DIR)/,$(EXT_FILES)) $(EXT_INSTALL_DIR)/
	cp data/org.gnome.clipway.gschema.xml $(EXT_INSTALL_DIR)/gschemas/org.gnome.clipway.gschema.xml
	glib-compile-schemas $(EXT_INSTALL_DIR)/gschemas

install-service:
	mkdir -p $(UNIT_DIR)
	cp data/clipway-daemon.service $(UNIT_DIR)/clipway-daemon.service
	systemctl --user daemon-reload
	systemctl --user enable --now clipway-daemon.service

install-dbus-service:
	mkdir -p $(DBUS_SERVICE_DIR)
	printf '[D-BUS Service]\nName=io.clipway.Clipway\nExec=%s/bin/clipway-daemon --daemon\n' "$(PREFIX)" > $(DBUS_SERVICE_DIR)/io.clipway.Clipway.service

install: install-schemas install-daemon install-extension install-dbus-service install-service
	@echo "Installed. Log out and back in to load the GNOME Shell extension."

dev-install: install-schemas install-extension
	@echo "Installed schemas and extension for development."

extension-zip:
	@rm -f clipway-extension.zip
	@if command -v zip > /dev/null 2>&1; then \
		cd $(EXT_DIR) && zip -q ../clipway-extension.zip $(EXT_FILES); \
	else \
		cd $(EXT_DIR) && python3 -m zipfile -c ../clipway-extension.zip $(EXT_FILES); \
	fi
	@echo "built clipway-extension.zip"

uninstall:
	rm -rf $(EXT_INSTALL_DIR)
	rm -f $(BIN_DIR)/clipway-daemon
	rm -f $(UNIT_DIR)/clipway-daemon.service
	rm -f $(DBUS_SERVICE_DIR)/io.clipway.Clipway.service
	rm -f $(SCHEMA_DIR)/org.gnome.clipway.gschema.xml
	systemctl --user disable --now clipway-daemon.service || true
	- glib-compile-schemas $(SCHEMA_DIR) || true

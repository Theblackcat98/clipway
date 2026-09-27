use std::cell::RefCell;
use std::sync::Arc;

use adw::prelude::*;

use crate::dbus::Bridge;
use crate::gui;
use crate::settings::Settings;

/// Suggested, never shipped as a default: extensions.gnome.org forbids
/// default shortcuts for clipboard data, and `<Super>v` is GNOME's own
/// notification-list shortcut.
const SUGGESTED_SHORTCUT: &str = "<Super><Shift>v";

/// Schemas whose keybindings a new shortcut is checked against.
const KEYBINDING_SCHEMAS: [&str; 5] = [
    "org.gnome.shell.keybindings",
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    "org.gnome.settings-daemon.plugins.media-keys",
];

thread_local! {
    static ACTIVE: RefCell<Option<adw::PreferencesDialog>> = const { RefCell::new(None) };
}

pub fn show(bridge: Arc<Bridge>, settings: Arc<Settings>) {
    ACTIVE.with(|slot| {
        let existing = slot.borrow().as_ref().cloned();
        if let Some(window) = existing {
            window.present(None::<&gtk::Widget>);
            return;
        }
        let window = build(&bridge, &settings);
        window.connect_closed(|_| {
            ACTIVE.with(|slot| {
                slot.borrow_mut().take();
            });
        });
        *slot.borrow_mut() = Some(window.clone());
        window.present(None::<&gtk::Widget>);
    });
}

fn build(bridge: &Arc<Bridge>, app_settings: &Settings) -> adw::PreferencesDialog {
    let settings = app_settings.raw();
    let window = adw::PreferencesDialog::new();
    window.set_title("Clipway Settings");
    window.set_content_width(680);
    window.set_content_height(640);
    window.set_search_enabled(true);

    let page = adw::PreferencesPage::new();
    page.set_title("Clipway");
    page.set_icon_name(Some("edit-paste-symbolic"));
    window.add(&page);

    let shortcut_group = adw::PreferencesGroup::new();
    shortcut_group.set_title("Shortcut");
    shortcut_group.set_description(Some(
        "Requires the Clipway GNOME Shell extension. No shortcut is set by default.",
    ));
    page.add(&shortcut_group);
    shortcut_group.add(&shortcut_row(settings, &window));

    let history = adw::PreferencesGroup::new();
    history.set_title("History");
    history.set_description(Some("How much clipboard history Clipway keeps."));
    page.add(&history);
    history.add(&spin_row(
        settings,
        "history-depth",
        "History depth (unpinned items)",
        (25.0, 10_000.0, 25.0),
        1.0,
    ));
    history.add(&spin_row(
        settings,
        "max-text-bytes",
        "Maximum text size (KiB)",
        (1.0, 65_536.0, 64.0),
        1024.0,
    ));
    history.add(&spin_row(
        settings,
        "max-image-bytes",
        "Maximum image size (KiB)",
        (1.0, 131_072.0, 1024.0),
        1024.0,
    ));
    let clear_on_logout = adw::SwitchRow::new();
    clear_on_logout.set_title("Clear history when the session ends");
    clear_on_logout.set_subtitle(
        "Unpinned entries are removed when you log out or restart. Pinned entries are kept.",
    );
    settings
        .bind("clear-on-logout", &clear_on_logout, "active")
        .build();
    history.add(&clear_on_logout);

    let privacy = adw::PreferencesGroup::new();
    privacy.set_title("Privacy");
    privacy.set_description(Some(
        "Content that password managers mark as secret is never recorded.",
    ));
    page.add(&privacy);
    let incognito = adw::SwitchRow::new();
    incognito.set_title("Incognito mode");
    incognito.set_subtitle("Do not capture new clipboard content while enabled.");
    settings.bind("incognito", &incognito, "active").build();
    privacy.add(&incognito);
    let sync_primary = adw::SwitchRow::new();
    sync_primary.set_title("Track primary selection");
    sync_primary.set_subtitle(
        "Also record text you select (the middle-click paste buffer). Records a lot of fragments.",
    );
    settings
        .bind("sync-primary", &sync_primary, "active")
        .build();
    privacy.add(&sync_primary);

    let excluded_group = adw::PreferencesGroup::new();
    excluded_group.set_title("Excluded applications");
    excluded_group.set_description(Some(
        "Nothing copied while one of these apps is focused is recorded. Use the app ID \
         (for example org.keepassxc.KeePassXC) or the window class. Copies made from a \
         terminal, such as pass or wl-copy, count as the terminal.",
    ));
    page.add(&excluded_group);
    let excluded_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    excluded_group.add(&excluded_box);
    let add_row = adw::EntryRow::new();
    add_row.set_title("Add app ID or window class");
    {
        let settings = settings.clone();
        let excluded_box = excluded_box.clone();
        add_row.connect_apply(move |entry| add_excluded(entry, &settings, &excluded_box));
    }
    add_row.set_show_apply_button(true);
    {
        let settings = settings.clone();
        let excluded_box = excluded_box.clone();
        add_row.connect_entry_activated(move |entry| add_excluded(entry, &settings, &excluded_box));
    }
    excluded_group.add(&add_row);
    rebuild_excluded(&excluded_box, settings);

    let data = adw::PreferencesGroup::new();
    data.set_title("Data");
    page.add(&data);
    let clear_row = adw::ActionRow::new();
    clear_row.set_title("Clear history…");
    let count_label = gtk::Label::new(Some(""));
    count_label.add_css_class("dim-label");
    clear_row.add_suffix(&count_label);
    let clear_button = gtk::Button::with_label("Clear…");
    clear_button.add_css_class("destructive-action");
    clear_button.set_valign(gtk::Align::Center);
    clear_row.add_suffix(&clear_button);
    data.add(&clear_row);
    update_count(bridge, &count_label);
    {
        let bridge = bridge.clone();
        let window_ref = window.downgrade();
        clear_button.connect_clicked(move |_| {
            let Some(window) = window_ref.upgrade() else {
                return;
            };
            let bridge_for_count = bridge.clone();
            let count_label = count_label.clone();
            gui::confirm_clear(Some(window.upcast_ref()), &bridge, move || {
                update_count(&bridge_for_count, &count_label);
            });
        });
    }

    let about = adw::PreferencesGroup::new();
    about.set_title("About");
    page.add(&about);
    let version = adw::ActionRow::new();
    version.set_title("Clipway");
    version.set_subtitle(&format!("Version {}", env!("CARGO_PKG_VERSION")));
    about.add(&version);
    let database = adw::ActionRow::new();
    database.set_title("History database (encrypted)");
    database.set_subtitle(&crate::database_path_display());
    database.set_subtitle_selectable(true);
    about.add(&database);

    window
}

fn shortcut_row(settings: &gio::Settings, window: &adw::PreferencesDialog) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title("Open clipboard history");
    row.set_use_markup(false);

    let set_button = gtk::Button::with_label("Set…");
    set_button.set_valign(gtk::Align::Center);
    let suggest_button = gtk::Button::with_label("Use Super+Shift+V");
    suggest_button.set_valign(gtk::Align::Center);
    suggest_button.add_css_class("flat");
    let clear_button = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear_button.set_valign(gtk::Align::Center);
    clear_button.add_css_class("flat");
    clear_button.set_tooltip_text(Some("Remove shortcut"));
    row.add_suffix(&suggest_button);
    row.add_suffix(&clear_button);
    row.add_suffix(&set_button);

    let refresh = {
        let row = row.clone();
        let settings = settings.clone();
        let suggest_button = suggest_button.clone();
        let clear_button = clear_button.clone();
        move || {
            let current = settings.strv("popup-keybinding");
            let accel = current
                .iter()
                .map(|value| value.as_str())
                .find(|value| !value.is_empty() && *value != "disabled");
            suggest_button.set_visible(accel.is_none());
            clear_button.set_visible(accel.is_some());
            let Some(accel) = accel else {
                row.set_subtitle("Not set");
                return;
            };
            let label = accelerator_label(accel);
            match find_conflict(accel) {
                Some(conflict) => row.set_subtitle(&format!(
                    "{label} — also used by GNOME ({conflict}); one of them will not work"
                )),
                None => row.set_subtitle(&label),
            }
        }
    };
    refresh();
    {
        let refresh = refresh.clone();
        settings.connect_changed(Some("popup-keybinding"), move |_, _| refresh());
    }
    {
        let settings = settings.clone();
        suggest_button.connect_clicked(move |_| {
            let _ = settings.set_strv("popup-keybinding", [SUGGESTED_SHORTCUT]);
        });
    }
    {
        let settings = settings.clone();
        clear_button.connect_clicked(move |_| {
            let _ = settings.set_strv("popup-keybinding", [""; 0]);
        });
    }
    {
        let settings = settings.clone();
        let window = window.downgrade();
        set_button.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                capture_shortcut(&window, &settings);
            }
        });
    }
    row
}

/// A dialog that records the next key combination.
fn capture_shortcut(parent: &adw::PreferencesDialog, settings: &gio::Settings) {
    let dialog = adw::AlertDialog::new(
        Some("Press the new shortcut"),
        Some("Press Escape to cancel, or Backspace to remove the shortcut."),
    );
    dialog.add_response("cancel", "_Cancel");
    dialog.set_close_response("cancel");

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let settings = settings.clone();
    let dialog_ref = dialog.downgrade();
    keys.connect_key_pressed(move |_, keyval, _, state| {
        use gtk::gdk::Key;
        let mods = state & gtk::accelerator_get_default_mod_mask();
        let keyval = keyval.to_lower();
        let close = || {
            if let Some(dialog) = dialog_ref.upgrade() {
                dialog.close();
            }
        };
        if mods.is_empty() && keyval == Key::Escape {
            close();
            return glib::Propagation::Stop;
        }
        if mods.is_empty() && keyval == Key::BackSpace {
            let _ = settings.set_strv("popup-keybinding", [""; 0]);
            close();
            return glib::Propagation::Stop;
        }
        if is_modifier(keyval) || !gtk::accelerator_valid(keyval, mods) {
            return glib::Propagation::Stop;
        }
        // A shortcut without Super/Ctrl/Alt would swallow ordinary typing.
        let needs = gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::ALT_MASK;
        if !mods.intersects(needs) {
            return glib::Propagation::Stop;
        }
        let name = gtk::accelerator_name(keyval, mods);
        let _ = settings.set_strv("popup-keybinding", [name.as_str()]);
        close();
        glib::Propagation::Stop
    });
    dialog.add_controller(keys);
    dialog.present(Some(parent));
}

fn is_modifier(keyval: gtk::gdk::Key) -> bool {
    use gtk::gdk::Key;
    matches!(
        keyval,
        Key::Shift_L
            | Key::Shift_R
            | Key::Control_L
            | Key::Control_R
            | Key::Alt_L
            | Key::Alt_R
            | Key::Meta_L
            | Key::Meta_R
            | Key::Super_L
            | Key::Super_R
            | Key::Hyper_L
            | Key::Hyper_R
            | Key::ISO_Level3_Shift
            | Key::Caps_Lock
    )
}

fn accelerator_label(accel: &str) -> String {
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => accel.to_string(),
    }
}

/// Returns the name of a GNOME keybinding that uses the same accelerator.
fn find_conflict(accel: &str) -> Option<String> {
    let wanted = gtk::accelerator_parse(accel)?;
    let wanted = (wanted.0.to_lower(), wanted.1);
    let source = gio::SettingsSchemaSource::default()?;
    for schema_id in KEYBINDING_SCHEMAS {
        let Some(schema) = source.lookup(schema_id, true) else {
            continue;
        };
        let settings = gio::Settings::new(schema_id);
        for key in schema.list_keys() {
            if schema.key(&key).value_type().as_str() != "as" {
                continue;
            }
            let taken = settings.strv(&key).iter().any(|value| {
                gtk::accelerator_parse(value.as_str())
                    .is_some_and(|(k, m)| (k.to_lower(), m) == wanted)
            });
            if taken {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn spin_row(
    settings: &gio::Settings,
    key: &str,
    title: &str,
    (min, max, step): (f64, f64, f64),
    scale: f64,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_value(f64::from(settings.uint(key)) / scale);
    let settings = settings.clone();
    let key = key.to_string();
    row.connect_value_notify(move |row| {
        let _ = settings.set_uint(&key, (row.value() * scale).round() as u32);
    });
    row
}

fn add_excluded(entry: &adw::EntryRow, settings: &gio::Settings, excluded_box: &gtk::Box) {
    let value = entry.text().trim().to_string();
    entry.set_text("");
    if value.is_empty() {
        return;
    }
    let mut apps: Vec<String> = settings
        .strv("excluded-apps")
        .iter()
        .map(|app| app.to_string())
        .collect();
    if apps.iter().any(|app| app.eq_ignore_ascii_case(&value)) {
        return;
    }
    apps.push(value);
    let _ = settings.set_strv("excluded-apps", apps);
    rebuild_excluded(excluded_box, settings);
}

fn rebuild_excluded(box_widget: &gtk::Box, settings: &gio::Settings) {
    while let Some(child) = box_widget.first_child() {
        box_widget.remove(&child);
    }
    let apps: Vec<String> = settings
        .strv("excluded-apps")
        .iter()
        .map(|app| app.to_string())
        .collect();
    if apps.is_empty() {
        let empty = adw::ActionRow::new();
        empty.set_title("No applications excluded");
        empty.set_subtitle("Everything you copy is captured.");
        box_widget.append(&empty);
        return;
    }
    for app in apps {
        let row = adw::ActionRow::new();
        // App IDs are user-supplied text, never markup.
        row.set_use_markup(false);
        row.set_title(&app);
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_valign(gtk::Align::Center);
        remove.set_tooltip_text(Some("Remove"));
        let settings = settings.clone();
        let list_box = box_widget.clone();
        let needle = app.clone();
        remove.connect_clicked(move |_| {
            let remaining: Vec<String> = settings
                .strv("excluded-apps")
                .iter()
                .map(|entry| entry.to_string())
                .filter(|entry| !entry.eq_ignore_ascii_case(&needle))
                .collect();
            let _ = settings.set_strv("excluded-apps", remaining);
            rebuild_excluded(&list_box, &settings);
        });
        row.add_suffix(&remove);
        box_widget.append(&row);
    }
}

fn update_count(bridge: &Arc<Bridge>, label: &gtk::Label) {
    let (total, pinned) = bridge.store().counts().unwrap_or((0, 0));
    label.set_text(&format!("{total} stored, {pinned} pinned"));
}

use std::cell::RefCell;
use std::sync::Arc;

use adw::prelude::*;

use crate::dbus::Bridge;
use crate::settings::Settings;

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

    let history = adw::PreferencesGroup::new();
    history.set_title("History");
    history.set_description(Some("How much clipboard history Clipway keeps."));
    page.add(&history);
    history.add(&spin_row(
        settings,
        "history-depth",
        "History depth (items)",
        25.0,
        10_000.0,
        1.0,
    ));
    history.add(&spin_row(
        settings,
        "max-text-bytes",
        "Maximum text size (KiB)",
        1.0,
        65_536.0,
        1024.0,
    ));
    history.add(&spin_row(
        settings,
        "max-image-bytes",
        "Maximum image size (KiB)",
        1.0,
        131_072.0,
        1024.0,
    ));
    let clear_on_logout = adw::SwitchRow::new();
    clear_on_logout.set_title("Clear history on logout");
    clear_on_logout.set_subtitle("Pinned entries are always kept.");
    settings
        .bind("clear-on-logout", &clear_on_logout, "active")
        .build();
    history.add(&clear_on_logout);

    let privacy = adw::PreferencesGroup::new();
    privacy.set_title("Privacy");
    page.add(&privacy);
    let incognito = adw::SwitchRow::new();
    incognito.set_title("Incognito mode");
    incognito.set_subtitle("Do not capture new clipboard content while enabled.");
    settings.bind("incognito", &incognito, "active").build();
    privacy.add(&incognito);
    let sync_primary = adw::SwitchRow::new();
    sync_primary.set_title("Track primary selection");
    sync_primary.set_subtitle("Record text selected with the middle mouse button.");
    settings
        .bind("sync-primary", &sync_primary, "active")
        .build();
    privacy.add(&sync_primary);

    let excluded_group = adw::PreferencesGroup::new();
    excluded_group.set_title("Excluded applications");
    excluded_group.set_description(Some(
        "Clipboard content copied from these applications is never captured. Entries use the window class name, for example firefox or org.gnome.Nautilus.",
    ));
    page.add(&excluded_group);
    let excluded_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    excluded_group.add(&excluded_box);
    let add_row = adw::EntryRow::new();
    add_row.set_title("Add application class");
    {
        let settings = settings.clone();
        let excluded_box = excluded_box.clone();
        add_row.connect_activate(move |entry| {
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
            rebuild_excluded(&excluded_box, &settings);
        });
    }
    excluded_group.add(&add_row);
    rebuild_excluded(&excluded_box, settings);

    let data = adw::PreferencesGroup::new();
    data.set_title("Data");
    page.add(&data);
    let clear_row = adw::ActionRow::new();
    clear_row.set_title("Clear history now");
    let count_label = gtk::Label::new(Some(""));
    count_label.add_css_class("dim-label");
    clear_row.add_suffix(&count_label);
    let clear_button = gtk::Button::with_label("Clear");
    clear_button.add_css_class("destructive-action");
    clear_row.add_suffix(&clear_button);
    data.add(&clear_row);
    update_count(bridge, &count_label);
    {
        let bridge = bridge.clone();
        let count_label = count_label.clone();
        let window_ref = window.downgrade();
        clear_button.connect_clicked(move |_| {
            let Some(window) = window_ref.upgrade() else {
                return;
            };
            let dialog = adw::AlertDialog::new(
                Some("Clear clipboard history?"),
                Some("Every entry, including pinned ones, will be deleted."),
            );
            dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear")]);
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let bridge = bridge.clone();
            let count_label = count_label.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "clear" {
                    let bridge = bridge.clone();
                    let count_label = count_label.clone();
                    glib::spawn_future_local(async move {
                        let _ = bridge.clear(false).await;
                        update_count(&bridge, &count_label);
                    });
                }
                dialog.close();
            });
            dialog.present(Some(&window));
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
    database.set_title("History database");
    database.set_subtitle(&crate::database_path_display());
    about.add(&database);
    let contract = adw::ActionRow::new();
    contract.set_title("D-Bus interface");
    contract.set_subtitle("io.clipway.ClipboardManager1");
    about.add(&contract);

    window
}

fn spin_row(
    settings: &gio::Settings,
    key: &str,
    title: &str,
    min: f64,
    max: f64,
    step: f64,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_value(f64::from(settings.uint(key)) / step);
    let settings = settings.clone();
    let key = key.to_string();
    row.connect_value_notify(move |row| {
        let _ = settings.set_uint(&key, (row.value() * step).round() as u32);
    });
    row
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

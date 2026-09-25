use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use anyhow::Result;
use glib::{OptionArg, OptionFlags};

use crate::dbus::Bridge;
use crate::popup;
use crate::prefs;
use crate::settings::Settings;

pub const APP_ID: &str = "io.clipway.Clipway";

thread_local! {
    static HOLD: RefCell<Option<gio::ApplicationHoldGuard>> = const { RefCell::new(None) };
}

pub fn run(bridge: Arc<Bridge>, settings: Arc<Settings>) -> Result<()> {
    adw::init()?;
    let app = adw::Application::builder().application_id(APP_ID).build();

    {
        let bridge_for_watch = bridge.clone();
        settings.raw().connect("changed", false, move |_| {
            if let Ok(fresh) = Settings::new() {
                bridge_for_watch.update_settings(fresh.snapshot());
            }
            None
        });
    }

    let preferences_requested = Rc::new(Cell::new(false));
    let background = Rc::new(Cell::new(false));
    let screenshot = Rc::new(RefCell::new(None::<String>));

    app.add_main_option(
        "screenshot",
        glib::Char::from(b's'),
        OptionFlags::NONE,
        OptionArg::String,
        "Save a screenshot of the popup to a PNG file and exit",
        Some("FILENAME"),
    );
    app.add_main_option(
        "preferences",
        glib::Char::from(b'p'),
        OptionFlags::NONE,
        OptionArg::None,
        "Open settings",
        None,
    );
    app.add_main_option(
        "daemon",
        glib::Char::from(b'd'),
        OptionFlags::NONE,
        OptionArg::None,
        "Start without opening a window",
        None,
    );
    app.set_flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE);

    let preferences_flag = preferences_requested.clone();
    let background_flag = background.clone();
    let screenshot_flag = screenshot.clone();
    app.connect_command_line(move |_app, cmdline| {
        let options = cmdline.options_dict();
        let flag = |key: &str| {
            options
                .lookup_value(key, Some(glib::VariantTy::BOOLEAN))
                .and_then(|value| value.get::<bool>())
                .unwrap_or(false)
        };
        if flag("preferences") {
            preferences_flag.set(true);
        }
        if flag("daemon") {
            background_flag.set(true);
        }
        if let Some(path) = options
            .lookup_value("screenshot", Some(glib::VariantTy::STRING))
            .and_then(|value| value.get::<String>())
        {
            *screenshot_flag.borrow_mut() = Some(path);
        }
        glib::ExitCode::SUCCESS
    });

    let startup_bridge = bridge.clone();
    let startup_settings = settings.clone();
    let startup_screenshot = screenshot.clone();
    app.connect_startup(move |app| {
        register_actions(app, &startup_bridge, &startup_settings);
        if startup_screenshot.borrow().is_none() {
            HOLD.with(|slot| {
                *slot.borrow_mut() = Some(app.hold());
            });
        }
    });

    let activate_bridge = bridge;
    let activate_settings = settings;
    let preferences_flag = preferences_requested;
    let background_flag = background;
    let activate_screenshot = screenshot;
    app.connect_activate(move |app| {
        if background_flag.get() {
            return;
        }
        if preferences_flag.get() {
            let _ = app;
            prefs::show(activate_bridge.clone(), activate_settings.clone());
        } else {
            popup::show(app, activate_bridge.clone(), "");
        }
        if let Some(path) = activate_screenshot.borrow().clone() {
            popup::schedule_screenshot(app.clone(), path);
        }
    });

    app.run();
    Ok(())
}

fn register_actions(app: &adw::Application, bridge: &Arc<Bridge>, settings: &Arc<Settings>) {
    let popup_action = gio::SimpleAction::new("popup", None);
    let app_for_popup = app.clone();
    let bridge_for_popup = bridge.clone();
    popup_action.connect_activate(move |_, parameter| {
        let query = parameter
            .and_then(|value| value.str())
            .map(|value| value.to_string())
            .unwrap_or_default();
        popup::show(&app_for_popup, bridge_for_popup.clone(), &query);
    });
    app.add_action(&popup_action);

    let preferences_action = gio::SimpleAction::new("preferences", None);
    let bridge_for_prefs = bridge.clone();
    let settings_for_prefs = settings.clone();
    preferences_action.connect_activate(move |_, _| {
        prefs::show(bridge_for_prefs.clone(), settings_for_prefs.clone());
    });
    app.add_action(&preferences_action);
}

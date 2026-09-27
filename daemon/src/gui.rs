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

    // History can change on the D-Bus thread (a new copy arrives); refresh an
    // open popup on the main thread when it does.
    bridge.set_change_hook(|| {
        glib::MainContext::default().invoke(popup::refresh_if_visible);
    });

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
    // Handled in main() before the application starts; declared here so
    // GApplication does not reject it.
    app.add_main_option(
        "reset-history",
        glib::Char::from(0),
        OptionFlags::NONE,
        OptionArg::None,
        "Move the current history aside and start a new, empty one",
        None,
    );
    app.set_flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE);

    // Options apply to the invocation that carried them only; a later
    // launch from the app grid (org.freedesktop.Application.Activate) always
    // opens the popup.
    let cmdline_bridge = bridge.clone();
    let cmdline_settings = settings.clone();
    let screenshot_flag = screenshot.clone();
    app.connect_command_line(move |app, cmdline| {
        let options = cmdline.options_dict();
        let flag = |key: &str| {
            options
                .lookup_value(key, Some(glib::VariantTy::BOOLEAN))
                .and_then(|value| value.get::<bool>())
                .unwrap_or(false)
        };
        if flag("daemon") {
            return glib::ExitCode::SUCCESS;
        }
        if flag("preferences") {
            prefs::show(cmdline_bridge.clone(), cmdline_settings.clone());
            return glib::ExitCode::SUCCESS;
        }
        if let Some(path) = options
            .lookup_value("screenshot", Some(glib::VariantTy::STRING))
            .and_then(|value| value.get::<String>())
        {
            *screenshot_flag.borrow_mut() = Some(path);
        }
        app.activate();
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

    app.connect_activate(move |app| {
        popup::show(app, bridge.clone());
        if let Some(path) = screenshot.borrow_mut().take() {
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
    popup_action.connect_activate(move |_, _| {
        popup::show(&app_for_popup, bridge_for_popup.clone());
    });
    app.add_action(&popup_action);

    let preferences_action = gio::SimpleAction::new("preferences", None);
    let bridge_for_prefs = bridge.clone();
    let settings_for_prefs = settings.clone();
    preferences_action.connect_activate(move |_, _| {
        prefs::show(bridge_for_prefs.clone(), settings_for_prefs.clone());
    });
    app.add_action(&preferences_action);

    // The panel menu's "Clear History…" lands here so it gets the same
    // confirmation as the popup and Preferences.
    let clear_action = gio::SimpleAction::new("clear-history", None);
    let bridge_for_clear = bridge.clone();
    clear_action.connect_activate(move |_, _| {
        confirm_clear(None, &bridge_for_clear, || {});
    });
    app.add_action(&clear_action);
}

/// Asks before clearing. The default keeps pinned entries; deleting them too
/// is a separate, explicit choice.
pub fn confirm_clear(
    parent: Option<&gtk::Widget>,
    bridge: &Arc<Bridge>,
    on_done: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(
        Some("Clear clipboard history?"),
        Some(
            "Unpinned entries will be deleted. Pinned entries are kept unless you choose Delete Everything.",
        ),
    );
    dialog.add_responses(&[
        ("cancel", "_Cancel"),
        ("clear-all", "Delete _Everything"),
        ("clear", "_Clear History"),
    ]);
    dialog.set_response_appearance("clear-all", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("clear", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let bridge = bridge.clone();
    let on_done = Rc::new(on_done);
    dialog.connect_response(None, move |_, response| {
        let keep_pinned = match response {
            "clear" => true,
            "clear-all" => false,
            _ => return,
        };
        let bridge = bridge.clone();
        let on_done = on_done.clone();
        glib::spawn_future_local(async move {
            if let Err(error) = bridge.clear(keep_pinned).await {
                eprintln!("clipway: clearing history failed: {error}");
            }
            popup::refresh_if_visible();
            popup::toast("History cleared");
            on_done();
        });
    });
    dialog.present(parent);
}

/// Shown when the history database exists but cannot be decrypted. Returns
/// true if the user chose to start over. Runs before the application starts.
pub fn confirm_reset(details: &str) -> bool {
    if adw::init().is_err() {
        return false;
    }
    let dialog = adw::AlertDialog::new(
        Some("Clipway can't open your clipboard history"),
        Some(&format!(
            "The key that protects your history is missing from the login keyring or no \
             longer matches. This usually happens after the keyring was reset.\n\n\
             Start a new, empty history? The old file is kept, renamed, in case the key \
             turns up again.\n\n{details}"
        )),
    );
    dialog.add_responses(&[("quit", "_Quit"), ("reset", "_Start New History")]);
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("quit"));
    dialog.set_close_response("quit");

    let main_loop = glib::MainLoop::new(None, false);
    let choice = Rc::new(Cell::new(false));
    {
        let main_loop = main_loop.clone();
        let choice = choice.clone();
        dialog.connect_response(None, move |_, response| {
            choice.set(response == "reset");
            main_loop.quit();
        });
    }
    dialog.present(None::<&gtk::Widget>);
    main_loop.run();
    choice.get()
}

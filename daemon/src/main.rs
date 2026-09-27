#![cfg_attr(not(feature = "gui"), allow(dead_code))]

mod crypto;
mod dbus;
mod model;
mod settings;
mod store;

#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "gui")]
mod popup;
#[cfg(feature = "gui")]
mod prefs;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::crypto::KeyMissing;
use crate::dbus::Bridge;
use crate::settings::Settings;
use crate::store::{Store, WrongKey};

/// Exit status when the history cannot be unlocked and the user chose not to
/// reset it. The systemd unit lists it in `RestartPreventExitStatus=` so the
/// service does not crash-loop.
pub const EXIT_HISTORY_LOCKED: i32 = 3;

fn main() -> Result<()> {
    let db = db_path()?;
    if std::env::args().any(|arg| arg == "--reset-history") {
        reset_history(&db)?;
    }

    let store = match open_store(&db) {
        Ok(store) => store,
        Err(error) if is_unreadable_history(&error) => recover(&db, &error)?,
        Err(error) => return Err(error),
    };

    let settings = Settings::new()?;
    let bridge = Arc::new(Bridge::new(Arc::new(store), settings.snapshot()));
    bridge.start_session();
    dbus::spawn_service(bridge.clone());

    #[cfg(feature = "gui")]
    {
        #[allow(clippy::arc_with_non_send_sync)]
        gui::run(bridge, Arc::new(settings))
    }
    #[cfg(not(feature = "gui"))]
    {
        println!("clipway-daemon: running headless (built without the gui feature)");
        zbus::block_on(std::future::pending::<()>());
        Ok(())
    }
}

fn open_store(db: &Path) -> Result<Store> {
    let key = crypto::database_key(db.exists()).context("obtaining the database key")?;
    Store::open(db, &key)
}

fn is_unreadable_history(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<KeyMissing>().is_some() || cause.downcast_ref::<WrongKey>().is_some()
    })
}

/// The database exists but cannot be decrypted (the login keyring was reset,
/// or the key was deleted). Ask before starting over; never do it silently.
fn recover(db: &Path, error: &anyhow::Error) -> Result<Store> {
    eprintln!("clipway: {error:#}");
    #[cfg(feature = "gui")]
    let reset = gui::confirm_reset(&format!("{error:#}"));
    #[cfg(not(feature = "gui"))]
    let reset = false;
    if !reset {
        eprintln!(
            "clipway: history left untouched at {}. Run 'clipway-daemon --reset-history' to \
             move it aside and start a new, empty history.",
            db.display()
        );
        std::process::exit(EXIT_HISTORY_LOCKED);
    }
    reset_history(db)?;
    open_store(db)
}

/// Moves the old database aside (it is kept, not deleted) and stores a fresh
/// key in the keyring.
fn reset_history(db: &Path) -> Result<()> {
    if db.exists() {
        let stamp = dbus::now_millis();
        let backup = db.with_extension(format!("db.unreadable-{stamp}"));
        std::fs::rename(db, &backup).with_context(|| format!("moving {} aside", db.display()))?;
        let _ = std::fs::remove_file(db.with_extension("db-journal"));
        eprintln!("clipway: old history moved to {}", backup.display());
    }
    crypto::reset_key()?;
    Ok(())
}

pub fn database_path_display() -> String {
    db_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "(unknown)".to_string())
}

fn db_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLIPWAY_DB_PATH") {
        return Ok(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .context("cannot determine the data directory; set XDG_DATA_HOME or HOME")?;
    Ok(base.join("clipway").join("history.db"))
}

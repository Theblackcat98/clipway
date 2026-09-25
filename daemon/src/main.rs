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

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::dbus::Bridge;
use crate::settings::Settings;
use crate::store::Store;

fn main() -> Result<()> {
    let key = crypto::database_key().context("obtaining the database key")?;
    let settings = Settings::new()?;
    let snapshot = settings.snapshot();
    let store = Arc::new(Store::open(&db_path()?, &key)?);
    let bridge = Arc::new(Bridge::new(store, snapshot));
    dbus::spawn_service(bridge.clone());
    dbus::spawn_session_watchers(bridge.clone());

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

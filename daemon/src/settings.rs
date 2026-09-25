use anyhow::{Result, bail};
use gio::prelude::*;
use gio::{Settings as GioSettings, SettingsSchemaSource};

const SCHEMA_ID: &str = "org.gnome.clipway";

pub struct Settings {
    inner: GioSettings,
}

impl Settings {
    pub fn new() -> Result<Self> {
        let source = SettingsSchemaSource::default()
            .ok_or_else(|| anyhow::anyhow!("no GSettings schema source available"))?;
        if source.lookup(SCHEMA_ID, true).is_none() {
            bail!("GSettings schema {SCHEMA_ID} is not installed; run 'make install-schemas'");
        }
        Ok(Self {
            inner: GioSettings::new(SCHEMA_ID),
        })
    }

    pub fn incognito(&self) -> bool {
        self.inner.boolean("incognito")
    }

    pub fn excluded_apps(&self) -> Vec<String> {
        self.inner
            .strv("excluded-apps")
            .iter()
            .map(|app| app.trim().to_lowercase())
            .filter(|app| !app.is_empty())
            .collect()
    }

    pub fn history_depth(&self) -> u32 {
        self.inner.uint("history-depth").clamp(25, 10_000)
    }

    pub fn max_text_bytes(&self) -> usize {
        self.inner
            .uint("max-text-bytes")
            .clamp(1024, 64 * 1024 * 1024) as usize
    }

    pub fn max_image_bytes(&self) -> usize {
        self.inner
            .uint("max-image-bytes")
            .clamp(1024, 128 * 1024 * 1024) as usize
    }

    pub fn clear_on_logout(&self) -> bool {
        self.inner.boolean("clear-on-logout")
    }

    pub fn raw(&self) -> &GioSettings {
        &self.inner
    }

    pub fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            incognito: self.incognito(),
            history_depth: self.history_depth(),
            max_text_bytes: self.max_text_bytes(),
            max_image_bytes: self.max_image_bytes(),
            clear_on_logout: self.clear_on_logout(),
            excluded_apps: self.excluded_apps(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    pub incognito: bool,
    pub history_depth: u32,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
    pub clear_on_logout: bool,
    pub excluded_apps: Vec<String>,
}

impl SettingsSnapshot {
    pub fn is_excluded(&self, source_app: &str) -> bool {
        let needle = source_app.trim().to_lowercase();
        !needle.is_empty() && self.excluded_apps.contains(&needle)
    }
}

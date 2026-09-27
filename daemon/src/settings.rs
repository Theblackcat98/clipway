use anyhow::{Result, bail};
use gio::prelude::*;
use gio::{Settings as GioSettings, SettingsSchemaSource};

/// The app owns all settings. The Shell extension reads this schema too (it
/// ships no schema of its own), so both halves always agree.
pub const SCHEMA_ID: &str = "io.clipway.Clipway";

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
            .map(|app| normalize_app_id(app.as_str()))
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

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
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

/// Lower-cases and drops a trailing `.desktop`, so `org.keepassxc.KeePassXC`,
/// `org.keepassxc.KeePassXC.desktop` and `org.keepassxc.keepassxc` all match.
pub fn normalize_app_id(app: &str) -> String {
    let lowered = app.trim().to_lowercase();
    lowered
        .strip_suffix(".desktop")
        .map(str::to_string)
        .unwrap_or(lowered)
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
    /// `source_app` may hold several identifiers separated by `|` (app id,
    /// window class); any of them matching excludes the entry.
    pub fn is_excluded(&self, source_app: &str) -> bool {
        source_app
            .split('|')
            .map(normalize_app_id)
            .filter(|id| !id.is_empty())
            .any(|id| self.excluded_apps.contains(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(excluded: &[&str]) -> SettingsSnapshot {
        SettingsSnapshot {
            incognito: false,
            history_depth: 500,
            max_text_bytes: 1024,
            max_image_bytes: 1024,
            clear_on_logout: true,
            excluded_apps: excluded.iter().map(|app| normalize_app_id(app)).collect(),
        }
    }

    #[test]
    fn matches_app_ids_and_window_classes_case_insensitively() {
        let settings = snapshot(&["org.keepassxc.KeePassXC.desktop", "bitwarden"]);
        assert!(settings.is_excluded("org.keepassxc.KeePassXC"));
        assert!(settings.is_excluded("org.keepassxc.KeePassXC|keepassxc"));
        assert!(settings.is_excluded("com.bitwarden.desktop|Bitwarden"));
        assert!(!settings.is_excluded("org.gnome.TextEditor|gnome-text-editor"));
        assert!(!settings.is_excluded(""));
    }
}

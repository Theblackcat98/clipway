//! Session-bus contract between the daemon and the GNOME Shell extension.
//!
//! Both directions are authenticated: the daemon only answers callers whose
//! executable is the root-owned `gnome-shell` binary, and before sending a
//! payload to the extension it checks that the owner of
//! `io.clipway.Extension` is that same process. A sandboxed app, or a process
//! that grabs one of the well-known names first, gets nothing.
//!
//! Threat model, stated plainly: an unsandboxed program running as the same
//! user can already ask the Secret Service for the database key. These checks
//! stop the D-Bus API from adding *new* capabilities (reading history without
//! a prompt, setting the clipboard from the background); they are not a
//! defence against arbitrary same-user code.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, connection, fdo, interface, proxy};

use crate::model::{EntryMeta, is_restorable, is_sensitive, max_bytes_for, normalize};
use crate::settings::SettingsSnapshot;
use crate::store::{AddOptions, AddOutcome, Store};

pub const BUS_NAME: &str = "io.clipway.ClipboardManager";
pub const OBJECT_PATH: &str = "/io/clipway/ClipboardManager";
pub const ENTRY_PATH_PREFIX: &str = "/io/clipway/entries";
pub const EXTENSION_BUS_NAME: &str = "io.clipway.Extension";
pub const EXTENSION_OBJECT_PATH: &str = "/io/clipway/Extension";

/// `GetRecent` never returns more than this many rows.
const RECENT_MAX: u32 = 50;

#[proxy(
    interface = "io.clipway.Extension1",
    default_service = "io.clipway.Extension",
    default_path = "/io/clipway/Extension"
)]
trait Extension {
    fn set_clipboard(&self, mime: &str, data: Vec<u8>) -> zbus::Result<()>;
}

pub fn entry_path(id: i64) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("{ENTRY_PATH_PREFIX}/{id}"))
        .expect("entry object path is valid")
}

pub fn parse_entry_id(path: &str) -> Option<i64> {
    path.strip_prefix(&format!("{ENTRY_PATH_PREFIX}/"))?
        .parse()
        .ok()
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

type ChangeHook = Box<dyn Fn() + Send + Sync>;

pub struct Bridge {
    store: Arc<Store>,
    settings: Mutex<SettingsSnapshot>,
    conn: OnceLock<Connection>,
    on_change: OnceLock<ChangeHook>,
}

impl Bridge {
    pub fn new(store: Arc<Store>, settings: SettingsSnapshot) -> Self {
        Self {
            store,
            settings: Mutex::new(settings),
            conn: OnceLock::new(),
            on_change: OnceLock::new(),
        }
    }

    pub fn attach_connection(&self, conn: &Connection) {
        let _ = self.conn.set(conn.clone());
    }

    /// Called (from any thread) whenever history changes. The GUI uses it to
    /// refresh an open popup.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_change_hook(&self, hook: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_change.set(Box::new(hook));
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn update_settings(&self, settings: SettingsSnapshot) {
        *self
            .settings
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = settings;
    }

    pub fn snapshot(&self) -> SettingsSnapshot {
        self.settings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<EntryMeta>> {
        self.store.recent(limit)
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<EntryMeta>> {
        self.store.search(query, limit)
    }

    pub async fn add_entry(&self, mime: &str, data: &[u8], source_app: &str) -> fdo::Result<()> {
        if is_sensitive(mime) {
            return Ok(());
        }
        let Some(item) = normalize(mime, data) else {
            return Ok(());
        };
        let settings = self.snapshot();
        if settings.incognito || settings.is_excluded(source_app) {
            return Ok(());
        }
        let options = AddOptions {
            max_bytes: max_bytes_for(item.kind, &settings),
            depth: settings.history_depth,
            now: now_millis(),
        };
        // Record the most specific identifier (the app id comes first).
        let source = source_app.split('|').next().unwrap_or_default();
        let outcome = self
            .store
            .add(&item, source, &options)
            .map_err(|error| fdo::Error::Failed(format!("{error}")))?;
        match outcome {
            AddOutcome::Added(_) | AddOutcome::Duplicate(_) => {
                self.history_changed().await;
                Ok(())
            }
            AddOutcome::Rejected(reason) => {
                eprintln!("clipway: dropped clipboard entry: {reason}");
                Ok(())
            }
        }
    }

    /// Clears history. Pinned entries are kept unless `keep_pinned` is false.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub async fn clear(&self, keep_pinned: bool) -> Result<()> {
        if keep_pinned {
            self.store.clear_unpinned()?;
        } else {
            self.store.clear()?;
        }
        self.history_changed().await;
        Ok(())
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        self.store.set_pinned(id, pinned)
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn delete(&self, id: i64) -> Result<bool> {
        self.store.delete(id)
    }

    /// Applies "clear history on logout". `$XDG_RUNTIME_DIR` is emptied when
    /// the user's last session ends and at reboot, so a missing marker there
    /// means this is the first start of a new session. Unlike watching for
    /// logout signals, this also covers crashes and power loss, and cannot
    /// be triggered by another process.
    pub fn start_session(&self) {
        let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return;
        };
        let marker = std::path::PathBuf::from(runtime)
            .join("clipway")
            .join("session-started");
        if marker.exists() {
            return;
        }
        if self.snapshot().clear_on_logout {
            match self.store.clear_unpinned() {
                Ok(()) => eprintln!("clipway: new session; cleared unpinned history"),
                Err(error) => {
                    eprintln!("clipway: clearing history for a new session failed: {error}")
                }
            }
        }
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&marker, b"") {
            eprintln!("clipway: could not write {}: {error}", marker.display());
        }
    }

    /// Puts an entry on the clipboard through the extension. Called by the
    /// popup while its window has focus; the extension checks that.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub async fn paste(&self, id: i64) -> Result<()> {
        let entry = self
            .store
            .get(id)?
            .ok_or_else(|| anyhow!("that entry no longer exists"))?;
        anyhow::ensure!(
            is_restorable(&entry.meta.mime),
            "entries of type {} cannot be restored",
            entry.meta.mime
        );
        let conn = self
            .conn
            .get()
            .ok_or_else(|| anyhow!("not connected to the session bus"))?;
        let owner = verified_extension_owner(conn).await?;
        let proxy = ExtensionProxy::builder(conn)
            .destination(owner)?
            .path(EXTENSION_OBJECT_PATH)?
            .build()
            .await?;
        proxy
            .set_clipboard(&entry.meta.mime, entry.data)
            .await
            .context("the GNOME Shell extension refused the clipboard update")?;
        Ok(())
    }

    async fn history_changed(&self) {
        if let Some(hook) = self.on_change.get() {
            hook();
        }
        let Some(conn) = self.conn.get() else {
            return;
        };
        let Ok(iface) = conn
            .object_server()
            .interface::<_, Manager>(OBJECT_PATH)
            .await
        else {
            return;
        };
        if let Err(error) = Manager::history_changed(iface.signal_emitter()).await {
            eprintln!("clipway: emitting HistoryChanged failed: {error}");
        }
    }
}

/// True when `pid` runs the root-owned GNOME Shell binary as our own user.
fn is_gnome_shell(pid: u32, uid: Option<u32>) -> bool {
    if uid != Some(current_uid()) {
        return false;
    }
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let named_shell = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("gnome-shell"));
    // Only root can create a root-owned file, so a same-user process cannot
    // fake this with its own copy.
    let root_owned = std::fs::metadata(&exe).is_ok_and(|meta| meta.uid() == 0);
    named_shell && root_owned
}

fn current_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(u32::MAX)
}

/// Development escape hatch for exercising the API with `busctl`/`gdbus`.
/// Compiled out of release builds.
fn allow_any_caller() -> bool {
    cfg!(debug_assertions) && std::env::var_os("CLIPWAY_ALLOW_ANY_CALLER").is_some()
}

async fn peer_is_gnome_shell(conn: &Connection, peer: &UniqueName<'_>) -> Result<bool> {
    let dbus = fdo::DBusProxy::new(conn).await?;
    let credentials = dbus
        .get_connection_credentials(BusName::Unique(peer.to_owned()))
        .await?;
    let Some(pid) = credentials.process_id() else {
        return Ok(false);
    };
    Ok(is_gnome_shell(pid, credentials.unix_user_id()))
}

async fn verified_extension_owner(conn: &Connection) -> Result<zbus::names::OwnedUniqueName> {
    let dbus = fdo::DBusProxy::new(conn).await?;
    let owner = dbus
        .get_name_owner(BusName::try_from(EXTENSION_BUS_NAME)?)
        .await
        .map_err(|_| anyhow!("the Clipway GNOME Shell extension is not running"))?;
    anyhow::ensure!(
        allow_any_caller() || peer_is_gnome_shell(conn, &owner).await?,
        "{EXTENSION_BUS_NAME} is not owned by GNOME Shell; refusing to send clipboard data"
    );
    Ok(owner)
}

pub struct Manager {
    bridge: Arc<Bridge>,
    /// Unique names already verified. Unique names are never reused on a
    /// bus, so a cached answer cannot go stale.
    trusted: Mutex<HashSet<String>>,
}

impl Manager {
    async fn authorize(&self, header: &Header<'_>, conn: &Connection) -> fdo::Result<()> {
        if allow_any_caller() {
            return Ok(());
        }
        let sender = header
            .sender()
            .ok_or_else(|| fdo::Error::AccessDenied("no sender".into()))?;
        if self
            .trusted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains(sender.as_str())
        {
            return Ok(());
        }
        let allowed = peer_is_gnome_shell(conn, sender)
            .await
            .map_err(|error| fdo::Error::Failed(error.to_string()))?;
        if !allowed {
            return Err(fdo::Error::AccessDenied(
                "only the Clipway GNOME Shell extension may use this interface".into(),
            ));
        }
        self.trusted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(sender.to_string());
        Ok(())
    }
}

#[interface(name = "io.clipway.ClipboardManager1")]
impl Manager {
    async fn add_entry(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        mime: &str,
        data: Vec<u8>,
        source_app: &str,
    ) -> fdo::Result<()> {
        self.authorize(&header, conn).await?;
        self.bridge.add_entry(mime, &data, source_app).await
    }

    async fn get_recent(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        limit: u32,
    ) -> fdo::Result<Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>> {
        self.authorize(&header, conn).await?;
        self.bridge
            .recent(limit.clamp(1, RECENT_MAX))
            .map(|entries| entries.iter().map(recent_row).collect())
            .map_err(|error| fdo::Error::Failed(format!("{error}")))
    }

    /// Returns an entry's payload so the extension can restore it itself
    /// (used by the panel menu).
    async fn get_entry(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        id: OwnedObjectPath,
    ) -> fdo::Result<(String, Vec<u8>)> {
        self.authorize(&header, conn).await?;
        let id = parse_entry_id(id.as_str())
            .ok_or_else(|| fdo::Error::InvalidArgs("not a Clipway entry path".into()))?;
        let entry = self
            .bridge
            .store
            .get(id)
            .map_err(|error| fdo::Error::Failed(format!("{error}")))?
            .ok_or_else(|| fdo::Error::Failed("no such entry".into()))?;
        if !is_restorable(&entry.meta.mime) {
            return Err(fdo::Error::NotSupported(format!(
                "entries of type {} cannot be restored",
                entry.meta.mime
            )));
        }
        Ok((entry.meta.mime, entry.data))
    }

    #[zbus(signal)]
    async fn history_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn recent_row(meta: &EntryMeta) -> (OwnedObjectPath, HashMap<String, OwnedValue>) {
    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), owned(meta.kind.as_i64()));
    fields.insert("preview".to_string(), owned(meta.preview.clone()));
    fields.insert("source".to_string(), owned(meta.source.clone()));
    fields.insert("pinned".to_string(), owned(meta.pinned));
    (entry_path(meta.id), fields)
}

fn owned<T: Into<Value<'static>>>(value: T) -> OwnedValue {
    OwnedValue::try_from(value.into()).expect("variant conversion")
}

async fn build_service(bridge: Arc<Bridge>) -> Result<Connection> {
    let manager = Manager {
        bridge,
        trusted: Mutex::new(HashSet::new()),
    };
    let conn = connection::Builder::session()
        .context("connecting to the session bus")?
        .name(BUS_NAME)
        .context("requesting the Clipway bus name")?
        .serve_at(OBJECT_PATH, manager)
        .context("serving the Clipway interface")?
        .build()
        .await
        .context("building the session connection (is another clipway-daemon running?)")?;
    Ok(conn)
}

pub fn spawn_service(bridge: Arc<Bridge>) {
    let spawned = std::thread::Builder::new()
        .name("clipway-dbus".into())
        .spawn(
            move || match zbus::block_on(build_service(bridge.clone())) {
                Ok(conn) => {
                    bridge.attach_connection(&conn);
                    zbus::block_on(std::future::pending::<()>());
                }
                Err(error) => eprintln!("clipway: D-Bus service failed to start: {error:#}"),
            },
        );
    if let Err(error) = spawned {
        eprintln!("clipway: could not spawn the D-Bus thread: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_round_trip() {
        let path = entry_path(42);
        assert_eq!(parse_entry_id(path.as_str()), Some(42));
        assert_eq!(parse_entry_id("/io/clipway/other/42"), None);
    }

    #[test]
    fn this_test_process_is_not_gnome_shell() {
        assert!(!is_gnome_shell(std::process::id(), Some(current_uid())));
        assert!(!is_gnome_shell(1, Some(0)));
    }
}

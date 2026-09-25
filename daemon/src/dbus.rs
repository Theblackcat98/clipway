use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MessageStream, connection, fdo, interface, proxy};

#[cfg(feature = "gui")]
use gtk::gdk::prelude::DisplayExt;

use crate::model::{Entry, EntryMeta, classify, is_sensitive, max_bytes_for};
use crate::settings::SettingsSnapshot;
use crate::store::{AddOptions, AddOutcome, Store};

pub const BUS_NAME: &str = "io.clipway.ClipboardManager";
pub const OBJECT_PATH: &str = "/io/clipway/ClipboardManager";
pub const ENTRY_PATH_PREFIX: &str = "/io/clipway/entries";
pub const EXTENSION_BUS_NAME: &str = "io.clipway.Extension";
pub const EXTENSION_OBJECT_PATH: &str = "/io/clipway/Extension";

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

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub struct Bridge {
    store: Arc<Store>,
    settings: Mutex<SettingsSnapshot>,
    conn: OnceLock<Connection>,
}

impl Bridge {
    pub fn new(store: Arc<Store>, settings: SettingsSnapshot) -> Self {
        Self {
            store,
            settings: Mutex::new(settings),
            conn: OnceLock::new(),
        }
    }

    pub fn attach_connection(&self, conn: &Connection) {
        let _ = self.conn.set(conn.clone());
    }

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

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<EntryMeta>> {
        self.store.recent(limit)
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<EntryMeta>> {
        self.store.search(query, limit)
    }

    pub async fn add_entry(&self, mime: &str, data: Vec<u8>, source_app: &str) -> fdo::Result<()> {
        if is_sensitive(mime) {
            return Ok(());
        }
        let Some(kind) = classify(mime) else {
            return Ok(());
        };
        let settings = self.snapshot();
        if settings.incognito || settings.is_excluded(source_app) {
            return Ok(());
        }
        let options = AddOptions {
            max_bytes: max_bytes_for(kind, &settings),
            depth: settings.history_depth,
            now: now_secs(),
        };
        let outcome = self
            .store
            .add(kind, mime, data, source_app, &options)
            .map_err(|error| fdo::Error::Failed(format!("{error}")))?;
        match outcome {
            AddOutcome::Added(_) | AddOutcome::Duplicate(_) => self
                .emit_history_changed()
                .await
                .map_err(|error| fdo::Error::Failed(error.to_string())),
            AddOutcome::Rejected(reason) => {
                eprintln!("clipway: dropped clipboard entry: {reason}");
                Ok(())
            }
        }
    }

    pub async fn clear(&self, keep_pinned: bool) -> Result<()> {
        if keep_pinned {
            self.store.clear_unpinned()?;
        } else {
            self.store.clear()?;
        }
        self.emit_history_changed().await.ok();
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        self.store.set_pinned(id, pinned)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        self.store.delete(id)
    }

    pub fn on_session_end(&self) {
        if !self.snapshot().clear_on_logout {
            return;
        }
        if let Err(error) = self.store.clear_unpinned() {
            eprintln!("clipway: clearing history on logout failed: {error}");
        }
    }

    pub async fn paste(&self, id: i64) -> bool {
        let Ok(Some(entry)) = self.store.get(id) else {
            return false;
        };
        if self.set_via_extension(&entry).await {
            return true;
        }
        self.set_via_gtk(&entry)
    }

    async fn set_via_extension(&self, entry: &Entry) -> bool {
        let Some(conn) = self.conn.get() else {
            return false;
        };
        let Ok(builder) = ExtensionProxy::builder(conn)
            .destination(EXTENSION_BUS_NAME)
            .and_then(|builder| builder.path(EXTENSION_OBJECT_PATH))
        else {
            return false;
        };
        let Ok(proxy) = builder.build().await else {
            return false;
        };
        proxy
            .set_clipboard(&entry.meta.mime, entry.data.clone())
            .await
            .is_ok()
    }

    fn set_via_gtk(&self, entry: &Entry) -> bool {
        #[cfg(feature = "gui")]
        {
            let mime = entry.meta.mime.clone();
            let data = entry.data.clone();
            glib::idle_add_local_once(move || {
                let Some(display) = gtk::gdk::Display::default() else {
                    return;
                };
                let clipboard = display.clipboard();
                let bytes = glib::Bytes::from(data.as_slice());
                let provider = gtk::gdk::ContentProvider::for_bytes(&mime, &bytes);
                let _ = clipboard.set_content(Some(&provider));
            });
            true
        }
        #[cfg(not(feature = "gui"))]
        {
            let _ = entry;
            false
        }
    }

    async fn emit_history_changed(&self) -> zbus::Result<()> {
        let Some(conn) = self.conn.get() else {
            return Ok(());
        };
        let iface = conn
            .object_server()
            .interface::<_, Manager>(OBJECT_PATH)
            .await?;
        ManagerSignals::history_changed(&iface).await
    }
}

pub struct Manager {
    bridge: Arc<Bridge>,
}

#[interface(name = "io.clipway.ClipboardManager1")]
impl Manager {
    async fn add_entry(&self, mime: &str, data: Vec<u8>, source_app: &str) -> fdo::Result<()> {
        self.bridge.add_entry(mime, data, source_app).await
    }

    async fn get_recent(
        &self,
        limit: u32,
    ) -> fdo::Result<Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>> {
        self.bridge
            .recent(limit.clamp(1, 50))
            .map(|entries| entries.iter().map(recent_row).collect())
            .map_err(|error| fdo::Error::Failed(format!("{error}")))
    }

    async fn paste_entry(&self, id: OwnedObjectPath) -> fdo::Result<bool> {
        let Some(id) = parse_entry_id(id.as_str()) else {
            return Ok(false);
        };
        Ok(self.bridge.paste(id).await)
    }

    async fn clear_history(&self) -> fdo::Result<()> {
        self.bridge
            .clear(false)
            .await
            .map_err(|error| fdo::Error::Failed(format!("{error}")))
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
    let conn = connection::Builder::session()
        .context("connecting to the session bus")?
        .name(BUS_NAME)
        .context("requesting the Clipway bus name")?
        .serve_at(OBJECT_PATH, Manager { bridge })
        .context("serving the Clipway interface")?
        .build()
        .await
        .context("building the session connection")?;
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

fn watch_signals(rule: zbus::MatchRule<'static>, bridge: Arc<Bridge>, conn: Connection) {
    let _ = std::thread::Builder::new()
        .name("clipway-session-watch".into())
        .spawn(move || {
            zbus::block_on(async move {
                let Ok(mut stream) = MessageStream::for_match_rule(rule, &conn, None).await else {
                    return;
                };
                while let Some(_message) = futures_lite::StreamExt::next(&mut stream).await {
                    bridge.on_session_end();
                }
                std::future::pending::<()>().await;
            });
        });
}

pub fn spawn_session_watchers(bridge: Arc<Bridge>) {
    match zbus::block_on(Connection::session()) {
        Ok(conn) => {
            let Ok(rule) = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender("org.gnome.SessionManager")
                .and_then(|builder| builder.interface("org.gnome.SessionManager.EndSessionDialog"))
                .map(|builder| builder.build())
            else {
                return;
            };
            watch_signals(rule, bridge.clone(), conn);
        }
        Err(error) => eprintln!("clipway: cannot watch the session bus: {error}"),
    }

    match zbus::block_on(Connection::system()) {
        Ok(conn) => {
            let Ok(rule) = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.login1.Manager")
                .and_then(|builder| builder.member("PrepareForShutdown"))
                .map(|builder| builder.build())
            else {
                return;
            };
            watch_signals(rule, bridge, conn);
        }
        Err(error) => eprintln!("clipway: cannot watch the system bus: {error}"),
    }
}

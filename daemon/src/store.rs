use std::fmt;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::crypto::hex_encode;
use crate::model::{
    Entry, EntryMeta, Kind, Normalized, content_hash, normalize, preview, search_text, thumbnail,
};

/// Current on-disk schema version (`PRAGMA user_version`).
const SCHEMA_VERSION: i64 = 2;

/// Timestamps are milliseconds, and `id` breaks ties, so two copies made in
/// the same millisecond still come back newest-first.
const CREATE_V2: &str = "
CREATE TABLE entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind INTEGER NOT NULL,
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    text TEXT,
    preview TEXT NOT NULL DEFAULT '',
    thumb BLOB,
    hash TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT '',
    ts INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX entries_hash ON entries (hash);
CREATE INDEX entries_recent ON entries (pinned DESC, ts DESC, id DESC);
";

/// Listing never selects `data`: previews and thumbnails are precomputed.
const SELECT_META: &str = "SELECT id, kind, mime, preview, source, ts, pinned, thumb FROM entries";
const ORDER_RECENT: &str = "ORDER BY pinned DESC, ts DESC, id DESC";

pub struct AddOptions {
    pub max_bytes: usize,
    pub depth: u32,
    /// Milliseconds since the Unix epoch.
    pub now: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added(i64),
    Duplicate(i64),
    Rejected(&'static str),
}

/// The key does not decrypt the database (wrong key, or not a Clipway file).
#[derive(Debug)]
pub struct WrongKey;

impl fmt::Display for WrongKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the history database cannot be decrypted with the stored key")
    }
}

impl std::error::Error for WrongKey {}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &std::path::Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        restrict_permissions(path);
        apply_key(&conn, key)?;
        Self::migrate(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory(key: &[u8]) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        apply_key(&conn, key)?;
        Self::migrate(conn)
    }

    fn migrate(mut conn: Connection) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let has_table: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'entries')",
            [],
            |row| row.get(0),
        )?;
        let tx = conn.transaction()?;
        match (has_table, version) {
            (false, _) => tx.execute_batch(CREATE_V2)?,
            (true, 1) => migrate_v1_to_v2(&tx).context("migrating history to schema v2")?,
            (true, SCHEMA_VERSION) => {}
            (true, other) => anyhow::bail!(
                "history database has schema version {other}; this Clipway understands up to {SCHEMA_VERSION}"
            ),
        }
        tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        tx.commit()?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn add(&self, item: &Normalized, source: &str, opts: &AddOptions) -> Result<AddOutcome> {
        if item.data.is_empty() {
            return Ok(AddOutcome::Rejected("empty payload"));
        }
        if item.data.len() > opts.max_bytes {
            return Ok(AddOutcome::Rejected("payload exceeds size cap"));
        }
        let hash = content_hash(item.mime, &item.data);
        let conn = self.conn();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM entries WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            conn.execute(
                "UPDATE entries SET ts = ?2, source = ?3 WHERE id = ?1",
                params![id, opts.now, source],
            )?;
            return Ok(AddOutcome::Duplicate(id));
        }
        conn.execute(
            "INSERT INTO entries (kind, mime, data, text, preview, thumb, hash, source, ts, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                item.kind.as_i64(),
                item.mime,
                &item.data,
                search_text(item.kind, &item.data),
                preview(item.kind, &item.data),
                thumbnail(item.kind, &item.data),
                hash,
                source,
                opts.now,
            ],
        )?;
        let id = conn.last_insert_rowid();
        // Depth counts unpinned entries only, so pins never crowd out new copies.
        conn.execute(
            &format!(
                "DELETE FROM entries
                 WHERE pinned = 0
                   AND id NOT IN (SELECT id FROM entries WHERE pinned = 0 {ORDER_RECENT} LIMIT ?1)"
            ),
            params![i64::from(opts.depth)],
        )?;
        Ok(AddOutcome::Added(id))
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<EntryMeta>> {
        let conn = self.conn();
        let sql = format!("{SELECT_META} {ORDER_RECENT} LIMIT ?1");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![i64::from(limit)], map_meta)?;
        collect(rows)
    }

    /// Case-insensitive substring search over text and file paths. Pinned
    /// entries always stay visible.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<EntryMeta>> {
        let conn = self.conn();
        let pattern = format!("%{}%", escape_like(query));
        let sql = format!(
            "{SELECT_META}
             WHERE pinned = 1 OR text LIKE ?1 ESCAPE '\\'
             {ORDER_RECENT}
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![pattern, i64::from(limit)], map_meta)?;
        collect(rows)
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>> {
        let conn = self.conn();
        let sql = "SELECT id, kind, mime, preview, source, ts, pinned, thumb, data FROM entries WHERE id = ?1";
        let found = conn
            .query_row(sql, params![id], |row| {
                let meta = map_meta(row)?;
                let data: Vec<u8> = row.get(8)?;
                Ok(Entry { meta, data })
            })
            .optional()?;
        Ok(found)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE entries SET pinned = ?2 WHERE id = ?1",
            params![id, i64::from(pinned)],
        )?;
        Ok(changed > 0)
    }

    pub fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.conn();
        let changed = conn.execute("DELETE FROM entries WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    /// Deletes everything, pinned entries included.
    pub fn clear(&self) -> Result<()> {
        self.conn().execute("DELETE FROM entries", [])?;
        Ok(())
    }

    pub fn clear_unpinned(&self) -> Result<()> {
        self.conn()
            .execute("DELETE FROM entries WHERE pinned = 0", [])?;
        Ok(())
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn counts(&self) -> Result<(u32, u32)> {
        let conn = self.conn();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))?;
        let pinned: i64 =
            conn.query_row("SELECT COUNT(*) FROM entries WHERE pinned = 1", [], |row| {
                row.get(0)
            })?;
        Ok((total as u32, pinned as u32))
    }
}

fn apply_key(conn: &Connection, key: &[u8]) -> Result<()> {
    anyhow::ensure!(key.len() >= 16, "database key must be at least 16 bytes");
    conn.execute_batch(&format!(
        "PRAGMA key = \"x'{}'\";
         PRAGMA cipher_memory_security = ON;",
        hex_encode(key)
    ))
    .context("applying SQLCipher key")?;
    // SQLCipher only checks the key on the first read, so read before
    // anything else can fail with a less specific error.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|_| anyhow::Error::new(WrongKey))?;
    // secure_delete overwrites deleted rows, so "Clear history" does not
    // leave old entries in free pages.
    conn.execute_batch(
        "PRAGMA secure_delete = ON;
         PRAGMA foreign_keys = ON;
         PRAGMA synchronous = NORMAL;",
    )
    .context("configuring the database")?;
    Ok(())
}

fn migrate_v1_to_v2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE entries RENAME TO entries_v1;
         DROP INDEX IF EXISTS entries_recent;",
    )?;
    tx.execute_batch(CREATE_V2)?;
    let mut select =
        tx.prepare("SELECT id, mime, data, source, ts, pinned FROM entries_v1 ORDER BY id")?;
    let rows = select.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut insert = tx.prepare(
        "INSERT INTO entries (id, kind, mime, data, text, preview, thumb, hash, source, ts, pinned)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (hash) DO UPDATE SET
             pinned = max(pinned, excluded.pinned),
             ts = max(ts, excluded.ts)",
    )?;
    for row in rows {
        let (id, mime, data, source, ts_seconds, pinned) = row?;
        // v1 stored raw X11 text types and Nautilus cut/copy markers.
        let Some(item) = normalize(&mime, &data) else {
            continue;
        };
        insert.execute(params![
            id,
            item.kind.as_i64(),
            item.mime,
            &item.data,
            search_text(item.kind, &item.data),
            preview(item.kind, &item.data),
            thumbnail(item.kind, &item.data),
            content_hash(item.mime, &item.data),
            source,
            ts_seconds.saturating_mul(1000),
            pinned,
        ])?;
    }
    drop(insert);
    drop(select);
    tx.execute_batch("DROP TABLE entries_v1;")?;
    Ok(())
}

fn create_private_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating database directory {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting permissions on {}", dir.display()))?;
    Ok(())
}

fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "clipway: could not restrict permissions on {}: {error}",
            path.display()
        );
    }
}

fn map_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntryMeta> {
    let kind_value: i64 = row.get(1)?;
    let pinned: i64 = row.get(6)?;
    Ok(EntryMeta {
        id: row.get(0)?,
        kind: Kind::from_i64(kind_value).unwrap_or(Kind::Text),
        mime: row.get(2)?,
        preview: row.get(3)?,
        source: row.get(4)?,
        ts: row.get(5)?,
        pinned: pinned != 0,
        thumb: row.get(7)?,
    })
}

fn collect<I>(rows: I) -> Result<Vec<EntryMeta>>
where
    I: Iterator<Item = rusqlite::Result<EntryMeta>>,
{
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TEXT_MIME, URI_LIST_MIME};

    const KEY: [u8; 32] = [0x42; 32];

    fn store() -> Store {
        Store::open_in_memory(&KEY).expect("open in-memory store")
    }

    fn options(now: i64, depth: u32) -> AddOptions {
        AddOptions {
            max_bytes: 1024,
            depth,
            now,
        }
    }

    fn add(store: &Store, mime: &str, data: &[u8], now: i64) -> AddOutcome {
        add_with_depth(store, mime, data, now, 10)
    }

    fn add_with_depth(store: &Store, mime: &str, data: &[u8], now: i64, depth: u32) -> AddOutcome {
        let item = normalize(mime, data).expect("supported mime");
        store
            .add(&item, "app", &options(now, depth))
            .expect("add entry")
    }

    fn previews(store: &Store) -> Vec<String> {
        store
            .recent(50)
            .unwrap()
            .into_iter()
            .map(|meta| meta.preview)
            .collect()
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "clipway-store-{tag}-{}-{:?}",
            std::process::id(),
            std::time::Instant::now()
        ))
    }

    #[test]
    fn adds_and_reads_entries() {
        let store = store();
        let outcome = add(&store, "text/plain", b"hello", 100);
        assert!(matches!(outcome, AddOutcome::Added(_)));
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].preview, "hello");
        assert_eq!(recent[0].source, "app");
        assert_eq!(recent[0].mime, TEXT_MIME);
        let entry = store.get(recent[0].id).unwrap().unwrap();
        assert_eq!(entry.data, b"hello");
    }

    #[test]
    fn duplicate_content_moves_entry_to_top() {
        let store = store();
        add(&store, "text/plain", b"one", 100);
        add(&store, "text/plain", b"two", 200);
        let outcome = add(&store, "text/plain", b"one", 300);
        assert!(matches!(outcome, AddOutcome::Duplicate(_)));
        assert_eq!(previews(&store), ["one", "two"]);
    }

    #[test]
    fn same_text_from_x11_and_wayland_is_one_entry() {
        let store = store();
        add(&store, "UTF8_STRING", b"shared", 1);
        let outcome = add(&store, "text/plain;charset=utf-8", b"shared", 2);
        assert!(matches!(outcome, AddOutcome::Duplicate(_)));
        assert_eq!(store.counts().unwrap().0, 1);
    }

    #[test]
    fn same_millisecond_copies_list_newest_first() {
        let store = store();
        add(&store, "text/plain", b"first copy", 1000);
        add(&store, "text/plain", b"second copy", 1000);
        assert_eq!(previews(&store), ["second copy", "first copy"]);
    }

    #[test]
    fn rejects_empty_and_oversized_payloads() {
        let store = store();
        assert_eq!(
            add(&store, "text/plain", b"", 1),
            AddOutcome::Rejected("empty payload")
        );
        assert_eq!(
            add(&store, "text/plain", &[b'x'; 2048], 1),
            AddOutcome::Rejected("payload exceeds size cap")
        );
        assert_eq!(store.counts().unwrap().0, 0);
    }

    #[test]
    fn evicts_unpinned_beyond_depth_but_keeps_pinned() {
        let store = store();
        for index in 0..5 {
            add(
                &store,
                "text/plain",
                format!("item-{index}").as_bytes(),
                index,
            );
        }
        let first = store.recent(10).unwrap()[0].id;
        store.set_pinned(first, true).unwrap();
        for index in 5..15 {
            add(
                &store,
                "text/plain",
                format!("item-{index}").as_bytes(),
                index,
            );
        }
        let (total, pinned) = store.counts().unwrap();
        assert_eq!(total, 11, "depth 10 unpinned + 1 pinned");
        assert_eq!(pinned, 1);
        assert!(store.get(first).unwrap().is_some());
    }

    #[test]
    fn pins_do_not_crowd_out_new_copies() {
        let store = store();
        for index in 0..25 {
            add_with_depth(
                &store,
                "text/plain",
                format!("pin-{index}").as_bytes(),
                index,
                25,
            );
        }
        for meta in store.recent(50).unwrap() {
            store.set_pinned(meta.id, true).unwrap();
        }
        let outcome = add_with_depth(&store, "text/plain", b"new copy", 100, 25);
        let AddOutcome::Added(id) = outcome else {
            panic!("expected Added, got {outcome:?}");
        };
        assert!(store.get(id).unwrap().is_some());
        assert_eq!(store.counts().unwrap(), (26, 25));
    }

    #[test]
    fn searches_text_and_file_paths() {
        let store = store();
        add(&store, "text/plain", b"needle in a haystack", 1);
        add(
            &store,
            "x-special/gnome-copied-files",
            b"copy\nfile:///home/user/needles/report.pdf",
            2,
        );
        add(&store, "image/png", &[0x89, b'P', b'N', b'G'], 3);
        assert_eq!(store.search("needle", 10).unwrap().len(), 2);
        assert_eq!(store.search("report", 10).unwrap().len(), 1);
        assert_eq!(store.search("nothing", 10).unwrap().len(), 0);
    }

    #[test]
    fn file_entries_are_stored_as_uri_lists() {
        let store = store();
        add(
            &store,
            "x-special/gnome-copied-files",
            b"cut\nfile:///tmp/a.txt",
            1,
        );
        let meta = &store.recent(1).unwrap()[0];
        assert_eq!(meta.kind, Kind::Files);
        assert_eq!(meta.mime, URI_LIST_MIME);
        let entry = store.get(meta.id).unwrap().unwrap();
        assert_eq!(entry.data, b"file:///tmp/a.txt\r\n");
    }

    #[test]
    fn search_escapes_wildcards() {
        let store = store();
        add(&store, "text/plain", b"100% done", 1);
        add(&store, "text/plain", b"nothing here", 2);
        assert_eq!(store.search("100%", 10).unwrap().len(), 1);
        assert_eq!(store.search("%", 10).unwrap().len(), 1);
    }

    #[test]
    fn pins_deletes_and_clears() {
        let store = store();
        add(&store, "text/plain", b"keep me", 1);
        add(&store, "text/plain", b"drop me", 2);
        let ids: Vec<i64> = store.recent(10).unwrap().iter().map(|e| e.id).collect();
        assert!(store.set_pinned(ids[1], true).unwrap());
        assert!(store.delete(ids[0]).unwrap());
        assert!(!store.delete(ids[0]).unwrap());
        let remaining = store.recent(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].pinned);
        add(&store, "text/plain", b"unpinned", 3);
        store.clear_unpinned().unwrap();
        assert_eq!(store.counts().unwrap(), (1, 1));
        store.clear().unwrap();
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn migrates_v1_databases() {
        let conn = Connection::open_in_memory().unwrap();
        apply_key(&conn, &KEY).unwrap();
        conn.execute_batch(
            "CREATE TABLE entries (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, kind INTEGER NOT NULL,
                 mime TEXT NOT NULL, data BLOB NOT NULL, text TEXT,
                 source TEXT NOT NULL DEFAULT '', ts INTEGER NOT NULL,
                 pinned INTEGER NOT NULL DEFAULT 0);
             CREATE INDEX entries_recent ON entries (pinned DESC, ts DESC);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        let rows: [(i64, &str, &[u8], i64, i64); 4] = [
            (0, "UTF8_STRING", b"same text", 10, 0),
            (0, "text/plain;charset=utf-8", b"same text", 20, 1),
            (
                2,
                "x-special/gnome-copied-files",
                b"cut\nfile:///tmp/x",
                30,
                0,
            ),
            (0, "STRING", &[b'c', 0xe9], 40, 0),
        ];
        for (kind, mime, data, ts, pinned) in rows {
            conn.execute(
                "INSERT INTO entries (kind, mime, data, text, source, ts, pinned)
                 VALUES (?1, ?2, ?3, NULL, 'old', ?4, ?5)",
                params![kind, mime, data, ts, pinned],
            )
            .unwrap();
        }
        let store = Store::migrate(conn).unwrap();
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 3, "the two 'same text' rows merge");
        let merged = recent.iter().find(|m| m.preview == "same text").unwrap();
        assert!(merged.pinned);
        assert_eq!(merged.ts, 20_000, "seconds become milliseconds");
        assert!(
            recent
                .iter()
                .all(|m| m.mime != "x-special/gnome-copied-files")
        );
        assert!(recent.iter().any(|m| m.preview == "cé"));
        let version: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn wrong_key_is_reported_as_wrong_key() {
        let path = temp_path("key").with_extension("db");
        {
            let store = Store::open(&path, &[0x11; 32]).unwrap();
            add(&store, "text/plain", b"secret", 1);
        }
        let wrong = Store::open(&path, &[0x22; 32]);
        let error = wrong.err().expect("wrong key must fail");
        assert!(error.downcast_ref::<WrongKey>().is_some());
        let right = Store::open(&path, &[0x11; 32]).unwrap();
        assert_eq!(right.recent(10).unwrap().len(), 1);
        drop(right);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn database_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_path("perms");
        let path = dir.join("history.db");
        drop(Store::open(&path, &KEY).unwrap());
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

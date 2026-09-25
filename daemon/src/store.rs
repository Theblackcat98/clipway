use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::crypto::hex_encode;
use crate::model::{Entry, EntryMeta, Kind, text_of};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind INTEGER NOT NULL,
    mime TEXT NOT NULL,
    data BLOB NOT NULL,
    text TEXT,
    source TEXT NOT NULL DEFAULT '',
    ts INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS entries_recent ON entries (pinned DESC, ts DESC);
PRAGMA user_version = 1;
";

const SELECT_COLUMNS: &str = "SELECT id, kind, mime, data, source, ts, pinned FROM entries";

pub struct AddOptions {
    pub max_bytes: usize,
    pub depth: u32,
    pub now: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddOutcome {
    Added(i64),
    Duplicate(i64),
    Rejected(&'static str),
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &std::path::Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        Self::init(conn, key)
    }

    #[cfg(test)]
    pub fn open_in_memory(key: &[u8]) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, key)
    }

    fn init(conn: Connection, key: &[u8]) -> Result<Self> {
        anyhow::ensure!(key.len() >= 16, "database key must be at least 16 bytes");
        conn.execute_batch(&format!(
            "PRAGMA key = \"x'{}'\";
             PRAGMA cipher_memory_security = ON;
             PRAGMA foreign_keys = ON;
             PRAGMA synchronous = NORMAL;",
            hex_encode(key)
        ))
        .context("applying SQLCipher key")?;
        conn.execute_batch(SCHEMA).context("creating schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn add(
        &self,
        kind: Kind,
        mime: &str,
        data: Vec<u8>,
        source: &str,
        opts: &AddOptions,
    ) -> Result<AddOutcome> {
        if data.is_empty() {
            return Ok(AddOutcome::Rejected("empty payload"));
        }
        if data.len() > opts.max_bytes {
            return Ok(AddOutcome::Rejected("payload exceeds size cap"));
        }
        let conn = self.conn();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM entries WHERE kind = ?1 AND mime = ?2 AND data = ?3",
                params![kind.as_i64(), mime, &data],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            conn.execute(
                "UPDATE entries SET ts = ?2 WHERE id = ?1",
                params![id, opts.now],
            )?;
            return Ok(AddOutcome::Duplicate(id));
        }
        let text = text_of(kind, &data);
        conn.execute(
            "INSERT INTO entries (kind, mime, data, text, source, ts, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![kind.as_i64(), mime, &data, &text, source, opts.now],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "DELETE FROM entries
             WHERE pinned = 0
               AND id NOT IN (SELECT id FROM entries ORDER BY pinned DESC, ts DESC LIMIT ?1)",
            params![i64::from(opts.depth)],
        )?;
        Ok(AddOutcome::Added(id))
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<EntryMeta>> {
        let conn = self.conn();
        let sql = format!("{SELECT_COLUMNS} ORDER BY pinned DESC, ts DESC LIMIT ?1");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![i64::from(limit)], map_meta)?;
        collect(rows)
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<EntryMeta>> {
        let conn = self.conn();
        let pattern = format!("%{}%", escape_like(query));
        let sql = format!(
            "{SELECT_COLUMNS}
             WHERE pinned = 1 OR text LIKE ?1 ESCAPE '\\'
             ORDER BY pinned DESC, ts DESC
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![pattern, i64::from(limit)], map_meta)?;
        collect(rows)
    }

    pub fn get(&self, id: i64) -> Result<Option<Entry>> {
        let conn = self.conn();
        let sql = format!("{SELECT_COLUMNS} WHERE id = ?1");
        let found = conn
            .query_row(&sql, params![id], |row| {
                let meta = map_meta(row)?;
                let data: Vec<u8> = row.get(3)?;
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

    pub fn clear(&self) -> Result<()> {
        self.conn().execute("DELETE FROM entries", [])?;
        Ok(())
    }

    pub fn clear_unpinned(&self) -> Result<()> {
        self.conn()
            .execute("DELETE FROM entries WHERE pinned = 0", [])?;
        Ok(())
    }

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

fn map_meta(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntryMeta> {
    let id: i64 = row.get(0)?;
    let kind_value: i64 = row.get(1)?;
    let mime: String = row.get(2)?;
    let data: Vec<u8> = row.get(3)?;
    let source: String = row.get(4)?;
    let ts: i64 = row.get(5)?;
    let pinned: i64 = row.get(6)?;
    let kind = Kind::from_i64(kind_value).unwrap_or(Kind::Text);
    Ok(EntryMeta {
        id,
        kind,
        preview: crate::model::preview(kind, &data),
        mime,
        source,
        ts,
        pinned: pinned != 0,
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

    fn store() -> Store {
        Store::open_in_memory(&[0x42; 32]).expect("open in-memory store")
    }

    fn options(now: i64) -> AddOptions {
        AddOptions {
            max_bytes: 1024,
            depth: 10,
            now,
        }
    }

    fn add(store: &Store, kind: Kind, mime: &str, data: &[u8], now: i64) -> AddOutcome {
        store
            .add(kind, mime, data.to_vec(), "app", &options(now))
            .expect("add entry")
    }

    #[test]
    fn adds_and_reads_entries() {
        let store = store();
        let outcome = add(&store, Kind::Text, "text/plain", b"hello", 100);
        assert!(matches!(outcome, AddOutcome::Added(_)));
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].preview, "hello");
        assert_eq!(recent[0].source, "app");
        let entry = store.get(recent[0].id).unwrap().unwrap();
        assert_eq!(entry.data, b"hello");
    }

    #[test]
    fn duplicate_content_moves_entry_to_top() {
        let store = store();
        add(&store, Kind::Text, "text/plain", b"one", 100);
        add(&store, Kind::Text, "text/plain", b"two", 200);
        let outcome = add(&store, Kind::Text, "text/plain", b"one", 300);
        assert!(matches!(outcome, AddOutcome::Duplicate(_)));
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].preview, "one");
    }

    #[test]
    fn rejects_empty_and_oversized_payloads() {
        let store = store();
        assert_eq!(
            add(&store, Kind::Text, "text/plain", b"", 1),
            AddOutcome::Rejected("empty payload")
        );
        assert_eq!(
            add(&store, Kind::Text, "text/plain", &[b'x'; 2048], 1),
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
                Kind::Text,
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
                Kind::Text,
                "text/plain",
                format!("item-{index}").as_bytes(),
                index,
            );
        }
        let (total, pinned) = store.counts().unwrap();
        assert_eq!(total, 10);
        assert_eq!(pinned, 1);
        let pinned_entry = store.get(first).unwrap();
        assert!(pinned_entry.is_some());
    }

    #[test]
    fn searches_text_and_file_paths() {
        let store = store();
        add(&store, Kind::Text, "text/plain", b"needle in a haystack", 1);
        add(
            &store,
            Kind::Files,
            "x-special/gnome-copied-files",
            b"copy\n/home/user/needles/report.pdf",
            2,
        );
        add(
            &store,
            Kind::Image,
            "image/png",
            &[0x89, b'P', b'N', b'G'],
            3,
        );
        assert_eq!(store.search("needle", 10).unwrap().len(), 2);
        assert_eq!(store.search("report", 10).unwrap().len(), 1);
        assert_eq!(store.search("nothing", 10).unwrap().len(), 0);
    }

    #[test]
    fn search_escapes_wildcards() {
        let store = store();
        add(&store, Kind::Text, "text/plain", b"100% done", 1);
        add(&store, Kind::Text, "text/plain", b"nothing here", 2);
        assert_eq!(store.search("100%", 10).unwrap().len(), 1);
        assert_eq!(store.search("%", 10).unwrap().len(), 1);
    }

    #[test]
    fn pins_deletes_and_clears() {
        let store = store();
        add(&store, Kind::Text, "text/plain", b"keep me", 1);
        add(&store, Kind::Text, "text/plain", b"drop me", 2);
        let ids: Vec<i64> = store.recent(10).unwrap().iter().map(|e| e.id).collect();
        assert!(store.set_pinned(ids[0], true).unwrap());
        assert!(store.delete(ids[1]).unwrap());
        assert!(!store.delete(ids[1]).unwrap());
        let remaining = store.recent(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].pinned);
        store.clear_unpinned().unwrap();
        assert_eq!(store.counts().unwrap(), (1, 1));
        store.clear().unwrap();
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn wrong_key_cannot_read_file() {
        let path = std::env::temp_dir().join(format!(
            "clipway-store-test-{}-{:?}.db",
            std::process::id(),
            std::time::Instant::now()
        ));
        {
            let store = Store::open(&path, &[0x11; 32]).unwrap();
            add(&store, Kind::Text, "text/plain", b"secret", 1);
        }
        let wrong = Store::open(&path, &[0x22; 32]);
        match wrong {
            Err(_) => {}
            Ok(store) => assert!(store.recent(10).is_err()),
        }
        let right = Store::open(&path, &[0x11; 32]).unwrap();
        assert_eq!(right.recent(10).unwrap().len(), 1);
        drop(right);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}

//! Clipboard history stored in SQLite. The daemon adds items while the popup
//! reads, pins and deletes them; WAL mode lets both processes share the file.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub use rusqlite::{Error, Result};

/// Characters of each item loaded for listing and searching. The full text is
/// only read when an item is picked, so huge copies don't slow the popup down.
pub const EXCERPT_CHARS: usize = 10_000;

/// What was copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    /// `file://` URIs.
    Files(Vec<String>),
    Image { mime_type: String, data: Vec<u8> },
}

/// The kind of [`Content`] an item holds. Stored as an integer, so the values must not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text = 0,
    Files = 1,
    Image = 2,
}

impl Kind {
    fn from_i64(value: i64) -> Self {
        match value {
            1 => Self::Files,
            2 => Self::Image,
            _ => Self::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: i64,
    pub kind: Kind,
    /// The first [`EXCERPT_CHARS`] characters of the text. For files, their
    /// URIs, one per line. Empty for images.
    pub excerpt: String,
    pub source_app: Option<String>,
    pub pinned: bool,
}

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS items (
    id           INTEGER PRIMARY KEY,
    text         TEXT    NOT NULL,
    hash         INTEGER NOT NULL,
    source_app   TEXT,
    first_copied INTEGER NOT NULL,
    last_copied  INTEGER NOT NULL,
    copy_count   INTEGER NOT NULL DEFAULT 1,
    pinned_at    INTEGER
);
CREATE INDEX IF NOT EXISTS items_hash ON items (hash);
CREATE INDEX IF NOT EXISTS items_last_copied ON items (last_copied);
PRAGMA user_version = 1;
";

/// Images and files. Text items keep an empty `data`; images keep an empty `text`.
const MIGRATION_V2: &str = "
ALTER TABLE items ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;
ALTER TABLE items ADD COLUMN mime_type TEXT;
ALTER TABLE items ADD COLUMN data BLOB;
ALTER TABLE items ADD COLUMN thumbnail BLOB;
PRAGMA user_version = 2;
";

pub struct History {
    conn: Connection,
}

impl History {
    /// Opens the history at its default location, creating the directory if needed.
    pub fn open_default() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let path = crate::history_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self::open(&path)?)
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Read inside the transaction, so the daemon and the popup can't both migrate.
        let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 1 {
            tx.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            tx.execute_batch(MIGRATION_V2)?;
        }
        tx.commit()?;
        Ok(Self { conn })
    }

    /// Records a copy. Copying something that is already in the history moves
    /// it to the top. The oldest unpinned items beyond `history_size` are
    /// removed.
    pub fn add(&mut self, content: &Content, source_app: Option<&str>, history_size: usize) -> Result<i64> {
        let (kind, text, mime_type, data) = match content {
            Content::Text(text) => (Kind::Text, text.clone(), None, &[][..]),
            Content::Files(uris) => (Kind::Files, uris.join("\n"), None, &[][..]),
            Content::Image { mime_type, data } => (Kind::Image, String::new(), Some(mime_type.as_str()), &data[..]),
        };
        let hash = content_hash(text.as_bytes()) ^ content_hash(data);
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = next_timestamp(&tx)?;

        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM items
                 WHERE hash = ?1 AND kind = ?2 AND text = ?3 AND COALESCE(data, x'') = ?4",
                params![hash, kind as i64, text, data],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE items
                     SET last_copied = ?1, copy_count = copy_count + 1, source_app = COALESCE(?2, source_app)
                     WHERE id = ?3",
                    params![now, source_app, id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO items (kind, text, mime_type, data, hash, source_app, first_copied, last_copied)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![kind as i64, text, mime_type, (!data.is_empty()).then_some(data), hash, source_app, now],
                )?;
                tx.last_insert_rowid()
            }
        };

        tx.execute(
            "DELETE FROM items WHERE pinned_at IS NULL AND id NOT IN (
                 SELECT id FROM items WHERE pinned_at IS NULL ORDER BY last_copied DESC LIMIT ?1
             )",
            params![i64::try_from(history_size).unwrap_or(i64::MAX)],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Moves an item to the top, as if it had just been copied again.
    pub fn touch(&self, id: i64) -> Result<()> {
        // One statement, so it's atomic without a transaction. Increasing like `next_timestamp`.
        self.conn.execute(
            "UPDATE items
             SET last_copied = MAX(?1, (SELECT COALESCE(MAX(last_copied), 0) + 1 FROM items)),
                 copy_count = copy_count + 1
             WHERE id = ?2",
            params![now_millis(), id],
        )?;
        Ok(())
    }

    /// All items: pinned ones first in the order they were pinned, then the
    /// rest from most to least recently copied.
    pub fn items(&self) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, substr(text, 1, ?1), source_app, pinned_at IS NOT NULL
             FROM items
             ORDER BY pinned_at IS NULL, pinned_at, last_copied DESC",
        )?;
        stmt.query_map(params![EXCERPT_CHARS as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                kind: Kind::from_i64(row.get(1)?),
                excerpt: row.get(2)?,
                source_app: row.get(3)?,
                pinned: row.get(4)?,
            })
        })?
        .collect()
    }

    /// The full content of an item, or `None` if it no longer exists.
    pub fn content(&self, id: i64) -> Result<Option<Content>> {
        self.conn
            .query_row(
                "SELECT kind, text, mime_type, data FROM items WHERE id = ?1",
                params![id],
                |row| {
                    Ok(match Kind::from_i64(row.get(0)?) {
                        Kind::Text => Content::Text(row.get(1)?),
                        Kind::Files => Content::Files(row.get::<_, String>(1)?.lines().map(str::to_owned).collect()),
                        Kind::Image => Content::Image {
                            mime_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                            data: row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                        },
                    })
                },
            )
            .optional()
    }

    /// The PNG thumbnail of an image item, if it has one.
    pub fn thumbnail(&self, id: i64) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row("SELECT thumbnail FROM items WHERE id = ?1", params![id], |row| row.get(0))
            .optional()
            .map(Option::flatten)
    }

    pub fn set_thumbnail(&self, id: i64, png: &[u8]) -> Result<()> {
        self.conn
            .execute("UPDATE items SET thumbnail = ?1 WHERE id = ?2", params![png, id])?;
        Ok(())
    }

    /// Frees SQLite's cache, which grows while large images are written.
    pub fn release_memory(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA shrink_memory")
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE items SET pinned_at = CASE WHEN ?2 THEN COALESCE(pinned_at, ?3) END WHERE id = ?1",
            params![id, pinned, now_millis()],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Removes all unpinned items, or every item when `include_pinned` is set.
    pub fn clear(&self, include_pinned: bool) -> Result<usize> {
        if include_pinned {
            self.conn.execute("DELETE FROM items", [])
        } else {
            self.conn.execute("DELETE FROM items WHERE pinned_at IS NULL", [])
        }
    }
}

/// Strictly increasing, so copies within the same millisecond keep their order.
fn next_timestamp(tx: &Transaction) -> Result<i64> {
    let latest: i64 = tx.query_row("SELECT COALESCE(MAX(last_copied), 0) FROM items", [], |row| row.get(0))?;
    Ok(now_millis().max(latest + 1))
}

/// 64-bit FNV-1a. Only used to find duplicates quickly, so it just needs to be
/// stable across builds (unlike `std`'s hasher). Empty input hashes to 0, so
/// text items keep the hashes they had before images existed.
fn content_hash(bytes: &[u8]) -> i64 {
    if bytes.is_empty() {
        return 0;
    }
    let hash = bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
    hash as i64
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Content {
        Content::Text(text.to_owned())
    }

    fn image(data: &[u8]) -> Content {
        Content::Image { mime_type: "image/png".to_owned(), data: data.to_vec() }
    }

    fn texts(history: &History) -> Vec<String> {
        history.items().unwrap().into_iter().map(|item| item.excerpt).collect()
    }

    #[test]
    fn newest_first_and_duplicates_move_to_top() {
        let mut history = History::open_in_memory().unwrap();
        history.add(&text("one"), Some("a.desktop"), 10).unwrap();
        history.add(&text("two"), None, 10).unwrap();
        history.add(&text("one"), None, 10).unwrap();
        assert_eq!(texts(&history), ["one", "two"]);
        assert_eq!(history.items().unwrap()[0].source_app.as_deref(), Some("a.desktop"));
    }

    #[test]
    fn trims_unpinned_items_only() {
        let mut history = History::open_in_memory().unwrap();
        let pinned = history.add(&text("pinned"), None, 2).unwrap();
        history.set_pinned(pinned, true).unwrap();
        for item in ["a", "b", "c"] {
            history.add(&text(item), None, 2).unwrap();
        }
        assert_eq!(texts(&history), ["pinned", "c", "b"]);
    }

    #[test]
    fn pinned_items_keep_pin_order() {
        let mut history = History::open_in_memory().unwrap();
        let first = history.add(&text("first"), None, 10).unwrap();
        let second = history.add(&text("second"), None, 10).unwrap();
        history.add(&text("third"), None, 10).unwrap();
        history.set_pinned(first, true).unwrap();
        history.set_pinned(second, true).unwrap();
        history.add(&text("first"), None, 10).unwrap();
        assert_eq!(texts(&history), ["first", "second", "third"]);

        history.set_pinned(first, false).unwrap();
        assert_eq!(texts(&history), ["second", "first", "third"]);
    }

    #[test]
    fn excerpt_is_truncated_but_content_is_complete() {
        let mut history = History::open_in_memory().unwrap();
        let long = "é".repeat(EXCERPT_CHARS + 5);
        let id = history.add(&text(&long), None, 10).unwrap();
        assert_eq!(history.items().unwrap()[0].excerpt.chars().count(), EXCERPT_CHARS);
        assert_eq!(history.content(id).unwrap(), Some(text(&long)));
    }

    #[test]
    fn stores_files_and_images() {
        let mut history = History::open_in_memory().unwrap();
        let files = Content::Files(vec!["file:///a%20b".to_owned(), "file:///c".to_owned()]);
        let files_id = history.add(&files, None, 10).unwrap();
        let image_id = history.add(&image(b"\x89PNG pixels"), None, 10).unwrap();

        let items = history.items().unwrap();
        assert_eq!((items[0].kind, items[0].excerpt.as_str()), (Kind::Image, ""));
        assert_eq!((items[1].kind, items[1].excerpt.as_str()), (Kind::Files, "file:///a%20b\nfile:///c"));
        assert_eq!(history.content(files_id).unwrap(), Some(files));
        assert_eq!(history.content(image_id).unwrap(), Some(image(b"\x89PNG pixels")));

        assert_eq!(history.thumbnail(image_id).unwrap(), None);
        history.set_thumbnail(image_id, b"small").unwrap();
        assert_eq!(history.thumbnail(image_id).unwrap().as_deref(), Some(&b"small"[..]));
    }

    #[test]
    fn duplicates_are_per_kind_and_content() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add(&image(b"a"), None, 10).unwrap();
        history.add(&image(b"b"), None, 10).unwrap();
        history.add(&text("file:///a"), None, 10).unwrap();
        history.add(&Content::Files(vec!["file:///a".to_owned()]), None, 10).unwrap();
        assert_eq!(history.add(&image(b"a"), None, 10).unwrap(), a);
        assert_eq!(history.items().unwrap().len(), 4);
        assert_eq!(history.items().unwrap()[0].id, a);
    }

    #[test]
    fn touch_moves_an_item_to_the_top() {
        let mut history = History::open_in_memory().unwrap();
        let first = history.add(&text("first"), Some("a.desktop"), 10).unwrap();
        history.add(&text("second"), None, 10).unwrap();
        history.touch(first).unwrap();
        assert_eq!(texts(&history), ["first", "second"]);
        assert_eq!(history.items().unwrap()[0].source_app.as_deref(), Some("a.desktop"));
    }

    #[test]
    fn delete_and_clear() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add(&text("a"), None, 10).unwrap();
        let b = history.add(&text("b"), None, 10).unwrap();
        history.add(&text("c"), None, 10).unwrap();
        history.set_pinned(b, true).unwrap();

        history.delete(a).unwrap();
        assert_eq!(history.content(a).unwrap(), None);
        assert_eq!(history.clear(false).unwrap(), 1);
        assert_eq!(texts(&history), ["b"]);
        assert_eq!(history.clear(true).unwrap(), 1);
        assert!(texts(&history).is_empty());
    }

    fn temp_db(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("laccy-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        (dir, path)
    }

    #[test]
    fn reopening_a_database_keeps_items() {
        let (dir, path) = temp_db("reopen");
        History::open(&path).unwrap().add(&text("kept"), None, 10).unwrap();
        assert_eq!(texts(&History::open(&path).unwrap()), ["kept"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrates_a_text_only_database() {
        let (dir, path) = temp_db("migrate");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                "INSERT INTO items (text, hash, first_copied, last_copied) VALUES ('old', ?1, 1, 1)",
                params![content_hash(b"old")],
            )
            .unwrap();
        }
        let mut history = History::open(&path).unwrap();
        let items = history.items().unwrap();
        assert_eq!((items[0].kind, items[0].excerpt.as_str()), (Kind::Text, "old"));
        // The old row's hash still finds it.
        assert_eq!(history.add(&text("old"), None, 10).unwrap(), items[0].id);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

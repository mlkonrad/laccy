//! Clipboard history stored in SQLite. The daemon adds items while the popup
//! reads, pins and deletes them; WAL mode lets both processes share the file.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub use rusqlite::{Error, Result};

/// Characters of each item loaded for listing and searching. The full text is
/// only read when an item is picked, so huge copies don't slow the popup down.
pub const EXCERPT_CHARS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: i64,
    /// The first [`EXCERPT_CHARS`] characters of the text.
    pub excerpt: String,
    pub source_app: Option<String>,
    pub pinned: bool,
}

const SCHEMA: &str = "
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
        let version: i32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(SCHEMA)?;
            tx.commit()?;
        }
        // Future schema migrations go here, keyed on `version`.
        Ok(Self { conn })
    }

    /// Records a copy of `text`. Copying text that is already in the history
    /// moves it to the top. The oldest unpinned items beyond `history_size` are
    /// removed.
    pub fn add(&mut self, text: &str, source_app: Option<&str>, history_size: usize) -> Result<i64> {
        let hash = text_hash(text);
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Strictly increasing, so copies within the same millisecond keep their order.
        let latest: i64 = tx.query_row("SELECT COALESCE(MAX(last_copied), 0) FROM items", [], |row| row.get(0))?;
        let now = now_millis().max(latest + 1);

        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM items WHERE hash = ?1 AND text = ?2",
                params![hash, text],
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
                    "INSERT INTO items (text, hash, source_app, first_copied, last_copied)
                     VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![text, hash, source_app, now],
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

    /// All items: pinned ones first in the order they were pinned, then the
    /// rest from most to least recently copied.
    pub fn items(&self) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, substr(text, 1, ?1), source_app, pinned_at IS NOT NULL
             FROM items
             ORDER BY pinned_at IS NULL, pinned_at, last_copied DESC",
        )?;
        stmt.query_map(params![EXCERPT_CHARS as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                excerpt: row.get(1)?,
                source_app: row.get(2)?,
                pinned: row.get(3)?,
            })
        })?
        .collect()
    }

    /// The full text of an item, or `None` if it no longer exists.
    pub fn text(&self, id: i64) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT text FROM items WHERE id = ?1", params![id], |row| row.get(0))
            .optional()
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

/// 64-bit FNV-1a. Only used to find duplicates quickly, so it just needs to be
/// stable across builds (unlike `std`'s hasher).
fn text_hash(text: &str) -> i64 {
    let hash = text
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3));
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

    fn texts(history: &History) -> Vec<String> {
        history.items().unwrap().into_iter().map(|item| item.excerpt).collect()
    }

    #[test]
    fn newest_first_and_duplicates_move_to_top() {
        let mut history = History::open_in_memory().unwrap();
        history.add("one", Some("a.desktop"), 10).unwrap();
        history.add("two", None, 10).unwrap();
        history.add("one", None, 10).unwrap();
        assert_eq!(texts(&history), ["one", "two"]);
        assert_eq!(history.items().unwrap()[0].source_app.as_deref(), Some("a.desktop"));
    }

    #[test]
    fn trims_unpinned_items_only() {
        let mut history = History::open_in_memory().unwrap();
        let pinned = history.add("pinned", None, 2).unwrap();
        history.set_pinned(pinned, true).unwrap();
        for text in ["a", "b", "c"] {
            history.add(text, None, 2).unwrap();
        }
        assert_eq!(texts(&history), ["pinned", "c", "b"]);
    }

    #[test]
    fn pinned_items_keep_pin_order() {
        let mut history = History::open_in_memory().unwrap();
        let first = history.add("first", None, 10).unwrap();
        let second = history.add("second", None, 10).unwrap();
        history.add("third", None, 10).unwrap();
        history.set_pinned(first, true).unwrap();
        history.set_pinned(second, true).unwrap();
        history.add("first", None, 10).unwrap();
        assert_eq!(texts(&history), ["first", "second", "third"]);

        history.set_pinned(first, false).unwrap();
        assert_eq!(texts(&history), ["second", "first", "third"]);
    }

    #[test]
    fn excerpt_is_truncated_but_text_is_complete() {
        let mut history = History::open_in_memory().unwrap();
        let long = "é".repeat(EXCERPT_CHARS + 5);
        let id = history.add(&long, None, 10).unwrap();
        assert_eq!(history.items().unwrap()[0].excerpt.chars().count(), EXCERPT_CHARS);
        assert_eq!(history.text(id).unwrap().as_deref(), Some(long.as_str()));
    }

    #[test]
    fn delete_and_clear() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add("a", None, 10).unwrap();
        let b = history.add("b", None, 10).unwrap();
        history.add("c", None, 10).unwrap();
        history.set_pinned(b, true).unwrap();

        history.delete(a).unwrap();
        assert_eq!(history.text(a).unwrap(), None);
        assert_eq!(history.clear(false).unwrap(), 1);
        assert_eq!(texts(&history), ["b"]);
        assert_eq!(history.clear(true).unwrap(), 1);
        assert!(texts(&history).is_empty());
    }

    #[test]
    fn reopening_a_database_keeps_items() {
        let dir = std::env::temp_dir().join(format!("laccy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.db");
        History::open(&path).unwrap().add("kept", None, 10).unwrap();
        assert_eq!(texts(&History::open(&path).unwrap()), ["kept"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

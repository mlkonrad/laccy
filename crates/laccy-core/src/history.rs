//! Clipboard history stored in SQLite. The daemon adds items while the popup
//! reads, pins and deletes them; WAL mode lets both processes share the file.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub use rusqlite::{Error, Result};

/// Characters of each item loaded for listing and searching. The full text is
/// only read when an item is picked, so huge copies don't slow the popup down.
pub const EXCERPT_CHARS: usize = 10_000;

/// The keys pinned items get, in the order they're handed out: Ctrl plus the
/// key picks the item in the popup. Letters the search field uses with Ctrl
/// (select all, copy, cut, paste, undo, redo) and Q (quit) are left out.
pub const PIN_KEYS: &str = "bdefghijklmnoprstuw";

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

/// The order of unpinned items. Pinned items stay together, in the order they
/// were pinned, at the [`PinPosition`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Most recently copied first.
    #[default]
    LastCopied,
    /// Most recently copied for the first time first.
    FirstCopied,
    /// Most copied first; ties go to the most recently copied.
    CopyCount,
}

impl SortOrder {
    /// Every order, in the order the preferences window lists them.
    pub const ALL: [Self; 3] = [Self::LastCopied, Self::FirstCopied, Self::CopyCount];

    /// The order's value in GSettings.
    pub fn name(self) -> &'static str {
        match self {
            Self::LastCopied => "last-copied",
            Self::FirstCopied => "first-copied",
            Self::CopyCount => "copy-count",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.name() == name)
    }

    fn order_by(self) -> &'static str {
        match self {
            Self::LastCopied => "last_copied DESC",
            Self::FirstCopied => "first_copied DESC, last_copied DESC",
            Self::CopyCount => "copy_count DESC, last_copied DESC",
        }
    }
}

/// Where pinned items are listed, like Maccy's "Pin to" setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PinPosition {
    /// Above the other items.
    #[default]
    Top,
    /// Below the other items.
    Bottom,
}

impl PinPosition {
    /// Every position, in the order the preferences window lists them.
    pub const ALL: [Self; 2] = [Self::Top, Self::Bottom];

    /// The position's value in GSettings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|position| position.name() == name)
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
    /// The pinned item's key from [`PIN_KEYS`]. `None` when unpinned, or when
    /// every key was taken as it was pinned.
    pub pin_key: Option<char>,
    /// The pinned item's name, shown instead of its content. `None` when
    /// unpinned or unnamed.
    pub name: Option<String>,
    /// When it was first and last copied, in milliseconds since the Unix epoch.
    pub first_copied: i64,
    pub last_copied: i64,
    /// How many times it was copied or picked.
    pub copy_count: i64,
}

/// A formatted version of a text item, like its HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub mime_type: String,
    pub data: Vec<u8>,
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

/// Formatted versions of text items. They go when their item does, or a new
/// item reusing its id would get them.
const MIGRATION_V3: &str = "
CREATE TABLE formats (
    item_id   INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
    mime_type TEXT    NOT NULL,
    data      BLOB    NOT NULL,
    PRIMARY KEY (item_id, mime_type)
);
PRAGMA user_version = 3;
";

/// A key for each pinned item, see [`PIN_KEYS`]. Items pinned before are given
/// keys by [`assign_missing_pin_keys`].
const MIGRATION_V4: &str = "
ALTER TABLE items ADD COLUMN pin_key TEXT;
PRAGMA user_version = 4;
";

/// Names for pinned items, see [`History::edit_pin`].
const MIGRATION_V5: &str = "
ALTER TABLE items ADD COLUMN name TEXT;
PRAGMA user_version = 5;
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
        // Per connection, and not inside a transaction. Deletes formats with their item.
        conn.pragma_update(None, "foreign_keys", true)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Read inside the transaction, so the daemon and the popup can't both migrate.
        let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 1 {
            tx.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            tx.execute_batch(MIGRATION_V2)?;
        }
        if version < 3 {
            tx.execute_batch(MIGRATION_V3)?;
        }
        if version < 4 {
            tx.execute_batch(MIGRATION_V4)?;
            assign_missing_pin_keys(&tx)?;
        }
        if version < 5 {
            tx.execute_batch(MIGRATION_V5)?;
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

    /// All items: pinned ones in the order they were pinned, above or below
    /// the rest as `pins` says, and the rest in `order`.
    pub fn items(&self, order: SortOrder, pins: PinPosition) -> Result<Vec<Item>> {
        let pins_first = match pins {
            PinPosition::Top => "ASC",
            PinPosition::Bottom => "DESC",
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, kind, substr(text, 1, ?1), source_app, pinned_at IS NOT NULL, first_copied, last_copied, copy_count,
                    pin_key, name
             FROM items
             ORDER BY pinned_at IS NULL {pins_first}, pinned_at, {}",
            order.order_by()
        ))?;
        stmt.query_map(params![EXCERPT_CHARS as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                kind: Kind::from_i64(row.get(1)?),
                excerpt: row.get(2)?,
                source_app: row.get(3)?,
                pinned: row.get(4)?,
                pin_key: row.get::<_, Option<String>>(8)?.and_then(|key| key.chars().next()),
                name: row.get(9)?,
                first_copied: row.get(5)?,
                last_copied: row.get(6)?,
                copy_count: row.get(7)?,
            })
        })?
        .collect()
    }

    /// The most recently copied unpinned item, if there is one.
    pub fn latest_unpinned(&self) -> Result<Option<Item>> {
        self.conn
            .query_row(
                "SELECT id, kind, substr(text, 1, ?1), source_app, first_copied, last_copied, copy_count
                 FROM items
                 WHERE pinned_at IS NULL
                 ORDER BY last_copied DESC
                 LIMIT 1",
                params![EXCERPT_CHARS as i64],
                |row| {
                    Ok(Item {
                        id: row.get(0)?,
                        kind: Kind::from_i64(row.get(1)?),
                        excerpt: row.get(2)?,
                        source_app: row.get(3)?,
                        pinned: false,
                        pin_key: None,
                        name: None,
                        first_copied: row.get(4)?,
                        last_copied: row.get(5)?,
                        copy_count: row.get(6)?,
                    })
                },
            )
            .optional()
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

    /// The formatted versions of a text item, if its copy had any.
    pub fn formats(&self, id: i64) -> Result<Vec<Format>> {
        let mut stmt = self
            .conn
            .prepare("SELECT mime_type, data FROM formats WHERE item_id = ?1 ORDER BY mime_type")?;
        stmt.query_map(params![id], |row| Ok(Format { mime_type: row.get(0)?, data: row.get(1)? }))?
            .collect()
    }

    /// Replaces the formatted versions of a text item. Don't call it for a copy
    /// without formatting, so copying just the text keeps what the item had.
    pub fn set_formats(&mut self, id: i64, formats: &[Format]) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM formats WHERE item_id = ?1", params![id])?;
        for format in formats {
            tx.execute(
                "INSERT OR REPLACE INTO formats (item_id, mime_type, data) VALUES (?1, ?2, ?3)",
                params![id, format.mime_type, format.data],
            )?;
        }
        tx.commit()
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

    /// Pins or unpins an item. A newly pinned item gets the first free key
    /// from [`PIN_KEYS`]; an unpinned one gives its key back and loses its name.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        // Immediate, so two popups can't hand out the same key. Unchecked, as
        // the popup shares the history; nothing else uses it meanwhile.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if pinned {
            // Only a newly pinned item gets a key: a pin set to no key keeps none.
            let key = free_pin_keys(&tx)?.first().map(char::to_string);
            tx.execute(
                "UPDATE items SET pinned_at = ?2, pin_key = ?3 WHERE id = ?1 AND pinned_at IS NULL",
                params![id, now_millis(), key],
            )?;
        } else {
            tx.execute("UPDATE items SET pinned_at = NULL, pin_key = NULL, name = NULL WHERE id = ?1", params![id])?;
        }
        tx.commit()
    }

    /// The keys from [`PIN_KEYS`] no pinned item has, in order.
    pub fn free_pin_keys(&self) -> Result<Vec<char>> {
        free_pin_keys(&self.conn)
    }

    /// Renames a pinned item and changes its key, and its text if `text` is
    /// given. A blank `name` removes the name; `None` for `key` leaves the item
    /// without one. A key not in [`PIN_KEYS`] is ignored, and one another item
    /// has moves to this one. New text replaces a text item's content and drops
    /// its formats, which no longer match; empty text, and text for an image or
    /// files, is ignored. Does nothing to an unpinned item.
    pub fn edit_pin(&self, id: i64, name: &str, key: Option<char>, text: Option<&str>) -> Result<()> {
        let name = Some(name.trim()).filter(|name| !name.is_empty());
        let key = key.filter(|key| PIN_KEYS.contains(*key)).map(|key| key.to_string());
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let item: Option<(Kind, String)> = tx
            .query_row(
                "SELECT kind, text FROM items WHERE id = ?1 AND pinned_at IS NOT NULL",
                params![id],
                |row| Ok((Kind::from_i64(row.get(0)?), row.get(1)?)),
            )
            .optional()?;
        let Some((kind, old_text)) = item else {
            return Ok(());
        };
        if key.is_some() {
            tx.execute("UPDATE items SET pin_key = NULL WHERE pin_key = ?1 AND id != ?2", params![key, id])?;
        }
        tx.execute("UPDATE items SET name = ?2, pin_key = ?3 WHERE id = ?1", params![id, name, key])?;
        if let Some(text) = text.filter(|text| kind == Kind::Text && !text.is_empty() && *text != old_text) {
            // Hashed like `add` hashes text, so copying the new text finds this item.
            tx.execute(
                "UPDATE items SET text = ?2, hash = ?3 WHERE id = ?1",
                params![id, text, content_hash(text.as_bytes())],
            )?;
            tx.execute("DELETE FROM formats WHERE item_id = ?1", params![id])?;
        }
        tx.commit()
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

/// Gives pinned items without a key the free keys from [`PIN_KEYS`], earliest
/// pinned first, until they run out.
fn assign_missing_pin_keys(tx: &Transaction) -> Result<()> {
    let mut free = free_pin_keys(tx)?.into_iter();
    let missing: Vec<i64> = tx
        .prepare("SELECT id FROM items WHERE pinned_at IS NOT NULL AND pin_key IS NULL ORDER BY pinned_at, id")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_>>()?;
    for (id, key) in missing.into_iter().zip(&mut free) {
        tx.execute("UPDATE items SET pin_key = ?1 WHERE id = ?2", params![key.to_string(), id])?;
    }
    Ok(())
}

/// The keys from [`PIN_KEYS`] no item has, in order.
fn free_pin_keys(conn: &Connection) -> Result<Vec<char>> {
    let used: String = conn
        .prepare("SELECT pin_key FROM items WHERE pin_key IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_>>()?;
    Ok(PIN_KEYS.chars().filter(|key| !used.contains(*key)).collect())
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

    fn html(data: &str) -> Format {
        Format { mime_type: "text/html".to_owned(), data: data.as_bytes().to_vec() }
    }

    fn texts(history: &History) -> Vec<String> {
        history.items(SortOrder::LastCopied, PinPosition::Top).unwrap().into_iter().map(|item| item.excerpt).collect()
    }

    #[test]
    fn only_formatted_copies_replace_formats() {
        let mut history = History::open_in_memory().unwrap();
        let id = history.add(&text("hello"), None, 10).unwrap();
        history.set_formats(id, &[html("<b>hello</b>")]).unwrap();
        // The daemon doesn't set formats for a plain copy of the same text.
        assert_eq!(history.add(&text("hello"), None, 10).unwrap(), id);
        assert_eq!(history.formats(id).unwrap(), [html("<b>hello</b>")]);

        history.set_formats(id, &[html("<i>hello</i>")]).unwrap();
        assert_eq!(history.formats(id).unwrap(), [html("<i>hello</i>")]);
    }

    #[test]
    fn formats_go_with_their_item() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add(&text("a"), None, 1).unwrap();
        history.set_formats(a, &[html("a")]).unwrap();
        // Trims "a".
        let b = history.add(&text("b"), None, 1).unwrap();
        history.set_formats(b, &[html("b")]).unwrap();
        history.delete(b).unwrap();

        let count: i64 = history.conn.query_row("SELECT COUNT(*) FROM formats", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        // Ids get reused once the table is empty; the new item must not inherit formats.
        let c = history.add(&text("c"), None, 1).unwrap();
        assert!(history.formats(c).unwrap().is_empty());
    }

    #[test]
    fn newest_first_and_duplicates_move_to_top() {
        let mut history = History::open_in_memory().unwrap();
        history.add(&text("one"), Some("a.desktop"), 10).unwrap();
        history.add(&text("two"), None, 10).unwrap();
        history.add(&text("one"), None, 10).unwrap();
        assert_eq!(texts(&history), ["one", "two"]);
        assert_eq!(history.items(SortOrder::LastCopied, PinPosition::Top).unwrap()[0].source_app.as_deref(), Some("a.desktop"));
    }

    #[test]
    fn sort_orders_keep_pinned_items_together() {
        let mut history = History::open_in_memory().unwrap();
        for item in ["a", "b", "c", "b", "b", "c", "a"] {
            history.add(&text(item), None, 10).unwrap();
        }
        let pinned = history.add(&text("pinned"), None, 10).unwrap();
        history.set_pinned(pinned, true).unwrap();

        let sorted = |order| -> Vec<String> {
            history.items(order, PinPosition::Top).unwrap().into_iter().map(|item| item.excerpt).collect()
        };
        assert_eq!(sorted(SortOrder::LastCopied), ["pinned", "a", "c", "b"]);
        assert_eq!(sorted(SortOrder::FirstCopied), ["pinned", "c", "b", "a"]);
        // b was copied 3 times; a and c twice, and a more recently.
        assert_eq!(sorted(SortOrder::CopyCount), ["pinned", "b", "a", "c"]);

        let at_bottom: Vec<String> = history
            .items(SortOrder::LastCopied, PinPosition::Bottom)
            .unwrap()
            .into_iter()
            .map(|item| item.excerpt)
            .collect();
        assert_eq!(at_bottom, ["a", "c", "b", "pinned"]);

        let b = &history.items(SortOrder::CopyCount, PinPosition::Top).unwrap()[1];
        assert_eq!(b.copy_count, 3);
        assert!(b.first_copied < b.last_copied);
    }

    #[test]
    fn latest_unpinned_skips_pinned_items() {
        let mut history = History::open_in_memory().unwrap();
        assert_eq!(history.latest_unpinned().unwrap(), None);
        history.add(&text("older"), None, 10).unwrap();
        let newer = history.add(&text("newer"), None, 10).unwrap();
        assert_eq!(history.latest_unpinned().unwrap().unwrap().id, newer);
        history.set_pinned(newer, true).unwrap();
        assert_eq!(history.latest_unpinned().unwrap().unwrap().excerpt, "older");
    }

    #[test]
    fn sort_order_names_round_trip() {
        for order in SortOrder::ALL {
            assert_eq!(SortOrder::from_name(order.name()), Some(order));
        }
        assert_eq!(SortOrder::from_name("random"), None);
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
        assert_eq!(history.items(SortOrder::LastCopied, PinPosition::Top).unwrap()[0].excerpt.chars().count(), EXCERPT_CHARS);
        assert_eq!(history.content(id).unwrap(), Some(text(&long)));
    }

    #[test]
    fn stores_files_and_images() {
        let mut history = History::open_in_memory().unwrap();
        let files = Content::Files(vec!["file:///a%20b".to_owned(), "file:///c".to_owned()]);
        let files_id = history.add(&files, None, 10).unwrap();
        let image_id = history.add(&image(b"\x89PNG pixels"), None, 10).unwrap();

        let items = history.items(SortOrder::LastCopied, PinPosition::Top).unwrap();
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
        assert_eq!(history.items(SortOrder::LastCopied, PinPosition::Top).unwrap().len(), 4);
        assert_eq!(history.items(SortOrder::LastCopied, PinPosition::Top).unwrap()[0].id, a);
    }

    #[test]
    fn touch_moves_an_item_to_the_top() {
        let mut history = History::open_in_memory().unwrap();
        let first = history.add(&text("first"), Some("a.desktop"), 10).unwrap();
        history.add(&text("second"), None, 10).unwrap();
        history.touch(first).unwrap();
        assert_eq!(texts(&history), ["first", "second"]);
        assert_eq!(history.items(SortOrder::LastCopied, PinPosition::Top).unwrap()[0].source_app.as_deref(), Some("a.desktop"));
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
        let items = history.items(SortOrder::LastCopied, PinPosition::Top).unwrap();
        assert_eq!((items[0].kind, items[0].excerpt.as_str()), (Kind::Text, "old"));
        // The old row's hash still finds it.
        assert_eq!(history.add(&text("old"), None, 10).unwrap(), items[0].id);
        history.set_formats(items[0].id, &[html("<b>old</b>")]).unwrap();
        assert_eq!(history.formats(items[0].id).unwrap(), [html("<b>old</b>")]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Each item's key, by text: pins made within a millisecond have no set order.
    fn pin_keys(history: &History) -> Vec<(String, Option<char>)> {
        let mut keys: Vec<_> =
            history.items(SortOrder::LastCopied, PinPosition::Top).unwrap().into_iter().map(|item| (item.excerpt, item.pin_key)).collect();
        keys.sort();
        keys
    }

    #[test]
    fn pinned_items_get_free_keys_in_order() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add(&text("a"), None, 10).unwrap();
        let b = history.add(&text("b"), None, 10).unwrap();
        let c = history.add(&text("c"), None, 10).unwrap();
        history.set_pinned(a, true).unwrap();
        history.set_pinned(b, true).unwrap();
        // Pinning again keeps the key.
        history.set_pinned(a, true).unwrap();
        assert_eq!(pin_keys(&history), [("a".into(), Some('b')), ("b".into(), Some('d')), ("c".into(), None)]);

        // Unpinning gives the key back, and the next pin takes it.
        history.set_pinned(a, false).unwrap();
        history.set_pinned(c, true).unwrap();
        assert_eq!(pin_keys(&history), [("a".into(), None), ("b".into(), Some('d')), ("c".into(), Some('b'))]);
    }

    fn pin(history: &History, id: i64) -> (Option<String>, Option<char>) {
        let item = history.items(SortOrder::LastCopied, PinPosition::Top).unwrap().into_iter().find(|item| item.id == id);
        let item = item.unwrap();
        (item.name, item.pin_key)
    }

    #[test]
    fn edit_pin_renames_and_rekeys() {
        let mut history = History::open_in_memory().unwrap();
        let a = history.add(&text("a"), None, 10).unwrap();
        let b = history.add(&text("b"), None, 10).unwrap();
        let c = history.add(&text("c"), None, 10).unwrap();
        history.set_pinned(a, true).unwrap();
        history.set_pinned(b, true).unwrap();
        assert_eq!(history.free_pin_keys().unwrap()[..2], ['e', 'f']);

        history.edit_pin(a, "  Work email ", Some('x'), None).unwrap();
        // X isn't a pin key, so A loses its key.
        assert_eq!(pin(&history, a), (Some("Work email".into()), None));
        // Taking B's key moves it.
        history.edit_pin(a, "Work email", Some('d'), None).unwrap();
        assert_eq!(pin(&history, a), (Some("Work email".into()), Some('d')));
        assert_eq!(pin(&history, b), (None, None));

        // A pin left without a key keeps none when another item is pinned.
        history.set_pinned(c, true).unwrap();
        assert_eq!(pin(&history, b), (None, None));
        assert_eq!(pin(&history, c), (None, Some('b')));

        // A blank name removes it, and unpinned items can't be edited.
        history.edit_pin(a, "  ", Some('d'), None).unwrap();
        assert_eq!(pin(&history, a), (None, Some('d')));
        history.edit_pin(a, "Again", Some('d'), None).unwrap();
        history.set_pinned(a, false).unwrap();
        assert_eq!(pin(&history, a), (None, None));
        history.edit_pin(a, "Unpinned", Some('e'), None).unwrap();
        assert_eq!(pin(&history, a), (None, None));
    }

    #[test]
    fn edit_pin_replaces_text_and_drops_formats() {
        let mut history = History::open_in_memory().unwrap();
        let id = history.add(&text("hello"), None, 10).unwrap();
        history.set_formats(id, &[html("<b>hello</b>")]).unwrap();
        history.set_pinned(id, true).unwrap();

        // The same text, or none, keeps the formatting.
        history.edit_pin(id, "Greeting", Some('b'), Some("hello")).unwrap();
        history.edit_pin(id, "Greeting", Some('b'), None).unwrap();
        assert_eq!(history.formats(id).unwrap(), [html("<b>hello</b>")]);

        // Empty text is ignored.
        history.edit_pin(id, "Greeting", Some('b'), Some("")).unwrap();
        assert_eq!(history.content(id).unwrap(), Some(text("hello")));

        history.edit_pin(id, "Greeting", Some('b'), Some("hello, world")).unwrap();
        assert_eq!(history.content(id).unwrap(), Some(text("hello, world")));
        assert!(history.formats(id).unwrap().is_empty());
        assert_eq!(pin(&history, id), (Some("Greeting".into()), Some('b')));
        // Copying the new text finds the edited item.
        assert_eq!(history.add(&text("hello, world"), None, 10).unwrap(), id);

        // An image's content can't be replaced with text.
        let image_id = history.add(&image(b"png"), None, 10).unwrap();
        history.set_pinned(image_id, true).unwrap();
        history.edit_pin(image_id, "Logo", None, Some("text")).unwrap();
        assert_eq!(history.content(image_id).unwrap(), Some(image(b"png")));
        assert_eq!(pin(&history, image_id), (Some("Logo".into()), None));
    }

    #[test]
    fn pins_beyond_the_keys_get_none() {
        let mut history = History::open_in_memory().unwrap();
        let count = PIN_KEYS.chars().count();
        for i in 0..=count {
            let id = history.add(&text(&format!("{i:02}")), None, 100).unwrap();
            history.set_pinned(id, true).unwrap();
        }
        let keys: Vec<Option<char>> = pin_keys(&history).into_iter().map(|(_, key)| key).collect();
        assert_eq!(keys[..count], PIN_KEYS.chars().map(Some).collect::<Vec<_>>()[..]);
        assert_eq!(keys[count], None);
    }

    #[test]
    fn migration_gives_existing_pins_keys_in_pin_order() {
        let (dir, path) = temp_db("migrate-pins");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute_batch(MIGRATION_V3).unwrap();
            for (text, pinned_at) in [("later", Some(20)), ("earlier", Some(10)), ("unpinned", None)] {
                conn.execute(
                    "INSERT INTO items (text, hash, first_copied, last_copied, pinned_at) VALUES (?1, 0, 1, 1, ?2)",
                    params![text, pinned_at],
                )
                .unwrap();
            }
        }
        let history = History::open(&path).unwrap();
        assert_eq!(
            pin_keys(&history),
            [("earlier".into(), Some('b')), ("later".into(), Some('d')), ("unpinned".into(), None)]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

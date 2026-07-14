use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const TRASH_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub body: String,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub cursor_start: i64,
    pub cursor_end: i64,
    pub scroll_top: f64,
    pub persisted: bool,
    pub ordinal: i64,
    pub total: i64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteInput {
    pub id: String,
    pub body: String,
    pub position: i64,
    pub created_at: i64,
    pub cursor_start: i64,
    pub cursor_end: i64,
    pub scroll_top: f64,
    pub persisted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub persisted: bool,
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSummary {
    pub id: String,
    pub snippet: String,
    pub created_at: i64,
    pub position: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedPage {
    pub id: String,
    pub snippet: String,
    pub created_at: i64,
    pub position: i64,
    pub deleted_at: i64,
}

#[derive(Debug, Serialize)]
pub struct Neighbors {
    pub older: Option<Note>,
    pub newer: Option<Note>,
}

pub struct SavedWindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub display_identifier: Option<String>,
}

pub struct Database {
    connection: Connection,
    data_dir: PathBuf,
}

impl Database {
    pub fn open(data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&data_dir).map_err(error)?;
        let path = data_dir.join("notes.sqlite");
        let connection = Connection::open(path).map_err(error)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS notes (
                   id TEXT PRIMARY KEY,
                   body TEXT NOT NULL DEFAULT '',
                   position INTEGER NOT NULL UNIQUE,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   cursor_start INTEGER NOT NULL DEFAULT 0,
                   cursor_end INTEGER NOT NULL DEFAULT 0,
                   scroll_top REAL NOT NULL DEFAULT 0,
                   deleted_at INTEGER
                 );
                 CREATE TABLE IF NOT EXISTS app_state (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS settings (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS window_state (
                   id INTEGER PRIMARY KEY CHECK (id = 1),
                   x INTEGER NOT NULL,
                   y INTEGER NOT NULL,
                   width INTEGER NOT NULL,
                   height INTEGER NOT NULL,
                   scale_factor REAL NOT NULL,
                   display_identifier TEXT
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(note_id UNINDEXED, body);
                 CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes WHEN new.deleted_at IS NULL BEGIN
                   INSERT INTO notes_fts(note_id, body) VALUES (new.id, new.body);
                 END;
                 CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
                   DELETE FROM notes_fts WHERE note_id = old.id;
                   INSERT INTO notes_fts(note_id, body)
                     SELECT new.id, new.body WHERE new.deleted_at IS NULL;
                 END;
                 CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
                   DELETE FROM notes_fts WHERE note_id = old.id;
                 END;",
            )
            .map_err(error)?;

        let has_display_identifier: bool = connection
            .query_row(
                "SELECT count(*) > 0 FROM pragma_table_info('window_state') WHERE name='display_identifier'",
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !has_display_identifier {
            connection
                .execute(
                    "ALTER TABLE window_state ADD COLUMN display_identifier TEXT",
                    [],
                )
                .map_err(error)?;
        }

        let db = Self {
            connection,
            data_dir,
        };
        db.purge_deleted()?;
        db.ensure_search_index()?;
        Ok(db)
    }

    fn ensure_search_index(&self) -> Result<(), String> {
        let indexed: i64 = self
            .connection
            .query_row("SELECT count(*) FROM notes_fts", [], |row| row.get(0))
            .map_err(error)?;
        let active: i64 = self
            .connection
            .query_row(
                "SELECT count(*) FROM notes WHERE deleted_at IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if indexed != active {
            self.connection
                .execute("DELETE FROM notes_fts", [])
                .map_err(error)?;
            self.connection
                .execute(
                    "INSERT INTO notes_fts(note_id, body) SELECT id, body FROM notes WHERE deleted_at IS NULL",
                    [],
                )
                .map_err(error)?;
        }
        Ok(())
    }

    pub fn initial_note(&self) -> Result<Note, String> {
        let active_id = self.setting_from("app_state", "active_note_id")?;
        if let Some(id) = active_id
            && let Some(note) = self.note_by_id(&id)?
        {
            return Ok(note);
        }
        if let Some(note) = self.newest_note()? {
            self.set_in("app_state", "active_note_id", &note.id)?;
            return Ok(note);
        }
        Ok(self.transient_note(1))
    }

    pub fn save_note(&mut self, input: &NoteInput) -> Result<SaveResult, String> {
        let now = now_ms();
        if !input.persisted && input.body.trim().is_empty() {
            return Ok(SaveResult {
                persisted: false,
                updated_at: now,
            });
        }
        let transaction = self.connection.transaction().map_err(error)?;
        transaction
            .execute(
                "INSERT INTO notes(id, body, position, created_at, updated_at, cursor_start, cursor_end, scroll_top, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)
                 ON CONFLICT(id) DO UPDATE SET
                   body=excluded.body, updated_at=excluded.updated_at,
                   cursor_start=excluded.cursor_start, cursor_end=excluded.cursor_end,
                   scroll_top=excluded.scroll_top",
                params![
                    input.id,
                    input.body,
                    input.position,
                    input.created_at,
                    now,
                    input.cursor_start,
                    input.cursor_end,
                    input.scroll_top,
                ],
            )
            .map_err(error)?;
        transaction
            .execute(
                "INSERT INTO app_state(key, value) VALUES ('active_note_id', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [&input.id],
            )
            .map_err(error)?;
        transaction.commit().map_err(error)?;
        Ok(SaveResult {
            persisted: true,
            updated_at: now,
        })
    }

    pub fn navigate(&self, note_id: &str, direction: i32) -> Result<Note, String> {
        let current = self.note_by_id(note_id)?;
        let next = match (current.as_ref(), direction) {
            (Some(note), value) if value < 0 => self.connection
                .query_row(
                    "SELECT id FROM notes WHERE deleted_at IS NULL AND position < ?1 ORDER BY position DESC LIMIT 1",
                    [note.position],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(error)?,
            (Some(note), _) => self.connection
                .query_row(
                    "SELECT id FROM notes WHERE deleted_at IS NULL AND position > ?1 ORDER BY position ASC LIMIT 1",
                    [note.position],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(error)?,
            (None, value) if value < 0 => return self.newest_note().map(|note| note.unwrap_or_else(|| self.transient_note(1))),
            (None, _) => return Ok(self.transient_note(self.next_position()?)),
        };

        if let Some(id) = next {
            self.set_in("app_state", "active_note_id", &id)?;
            return self
                .note_by_id(&id)?
                .ok_or_else(|| "page disappeared".to_string());
        }

        let current = current.expect("matched above");
        if direction > 0 && !current.body.trim().is_empty() {
            return Ok(self.transient_note(self.next_position()?));
        }
        Ok(current)
    }

    pub fn neighbors(&self, note_id: &str) -> Result<Neighbors, String> {
        let Some(current) = self.note_by_id(note_id)? else {
            return Ok(Neighbors {
                older: self.newest_note()?,
                newer: None,
            });
        };

        let older = self.neighbor(&current, -1)?;
        let mut newer = self.neighbor(&current, 1)?;
        if newer.is_none() && !current.body.trim().is_empty() {
            newer = Some(self.transient_note(self.next_position()?));
        }
        Ok(Neighbors { older, newer })
    }

    pub fn select_note(&self, id: &str) -> Result<Note, String> {
        let note = self
            .note_by_id(id)?
            .ok_or_else(|| "page not found".to_string())?;
        self.set_in("app_state", "active_note_id", id)?;
        Ok(note)
    }

    pub fn delete_note(&mut self, id: &str) -> Result<(Note, Option<String>), String> {
        let current = match self.note_by_id(id)? {
            Some(note) => note,
            None => {
                return Ok((
                    self.newest_note()?
                        .unwrap_or_else(|| self.transient_note(1)),
                    None,
                ));
            }
        };
        let now = now_ms();
        self.connection
            .execute(
                "UPDATE notes SET deleted_at=?1 WHERE id=?2",
                params![now, id],
            )
            .map_err(error)?;

        let next_id = self.connection
            .query_row(
                "SELECT id FROM notes WHERE deleted_at IS NULL AND position > ?1 ORDER BY position ASC LIMIT 1",
                [current.position],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
            .or(self.connection
                .query_row(
                    "SELECT id FROM notes WHERE deleted_at IS NULL AND position < ?1 ORDER BY position DESC LIMIT 1",
                    [current.position],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(error)?);

        let note = if let Some(next_id) = next_id {
            self.set_in("app_state", "active_note_id", &next_id)?;
            self.note_by_id(&next_id)?
                .ok_or_else(|| "page disappeared".to_string())?
        } else {
            self.connection
                .execute("DELETE FROM app_state WHERE key='active_note_id'", [])
                .map_err(error)?;
            self.transient_note(1)
        };
        Ok((note, Some(id.to_string())))
    }

    pub fn restore_note(&self, id: &str) -> Result<Note, String> {
        self.connection
            .execute("UPDATE notes SET deleted_at=NULL WHERE id=?1", [id])
            .map_err(error)?;
        self.set_in("app_state", "active_note_id", id)?;
        self.note_by_id(id)?
            .ok_or_else(|| "deleted page not found".to_string())
    }

    pub fn list_pages(&self, query: &str) -> Result<Vec<PageSummary>, String> {
        let mut pages = Vec::new();
        if query.trim().is_empty() {
            let mut statement = self.connection
                .prepare("SELECT id, substr(replace(body, char(10), ' '), 1, 240), created_at, position FROM notes WHERE deleted_at IS NULL ORDER BY created_at DESC, position DESC")
                .map_err(error)?;
            let rows = statement.query_map([], page_summary).map_err(error)?;
            for row in rows {
                pages.push(row.map_err(error)?);
            }
        } else {
            let fts_query = fts_query(query);
            let mut statement = self.connection
                .prepare("SELECT n.id, replace(snippet(notes_fts, 1, '', '', ' … ', 32), char(10), ' '), n.created_at, n.position FROM notes_fts JOIN notes n ON n.id=notes_fts.note_id WHERE notes_fts MATCH ?1 AND n.deleted_at IS NULL ORDER BY n.created_at DESC, n.position DESC")
                .map_err(error)?;
            let rows = statement
                .query_map([fts_query], page_summary)
                .map_err(error)?;
            for row in rows {
                pages.push(row.map_err(error)?);
            }
        }
        Ok(pages)
    }

    pub fn list_deleted(&self) -> Result<Vec<DeletedPage>, String> {
        self.purge_deleted()?;
        let mut statement = self.connection
            .prepare("SELECT id, substr(replace(body, char(10), ' '), 1, 240), created_at, position, deleted_at FROM notes WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC")
            .map_err(error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(DeletedPage {
                    id: row.get(0)?,
                    snippet: row.get(1)?,
                    created_at: row.get(2)?,
                    position: row.get(3)?,
                    deleted_at: row.get(4)?,
                })
            })
            .map_err(error)?;
        let mut pages = Vec::new();
        for row in rows {
            pages.push(row.map_err(error)?);
        }
        Ok(pages)
    }

    pub fn shortcut(&self) -> Result<String, String> {
        Ok(self
            .setting_from("settings", "shortcut")?
            .unwrap_or_else(|| "CommandOrControl+Shift+Space".to_string()))
    }

    pub fn set_shortcut(&self, value: &str) -> Result<(), String> {
        self.set_in("settings", "shortcut", value)
    }

    pub fn launch_at_login(&self) -> Result<bool, String> {
        Ok(self.setting_from("settings", "launch_at_login")?.as_deref() != Some("false"))
    }

    pub fn set_launch_at_login(&self, enabled: bool) -> Result<(), String> {
        self.set_in(
            "settings",
            "launch_at_login",
            if enabled { "true" } else { "false" },
        )
    }

    pub fn window_state(&self) -> Result<Option<SavedWindowState>, String> {
        self.connection
            .query_row(
                "SELECT x, y, width, height, scale_factor, display_identifier FROM window_state WHERE id=1",
                [],
                |row| {
                    Ok(SavedWindowState {
                        x: row.get(0)?,
                        y: row.get(1)?,
                        width: row.get(2)?,
                        height: row.get(3)?,
                        scale_factor: row.get(4)?,
                        display_identifier: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(error)
    }

    pub fn save_window_state(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        scale: f64,
        display_identifier: Option<String>,
    ) -> Result<(), String> {
        self.connection.execute(
            "INSERT INTO window_state(id, x, y, width, height, scale_factor, display_identifier) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET x=excluded.x, y=excluded.y, width=excluded.width, height=excluded.height, scale_factor=excluded.scale_factor, display_identifier=excluded.display_identifier",
            params![x, y, width, height, scale, display_identifier],
        ).map_err(error)?;
        Ok(())
    }

    pub fn backup(&self) -> Result<PathBuf, String> {
        self.purge_deleted()?;
        let backup_dir = self.data_dir.join("backups");
        fs::create_dir_all(&backup_dir).map_err(error)?;
        let destination = backup_dir.join(format!("notes-{}-{}.sqlite", now_ms(), Uuid::new_v4()));
        let mut destination_db = Connection::open(&destination).map_err(error)?;
        let backup =
            rusqlite::backup::Backup::new(&self.connection, &mut destination_db).map_err(error)?;
        backup
            .run_to_completion(64, std::time::Duration::from_millis(5), None)
            .map_err(error)?;
        drop(backup);
        rotate_files(&backup_dir, 5)?;
        Ok(destination)
    }

    pub fn export_markdown(&self) -> Result<PathBuf, String> {
        let export_dir = self.data_dir.join("exports");
        fs::create_dir_all(&export_dir).map_err(error)?;
        let path = export_dir.join(format!("not-export-{}.md", now_ms()));
        let mut statement = self
            .connection
            .prepare("SELECT body FROM notes WHERE deleted_at IS NULL ORDER BY position ASC")
            .map_err(error)?;
        let bodies = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?;
        let mut output = String::new();
        for (index, body) in bodies.enumerate() {
            if index > 0 {
                output.push_str("\n\n---\n\n");
            }
            output.push_str(&body.map_err(error)?);
        }
        fs::write(&path, output).map_err(error)?;
        Ok(path)
    }

    fn purge_deleted(&self) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
                [now_ms() - TRASH_RETENTION_MS],
            )
            .map_err(error)?;
        Ok(())
    }

    fn newest_note(&self) -> Result<Option<Note>, String> {
        let id = self
            .connection
            .query_row(
                "SELECT id FROM notes WHERE deleted_at IS NULL ORDER BY position DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?;
        id.map(|id| self.note_by_id(&id))
            .transpose()
            .map(|value| value.flatten())
    }

    fn neighbor(&self, current: &Note, direction: i32) -> Result<Option<Note>, String> {
        let (comparison, order) = if direction < 0 {
            ("<", "DESC")
        } else {
            (">", "ASC")
        };
        let sql = format!(
            "SELECT id FROM notes WHERE deleted_at IS NULL AND position {comparison} ?1 ORDER BY position {order} LIMIT 1"
        );
        let id = self
            .connection
            .query_row(&sql, [current.position], |row| row.get::<_, String>(0))
            .optional()
            .map_err(error)?;
        id.map(|id| self.note_by_id(&id))
            .transpose()
            .map(|note| note.flatten())
    }

    fn note_by_id(&self, id: &str) -> Result<Option<Note>, String> {
        let total = self.active_count()?;
        self.connection
            .query_row(
                "SELECT id, body, position, created_at, updated_at, cursor_start, cursor_end, scroll_top,
                        (SELECT count(*) FROM notes before WHERE before.deleted_at IS NULL AND before.position <= notes.position)
                 FROM notes WHERE id=?1 AND deleted_at IS NULL",
                [id],
                |row| Ok(Note {
                    id: row.get(0)?, body: row.get(1)?, position: row.get(2)?, created_at: row.get(3)?,
                    updated_at: row.get(4)?, cursor_start: row.get(5)?, cursor_end: row.get(6)?,
                    scroll_top: row.get(7)?, persisted: true, ordinal: row.get(8)?, total,
                }),
            )
            .optional()
            .map_err(error)
    }

    fn transient_note(&self, position: i64) -> Note {
        let now = now_ms();
        let total = self.active_count().unwrap_or(0) + 1;
        Note {
            id: Uuid::new_v4().to_string(),
            body: String::new(),
            position,
            created_at: now,
            updated_at: now,
            cursor_start: 0,
            cursor_end: 0,
            scroll_top: 0.0,
            persisted: false,
            ordinal: total,
            total,
        }
    }

    fn active_count(&self) -> Result<i64, String> {
        self.connection
            .query_row(
                "SELECT count(*) FROM notes WHERE deleted_at IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(error)
    }

    fn next_position(&self) -> Result<i64, String> {
        self.connection
            .query_row(
                "SELECT coalesce(max(position), 0) + 1 FROM notes",
                [],
                |row| row.get(0),
            )
            .map_err(error)
    }

    fn setting_from(&self, table: &str, key: &str) -> Result<Option<String>, String> {
        let sql = format!("SELECT value FROM {table} WHERE key=?1");
        self.connection
            .query_row(&sql, [key], |row| row.get(0))
            .optional()
            .map_err(error)
    }

    fn set_in(&self, table: &str, key: &str, value: &str) -> Result<(), String> {
        let sql = format!(
            "INSERT INTO {table}(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value"
        );
        self.connection
            .execute(&sql, params![key, value])
            .map_err(error)?;
        Ok(())
    }
}

fn page_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageSummary> {
    Ok(PageSummary {
        id: row.get(0)?,
        snippet: row.get(1)?,
        created_at: row.get(2)?,
        position: row.get(3)?,
    })
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn rotate_files(directory: &Path, keep: usize) -> Result<(), String> {
    let mut files = fs::read_dir(directory)
        .map_err(error)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|entry| entry.file_name());
    let remove_count = files.len().saturating_sub(keep);
    for entry in files.into_iter().take(remove_count) {
        fs::remove_file(entry.path()).map_err(error)?;
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Database {
        let path = std::env::temp_dir().join(format!("not-test-{}", Uuid::new_v4()));
        Database::open(path).unwrap()
    }

    fn save(db: &mut Database, note: &Note, body: &str) {
        db.save_note(&NoteInput {
            id: note.id.clone(),
            body: body.to_string(),
            position: note.position,
            created_at: note.created_at,
            cursor_start: body.len() as i64,
            cursor_end: body.len() as i64,
            scroll_top: 0.0,
            persisted: note.persisted,
        })
        .unwrap();
    }

    #[test]
    fn transient_blank_is_not_persisted() {
        let mut db = database();
        let blank = db.initial_note().unwrap();
        save(&mut db, &blank, "");
        assert_eq!(db.active_count().unwrap(), 0);
    }

    #[test]
    fn forward_navigation_creates_only_one_blank() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "hello");
        let blank = db.navigate(&first.id, 1).unwrap();
        assert!(!blank.persisted);
        let same = db.navigate(&blank.id, 1).unwrap();
        assert!(!same.persisted);
        assert_eq!(db.active_count().unwrap(), 1);
    }

    #[test]
    fn empty_existing_page_is_retained() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "hello");
        let saved = db.select_note(&first.id).unwrap();
        save(&mut db, &saved, "");
        assert_eq!(db.active_count().unwrap(), 1);
        assert_eq!(db.select_note(&first.id).unwrap().body, "");
    }

    #[test]
    fn delete_prefers_newer_and_can_restore() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "one");
        let second = db.navigate(&first.id, 1).unwrap();
        save(&mut db, &second, "two");
        let (active, deleted) = db.delete_note(&first.id).unwrap();
        assert_eq!(active.id, second.id);
        assert_eq!(db.restore_note(&deleted.unwrap()).unwrap().id, first.id);
    }

    #[test]
    fn full_text_search_matches_body_content() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        let body = format!("{} a buried searchable phrase", "prefix ".repeat(80));
        save(&mut db, &first, &body);
        let pages = db.list_pages("searchable").unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, first.id);
        assert!(pages[0].snippet.contains("searchable"));
    }

    #[test]
    fn neighbors_include_adjacent_pages_and_one_trailing_blank() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "one");
        let second = db.navigate(&first.id, 1).unwrap();
        save(&mut db, &second, "two");

        let around_first = db.neighbors(&first.id).unwrap();
        assert!(around_first.older.is_none());
        assert_eq!(around_first.newer.unwrap().id, second.id);

        let around_second = db.neighbors(&second.id).unwrap();
        assert_eq!(around_second.older.unwrap().id, first.id);
        assert!(!around_second.newer.unwrap().persisted);
    }

    #[test]
    fn window_state_retains_display_identifier() {
        let db = database();
        db.save_window_state(10, 20, 440, 340, 2.0, Some("Display A".to_string()))
            .unwrap();
        let saved = db.window_state().unwrap().unwrap();
        assert_eq!(saved.display_identifier.as_deref(), Some("Display A"));
    }

    #[test]
    fn rotating_backup_is_readable_and_keeps_five_files() {
        let mut db = database();
        let note = db.initial_note().unwrap();
        save(&mut db, &note, "recoverable text");
        let mut latest = None;
        for _ in 0..7 {
            latest = Some(db.backup().unwrap());
        }
        let backup_dir = db.data_dir.join("backups");
        assert_eq!(fs::read_dir(&backup_dir).unwrap().count(), 5);
        let backup = Connection::open(latest.unwrap()).unwrap();
        let body: String = backup
            .query_row("SELECT body FROM notes LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(body, "recoverable text");
    }

    #[test]
    fn relaunch_restores_active_page_selection_and_scroll() {
        let path = std::env::temp_dir().join(format!("not-relaunch-test-{}", Uuid::new_v4()));
        let note_id;
        {
            let mut db = Database::open(path.clone()).unwrap();
            let note = db.initial_note().unwrap();
            note_id = note.id.clone();
            db.save_note(&NoteInput {
                id: note.id,
                body: "restored body".to_string(),
                position: note.position,
                created_at: note.created_at,
                cursor_start: 3,
                cursor_end: 8,
                scroll_top: 42.5,
                persisted: note.persisted,
            })
            .unwrap();
        }

        let reopened = Database::open(path).unwrap();
        let restored = reopened.initial_note().unwrap();
        assert_eq!(restored.id, note_id);
        assert_eq!(restored.body, "restored body");
        assert_eq!((restored.cursor_start, restored.cursor_end), (3, 8));
        assert_eq!(restored.scroll_top, 42.5);
    }

    #[test]
    fn reopening_purges_deleted_pages_older_than_seven_days() {
        let path = std::env::temp_dir().join(format!("not-purge-test-{}", Uuid::new_v4()));
        {
            let mut db = Database::open(path.clone()).unwrap();
            let note = db.initial_note().unwrap();
            save(&mut db, &note, "expired trash");
            db.delete_note(&note.id).unwrap();
            db.connection
                .execute(
                    "UPDATE notes SET deleted_at=?1 WHERE id=?2",
                    params![now_ms() - TRASH_RETENTION_MS - 1, note.id],
                )
                .unwrap();
            assert!(db.list_deleted().unwrap().is_empty());
        }

        let reopened = Database::open(path).unwrap();
        assert_eq!(reopened.active_count().unwrap(), 0);
        assert!(reopened.list_deleted().unwrap().is_empty());
    }

    #[test]
    fn full_text_search_finds_a_buried_match_across_many_large_pages() {
        let mut db = database();
        let mut current = db.initial_note().unwrap();
        for index in 0..120 {
            let suffix = if index == 117 {
                " unique-final-needle"
            } else {
                ""
            };
            let body = format!("page {index} {}{suffix}", "substantial body ".repeat(300));
            save(&mut db, &current, &body);
            current = db.navigate(&current.id, 1).unwrap();
        }

        let pages = db.list_pages("unique-final-needle").unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages[0].snippet.contains("unique-final-needle"));
    }
}

use image::ImageFormat;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::clipboard::{THUMBNAIL_MAX_HEIGHT, THUMBNAIL_MAX_WIDTH};

const TRASH_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

fn normalize_glass_settings(mut settings: GlassSettings) -> Result<GlassSettings, String> {
    settings.dark_tint = normalize_tint(&settings.dark_tint)?;
    settings.light_tint = normalize_tint(&settings.light_tint)?;
    settings.opacity = settings.opacity.min(100);
    Ok(settings)
}

fn normalize_tint(value: &str) -> Result<String, String> {
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("invalid glass tint".to_string());
    }
    Ok(value.to_ascii_uppercase())
}

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub note_id: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: u64,
    pub content_hash: String,
    pub thumbnail_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardSettings {
    pub capture_text: bool,
    pub capture_images: bool,
    pub ignore_duplicates: bool,
    pub ignore_whitespace: bool,
    pub ignore_sensitive: bool,
    pub minimum_text_length: usize,
    pub maximum_text_length: usize,
    pub ignored_applications: String,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            capture_text: true,
            capture_images: true,
            ignore_duplicates: true,
            ignore_whitespace: true,
            ignore_sensitive: true,
            minimum_text_length: 1,
            maximum_text_length: 100_000,
            ignored_applications: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub custom_program: String,
    pub custom_arguments: String,
    pub last_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct GlassSettings {
    pub enabled: bool,
    pub dark_tint: String,
    pub light_tint: String,
    pub opacity: u8,
}

impl Default for GlassSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            dark_tint: "#161619".to_string(),
            light_tint: "#F5F5F7".to_string(),
            opacity: 28,
        }
    }
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
    pub relative_x: Option<f64>,
    pub relative_y: Option<f64>,
    pub logical_width: Option<f64>,
    pub logical_height: Option<f64>,
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
                   deleted_at INTEGER,
                   source TEXT NOT NULL DEFAULT 'manual',
                   capture_hash TEXT
                 );
                 CREATE TABLE IF NOT EXISTS attachments (
                   id TEXT PRIMARY KEY,
                   note_id TEXT NOT NULL,
                   path TEXT NOT NULL,
                   thumbnail_path TEXT,
                   mime_type TEXT NOT NULL,
                   width INTEGER NOT NULL,
                   height INTEGER NOT NULL,
                   byte_size INTEGER NOT NULL,
                   content_hash TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   FOREIGN KEY(note_id) REFERENCES notes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_attachments_note_id ON attachments(note_id);
                 DROP INDEX IF EXISTS idx_attachments_hash_note;
                 CREATE INDEX IF NOT EXISTS idx_attachments_hash ON attachments(content_hash);
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
                   display_identifier TEXT,
                   relative_x REAL,
                   relative_y REAL,
                   logical_width REAL,
                   logical_height REAL
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

        for (column, sql) in [
            (
                "source",
                "ALTER TABLE notes ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'",
            ),
            (
                "capture_hash",
                "ALTER TABLE notes ADD COLUMN capture_hash TEXT",
            ),
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT count(*) > 0 FROM pragma_table_info('notes') WHERE name=?1",
                    [column],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if !exists {
                connection.execute(sql, []).map_err(error)?;
            }
        }
        connection
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_notes_capture_hash ON notes(capture_hash) WHERE capture_hash IS NOT NULL",
                [],
            )
            .map_err(error)?;

        let has_thumbnail_path: bool = connection
            .query_row(
                "SELECT count(*) > 0 FROM pragma_table_info('attachments') WHERE name='thumbnail_path'",
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        if !has_thumbnail_path {
            connection
                .execute("ALTER TABLE attachments ADD COLUMN thumbnail_path TEXT", [])
                .map_err(error)?;
        }

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
        for (column, sql) in [
            (
                "relative_x",
                "ALTER TABLE window_state ADD COLUMN relative_x REAL",
            ),
            (
                "relative_y",
                "ALTER TABLE window_state ADD COLUMN relative_y REAL",
            ),
            (
                "logical_width",
                "ALTER TABLE window_state ADD COLUMN logical_width REAL",
            ),
            (
                "logical_height",
                "ALTER TABLE window_state ADD COLUMN logical_height REAL",
            ),
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT count(*) > 0 FROM pragma_table_info('window_state') WHERE name=?1",
                    [column],
                    |row| row.get(0),
                )
                .map_err(error)?;
            if !exists {
                connection.execute(sql, []).map_err(error)?;
            }
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

    #[cfg(test)]
    pub fn create_page(
        &self,
        body: &str,
        source: &str,
        capture_hash: Option<&str>,
        activate: bool,
    ) -> Result<Note, String> {
        let id = Uuid::new_v4().to_string();
        let position = self.next_position()?;
        let now = now_ms();
        let cursor = body.encode_utf16().count() as i64;
        self.connection
            .execute(
                "INSERT INTO notes(id, body, position, created_at, updated_at, cursor_start, cursor_end, scroll_top, deleted_at, source, capture_hash)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5, 0, NULL, ?6, ?7)",
                params![id, body, position, now, cursor, source, capture_hash],
            )
            .map_err(error)?;
        if activate {
            self.set_in("app_state", "active_note_id", &id)?;
        }
        self.note_by_id(&id)?
            .ok_or_else(|| "created page disappeared".to_string())
    }

    #[cfg(test)]
    pub fn page_for_capture_hash(&self, hash: &str) -> Result<Option<Note>, String> {
        let id = self
            .connection
            .query_row(
                "SELECT id FROM notes WHERE capture_hash=?1 AND deleted_at IS NULL ORDER BY created_at DESC LIMIT 1",
                [hash],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?;
        id.map(|id| self.note_by_id(&id))
            .transpose()
            .map(|note| note.flatten())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_note_with_attachment(
        &mut self,
        input: &NoteInput,
        attachment_id: &str,
        png: &[u8],
        thumbnail_png: &[u8],
        width: u32,
        height: u32,
        content_hash: &str,
        source: &str,
        capture_hash: Option<&str>,
    ) -> Result<Note, String> {
        let attachment_dir = self.data_dir.join("attachments");
        fs::create_dir_all(&attachment_dir).map_err(error)?;
        let filename = format!("{attachment_id}.png");
        let final_path = attachment_dir.join(&filename);
        let thumbnail_filename = format!("{attachment_id}.thumb-v2.png");
        let thumbnail_final_path = attachment_dir.join(&thumbnail_filename);
        let temporary_path = attachment_dir.join(format!(".{attachment_id}.tmp"));
        let thumbnail_temporary_path = attachment_dir.join(format!(".{attachment_id}.thumb.tmp"));
        let file_result = (|| -> Result<(), String> {
            fs::write(&temporary_path, png).map_err(error)?;
            fs::write(&thumbnail_temporary_path, thumbnail_png).map_err(error)?;
            fs::rename(&temporary_path, &final_path).map_err(error)?;
            fs::rename(&thumbnail_temporary_path, &thumbnail_final_path).map_err(error)?;
            Ok(())
        })();
        if let Err(failure) = file_result {
            for path in [
                &temporary_path,
                &thumbnail_temporary_path,
                &final_path,
                &thumbnail_final_path,
            ] {
                let _ = fs::remove_file(path);
            }
            return Err(failure);
        }

        let now = now_ms();
        let transaction = self.connection.transaction().map_err(error)?;
        let result = (|| -> Result<(), rusqlite::Error> {
            transaction.execute(
                "INSERT INTO notes(id, body, position, created_at, updated_at, cursor_start, cursor_end, scroll_top, deleted_at, source, capture_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET body=excluded.body, updated_at=excluded.updated_at,
                   cursor_start=excluded.cursor_start, cursor_end=excluded.cursor_end, scroll_top=excluded.scroll_top",
                params![input.id, input.body, input.position, input.created_at, now, input.cursor_start, input.cursor_end, input.scroll_top, source, capture_hash],
            )?;
            transaction.execute(
                "INSERT INTO app_state(key, value) VALUES ('active_note_id', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [&input.id],
            )?;
            transaction.execute(
                "INSERT INTO attachments(id, note_id, path, thumbnail_path, mime_type, width, height, byte_size, content_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'image/png', ?5, ?6, ?7, ?8, ?9)",
                params![attachment_id, input.id, filename, thumbnail_filename, width, height, png.len() as i64, content_hash, now],
            )?;
            transaction.commit()?;
            Ok(())
        })();
        if let Err(failure) = result {
            let _ = fs::remove_file(final_path);
            let _ = fs::remove_file(thumbnail_final_path);
            return Err(error(failure));
        }
        self.note_by_id(&input.id)?
            .ok_or_else(|| "saved page disappeared".to_string())
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn create_image_page(
        &mut self,
        attachment_id: &str,
        png: &[u8],
        thumbnail_png: &[u8],
        width: u32,
        height: u32,
        content_hash: &str,
        activate: bool,
    ) -> Result<Note, String> {
        let previous_active = self.setting_from("app_state", "active_note_id")?;
        let body = format!("![clipboard image](attachment://{attachment_id})");
        let now = now_ms();
        let cursor = body.encode_utf16().count() as i64;
        let input = NoteInput {
            id: Uuid::new_v4().to_string(),
            body,
            position: self.next_position()?,
            created_at: now,
            cursor_start: cursor,
            cursor_end: cursor,
            scroll_top: 0.0,
            persisted: false,
        };
        let note = self.save_note_with_attachment(
            &input,
            attachment_id,
            png,
            thumbnail_png,
            width,
            height,
            content_hash,
            "clipboard_image",
            Some(content_hash),
        )?;
        if !activate {
            if let Some(previous_active) = previous_active {
                self.set_in("app_state", "active_note_id", &previous_active)?;
            } else {
                self.connection
                    .execute("DELETE FROM app_state WHERE key='active_note_id'", [])
                    .map_err(error)?;
            }
        }
        Ok(note)
    }

    pub fn list_attachments(&self, note_id: &str) -> Result<Vec<Attachment>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM attachments WHERE note_id=?1 ORDER BY created_at ASC")
            .map_err(error)?;
        let ids = statement
            .query_map([note_id], |row| row.get::<_, String>(0))
            .map_err(error)?;
        let mut attachments = Vec::new();
        for id in ids {
            attachments.push(self.attachment_by_id(&id.map_err(error)?)?);
        }
        Ok(attachments)
    }

    fn attachment_by_id(&self, id: &str) -> Result<Attachment, String> {
        self.connection
            .query_row(
                "SELECT id, note_id, mime_type, width, height, byte_size, content_hash FROM attachments WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(error)
            .map(|(id, note_id, mime_type, width, height, byte_size, content_hash)| Attachment {
                    thumbnail_url: format!("not-asset://localhost/thumbnail/{id}"),
                    id,
                    note_id,
                    mime_type,
                    width,
                    height,
                    byte_size: byte_size.max(0) as u64,
                    content_hash,
                })
    }

    pub fn active_attachment_thumbnail(&mut self, id: &str) -> Result<(Vec<u8>, String), String> {
        let record = self
            .connection
            .query_row(
                "SELECT a.path, a.thumbnail_path, a.mime_type FROM attachments a
             JOIN app_state s ON s.key='active_note_id' AND s.value=a.note_id
             JOIN notes n ON n.id=a.note_id AND n.deleted_at IS NULL
             WHERE a.id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(error)?
            .ok_or_else(|| "attachment is not active".to_string())?;
        let (original_filename, thumbnail_filename, _mime_type) = record;
        validate_filename(&original_filename)?;
        let attachment_dir = self.data_dir.join("attachments");
        if let Some(filename) = thumbnail_filename.as_deref() {
            validate_filename(filename)?;
            let path = attachment_dir.join(filename);
            if filename.ends_with(".thumb-v2.png") && path.is_file() {
                return Ok((fs::read(path).map_err(error)?, "image/png".to_string()));
            }
        }
        let source_path = thumbnail_filename
            .as_deref()
            .map(|filename| attachment_dir.join(filename))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| attachment_dir.join(&original_filename));
        let source = fs::read(source_path).map_err(error)?;
        let decoded = image::load_from_memory(&source).map_err(error)?;
        let thumbnail = decoded.thumbnail(THUMBNAIL_MAX_WIDTH, THUMBNAIL_MAX_HEIGHT);
        let filename = format!("{id}.thumb-v2.png");
        let path = attachment_dir.join(&filename);
        thumbnail
            .save_with_format(&path, ImageFormat::Png)
            .map_err(error)?;
        self.connection
            .execute(
                "UPDATE attachments SET thumbnail_path=?1 WHERE id=?2",
                params![filename, id],
            )
            .map_err(error)?;
        if let Some(old_filename) = thumbnail_filename
            && old_filename != filename
        {
            let _ = fs::remove_file(attachment_dir.join(old_filename));
        }
        Ok((fs::read(path).map_err(error)?, "image/png".to_string()))
    }

    pub fn revision_matches(
        &self,
        note_id: &str,
        expected_updated_at: i64,
        persisted: bool,
    ) -> Result<bool, String> {
        let revision = self
            .connection
            .query_row(
                "SELECT updated_at FROM notes WHERE id=?1 AND deleted_at IS NULL",
                [note_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(error)?;
        Ok(match revision {
            Some(value) => value == expected_updated_at,
            None => !persisted,
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

    pub fn new_note(&self) -> Result<Note, String> {
        if let Some(newest) = self.newest_note()?
            && newest.body.trim().is_empty()
        {
            self.set_in("app_state", "active_note_id", &newest.id)?;
            return Ok(newest);
        }
        Ok(self.transient_note(self.next_position()?))
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

    pub fn shortcut_label(&self) -> Result<Option<String>, String> {
        self.setting_from("settings", "shortcut_label")
    }

    pub fn set_shortcut(&mut self, value: &str, label: &str) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(error)?;
        for (key, setting) in [("shortcut", value), ("shortcut_label", label)] {
            transaction
                .execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    params![key, setting],
                )
                .map_err(error)?;
        }
        transaction.commit().map_err(error)
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

    pub fn font_size(&self) -> Result<i64, String> {
        Ok(self
            .setting_from("settings", "font_size")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(15)
            .clamp(11, 28))
    }

    pub fn set_font_size(&self, value: i64) -> Result<i64, String> {
        let value = value.clamp(11, 28);
        self.set_in("settings", "font_size", &value.to_string())?;
        Ok(value)
    }

    pub fn theme(&self) -> Result<String, String> {
        let value = self
            .setting_from("settings", "theme")?
            .unwrap_or_else(|| "auto".to_string());
        Ok(match value.as_str() {
            "dark" | "light" => value,
            _ => "auto".to_string(),
        })
    }

    pub fn set_theme(&self, value: &str) -> Result<String, String> {
        let value = match value {
            "auto" | "dark" | "light" => value,
            _ => return Err("invalid theme".to_string()),
        };
        self.set_in("settings", "theme", value)?;
        Ok(value.to_string())
    }

    pub fn glass_settings(&self) -> Result<GlassSettings, String> {
        let Some(value) = self.setting_from("settings", "glass_settings")? else {
            return Ok(GlassSettings {
                enabled: self.setting_from("settings", "theme")?.as_deref() == Some("glass"),
                ..GlassSettings::default()
            });
        };
        let mut settings = serde_json::from_str::<GlassSettings>(&value).unwrap_or_default();
        if let Ok(stored) = serde_json::from_str::<serde_json::Value>(&value)
            && let Some(tint) = stored.get("tint").and_then(|value| value.as_str())
        {
            settings.dark_tint = tint.to_string();
            settings.light_tint = tint.to_string();
        }
        Ok(normalize_glass_settings(settings).unwrap_or_default())
    }

    pub fn set_glass_settings(&self, settings: GlassSettings) -> Result<GlassSettings, String> {
        let settings = normalize_glass_settings(settings)?;
        self.set_in(
            "settings",
            "glass_settings",
            &serde_json::to_string(&settings).map_err(error)?,
        )?;
        Ok(settings)
    }

    pub fn clipboard_settings(&self) -> Result<ClipboardSettings, String> {
        let Some(value) = self.setting_from("settings", "clipboard_settings")? else {
            return Ok(ClipboardSettings::default());
        };
        serde_json::from_str(&value).map_err(error)
    }

    pub fn set_clipboard_settings(
        &self,
        mut settings: ClipboardSettings,
    ) -> Result<ClipboardSettings, String> {
        settings.minimum_text_length = settings.minimum_text_length.clamp(1, 100_000);
        settings.maximum_text_length = settings.maximum_text_length.clamp(1, 1_000_000);
        if settings.minimum_text_length > settings.maximum_text_length {
            settings.minimum_text_length = settings.maximum_text_length;
        }
        let value = serde_json::to_string(&settings).map_err(error)?;
        self.set_in("settings", "clipboard_settings", &value)?;
        Ok(settings)
    }

    pub fn ai_settings(&self) -> Result<AiSettings, String> {
        let Some(value) = self.setting_from("settings", "ai_settings")? else {
            return Ok(AiSettings::default());
        };
        serde_json::from_str(&value).map_err(error)
    }

    pub fn set_ai_settings(&self, settings: &AiSettings) -> Result<AiSettings, String> {
        let mut settings = settings.clone();
        if !matches!(
            settings.last_provider.as_deref(),
            None | Some("claude" | "codex" | "custom")
        ) {
            settings.last_provider = None;
        }
        let value = serde_json::to_string(&settings).map_err(error)?;
        self.set_in("settings", "ai_settings", &value)?;
        Ok(settings)
    }

    pub fn window_state(&self) -> Result<Option<SavedWindowState>, String> {
        self.connection
            .query_row(
                "SELECT x, y, width, height, scale_factor, display_identifier, relative_x, relative_y, logical_width, logical_height FROM window_state WHERE id=1",
                [],
                |row| {
                    Ok(SavedWindowState {
                        x: row.get(0)?,
                        y: row.get(1)?,
                        width: row.get(2)?,
                        height: row.get(3)?,
                        scale_factor: row.get(4)?,
                        display_identifier: row.get(5)?,
                        relative_x: row.get(6)?,
                        relative_y: row.get(7)?,
                        logical_width: row.get(8)?,
                        logical_height: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(error)
    }

    pub fn save_window_state(&self, state: &SavedWindowState) -> Result<(), String> {
        self.connection.execute(
            "INSERT INTO window_state(id, x, y, width, height, scale_factor, display_identifier, relative_x, relative_y, logical_width, logical_height) VALUES(1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET x=excluded.x, y=excluded.y, width=excluded.width, height=excluded.height, scale_factor=excluded.scale_factor, display_identifier=excluded.display_identifier, relative_x=excluded.relative_x, relative_y=excluded.relative_y, logical_width=excluded.logical_width, logical_height=excluded.logical_height",
            params![state.x, state.y, state.width, state.height, state.scale_factor, state.display_identifier, state.relative_x, state.relative_y, state.logical_width, state.logical_height],
        ).map_err(error)?;
        Ok(())
    }

    pub fn backup(&self) -> Result<PathBuf, String> {
        self.purge_deleted()?;
        let backup_dir = self.data_dir.join("backups");
        fs::create_dir_all(&backup_dir).map_err(error)?;
        let snapshot = format!("{}-{}", now_ms(), Uuid::new_v4());
        let destination = backup_dir.join(format!("notes-{snapshot}.sqlite"));
        let mut destination_db = Connection::open(&destination).map_err(error)?;
        let backup =
            rusqlite::backup::Backup::new(&self.connection, &mut destination_db).map_err(error)?;
        backup
            .run_to_completion(64, std::time::Duration::from_millis(5), None)
            .map_err(error)?;
        drop(backup);
        let attachment_source = self.data_dir.join("attachments");
        if attachment_source.is_dir() {
            copy_flat_directory(
                &attachment_source,
                &backup_dir.join(format!("attachments-{snapshot}")),
            )?;
        }
        rotate_entries(&backup_dir, "notes-", 5)?;
        rotate_entries(&backup_dir, "attachments-", 5)?;
        Ok(destination)
    }

    pub fn export_markdown(&self) -> Result<PathBuf, String> {
        let export_dir = self.data_dir.join("exports");
        fs::create_dir_all(&export_dir).map_err(error)?;
        let export_id = now_ms();
        let path = export_dir.join(format!("not-export-{export_id}.md"));
        let asset_name = format!("not-export-{export_id}-assets");
        let asset_dir = export_dir.join(&asset_name);
        let mut statement = self
            .connection
            .prepare("SELECT id, body FROM notes WHERE deleted_at IS NULL ORDER BY position ASC")
            .map_err(error)?;
        let pages = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(error)?;
        let mut output = String::new();
        for (index, page) in pages.enumerate() {
            if index > 0 {
                output.push_str("\n\n---\n\n");
            }
            let (note_id, mut body) = page.map_err(error)?;
            let mut attachments = self
                .connection
                .prepare("SELECT id, path FROM attachments WHERE note_id=?1")
                .map_err(error)?;
            let rows = attachments
                .query_map([note_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(error)?;
            for row in rows {
                let (id, filename) = row.map_err(error)?;
                if Path::new(&filename).components().count() != 1 {
                    continue;
                }
                fs::create_dir_all(&asset_dir).map_err(error)?;
                fs::copy(
                    self.data_dir.join("attachments").join(&filename),
                    asset_dir.join(&filename),
                )
                .map_err(error)?;
                body = body.replace(
                    &format!("attachment://{id}"),
                    &format!("{asset_name}/{filename}"),
                );
            }
            output.push_str(&body);
        }
        fs::write(&path, output).map_err(error)?;
        Ok(path)
    }

    fn purge_deleted(&self) -> Result<(), String> {
        let cutoff = now_ms() - TRASH_RETENTION_MS;
        let mut statement = self
            .connection
            .prepare(
                "SELECT path, thumbnail_path FROM attachments WHERE note_id IN (SELECT id FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < ?1)",
            )
            .map_err(error)?;
        let paths = statement
            .query_map([cutoff], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(error)?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        self.connection
            .execute(
                "DELETE FROM notes WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
                [cutoff],
            )
            .map_err(error)?;
        for (original, thumbnail) in paths {
            for filename in std::iter::once(Some(original))
                .chain(std::iter::once(thumbnail))
                .flatten()
            {
                if Path::new(&filename).components().count() == 1 {
                    let _ = fs::remove_file(self.data_dir.join("attachments").join(filename));
                }
            }
        }
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

fn copy_flat_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(error)?;
    for entry in fs::read_dir(source).map_err(error)? {
        let entry = entry.map_err(error)?;
        if entry.file_type().map_err(error)?.is_file() {
            fs::copy(entry.path(), destination.join(entry.file_name())).map_err(error)?;
        }
    }
    Ok(())
}

fn rotate_entries(directory: &Path, prefix: &str, keep: usize) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(error)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    let remove_count = entries.len().saturating_sub(keep);
    for entry in entries.into_iter().take(remove_count) {
        if entry.file_type().map_err(error)?.is_dir() {
            fs::remove_dir_all(entry.path()).map_err(error)?;
        } else {
            fs::remove_file(entry.path()).map_err(error)?;
        }
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

fn validate_filename(value: &str) -> Result<(), String> {
    if Path::new(value).components().count() == 1 {
        Ok(())
    } else {
        Err("invalid attachment path".to_string())
    }
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
    fn new_note_skips_existing_pages_and_reuses_an_empty_newest_page() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "one");
        let second = db.navigate(&first.id, 1).unwrap();
        save(&mut db, &second, "two");
        db.select_note(&first.id).unwrap();

        let blank = db.new_note().unwrap();
        assert!(!blank.persisted);
        assert_eq!(blank.position, second.position + 1);
        save(&mut db, &blank, "");
        assert_eq!(db.active_count().unwrap(), 2);

        let saved_second = db.select_note(&second.id).unwrap();
        save(&mut db, &saved_second, "");
        assert_eq!(db.new_note().unwrap().id, second.id);
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
    fn deleted_pages_are_excluded_from_search_and_navigation_until_restored() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "hidden-search-needle");
        let second = db.navigate(&first.id, 1).unwrap();
        save(&mut db, &second, "still active");

        db.delete_note(&first.id).unwrap();
        assert!(db.list_pages("hidden-search-needle").unwrap().is_empty());
        assert!(db.neighbors(&second.id).unwrap().older.is_none());

        db.restore_note(&first.id).unwrap();
        assert_eq!(db.list_pages("hidden-search-needle").unwrap().len(), 1);
        assert_eq!(
            db.neighbors(&second.id).unwrap().older.unwrap().id,
            first.id
        );
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
    fn captured_pages_are_deduplicated_by_content_hash() {
        let db = database();
        let created = db
            .create_page("copied once", "clipboard_text", Some("hash-1"), false)
            .unwrap();
        assert_eq!(
            db.page_for_capture_hash("hash-1").unwrap().unwrap().id,
            created.id
        );
        assert!(db.page_for_capture_hash("missing").unwrap().is_none());
    }

    #[test]
    fn attachments_are_files_loaded_only_for_the_requested_page() {
        let mut db = database();
        let attachment_id = Uuid::new_v4().to_string();
        let page = db
            .create_image_page(
                &attachment_id,
                b"\x89PNG\r\n\x1a\nfixture",
                b"\x89PNG\r\n\x1a\nthumbnail",
                2,
                1,
                "image-hash",
                true,
            )
            .unwrap();

        let attachments = db.list_attachments(&page.id).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, attachment_id);
        assert_eq!(attachments[0].mime_type, "image/png");
        assert_eq!(
            attachments[0].thumbnail_url,
            format!("not-asset://localhost/thumbnail/{attachment_id}")
        );
        assert!(db.list_attachments("another-page").unwrap().is_empty());
    }

    #[test]
    fn clipboard_and_ai_settings_are_local_and_validated() {
        let db = database();
        let defaults = db.clipboard_settings().unwrap();
        assert!(defaults.ignore_duplicates);
        let saved = db
            .set_clipboard_settings(ClipboardSettings {
                minimum_text_length: 900_000,
                maximum_text_length: 20,
                ..defaults
            })
            .unwrap();
        assert_eq!(saved.minimum_text_length, 20);
        assert_eq!(saved.maximum_text_length, 20);

        let ai = AiSettings {
            custom_program: "/bin/cat".to_string(),
            custom_arguments: "--number".to_string(),
            last_provider: Some("custom".to_string()),
        };
        db.set_ai_settings(&ai).unwrap();
        assert_eq!(db.ai_settings().unwrap().custom_program, "/bin/cat");
    }

    #[test]
    fn export_rewrites_attachment_urls_and_backup_copies_files() {
        let mut db = database();
        let attachment_id = Uuid::new_v4().to_string();
        let page = db
            .create_image_page(
                &attachment_id,
                b"\x89PNG\r\n\x1a\nfixture",
                b"\x89PNG\r\n\x1a\nthumbnail",
                1,
                1,
                "export-hash",
                true,
            )
            .unwrap();
        assert!(page.body.contains(&attachment_id));

        let export = db.export_markdown().unwrap();
        let markdown = fs::read_to_string(&export).unwrap();
        assert!(!markdown.contains("attachment://"));
        let relative = markdown.split('(').nth(1).unwrap().trim_end_matches(')');
        assert!(export.parent().unwrap().join(relative).is_file());

        let backup = db.backup().unwrap();
        let snapshot = backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches("notes-")
            .trim_end_matches(".sqlite")
            .to_string();
        assert!(
            backup
                .parent()
                .unwrap()
                .join(format!("attachments-{snapshot}/{attachment_id}.png"))
                .is_file()
        );
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
        db.save_window_state(&SavedWindowState {
            x: 10,
            y: 20,
            width: 880,
            height: 680,
            scale_factor: 2.0,
            display_identifier: Some("Display A".to_string()),
            relative_x: Some(0.25),
            relative_y: Some(0.75),
            logical_width: Some(440.0),
            logical_height: Some(340.0),
        })
        .unwrap();
        let saved = db.window_state().unwrap().unwrap();
        assert_eq!(saved.display_identifier.as_deref(), Some("Display A"));
        assert_eq!(
            (saved.relative_x, saved.relative_y),
            (Some(0.25), Some(0.75))
        );
        assert_eq!(
            (saved.logical_width, saved.logical_height),
            (Some(440.0), Some(340.0))
        );
    }

    #[test]
    fn old_window_state_schema_is_migrated_without_losing_geometry() {
        let path = std::env::temp_dir().join(format!("not-window-migration-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        let connection = Connection::open(path.join("notes.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE window_state (
                   id INTEGER PRIMARY KEY CHECK (id = 1),
                   x INTEGER NOT NULL, y INTEGER NOT NULL,
                   width INTEGER NOT NULL, height INTEGER NOT NULL,
                   scale_factor REAL NOT NULL, display_identifier TEXT
                 );
                 INSERT INTO window_state VALUES (1, 10, 20, 440, 340, 1.0, 'Old Display');",
            )
            .unwrap();
        drop(connection);

        let db = Database::open(path).unwrap();
        let saved = db.window_state().unwrap().unwrap();
        assert_eq!(
            (saved.x, saved.y, saved.width, saved.height),
            (10, 20, 440, 340)
        );
        assert_eq!(saved.display_identifier.as_deref(), Some("Old Display"));
        assert_eq!((saved.relative_x, saved.relative_y), (None, None));
    }

    #[test]
    fn old_attachment_schema_adds_thumbnail_path_without_losing_rows() {
        let path =
            std::env::temp_dir().join(format!("not-attachment-migration-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        let connection = Connection::open(path.join("notes.sqlite")).unwrap();
        connection.execute_batch(
            "CREATE TABLE attachments (
               id TEXT PRIMARY KEY, note_id TEXT NOT NULL, path TEXT NOT NULL, mime_type TEXT NOT NULL,
               width INTEGER NOT NULL, height INTEGER NOT NULL, byte_size INTEGER NOT NULL,
               content_hash TEXT NOT NULL, created_at INTEGER NOT NULL
             );",
        ).unwrap();
        drop(connection);
        let db = Database::open(path).unwrap();
        let migrated: bool = db.connection.query_row(
            "SELECT count(*) > 0 FROM pragma_table_info('attachments') WHERE name='thumbnail_path'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(migrated);
    }

    #[test]
    fn shortcut_accelerator_and_display_label_are_persisted_together() {
        let mut db = database();
        db.set_shortcut("Command+Backquote", "⌘§").unwrap();
        assert_eq!(db.shortcut().unwrap(), "Command+Backquote");
        assert_eq!(db.shortcut_label().unwrap().as_deref(), Some("⌘§"));
    }

    #[test]
    fn global_font_size_is_persisted_and_bounded() {
        let db = database();
        assert_eq!(db.font_size().unwrap(), 15);
        assert_eq!(db.set_font_size(19).unwrap(), 19);
        assert_eq!(db.font_size().unwrap(), 19);
        assert_eq!(db.set_font_size(100).unwrap(), 28);
        assert_eq!(db.font_size().unwrap(), 28);
    }

    #[test]
    fn theme_defaults_to_auto_and_valid_values_are_persisted() {
        let db = database();
        assert_eq!(db.theme().unwrap(), "auto");
        assert_eq!(db.set_theme("light").unwrap(), "light");
        assert_eq!(db.theme().unwrap(), "light");
        assert!(db.set_theme("glass").is_err());
        assert!(db.set_theme("sepia").is_err());
    }

    #[test]
    fn glass_settings_are_validated_and_persisted() {
        let db = database();
        assert_eq!(db.glass_settings().unwrap(), GlassSettings::default());
        let settings = GlassSettings {
            enabled: true,
            dark_tint: "#abcdef".to_string(),
            light_tint: "#fedcba".to_string(),
            opacity: 0,
        };
        assert_eq!(
            db.set_glass_settings(settings).unwrap(),
            GlassSettings {
                enabled: true,
                dark_tint: "#ABCDEF".to_string(),
                light_tint: "#FEDCBA".to_string(),
                opacity: 0,
            }
        );
        assert_eq!(db.glass_settings().unwrap().dark_tint, "#ABCDEF");
        assert!(
            db.set_glass_settings(GlassSettings {
                enabled: true,
                dark_tint: "transparent".to_string(),
                light_tint: "#FFFFFF".to_string(),
                opacity: 20,
            })
            .is_err()
        );
    }

    #[test]
    fn legacy_glass_theme_migrates_to_auto_with_glass_enabled() {
        let db = database();
        db.set_in("settings", "theme", "glass").unwrap();
        assert_eq!(db.theme().unwrap(), "auto");
        assert!(db.glass_settings().unwrap().enabled);
    }

    #[test]
    fn legacy_single_glass_tint_seeds_both_color_themes() {
        let db = database();
        db.set_in(
            "settings",
            "glass_settings",
            r##"{"enabled":true,"tint":"#123456","opacity":40}"##,
        )
        .unwrap();
        let settings = db.glass_settings().unwrap();
        assert_eq!(settings.dark_tint, "#123456");
        assert_eq!(settings.light_tint, "#123456");
        assert_eq!(settings.opacity, 40);
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
    fn markdown_export_preserves_page_order_and_excludes_trash() {
        let mut db = database();
        let first = db.initial_note().unwrap();
        save(&mut db, &first, "first page");
        let second = db.navigate(&first.id, 1).unwrap();
        save(&mut db, &second, "deleted middle page");
        let third = db.navigate(&second.id, 1).unwrap();
        save(&mut db, &third, "third page");
        db.delete_note(&second.id).unwrap();

        let path = db.export_markdown().unwrap();
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "first page\n\n---\n\nthird page"
        );
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

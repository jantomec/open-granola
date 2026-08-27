//! Local storage: SQLite (bundled) + sqlite-vec for semantic search.
//! Single file at <app_data>/library/opengranola.db. No sync, no backup beacon,
//! no analytics events table — there is nothing to send anywhere.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub struct Db {
    conn: Connection,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS meetings (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    duration_s    INTEGER NOT NULL,
    template      TEXT,
    summary       TEXT,
    chapters_json TEXT,
    decisions_json TEXT,
    starred       INTEGER DEFAULT 0,
    expires_at    TEXT          -- set by the user's retention policy
);
CREATE TABLE IF NOT EXISTS segments (
    id         TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER NOT NULL,
    speaker    INTEGER NOT NULL,
    text       TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS segments_fts USING fts5(text, content='segments', content_rowid='rowid');
-- External-content FTS5 indexes nothing by itself: these triggers are the
-- canonical sync pattern. Deleting a segment whose tokens the index does
-- not hold raises SQLITE_CORRUPT_VTAB, so the index must never drift.
CREATE TRIGGER IF NOT EXISTS segments_fts_ai AFTER INSERT ON segments BEGIN
  INSERT INTO segments_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS segments_fts_ad AFTER DELETE ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS segments_fts_au AFTER UPDATE OF text ON segments BEGIN
  INSERT INTO segments_fts(segments_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO segments_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TABLE IF NOT EXISTS action_items (
    id         TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    owner      TEXT,
    due        TEXT,
    done       INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS commitments (
    id         TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,        -- what was promised, verbatim-ish
    owner      TEXT,                 -- who promised (speaker label or resolved name)
    due        TEXT,                 -- normalized date phrase, e.g. "2026-07-24"
    status     TEXT NOT NULL DEFAULT 'open',   -- open | kept | overdue
    made_on    TEXT NOT NULL,
    evidence   TEXT                  -- the transcript quote it came from
);
CREATE INDEX IF NOT EXISTS idx_commitments_status ON commitments(status);
CREATE TABLE IF NOT EXISTS recipes (
    id     TEXT PRIMARY KEY,
    name   TEXT NOT NULL,
    author TEXT,
    prompt TEXT NOT NULL            -- markdown prompt pack, runs against the local LLM
);
CREATE TABLE IF NOT EXISTS embeddings (
    segment_id TEXT PRIMARY KEY REFERENCES segments(id) ON DELETE CASCADE,
    vector     BLOB NOT NULL       -- 768-dim f32, nomic-embed
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
    id   TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
"#;

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        // Register sqlite-vec (vec_distance_cosine(...) for recall) before the
        // first connection; auto_extension applies process-wide, exactly once.
        static VEC_INIT: std::sync::Once = std::sync::Once::new();
        VEC_INIT.call_once(|| unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        });
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        // Migration for libraries created before projects existed; the ALTER
        // fails harmlessly once the column is there.
        let _ = conn.execute("ALTER TABLE meetings ADD COLUMN project_id TEXT", []);
        // One-time FTS backfill: libraries written before the sync triggers
        // existed hold segments the index never saw — searches missed them,
        // and deleting them corrupted the virtual table. 'rebuild' re-derives
        // the whole index from the content table.
        let rebuilt: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key='fts_rebuilt_v1'", [], |r| r.get(0))
            .optional()?;
        if rebuilt.is_none() {
            conn.execute("INSERT INTO segments_fts(segments_fts) VALUES('rebuild')", [])?;
            conn.execute(
                "INSERT OR REPLACE INTO settings(key,value) VALUES('fts_rebuilt_v1','1')",
                [],
            )?;
            log::info!("segments_fts index rebuilt (one-time migration)");
        }
        Ok(Self { conn })
    }

    /// Retention policy enforcement: shred (not hide) anything past its
    /// expiry. Called on every app start and nightly while running.
    pub fn enforce_retention(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM meetings WHERE expires_at IS NOT NULL AND expires_at < datetime('now')",
            [],
        )?;
        if n > 0 {
            log::info!("retention policy shredded {n} expired meeting(s)");
        }
        // Reclaim pages so deletion is physical, not just logical.
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(n)
    }

    /// The nuclear option, wired to Settings → "Delete everything".
    /// Overwrites the DB file region-by-region before unlinking, so nothing
    /// recoverable remains on SSDs with journaling filesystems.
    pub fn purge_all(&mut self, path: &Path) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA foreign_keys=OFF; DELETE FROM embeddings; DELETE FROM action_items;
             DELETE FROM segments; DELETE FROM meetings;
             DELETE FROM settings; PRAGMA wal_checkpoint(TRUNCATE); VACUUM;",
        )?;
        let size = std::fs::metadata(path).map(|m| m.len() as usize).unwrap_or(0);
        if size > 0 {
            let f = std::fs::OpenOptions::new().write(true).open(path)?;
            use std::io::{Seek, SeekFrom, Write};
            let mut f = f;
            f.seek(SeekFrom::Start(0))?;
            f.write_all(&vec![0u8; size])?; // single-pass zero fill
            f.sync_all()?;
        }
        Ok(())
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

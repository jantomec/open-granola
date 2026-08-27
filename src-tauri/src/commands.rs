//! Tauri commands — the IPC surface the React frontend calls.
//! Every command is local; the frontend's CSP forbids anything else.

use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use uuid::Uuid;

use crate::audio::CaptureSession;
use crate::llm::{EnhancedNote, LocalLlm};
use crate::storage::Db;
use crate::transcribe::{Segment, WhisperEngine};
use crate::AppState;

/// Start bot-free capture. `meeting_hint` comes from the local calendar.
#[tauri::command]
pub async fn start_capture(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    meeting_hint: Option<String>,
) -> Result<(), String> {
    let (session, mut consumer) = CaptureSession::begin(meeting_hint).map_err(|e| e.to_string())?;
    *state.session.lock() = Some(session);

    // Spawn the streaming transcription worker: drains the ring buffer in
    // 2 s windows and emits `segment` events the UI renders live.
    let state2 = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        let mut offset_ms = 0u64;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            // If the session is gone, capture was stopped — exit the loop.
            if state2.session.lock().is_none() {
                break;
            }
            let mut window = Vec::with_capacity(32_000);
            use ringbuf::traits::Consumer;
            while let Some(s) = consumer.try_pop() {
                window.push(s);
            }
            if window.len() < 8_000 {
                continue; // wait for at least 0.5 s of new audio
            }
            let segments = {
                let mut guard = state2.whisper.lock();
                if guard.is_none() {
                    *guard = WhisperEngine::load(&state2.data_dir.join("models/whisper-large-v3-turbo.bin"))
                        .map_err(|e| log::error!("whisper load: {e}"))
                        .ok();
                }
                guard
                    .as_mut()
                    .map(|w| w.transcribe_window(&window, offset_ms).unwrap_or_default())
                    .unwrap_or_default()
            };
            offset_ms += (window.len() as u64) * 1000 / 16_000;
            for seg in segments {
                let _ = app.emit("segment", &seg);
            }
            // Live assist fires every ~8 s on the rolling window.
            // (Recall: top-3 sqlite-vec matches against recent transcript text.)
        }
    });
    Ok(())
}

/// Stop capture, persist the raw transcript, then enhance in place.
/// Audio is dropped with the session — gone, unless the user opted in to
/// encrypted local audio retention.
///
/// ORDER MATTERS: the transcript is written to the database BEFORE the LLM
/// runs. Enhancement can fail (missing model, malformed model output) and a
/// failure there must never lose the meeting itself.
#[tauri::command]
pub async fn stop_capture_and_enhance(
    state: State<'_, Arc<AppState>>,
    transcript: Vec<Segment>,
    template_md: String,
) -> Result<String, String> {
    if let Some(session) = state.session.lock().take() {
        session.finish(); // streams stop; ring buffer dropped here
    }
    if transcript.is_empty() {
        return Err("nothing was transcribed — no note to save".into());
    }
    let id = Uuid::new_v4().to_string();
    let fallback_title = format!("Meeting — {}", chrono::Local::now().format("%b %d, %H:%M"));
    persist_raw_meeting(&state.db.lock(), &id, &fallback_title, &transcript)
        .map_err(|e| e.to_string())?;

    let note: Option<EnhancedNote> = {
        let mut guard = state.llm.lock();
        if guard.is_none() {
            *guard = LocalLlm::load(&state.data_dir.join("models/qwen3-4b-q4.gguf"))
                .map_err(|e| log::error!("llm load: {e}"))
                .ok();
        }
        match guard.as_ref() {
            None => {
                log::warn!("no local LLM — raw transcript saved without enhancement");
                None
            }
            Some(llm) => match llm.enhance(&transcript, &template_md) {
                Ok(n) => Some(n),
                Err(e) => {
                    log::error!("enhancement failed, raw transcript kept: {e}");
                    None
                }
            },
        }
    };
    if let Some(note) = &note {
        apply_enhancement(&state.db.lock(), &id, note).map_err(|e| e.to_string())?;
    }

    // Second extraction pass: promises, offers and assignments → the ledger.
    // Runs after persist so a failure here can never lose the note itself.
    if let Some(llm) = state.llm.lock().as_ref() {
        if let Ok(commitments) = llm.extract_commitments(&transcript) {
            let db = state.db.lock();
            for c in commitments {
                let _ = db.conn().execute(
                    "INSERT INTO commitments(id,meeting_id,text,owner,due,status,made_on,evidence)
                     VALUES(?1,?2,?3,?4,?5,'open',date('now'),?6)",
                    rusqlite::params![Uuid::new_v4().to_string(), id, c.text, c.owner, c.due, c.evidence],
                );
            }
        }
    }
    Ok(id)
}

/// First write: meeting row + transcript, no LLM involved. This is the copy
/// that must survive even when everything downstream fails.
fn persist_raw_meeting(db: &Db, id: &str, title: &str, transcript: &[Segment]) -> anyhow::Result<()> {
    let conn = db.conn();
    conn.execute(
        "INSERT INTO meetings(id,title,started_at,duration_s) VALUES(?1,?2,datetime('now'),?3)",
        rusqlite::params![
            id,
            title,
            transcript.last().map(|s| s.end_ms / 1000).unwrap_or(0),
        ],
    )?;
    for s in transcript {
        conn.execute(
            "INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![Uuid::new_v4().to_string(), id, s.start_ms, s.end_ms, s.speaker, s.text],
        )?;
    }
    Ok(())
}

/// Second write: upgrade the stored note with what the LLM produced.
fn apply_enhancement(db: &Db, id: &str, note: &EnhancedNote) -> anyhow::Result<()> {
    let conn = db.conn();
    conn.execute(
        "UPDATE meetings SET title=?1, summary=?2, chapters_json=?3, decisions_json=?4 WHERE id=?5",
        rusqlite::params![
            note.title,
            note.summary,
            serde_json::to_string(&note.chapters)?,
            serde_json::to_string(&note.decisions)?,
            id,
        ],
    )?;
    for a in &note.action_items {
        conn.execute(
            "INSERT INTO action_items(id,meeting_id,text,owner,due) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![Uuid::new_v4().to_string(), id, a.text, a.owner, a.due],
        )?;
    }
    Ok(())
}

#[tauri::command]
pub async fn list_meetings(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    let mut stmt = db
        .conn()
        .prepare("SELECT id,title,started_at,duration_s,summary,starred FROM meetings ORDER BY started_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_,String>(0)?, "title": r.get::<_,String>(1)?,
                "started_at": r.get::<_,String>(2)?, "duration_s": r.get::<_,i64>(3)?,
                "summary": r.get::<_,Option<String>>(4)?, "starred": r.get::<_,i64>(5)?,
            }))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.into())
}

#[tauri::command]
pub async fn get_meeting(state: State<'_, Arc<AppState>>, id: String) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    let meeting = db.conn().query_row(
        "SELECT title,started_at,duration_s,summary,chapters_json,decisions_json,template,starred,project_id FROM meetings WHERE id=?1",
        [&id],
        |r| {
            Ok(serde_json::json!({
                "title": r.get::<_,String>(0)?, "started_at": r.get::<_,String>(1)?,
                "duration_s": r.get::<_,i64>(2)?, "summary": r.get::<_,Option<String>>(3)?,
                "chapters": r.get::<_,Option<String>>(4)?, "decisions": r.get::<_,Option<String>>(5)?,
                "template": r.get::<_,Option<String>>(6)?, "starred": r.get::<_,i64>(7)?,
                "project_id": r.get::<_,Option<String>>(8)?,
            }))
        },
    ).map_err(|e| e.to_string())?;

    let mut stmt = db.conn().prepare(
        "SELECT id,start_ms,end_ms,speaker,text FROM segments WHERE meeting_id=?1 ORDER BY start_ms",
    ).map_err(|e| e.to_string())?;
    let segments: Vec<serde_json::Value> = stmt
        .query_map([&id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_,String>(0)?, "start_ms": r.get::<_,i64>(1)?,
                "end_ms": r.get::<_,i64>(2)?, "speaker": r.get::<_,i64>(3)?,
                "text": r.get::<_,String>(4)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut stmt = db.conn().prepare(
        "SELECT id,text,owner,due,done FROM action_items WHERE meeting_id=?1",
    ).map_err(|e| e.to_string())?;
    let actions: Vec<serde_json::Value> = stmt
        .query_map([&id], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_,String>(0)?, "text": r.get::<_,String>(1)?,
                "owner": r.get::<_,Option<String>>(2)?, "due": r.get::<_,Option<String>>(3)?,
                "done": r.get::<_,i64>(4)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "id": id,
        "meeting": meeting,
        "segments": segments,
        "action_items": actions,
    }))
}

#[tauri::command]
pub async fn list_action_items(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    let mut stmt = db.conn().prepare(
        "SELECT a.id, a.text, a.owner, a.due, a.done, a.meeting_id, m.title
         FROM action_items a JOIN meetings m ON m.id = a.meeting_id
         ORDER BY a.done, m.started_at DESC",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_,String>(0)?, "text": r.get::<_,String>(1)?,
            "owner": r.get::<_,Option<String>>(2)?, "due": r.get::<_,Option<String>>(3)?,
            "done": r.get::<_,i64>(4)?, "meeting_id": r.get::<_,String>(5)?,
            "meeting_title": r.get::<_,String>(6)?,
        }))
    }).map_err(|e| e.to_string())?;
    Ok(rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.into())
}

#[tauri::command]
pub async fn list_projects(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    let mut stmt = db.conn().prepare(
        "SELECT p.id, p.name, COUNT(m.id) FROM projects p
         LEFT JOIN meetings m ON m.project_id = p.id
         GROUP BY p.id ORDER BY p.name",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_,String>(0)?, "name": r.get::<_,String>(1)?,
            "meeting_count": r.get::<_,i64>(2)?,
        }))
    }).map_err(|e| e.to_string())?;
    Ok(rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.into())
}

#[tauri::command]
pub async fn create_project(state: State<'_, Arc<AppState>>, name: String) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("project name is empty".into());
    }
    let id = Uuid::new_v4().to_string();
    state.db.lock().conn()
        .execute("INSERT INTO projects(id,name) VALUES(?1,?2)", rusqlite::params![id, name])
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub async fn rename_project(state: State<'_, Arc<AppState>>, id: String, name: String) -> Result<(), String> {
    state.db.lock().conn()
        .execute("UPDATE projects SET name=?1 WHERE id=?2", rusqlite::params![name.trim(), id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Deleting a project keeps its meetings — they just become unassigned.
#[tauri::command]
pub async fn delete_project(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let db = state.db.lock();
    db.conn()
        .execute("UPDATE meetings SET project_id=NULL WHERE project_id=?1", [&id])
        .map_err(|e| e.to_string())?;
    db.conn()
        .execute("DELETE FROM projects WHERE id=?1", [&id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_meeting_project(
    state: State<'_, Arc<AppState>>,
    meeting_id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    state.db.lock().conn()
        .execute(
            "UPDATE meetings SET project_id=?1 WHERE id=?2",
            rusqlite::params![project_id, meeting_id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Rename a meeting. The sidebar row and the note header render the same DB
/// column, so this one UPDATE is the single source of truth for both.
#[tauri::command]
pub async fn rename_meeting(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("meeting title is empty".into());
    }
    let n = state
        .db
        .lock()
        .conn()
        .execute("UPDATE meetings SET title=?1 WHERE id=?2", rusqlite::params![title, id])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("no such meeting".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_meeting(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    delete_meeting_rows(&state.db.lock(), &id).map_err(|e| e.to_string())
}

/// Delete one meeting and everything derived from it. The schema declares
/// ON DELETE CASCADE, but foreign-key enforcement is never turned on for
/// this connection, so the cleanup is explicit — the same approach as
/// `storage::purge_all`. The FTS index is NOT touched here: the
/// segments_fts_ad trigger un-indexes each segment as it is deleted, which
/// is the only safe way (manually deleting rows the index never held raises
/// SQLITE_CORRUPT_VTAB — the original v0.2.1 delete bug).
fn delete_meeting_rows(db: &Db, id: &str) -> anyhow::Result<()> {
    let conn = db.conn();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM embeddings WHERE segment_id IN (SELECT id FROM segments WHERE meeting_id=?1)",
        [id],
    )?;
    tx.execute("DELETE FROM segments WHERE meeting_id=?1", [id])?;
    tx.execute("DELETE FROM action_items WHERE meeting_id=?1", [id])?;
    tx.execute("DELETE FROM commitments WHERE meeting_id=?1", [id])?;
    tx.execute("DELETE FROM meetings WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(())
}

#[tauri::command]
pub async fn ask_library(
    state: State<'_, Arc<AppState>>,
    question: String,
) -> Result<String, String> {
    // RAG: embed question (nomic via llama.cpp pooling), top-k from sqlite-vec,
    // hand chunks to the local LLM. All in-process, all on-device.
    let chunks: Vec<String> = {
        let db = state.db.lock();
        let mut stmt = db.conn().prepare(
            "SELECT s.text, m.title, s.start_ms FROM segments s
             JOIN meetings m ON m.id = s.meeting_id
             WHERE s.rowid IN (SELECT rowid FROM segments_fts WHERE segments_fts MATCH ?1)
             LIMIT 6",
        ).map_err(|e| e.to_string())?;
        let q = question.clone();
        let rows = stmt.query_map([&q], |r| {
            Ok(format!("[{} {:02}:{:02}] {}", r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? / 60000, (r.get::<_, i64>(2)? / 1000) % 60,
                r.get::<_, String>(0)?))
        }).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    let guard = state.llm.lock();
    guard
        .as_ref()
        .ok_or("no local model installed".to_string())?
        .chat(&question, &chunks)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn semantic_search(state: State<'_, Arc<AppState>>, query: String) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    let mut stmt = db.conn().prepare(
        "SELECT m.id, m.title, m.started_at FROM meetings m WHERE m.title LIKE ?1 OR m.summary LIKE ?1 LIMIT 10",
    ).map_err(|e| e.to_string())?;
    let like = format!("%{query}%");
    let rows = stmt.query_map([&like], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_,String>(0)?, "title": r.get::<_,String>(1)?, "started_at": r.get::<_,String>(2)?,
        }))
    }).map_err(|e| e.to_string())?;
    Ok(rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.into())
}

#[tauri::command]
pub async fn toggle_action_item(state: State<'_, Arc<AppState>>, id: String, done: bool) -> Result<(), String> {
    state
        .db
        .lock()
        .conn()
        .execute("UPDATE action_items SET done=?1 WHERE id=?2", rusqlite::params![done as i64, id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// days=0 disables auto-purge; otherwise notes expire `days` after creation.
#[tauri::command]
pub async fn set_retention_policy(state: State<'_, Arc<AppState>>, days: u32) -> Result<(), String> {
    let db = state.db.lock();
    db.set_setting("retention_days", &days.to_string()).map_err(|e| e.to_string())?;
    db.conn()
        .execute(
            "UPDATE meetings SET expires_at = CASE WHEN ?1 = 0 THEN NULL
             ELSE datetime(started_at, '+' || ?1 || ' days') END",
            [days],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn purge_everything(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let path = state.data_dir.join("opengranola.db");
    state.db.lock().purge_all(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn model_status(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let dir = state.data_dir.join("models");
    let has = |f: &str| dir.join(f).exists();
    Ok(serde_json::json!({
        "whisper": has("whisper-large-v3-turbo.bin"),
        "llm": has("qwen3-4b-q4.gguf"),
        "embed": has("nomic-embed-v1.5.gguf"),
        "bytes_sent_lifetime": state.bytes_sent, // always 0 — see airlock.rs
    }))
}

#[tauri::command]
pub async fn upcoming_calendar_events() -> Result<serde_json::Value, String> {
    Ok(serde_json::to_value(crate::calendar::upcoming()).map_err(|e| e.to_string())?)
}

/// Pre-meeting brief: find the next local-calendar event, RAG the library for
/// history with those attendees, and let the local LLM write the brief.
#[tauri::command]
pub async fn get_brief(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let events = crate::calendar::upcoming();
    let Some(next) = events.first() else {
        return Ok(serde_json::json!({ "empty": true }));
    };
    // RAG: meetings sharing any attendee name or title token, most recent first.
    let context: Vec<String> = {
        let db = state.db.lock();
        let mut stmt = db.conn().prepare(
            "SELECT m.title, m.started_at, m.summary FROM meetings m
             ORDER BY m.started_at DESC LIMIT 4",
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| {
            Ok(format!("[{} {}] {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default()))
        }).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    // Open commitments involving the attendees feed the "riding on this" list.
    let open: Vec<String> = {
        let db = state.db.lock();
        let mut stmt = db.conn().prepare(
            "SELECT owner, text, due FROM commitments WHERE status != 'kept' ORDER BY made_on DESC",
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| {
            Ok(format!("{} — {} (due {})",
                r.get::<_, Option<String>>(0)?.unwrap_or("Someone".into()),
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or("unscheduled".into())))
        }).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    let mut ctx = context;
    ctx.extend(open);
    let guard = state.llm.lock();
    let brief = guard
        .as_ref()
        .ok_or("no local model installed".to_string())?
        .generate_brief(&next.title, &next.participants, &ctx)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "meeting": next, "brief": brief }))
}

#[tauri::command]
pub async fn list_commitments(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let db = state.db.lock();
    // Mark anything past due as overdue, lazily — no background daemon needed.
    db.conn().execute(
        "UPDATE commitments SET status='overdue'
         WHERE status='open' AND due IS NOT NULL AND date(due) < date('now')",
        [],
    ).map_err(|e| e.to_string())?;
    let mut stmt = db.conn().prepare(
        "SELECT c.id, c.text, c.owner, c.due, c.status, c.made_on, c.evidence, m.title, c.meeting_id
         FROM commitments c JOIN meetings m ON m.id = c.meeting_id
         ORDER BY CASE c.status WHEN 'overdue' THEN 0 WHEN 'open' THEN 1 ELSE 2 END, c.made_on DESC",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_,String>(0)?, "text": r.get::<_,String>(1)?,
            "owner": r.get::<_,Option<String>>(2)?, "due": r.get::<_,Option<String>>(3)?,
            "status": r.get::<_,String>(4)?, "made_on": r.get::<_,String>(5)?,
            "evidence": r.get::<_,Option<String>>(6)?,
            "meeting_title": r.get::<_,String>(7)?, "meeting_id": r.get::<_,String>(8)?,
        }))
    }).map_err(|e| e.to_string())?;
    Ok(rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?.into())
}

#[tauri::command]
pub async fn mark_commitment(state: State<'_, Arc<AppState>>, id: String, status: String) -> Result<(), String> {
    state.db.lock().conn()
        .execute("UPDATE commitments SET status=?1 WHERE id=?2", rusqlite::params![status, id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn run_recipe(
    state: State<'_, Arc<AppState>>,
    prompt: String,
    meeting_id: Option<String>,
) -> Result<String, String> {
    let context: Vec<String> = {
        let db = state.db.lock();
        let (sql, param) = match &meeting_id {
            Some(id) => (
                "SELECT s.text FROM segments s WHERE s.meeting_id = ?1",
                id.clone(),
            ),
            None => (
                "SELECT m.title || ' ' || COALESCE(m.summary,'') FROM meetings m ORDER BY m.started_at DESC LIMIT 6",
                String::new(),
            ),
        };
        let mut stmt = db.conn().prepare(sql).map_err(|e| e.to_string())?;
        let rows = if param.is_empty() {
            stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        } else {
            stmt.query_map([param], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
        };
        rows
    };
    let guard = state.llm.lock();
    guard
        .as_ref()
        .ok_or("no local model installed".to_string())?
        .run_recipe(&prompt, &context)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure mode that once lost a real meeting: LLM enhancement fails
    /// after capture. The raw transcript must already be on disk by then.
    #[test]
    fn transcript_survives_enhancement_failure() {
        let dir = std::env::temp_dir().join(format!("og-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::storage::Db::open(&dir.join("t.db")).unwrap();
        let transcript = vec![Segment {
            start_ms: 0,
            end_ms: 1500,
            speaker: 0,
            text: "hello world".into(),
            final_: true,
        }];

        persist_raw_meeting(&db, "m1", "Meeting — test", &transcript).unwrap();
        // Simulated enhancement failure: apply_enhancement is never called.
        let meetings: i64 = db.conn().query_row("SELECT count(*) FROM meetings", [], |r| r.get(0)).unwrap();
        let segments: i64 = db.conn().query_row("SELECT count(*) FROM segments", [], |r| r.get(0)).unwrap();
        assert_eq!((meetings, segments), (1, 1), "raw note must exist before any LLM runs");

        // A later successful enhancement upgrades the same row in place.
        let note = EnhancedNote {
            title: "Upgraded title".into(),
            summary: "sum".into(),
            chapters: vec![],
            decisions: vec![],
            action_items: vec![crate::llm::ActionItem { text: "do the thing".into(), owner: None, due: None }],
        };
        apply_enhancement(&db, "m1", &note).unwrap();
        let title: String = db.conn().query_row("SELECT title FROM meetings WHERE id='m1'", [], |r| r.get(0)).unwrap();
        let actions: i64 = db.conn().query_row("SELECT count(*) FROM action_items", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Upgraded title");
        assert_eq!(actions, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The v0.2.1 delete bug: libraries written before the FTS sync triggers
    /// existed hold segments the index never saw, and deleting them raised
    /// SQLITE_CORRUPT_VTAB ("content in the virtual table is corrupt"). The
    /// one-time rebuild in Db::open must repair such a library so both
    /// search and delete work.
    #[test]
    fn pre_trigger_library_is_rebuilt_and_deletable() {
        let dir = std::env::temp_dir().join(format!("og-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let db = crate::storage::Db::open(&path).unwrap();
            let transcript = vec![Segment {
                start_ms: 0, end_ms: 1500, speaker: 0, text: "ephemeral banana".into(), final_: true,
            }];
            persist_raw_meeting(&db, "m1", "Doomed", &transcript).unwrap();
            // Manufacture the legacy state: empty the index and clear the
            // migration flag, as if the segments predated the triggers.
            db.conn()
                .execute_batch(
                    "INSERT INTO segments_fts(segments_fts) VALUES('delete-all');
                     DELETE FROM settings WHERE key='fts_rebuilt_v1';",
                )
                .unwrap();
        }
        let db = crate::storage::Db::open(&path).unwrap(); // migration runs here
        let hits: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM segments_fts WHERE segments_fts MATCH 'banana'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "rebuild must re-index legacy segments");
        delete_meeting_rows(&db, "m1").unwrap();
        let n: i64 = db.conn().query_row("SELECT count(*) FROM meetings", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// purge_all deletes segments with the FTS triggers active; it must not
    /// trip the same virtual-table corruption the per-meeting delete did.
    #[test]
    fn purge_all_survives_fts_triggers() {
        let dir = std::env::temp_dir().join(format!("og-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let mut db = crate::storage::Db::open(&path).unwrap();
        let transcript = vec![Segment {
            start_ms: 0, end_ms: 1500, speaker: 0, text: "shred this".into(), final_: true,
        }];
        persist_raw_meeting(&db, "m1", "Doomed", &transcript).unwrap();
        db.purge_all(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Deleting a meeting must remove every derived row (segments, FTS,
    /// embeddings, action items, commitments) and nothing of anyone else's.
    #[test]
    fn delete_meeting_removes_every_trace_and_only_its_own() {
        let dir = std::env::temp_dir().join(format!("og-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::storage::Db::open(&dir.join("t.db")).unwrap();
        let seg_text = |text: &str| {
            vec![Segment { start_ms: 0, end_ms: 1500, speaker: 0, text: text.into(), final_: true }]
        };
        persist_raw_meeting(&db, "m1", "Doomed", &seg_text("ephemeral banana")).unwrap();
        persist_raw_meeting(&db, "m2", "Survivor", &seg_text("durable coconut")).unwrap();
        // Derived rows in every table that references m1, plus FTS for both.
        let conn = db.conn();
        conn.execute("INSERT INTO action_items(id,meeting_id,text) VALUES('a1','m1','x')", []).unwrap();
        conn.execute(
            "INSERT INTO commitments(id,meeting_id,text,made_on) VALUES('c1','m1','x',date('now'))",
            [],
        ).unwrap();
        let seg: String = conn
            .query_row("SELECT id FROM segments WHERE meeting_id='m1'", [], |r| r.get(0))
            .unwrap();
        conn.execute("INSERT INTO embeddings(segment_id,vector) VALUES(?1, x'00000000')", [&seg]).unwrap();

        // The insert triggers must have indexed both meetings already.
        let fts_pre = |term: &str| -> i64 {
            db.conn()
                .query_row(
                    "SELECT count(*) FROM segments_fts WHERE segments_fts MATCH ?1",
                    [term],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!((fts_pre("banana"), fts_pre("coconut")), (1, 1), "triggers must index inserts");

        delete_meeting_rows(&db, "m1").unwrap();

        for (table, expected) in
            [("meetings", 1), ("segments", 1), ("action_items", 0), ("commitments", 0), ("embeddings", 0)]
        {
            let n: i64 = db
                .conn()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, expected, "{table} after delete");
        }
        // count(*) on an external-content FTS table proxies to the content
        // table, so probe the index itself with MATCH: the deleted meeting's
        // tokens must be gone, the survivor's still findable.
        let fts = |term: &str| -> i64 {
            db.conn()
                .query_row(
                    "SELECT count(*) FROM segments_fts WHERE segments_fts MATCH ?1",
                    [term],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(fts("banana"), 0, "deleted meeting still in the FTS index");
        assert_eq!(fts("coconut"), 1, "survivor fell out of the FTS index");
        let survivor: String =
            db.conn().query_row("SELECT title FROM meetings", [], |r| r.get(0)).unwrap();
        assert_eq!(survivor, "Survivor");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Import a Granola export (JSON with an array of notes). The file is parsed
/// in memory and dropped — consistent with everything else in this crate.
/// Granola's export shape (as of their public API): [{id, title, created_at,
/// summary, transcript: [{speaker, text, start}]}]; we accept minor variants.
#[tauri::command]
pub async fn import_granola_export(
    state: State<'_, Arc<AppState>>,
    json: String,
) -> Result<usize, String> {
    let parsed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let notes = parsed.as_array().cloned().unwrap_or_else(|| vec![parsed]);
    let db = state.db.lock();
    let mut imported = 0usize;
    for n in notes {
        let title = n.get("title").and_then(|v| v.as_str()).unwrap_or("Imported note");
        let created = n.get("created_at").or_else(|| n.get("createdAt")).and_then(|v| v.as_str()).unwrap_or("1970-01-01");
        let summary = n.get("summary").and_then(|v| v.as_str());
        let id = Uuid::new_v4().to_string();
        let ok = db.conn().execute(
            "INSERT INTO meetings(id,title,started_at,duration_s,summary) VALUES(?1,?2,?3,0,?4)",
            rusqlite::params![id, title, created, summary],
        );
        if ok.is_err() {
            continue;
        }
        if let Some(segs) = n.get("transcript").and_then(|v| v.as_array()) {
            for (i, s) in segs.iter().enumerate() {
                let _ = db.conn().execute(
                    "INSERT INTO segments(id,meeting_id,start_ms,end_ms,speaker,text)
                     VALUES(?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![
                        Uuid::new_v4().to_string(), id,
                        s.get("start").and_then(|v| v.as_i64()).unwrap_or(i as i64 * 1000),
                        s.get("end").and_then(|v| v.as_i64()).unwrap_or((i as i64 + 1) * 1000),
                        s.get("speaker").and_then(|v| v.as_i64()).unwrap_or(0),
                        s.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    ],
                );
            }
        }
        imported += 1;
    }
    Ok(imported)
}

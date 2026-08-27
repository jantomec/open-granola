//! Open Granola — local-first meeting notes.
//!
//! Design law, in order:
//! 1. NO network. There is no http client in this crate. Airlock (see `airlock.rs`)
//!    additionally asks the OS to deny outbound connections at runtime.
//! 2. Raw audio lives in memory only, unless the user explicitly opts in to
//!    encrypted local audio retention.
//! 3. Every model runs on-device: whisper.cpp (transcription), llama.cpp
//!    (enhancement/chat), nomic-embed (semantic search index).
//! 4. The user can delete everything, truly, with one call: `storage::purge_all`.

mod airlock;
pub mod audio;
mod calendar;
mod commands;
pub mod diarize;
mod llm;
mod storage;
mod transcribe;

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::Manager;

/// Shared application state. All heavy resources (model contexts) are loaded
/// lazily and kept behind `parking_lot` mutexes — the lock order is documented
/// at each field to prevent inference-time deadlocks.
pub struct AppState {
    pub data_dir: PathBuf,
    pub db: Mutex<storage::Db>,
    pub session: Mutex<Option<audio::CaptureSession>>,
    pub whisper: Mutex<Option<transcribe::WhisperEngine>>,
    pub llm: Mutex<Option<llm::LocalLlm>>,
    /// Cumulative counter shown in Settings: bytes sent over any socket.
    /// It is hard-wired to zero and exists so the UI can make the claim
    /// "0 bytes sent — ever" honestly.
    pub bytes_sent: u64,
}

pub fn run() {
    env_logger::init();
    airlock::engage(); // before anything else touches the network stack

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?.join("library");
            std::fs::create_dir_all(&data_dir)?;
            let db = storage::Db::open(&data_dir.join("opengranola.db"))?;
            // Shred notes past the retention policy and Recently Deleted
            // items older than 30 days. Failure must never block startup.
            if let Err(e) = db.enforce_retention() {
                log::error!("retention enforcement failed: {e}");
            }
            app.manage(Arc::new(AppState {
                data_dir,
                db: Mutex::new(db),
                session: Mutex::new(None),
                whisper: Mutex::new(None),
                llm: Mutex::new(None),
                bytes_sent: 0,
            }));
            Ok(())
        })
        // macOS convention: the red button closes the window, the app stays
        // in the Dock, and clicking the Dock icon brings the window back.
        // Without this pair of handlers, closing destroyed the main window
        // while the hidden capture window kept the app alive — so a Dock
        // click had nothing left to reopen and did nothing.
        .on_window_event(|window, event| {
            #[cfg(target_os = "macos")]
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    log::info!("main window close → hiding; Dock icon reopens it");
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (window, event);
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_capture,
            commands::stop_capture_and_enhance,
            commands::list_meetings,
            commands::get_meeting,
            commands::list_action_items,
            commands::list_projects,
            commands::create_project,
            commands::rename_project,
            commands::delete_project,
            commands::set_meeting_project,
            commands::rename_meeting,
            commands::delete_meeting,
            commands::restore_meeting,
            commands::delete_meeting_permanently,
            commands::list_deleted_meetings,
            commands::ask_library,
            commands::semantic_search,
            commands::toggle_action_item,
            commands::set_retention_policy,
            commands::purge_everything,
            commands::model_status,
            commands::upcoming_calendar_events,
            commands::get_brief,
            commands::list_commitments,
            commands::mark_commitment,
            commands::run_recipe,
            commands::import_granola_export,
        ])
        .build(tauri::generate_context!())
        .expect("error while running open-granola")
        .run(|app, event| {
            // Dock icon clicked (macOS "reopen"): surface the main window.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                log::info!("reopen event → showing main window");
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

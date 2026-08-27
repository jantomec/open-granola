# Open Granola — agent notes

Tauri 2 app: React/TS frontend (`src/`), Rust core (`src-tauri/src/`). Local-first
meeting notes; the privacy law is **no network code, ever** (see `airlock.rs` and
`.github/workflows/airlock.yml` — CI greps the dep tree and source for network use).

## Verify

- Backend: `cd src-tauri && cargo test --lib` — 6 tests, ~78 s wall (75 s of that is
  the airlock connect-timeout test; the rest run in <1 s).
- Frontend: `npm run build` (tsc + vite) and `npm run lint`.
  Lint has **13 pre-existing errors** (shadcn export patterns, a `Date.now` in render);
  treat "13 problems" as the current baseline — new code must not add to it.
- GitHub Actions has never run on this fork (Actions disabled by default on forks);
  local gates are the only gates.
- Releases: see `docs/RELEASING.md`, including how the signed app was smoke-tested.

## Contracts that are easy to break

- **SQLite foreign keys are OFF.** The schema's `ON DELETE CASCADE` clauses never fire.
  All deletion is explicit: `Db::delete_meeting_rows` (storage.rs) is the single hard
  delete; retention/trash sweeps and the permanent-delete command all go through it.
- **`segments_fts` is an external-content FTS5 table kept in sync by triggers**
  (`segments_fts_ai/ad/au` in the schema). Never INSERT/DELETE on it directly —
  deleting rows the index doesn't hold raises SQLITE_CORRUPT_VTAB and aborts the
  transaction (shipped as the v0.2.1 delete bug; test fixtures that hand-populate the
  index will hide it). `count(*)` on it proxies to the content table — probe the index
  with MATCH queries in tests.
- **Meetings soft-delete** (`deleted_at` column) into Recently Deleted; every
  live-library query must filter `deleted_at IS NULL` (list, search, briefs, action
  items, commitments, project counts). 30-day shred runs in `enforce_retention` on
  startup (lib.rs setup).
- **The App Sandbox entitlement must stay off.** WKWebView's helper processes cannot
  start in a sandboxed app without `network.client`, which Airlock refuses — the
  sandboxed build white-screens with an infinite webview reload loop. Without the
  sandbox, `airlock.rs`'s seatbelt `deny network*` loads (it cannot nest inside the
  App Sandbox). Full story in `src-tauri/entitlements.plist`.
- **Transcript-first persistence:** `stop_capture_and_enhance` writes the raw
  transcript before the LLM runs; enhancement failure must never lose a meeting.
- macOS window close hides (`on_window_event` in lib.rs); `RunEvent::Reopen` re-shows.
  There is a second, always-hidden `capture` window — the app never "runs out of
  windows", so reopen handling is what makes Dock clicks work.

## Known fictions / stubs (inherited, documented in README)

- Windows/Linux loopback in `audio/loopback.rs`: sleep-loop stubs.
- `airlock.rs` Windows WFP function: logs but filters nothing (and references a
  nonexistent `airlock_win.rs`).
- In-app model download buttons: inert. Models are placed manually
  (`~/Library/Application Support/app.opengranola/library/models/`, exact names in
  README).

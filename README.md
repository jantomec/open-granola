<p align="center">
  <img src="docs/assets/logo.svg" width="72" alt="Open Granola logo" />
</p>

<h1 align="center">Open Granola</h1>

<p align="center">
  <strong>Your meetings, remembered. Nothing leaves your machine.</strong>
</p>

<p align="center">
  Free, open-source (Apache-2.0) AI meeting notes for <strong>macOS · Windows · Linux</strong>.<br/>
  Bot-free capture · on-device Whisper · local LLM notes · zero cloud · zero accounts · zero data retention.
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#why-open-granola">Why Open Granola</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="docs/ARCHITECTURE.md">Architecture</a> ·
  <a href="PRIVACY.md">Privacy</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

---

Open Granola is the meeting notepad for people who read the privacy policy first. It captures any call
(Zoom, Meet, Teams, Webex, huddles, in-person) **without a bot joining**, transcribes it with
**whisper.cpp on your own hardware**, and turns it into chapters, decisions and action items with a
**local LLM** (llama.cpp). There is no Open Granola server. There is no account. There is no telemetry.
There isn't even a network stack in the binary — we call that **Airlock**, and you can verify it
yourself in about 40 lines of source.

> *Granola's workflow, everyone's source code. Your meetings stay yours.*

## Status of this fork

**This fork ([jantomec/open-granola](https://github.com/jantomec/open-granola)) is the version that
actually builds and runs.** The repository it was forked from was scaffolded but never compiled —
the Rust backend had 111 compile errors, the icon set and Tauri CLI were missing, and the lockfile
pointed at a dead registry mirror. As of August 2026 this fork:

- **builds and runs on macOS** (Apple Silicon, Metal); CUDA is enabled only for Linux/Windows targets
- **never loses a recording** — the transcript is persisted to SQLite *before* the LLM runs;
  a failed enhancement leaves a raw timestamped note instead of silently discarding the meeting
- **has real speaker diarization** — NeMo TitaNet-small embeddings via sherpa-onnx, clustered
  online, replacing a placeholder heuristic
- **has projects** — create/rename/delete in the sidebar, assign meetings, filter the library
- **shows your real library** — the bundled sample data is used only by the in-browser demo

### Known gaps (inherited, not yet fixed)

- **System-audio loopback is a stub** — only the microphone is captured, so remote participants
  are heard only if they play through your speakers. The per-platform loopback code in
  `src-tauri/src/audio/loopback.rs` is skeleton-only.
- **In-app model download is not implemented** — the Download buttons in Settings are inert.
  Place model files manually in `~/Library/Application Support/app.opengranola/library/models/`:

  | File name (must match exactly) | Model | Source |
  |---|---|---|
  | `whisper-large-v3-turbo.bin` | Whisper Large v3 Turbo (ggml) | [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp) |
  | `qwen3-4b-q4.gguf` | Qwen3-4B Q4_K_M | [Qwen/Qwen3-4B-GGUF](https://huggingface.co/Qwen/Qwen3-4B-GGUF) |
  | `nomic-embed-v1.5.gguf` | Nomic Embed v1.5 | [nomic-ai/nomic-embed-text-v1.5-GGUF](https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF) |
  | `speaker-embed.onnx` | NeMo TitaNet-small | [sherpa-onnx speaker models](https://github.com/k2-fsa/sherpa-onnx/releases/tag/speaker-recongition-models) (`nemo_en_titanet_small.onnx`) |

- The Parakeet and Llama 8B entries in Settings are display-only; the backend loads the
  fixed filenames above.
- **Only the Granola importer works** — the Otter / Fireflies / read.ai import button in
  Settings does nothing yet.
- **No action-item export** — the notes stay in the app; Markdown/Obsidian/clipboard export
  is on the roadmap.

## Why Open Granola

Granola proved that bot-free capture is the right idea. It also uploads your audio, trains on your
data unless you opt out (org-wide opt-out costs $35/user/mo), caps the free tier's history, skips
Linux entirely, and offers no help while the meeting is actually happening. Open Granola keeps the idea and
removes the business model:

| | **Open Granola** | Granola |
|---|---|---|
| Price | **Free, forever** | $0–35/user/mo |
| Audio & AI processing | **100% on-device** | Cloud |
| Trains on your data | **Impossible (no network)** | Opt-out; org-wide = Enterprise |
| Data retention | **None — audio shredded post-transcript** | Server-side, tier-dependent |
| Works offline | **Yes, fully** | No |
| Live assist during the call | **On-device recall, facts, follow-ups** | Yes — in their cloud |
| Mobile capture | Companion via local pairing (roadmap) | iOS + Android apps |
| Speaker diarization | **On-device, free** | Cloud, degrades past 3 people |
| Linux | **First-class** | No |
| License | **Apache-2.0** | Proprietary |

## Features

- 🎙️ **Bot-free capture** — nobody in the call sees anything. Today this means microphone capture; per-platform system-audio loopback (CoreAudio process-tap / WASAPI / PipeWire) is in progress — see [Known gaps](#known-gaps-inherited-not-yet-fixed).
- ⚡ **Streaming on-device transcription** — Whisper Large v3 Turbo, 99 languages, with custom vocabulary for your names, numbers and jargon. (NVIDIA Parakeet: planned.)
- 🧠 **Local AI notes** — an embedded GGUF model (Qwen3-4B default) writes the summary, chapters, decisions and action items with owners and due dates the moment you stop.
- 💡 **Live assist** — during the meeting, a private panel surfaces recall from past notes, relevant facts, and suggested follow-up questions. Only you see it.
- 🔍 **Semantic search + chat** — every meeting is embedded into a local sqlite-vec index. Ask “what did Vesper say about compliance?” and get answers with timestamps.
- 🗞️ **Pre-meeting Briefs** — before each call, Open Granola writes a private brief: what happened last time with these people, which commitments are riding on this meeting, and three things worth raising. All from local RAG.
- 🤝 **Commitment ledger** — every promise anyone makes (“I'll have it by Friday”) is extracted, tracked across meetings, and resurfaced when due. Nobody else builds this at any price.
- 📖 **Recipes** — shareable Markdown prompt packs (objection miner, board-update extractor…) that run on your local model. Publish them with a PR.
- 📥 **Importer** — migrate from a Granola JSON export in one click. (Otter, Fireflies and read.ai importers: planned.)
- 📅 **Calendar-aware** — reads your local calendar (EventKit / ICS / CalDAV cache) to auto-title notes and prompt capture. Google and Outlook treated equally — no account needed.
- 🗂️ **Templates** — product sync, 1:1, sales discovery, interview, standup, board update — or your own Markdown.
- 🔐 **Airlock** — one build flag removes the network stack; the macOS sandbox additionally denies outbound sockets below the process. See [`src-tauri/src/airlock.rs`](src-tauri/src/airlock.rs).
- 🗑️ **Real deletion** — audio lives only in RAM and dies at transcription; retention auto-purge *shreds* notes, transcripts and embeddings (with `VACUUM`, so it's physical); one-click purge zero-fills the database file before unlinking it.

## Install

There are no binary releases yet — build from source (below). Packaged builds (`.dmg`, `.msi`,
AppImage/deb/Flatpak) will appear in
[**Releases**](https://github.com/jantomec/open-granola/releases) once the first release is cut.

Open Granola never opens a socket, and in-app model download is not implemented yet — so fetch the
model files yourself (~3 GB) and place them in the library folder. The exact file names and sources
are in the table under [Known gaps](#known-gaps-inherited-not-yet-fixed).

### Build from source

```bash
git clone https://github.com/jantomec/open-granola.git && cd open-granola
npm install
npm run tauri dev        # dev build
npm run tauri build      # release bundles in src-tauri/target/release/bundle
```

Prereqs: Rust ≥ 1.77, Node ≥ 20, and the [Tauri platform deps](https://v2.tauri.app/start/prerequisites/).
On Linux: `libpipewire-0.3-dev`, `libwebkit2gtk-4.1-dev`, `libasound2-dev`.

## How it works

```
mic ──┐                                  ┌─► streaming transcript (UI)
      ├─► mix → 16 kHz mono ring buffer ─┤
system┘   (RAM only — never on disk)     │   whisper.cpp windows (2 s / 500 ms stride)
                                         └─► TitaNet speaker embeddings → online clustering → labels
stop ─► transcript ─► llama.cpp ─► structured notes (summary · chapters · decisions · actions)
     ─► nomic-embed ─► sqlite-vec index ─► semantic search + chat + live-assist recall
audio ─► shredded (default) or encrypted-at-rest (opt-in)
```

Full details in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## The privacy model (the short version)

1. **No network code exists in the app.** CI rejects any PR that adds an HTTP/WebSocket dependency.
2. **The OS enforces it too.** The macOS build ships without the `network.client` entitlement and
   loads a seatbelt profile denying outbound sockets at launch.
3. **Audio is deleted when transcription finishes.** It only ever lives in RAM. (Opt-in encrypted
   local retention is planned, not yet built.)
4. **One-click purge** zero-fills the database file before unlinking it.
5. **Apache-2.0** — audit every line, or pay someone to. ([PRIVACY.md](PRIVACY.md))

## Roadmap

- [x] Mic capture + streaming Whisper + on-device diarization
- [x] Local LLM enhancement, chat, semantic search
- [x] Live assist (recall, facts, follow-ups)
- [x] Pre-meeting Briefs + cross-meeting commitment ledger
- [x] Recipes + Granola JSON importer
- [ ] System-audio loopback (macOS process-tap first, then WASAPI / PipeWire)
- [ ] In-app model download
- [ ] Otter / Fireflies / read.ai importers
- [ ] Action-item export (Markdown, Obsidian vault, clipboard)
- [ ] Opt-in encrypted audio retention + playback to verify lines
- [ ] Push-to-talk dictation in any app
- [ ] Local speaker identification ("that was Priya", trained on-device)
- [ ] SIEM-friendly signed audit export
- [ ] iOS/Android companion via local Wi-Fi pairing (still no cloud)

## Contributing

We'd love your help — see [CONTRIBUTING.md](CONTRIBUTING.md). Good first issues are labeled, and the
rule is simple: **no PR may add a network dependency, an account system, or telemetry.** Everything
else is negotiable.

## License

[Apache-2.0](LICENSE) © Open Granola contributors. Use it, fork it, ship it in your company, sell support
for it — just keep the license notice.

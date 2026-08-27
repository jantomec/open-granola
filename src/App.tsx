import { useCallback, useEffect, useState } from "react";
import { CaptureBar } from "./components/CaptureBar";
import { CommandPalette } from "./components/CommandPalette";
import { HomeView } from "./components/HomeView";
import { NoteView } from "./components/NoteView";
import { SettingsView } from "./components/SettingsView";
import { Sidebar } from "./components/Sidebar";
import { ActionItemsView } from "./components/ActionItemsView";
import { CommitmentsView } from "./components/CommitmentsView";
import { TemplatesView } from "./components/TemplatesView";
import { MEETINGS, PEOPLE } from "./lib/data";
import { getBackend } from "./lib/backend";
import { useLiveSession, type LiveLine } from "./hooks/useLiveSession";
import { useTauriSession } from "./hooks/useTauriSession";
import type { Brief, Meeting, Project, View } from "./lib/types";

const backend = getBackend();

export default function App() {
  const [dark, setDark] = useState(false);
  const [view, setView] = useState<View>({ kind: "home" });
  const [paletteOpen, setPaletteOpen] = useState(false);
  // The bundled sample library exists for the browser demo only; the desktop
  // app starts empty and shows exactly what the local database holds.
  const [meetings, setMeetings] = useState<Meeting[]>(backend.mode === "tauri" ? [] : MEETINGS);
  const [brief, setBrief] = useState<Brief | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  const reloadProjects = useCallback(() => {
    backend.listProjects().then(setProjects).catch(() => {});
  }, []);

  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
  }, [dark]);

  // Load the real library when running inside Tauri; demo mode keeps the
  // bundled sample library that powers the browser preview.
  useEffect(() => {
    if (backend.mode === "tauri") {
      backend.listMeetings().then(setMeetings).catch(() => {});
      reloadProjects();
    }
    backend.getBrief().then(setBrief).catch(() => {});
  }, [reloadProjects]);

  const createProject = useCallback(
    (name: string) => backend.createProject(name).then(reloadProjects),
    [reloadProjects],
  );
  const renameProject = useCallback(
    (id: string, name: string) => backend.renameProject(id, name).then(reloadProjects),
    [reloadProjects],
  );
  const deleteProject = useCallback(
    (id: string) =>
      backend.deleteProject(id).then(() => {
        setActiveProject((p) => (p === id ? null : p));
        setMeetings((prev) => prev.map((m) => (m.projectId === id ? { ...m, projectId: undefined } : m)));
        reloadProjects();
      }),
    [reloadProjects],
  );
  const assignProject = useCallback(
    (meetingId: string, projectId: string | null) =>
      backend.setMeetingProject(meetingId, projectId).then(() => {
        setMeetings((prev) =>
          prev.map((m) => (m.id === meetingId ? { ...m, projectId: projectId ?? undefined } : m)),
        );
        reloadProjects();
      }),
    [reloadProjects],
  );
  const deleteMeeting = useCallback(
    (id: string) =>
      backend
        .deleteMeeting(id)
        .then(() => {
          setMeetings((prev) => prev.filter((m) => m.id !== id));
          setView((v) => (v.kind === "meeting" && v.id === id ? { kind: "home" } : v));
          reloadProjects();
        })
        .catch((e) => console.error("delete failed:", e)),
    [reloadProjects],
  );
  const renameMeeting = useCallback(
    (id: string, title: string) =>
      backend
        .renameMeeting(id, title)
        .then(() => {
          setMeetings((prev) => prev.map((m) => (m.id === id ? { ...m, title } : m)));
        })
        .catch((e) => console.error("rename failed:", e)),
    [],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Demo finish: build the meeting client-side from the simulated lines.
  const handleDemoFinish = useCallback((lines: LiveLine[]) => {
    if (lines.length === 0) return;
    const speakers = [...new Set(lines.map((l) => l.speaker))];
    const meeting: Meeting = {
      id: `m-live-${Date.now()}`,
      title: "Aurora checkpoint — onboarding funnel",
      date: new Date().toISOString(),
      durationMin: Math.max(1, Math.round(lines.join(" ").split(" ").length / 150)),
      participants: [
        PEOPLE.you,
        ...speakers.filter((s) => s !== "You").map((s) => Object.values(PEOPLE).find((p) => p.name === s) ?? PEOPLE.devon),
      ],
      tags: ["aurora", "live"],
      template: "Product sync",
      summary:
        "The team diagnosed a 61% drop-off at the workspace-invite step of onboarding. Priya's cohort data suggests deferring invites until day two lifts activation by roughly eight points. The reordered flow ships flag-gated Friday as a 50/50 split.",
      chapters: [
        { title: "Where users bail", timestamp: "00:20", body: "Step three — the workspace invite screen — loses 61% of new users, 3.4× the funnel median." },
        { title: "The fix", timestamp: "01:40", body: "Defer invites until after the first project exists. Beta data projects an ~8-point activation lift." },
        { title: "Rollout", timestamp: "03:10", body: "Flag-gated 50/50 split shipping Friday. Priya monitors the activation dashboard." },
      ],
      decisions: [
        "Defer team invites to day two of onboarding",
        "Ship reordered flow flag-gated Friday as a 50/50 experiment",
      ],
      transcript: lines.map((l, i) => ({
        id: l.id,
        speakerId: l.speaker === "You" ? "you" : (Object.entries(PEOPLE).find(([, p]) => p.name === l.speaker)?.[0] ?? "devon"),
        start: i * 22,
        text: l.text,
      })),
    };
    setMeetings((prev) => [meeting, ...prev]);
    setView({ kind: "meeting", id: meeting.id });
  }, []);

  // Tauri finish: hand the transcript to the Rust core, which enhances and
  // persists it, then reload the library and open the new note.
  const handleTauriFinish = useCallback(async (lines: LiveLine[]) => {
    if (lines.length === 0) return;
    const segments = lines.map((l, i) => ({
      start_ms: i * 22000,
      end_ms: (i + 1) * 22000,
      speaker: l.speaker === "You" ? 0 : parseInt(l.speaker.replace("Speaker ", "") || "1", 10),
      text: l.text,
      final_: true,
    }));
    try {
      const id = await backend.stopCapture(segments, "Product sync");
      const fresh = await backend.listMeetings();
      setMeetings(fresh);
      setView({ kind: "meeting", id });
    } catch (e) {
      console.error("save failed:", e);
      setSaveError(String(e));
    }
  }, []);

  const demo = useLiveSession(handleDemoFinish);
  const tauri = useTauriSession(handleTauriFinish);
  const live = backend.mode === "demo" ? demo : tauri;

  const openMeeting = (id: string) => setView({ kind: "meeting", id });
  const activeMeeting = view.kind === "meeting" ? meetings.find((m) => m.id === view.id) : undefined;
  const visibleMeetings = activeProject
    ? meetings.filter((m) => m.projectId === activeProject)
    : meetings;

  return (
    <div className="relative flex h-screen overflow-hidden bg-background text-foreground">
      <Sidebar
        meetings={visibleMeetings}
        projects={projects}
        activeProject={activeProject}
        onSelectProject={setActiveProject}
        onCreateProject={createProject}
        onRenameProject={renameProject}
        onDeleteProject={deleteProject}
        onDeleteMeeting={deleteMeeting}
        onMoveMeeting={assignProject}
        view={view}
        onNavigate={setView}
        onOpenSearch={() => setPaletteOpen(true)}
        onRecord={live.start}
        recording={live.active}
        onStop={live.stop}
        dark={dark}
        onToggleDark={() => setDark(!dark)}
      />

      <main className="flex min-w-0 flex-1 flex-col">
        {view.kind === "home" && (
          <HomeView
            meetings={visibleMeetings}
            brief={brief}
            onOpenMeeting={openMeeting}
            onRecord={live.start}
            onAsk={() => openMeeting(meetings[0]?.id ?? "")}
          />
        )}
        {view.kind === "meeting" && activeMeeting && (
          <NoteView
            meeting={activeMeeting}
            projects={projects}
            onSetProject={(pid) => assignProject(activeMeeting.id, pid)}
            onRename={(title) => renameMeeting(activeMeeting.id, title)}
            onToggleAction={(id, done) => backend.toggleAction(id, done)}
            askFn={backend.mode === "tauri" ? (q) => backend.ask(q, activeMeeting.id) : undefined}
          />
        )}
        {view.kind === "actions" && (
          <ActionItemsView
            onOpenMeeting={openMeeting}
            onToggle={backend.mode === "tauri" ? (id, done) => backend.toggleAction(id, done) : undefined}
          />
        )}
        {view.kind === "commitments" && <CommitmentsView onOpenMeeting={openMeeting} />}
        {view.kind === "templates" && <TemplatesView />}
        {view.kind === "settings" && <SettingsView />}
      </main>

      {live.active && (
        <CaptureBar elapsed={live.elapsed} lines={live.lines} suggestions={live.suggestions} onStop={live.stop} />
      )}

      {saveError && (
        <div className="fixed bottom-4 right-4 z-50 max-w-sm rounded-2xl border border-destructive/40 bg-card p-4 shadow-lg">
          <div className="text-[13px] font-semibold text-destructive">Saving the meeting failed</div>
          <p className="mt-1 break-words text-[12.5px] text-muted-foreground">{saveError}</p>
          <button
            onClick={() => setSaveError(null)}
            className="mt-2 text-[12px] font-semibold underline"
          >
            Dismiss
          </button>
        </div>
      )}

      {paletteOpen && (
        <CommandPalette
          meetings={meetings}
          onClose={() => setPaletteOpen(false)}
          onOpenMeeting={openMeeting}
          searchFn={backend.mode === "tauri" ? (q) => backend.search(q) : undefined}
          onOpenActions={() => {
            setPaletteOpen(false);
            setView({ kind: "actions" });
          }}
        />
      )}
    </div>
  );
}

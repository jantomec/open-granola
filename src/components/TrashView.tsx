import { ArchiveRestore, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { getBackend } from "../lib/backend";
import type { DeletedMeeting } from "../lib/types";

interface Props {
  /** Called after a restore so the app can refresh the live library. */
  onRestored: () => void;
  onError: (message: string) => void;
}

const KEEP_DAYS = 30;

function daysLeft(deletedAt: string) {
  const gone = new Date(deletedAt).getTime() + KEEP_DAYS * 86400000;
  return Math.max(0, Math.ceil((gone - Date.now()) / 86400000));
}

export function TrashView({ onRestored, onError }: Props) {
  const backend = getBackend();
  const [items, setItems] = useState<DeletedMeeting[]>([]);
  const [loaded, setLoaded] = useState(false);

  const reload = useCallback(() => {
    backend
      .listDeletedMeetings()
      .then((rows) => {
        setItems(rows);
        setLoaded(true);
      })
      .catch((e) => onError(`Loading Recently Deleted failed: ${e}`));
  }, [backend, onError]);

  useEffect(reload, [reload]);

  const restore = (id: string) =>
    backend
      .restoreMeeting(id)
      .then(() => {
        reload();
        onRestored();
      })
      .catch((e) => onError(`Restoring the meeting failed: ${e}`));

  const deleteForever = (id: string) =>
    backend
      .deleteMeetingPermanently(id)
      .then(reload)
      .catch((e) => onError(`Deleting the meeting failed: ${e}`));

  return (
    <div className="scrollbar-thin flex-1 overflow-y-auto">
      <div className="mx-auto max-w-2xl px-8 py-10">
        <h1 className="font-display text-[26px] leading-tight">Recently Deleted</h1>
        <p className="mt-1 text-[12.5px] text-muted-foreground">
          Deleted meetings are kept for {KEEP_DAYS} days, then shredded — transcript, embeddings and
          all. Deleting here is immediate and permanent.
        </p>

        {loaded && items.length === 0 && (
          <div className="mt-10 flex flex-col items-center gap-2 text-muted-foreground">
            <Trash2 size={22} />
            <p className="text-[13px]">Nothing here — deleted meetings will appear in this folder.</p>
          </div>
        )}

        <div className="mt-6 space-y-2">
          {items.map((m) => (
            <div
              key={m.id}
              className="flex items-center gap-3 rounded-xl border border-border bg-card px-4 py-3"
            >
              <div className="min-w-0 flex-1">
                <div className="truncate text-[14px] font-medium">{m.title}</div>
                <div className="mt-0.5 text-[11.5px] text-muted-foreground">
                  {new Date(m.date).toLocaleDateString()} · {m.durationMin} min · gone in{" "}
                  {daysLeft(m.deletedAt)} {daysLeft(m.deletedAt) === 1 ? "day" : "days"}
                </div>
              </div>
              <button
                onClick={() => restore(m.id)}
                className="flex items-center gap-1.5 rounded-lg border border-border px-2.5 py-1.5 text-[12px] font-semibold transition-colors hover:bg-secondary"
                title="Restore to the library"
              >
                <ArchiveRestore size={13} /> Restore
              </button>
              <button
                onClick={() => deleteForever(m.id)}
                className="flex items-center gap-1.5 rounded-lg border border-destructive/40 px-2.5 py-1.5 text-[12px] font-semibold text-destructive transition-colors hover:bg-destructive hover:text-destructive-foreground"
                title="Delete permanently, right now"
              >
                <Trash2 size={13} /> Delete
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

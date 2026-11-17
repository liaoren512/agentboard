import type { SessionSnapshot } from "./types";

export interface Groups {
  attention: SessionSnapshot[];
  running: SessionSnapshot[];
  done: { today: SessionSnapshot[]; yesterday: SessionSnapshot[]; earlier: SessionSnapshot[] };
}

const isDone = (s: SessionSnapshot) => s.status === "done";
const isAttention = (s: SessionSnapshot) => s.status.startsWith("waiting") || s.status === "failed";

export function groupSessions(
  list: SessionSnapshot[],
  now: number,
  filter?: { agent?: string },
): Groups {
  const f = filter?.agent ? list.filter((s) => s.agent === filter.agent) : list;
  const dayStart = (t: number) => { const d = new Date(t); d.setHours(0, 0, 0, 0); return d.getTime(); };
  const today = dayStart(now), yest = dayStart(now) - 86400_000;
  const g: Groups = { attention: [], running: [], done: { today: [], yesterday: [], earlier: [] } };
  for (const s of f) {
    if (isAttention(s)) g.attention.push(s);
    else if (!isDone(s)) g.running.push(s);
    else {
      const t = s.lastActivity;
      if (t >= today) g.done.today.push(s);
      else if (t >= yest) g.done.yesterday.push(s);
      else g.done.earlier.push(s);
    }
  }
  return g;
}

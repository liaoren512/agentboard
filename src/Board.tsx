import { useMemo, useState } from "react";
import { groupSessions } from "./board-model";
import type { SessionSnapshot } from "./types";
import SessionRow from "./SessionRow";

const AGENT_FILTERS = [
  { key: "", label: "全部" },
  { key: "claude_code", label: "Claude Code" },
  { key: "zcode", label: "ZCode" },
] as const;

export default function Board({ sessions }: { sessions: SessionSnapshot[] }) {
  const [agent, setAgent] = useState<string>("");
  const groups = useMemo(
    () => groupSessions(sessions, Date.now(), agent ? { agent } : undefined),
    [sessions, agent],
  );

  const doneGroups = [
    { label: "今天", list: groups.done.today },
    { label: "昨天", list: groups.done.yesterday },
    { label: "更早", list: groups.done.earlier },
  ];
  const empty =
    groups.attention.length + groups.running.length +
    groups.done.today.length + groups.done.yesterday.length + groups.done.earlier.length === 0;

  return (
    <div className="board">
      <div className="filter">
        {AGENT_FILTERS.map((f) => (
          <button
            key={f.key}
            className={`chip ${agent === f.key ? "chip-active" : ""}`}
            onClick={() => setAgent(f.key)}
          >
            {f.label}
          </button>
        ))}
      </div>

      {empty && <div className="empty">暂无会话</div>}

      {groups.attention.length > 0 && (
        <section>
          <h2>需处理</h2>
          {groups.attention.map((s) => <SessionRow key={s.sessionId} session={s} />)}
        </section>
      )}

      {groups.running.length > 0 && (
        <section>
          <h2>运行中</h2>
          {groups.running.map((s) => <SessionRow key={s.sessionId} session={s} />)}
        </section>
      )}

      {doneGroups.map(
        (g) =>
          g.list.length > 0 && (
            <section key={g.label}>
              <h2>完成 · {g.label}</h2>
              {g.list.map((s) => <SessionRow key={s.sessionId} session={s} />)}
            </section>
          ),
      )}
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SessionSnapshot } from "./types";

const STATUS_LABEL: Record<SessionSnapshot["status"], string> = {
  running: "运行中",
  waiting_permission: "等待权限",
  waiting_question: "等待提问",
  waiting_idle: "空闲",
  waiting_other: "等待",
  failed: "失败",
  done: "完成",
};

const AGENT_LABEL: Record<SessionSnapshot["agent"], string> = {
  claude_code: "Claude",
  zcode: "ZCode",
};

function statusClass(status: SessionSnapshot["status"]): string {
  return status.startsWith("waiting") ? "waiting" : status;
}

function formatRelative(ts: number): string {
  const diff = Date.now() - ts;
  if (diff < 60_000) return "刚刚";
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} 小时前`;
  const d = new Date(ts);
  return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()}`;
}

export default function SessionRow({ session }: { session: SessionSnapshot }) {
  const [note, setNote] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const jump = () => {
    invoke<string>("jump", { sessionId: session.sessionId })
      .then((n) => {
        setNote(n);
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setNote(null), 2500);
      })
      .catch((e) => console.warn("jump failed:", e));
  };
  return (
    <>
      <div className="row" onClick={jump}>
        <span className={`agent-badge agent-${session.agent}`}>{AGENT_LABEL[session.agent]}</span>
        <span className="title" title={session.title}>{session.title}</span>
        <span className="meta">
          {session.repo}
          {session.branch ? ` · ${session.branch}` : ""}
        </span>
        <span className="time">{formatRelative(session.lastActivity)}</span>
        <span className={`status-badge status-${statusClass(session.status)}`}>
          {STATUS_LABEL[session.status]}
        </span>
        {session.status.startsWith("waiting") && session.waitingReason && (
          <span className="waiting-reason" title={session.waitingReason}>
            {session.waitingReason}
          </span>
        )}
        {session.status === "done" && session.doneInferred && (
          <span className="inferred-badge" title="由静默推断完成">推断</span>
        )}
        {session.status === "done" && session.replyHead && (
          <span className="reply-head" title={session.replyHead}>
            {session.replyHead}
          </span>
        )}
      </div>
      {note && <div className="jump-note">{note}</div>}
    </>
  );
}

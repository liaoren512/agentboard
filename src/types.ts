// 与 Rust camelCase 序列化严格一致
export type AgentKind = "claude_code" | "zcode";
export type WaitingKind = "permission" | "question" | "idle" | "other";
export type SessionStatus = "running" | `waiting_${WaitingKind}` | "failed" | "done";
export interface SessionSnapshot {
  sessionId: string; agent: AgentKind; title: string; cwd: string; repo: string;
  branch: string | null; status: SessionStatus; doneInferred: boolean;
  waitingReason: string | null; replyHead: string | null;
  lastActivity: number; timeCreated: number;
}

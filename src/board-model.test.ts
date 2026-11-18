import { describe, it, expect } from "vitest";
import { groupSessions } from "./board-model";
import type { SessionSnapshot } from "./types";

const mk = (over: Partial<SessionSnapshot>): SessionSnapshot => ({
  sessionId: "s", agent: "claude_code", title: "t", cwd: "C:\\r", repo: "r",
  branch: null, status: "running", doneInferred: false,
  waitingReason: null, replyHead: null,
  lastActivity: 0, timeCreated: 0, ...over,
});

describe("groupSessions", () => {
  it("waiting/failed 置顶，其后 running，done 分组", () => {
    const now = new Date(2026, 10, 17, 12, 0).getTime(); // 本地时区
    const list = [
      mk({ sessionId: "d1", status: "done", lastActivity: now - 86400_000 * 3 }),
      mk({ sessionId: "r1", status: "running" }),
      mk({ sessionId: "w1", status: "waiting_permission" }),
      mk({ sessionId: "f1", status: "failed" }),
      mk({ sessionId: "d2", status: "done", lastActivity: now - 3600_000 }),
      mk({ sessionId: "d3", status: "done", lastActivity: now - 86400_000 - 3600_000 }),
    ];
    const g = groupSessions(list, now);
    expect(g.attention.map((s) => s.sessionId)).toEqual(["w1", "f1"]);
    expect(g.running.map((s) => s.sessionId)).toEqual(["r1"]);
    expect(g.done.today.map((s) => s.sessionId)).toEqual(["d2"]);
    expect(g.done.yesterday.map((s) => s.sessionId)).toEqual(["d3"]);
    expect(g.done.earlier.map((s) => s.sessionId)).toEqual(["d1"]);
  });

  it("agent 过滤", () => {
    const list = [mk({ sessionId: "a", agent: "claude_code" }), mk({ sessionId: "b", agent: "zcode" })];
    expect(groupSessions(list, 0, { agent: "zcode" }).running.map((s) => s.sessionId)).toEqual(["b"]);
  });
});

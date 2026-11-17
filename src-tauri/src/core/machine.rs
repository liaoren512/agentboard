use crate::core::types::*;
use std::collections::HashMap;

pub struct Update {
    pub snapshot: SessionSnapshot,
    pub should_notify: bool,
}

struct Inner {
    snap: SessionSnapshot,
    /// 本轮已通知过的终态（Waiting/Failed/Done）。回到 Running 时清空 → 重新武装。
    notified: Option<SessionStatus>,
}

pub struct Machine {
    sessions: HashMap<String, Inner>,
    done_after_ms: u64,
}

impl Machine {
    pub fn new(done_after_ms: u64) -> Self {
        Self {
            sessions: HashMap::new(),
            done_after_ms,
        }
    }

    /// 每次注入 Tick 前由 lib.rs 按 config 覆盖（设置页改动实时生效）
    pub fn set_done_after_ms(&mut self, ms: u64) {
        self.done_after_ms = ms;
    }

    pub fn process(&mut self, event: &UnifiedEvent) -> Vec<Update> {
        let mut out = Vec::new();
        match event {
            UnifiedEvent::Tick { now } => {
                let done_after = self.done_after_ms;
                for inner in self.sessions.values_mut() {
                    if inner.snap.status == SessionStatus::Running
                        && now.saturating_sub(inner.snap.last_activity) >= done_after
                    {
                        inner.snap.status = SessionStatus::Done;
                        inner.snap.done_inferred = true;
                        let notify = inner.notified != Some(SessionStatus::Done);
                        inner.notified = Some(SessionStatus::Done);
                        out.push(Update {
                            snapshot: inner.snap.clone(),
                            should_notify: notify,
                        });
                    }
                }
            }
            UnifiedEvent::Discovered {
                agent,
                session_id,
                cwd,
                title,
                ts,
            } => {
                // Discovered 一律先置 Running（last_activity=事件 ts）。两种走向，如实描述：
                // - 首扫发现的历史会话（ts=真实历史时间）：首个 Tick 即静默推断 Done
                //   （done_inferred，通知被 lib.rs 的 APP_START 闸拦下）；turn 结束后的
                //   实时 Waiting（ts=now）恢复等待态并通知。
                // - 持续实时活动的会话：Activity ts=now 把 last_activity 顶到 now，
                //   阈值内 Tick 不误推断（链路见 collectors/zcode_db.rs）。
                let entry = self
                    .sessions
                    .entry(session_id.clone())
                    .or_insert_with(|| Inner {
                        snap: SessionSnapshot {
                            session_id: session_id.clone(),
                            agent: *agent,
                            title: title.clone().unwrap_or_else(|| "(未命名)".into()),
                            cwd: cwd.clone(),
                            repo: repo_name(cwd),
                            branch: branch_from_git(cwd),
                            status: SessionStatus::Running,
                            done_inferred: false,
                            waiting_reason: None,
                            reply_head: None,
                            last_activity: *ts,
                            time_created: *ts,
                        },
                        notified: None,
                    });
                if let Some(t) = title {
                    entry.snap.title = t.clone();
                }
                entry.snap.status = SessionStatus::Running;
                entry.snap.done_inferred = false;
                entry.notified = None;
                // 新一轮开始：等待原因与上一轮完成摘要一并清掉
                entry.snap.waiting_reason = None;
                entry.snap.reply_head = None;
                entry.snap.last_activity = entry.snap.last_activity.max(*ts);
                out.push(Update {
                    snapshot: entry.snap.clone(),
                    should_notify: false,
                });
            }
            UnifiedEvent::Activity {
                agent,
                session_id,
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    e.snap.status = SessionStatus::Running;
                    e.snap.done_inferred = false;
                    e.notified = None; // 重新武装
                    e.snap.waiting_reason = None; // 活动即不再等待
                    e.snap.reply_head = None;
                    e.snap.last_activity = e.snap.last_activity.max(*ts);
                    out.push(Update {
                        snapshot: e.snap.clone(),
                        should_notify: false,
                    });
                }
            }
            UnifiedEvent::Waiting {
                agent,
                session_id,
                kind,
                reason,
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    let new_status = SessionStatus::Waiting(*kind);
                    if e.snap.status != new_status {
                        e.snap.status = new_status.clone();
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                    }
                    e.snap.waiting_reason = reason.clone(); // "在等什么"
                    e.snap.reply_head = None; // 上一轮摘要随新一轮作废
                    let notify = e.notified.as_ref() != Some(&new_status);
                    if notify {
                        e.notified = Some(new_status.clone());
                    }
                    out.push(Update {
                        snapshot: e.snap.clone(),
                        should_notify: notify,
                    });
                }
            }
            UnifiedEvent::TurnFailed {
                agent,
                session_id,
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    if e.snap.status != SessionStatus::Failed {
                        e.snap.status = SessionStatus::Failed;
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                        let notify = e.notified != Some(SessionStatus::Failed);
                        if notify {
                            e.notified = Some(SessionStatus::Failed);
                        }
                        out.push(Update {
                            snapshot: e.snap.clone(),
                            should_notify: notify,
                        });
                    }
                }
            }
            UnifiedEvent::TurnFinished {
                agent,
                session_id,
                reply_head,
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    e.snap.status = SessionStatus::Done;
                    e.snap.done_inferred = false;
                    e.snap.last_activity = e.snap.last_activity.max(*ts);
                    e.snap.reply_head = reply_head.clone(); // 真实完成摘要（Done 态展示）
                    e.snap.waiting_reason = None; // 完成即不再等待
                    let notify = e.notified != Some(SessionStatus::Done);
                    if notify {
                        e.notified = Some(SessionStatus::Done);
                    }
                    out.push(Update {
                        snapshot: e.snap.clone(),
                        should_notify: notify,
                    });
                }
            }
            UnifiedEvent::Closed {
                agent,
                session_id,
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    if e.snap.status != SessionStatus::Done {
                        e.snap.status = SessionStatus::Done;
                        e.snap.done_inferred = false;
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                        let notify = e.notified != Some(SessionStatus::Done);
                        if notify {
                            e.notified = Some(SessionStatus::Done);
                        }
                        out.push(Update {
                            snapshot: e.snap.clone(),
                            should_notify: notify,
                        });
                    }
                }
            }
        }
        out
    }

    fn get_mut(&mut self, agent: &AgentKind, session_id: &str) -> Option<&mut Inner> {
        let e = self.sessions.get_mut(session_id)?;
        if e.snap.agent != *agent {
            return None;
        }
        Some(e)
    }

    pub fn snapshots(&self) -> Vec<SessionSnapshot> {
        let mut v: Vec<SessionSnapshot> = self.sessions.values().map(|i| i.snap.clone()).collect();
        // 排序由前端做（board-model），这里按最近活动倒序保证列表稳定
        v.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: u64 = 1_000_000_000_000;
    const DONE_AFTER: u64 = 15 * 60 * 1000;

    fn disc(agent: AgentKind, id: &str, ts: u64) -> UnifiedEvent {
        UnifiedEvent::Discovered {
            agent,
            session_id: id.into(),
            cwd: r"C:\repo\x".into(),
            title: Some("t".into()),
            ts,
        }
    }

    #[test]
    fn discovered_then_activity_runs_then_tick_completes_inferred() {
        let mut m = Machine::new(DONE_AFTER);
        let ups = m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
        assert!(!ups[0].should_notify); // 运行中不通知

        let ups = m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            ts: TS + 1000,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);

        // 停滞超过阈值 → 推断完成 + 通知一次
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + 1000 + DONE_AFTER,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(ups[0].snapshot.done_inferred);
        assert!(ups[0].should_notify);

        // 推断完成后的 Tick 不再重复通知
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + 1000 + 2 * DONE_AFTER,
        });
        assert!(ups.is_empty());
    }

    #[test]
    fn waiting_notifies_once_then_activity_rearms() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        let ups = m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Permission,
            reason: None,
            ts: TS,
        });
        assert_eq!(
            ups[0].snapshot.status,
            SessionStatus::Waiting(WaitingKind::Permission)
        );
        assert!(ups[0].should_notify);

        let ups = m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Permission,
            reason: None,
            ts: TS + 1,
        });
        assert!(!ups[0].should_notify, "同一等待期内不重复通知");

        // 活动回到运行中，再等待 → 重新通知
        m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            ts: TS + 2,
        });
        let ups = m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Permission,
            reason: None,
            ts: TS + 3,
        });
        assert!(ups[0].should_notify);
    }

    #[test]
    fn failed_then_activity_clears() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::TurnFailed {
            agent: AgentKind::ZCode,
            session_id: "z1".into(),
            ts: TS,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Failed);
        assert!(ups[0].should_notify);
        let ups = m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ZCode,
            session_id: "z1".into(),
            ts: TS + 1,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
    }

    #[test]
    fn explicit_finish_is_not_inferred() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::TurnFinished {
            agent: AgentKind::ZCode,
            session_id: "z1".into(),
            reply_head: None,
            ts: TS,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(!ups[0].snapshot.done_inferred);
        assert!(ups[0].should_notify);
    }

    #[test]
    fn closed_marks_done_and_tick_drops_nothing() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS));
        let ups = m.process(&UnifiedEvent::Closed {
            agent: AgentKind::ZCode,
            session_id: "z1".into(),
            ts: TS,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
    }

    #[test]
    fn waiting_does_not_infer_done() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Idle,
            reason: None,
            ts: TS,
        });
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + 2 * DONE_AFTER,
        });
        assert!(ups.is_empty(), "等待中的会话不做完成推断");
    }

    /// 回归（验收缺陷：活会话误判 Done）：首扫 Discovered 带历史 ts → Running，
    /// 随后活会话的实时 Activity（ts=now）把 last_activity 顶起 → 阈值内的 Tick
    /// 绝不推断 Done（否则正在跑的会话会被标成"完成"）。
    #[test]
    fn historical_discover_then_fresh_activity_stays_running() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ZCode, "z1", TS - 3_600_000)); // 首扫：历史时间
        let ups = m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ZCode,
            session_id: "z1".into(),
            ts: TS, // 实时活动顶起 last_activity
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + DONE_AFTER - 1,
        });
        assert!(
            ups.iter().all(|u| u.snapshot.status != SessionStatus::Done),
            "实时活动后阈值内不误推断 Done"
        );
    }

    /// 回归（v0.1.1 接线）：set_done_after_ms 注入后，Tick 推断立即用新阈值
    #[test]
    fn set_done_after_ms_takes_effect_on_tick() {
        let mut m = Machine::new(60_000);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        // 阈值缩到 1s：TS+1000 处即推断 Done（旧阈值 60s 内不会推断）
        m.set_done_after_ms(1_000);
        let ups = m.process(&UnifiedEvent::Tick { now: TS + 1_000 });
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
    }

    // v0.1.3：Waiting 存 waiting_reason / TurnFinished 置 reply_head / Activity 与 Discovered 双清
    #[test]
    fn waiting_stores_reason_and_activity_clears_both() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        let ups = m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Permission,
            reason: Some("git push origin main".into()),
            ts: TS,
        });
        assert_eq!(
            ups[0].snapshot.waiting_reason.as_deref(),
            Some("git push origin main")
        );
        assert_eq!(ups[0].snapshot.reply_head, None);

        // Activity 双清
        let ups = m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            ts: TS + 1,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Running);
        assert_eq!(ups[0].snapshot.waiting_reason, None);
        assert_eq!(ups[0].snapshot.reply_head, None);
    }

    #[test]
    fn turn_finished_stores_reply_head_and_clears_reason() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        m.process(&UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            kind: WaitingKind::Permission,
            reason: Some("waiting for x".into()),
            ts: TS,
        });
        let ups = m.process(&UnifiedEvent::TurnFinished {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            reply_head: Some("已完成构建并推送".into()),
            ts: TS + 1,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(!ups[0].snapshot.done_inferred);
        assert_eq!(
            ups[0].snapshot.reply_head.as_deref(),
            Some("已完成构建并推送")
        );
        assert_eq!(ups[0].snapshot.waiting_reason, None, "完成即不再等待");
        assert!(ups[0].should_notify);
    }

    #[test]
    fn discovered_clears_stale_reason_and_reply_head() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        m.process(&UnifiedEvent::TurnFinished {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            reply_head: Some("旧一轮摘要".into()),
            ts: TS,
        });
        // 新一轮开始（转录写入 → Discovered）：双清
        let ups = m.process(&disc(AgentKind::ClaudeCode, "s1", TS + 5));
        assert_eq!(ups[0].snapshot.reply_head, None);
        assert_eq!(ups[0].snapshot.waiting_reason, None);

        // Activity 同样清残留
        m.process(&UnifiedEvent::TurnFinished {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            reply_head: Some("old2".into()),
            ts: TS + 6,
        });
        let ups = m.process(&UnifiedEvent::Activity {
            agent: AgentKind::ClaudeCode,
            session_id: "s1".into(),
            ts: TS + 7,
        });
        assert_eq!(ups[0].snapshot.reply_head, None);
    }

    /// 推不出不硬标：Tick 推断 Done 不产生 reply_head（真实摘要只来自 Stop 事件）。
    #[test]
    fn inferred_done_has_no_reply_head() {
        let mut m = Machine::new(DONE_AFTER);
        m.process(&disc(AgentKind::ClaudeCode, "s1", TS));
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + DONE_AFTER,
        });
        assert_eq!(ups[0].snapshot.status, SessionStatus::Done);
        assert!(ups[0].snapshot.done_inferred);
        assert_eq!(ups[0].snapshot.reply_head, None);
    }
}

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
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
                    let new_status = SessionStatus::Waiting(*kind);
                    if e.snap.status != new_status {
                        e.snap.status = new_status.clone();
                        e.snap.last_activity = e.snap.last_activity.max(*ts);
                    }
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
                ts,
            } => {
                if let Some(e) = self.get_mut(agent, session_id) {
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
            ts: TS,
        });
        let ups = m.process(&UnifiedEvent::Tick {
            now: TS + 2 * DONE_AFTER,
        });
        assert!(ups.is_empty(), "等待中的会话不做完成推断");
    }
}

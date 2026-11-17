// src-tauri/src/collectors/claude_events.rs
use crate::core::types::*;
use std::path::PathBuf;

pub struct HookEventsPoller {
    file: PathBuf,
    offset: u64,
}

impl HookEventsPoller {
    pub fn new(file: PathBuf) -> Self {
        Self { file, offset: 0 }
    }

    pub fn poll(&mut self) -> Vec<UnifiedEvent> {
        let mut out = Vec::new();
        let Ok(data) = std::fs::read(&self.file) else {
            return out;
        };
        // 轮转/截断（Task 5 轮转 = rename 旧文件 + 新建空文件）：offset 超过新文件长度
        // 时从头读，首次 poll 必命中归零分支，轮转前后事件无缺口。
        if (self.offset as usize) > data.len() {
            self.offset = 0;
        }
        let start = self.offset as usize;
        let fresh = &data[start..];
        // 只消费到最后一个换行（含）：无换行尾的最后一段是半行，留待下次补全后整行消费。
        // 相比 brief 参考码的逐行计数补偿（半行先行推进 offset，补全后该行 JSON 截断被
        // 跳过、事件丢失），此处 rposition 版是必要修复而非仅简化。
        let end = match fresh.iter().rposition(|b| *b == b'\n') {
            Some(i) => start + i + 1,
            None => start,
        };
        for line in data[start..end].split(|b| *b == b'\n') {
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let Some(session_id) = v.get("sessionId").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(ts) = v.get("ts").and_then(|x| x.as_u64()) else {
                continue;
            };
            // v0.1.3：按事件行 event 字段分派。Notification 行（events_file 自 v0.1.3 起
            // 补 event:"notification"）与旧文件 kind-only 行（无 event 字段）同走既有
            // Waiting(kind) 路径——否则升级后 idle 等待检测端到端断裂（审查修复）。
            match v.get("event").and_then(|x| x.as_str()) {
                Some("permission_request") => {
                    let reason = v
                        .get("tool_summary")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    out.push(UnifiedEvent::Waiting {
                        agent: AgentKind::ClaudeCode,
                        session_id: session_id.to_string(),
                        kind: WaitingKind::Permission,
                        reason,
                        ts,
                    });
                }
                Some("stop") => {
                    let reply_head = v
                        .get("reply_head")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    out.push(UnifiedEvent::TurnFinished {
                        agent: AgentKind::ClaudeCode,
                        session_id: session_id.to_string(),
                        reply_head,
                        ts,
                    });
                }
                Some("notification") | None => {
                    let kind = match v.get("kind").and_then(|x| x.as_str()).unwrap_or("") {
                        "permission_prompt" => WaitingKind::Permission,
                        "idle_prompt" => WaitingKind::Idle,
                        _ => WaitingKind::Other,
                    };
                    out.push(UnifiedEvent::Waiting {
                        agent: AgentKind::ClaudeCode,
                        session_id: session_id.to_string(),
                        kind,
                        reason: None,
                        ts,
                    });
                }
                Some(_) => continue, // 未识别 event → 跳过
            }
        }
        self.offset = end as u64;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    #[test]
    fn parses_waiting_events_incrementally() {
        let f = std::env::temp_dir().join(format!("ab-he-{}.jsonl", std::process::id()));
        std::fs::write(&f, "").unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        assert!(p.poll().is_empty());

        let line = r#"{"agent":"claude-code","sessionId":"s1","cwd":"C:\\w","kind":"permission_prompt","message":"needs permission","ts":123}"#;
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let ev = p.poll();
        assert!(matches!(
            &ev[..],
            [UnifiedEvent::Waiting {
                kind: WaitingKind::Permission,
                ts: 123,
                ..
            }]
        ));

        // 同一行不重复消费
        assert!(p.poll().is_empty());
        std::fs::remove_file(&f).unwrap();
    }

    // v0.1.3：event 字段分派
    #[test]
    fn permission_request_event_maps_to_waiting_with_reason() {
        let f = std::env::temp_dir().join(format!("ab-he4-{}.jsonl", std::process::id()));
        let line = r#"{"agent":"claude-code","sessionId":"s1","event":"permission_request","kind":"permission_prompt","tool_summary":"git push origin main","ts":50}"#;
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert!(matches!(
            &ev[..],
            [UnifiedEvent::Waiting {
                kind: WaitingKind::Permission,
                reason: Some(summary),
                ts: 50,
                ..
            }] if summary == "git push origin main"
        ));
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn stop_event_maps_to_turn_finished_with_reply_head() {
        let f = std::env::temp_dir().join(format!("ab-he5-{}.jsonl", std::process::id()));
        // 带 reply_head
        let lines = concat!(
            r#"{"agent":"claude-code","sessionId":"s2","event":"stop","reply_head":"构建已完成","ts":60}"#,
            "\n",
            r#"{"agent":"claude-code","sessionId":"s3","event":"stop","ts":70}"#,
            "\n",
        );
        std::fs::write(&f, lines).unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert_eq!(ev.len(), 2);
        assert!(matches!(
            &ev[0],
            UnifiedEvent::TurnFinished {
                reply_head: Some(head),
                ts: 60,
                ..
            } if head == "构建已完成"
        ));
        assert!(matches!(
            &ev[1],
            UnifiedEvent::TurnFinished {
                reply_head: None,
                ts: 70,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }

    // 审查修复回归：v0.1.3 起 events_file 给 Notification 行补 event:"notification"，
    // 必须仍走既有 kind-only Waiting 路径（曾有 Some(_) => continue 把它整类丢掉，
    // idle 等待检测端到端断裂）。
    #[test]
    fn notification_event_lines_still_reach_waiting_path() {
        let f = std::env::temp_dir().join(format!("ab-he7-{}.jsonl", std::process::id()));
        let lines = concat!(
            r#"{"agent":"claude-code","sessionId":"s6","event":"notification","kind":"idle_prompt","ts":100}"#,
            "\n",
            r#"{"agent":"claude-code","sessionId":"s7","event":"notification","kind":"permission_prompt","ts":101}"#,
            "\n",
        );
        std::fs::write(&f, lines).unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert_eq!(ev.len(), 2, "带 event:\"notification\" 的行必须消费");
        assert!(matches!(
            &ev[0],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Idle,
                reason: None,
                ts: 100,
                ..
            }
        ));
        assert!(matches!(
            &ev[1],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Permission,
                reason: None,
                ts: 101,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn unknown_event_name_is_skipped_but_legacy_lines_still_map() {
        let f = std::env::temp_dir().join(format!("ab-he6-{}.jsonl", std::process::id()));
        // 未识别 event → 跳过；无 event 字段（旧文件 kind-only 行）→ 兼容 Waiting
        let lines = concat!(
            r#"{"agent":"claude-code","sessionId":"s4","event":"mystery","ts":80}"#,
            "\n",
            r#"{"agent":"claude-code","sessionId":"s5","kind":"idle_prompt","ts":90}"#,
            "\n",
        );
        std::fs::write(&f, lines).unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            &ev[0],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Idle,
                reason: None,
                ts: 90,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn garbage_lines_skipped() {
        let f = std::env::temp_dir().join(format!("ab-he2-{}.jsonl", std::process::id()));
        std::fs::write(&f, "garbage\n{\"agent\":\"claude-code\",\"sessionId\":\"s2\",\"kind\":\"idle_prompt\",\"ts\":9}\n{\"agent\":\"claude-code\",\"sessionId\":\"s3\",\"kind\":\"whatever\",\"ts\":10}\n").unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        let ev = p.poll();
        assert_eq!(ev.len(), 2);
        assert!(matches!(
            &ev[0],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Idle,
                ts: 9,
                ..
            }
        ));
        assert!(matches!(
            &ev[1],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Other,
                ts: 10,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }

    // Task 5 轮转 = rename 旧文件 + 新建空文件：长生命周期 poller 的 offset 持旧大值，
    // 新文件第一次 poll 不得 panic，且要从新文件头完整消费。
    #[test]
    fn shrink_resets_offset_and_repolls() {
        let f = std::env::temp_dir().join(format!("ab-he3-{}.jsonl", std::process::id()));
        std::fs::write(&f, "garbage\n".repeat(1000)).unwrap();
        let mut p = HookEventsPoller::new(f.clone());
        assert!(p.poll().is_empty()); // offset 推进到旧文件尾

        let line = r#"{"agent":"claude-code","sessionId":"s4","kind":"permission_prompt","ts":7}"#;
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let ev = p.poll();
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            &ev[0],
            UnifiedEvent::Waiting {
                kind: WaitingKind::Permission,
                ts: 7,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }
}

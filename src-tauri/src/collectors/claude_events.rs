// src-tauri/src/collectors/claude_events.rs
use crate::core::types::*;
use std::path::PathBuf;

/// hook 事件文件轮询器（泛化于 v0.1.3）：同一 offset 消费逻辑按 agent 分派映射。
/// Claude → events/claude-code.jsonl（sessionId 驼峰行）；ZCode → events/zcode.jsonl
/// （session_id 蛇形行，schema 照 pitwall-hook FILTER 的 zcode 分支）。
pub struct HookEventsPoller {
    file: PathBuf,
    agent: AgentKind,
    offset: u64,
}

impl HookEventsPoller {
    pub fn new(file: PathBuf, agent: AgentKind) -> Self {
        Self {
            file,
            agent,
            offset: 0,
        }
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
            // Claude 行用 sessionId（驼峰），ZCode 行用 session_id（蛇形，pitwall schema）
            let Some(session_id) = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("session_id").and_then(|x| x.as_str()))
            else {
                continue;
            };
            let Some(ts) = v.get("ts").and_then(|x| x.as_u64()) else {
                continue;
            };
            // v0.1.3：按 agent + 事件行 event 字段分派。Claude 的 Notification 行
            //（events_file 自 v0.1.3 起补 event:"notification"）与旧文件 kind-only 行
            //（无 event 字段）同走既有 Waiting(kind) 路径——否则升级后 idle 等待检测
            // 端到端断裂（审查修复）。
            match (self.agent, v.get("event").and_then(|x| x.as_str())) {
                (AgentKind::ClaudeCode, Some("permission_request")) => {
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
                (AgentKind::ClaudeCode, Some("stop")) => {
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
                (AgentKind::ClaudeCode, Some("notification") | None) => {
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
                // Claude 未识别 event → 跳过
                (AgentKind::ClaudeCode, Some(_)) => continue,
                // ZCode：hook 事件名直通（v0.1.3 只注册这四个事件，其余防御性跳过）
                (AgentKind::ZCode, Some("PermissionRequest")) => {
                    let reason = v
                        .get("tool_summary")
                        .and_then(|x| x.as_str())
                        .map(str::to_string);
                    out.push(UnifiedEvent::Waiting {
                        agent: AgentKind::ZCode,
                        session_id: session_id.to_string(),
                        kind: WaitingKind::Permission,
                        reason,
                        ts,
                    });
                }
                (AgentKind::ZCode, Some("PreToolUse" | "PostToolUse" | "PostToolUseFailure")) => {
                    out.push(UnifiedEvent::Activity {
                        agent: AgentKind::ZCode,
                        session_id: session_id.to_string(),
                        ts,
                    });
                }
                (AgentKind::ZCode, _) => continue,
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ClaudeCode);
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

    // ---------- v0.1.3 ZCode 消费（events/zcode.jsonl，蛇形 session_id） ----------

    #[test]
    fn zcode_permission_request_maps_to_waiting_with_reason() {
        let f = std::env::temp_dir().join(format!("ab-zhe1-{}.jsonl", std::process::id()));
        let line = r#"{"v":1,"ts":50,"agent":"zcode","event":"PermissionRequest","session_id":"z1","turn_id":"t1","tool_name":"Bash","tool_summary":"git push origin main","payload_keys":["session_id","tool_input"]}"#;
        std::fs::write(&f, format!("{line}\n")).unwrap();
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ZCode);
        let ev = p.poll();
        assert!(matches!(
            &ev[..],
            [UnifiedEvent::Waiting {
                agent: AgentKind::ZCode,
                kind: WaitingKind::Permission,
                reason: Some(summary),
                ts: 50,
                ..
            }] if summary == "git push origin main"
        ));
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn zcode_tool_events_map_to_activity() {
        let f = std::env::temp_dir().join(format!("ab-zhe2-{}.jsonl", std::process::id()));
        let lines = concat!(
            r#"{"v":1,"ts":60,"agent":"zcode","event":"PreToolUse","session_id":"z1","tool_name":"Read"}"#,
            "\n",
            r#"{"v":1,"ts":61,"agent":"zcode","event":"PostToolUseFailure","session_id":"z1","tool_name":"Bash"}"#,
            "\n",
            // PermissionRequest 无 tool_summary → reason=None 仍产 Waiting
            r#"{"v":1,"ts":62,"agent":"zcode","event":"PermissionRequest","session_id":"z2"}"#,
            "\n",
        );
        std::fs::write(&f, lines).unwrap();
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ZCode);
        let ev = p.poll();
        assert_eq!(ev.len(), 3);
        assert!(matches!(
            &ev[0],
            UnifiedEvent::Activity {
                agent: AgentKind::ZCode,
                ts: 60,
                ..
            }
        ));
        assert!(matches!(
            &ev[1],
            UnifiedEvent::Activity {
                agent: AgentKind::ZCode,
                ts: 61,
                ..
            }
        ));
        assert!(matches!(
            &ev[2],
            UnifiedEvent::Waiting {
                agent: AgentKind::ZCode,
                kind: WaitingKind::Permission,
                reason: None,
                ts: 62,
                ..
            }
        ));
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn zcode_unknown_events_and_claude_lines_are_skipped() {
        let f = std::env::temp_dir().join(format!("ab-zhe3-{}.jsonl", std::process::id()));
        let lines = concat!(
            // ZCode 未消费事件（v0.1.3 只注册四事件）
            r#"{"v":1,"ts":70,"agent":"zcode","event":"SessionStart","session_id":"z3"}"#,
            "\n",
            // Claude 形状的行（驼峰 sessionId / kind-only）不该被 ZCode poller 消费
            r#"{"agent":"claude-code","sessionId":"c1","kind":"idle_prompt","ts":71}"#,
            "\n",
            // 垃圾行
            "garbage\n",
        );
        std::fs::write(&f, lines).unwrap();
        let mut p = HookEventsPoller::new(f.clone(), AgentKind::ZCode);
        assert!(p.poll().is_empty(), "ZCode poller 只消费四事件");
        std::fs::remove_file(&f).unwrap();
    }
}

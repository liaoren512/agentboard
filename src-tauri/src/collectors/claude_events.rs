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
            let kind = match v.get("kind").and_then(|x| x.as_str()).unwrap_or("") {
                "permission_prompt" => WaitingKind::Permission,
                "idle_prompt" => WaitingKind::Idle,
                _ => WaitingKind::Other,
            };
            out.push(UnifiedEvent::Waiting {
                agent: AgentKind::ClaudeCode,
                session_id: session_id.to_string(),
                kind,
                ts,
            });
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

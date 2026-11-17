use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::core::types::*;

/// 简易临时目录守卫：Drop 时递归删除（仅测试用）。
#[cfg(test)]
pub(crate) mod tempdir_guard {
    pub struct TempDir(pub std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Claude 转录采集器（窄接口：只依赖"JSONL 追加写入"这一行为，格式变化只改 parse 逻辑）。
pub struct TranscriptPoller {
    root: PathBuf,
    offsets: HashMap<PathBuf, u64>,
    known: HashSet<String>,
}

impl TranscriptPoller {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            offsets: HashMap::new(),
            known: HashSet::new(),
        }
    }

    pub fn poll(&mut self, now: u64) -> Vec<UnifiedEvent> {
        let mut events = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return events;
        };
        for proj in entries.flatten() {
            let Ok(files) = std::fs::read_dir(proj.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                self.poll_file(&path, now, &mut events);
            }
        }
        events
    }

    fn poll_file(&mut self, path: &Path, now: u64, events: &mut Vec<UnifiedEvent>) {
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let start = *self.offsets.entry(path.to_path_buf()).or_insert(0);
        if (data.len() as u64) <= start {
            return;
        }
        let fresh = &data[start as usize..];
        let mut consumed = 0u64;
        let mut discovered = false;
        let mut activity = false;
        let mut failed = false;
        let mut session_id: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut title: Option<String> = None;
        let mut lines = fresh.split(|b| *b == b'\n').peekable();
        while let Some(line) = lines.next() {
            // 只消费完整行：本段后面还有分隔符(\n)才算 len+1；末尾空段/半行不计，留待下次 poll
            let complete = lines.peek().is_some();
            if complete {
                consumed += line.len() as u64 + 1;
            }
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let sid = v
                .get("sessionId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if sid.is_empty() {
                continue;
            }
            session_id = Some(sid.clone());
            if cwd.is_none() {
                cwd = v.get("cwd").and_then(|x| x.as_str()).map(|s| s.to_string());
            }
            if title.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("user") {
                title = extract_user_text(&v).map(|t| t.chars().take(80).collect());
            }
            if !failed && v.get("isApiErrorMessage").and_then(|x| x.as_bool()) == Some(true) {
                failed = true;
            }
            activity = true;
        }
        // 按行边界推进 offset（末尾半行未消费，下次 poll 重读）
        let new_off = start + consumed;
        self.offsets.insert(path.to_path_buf(), new_off);

        let Some(sid) = session_id else { return };
        let agent = AgentKind::ClaudeCode;
        if !self.known.contains(&sid) {
            self.known.insert(sid.clone());
            discovered = true;
        }
        if discovered {
            events.push(UnifiedEvent::Discovered {
                agent,
                session_id: sid.clone(),
                cwd: cwd.unwrap_or_default(),
                title,
                ts: now,
            });
        }
        if activity {
            events.push(UnifiedEvent::Activity {
                agent,
                session_id: sid.clone(),
                ts: now,
            });
        }
        if failed {
            events.push(UnifiedEvent::TurnFailed {
                agent,
                session_id: sid,
                ts: now,
            });
        }
    }
}

fn extract_user_text(v: &serde_json::Value) -> Option<String> {
    let msg = v.get("message")?;
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(a)) => a.iter().find_map(|c| {
            c.get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 每个测试独立目录（brief 原版共享 ab-tr-{pid}，并行测试互相读写 + Drop 互删，会 flaky）
    fn fixture_dir(name: &str) -> (tempdir_guard::TempDir, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("ab-tr-{}-{}", std::process::id(), name));
        let proj = base.join("C--Apps-repo-x");
        std::fs::create_dir_all(&proj).unwrap();
        (tempdir_guard::TempDir(base), proj)
    }
    // 见 tempdir_guard：Drop 时删目录，测试自清理

    const FIRST_LINE: &str = r#"{"type":"user","sessionId":"s-1","cwd":"C:\\Apps\\repo\\x","message":{"role":"user","content":"帮我修复登录 bug"},"timestamp":"2026-11-17T10:00:00Z"}"#;

    #[test]
    fn new_file_discovered_with_title_and_activity() {
        let (_g, proj) = fixture_dir("discover");
        std::fs::write(proj.join("s-1.jsonl"), format!("{FIRST_LINE}\n")).unwrap();
        let mut p = TranscriptPoller::new(_g.0.clone());
        let ev = p.poll(1000);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Discovered { session_id, title: Some(t), .. } if session_id == "s-1" && t.contains("修复登录"))));
        assert!(ev.iter().any(
            |e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id == "s-1")
        ));
    }

    #[test]
    fn appended_lines_emit_activity_error_emits_turn_failed() {
        let (_g, proj) = fixture_dir("append");
        let f = proj.join("s-1.jsonl");
        std::fs::write(&f, format!("{FIRST_LINE}\n")).unwrap();
        let mut p = TranscriptPoller::new(_g.0.clone());
        p.poll(1000);

        let err_line = r#"{"type":"assistant","sessionId":"s-1","isApiErrorMessage":true,"message":{"content":"API Error: 500"}}"#;
        std::fs::write(&f, format!("{FIRST_LINE}\n{err_line}\n")).unwrap();
        let ev = p.poll(2000);
        assert!(
            ev.iter()
                .any(|e| matches!(e, UnifiedEvent::Activity { .. }))
        );
        assert!(
            ev.iter()
                .any(|e| matches!(e, UnifiedEvent::TurnFailed { .. }))
        );
    }

    #[test]
    fn partial_line_is_buffered_not_parsed() {
        let (_g, proj) = fixture_dir("partial");
        let f = proj.join("s-2.jsonl");
        std::fs::write(&f, format!("{FIRST_LINE}\n{{\"type\":\"assi")).unwrap(); // 半行
        let mut p = TranscriptPoller::new(_g.0.clone());
        let ev = p.poll(1000);
        // 半行不能产生第二个事件（第一个完整行已产生 Discovered）
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, UnifiedEvent::Activity { .. }))
                .count(),
            1
        );
        // 补全后半行 → 下次 poll 产出 Activity
        std::fs::write(
            &f,
            format!("{FIRST_LINE}\n{{\"type\":\"assistant\",\"sessionId\":\"s-2\"}}\n"),
        )
        .unwrap();
        let ev = p.poll(2000);
        assert!(ev.iter().any(
            |e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id == "s-2")
        ));
    }
}

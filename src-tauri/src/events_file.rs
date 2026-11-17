use std::io::Write;
use std::path::Path;

pub const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

pub fn append_event(line: &str, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join("claude-code.jsonl");
    if file.metadata().map(|m| m.len()).unwrap_or(0) > ROTATE_BYTES {
        let _ = std::fs::rename(&file, dir.join("claude-code.jsonl.1"));
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")
}

/// message 文本 → kind 标签（与注册的 matcher 类型对齐，兜底 other）。
pub fn classify(message: &str) -> &'static str {
    let m = message.to_lowercase();
    if m.contains("permission") {
        "permission_prompt"
    } else if m.contains("waiting for your input") {
        "idle_prompt"
    } else {
        "other"
    }
}

/// 纯函数便于测试：stdin 文本 → 事件行；无 session_id 或解析失败 → None。
pub fn handle_input(input: &str, ts: u64) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    if session_id.is_empty() {
        return None;
    }
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
    let truncated: String = message.chars().take(120).collect();
    let event = serde_json::json!({
        "agent": "claude-code",
        "sessionId": session_id,
        "cwd": cwd,
        "kind": classify(message),
        "message": truncated,
        "ts": ts,
    });
    Some(event.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_rotates_over_10mb() {
        let dir = std::env::temp_dir().join(format!("ab-ev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        append_event(r#"{"a":1}"#, &dir).unwrap();
        append_event(r#"{"a":2}"#, &dir).unwrap();
        let content = std::fs::read_to_string(dir.join("claude-code.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 2);

        // 造一个超过阈值的文件 → 追加后应轮转
        let big = "x".repeat((ROTATE_BYTES + 1) as usize);
        std::fs::write(dir.join("claude-code.jsonl"), big).unwrap();
        append_event(r#"{"a":3}"#, &dir).unwrap();
        assert!(
            !std::fs::read_to_string(dir.join("claude-code.jsonl"))
                .unwrap()
                .contains("xxxx")
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2); // .jsonl + .jsonl.1
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classify_message() {
        assert_eq!(
            classify("Claude needs your permission to use Bash"),
            "permission_prompt"
        );
        assert_eq!(classify("Claude is waiting for your input"), "idle_prompt");
        assert_eq!(classify("Claude is an agent needing input"), "other");
    }

    #[test]
    fn handle_input_garbage_is_none_valid_is_line() {
        assert!(handle_input("not json", 1).is_none());
        assert!(
            handle_input(r#"{"hook_event_name":"PreToolUse"}"#, 1).is_none(),
            "无 session_id 不产事件"
        );
        let line = handle_input(
            r#"{"session_id":"s9","cwd":"C:\\w","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#, 42,
        ).unwrap();
        assert!(line.contains(r#""sessionId":"s9""#));
        assert!(line.contains(r#""kind":"permission_prompt""#));
        assert!(line.contains(r#""ts":42"#));
    }
}

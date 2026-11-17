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

// ---------- 脱敏截断（字段设计与 Pitwall pitwall-hook FILTER 对齐） ----------

/// jq cut：超 n 字截断加 …（按字符数，与 jq 的 codepoint length 一致）。
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// pitwall line_head：首个非空行（trim 前后空白），截 n 字加 …；无非空行 → None。
fn line_head(s: &str, n: usize) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| cut(l, n))
}

/// pitwall squashed：连续空白压成单个空格成一行，截 n 字加 …；全空白 → None。
fn squashed(s: &str, n: usize) -> Option<String> {
    let one = s.split_whitespace().collect::<Vec<&str>>().join(" ");
    if one.is_empty() {
        None
    } else {
        Some(cut(&one, n))
    }
}

/// PermissionRequest 的 tool_summary：tool_input 的 command/file_path/notebook_path/
/// url/pattern 首个非空行（120 字）；AskUserQuestion 特例取 questions[0].question。
/// tool_input 解析失败/字段缺失 → None（事件行省略该字段）。
fn tool_summary(v: &serde_json::Value) -> Option<String> {
    let ti = v.get("tool_input")?.as_object()?;
    let field = ["command", "file_path", "notebook_path", "url", "pattern"]
        .iter()
        .find_map(|k| ti.get(*k).and_then(|x| x.as_str()));
    let field = field.or_else(|| {
        if v.get("tool_name").and_then(|x| x.as_str()) == Some("AskUserQuestion") {
            ti.get("questions")
                .and_then(|q| q.as_array())
                .and_then(|a| a.first())
                .and_then(|q0| q0.get("question"))
                .and_then(|x| x.as_str())
        } else {
            None
        }
    });
    field.and_then(|s| line_head(s, 120))
}

/// 纯函数便于测试：stdin 文本 → 事件行；无 session_id 或解析失败 → None。
/// 按 hook_event_name 分派（v0.1.3）：
/// - Notification → 既有 classify 路径，事件行补 event:"notification"；
/// - PermissionRequest → event:"permission_request" + kind:"permission_prompt" + tool_summary；
/// - Stop → event:"stop" + reply_head（last_assistant_message 压一行 160 字）；
/// - 其余 → 既有路径不变（无 event 字段，消费端按兼容行处理）。None 字段一律省略。
pub fn handle_input(input: &str, ts: u64) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    if session_id.is_empty() {
        return None;
    }
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let hook = v
        .get("hook_event_name")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let mut event = serde_json::json!({
        "agent": "claude-code",
        "sessionId": session_id,
        "cwd": cwd,
        "ts": ts,
    });
    let obj = event.as_object_mut().expect("json! 一定是对象");
    match hook {
        "PermissionRequest" => {
            obj.insert("event".into(), "permission_request".into());
            obj.insert("kind".into(), "permission_prompt".into());
            if let Some(s) = tool_summary(&v) {
                obj.insert("tool_summary".into(), s.into());
            }
        }
        "Stop" => {
            obj.insert("event".into(), "stop".into());
            if let Some(s) = v
                .get("last_assistant_message")
                .and_then(|x| x.as_str())
                .and_then(|m| squashed(m, 160))
            {
                obj.insert("reply_head".into(), s.into());
            }
        }
        _ => {
            // Notification 及未注册事件：既有 classify 路径
            let message = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
            let truncated: String = message.chars().take(120).collect();
            obj.insert("kind".into(), classify(message).into());
            obj.insert("message".into(), truncated.into());
            if hook == "Notification" {
                obj.insert("event".into(), "notification".into());
            }
        }
    }
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

    // v0.1.3：事件行加 event 字段；Notification 行补 "notification"
    #[test]
    fn notification_line_gains_event_field() {
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"Notification","message":"Claude needs your permission to use Bash"}"#,
            1,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "notification");
        assert_eq!(v["kind"], "permission_prompt");
        assert!(v.get("tool_summary").is_none());
        assert!(v.get("reply_head").is_none());
    }

    // PermissionRequest → event:"permission_request" + kind + tool_summary
    #[test]
    fn permission_request_extracts_tool_summary() {
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"git push origin main\nsecond line"}}"#,
            2,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "permission_request");
        assert_eq!(v["kind"], "permission_prompt");
        // 取首个非空行
        assert_eq!(v["tool_summary"], "git push origin main");
        assert!(
            v.get("message").is_none(),
            "PermissionRequest 无 message 字段"
        );
    }

    #[test]
    fn tool_summary_truncates_to_120_chars() {
        let cmd = "x".repeat(150);
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": cmd },
        });
        let line = handle_input(&payload.to_string(), 3).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let expected = format!("{}…", "x".repeat(120));
        assert_eq!(v["tool_summary"], expected);
    }

    #[test]
    fn tool_summary_field_priority_and_ask_user_question() {
        // command 优先于 file_path
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"file_path":"C:\\a.txt","command":"ls"}}"#,
            4,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["tool_summary"], "ls");

        // AskUserQuestion 特例 → questions[0].question
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"questions":[{"header":"方案","question":"选哪个方案？"}]}}"#,
            5,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["tool_summary"], "选哪个方案？");
    }

    // tool_input 解析失败/字段缺失 → tool_summary 省略（None 字段不写入）
    #[test]
    fn permission_request_without_matching_field_omits_tool_summary() {
        // 字段全不匹配（query 不在提取名单）
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_name":"WebSearch","tool_input":{"query":"rust"}}"#,
            6,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("tool_summary").is_none());

        // tool_input 非对象（解析失败）
        let line = handle_input(
            r#"{"session_id":"s1","hook_event_name":"PermissionRequest","tool_input":"oops"}"#,
            7,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("tool_summary").is_none());
        assert_eq!(v["event"], "permission_request");
    }

    // Stop → event:"stop" + reply_head（last_assistant_message 压一行）
    #[test]
    fn stop_line_has_reply_head_squashed() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "Stop",
            "last_assistant_message": "第一行\n\n第二行   很长\n\t第三行",
        });
        let line = handle_input(&payload.to_string(), 8).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "stop");
        assert_eq!(v["reply_head"], "第一行 第二行 很长 第三行");
        assert!(v.get("kind").is_none(), "Stop 行无 kind");
    }

    #[test]
    fn stop_reply_head_truncated_and_omitted_when_missing() {
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "Stop",
            "last_assistant_message": "y".repeat(200),
        });
        let line = handle_input(&payload.to_string(), 9).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let expected = format!("{}…", "y".repeat(160));
        assert_eq!(v["reply_head"], expected);

        // 缺 last_assistant_message / 空串 → 省略，event 仍在
        for payload in [
            serde_json::json!({"session_id": "s1", "hook_event_name": "Stop"}),
            serde_json::json!({"session_id": "s1", "hook_event_name": "Stop", "last_assistant_message": ""}),
        ] {
            let line = handle_input(&payload.to_string(), 10).unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert!(v.get("reply_head").is_none());
            assert_eq!(v["event"], "stop");
        }
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

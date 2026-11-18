//! ZCode hooks 桥：把 ZCode 的 hook 事件脱敏成事件行，追加到 events/zcode.jsonl。
//! 权威参考（MIT）：docs/reference/pitwall-collector/pitwall-hook-zcode（校验桥：
//! 事件过滤 / 进程链 / db 校验）+ pitwall-hook FILTER 的 zcode 分支（ts 解析 / 字段抑制）。
//! 铁律：observe-only、任何异常不 panic（调用方兜底 exit 0）、无 stdout、毫秒级返回。
use std::path::Path;

/// v0.1.3 注册的四个 ZCode hook 事件（与 installer::ZCODE_EVENTS 对齐）。
pub const ZCODE_EVENTS: [&str; 4] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
];

/// 纯函数（注入进程链校验结果与 db 路径，便于测试）：stdin payload → 事件行。
/// ① event ∈ 四事件集合，否则 None；② 非桌面进程链 → None；
/// ③ payload 非 object / 无 session_id → None；
/// ④ PermissionRequest 需 db 校验：根交互会话（parent_id IS NULL 且
///    task_type='interactive' 且 time_deleted IS NULL）才产出，查不到/出错 → None；
///    其余事件不碰 db（db_path=None 也放行，照 pitwall-hook-zcode）。
/// ⑤ 事件行照 pitwall-hook FILTER 的 zcode 分支——刻意抑制 message/reason/error，
///    仅 PermissionRequest 记 tool_summary（告诉用户"在等什么"）。
pub fn run_zcode_hook(
    event: &str,
    stdin_json: &str,
    now_ms: u64,
    is_zcode_desktop: bool,
    db_path: Option<&Path>,
) -> Option<String> {
    // ① 事件过滤（大小写敏感，与注册的事件名一致）
    if !matches!(
        event,
        "PreToolUse" | "PermissionRequest" | "PostToolUse" | "PostToolUseFailure"
    ) {
        return None;
    }
    // ② 进程链校验结果（真实路径：sysinfo 自 PPID 上溯找 ZCode.exe）
    if !is_zcode_desktop {
        return None;
    }
    // ③ payload 守卫
    let payload: serde_json::Value = serde_json::from_str(stdin_json).ok()?;
    let obj = payload.as_object()?;
    let session_id = obj.get("session_id")?.as_str()?;
    if session_id.is_empty() {
        return None;
    }
    // ④ PermissionRequest 需 db 校验：只有根交互会话可发布（照 pitwall-hook-zcode）；
    //    其余事件不碰 db——它们只携带标识，消费端自行丢弃子会话。
    if event == "PermissionRequest" {
        let db = db_path?;
        if !interactive_root_session(db, session_id) {
            return None;
        }
    }
    // ⑤ 事件行（照 pitwall-hook FILTER 的 zcode 分支）：
    //    ts 解析失败 → now；刻意抑制 message/reason/error；None 字段一律省略。
    let ts = obj
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_zcode_ts)
        .unwrap_or(now_ms);
    let mut line = serde_json::json!({
        "v": 1,
        "ts": ts,
        "agent": "zcode",
        "event": event,
        "session_id": session_id,
        "payload_keys": obj.keys().collect::<Vec<_>>(),
    });
    let m = line.as_object_mut().expect("json! 一定是对象");
    if let Some(t) = obj.get("turnId").and_then(serde_json::Value::as_str) {
        m.insert("turn_id".into(), serde_json::json!(t));
    }
    if let Some(t) = obj.get("tool_name").and_then(serde_json::Value::as_str) {
        m.insert("tool_name".into(), serde_json::json!(t));
    }
    if let Some(t) = obj.get("tool_use_id").and_then(serde_json::Value::as_str) {
        m.insert("tool_use_id".into(), serde_json::json!(t));
    }
    if event == "PermissionRequest" {
        if let Some(s) = obj
            .get("tool_input")
            .and_then(|ti| {
                ["command", "file_path", "url", "path"]
                    .iter()
                    .find_map(|k| ti.get(*k).and_then(serde_json::Value::as_str))
            })
            .and_then(|s| crate::events_file::line_head(s, 120))
        {
            m.insert("tool_summary".into(), serde_json::json!(s));
        }
    }
    Some(line.to_string())
}

/// PermissionRequest 的 db 校验：readonly 查根交互会话。
/// 查无此行 / 打不开 / 查询出错 → false（照 pitwall 的 `|| exit 0` 语义，宁可漏报）。
fn interactive_root_session(db_path: &Path, session_id: &str) -> bool {
    let Ok(con) =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    con.query_row(
        "SELECT 1 FROM session WHERE id = ?1 AND parent_id IS NULL AND task_type = 'interactive' AND time_deleted IS NULL",
        rusqlite::params![session_id],
        |_| Ok(()),
    )
    .is_ok()
}

/// 真实包装（hook_main 调用）：sysinfo 进程链校验 + ZCode db 路径。
pub fn run_zcode_hook_real(event: &str, stdin_json: &str, now_ms: u64) -> Option<String> {
    let is_desktop = zcode_desktop_ancestor();
    run_zcode_hook(
        event,
        stdin_json,
        now_ms,
        is_desktop,
        Some(&crate::zcode_db_path()),
    )
}

/// 进程链校验：sysinfo 自 PPID 上溯 ≤8 层找 ZCode.exe（桌面主进程）。
/// 照 pitwall-hook-zcode 的 ps 链语义：防止 CLI 独立启动的会话冒充桌面会话。
pub fn zcode_desktop_ancestor() -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, false);
    let mut pid = Pid::from_u32(std::process::id());
    // 自身不算，从 PPID 起最多 8 层
    for _ in 0..8 {
        let Some(parent) = sys.process(pid).and_then(|p| p.parent()) else {
            return false;
        };
        pid = parent;
        let Some(p) = sys.process(pid) else {
            return false;
        };
        if process_is_zcode(p.name()) {
            return true;
        }
    }
    false
}

/// 进程名是否为桌面主进程 ZCode.exe（大小写不敏感、兼容缺 .exe 后缀的名称形式）。
fn process_is_zcode(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy().to_ascii_lowercase();
    let base = n.strip_suffix(".exe").unwrap_or(&n);
    base == "zcode"
}

/// payload.timestamp（ISO 8601，pitwall 正则 `^(?<whole>[^.]+)(?:\.(?<ms>\d{1,9}))?Z$`）
/// → epoch ms。whole 必须形如 YYYY-MM-DDTHH:MM:SS；毫秒取小数前 3 位（不足补 0，
/// 与 jq 的 `+ "000"` 后 `[0:3]` 一致）。格式不符 → None（调用方回退 now）。
fn parse_zcode_ts(s: &str) -> Option<u64> {
    let body = s.strip_suffix('Z')?;
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => {
            if w.contains('.')
                || f.is_empty()
                || f.len() > 9
                || !f.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            (w, f)
        }
        None => (body, ""),
    };
    let b = whole.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16) || c.is_ascii_digit())
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { whole.get(r)?.parse().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    let epoch_s = days * 86_400 + hour * 3_600 + minute * 60 + second;
    let ms: i64 = format!("{frac}000")
        .chars()
        .take(3)
        .collect::<String>()
        .parse()
        .ok()?;
    epoch_s
        .checked_mul(1000)?
        .checked_add(ms)
        .filter(|t| *t >= 0)
        .map(|t| t as u64)
}

/// Hinnant days_from_civil：公历日期 → 自 1970-01-01 的天数（无 chrono 依赖的最小实现）。
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(m) + 9) % 12; // [0, 11]，三月=0
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::PathBuf;

    // ---------- 事件过滤 / 进程链 / payload 守卫 ----------

    #[test]
    fn event_filter_accepts_only_four_zcode_events() {
        let payload = r#"{"session_id":"s1"}"#;
        // 三个不碰 db 的事件：db_path=None 也放行（PermissionRequest 需 db，在 db 用例覆盖）
        for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
            assert!(
                run_zcode_hook(event, payload, 1, true, None).is_some(),
                "{event} 应放行"
            );
        }
        for event in [
            "Notification",
            "SessionStart",
            "",
            "pretooluse",
            "PreToolUse ",
        ] {
            assert!(
                run_zcode_hook(event, payload, 1, true, None).is_none(),
                "非法事件 {event:?} 应为 None"
            );
        }
    }

    #[test]
    fn non_desktop_process_chain_is_none() {
        let payload = r#"{"session_id":"s1"}"#;
        assert!(
            run_zcode_hook("PreToolUse", payload, 1, false, None).is_none(),
            "进程链不含 ZCode.exe → None（防 CLI 冒充桌面会话）"
        );
    }

    #[test]
    fn payload_guards() {
        // 非 JSON / 非 object / 无 session_id / 空 session_id → None
        for bad in [
            "not json",
            "[1,2,3]",
            r#"{"turnId":"t1"}"#,
            r#"{"session_id":""}"#,
        ] {
            assert!(
                run_zcode_hook("PreToolUse", bad, 1, true, None).is_none(),
                "payload {bad:?} 应为 None"
            );
        }
    }

    // ---------- PermissionRequest 的 db 校验（只读查询） ----------

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ab-zhook-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_db(path: &Path) {
        let con = rusqlite::Connection::open(path).unwrap();
        con.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, parent_id TEXT, task_type TEXT, time_deleted INTEGER);
             INSERT INTO session VALUES ('root1', NULL, 'interactive', NULL);
             INSERT INTO session VALUES ('sub1', 'root1', 'interactive', NULL);
             INSERT INTO session VALUES ('agent1', NULL, 'agent', NULL);
             INSERT INTO session VALUES ('gone1', NULL, 'interactive', 123);",
        )
        .unwrap();
    }

    fn pr_line(db: Option<&Path>, session: &str) -> Option<String> {
        let payload = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PermissionRequest",
            "timestamp": "2026-11-18T12:34:56.123Z",
            "tool_name": "Bash",
            "tool_input": { "command": "git push" },
        })
        .to_string();
        run_zcode_hook("PermissionRequest", &payload, 1, true, db)
    }

    #[test]
    fn permission_request_validates_root_interactive_session() {
        let t = tmp("db");
        let db = t.join("db.sqlite");
        make_db(&db);
        assert!(pr_line(Some(&db), "root1").is_some(), "根交互会话放行");
        assert!(
            pr_line(Some(&db), "sub1").is_none(),
            "子会话（parent_id）拒绝"
        );
        assert!(
            pr_line(Some(&db), "agent1").is_none(),
            "task_type=agent 拒绝"
        );
        assert!(pr_line(Some(&db), "gone1").is_none(), "已删除会话拒绝");
        assert!(pr_line(Some(&db), "missing").is_none(), "查无此会话拒绝");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn permission_request_without_db_or_bad_db_is_none() {
        assert!(
            pr_line(None, "root1").is_none(),
            "PermissionRequest 无 db → None"
        );
        let t = tmp("bad-db");
        let missing = t.join("missing.sqlite");
        assert!(
            pr_line(Some(&missing), "root1").is_none(),
            "db 不存在 → None"
        );
        let garbage = t.join("garbage.sqlite");
        std::fs::write(&garbage, b"not a sqlite file").unwrap();
        assert!(
            pr_line(Some(&garbage), "root1").is_none(),
            "db 损坏/查询出错 → None"
        );
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn non_permission_events_do_not_touch_db() {
        // pitwall：只有审批请求需要 db；其余事件只带标识，db_path=None 也放行
        let payload = serde_json::json!({ "session_id": "s1", "turnId": "t1" }).to_string();
        assert!(
            run_zcode_hook("PreToolUse", &payload, 1, true, None).is_some(),
            "PreToolUse 无 db 也放行"
        );
    }

    // ---------- 事件行 schema（照 pitwall-hook FILTER 的 zcode 分支） ----------

    fn full_pr_payload() -> String {
        serde_json::json!({
            "session_id": "root1",
            "hook_event_name": "PermissionRequest",
            "timestamp": "2026-11-18T12:34:56.123Z",
            "turnId": "turn-9",
            "tool_name": "Bash",
            "tool_use_id": "call-1",
            "tool_input": { "command": "git push origin main\nsecond line" },
            "message": "should not appear",
            "reason": "should not appear",
            "error": "should not appear",
        })
        .to_string()
    }

    #[test]
    fn schema_line_completeness_and_field_suppression() {
        let line = run_zcode_hook("PermissionRequest", &full_pr_payload(), 999, true, None);
        assert!(
            line.is_none(),
            "db_path=None 时 PermissionRequest 不产出（含 schema 测试走 db 用例）"
        );
    }

    #[test]
    fn schema_line_fields_with_db() {
        let t = tmp("schema");
        let db = t.join("db.sqlite");
        make_db(&db);
        let line = run_zcode_hook(
            "PermissionRequest",
            &full_pr_payload(),
            999,
            true,
            Some(&db),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["ts"], 1_795_005_296_123u64, "timestamp → epoch ms");
        assert_eq!(v["agent"], "zcode");
        assert_eq!(v["event"], "PermissionRequest");
        assert_eq!(v["session_id"], "root1");
        assert_eq!(v["turn_id"], "turn-9", "turn_id 来自 payload.turnId");
        assert_eq!(v["tool_name"], "Bash");
        assert_eq!(v["tool_use_id"], "call-1");
        assert_eq!(v["tool_summary"], "git push origin main", "首个非空行");
        // payload_keys 记录字段名（内容不落盘）
        let keys = v["payload_keys"].as_array().unwrap();
        assert_eq!(keys.len(), 10, "payload 的全部字段名");
        for k in [
            "session_id",
            "hook_event_name",
            "timestamp",
            "turnId",
            "tool_name",
            "tool_use_id",
            "tool_input",
            "message",
            "reason",
            "error",
        ] {
            assert!(keys.iter().any(|x| x == k), "payload_keys 含 {k}");
        }
        // Pitwall 对 zcode 刻意抑制：不记 message/reason/error，也无 kind（claude 行才有）
        assert!(v.get("message").is_none());
        assert!(v.get("reason").is_none());
        assert!(v.get("error").is_none());
        assert!(v.get("kind").is_none());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn activity_events_have_no_tool_summary() {
        let t = tmp("act");
        let db = t.join("db.sqlite");
        make_db(&db);
        let payload = serde_json::json!({
            "session_id": "root1",
            "timestamp": "2026-11-18T12:34:56Z",
            "turnId": "t1",
            "tool_name": "Read",
            "tool_input": { "file_path": "C:\\x.rs" },
        })
        .to_string();
        let line = run_zcode_hook("PreToolUse", &payload, 7, true, Some(&db)).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(
            v.get("tool_summary").is_none(),
            "非 PermissionRequest 不记 tool_summary"
        );
        assert_eq!(v["ts"], 1_795_005_296_000u64, "无小数位毫秒 = 0");
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn tool_summary_field_priority_command_file_path_url_path() {
        let t = tmp("prio");
        let db = t.join("db.sqlite");
        make_db(&db);
        let mk = |ti: Value| -> String {
            serde_json::json!({
                "session_id": "root1",
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
                "tool_input": ti,
            })
            .to_string()
        };
        let line_of = |ti: Value| -> Value {
            run_zcode_hook("PermissionRequest", &mk(ti), 1, true, Some(&db))
                .and_then(|l| serde_json::from_str::<Value>(&l).ok())
                .unwrap()
        };
        // command 优先于 file_path / url / path
        let v = line_of(serde_json::json!({
            "path": "p", "url": "https://a", "file_path": "C:\\f", "command": "ls"
        }));
        assert_eq!(v["tool_summary"], "ls");
        // 缺 command → file_path
        let v = line_of(serde_json::json!({ "url": "https://a", "file_path": "C:\\f.txt" }));
        assert_eq!(v["tool_summary"], "C:\\f.txt");
        // 缺 command/file_path → url
        let v = line_of(serde_json::json!({ "path": "p", "url": "https://x/y" }));
        assert_eq!(v["tool_summary"], "https://x/y");
        // 只剩 path
        let v = line_of(serde_json::json!({ "path": "C:\\p" }));
        assert_eq!(v["tool_summary"], "C:\\p");
        // 四字段全缺 / tool_input 非对象 → tool_summary 省略（事件行仍在）
        let v = line_of(serde_json::json!({ "query": "rust" }));
        assert!(v.get("tool_summary").is_none());
        let line = run_zcode_hook(
            "PermissionRequest",
            r#"{"session_id":"root1","tool_input":"oops"}"#,
            1,
            true,
            Some(&db),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("tool_summary").is_none());
        std::fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn tool_summary_truncates_to_120_chars() {
        let t = tmp("trunc");
        let db = t.join("db.sqlite");
        make_db(&db);
        let cmd = "x".repeat(150);
        let payload = serde_json::json!({
            "session_id": "root1",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": cmd },
        })
        .to_string();
        let line = run_zcode_hook("PermissionRequest", &payload, 1, true, Some(&db)).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["tool_summary"], format!("{}…", "x".repeat(120)));
        std::fs::remove_dir_all(&t).unwrap();
    }

    // ---------- ts 解析（照 pitwall 的 ISO 正则 + 毫秒取前 3 位） ----------

    #[test]
    fn parse_zcode_ts_known_values() {
        assert_eq!(parse_zcode_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_zcode_ts("2024-01-01T00:00:00Z"),
            Some(1_704_067_200_000)
        );
        assert_eq!(
            parse_zcode_ts("2026-11-18T12:34:56Z"),
            Some(1_795_005_296_000)
        );
        assert_eq!(
            parse_zcode_ts("2026-11-18T12:34:56.123Z"),
            Some(1_795_005_296_123)
        );
        // 不足 3 位补 0（"5" → 500）
        assert_eq!(
            parse_zcode_ts("2026-11-18T12:34:56.5Z"),
            Some(1_795_005_296_500)
        );
        // 超过 3 位取前 3 位（照 jq [0:3]）
        assert_eq!(
            parse_zcode_ts("2026-11-18T12:34:56.123456789Z"),
            Some(1_795_005_296_123)
        );
    }

    #[test]
    fn parse_zcode_ts_rejects_malformed() {
        // pitwall 正则要求 Z 结尾且 whole 不含点；不合形 → None（回退 now）
        for bad in [
            "2026-11-18T12:34:56+08:00",
            "2026-11-18T12:34:56",
            "",
            "not-a-dateZ",
            "2026-13-01T00:00:00Z",
            "2026-11-32T00:00:00Z",
            "2026-11-18T25:00:00Z",
            "2026-11-18T12:34:56.1234567890123Z",
            "2026-11-18T12:34:56..5Z",
        ] {
            assert_eq!(parse_zcode_ts(bad), None, "{bad:?} 应解析失败");
        }
    }

    #[test]
    fn bad_timestamp_falls_back_to_now() {
        let t = tmp("tsfall");
        let db = t.join("db.sqlite");
        make_db(&db);
        let payload = serde_json::json!({
            "session_id": "root1",
            "hook_event_name": "PermissionRequest",
            "timestamp": "bogus",
            "tool_name": "Bash",
        })
        .to_string();
        let line = run_zcode_hook("PermissionRequest", &payload, 4242, true, Some(&db)).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ts"], 4242, "解析失败 → now");
        // 缺 timestamp 字段同样回退
        let payload =
            serde_json::json!({ "session_id": "root1", "hook_event_name": "PermissionRequest" })
                .to_string();
        let line = run_zcode_hook("PermissionRequest", &payload, 777, true, Some(&db)).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ts"], 777);
        std::fs::remove_dir_all(&t).unwrap();
    }

    // ---------- 进程名判定 ----------

    #[test]
    fn process_name_matches_zcode_exe_forms() {
        use std::ffi::OsStr;
        assert!(process_is_zcode(OsStr::new("ZCode.exe")));
        assert!(process_is_zcode(OsStr::new("zcode.exe")));
        assert!(process_is_zcode(OsStr::new("ZCODE.EXE")));
        assert!(process_is_zcode(OsStr::new("ZCode")));
        assert!(!process_is_zcode(OsStr::new("ZCode Helper.exe")));
        assert!(!process_is_zcode(OsStr::new("notzcode.exe")));
        assert!(!process_is_zcode(OsStr::new("node.exe")));
    }
}

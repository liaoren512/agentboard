use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
    // 显式指定：snake_case 会把 ZCode 拆成 "z_code"，与前端约定的 "zcode" 不符。
    #[serde(rename = "zcode")]
    ZCode,
}

impl AgentKind {
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::ZCode => "ZCode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingKind {
    Permission,
    Question,
    Idle,
    Other,
}

impl WaitingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WaitingKind::Permission => "permission",
            WaitingKind::Question => "question",
            WaitingKind::Idle => "idle",
            WaitingKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    Running,
    Waiting(WaitingKind), // "waiting_permission" / "waiting_idle" / ...
    Failed,
    Done,
}

// 序列化为扁平字符串："running" / "waiting_permission" / "waiting_idle" / ...（derive 的
// rename_all 对带数据变体只能产出 {"waiting":"idle"}，无法得到测试约定的 waiting_<kind>）。
impl Serialize for SessionStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            SessionStatus::Running => "running".to_string(),
            SessionStatus::Waiting(k) => format!("waiting_{}", k.as_str()),
            SessionStatus::Failed => "failed".to_string(),
            SessionStatus::Done => "done".to_string(),
        };
        serializer.serialize_str(&s)
    }
}

impl<'de> Deserialize<'de> for SessionStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "running" => Ok(SessionStatus::Running),
            "waiting_permission" => Ok(SessionStatus::Waiting(WaitingKind::Permission)),
            "waiting_question" => Ok(SessionStatus::Waiting(WaitingKind::Question)),
            "waiting_idle" => Ok(SessionStatus::Waiting(WaitingKind::Idle)),
            "waiting_other" => Ok(SessionStatus::Waiting(WaitingKind::Other)),
            "failed" => Ok(SessionStatus::Failed),
            "done" => Ok(SessionStatus::Done),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &[
                    "running",
                    "waiting_permission",
                    "waiting_question",
                    "waiting_idle",
                    "waiting_other",
                    "failed",
                    "done",
                ],
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub agent: AgentKind,
    pub title: String,
    pub cwd: String,
    pub repo: String,
    pub branch: Option<String>,
    pub status: SessionStatus,
    pub done_inferred: bool,
    /// "在等什么"摘要（PermissionRequest 的 tool_summary）；不等待时 None
    pub waiting_reason: Option<String>,
    /// 真实完成摘要（Stop 的 reply_head）；推断完成保持 None（推不出不硬标）
    pub reply_head: Option<String>,
    pub last_activity: u64, // epoch ms
    pub time_created: u64,
}

/// 采集器输出 / 状态机输入的唯一事件类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnifiedEvent {
    Discovered {
        agent: AgentKind,
        session_id: String,
        cwd: String,
        title: Option<String>,
        ts: u64,
    },
    Activity {
        agent: AgentKind,
        session_id: String,
        ts: u64,
    },
    Waiting {
        agent: AgentKind,
        session_id: String,
        kind: WaitingKind,
        /// 在等什么的摘要（Claude PermissionRequest 的 tool_summary）；其余来源 None
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        ts: u64,
    },
    TurnFailed {
        agent: AgentKind,
        session_id: String,
        ts: u64,
    },
    TurnFinished {
        agent: AgentKind,
        session_id: String,
        /// 完成摘要（Claude Stop 的 reply_head）；无摘要来源时 None
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_head: Option<String>,
        ts: u64,
    },
    Closed {
        agent: AgentKind,
        session_id: String,
        ts: u64,
    },
    Tick {
        now: u64,
    },
}

pub fn repo_name(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(cwd)
        .to_string()
}

/// 读 cwd/.git/HEAD 取分支；.git 是文件（worktree）时返回 None（v1 不解析 worktree 指针）。
pub fn branch_from_git(cwd: &str) -> Option<String> {
    let head = std::path::Path::new(cwd).join(".git").join("HEAD");
    let text = std::fs::read_to_string(head).ok()?;
    let line = text.lines().next()?;
    line.strip_prefix("ref: refs/heads/")
        .map(|s| s.trim().to_string())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // 锁死 AgentKind 的 serde 字符串：必须与前端 types.ts 的
    // "claude_code" | "zcode" 完全一致（教训：snake_case 曾把 ZCode 拆成 "z_code"）。
    #[test]
    fn agent_kind_serializes_to_frontend_strings() {
        assert_eq!(
            serde_json::to_string(&AgentKind::ZCode).unwrap(),
            r#""zcode""#
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::ClaudeCode).unwrap(),
            r#""claude_code""#
        );
    }

    #[test]
    fn agent_kind_serde_roundtrip() {
        for kind in [AgentKind::ClaudeCode, AgentKind::ZCode] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: AgentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn event_serde_roundtrip() {
        let e = UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "abc".into(),
            kind: WaitingKind::Permission,
            reason: None,
            ts: 1_732_000_000_000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"waiting""#));
        let back: UnifiedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    // v0.1.3：Waiting.reason / TurnFinished.reply_head——Some 序列化、None 省略、旧 JSON 兼容
    #[test]
    fn waiting_reason_and_reply_head_serde() {
        let w = UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s".into(),
            kind: WaitingKind::Permission,
            reason: Some("git push".into()),
            ts: 5,
        };
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains(r#""reason":"git push""#));
        let back: UnifiedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);

        // None → 字段省略，且旧 JSON（无 reason）可反序列化
        let w2 = UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "s".into(),
            kind: WaitingKind::Permission,
            reason: None,
            ts: 5,
        };
        let json2 = serde_json::to_string(&w2).unwrap();
        assert!(!json2.contains("reason"));
        let back2: UnifiedEvent = serde_json::from_str(&json2).unwrap();
        assert_eq!(w2, back2);
        let legacy = r#"{"type":"waiting","agent":"claude_code","session_id":"s","kind":"permission","ts":5}"#;
        let back3: UnifiedEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(back3, w2);

        let t = UnifiedEvent::TurnFinished {
            agent: AgentKind::ClaudeCode,
            session_id: "s".into(),
            reply_head: Some("完成摘要".into()),
            ts: 6,
        };
        let json4 = serde_json::to_string(&t).unwrap();
        assert!(json4.contains(r#""reply_head":"完成摘要""#));
        let back4: UnifiedEvent = serde_json::from_str(&json4).unwrap();
        assert_eq!(t, back4);
        let t2 = UnifiedEvent::TurnFinished {
            agent: AgentKind::ClaudeCode,
            session_id: "s".into(),
            reply_head: None,
            ts: 6,
        };
        let json5 = serde_json::to_string(&t2).unwrap();
        assert!(!json5.contains("reply_head"));
        let back5: UnifiedEvent = serde_json::from_str(&json5).unwrap();
        assert_eq!(t2, back5);
    }

    // SessionSnapshot 新字段 camelCase：waitingReason / replyHead
    #[test]
    fn snapshot_serializes_camel_case_new_fields() {
        let snap = SessionSnapshot {
            session_id: "s".into(),
            agent: AgentKind::ClaudeCode,
            title: "t".into(),
            cwd: r"C:\repo".into(),
            repo: "repo".into(),
            branch: None,
            status: SessionStatus::Waiting(WaitingKind::Permission),
            done_inferred: false,
            waiting_reason: Some("git push".into()),
            reply_head: None,
            last_activity: 1,
            time_created: 1,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains(r#""waitingReason":"git push""#));
        assert!(json.contains(r#""replyHead":null"#));
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.waiting_reason.as_deref(), Some("git push"));
        assert_eq!(back.reply_head, None);
    }

    #[test]
    fn status_waiting_serializes_kind() {
        let s = SessionStatus::Waiting(WaitingKind::Idle);
        assert_eq!(serde_json::to_string(&s).unwrap(), r#""waiting_idle""#);
    }

    #[test]
    fn repo_name_takes_last_segment() {
        assert_eq!(
            repo_name(r"C:\Apps\lr\workspace_ai\agentboard"),
            "agentboard"
        );
    }

    #[test]
    fn branch_reads_git_head() {
        let dir = std::env::temp_dir().join("ab-test-branch");
        let git = dir.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            branch_from_git(dir.to_str().unwrap()),
            Some("main".to_string())
        );
    }
}

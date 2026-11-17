use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    ClaudeCode,
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

    #[test]
    fn event_serde_roundtrip() {
        let e = UnifiedEvent::Waiting {
            agent: AgentKind::ClaudeCode,
            session_id: "abc".into(),
            kind: WaitingKind::Permission,
            ts: 1_732_000_000_000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"waiting""#));
        let back: UnifiedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
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

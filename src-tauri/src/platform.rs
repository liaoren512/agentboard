use crate::core::types::*;

pub struct JumpPayload {
    pub spawn: Option<(String, Vec<String>)>,
    pub clipboard: String,
    pub note: &'static str,
}

pub fn jump_payload(s: &SessionSnapshot) -> JumpPayload {
    match s.agent {
        AgentKind::ClaudeCode => JumpPayload {
            spawn: Some(("wt".into(), vec!["-d".into(), s.cwd.clone()])),
            clipboard: format!("claude --resume {}", s.session_id),
            note: "已打开终端窗口，续接命令已复制到剪贴板",
        },
        AgentKind::ZCode => JumpPayload {
            spawn: Some(("explorer".into(), vec![s.cwd.clone()])),
            clipboard: s.cwd.clone(),
            note: "已打开会话目录，路径已复制（ZCode 无 CLI 续接命令）",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(agent: AgentKind, id: &str, cwd: &str) -> SessionSnapshot {
        SessionSnapshot {
            session_id: id.into(),
            agent,
            title: "t".into(),
            cwd: cwd.into(),
            repo: "r".into(),
            branch: None,
            status: SessionStatus::Done,
            done_inferred: false,
            last_activity: 0,
            time_created: 0,
        }
    }

    #[test]
    fn claude_jump_wt_and_resume() {
        let p = jump_payload(&snap(AgentKind::ClaudeCode, "s1", r"C:\repo"));
        let (prog, args) = p.spawn.unwrap();
        assert_eq!(prog, "wt");
        assert_eq!(args, vec!["-d".to_string(), r"C:\repo".to_string()]);
        assert_eq!(p.clipboard, "claude --resume s1");
    }

    #[test]
    fn zcode_jump_explorer_and_clipboard_dir() {
        let p = jump_payload(&snap(AgentKind::ZCode, "z1", r"C:\repo\z"));
        let (prog, _args) = p.spawn.unwrap();
        assert_eq!(prog, "explorer");
        assert_eq!(p.clipboard, r"C:\repo\z");
    }
}

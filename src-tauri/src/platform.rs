use crate::core::types::*;
use std::path::{Path, PathBuf};

pub struct JumpPayload {
    pub spawn: Option<(String, Vec<String>)>,
    pub clipboard: String,
    pub note: &'static str,
}

/// wt 是 App Execution Alias（零字节 reparse 点），PATH 上解析不到（`where wt` 找不到），
/// `Command::new("wt")` 会失败；全路径 %LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe 可执行。
fn wt_candidates() -> Vec<PathBuf> {
    match std::env::var("LOCALAPPDATA") {
        Ok(local) => vec![
            PathBuf::from(local)
                .join("Microsoft")
                .join("WindowsApps")
                .join("wt.exe"),
        ],
        Err(_) => Vec::new(),
    }
}

/// 纯注入式：返回候选里第一个存在的文件（测试注入临时文件列表，不依赖真机环境）。
fn resolve_wt(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

/// wt 路径由调用方注入（None = 解析失败 → 回退 explorer），保持纯函数可测。
pub fn jump_payload_with(s: &SessionSnapshot, wt: Option<&Path>) -> JumpPayload {
    match s.agent {
        AgentKind::ClaudeCode => match wt {
            Some(wt) => JumpPayload {
                spawn: Some((
                    wt.to_string_lossy().into_owned(),
                    vec!["-d".into(), s.cwd.clone()],
                )),
                clipboard: format!("claude --resume {}", s.session_id),
                note: "已打开终端窗口，续接命令已复制到剪贴板",
            },
            // 回退：没找到 wt 全路径就用资源管理器打开会话目录，保证"点了有反应"
            None => JumpPayload {
                spawn: Some(("explorer".into(), vec![s.cwd.clone()])),
                clipboard: format!("claude --resume {}", s.session_id),
                note: "未找到 Windows Terminal，已打开会话目录，续接命令已复制到剪贴板",
            },
        },
        AgentKind::ZCode => JumpPayload {
            spawn: Some(("explorer".into(), vec![s.cwd.clone()])),
            clipboard: s.cwd.clone(),
            note: "已打开会话目录，路径已复制（ZCode 无 CLI 续接命令）",
        },
    }
}

pub fn jump_payload(s: &SessionSnapshot) -> JumpPayload {
    let wt = resolve_wt(&wt_candidates());
    jump_payload_with(s, wt.as_deref())
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
            waiting_reason: None,
            reply_head: None,
            last_activity: 0,
            time_created: 0,
        }
    }

    /// 回归（验收缺陷：点 Claude 会话行无反应）：wt 全路径存在时 spawn 用全路径，
    /// 不再依赖 PATH 解析 App Execution Alias（Command::new("wt") 解析不到）。
    #[test]
    fn claude_jump_uses_full_wt_path() {
        let wt = Path::new(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe");
        let p = jump_payload_with(&snap(AgentKind::ClaudeCode, "s1", r"C:\repo"), Some(wt));
        let (prog, args) = p.spawn.unwrap();
        assert_eq!(prog, wt.to_string_lossy());
        assert_eq!(args, vec!["-d".to_string(), r"C:\repo".to_string()]);
        assert_eq!(p.clipboard, "claude --resume s1");
    }

    /// 回退：wt 不可用（None）→ explorer 打开 cwd，行点击不再无反应。
    #[test]
    fn claude_jump_falls_back_to_explorer_without_wt() {
        let p = jump_payload_with(&snap(AgentKind::ClaudeCode, "s1", r"C:\repo"), None);
        let (prog, args) = p.spawn.unwrap();
        assert_eq!(prog, "explorer");
        assert_eq!(args, vec![r"C:\repo".to_string()]);
    }

    /// resolve_wt 只挑真实存在的候选（模拟：一个不存在的候选 + 一个存在的）。
    #[test]
    fn resolve_wt_picks_existing_candidate() {
        let dir = std::env::temp_dir().join(format!("ab-wt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("wt.exe");
        std::fs::write(&existing, b"stub").unwrap();
        let missing = dir.join("nope.exe");
        assert_eq!(resolve_wt(&[missing, existing.clone()]), Some(existing));
        assert_eq!(resolve_wt(&[]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zcode_jump_explorer_and_clipboard_dir() {
        let p = jump_payload(&snap(AgentKind::ZCode, "z1", r"C:\repo\z"));
        let (prog, _args) = p.spawn.unwrap();
        assert_eq!(prog, "explorer");
        assert_eq!(p.clipboard, r"C:\repo\z");
    }
}

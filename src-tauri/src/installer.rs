use serde_json::Value;
use std::path::{Path, PathBuf};

const MARKER: &str = "agentboard-hook.exe";
const MATCHER: &str = "permission_prompt|idle_prompt|agent_needs_input";

pub struct InstallReport {
    pub backup_path: PathBuf,
    pub changed: bool,
}

pub fn install_hooks(
    hook_exe: &Path,
    settings_path: &Path,
    backup_root: &Path,
) -> std::io::Result<InstallReport> {
    let exe_str = hook_exe.to_string_lossy().replace('/', "\\");
    let mut root: Value = std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    if group_present(&root) {
        return Ok(InstallReport {
            backup_path: PathBuf::new(),
            changed: false,
        });
    }

    // 1) 备份（仅在确要修改前）
    let ts = crate::core::types::now_ms();
    let backup_dir = backup_root.join(format!("{ts}"));
    if settings_path.exists() {
        std::fs::create_dir_all(&backup_dir)?;
        std::fs::copy(settings_path, backup_dir.join("settings.json"))?;
    }

    // 2) 合并
    let hooks = root
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let notification = hooks
        .as_object_mut()
        .unwrap()
        .entry("Notification")
        .or_insert_with(|| serde_json::json!([]));
    notification
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "matcher": MATCHER,
            "hooks": [{ "type": "command", "command": exe_str, "args": ["notification"] }]
        }));

    // 3) 写回（pretty，UTF-8）
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root).unwrap())?;
    prune_backups(backup_root, 10);
    Ok(InstallReport {
        backup_path: backup_dir.join("settings.json"),
        changed: true,
    })
}

pub fn uninstall_hooks(settings_path: &Path) -> std::io::Result<usize> {
    let text = std::fs::read_to_string(settings_path)?;
    let mut root: Value = serde_json::from_str(&text)?;
    let mut removed = 0;
    if let Some(groups) = root["hooks"]["Notification"].as_array_mut() {
        let before = groups.len();
        groups.retain(|g| !is_ours(g));
        removed = before - groups.len();
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(removed)
}

/// Notification 组里是否存在我们的 hook（供 Task 10 get_source_status 用）
pub fn ours_registered(settings_path: &Path) -> bool {
    std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|root| group_present(&root))
        .unwrap_or(false)
}

// exe 路径变化视为同一组件；更新路径时先 uninstall 再 install，故只按标记识别组
fn group_present(root: &Value) -> bool {
    root["hooks"]["Notification"]
        .as_array()
        .map(|groups| groups.iter().any(|g| is_ours(g)))
        .unwrap_or(false)
}

fn is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .map(|hs| {
            hs.iter().any(|h| {
                h["command"]
                    .as_str()
                    .map(|c| c.replace('/', "\\").contains(MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn prune_backups(backup_root: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(backup_root) else {
        return;
    };
    let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    while dirs.len() > keep {
        let _ = std::fs::remove_dir_all(dirs.remove(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("ab-inst-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn exe_and_settings(base: &Path) -> (PathBuf, PathBuf) {
        let exe = base.join("agentboard-hook.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let settings = base.join("settings.json");
        std::fs::write(&settings, r#"{"mcp":{"a":1},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"other.exe"}]}]}}"#).unwrap();
        (exe, settings)
    }

    #[test]
    fn install_adds_group_backs_up_and_preserves_existing() {
        let t = tmp("adds_group");
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        let report = install_hooks(&exe, &settings, &backup_root).unwrap();
        assert!(report.changed);
        assert!(report.backup_path.exists(), "备份文件存在");

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        // 既有内容原样保留
        assert_eq!(v["mcp"]["a"], 1);
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        // 我们的组已加入且 matcher 正确
        let groups = v["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]["matcher"],
            "permission_prompt|idle_prompt|agent_needs_input"
        );
        assert_eq!(groups[0]["hooks"][0]["type"], "command");
        assert!(groups[0]["hooks"][0]["args"][0] == "notification");
        assert!(
            groups[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("agentboard-hook.exe")
        );
        // hook exe 被复制到备份根同级的 bin 目录约定位置由调用方定；此处只验证注册串里是传入的 exe 路径
    }

    #[test]
    fn install_is_idempotent() {
        let t = tmp("idempotent");
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        install_hooks(&exe, &settings, &backup_root).unwrap();
        let report = install_hooks(&exe, &settings, &backup_root).unwrap();
        assert!(!report.changed, "重复安装无变更");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["hooks"]["Notification"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let t = tmp("uninstall");
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        install_hooks(&exe, &settings, &backup_root).unwrap();
        let removed = uninstall_hooks(&settings).unwrap();
        assert_eq!(removed, 1);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(
            v["hooks"]["Notification"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true)
        );
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["matcher"], "Bash",
            "别人的 hook 不能动"
        );
    }

    #[test]
    fn install_on_missing_file_creates_minimal() {
        let t = tmp("missing_file");
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::remove_file(&settings).unwrap();
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(report.changed);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(v["hooks"]["Notification"].as_array().unwrap().len() == 1);
    }
}

use serde_json::Value;
use std::path::{Path, PathBuf};

const MARKER: &str = "agentboard-hook.exe";
const MATCHER: &str = "permission_prompt|idle_prompt|agent_needs_input";

#[derive(Debug)]
pub struct InstallReport {
    pub backup_path: PathBuf,
    pub changed: bool,
}

/// v0.1.3 注册的三组事件。Notification 带 matcher（沿用既有）；PermissionRequest /
/// Stop 不带 matcher——官方文档：PermissionRequest 省略 matcher = 所有工具触发，
/// Stop 完全不支持 matcher（写了也会被忽略）。
const HOOK_EVENTS: [&str; 3] = ["Notification", "PermissionRequest", "Stop"];

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

    if !root.is_object() {
        // 顶层不是对象（如数组/字符串）没法安全合并：报错不 panic，原文件不动
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json 顶层不是 JSON 对象",
        ));
    }
    if HOOK_EVENTS.iter().all(|event| group_present(&root, event)) {
        return Ok(InstallReport {
            backup_path: PathBuf::new(),
            changed: false,
        });
    }

    // 1) 备份（仅在确要修改前）
    let ts = crate::core::types::now_ms();
    let backup_dir = backup_root.join(format!("{ts}"));
    let mut backup_path = PathBuf::new();
    if settings_path.exists() {
        std::fs::create_dir_all(&backup_dir)?;
        std::fs::copy(settings_path, backup_dir.join("settings.json"))?;
        backup_path = backup_dir.join("settings.json");
    }

    // 2) 合并（hooks / 各事件数组类型不对同样报错不 panic；此时已备份，原文件仍未动）
    let Some(obj) = root.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json 顶层不是 JSON 对象",
        ));
    };
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json 的 hooks 字段不是 JSON 对象",
        ));
    };
    // 逐事件 ensure（旧版本单组文件 → 只补缺的组，不重复）
    let mut added = false;
    for event in HOOK_EVENTS {
        let group = if event == "Notification" {
            serde_json::json!({
                "matcher": MATCHER,
                "hooks": [{ "type": "command", "command": exe_str, "args": ["notification"] }]
            })
        } else {
            serde_json::json!({
                "hooks": [{ "type": "command", "command": exe_str }]
            })
        };
        added |= ensure_group(hooks_obj, event, group)?;
    }
    if !added {
        // 前置检查已挡全存在的情况，此处防御：类型异常等导致的零改动按无变更返回
        return Ok(InstallReport {
            backup_path: PathBuf::new(),
            changed: false,
        });
    }

    // 3) 写回（pretty，UTF-8）
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root).unwrap())?;
    prune_backups(backup_root, 10);
    Ok(InstallReport {
        backup_path,
        changed: true,
    })
}

/// 向某事件数组 ensure 一组我们的 hook：已存在（按标记识别）→ 不动；缺 → push。
/// 事件已有值但不是数组 → InvalidData。
fn ensure_group(
    hooks_obj: &mut serde_json::Map<String, Value>,
    event: &str,
    group: Value,
) -> std::io::Result<bool> {
    let arr = hooks_obj
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]));
    let Some(groups) = arr.as_array_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("settings.json 的 hooks.{event} 不是 JSON 数组"),
        ));
    };
    if groups.iter().any(|g| is_ours(g)) {
        return Ok(false);
    }
    groups.push(group);
    Ok(true)
}

pub fn uninstall_hooks(settings_path: &Path) -> std::io::Result<usize> {
    let text = std::fs::read_to_string(settings_path)?;
    let mut root: Value = serde_json::from_str(&text)?;
    let mut removed = 0;
    for event in HOOK_EVENTS {
        if let Some(groups) = root["hooks"][event].as_array_mut() {
            let before = groups.len();
            groups.retain(|g| !is_ours(g));
            removed += before - groups.len();
        }
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&root)?)?;
    Ok(removed)
}

/// Notification 组里是否存在我们的 hook（供 get_source_status 用）
pub fn ours_registered(settings_path: &Path) -> bool {
    std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|root| group_present(&root, "Notification"))
        .unwrap_or(false)
}

// exe 路径变化视为同一组件；更新路径时先 uninstall 再 install，故只按标记识别组
fn group_present(root: &Value, event: &str) -> bool {
    root["hooks"][event]
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
        // v0.1.3：PermissionRequest / Stop 各一组；官方文档——两组均不带 matcher
        //（PermissionRequest 省略 matcher = 所有工具触发；Stop 完全不支持 matcher）
        let pr = &v["hooks"]["PermissionRequest"][0];
        assert!(
            pr.get("matcher").is_none(),
            "PermissionRequest 组无 matcher 字段"
        );
        assert_eq!(pr["hooks"][0]["type"], "command");
        assert!(
            pr["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("agentboard-hook.exe")
        );
        let stop = &v["hooks"]["Stop"][0];
        assert!(stop.get("matcher").is_none(), "Stop 组无 matcher 字段");
        assert!(
            stop["hooks"][0]["command"]
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
        for event in ["Notification", "PermissionRequest", "Stop"] {
            assert_eq!(
                v["hooks"][event].as_array().unwrap().len(),
                1,
                "{event} 组不重复"
            );
        }
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let t = tmp("uninstall");
        let (exe, settings) = exe_and_settings(&t.0);
        let backup_root = t.0.join("backups");
        install_hooks(&exe, &settings, &backup_root).unwrap();
        let removed = uninstall_hooks(&settings).unwrap();
        assert_eq!(removed, 3, "三组各删一个");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        for event in ["Notification", "PermissionRequest", "Stop"] {
            assert!(
                v["hooks"][event]
                    .as_array()
                    .map(|a| a.is_empty())
                    .unwrap_or(true)
            );
        }
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["matcher"], "Bash",
            "别人的 hook 不能动"
        );
    }

    // 旧版本（v0.1.2 及以前）settings 只有 Notification 单组：升级补装 PermissionRequest/Stop，
    // Notification 不重复。
    #[test]
    fn upgrades_legacy_single_group_file() {
        let t = tmp("upgrade");
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::write(
            &settings,
            r#"{"hooks":{"Notification":[{"matcher":"permission_prompt|idle_prompt|agent_needs_input","hooks":[{"type":"command","command":"C:\\old\\agentboard-hook.exe","args":["notification"]}]}]}}"#,
        )
        .unwrap();
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(report.changed);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["Notification"].as_array().unwrap().len(),
            1,
            "既有 Notification 组不重复补装"
        );
        assert_eq!(v["hooks"]["PermissionRequest"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        // 再装一次收敛为幂等
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(!report.changed);
    }

    #[test]
    fn install_on_missing_file_creates_minimal() {
        let t = tmp("missing_file");
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::remove_file(&settings).unwrap();
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(report.changed);
        assert!(
            report.backup_path.as_os_str().is_empty(),
            "缺文件新装无备份，backup_path 应为空"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        for event in ["Notification", "PermissionRequest", "Stop"] {
            assert_eq!(v["hooks"][event].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn install_rejects_non_object_settings_and_leaves_file_untouched() {
        let t = tmp("non_object");
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::write(&settings, "[1,2,3]").unwrap();
        let before = std::fs::read_to_string(&settings).unwrap();
        let err = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            before,
            "报错路径原文件不动"
        );
    }
}

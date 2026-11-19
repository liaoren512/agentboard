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

/// 我们的三组定义（与 HOOK_EVENTS 一一对应），幂等检查与合并共用同一构造，
/// 避免两处形状漂移。Notification 带 matcher；其余两组不带（见 HOOK_EVENTS 注）。
fn our_groups(cmd: &str) -> Vec<serde_json::Value> {
    HOOK_EVENTS
        .iter()
        .map(|event| {
            if *event == "Notification" {
                serde_json::json!({
                    "matcher": MATCHER,
                    "hooks": [{ "type": "command", "command": cmd, "args": ["notification"] }]
                })
            } else {
                serde_json::json!({
                    "hooks": [{ "type": "command", "command": cmd }]
                })
            }
        })
        .collect()
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

    if !root.is_object() {
        // 顶层不是对象（如数组/字符串）没法安全合并：报错不 panic，原文件不动
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "settings.json 顶层不是 JSON 对象",
        ));
    }
    // 组级幂等：每组按"对应事件键 + command 含标记"逐个判，三组全在才无变更
    //（v0.1.0 旧单组文件缺 PermissionRequest/Stop → 照常往下补装）
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
    // 组级补装：缺哪组 push 哪组，已有组（含旧版本 Notification 组）原样保留不重建
    let mut added = false;
    for (event, group) in HOOK_EVENTS.iter().cloned().zip(our_groups(&exe_str)) {
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
            format!("hooks.{event} 不是 JSON 数组"),
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

// ---------- ZCode hooks 注册器（~/.zcode/cli/config.json，v0.1.3） ----------
// 结构照 Pitwall install.sh install_zcode()（267-275 行）：nested events 形状
// hooks.enabled + hooks.events.<Event> = [{hooks:[{type:"process",command,args,timeoutMs}]}]。

/// v0.1.3 注册的四个 ZCode 事件（与 zcode_hook::ZCODE_EVENTS 对齐）。
const ZCODE_EVENTS: [&str; 4] = [
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
];

#[derive(Debug)]
pub struct ZcodeInstallReport {
    pub backup_path: PathBuf,
    pub changed: bool,
}

/// 安装 ZCode hooks：预检（第三方 disabled/malformed → 拒装）→ 备份 → 记录
/// hooks.enabled 原状（state 文件）→ 设 enabled=true + 四事件各 ensure 一组
/// process hook（args 带 "zcode <事件名>"，timeoutMs 65000）→ 幂等写回。
pub fn install_zcode_hooks(
    hook_exe: &Path,
    zcode_config_path: &Path,
    backup_root: &Path,
    state_path: &Path,
) -> std::io::Result<ZcodeInstallReport> {
    let exe_str = hook_exe.to_string_lossy().replace('/', "\\");
    let mut root: Value = match std::fs::read_to_string(zcode_config_path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("config.json 不是合法 JSON：{e}"),
            )
        })?,
        Err(_) => serde_json::json!({}),
    };
    if !root.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "config.json 顶层不是 JSON 对象",
        ));
    }
    // 预检（照 pitwall install.sh 249-255 行语义）：打开我们的集成开关，不得顺带激活
    // 别人关闭的 hooks——hooks 非 null 且非对象（malformed），或 enabled != true 且
    // events 里存在第三方 command → 拒装，配置原样不动。
    if let Some(hooks) = root.get("hooks").filter(|h| !h.is_null()) {
        if hooks.as_object().is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "config.json 的 hooks 字段不是 JSON 对象（malformed）；请先在 ZCode 中审核，未修改配置",
            ));
        }
        let enabled = hooks.get("enabled").and_then(Value::as_bool) == Some(true);
        if !enabled && third_party_command_count(hooks) > 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "存在已关闭的其它 hooks（hooks.enabled != true）；请先在 ZCode 中审核，未修改配置",
            ));
        }
    }
    // 幂等：四事件均已登记且 enabled=true → 无变更（不备份、不写 state）
    if ZCODE_EVENTS.iter().all(|e| zcode_group_present(&root, e))
        && root["hooks"]["enabled"].as_bool() == Some(true)
    {
        return Ok(ZcodeInstallReport {
            backup_path: PathBuf::new(),
            changed: false,
        });
    }

    // 1) 备份（仅在确要修改前）
    let ts = crate::core::types::now_ms();
    let backup_dir = backup_root.join(format!("{ts}"));
    let mut backup_path = PathBuf::new();
    if zcode_config_path.exists() {
        std::fs::create_dir_all(&backup_dir)?;
        std::fs::copy(zcode_config_path, backup_dir.join("config.json"))?;
        backup_path = backup_dir.join("config.json");
    }

    // 2) 记录 hooks.enabled 原状（仅在无 state 且 enabled != true 时写：原本就是 true
    //    说明开关不是我们打开的，卸载时无需恢复；已有 state 不覆盖，保留最初原状）
    let mut wrote_state = false;
    if !state_path.exists() {
        let hooks = root
            .get("hooks")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if hooks.get("enabled").and_then(Value::as_bool) != Some(true) {
            let hooks_obj = hooks.as_object();
            let state = serde_json::json!({
                "had_enabled": hooks_obj.map(|o| o.contains_key("enabled")).unwrap_or(false),
                "enabled": hooks.get("enabled").cloned().unwrap_or(Value::Null),
                "had_events": hooks_obj.map(|o| o.contains_key("events")).unwrap_or(false),
            });
            if let Some(parent) = state_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(state_path, serde_json::to_string_pretty(&state).unwrap())?;
            wrote_state = true;
        }
    }

    // 3) 合并：hooks.events 四事件各 ensure 一组（其他事件/其他 hook 原样保留），
    //    再把 hooks.enabled 置 true（照 pitwall：.hooks.enabled = true | .hooks.events = $events）
    let merge = (|| -> std::io::Result<()> {
        let obj = root.as_object_mut().expect("已检查顶层是对象");
        let hooks_val = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
        let Some(hooks_obj) = hooks_val.as_object_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "config.json 的 hooks 字段不是 JSON 对象",
            ));
        };
        let events_val = hooks_obj
            .entry("events")
            .or_insert_with(|| serde_json::json!({}));
        if events_val.is_null() {
            *events_val = serde_json::json!({}); // events: null 照 pitwall 的 `// {}` 容错
        }
        let Some(events_obj) = events_val.as_object_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "config.json 的 hooks.events 不是 JSON 对象",
            ));
        };
        for event in ZCODE_EVENTS {
            let group = serde_json::json!({
                "hooks": [{ "type": "process", "command": exe_str, "args": ["zcode", event], "timeoutMs": 65000 }]
            });
            ensure_group(events_obj, event, group)?;
        }
        hooks_obj.insert("enabled".into(), Value::Bool(true));
        Ok(())
    })();
    if let Err(e) = merge {
        // 合并失败：本次新建的 state 回滚删除，原配置文件自始至终未动
        if wrote_state {
            let _ = std::fs::remove_file(state_path);
        }
        return Err(e);
    }

    // 4) 写回（pretty，UTF-8）
    if let Some(parent) = zcode_config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        zcode_config_path,
        serde_json::to_string_pretty(&root).unwrap(),
    )?;
    prune_backups(backup_root, 10);
    Ok(ZcodeInstallReport {
        backup_path,
        changed: true,
    })
}

/// hooks.events 子树里非我们的 command 条目数（预检第三方检测，照 pitwall 的
/// `[.hooks.events // {} | .. | objects | select(has("command")) | .command | select(contains(pitwall-hook) | not)]`）。
fn third_party_command_count(v: &Value) -> usize {
    match v {
        Value::Object(m) => {
            let third = usize::from(
                m.get("command")
                    .and_then(Value::as_str)
                    .map(|c| !c.replace('/', "\\").contains(MARKER))
                    .unwrap_or(false),
            );
            third + m.values().map(third_party_command_count).sum::<usize>()
        }
        Value::Array(a) => a.iter().map(third_party_command_count).sum(),
        _ => 0,
    }
}

/// 子树里带 command 的对象总数（卸载恢复 enabled 的前置条件：已无任何 command 条目）。
fn count_command_objects(v: &Value) -> usize {
    match v {
        Value::Object(m) => {
            usize::from(m.contains_key("command"))
                + m.values().map(count_command_objects).sum::<usize>()
        }
        Value::Array(a) => a.iter().map(count_command_objects).sum(),
        _ => 0,
    }
}

/// 卸载 ZCode hooks：只删含我们命令的组，组空的事件键一并删除；按 state 恢复
/// enabled（照 pitwall uninstall_zcode 282-292 行：仅当 state 存在且 events 里已无
/// 任何 command 条目——"只在开关由我们打开且之后没有别人加入 hooks" 时恢复）。
/// 全空连 hooks 键删除；内容没变不写盘；state 文件成功后清理。
pub fn uninstall_zcode_hooks(
    zcode_config_path: &Path,
    state_path: &Path,
) -> std::io::Result<usize> {
    // state：存在且是对象才参与恢复（解析失败照 pitwall 按 null 处理）
    let state: Value = std::fs::read_to_string(state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Null);
    let Ok(text) = std::fs::read_to_string(zcode_config_path) else {
        let _ = std::fs::remove_file(state_path);
        return Ok(0);
    };
    let original: Value = serde_json::from_str(&text)?;
    let mut root = original.clone();
    let mut removed = 0usize;
    // 只删含我们命令的组；组空的事件键删除（照 pitwall strip 的 with_entries(select(length>0))）。
    // 用 get_mut 链而非 Value 索引：索引的 or_insert 语义会往缺键配置里塞 Null。
    if let Some(events_obj) = root
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .and_then(|h| h.get_mut("events"))
        .and_then(Value::as_object_mut)
    {
        for event in ZCODE_EVENTS {
            if let Some(groups) = events_obj.get_mut(event).and_then(Value::as_array_mut) {
                let before = groups.len();
                groups.retain(|g| !is_ours(g));
                removed += before - groups.len();
            }
        }
        let empty_keys: Vec<String> = events_obj
            .iter()
            .filter(|(_, v)| v.as_array().map(|a| a.is_empty()).unwrap_or(false))
            .map(|(k, _)| k.clone())
            .collect();
        for k in empty_keys {
            events_obj.remove(&k);
        }
    }
    // enabled 恢复：仅当 state 存在、hooks 是对象且 events 里已无任何 command 条目
    if state.is_object()
        && root["hooks"].is_object()
        && count_command_objects(&root["hooks"]["events"]) == 0
    {
        if let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            let events_empty = hooks_obj
                .get("events")
                .and_then(Value::as_object)
                .map(|o| o.is_empty())
                .unwrap_or(true);
            // events 键：state.had_events 或剩余事件非空 → 保留；否则删除
            if !(state["had_events"].as_bool().unwrap_or(false) || !events_empty) {
                hooks_obj.remove("events");
            }
            if state["had_enabled"].as_bool().unwrap_or(false) {
                hooks_obj.insert("enabled".into(), state["enabled"].clone());
            } else {
                hooks_obj.remove("enabled");
            }
            if hooks_obj.is_empty() {
                if let Some(top) = root.as_object_mut() {
                    top.remove("hooks");
                }
            }
        }
    }
    // 内容没变不写盘（照 pitwall edit_json 语义）；state 成功后清理
    if root != original {
        std::fs::write(zcode_config_path, serde_json::to_string_pretty(&root)?)?;
    }
    let _ = std::fs::remove_file(state_path);
    Ok(removed)
}

/// 健康检查：四个事件是否都已登记我们的 hook（供 get_source_status 用）。
pub fn ours_registered_zcode(zcode_config_path: &Path) -> bool {
    std::fs::read_to_string(zcode_config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|root| ZCODE_EVENTS.iter().all(|e| zcode_group_present(&root, e)))
        .unwrap_or(false)
}

/// hooks.events.<event> 里是否有我们的组（nested 形状）。
fn zcode_group_present(root: &Value, event: &str) -> bool {
    root["hooks"]["events"][event]
        .as_array()
        .map(|groups| groups.iter().any(is_ours))
        .unwrap_or(false)
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

    // v0.1.0 旧形态：settings 只有 Notification 一组含我们的 hook（旧 exe 路径），
    // 同数组还有第三方组：升级 → 只补装缺的 PermissionRequest/Stop 两组，
    // 原 Notification 组（含旧路径）与第三方组原样保留，changed=true。
    #[test]
    fn upgrades_legacy_single_group_file() {
        let t = tmp("upgrade");
        let (exe, settings) = exe_and_settings(&t.0);
        std::fs::write(
            &settings,
            r#"{"hooks":{"Notification":[{"matcher":"permission_prompt|idle_prompt|agent_needs_input","hooks":[{"type":"command","command":"C:\\old\\agentboard-hook.exe","args":["notification"]}]},{"matcher":"other","hooks":[{"type":"command","command":"third.exe"}]}]}}"#,
        )
        .unwrap();
        let report = install_hooks(&exe, &settings, &t.0.join("backups")).unwrap();
        assert!(report.changed);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let notif = v["hooks"]["Notification"].as_array().unwrap();
        assert_eq!(notif.len(), 2, "Notification 只保留原有两组，不重复补装");
        // 原我们的组原样保留（旧 exe 路径不被重建为当前路径）
        assert_eq!(
            notif[0]["hooks"][0]["command"], "C:\\old\\agentboard-hook.exe",
            "既有 Notification 组原样保留，不重建"
        );
        assert_eq!(notif[0]["hooks"][0]["args"][0], "notification");
        // 同数组第三方组不动
        assert_eq!(notif[1]["hooks"][0]["command"], "third.exe");
        // 缺的两组已补装（用当前传入的 exe 路径）
        assert_eq!(v["hooks"]["PermissionRequest"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(
            v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("agentboard-hook.exe")
        );
        // 再装一次收敛为幂等（三组全齐 → changed=false）
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

    // ---------- v0.1.3 ZCode hooks 注册器（全部临时目录，不碰真实 ~/.zcode） ----------

    /// 第四个返回值是 zcode-hooks-state 文件路径（放临时目录内）。
    fn zcode_base(base: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let exe = base.join("agentboard-hook.exe");
        std::fs::write(&exe, b"fake").unwrap();
        (
            exe,
            base.join("config.json"),
            base.join("backups"),
            base.join("zcode-hooks-state.json"),
        )
    }

    /// 数 config 里某事件的 ours 组数（按 agentboard-hook.exe 标记识别）。
    fn zcode_group_count(v: &Value, event: &str) -> usize {
        v["hooks"]["events"][event]
            .as_array()
            .map(|a| a.iter().filter(|g| is_ours(g)).count())
            .unwrap_or(0)
    }

    #[test]
    fn zcode_install_adds_nested_groups_enables_and_preserves_existing() {
        let t = tmp("zc-add");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        // 用户已有 hooks（enabled=true 才能通过预检）：另一事件下的第三方组
        std::fs::write(
            &config,
            r#"{"other":1,"hooks":{"enabled":true,"events":{"UserPromptSubmit":[{"hooks":[{"type":"process","command":"other.exe","timeoutMs":10}]}]}}}"#,
        )
        .unwrap();
        let report = install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        assert!(report.changed);
        assert!(report.backup_path.exists(), "改动前已备份 config.json");

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["other"], 1, "既有顶层键原样保留");
        assert_eq!(v["hooks"]["enabled"], true, "hooks.enabled 置 true");
        // 四个事件各一组，形状照 pitwall install.sh 267-275：process + args + timeoutMs
        for event in ZCODE_EVENTS {
            let groups = v["hooks"]["events"][event].as_array().unwrap();
            assert_eq!(groups.len(), 1, "{event} 一组");
            let h = &groups[0]["hooks"][0];
            assert_eq!(h["type"], "process");
            assert!(
                h["command"]
                    .as_str()
                    .unwrap()
                    .ends_with("agentboard-hook.exe")
            );
            assert_eq!(h["args"][0], "zcode");
            assert_eq!(h["args"][1], event, "args 带事件名");
            assert_eq!(h["timeoutMs"], 65000);
        }
        // 第三方组原样保留
        assert_eq!(
            v["hooks"]["events"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            "other.exe"
        );
        // enabled 原本就是 true → 不写 state（卸载时无需恢复开关）
        assert!(!state.exists(), "enabled 原为 true 时无 state 文件");
    }

    #[test]
    fn zcode_install_is_idempotent() {
        let t = tmp("zc-idem");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(&config, r#"{"hooks":{"enabled":true}}"#).unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        let report = install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        assert!(!report.changed, "重复安装无变更");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        for event in ZCODE_EVENTS {
            assert_eq!(zcode_group_count(&v, event), 1, "{event} 组不重复");
        }
    }

    #[test]
    fn zcode_precheck_rejects_disabled_or_malformed_third_party() {
        let t = tmp("zc-reject");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        // enabled 缺失/非 true 且 events 里有第三方 command → 拒装（照 pitwall 249-255 语义）
        for cfg in [
            r#"{"hooks":{"events":{"PreToolUse":[{"hooks":[{"type":"process","command":"other.exe"}]}]}}}"#,
            r#"{"hooks":{"enabled":false,"events":{"PreToolUse":[{"hooks":[{"type":"process","command":"other.exe"}]}]}}}"#,
            r#"{"hooks":"malformed"}"#,
        ] {
            std::fs::write(&config, cfg).unwrap();
            let before = std::fs::read_to_string(&config).unwrap();
            let err = install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                before,
                "拒装时配置原样不动"
            );
            assert!(!state.exists(), "拒装不写 state");
            assert!(!backup_root.exists(), "拒装不产生备份（预检在备份前）");
        }
    }

    #[test]
    fn zcode_precheck_allows_disabled_without_third_party_commands() {
        // enabled=false 但 events 里没有任何 command → 放行（启用不会激活别人的 hook）
        let t = tmp("zc-allow-disabled");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(&config, r#"{"hooks":{"enabled":false}}"#).unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["hooks"]["enabled"], true);
        // enabled 原为 false → 写 state 记录原状（卸载恢复）
        let s: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        assert_eq!(s["had_enabled"], true);
        assert_eq!(s["enabled"], false);
        assert_eq!(s["had_events"], false);
    }

    #[test]
    fn zcode_install_does_not_overwrite_existing_state() {
        let t = tmp("zc-state-keep");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(
            &state,
            r#"{"had_enabled":true,"enabled":false,"had_events":true}"#,
        )
        .unwrap();
        std::fs::write(&config, r#"{"hooks":{"enabled":true}}"#).unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        let s = std::fs::read_to_string(&state).unwrap();
        assert!(s.contains("had_events"), "已有 state 文件不被覆盖");
    }

    #[test]
    fn zcode_uninstall_restores_absent_enabled() {
        // 原本无 enabled（config = {}）：卸载后 enabled/events 键删除，全空连 hooks 键也删
        let t = tmp("zc-restore-absent");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(&config, r#"{}"#).unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        let removed = uninstall_zcode_hooks(&config, &state).unwrap();
        assert_eq!(removed, 4, "四事件各删一组");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert!(
            v.get("hooks").is_none(),
            "hooks 全空 → 整键删除，config 回到原状"
        );
        assert!(!state.exists(), "卸载后清掉 state 文件");
    }

    #[test]
    fn zcode_uninstall_restores_enabled_false() {
        // 原本 enabled=false（无第三方 command）：卸载恢复 false
        let t = tmp("zc-restore-false");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(&config, r#"{"hooks":{"enabled":false}}"#).unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        uninstall_zcode_hooks(&config, &state).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["hooks"]["enabled"], false, "恢复原 enabled=false");
        assert!(
            v["hooks"].get("events").is_none(),
            "events 由我们创建且已空 → 删除"
        );
        assert!(!state.exists());
    }

    #[test]
    fn zcode_uninstall_keeps_enabled_true_and_third_party() {
        // 原本 enabled=true（无 state）：卸载只删我们的组，enabled/events/第三方原样
        let t = tmp("zc-keep-true");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        std::fs::write(
            &config,
            r#"{"hooks":{"enabled":true,"events":{"UserPromptSubmit":[{"hooks":[{"type":"process","command":"other.exe"}]}]}}}"#,
        )
        .unwrap();
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        let removed = uninstall_zcode_hooks(&config, &state).unwrap();
        assert_eq!(removed, 4);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["hooks"]["enabled"], true, "无 state 不动 enabled");
        assert_eq!(
            v["hooks"]["events"]["UserPromptSubmit"][0]["hooks"][0]["command"], "other.exe",
            "第三方组保留"
        );
        for event in ZCODE_EVENTS {
            assert_eq!(zcode_group_count(&v, event), 0, "{event} 我们的组已删");
        }
    }

    #[test]
    fn zcode_uninstall_missing_config_clears_state() {
        let t = tmp("zc-missing");
        let (_exe, config, _backup_root, state) = zcode_base(&t.0);
        std::fs::write(
            &state,
            r#"{"had_enabled":false,"enabled":null,"had_events":false}"#,
        )
        .unwrap();
        let removed = uninstall_zcode_hooks(&config, &state).unwrap();
        assert_eq!(removed, 0, "无配置文件 → 无事可做");
        assert!(!state.exists(), "state 文件清理");
    }

    #[test]
    fn zcode_ours_registered_requires_all_four_events() {
        let t = tmp("zc-registered");
        let (exe, config, backup_root, state) = zcode_base(&t.0);
        assert!(!ours_registered_zcode(&config), "缺配置 → false");
        std::fs::write(&config, r#"{"hooks":{"enabled":true}}"#).unwrap();
        assert!(!ours_registered_zcode(&config), "未注册 → false");
        install_zcode_hooks(&exe, &config, &backup_root, &state).unwrap();
        assert!(ours_registered_zcode(&config), "四事件全注册 → true");
        // 删掉一个事件 → false
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        v["hooks"]["events"]
            .as_object_mut()
            .unwrap()
            .remove("PostToolUseFailure");
        std::fs::write(&config, serde_json::to_string(&v).unwrap()).unwrap();
        assert!(!ours_registered_zcode(&config), "缺任一事件 → false");
    }
}

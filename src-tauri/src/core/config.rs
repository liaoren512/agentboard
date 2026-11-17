use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub notify_enabled: bool,
    pub running_fresh_ms: u64,
    pub done_after_ms: u64,
}
impl Default for Config {
    fn default() -> Self { Self { notify_enabled: true, running_fresh_ms: 120_000, done_after_ms: 900_000 } }
}

pub fn path() -> std::path::PathBuf { crate::agentboard_dir().join("config.json") }

pub fn load() -> Config {
    std::fs::read_to_string(path()).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(p, serde_json::to_string_pretty(cfg).unwrap())
}

pub fn load_from(p: &std::path::Path) -> Config {
    std::fs::read_to_string(p).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn save_to(p: &std::path::Path, cfg: &Config) -> std::io::Result<()> {
    if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
    std::fs::write(p, serde_json::to_string_pretty(cfg).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrip_and_defaults() {
        // 不存在的路径 → 默认值
        let p = std::env::temp_dir().join("ab-test-config").join("missing.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(load_from(&p), Config::default());

        // save → load roundtrip，camelCase 字段
        let cfg = Config { notify_enabled: false, running_fresh_ms: 60_000, done_after_ms: 1_800_000 };
        save_to(&p, &cfg).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"notifyEnabled\""));
        assert_eq!(load_from(&p), cfg);
        let _ = std::fs::remove_file(&p);
    }
}

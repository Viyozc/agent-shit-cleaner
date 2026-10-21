//! 用户配置：`~/.config/asc/config.toml` + `~/.config/asc/rules.d/*.toml`
//!
//! 原则：配置缺失或损坏不致命——回退默认值，调用方负责把警告展示给用户；
//! 一切删除决策仍以人工确认为准，配置只影响"扫什么 / 默认勾什么"。

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `[[workspace]]` 一个工作区根
#[derive(Clone, Debug, Default, Deserialize)]
pub struct WorkspaceRoot {
    /// 工作区根目录（支持 `~`）
    pub root: String,
    /// 要跳过的子目录名（如 vendored 的依赖树）
    #[serde(default)]
    pub ignore: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// 工作区根列表（TUI/`asc clean` 无路径参数时使用）
    #[serde(default)]
    pub workspace: Vec<WorkspaceRoot>,
    /// 按规则 id 覆盖保留天数，如 `"claude-code/sessions" = 30`
    #[serde(default)]
    pub keep_days: BTreeMap<String, u64>,
    /// Tier B 规则（dist/build/out 等歧义目录名）总开关，默认关
    #[serde(default)]
    pub tier_b: bool,
    /// 活跃判定窗口（天）：项目源码 mtime 距今小于该值 → 项目活跃
    #[serde(default = "d_active_days")]
    pub active_days: u64,
    /// 回收站保留天数
    #[serde(default = "d_trash_keep_days")]
    pub trash_keep_days: u64,
}

fn d_active_days() -> u64 {
    30
}
fn d_trash_keep_days() -> u64 {
    7
}

impl Default for Config {
    fn default() -> Self {
        Self {
            workspace: Vec::new(),
            keep_days: BTreeMap::new(),
            tier_b: false,
            active_days: d_active_days(),
            trash_keep_days: d_trash_keep_days(),
        }
    }
}

/// 加载配置 + 加载过程中产生的警告（不致命）
#[derive(Debug, Default, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
}

impl Config {
    /// 配置文件路径（无 XDG 配置目录的平台上为 None）
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("asc/config.toml"))
    }

    /// 用户规则目录
    pub fn rules_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("asc/rules.d"))
    }

    /// 读配置；文件不存在 → 默认值；存在但解析失败 → 默认值 + 警告
    pub fn load() -> LoadedConfig {
        let mut lc = LoadedConfig::default();
        let Some(path) = Self::config_path() else {
            return lc;
        };
        match std::fs::read_to_string(&path) {
            Err(_) => return lc, // 没写配置是常态
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => lc.config = c,
                Err(e) => {
                    lc.config = Config::default();
                    lc.warnings
                        .push(format!("配置文件解析失败（{}），使用默认值", e));
                }
            },
        }
        lc
    }

    /// 用户规则文件（rules.d 下按文件名排序），返回 (路径, 内容)
    pub fn user_rule_texts() -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let Some(dir) = Self::rules_dir() else {
            return out;
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        let mut paths: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        paths.sort();
        for p in paths {
            if let Ok(text) = std::fs::read_to_string(&p) {
                out.push((p, text));
            }
        }
        out
    }

    /// 工作区根（`~` 已展开），返回 (root, ignore 列表)
    pub fn workspace_roots(&self) -> Vec<(PathBuf, Vec<String>)> {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        self.workspace
            .iter()
            .map(|w| {
                (
                    crate::rule::expand_tilde(&w.root, &home),
                    w.ignore.clone(),
                )
            })
            .filter(|(r, _)| r.is_dir())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_shaped_config() {
        // 注意：标量键必须放在 [keep_days] 表之前（TOML 作用域规则）
        let text = r#"
tier_b = true
active_days = 14
trash_keep_days = 3

[[workspace]]
root = "~/Documents"
ignore = ["vendored"]

[keep_days]
"claude-code/sessions" = 30
"workspace/rust-target" = 180
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.workspace.len(), 1);
        assert_eq!(c.workspace[0].root, "~/Documents");
        assert_eq!(c.workspace[0].ignore, vec!["vendored"]);
        assert_eq!(c.keep_days.get("claude-code/sessions"), Some(&30));
        assert!(c.tier_b);
        assert_eq!(c.active_days, 14);
        assert_eq!(c.trash_keep_days, 3);
    }

    #[test]
    fn defaults_when_empty() {
        let c: Config = toml::from_str("").unwrap();
        assert!(!c.tier_b);
        assert_eq!(c.active_days, 30);
        assert_eq!(c.trash_keep_days, 7);
        assert!(c.workspace.is_empty());
    }

    #[test]
    fn load_missing_file_is_default() {
        // 本测试机没有 ~/.config/asc/config.toml 时不误报；有配置也不失败
        let lc = Config::load();
        assert_eq!(lc.config.active_days, 30);
    }
}

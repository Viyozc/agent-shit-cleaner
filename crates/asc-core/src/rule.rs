use serde::Deserialize;

/// 规则作用域：home = agent 大本营固定路径；workspace = 项目内目录名模式
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Home,
    Workspace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Sessions,
    Logs,
    Caches,
    Telemetry,
    Packages,
    Temp,
    Build,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::Sessions => "sessions",
            Category::Logs => "logs",
            Category::Caches => "caches",
            Category::Telemetry => "telemetry",
            Category::Packages => "packages",
            Category::Temp => "temp",
            Category::Build => "build",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// 删了下次自动再生
    Safe,
    /// 按保留期过期后可删
    Retention,
    /// 可重新下载/重建，清理需显式确认
    Redownload,
}

impl Risk {
    pub fn label(&self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Retention => "retention",
            Risk::Redownload => "redownload",
        }
    }
}

/// 一条清理规则（详见 rules/rules.toml 注释）
#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    pub id: String,
    #[serde(default)]
    pub scope: Scope,
    /// home 规则：所属 agent
    pub agent: Option<String>,
    /// home 规则：目标路径（~ 展开）
    pub path: Option<String>,
    /// home 规则：文件名通配符（仅匹配文件，如 "*.jsonl"）
    pub pattern: Option<String>,
    /// workspace 规则：目录名（支持 {a,b} 多选）
    pub match_dir: Option<String>,
    /// workspace 规则：同级需存在的清单文件（支持 {a,b} 与 *）
    pub require_sibling: Option<String>,
    pub category: Category,
    pub risk: Risk,
    /// retention 规则的保留天数
    pub keep_days: Option<u64>,
    /// 规则分层：tier = "b" 的规则（dist/build/out 等歧义目录名）默认关闭，
    /// 需 config `tier_b = true` 才参与扫描
    pub tier: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    rule: Vec<Rule>,
}

/// 内置规则包（编译时 embed）
pub fn builtin_rules() -> Vec<Rule> {
    parse_rules(include_str!("../../../rules/rules.toml")).expect("内置规则包必须合法")
}

/// 组装最终生效规则：内置 + 用户规则（同 id 覆盖）→ Tier B 开关 → keep_days 覆盖。
/// `user_rule_texts` 为 (路径, 内容) 列表（来自 `Config::user_rule_texts()`，测试可注入）。
/// 返回 (规则, 警告)：坏的用户规则文件被跳过并生成警告，不阻断扫描。
pub fn load_rules(
    cfg: &crate::config::Config,
    user_rule_texts: &[(std::path::PathBuf, String)],
) -> (Vec<Rule>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut rules = builtin_rules();

    // 用户规则：同 id 覆盖内置，新 id 追加
    for (path, text) in user_rule_texts {
        match parse_rules(text) {
            Ok(user) => {
                for r in user {
                    match rules.iter().position(|b| b.id == r.id) {
                        Some(i) => rules[i] = r,
                        None => rules.push(r),
                    }
                }
            }
            Err(e) => warnings.push(format!("用户规则 {} 解析失败（已跳过）：{}", path.display(), e)),
        }
    }

    // Tier B 默认关闭
    if !cfg.tier_b {
        rules.retain(|r| r.tier.as_deref() != Some("b"));
    }

    // config 的 keep_days 覆盖（对 retention 规则才有意义，但统一应用）
    for r in &mut rules {
        if let Some(d) = cfg.keep_days.get(&r.id) {
            r.keep_days = Some(*d);
        }
    }
    (rules, warnings)
}

/// 解析规则 TOML
pub fn parse_rules(text: &str) -> Result<Vec<Rule>, String> {
    let f: RulesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for r in &f.rule {
        match r.scope {
            Scope::Home => {
                if r.path.is_none() {
                    return Err(format!("规则 {} 缺少 path", r.id));
                }
            }
            Scope::Workspace => {
                if r.match_dir.is_none() {
                    return Err(format!("规则 {} 缺少 match_dir", r.id));
                }
            }
        }
    }
    Ok(f.rule)
}

/// 展开 "{a,b}" 花括号多选 → 候选列表
pub fn expand_braces(s: &str) -> Vec<String> {
    let (start, end) = match (s.find('{'), s.find('}')) {
        (Some(a), Some(b)) if a < b => (a, b),
        _ => return vec![s.to_string()],
    };
    let prefix = &s[..start];
    let suffix = &s[end + 1..];
    s[start + 1..end]
        .split(',')
        .flat_map(|alt| expand_braces(&format!("{prefix}{alt}{suffix}")))
        .collect()
}

/// 极简通配符匹配：仅支持 '*'（任意序列），其余字符逐字比较。
fn wild_impl(pat: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < pat.len() && (pat[pi] == b'?' || pat[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            si = mark + 1;
            mark = si;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// 文件名通配符匹配（'*' 任意序列、'?' 单字符）
pub fn wild_match(pattern: &str, name: &str) -> bool {
    wild_impl(pattern.as_bytes(), name.as_bytes())
}

/// 把 `~` 前缀展开为实际 home
pub fn expand_tilde(path: &str, home: &std::path::Path) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home.join(rest)
    } else if path == "~" {
        home.to_path_buf()
    } else {
        std::path::PathBuf::from(path)
    }
}

use crate::never::is_never;
use crate::rule::{expand_braces, expand_tilde, wild_match, Category, Risk, Rule, Scope};
use crate::walk::{dir_size, parallel_walk, Flow, Visitor, WalkErrors};
use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

/// 扫描出的一个可清理单元
#[derive(Clone, Debug)]
pub struct JunkItem {
    pub rule_id: String,
    pub agent: Option<String>,
    pub category: Category,
    pub risk: Risk,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    /// retention 规则已超过保留期 / safe 规则恒真
    pub expired: bool,
    /// workspace 规则：同级缺少清单文件（⚠ 默认不清理）
    pub no_manifest: bool,
    /// workspace 规则：所属项目当前是否活跃（源码近期有改动）。
    /// Some(true) = 活跃，默认跳过；None = 大本营条目或未知。
    pub active: Option<bool>,
}

impl JunkItem {
    /// 保守口径的"可回收"（safe + 已过期 retention；redownload 单独统计）
    pub fn reclaimable(&self) -> bool {
        match self.risk {
            Risk::Safe => true,
            Risk::Retention => self.expired,
            Risk::Redownload => false,
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub items: Vec<JunkItem>,
    pub errors: Vec<(PathBuf, String)>,
    /// 因命中 NEVER 黑名单被过滤掉的条目数
    pub never_dropped: usize,
}

fn age_days(now: SystemTime, mtime: SystemTime) -> u64 {
    now.duration_since(mtime).unwrap_or_default().as_secs() / 86_400
}

fn expired_by(risk: Risk, keep_days: Option<u64>, now: SystemTime, mtime: SystemTime) -> bool {
    match risk {
        Risk::Safe => true,
        Risk::Retention => age_days(now, mtime) >= keep_days.unwrap_or(0),
        Risk::Redownload => false,
    }
}

fn finalize(mut result: ScanResult) -> ScanResult {
    let before = result.items.len();
    result.items.retain(|it| !is_never(&it.path));
    result.never_dropped = before - result.items.len();
    result
}

// ────────────────── Agent 大本营扫描 ──────────────────

struct FileCollector {
    pattern: String,
    out: Mutex<Vec<(PathBuf, u64, SystemTime)>>,
}

impl Visitor for FileCollector {
    fn on_file(&self, path: &Path, meta: &Metadata) {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if wild_match(&self.pattern, name) {
                if let Ok(mtime) = meta.modified() {
                    self.out
                        .lock()
                        .unwrap()
                        .push((path.to_path_buf(), meta.len(), mtime));
                }
            }
        }
    }
}

/// 扫描 agent 大本营。`home_override` 用于测试注入伪造 home 目录。
pub fn scan_home(home_override: Option<&Path>, rules: &[Rule]) -> ScanResult {
    let real_home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let home = home_override.unwrap_or(&real_home);
    let now = SystemTime::now();
    let mut result = ScanResult::default();
    let errs = WalkErrors::default();

    for rule in rules.iter().filter(|r| r.scope == Scope::Home) {
        let raw = rule.path.as_deref().unwrap_or_default();
        let path = expand_tilde(raw, home);
        if !path.exists() {
            continue;
        }
        let pattern = rule.pattern.as_deref();
        let mut raws: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        if let Some(pat) = pattern {
            // 模式规则：递归收集匹配文件
            let col = FileCollector {
                pattern: pat.to_string(),
                out: Mutex::new(Vec::new()),
            };
            parallel_walk(&path, &col, &errs);
            raws = col.out.into_inner().unwrap();
        } else if path.is_file() {
            // 路径本身就是文件
            if let Ok(md) = fs::metadata(&path) {
                if let Ok(mtime) = md.modified() {
                    raws.push((path.clone(), md.len(), mtime));
                }
            }
        } else {
            // 整目录规则：以直接子项（文件或目录）为单位
            for ent in fs::read_dir(&path).into_iter().flatten().flatten() {
                let p = ent.path();
                let Ok(md) = fs::metadata(&p) else { continue };
                let Ok(mtime) = md.modified() else { continue };
                let size = if md.is_dir() {
                    dir_size(&p, &errs)
                } else {
                    md.len()
                };
                raws.push((p, size, mtime));
            }
        }
        for (p, size, mtime) in raws {
            result.items.push(JunkItem {
                rule_id: rule.id.clone(),
                agent: rule.agent.clone(),
                category: rule.category,
                risk: rule.risk,
                path: p,
                size,
                mtime: Some(mtime),
                expired: expired_by(rule.risk, rule.keep_days, now, mtime),
                no_manifest: false,
                active: None,
            });
        }
    }
    result.errors = errs.inner.into_inner().unwrap();
    finalize(result)
}

// ────────────────── 工作区扫描 ──────────────────

struct WorkspaceHit {
    path: PathBuf,
    rule_idx: usize,
    manifest_ok: bool,
}

struct WorkspaceVisitor<'a> {
    /// 目录名 → workspace 规则下标列表（match_dir 已展开花括号）
    by_name: &'a HashMap<String, Vec<usize>>,
    rules: &'a [Rule],
    hits: Mutex<Vec<WorkspaceHit>>,
    found: AtomicUsize,
    /// 用户配置忽略的目录名
    ignore: &'a std::collections::HashSet<&'a str>,
}

impl Visitor for WorkspaceVisitor<'_> {
    fn on_dir(&self, path: &Path) -> Flow {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return Flow::Descend;
        };
        if name == ".git" || self.ignore.contains(name) {
            return Flow::Prune; // 永不进入 .git 与用户忽略目录
        }
        if let Some(idxs) = self.by_name.get(name) {
            // 多条规则命中同名目录时取第一条（当前内置规则无同名冲突）
            let idx = idxs[0];
            let manifest_ok = match &self.rules[idx].require_sibling {
                None => true,
                Some(req) => sibling_present(path.parent().unwrap_or(path), req),
            };
            self.found.fetch_add(1, Ordering::Relaxed);
            self.hits.lock().unwrap().push(WorkspaceHit {
                path: path.to_path_buf(),
                rule_idx: idx,
                manifest_ok,
            });
            return Flow::Prune; // 命中即剪枝，不再深入（嵌套 node_modules 不会重复计）
        }
        Flow::Descend
    }
}

fn sibling_present(parent: &Path, req: &str) -> bool {
    let names: Vec<String> = match fs::read_dir(parent) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => return false,
    };
    expand_braces(req).iter().any(|alt| {
        if alt.contains('*') {
            names.iter().any(|n| wild_match(alt, n))
        } else {
            names.iter().any(|n| n == alt)
        }
    })
}

/// 工作区扫描选项
#[derive(Clone, Debug)]
pub struct WorkspaceOpts {
    /// 活跃判定窗口（天）：项目源码 mtime 距今小于该值 → 项目活跃（默认跳过）
    pub active_days: u64,
    /// 忽略的目录名（config `[[workspace]].ignore`），扫描时不进入
    pub ignore: Vec<String>,
}

impl Default for WorkspaceOpts {
    fn default() -> Self {
        Self {
            active_days: 30,
            ignore: Vec::new(),
        }
    }
}

/// 扫描工作区根目录列表（Tier A 目录名模式）
pub fn scan_workspace(roots: &[PathBuf], rules: &[Rule], opts: &WorkspaceOpts) -> Vec<ScanResult> {
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in rules.iter().enumerate() {
        if r.scope != Scope::Workspace {
            continue;
        }
        for name in expand_braces(r.match_dir.as_deref().unwrap_or_default()) {
            by_name.entry(name).or_default().push(i);
        }
    }
    // 全部垃圾目录名（活跃判定走源码时也要剪掉它们，避免读 node_modules mtime 误判 + 白走）
    let junk_names: std::collections::HashSet<String> = by_name.keys().cloned().collect();
    roots
        .iter()
        .map(|root| scan_workspace_root(root, rules, &by_name, opts, &junk_names))
        .collect()
}

fn scan_workspace_root(
    root: &Path,
    rules: &[Rule],
    by_name: &HashMap<String, Vec<usize>>,
    opts: &WorkspaceOpts,
    junk_names: &std::collections::HashSet<String>,
) -> ScanResult {
    let now = SystemTime::now();
    let errs = WalkErrors::default();
    let ignore: std::collections::HashSet<&str> =
        opts.ignore.iter().map(|s| s.as_str()).collect();
    let visitor = WorkspaceVisitor {
        by_name,
        rules,
        hits: Mutex::new(Vec::new()),
        found: AtomicUsize::new(0),
        ignore: &ignore,
    };
    parallel_walk(root, &visitor, &errs);
    let hits = visitor.hits.into_inner().unwrap();
    let mut result = ScanResult::default();
    // 项目活跃判定缓存：同项目多垃圾目录只走一次源码
    let mut active_cache: HashMap<PathBuf, bool> = HashMap::new();
    for h in hits {
        let rule = &rules[h.rule_idx];
        let size = dir_size(&h.path, &errs);
        let mtime = fs::metadata(&h.path).and_then(|m| m.modified()).ok();
        // 垃圾目录的父目录 = 项目目录；根直下的命中无法保守判定 → 视为活跃
        let active = match h.path.parent() {
            Some(proj) if proj != root => *active_cache
                .entry(proj.to_path_buf())
                .or_insert_with(|| project_is_active(proj, junk_names, opts.active_days, now)),
            _ => true,
        };
        result.items.push(JunkItem {
            rule_id: rule.id.clone(),
            agent: None,
            category: rule.category,
            risk: rule.risk,
            path: h.path,
            size,
            mtime,
            expired: match mtime {
                Some(t) => expired_by(rule.risk, rule.keep_days, now, t),
                None => false,
            },
            no_manifest: !h.manifest_ok,
            active: Some(active),
        });
    }
    result.errors = errs.inner.into_inner().unwrap();
    finalize(result)
}

/// 项目是否活跃：项目内（剪掉垃圾目录与 .git，深度上限 4）存在
/// mtime 距今 < active_days 的文件即活跃。
fn project_is_active(
    proj: &Path,
    junk_names: &std::collections::HashSet<String>,
    active_days: u64,
    now: SystemTime,
) -> bool {
    fn rec(
        dir: &Path,
        depth: usize,
        junk: &std::collections::HashSet<String>,
        cutoff_secs: u64,
        now: SystemTime,
    ) -> bool {
        if depth > 4 {
            return false;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return false;
        };
        for ent in rd.flatten() {
            let Ok(md) = ent.metadata() else { continue };
            let name = ent.file_name().to_string_lossy().into_owned();
            if md.is_dir() {
                if name == ".git" || junk.contains(&name) {
                    continue;
                }
                if rec(&ent.path(), depth + 1, junk, cutoff_secs, now) {
                    return true;
                }
            } else if let Ok(m) = md.modified() {
                if now.duration_since(m).map(|d| d.as_secs()).unwrap_or(u64::MAX)
                    < cutoff_secs * 86_400
                {
                    return true;
                }
            }
        }
        false
    }
    rec(proj, 0, junk_names, active_days, now)
}

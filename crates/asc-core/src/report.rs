//! 扫描结果聚合（供 CLI 表格与未来 TUI 复用）

use crate::rule::Category;
use crate::scanner::{scan_home, scan_workspace, JunkItem, ScanResult};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ────────────────── 大本营报表 ──────────────────

#[derive(Debug)]
pub struct CatCell {
    pub category: Category,
    pub size: u64,
    pub count: usize,
    /// safe + 已过期 retention 的体积累计
    pub reclaimable: u64,
    pub redownload: bool,
}

#[derive(Debug)]
pub struct AgentRow {
    pub agent: String,
    pub total: u64,
    /// safe + 已过期 retention（不含 redownload）
    pub reclaimable: u64,
    /// redownload 级合计（需显式确认才可清理）
    pub redownload: u64,
    pub item_count: usize,
    pub cats: Vec<CatCell>,
}

#[derive(Debug, Default)]
pub struct HomeReport {
    pub rows: Vec<AgentRow>,
    pub total: u64,
    pub reclaimable: u64,
    pub redownload: u64,
    pub item_count: usize,
    pub never_dropped: usize,
}

pub fn build_home_report(res: &ScanResult) -> HomeReport {
    // agent → category → 聚合
    let mut by_agent: BTreeMap<String, BTreeMap<Category, CatCell>> = BTreeMap::new();
    let mut report = HomeReport {
        never_dropped: res.never_dropped,
        ..HomeReport::default()
    };
    for it in &res.items {
        let agent = it.agent.clone().unwrap_or_else(|| "?".into());
        let cell = by_agent
            .entry(agent.clone())
            .or_default()
            .entry(it.category)
            .or_insert(CatCell {
                category: it.category,
                size: 0,
                count: 0,
                reclaimable: 0,
                redownload: crate::rule::Risk::Redownload == it.risk,
            });
        cell.size += it.size;
        cell.count += 1;
        if it.reclaimable() {
            cell.reclaimable += it.size;
        }
        report.total += it.size;
        report.item_count += 1;
        if it.risk == crate::rule::Risk::Redownload {
            report.redownload += it.size;
        } else if it.reclaimable() {
            report.reclaimable += it.size;
        }
    }
    report.rows = by_agent
        .into_iter()
        .map(|(agent, cats)| {
            let mut cats: Vec<CatCell> = cats.into_values().collect();
            cats.sort_by_key(|c| std::cmp::Reverse(c.size));
            let total: u64 = cats.iter().map(|c| c.size).sum();
            let reclaimable: u64 = cats.iter().map(|c| c.reclaimable).sum();
            let redownload: u64 = cats
                .iter()
                .filter(|c| c.redownload)
                .map(|c| c.size)
                .sum();
            let item_count: usize = cats.iter().map(|c| c.count).sum();
            AgentRow {
                agent,
                total,
                reclaimable,
                redownload,
                item_count,
                cats,
            }
        })
        .collect();
    report.rows.sort_by(|a, b| b.total.cmp(&b.total).then(a.agent.cmp(&b.agent)));
    report
}

// ────────────────── 工作区报表 ──────────────────

#[derive(Debug)]
pub struct GroupRow {
    pub rule_id: String,
    pub dir_name: String,
    pub count: usize,
    pub total: u64,
    pub no_manifest: usize,
}

#[derive(Debug)]
pub struct TopItem {
    pub path: PathBuf,
    pub size: u64,
    pub no_manifest: bool,
}

#[derive(Debug, Default)]
pub struct WorkspaceReport {
    pub root: PathBuf,
    pub groups: Vec<GroupRow>,
    pub top: Vec<TopItem>,
    pub total: u64,
    pub count: usize,
    pub no_manifest_total: usize,
    pub never_dropped: usize,
}

pub fn build_workspace_report(root: &Path, res: &ScanResult, top_n: usize) -> WorkspaceReport {
    let mut report = WorkspaceReport {
        root: root.to_path_buf(),
        never_dropped: res.never_dropped,
        ..WorkspaceReport::default()
    };
    // rule_id → 分组
    let mut groups: BTreeMap<String, GroupRow> = BTreeMap::new();
    let mut top: Vec<TopItem> = res
        .items
        .iter()
        .map(|i| TopItem {
            path: i.path.clone(),
            size: i.size,
            no_manifest: i.no_manifest,
        })
        .collect();
    top.sort_by_key(|t| std::cmp::Reverse(t.size));
    top.truncate(top_n);
    report.top = top;
    for it in &res.items {
        let g = groups
            .entry(it.rule_id.clone())
            .or_insert_with(|| GroupRow {
                rule_id: it.rule_id.clone(),
                dir_name: it
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string(),
                count: 0,
                total: 0,
                no_manifest: 0,
            });
        g.count += 1;
        g.total += it.size;
        if it.no_manifest {
            g.no_manifest += 1;
        }
        report.total += it.size;
        report.count += 1;
        if it.no_manifest {
            report.no_manifest_total += 1;
        }
    }
    report.groups = groups.into_values().collect();
    report.groups.sort_by_key(|g| std::cmp::Reverse(g.total));
    report
}

/// 便捷入口：一次跑完两大战场（保留原始条目供清理与 TUI 使用）
pub struct ScanBundle {
    pub home_res: ScanResult,
    pub ws_results: Vec<(PathBuf, ScanResult)>,
    pub home: HomeReport,
    pub workspaces: Vec<WorkspaceReport>,
}

pub fn scan_bundle(
    workspace_roots: &[PathBuf],
    home_override: Option<&std::path::Path>,
) -> ScanBundle {
    scan_bundle_with(workspace_roots, home_override, &crate::rule::builtin_rules(), &Default::default())
}

/// 带规则与工作区选项的完整扫描（M2：配置/活跃判定从调用方注入）
pub fn scan_bundle_with(
    workspace_roots: &[PathBuf],
    home_override: Option<&std::path::Path>,
    rules: &[crate::rule::Rule],
    ws_opts: &crate::scanner::WorkspaceOpts,
) -> ScanBundle {
    let home_res = scan_home(home_override, rules);
    let home = build_home_report(&home_res);
    let ws_results = scan_workspace(workspace_roots, rules, ws_opts)
        .into_iter()
        .zip(workspace_roots.iter().cloned())
        .map(|(res, root)| (root, res))
        .collect::<Vec<_>>();
    let workspaces = ws_results
        .iter()
        .map(|(root, res)| build_workspace_report(root, res, 5))
        .collect();
    ScanBundle {
        home_res,
        ws_results,
        home,
        workspaces,
    }
}

/// 导出原始条目（供测试与未来 TUI 使用）
pub fn raw_items(res: &ScanResult) -> &[JunkItem] {
    &res.items
}

//! `asc clean`：计划打印 → 人工确认（y/N；--shred 需键入 DELETE）→ 执行。
//! 零自动删除：非交互环境（stdin 非 tty）只允许 --dry-run。

use asc_core::clean::{big_items, execute, make_plan, Dest, SHRED_THRESHOLD};
use asc_core::config::Config;
use asc_core::fmt::human_bytes;
use asc_core::rule::{load_rules, Category};
use asc_core::scanner::{scan_home, scan_workspace, JunkItem, WorkspaceOpts};
use asc_core::trash;
use owo_colors::OwoColorize;
use std::collections::BTreeMap;
use std::io::{IsTerminal, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

pub struct CleanOpts {
    pub paths: Vec<PathBuf>,
    pub category: Option<String>,
    pub agent: Option<String>,
    pub privacy: bool,
    pub shred: bool,
    pub dry_run: bool,
    /// 连活跃项目的工作区垃圾也清理（默认跳过活跃项目）
    pub include_active: bool,
}

fn parse_categories(s: &str) -> Result<Vec<Category>, String> {
    s.split(',')
        .map(|p| match p.trim() {
            "sessions" => Ok(Category::Sessions),
            "logs" => Ok(Category::Logs),
            "caches" => Ok(Category::Caches),
            "telemetry" => Ok(Category::Telemetry),
            "packages" => Ok(Category::Packages),
            "temp" => Ok(Category::Temp),
            "build" => Ok(Category::Build),
            other => Err(format!("未知类别 {other:?}（可选 sessions/logs/caches/telemetry/packages/temp/build）")),
        })
        .collect()
}

fn gather_items(paths: &[PathBuf], cfg: &Config) -> Vec<JunkItem> {
    let (rules, _warn) = load_rules(cfg, &Config::user_rule_texts());
    if paths.is_empty() {
        scan_home(None, &rules).items
    } else {
        let opts = WorkspaceOpts {
            active_days: cfg.active_days,
            ignore: cfg
                .workspace_roots()
                .iter()
                .flat_map(|(_, ig)| ig.iter().cloned())
                .collect(),
        };
        scan_workspace(paths, &rules, &opts)
            .into_iter()
            .flat_map(|r| r.items)
            .collect()
    }
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn confirm_typed(prompt: &str, word: &str) -> bool {
    print!("{prompt} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    line.trim() == word
}

pub fn run(opts: CleanOpts) -> i32 {
    let start = Instant::now();
    let cats = match &opts.category {
        Some(s) => match parse_categories(s) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("错误：{e}");
                return 2;
            }
        },
        None => None,
    };
    let agents: Vec<String> = opts
        .agent
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let lc = Config::load();
    for w in &lc.warnings {
        eprintln!("{} {w}", "⚠".yellow());
    }
    // CLI 路径 + config [[workspace]] 根合并（CLI 在前）；
    // 全空时并入自动发现根（agent 会话元数据），再全空 → 只扫大本营
    let paths: Vec<PathBuf> = {
        let mut ps = opts.paths.clone();
        for (r, _) in lc.config.workspace_roots() {
            if !ps.contains(&r) {
                ps.push(r);
            }
        }
        if ps.is_empty() {
            for d in asc_core::discover::discover_workspace_roots(30) {
                ps.push(d.path);
            }
        }
        ps
    };
    for p in &paths {
        if !p.is_dir() {
            eprintln!("错误：{} 不是目录", p.display());
            return 2;
        }
    }
    eprintln!("扫描…");
    let mut items = gather_items(&paths, &lc.config);

    // ── 选择口径 ──
    let mut skipped_unexpired = 0usize;
    let mut skipped_redownload = 0usize;
    let mut skipped_active = 0usize;
    items.retain(|it| {
        if !agents.is_empty() && !it
            .agent
            .as_deref()
            .map(|a| agents.contains(&a.to_lowercase()))
            .unwrap_or(false)
        {
            return false;
        }
        if it.active == Some(true) && !opts.include_active {
            skipped_active += 1;
            return false;
        }
        if opts.privacy {
            return it.category == Category::Sessions;
        }
        match &cats {
            Some(c) => c.contains(&it.category),
            None => {
                // 默认（安全档）：safe + 已过期 retention
                if it.reclaimable() {
                    true
                } else if it.risk == asc_core::rule::Risk::Redownload {
                    skipped_redownload += 1;
                    false
                } else {
                    skipped_unexpired += 1;
                    false
                }
            }
        }
    });

    if items.is_empty() {
        println!("没有符合条件的目标，无需清理。");
        return 0;
    }

    let plan = make_plan(items, opts.shred);
    let total_bytes: u64 = plan.iter().map(|p| p.item.size).sum();
    let (big_n, big_bytes) = big_items(&plan);

    // ── 计划打印 ──
    println!("\n{}", "── 清理计划 ".bold().cyan());
    let mut by_rule: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for p in &plan {
        let e = by_rule.entry(p.item.rule_id.as_str()).or_insert((0, 0));
        e.0 += 1;
        e.1 += p.item.size;
    }
    let mut rules: Vec<_> = by_rule.into_iter().collect();
    rules.sort_by_key(|r| std::cmp::Reverse(r.1 .1));
    for (rule, (n, sz)) in &rules {
        println!("  {:<10} {:>5} 项  {:>10}", rule, n, human_bytes(*sz));
    }
    let mut top: Vec<&PlannedRef> = plan.iter().collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.item.size));
    println!("{}", "最大 10 项:".dimmed());
    for p in top.iter().take(10) {
        println!(
            "  {:>10}  {}{}",
            human_bytes(p.item.size),
            p.item.path.display(),
            if p.dest == Dest::Shred {
                "  → 直接粉碎".red().to_string()
            } else {
                String::new()
            }
        );
    }
    let mode_note = if opts.privacy {
        "  ⚠ --privacy：包含未过期的会话记录\n".yellow().to_string()
    } else {
        String::new()
    };
    print!("{mode_note}");
    if big_n > 0 && !opts.shred {
        println!(
            "{} {} 个大件（{}）将进回收站：同盘不省空间，可加 --shred 直接粉碎",
            "ℹ".blue(),
            big_n,
            human_bytes(big_bytes)
        );
    }
    println!(
        "合计 {} 项 · {}（{}）",
        plan.len().to_string().bold(),
        human_bytes(total_bytes).bold(),
        if opts.shred && big_n > 0 {
            format!(
                "其中 {} 直接粉碎、{} 进回收站",
                human_bytes(big_bytes).red(),
                human_bytes(total_bytes - big_bytes)
            )
        } else {
            format!(
                "全部进回收站（保留 {} 天，可 asc trash restore 反悔）",
                lc.config.trash_keep_days
            )
        }
    );
    if skipped_redownload > 0 || skipped_unexpired > 0 || skipped_active > 0 {
        println!(
            "{} 已跳过：{} 项未过期、{} 项可再下载类（需 --category 显式指定）、{} 项属活跃项目（需 --include-active）",
            "ℹ".blue(),
            skipped_unexpired,
            skipped_redownload,
            skipped_active
        );
    }

    if opts.dry_run {
        println!("\n{} dry-run：以上仅为计划，未做任何修改。", "✓".green());
        return 0;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("\n错误：非交互环境只支持 --dry-run（删除必须人工确认，无跳过参数）。");
        return 3;
    }

    // ── 人工确认 ──
    if !confirm(&format!(
        "\n确认清理以上 {} 项 ({})？(y/N)",
        plan.len(),
        human_bytes(total_bytes)
    )) {
        println!("已取消，未做任何修改。");
        return 0;
    }
    if opts.shred && big_n > 0 && !confirm_typed(
        &format!(
            "其中 {} 个大件 ({}) 将{}直接粉碎、不可恢复。输入 DELETE 确认：",
            big_n,
            human_bytes(big_bytes),
            ""
        ),
        "DELETE",
    ) {
        println!("已取消粉碎与整个清理。");
        return 0;
    }

    // ── 执行 ──
    let trash_root = trash::trash_root();
    let progress = |done: usize, total: usize, path: &std::path::Path| {
        if total > 0 {
            eprint!("\r进度 {}/{}  {}", done + 1, total, path.display());
            let _ = std::io::stderr().flush();
        }
    };
    let out = execute(&plan, &trash_root, &progress);
    eprintln!("\r{}", " ".repeat(60));
    println!("\n{}", "── 清理完成 ".bold().green());
    println!(
        "  释放 {} · {} 项进回收站 · {} 项粉碎 · 失败 {} 项",
        human_bytes(out.freed_bytes).bold().green(),
        out.trashed_count,
        out.shredded_count,
        out.failed.len()
    );
    if let Some(id) = &out.trash_run_id {
        println!("  反悔：{} {}", "asc trash restore".cyan(), id);
    }
    if !out.failed.is_empty() {
        for (p, e) in out.failed.iter().take(10) {
            println!("  {} {}: {}", "✗".red(), p.display(), e);
        }
    }
    if !out.never_blocked.is_empty() {
        println!(
            "  {} NEVER 黑名单拦截 {} 项（规则可能配错，请反馈）",
            "⚠".yellow(),
            out.never_blocked.len()
        );
    }
    println!(
        "\n{} (耗时 {:.1}s)",
        "完成".dimmed(),
        start.elapsed().as_secs_f64()
    );
    // 保留引用避免未使用告警（SHRED_THRESHOLD 文档化用途）
    let _ = SHRED_THRESHOLD;
    0
}

type PlannedRef = asc_core::clean::PlannedItem;

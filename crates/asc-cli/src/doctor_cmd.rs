//! `asc doctor`：只读诊断——配置状态、检测到的 agent、规则统计、
//! 自动发现的工作区根、回收站状态。不做任何修改。

use asc_core::config::Config;
use asc_core::fmt::human_bytes;
use asc_core::rule::{builtin_rules, load_rules};
use asc_core::trash;
use asc_core::Scope;
use owo_colors::OwoColorize;

pub fn run() -> i32 {
    let lc = Config::load();
    for w in &lc.warnings {
        eprintln!("{} {w}", "⚠".yellow());
    }
    let cfg = &lc.config;

    println!(
        "{} v{}",
        "── agent-shit-cleaner doctor".bold().cyan(),
        env!("CARGO_PKG_VERSION")
    );

    // ── 配置 ──
    let cfg_path = Config::config_path();
    println!(
        "\n配置文件    {}{}",
        cfg_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(平台无配置目录)".into()),
        match (&cfg_path, cfg_path.as_ref().filter(|p| p.exists())) {
            (Some(_), Some(_)) => "（已存在）".green().to_string(),
            _ => "（未创建，用默认值）".dimmed().to_string(),
        }
    );
    let user_rules = Config::user_rule_texts();
    println!(
        "用户规则    {} 个文件",
        match Config::rules_dir() {
            Some(d) if d.exists() => user_rules.len().to_string(),
            _ => "0".dimmed().to_string(),
        }
    );
    println!(
        "生效配置    Tier B {} · 活跃窗口 {} 天 · 回收站保留 {} 天 · 工作区根 {} 个",
        if cfg.tier_b { "开" } else { "关" },
        cfg.active_days,
        cfg.trash_keep_days,
        cfg.workspace.len()
    );

    // ── 规则统计 ──
    let (rules, _) = load_rules(cfg, &user_rules);
    let builtin = builtin_rules();
    let tier_b_total = builtin
        .iter()
        .filter(|r| r.tier.as_deref() == Some("b"))
        .count();
    println!(
        "\n规则        生效 {} 条 / 内置 {} 条（Tier A {} · Tier B {}）",
        rules.len(),
        builtin.len(),
        builtin.len() - tier_b_total,
        tier_b_total
    );

    // ── 检测到的 agent（大本营路径真实存在才算装了） ──
    println!("\n{}", "检测到的 agent:".bold());
    let home = dirs::home_dir().unwrap_or_default();
    let mut agents: Vec<(String, Vec<&asc_core::rule::Rule>)> = Vec::new();
    for r in &rules {
        if r.scope != Scope::Home {
            continue;
        }
        let agent = r.agent.clone().unwrap_or_default();
        match agents.iter_mut().find(|(a, _)| *a == agent) {
            Some((_, v)) => v.push(r),
            None => agents.push((agent, vec![r])),
        }
    }
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    let mut found_any = false;
    for (agent, rs) in &agents {
        let hit = rs
            .iter()
            .filter(|r| {
                r.path
                    .as_deref()
                    .map(|p| asc_core::rule::expand_tilde(p, &home).exists())
                    .unwrap_or(false)
            })
            .count();
        if hit > 0 {
            found_any = true;
            println!("  {}  {} 条规则路径存在", agent.green(), hit);
        }
    }
    if !found_any {
        println!("  {}", "（未发现受支持 agent 的数据目录）".dimmed());
    }

    // ── 工作区根 ──
    println!("\n{}", "工作区根:".bold());
    for (r, ig) in cfg.workspace_roots() {
        println!(
            "  config  {}{}",
            r.display(),
            if ig.is_empty() {
                String::new()
            } else {
                format!("  (忽略: {})", ig.join(", "))
            }
        );
    }
    let discovered = asc_core::discover::discover_workspace_roots(30);
    if discovered.is_empty() {
        println!("  {}", "自动发现  (无)——claude/codex/cursor 的会话元数据里没有可解码的项目路径".dimmed());
    } else {
        println!("  自动发现  {} 个（claude-code/codex/cursor 会话元数据）", discovered.len());
        for d in discovered.iter().take(10) {
            println!("    [{:<11}] {}", d.source, crate::shorten(&d.path));
        }
        if discovered.len() > 10 {
            println!("    {} 另外 {} 个未显示", "…".dimmed(), discovered.len() - 10);
        }
    }

    // ── 回收站 ──
    let root = trash::trash_root();
    let runs = trash::list(&root);
    println!("\n{}", "回收站:".bold());
    if runs.is_empty() {
        println!("  空（{}）", root.display());
    } else {
        let total: u64 = runs.iter().flat_map(|r| r.entries.iter()).map(|e| e.size).sum();
        let n: usize = runs.iter().map(|r| r.entries.len()).sum();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let oldest = runs.iter().map(|r| r.created).min().unwrap_or(0);
        let days = (now.saturating_sub(oldest)) as f64 / 86_400.0;
        let days_str = format!("{days:.1}");
        println!(
            "  {} 个 run · {} 项 · {} · 最早 {} 天前",
            runs.len(),
            n,
            human_bytes(total).bold(),
            days_str
        );
        println!(
            "  过期清理  asc trash empty --expired（保留 {} 天）",
            cfg.trash_keep_days
        );
    }

    println!("\n{} 只读诊断完成，未做任何修改。", "✓".green());
    0
}

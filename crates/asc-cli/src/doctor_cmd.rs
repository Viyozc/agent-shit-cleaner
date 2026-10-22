//! `asc doctor`: read-only diagnostics — config status, detected agents,
//! rule stats, auto-discovered workspace roots. Modifies nothing.

use asc_core::config::Config;
use asc_core::rule::{builtin_rules, load_rules};
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

    // ── config ──
    let cfg_path = Config::config_path();
    println!(
        "\nconfig file  {}{}",
        cfg_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no config dir on this platform)".into()),
        match (&cfg_path, cfg_path.as_ref().filter(|p| p.exists())) {
            (Some(_), Some(_)) => " (exists)".green().to_string(),
            _ => " (not created; using defaults)".dimmed().to_string(),
        }
    );
    let user_rules = Config::user_rule_texts();
    println!(
        "user rules   {} file(s)",
        match Config::rules_dir() {
            Some(d) if d.exists() => user_rules.len().to_string(),
            _ => "0".dimmed().to_string(),
        }
    );
    println!(
        "effective    Tier B {} · active window {} days · {} workspace roots",
        if cfg.tier_b { "on" } else { "off" },
        cfg.active_days,
        cfg.workspace.len()
    );

    // ── rule stats ──
    let (rules, _) = load_rules(cfg, &user_rules);
    let builtin = builtin_rules();
    let tier_b_total = builtin
        .iter()
        .filter(|r| r.tier.as_deref() == Some("b"))
        .count();
    println!(
        "\nrules        {} active / {} builtin (Tier A {} · Tier B {})",
        rules.len(),
        builtin.len(),
        builtin.len() - tier_b_total,
        tier_b_total
    );

    // ── detected agents (a home path that actually exists = installed) ──
    println!("\n{}", "detected agents:".bold());
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
    agents.sort_by_key(|(a, _)| a.clone());
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
            println!("  {}  {} rule paths exist", agent.green(), hit);
        }
    }
    if !found_any {
        println!("  {}", "(no supported agent data dirs found)".dimmed());
    }

    // ── workspace roots ──
    println!("\n{}", "workspace roots:".bold());
    for (r, ig) in cfg.workspace_roots() {
        println!(
            "  config  {}{}",
            r.display(),
            if ig.is_empty() {
                String::new()
            } else {
                format!("  (ignore: {})", ig.join(", "))
            }
        );
    }
    let discovered = asc_core::discover::discover_workspace_roots(30);
    if discovered.is_empty() {
        println!("  {}", "discovery  (none) — no decodable project paths in claude/codex/cursor session metadata".dimmed());
    } else {
        println!(
            "  discovery {} root(s) (claude-code/codex/cursor session metadata)",
            discovered.len()
        );
        for d in discovered.iter().take(10) {
            println!("    [{:<11}] {}", d.source, crate::shorten(&d.path));
        }
        if discovered.len() > 10 {
            println!("    {} {} more not shown", "…".dimmed(), discovered.len() - 10);
        }
    }

    println!("\n{} read-only diagnostics done, nothing was modified.", "✓".green());
    0
}

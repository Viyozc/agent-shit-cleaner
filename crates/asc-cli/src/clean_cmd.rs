//! `asc clean` batch flow (reached when filter flags are used): print plan →
//! human confirmation (y/N; --shred requires typing DELETE) → execute.
//! Zero auto-deletion: without an interactive terminal only the plan is printed.
//! Trash = the OS trash (Finder Put Back / freedesktop / Recycle Bin).

use asc_core::clean::{big_items, execute, make_plan, system_trash, Dest, SHRED_THRESHOLD};
use asc_core::config::Config;
use asc_core::fmt::human_bytes;
use asc_core::rule::{load_rules, Category};
use asc_core::scanner::{scan_home, scan_workspace, JunkItem, WorkspaceOpts};
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
    /// Also clean workspace junk of active projects (skipped by default)
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
            other => Err(format!(
                "unknown category {other:?} (valid: sessions/logs/caches/telemetry/packages/temp/build)"
            )),
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
                eprintln!("error: {e}");
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
    // CLI paths + config [[workspace]] roots (CLI first);
    // when both empty, fall back to auto-discovered roots (agent session
    // metadata); still empty → scan agent homes only
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
            eprintln!("error: {} is not a directory", p.display());
            return 2;
        }
    }
    eprintln!("scanning…");
    let mut items = gather_items(&paths, &lc.config);

    // ── selection ──
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
                // default (safe profile): safe + expired retention
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
        println!("Nothing matched — nothing to clean.");
        return 0;
    }

    let plan = make_plan(items, opts.shred);
    let total_bytes: u64 = plan.iter().map(|p| p.item.size).sum();
    let (big_n, big_bytes) = big_items(&plan);

    // ── plan ──
    println!("\n{}", "── clean plan ".bold().cyan());
    let mut by_rule: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for p in &plan {
        let e = by_rule.entry(p.item.rule_id.as_str()).or_insert((0, 0));
        e.0 += 1;
        e.1 += p.item.size;
    }
    let mut rules: Vec<_> = by_rule.into_iter().collect();
    rules.sort_by_key(|r| std::cmp::Reverse(r.1 .1));
    for (rule, (n, sz)) in &rules {
        println!("  {:<10} {:>5} items  {:>10}", rule, n, human_bytes(*sz));
    }
    let mut top: Vec<&PlannedRef> = plan.iter().collect();
    top.sort_by_key(|p| std::cmp::Reverse(p.item.size));
    println!("{}", "top 10 by size:".dimmed());
    for p in top.iter().take(10) {
        println!(
            "  {:>10}  {}{}",
            human_bytes(p.item.size),
            p.item.path.display(),
            if p.dest == Dest::Shred {
                "  → shred".red().to_string()
            } else {
                String::new()
            }
        );
    }
    let mode_note = if opts.privacy {
        "  ⚠ --privacy: includes UNEXPIRED session transcripts\n".yellow().to_string()
    } else {
        String::new()
    };
    print!("{mode_note}");
    if big_n > 0 && !opts.shred {
        println!(
            "{} {} big item(s) ({}) will go to the trash: same-disk moves save no space; add --shred to delete outright",
            "ℹ".blue(),
            big_n,
            human_bytes(big_bytes)
        );
    }
    println!(
        "total {} items · {} ({})",
        plan.len().to_string().bold(),
        human_bytes(total_bytes).bold(),
        if opts.shred && big_n > 0 {
            format!(
                "{} shredded, {} to trash",
                human_bytes(big_bytes).red(),
                human_bytes(total_bytes - big_bytes)
            )
        } else {
            "all to the OS trash (recover via Finder Put Back / your desktop environment)".to_string()
        }
    );
    if skipped_redownload > 0 || skipped_unexpired > 0 || skipped_active > 0 {
        println!(
            "{} skipped: {} unexpired, {} redownloadable (needs --category), {} in active projects (needs --include-active)",
            "ℹ".blue(),
            skipped_unexpired,
            skipped_redownload,
            skipped_active
        );
    }

    if !std::io::stdin().is_terminal() {
        println!(
            "\n{} non-interactive run: plan only — nothing was deleted (deletion needs a human).",
            "✓".green()
        );
        return 0;
    }

    // ── confirmation ──
    if !confirm(&format!(
        "\nClean the {} items above ({})? (y/N)",
        plan.len(),
        human_bytes(total_bytes)
    )) {
        println!("Cancelled — nothing was modified.");
        return 0;
    }
    if opts.shred
        && big_n > 0
        && !confirm_typed(
            &format!(
                "{} big item(s) ({}) will be shredded IRREVERSIBLY. Type DELETE to confirm:",
                big_n,
                human_bytes(big_bytes)
            ),
            "DELETE",
        )
    {
        println!("Cancelled shred and the whole clean.");
        return 0;
    }

    // ── execute ──
    let progress = |done: usize, total: usize, path: &std::path::Path| {
        if total > 0 {
            eprint!("\rprogress {}/{}  {}", done + 1, total, path.display());
            let _ = std::io::stderr().flush();
        }
    };
    let out = execute(&plan, &system_trash, &progress);
    eprintln!("\r{}", " ".repeat(60));
    println!("\n{}", "── clean finished ".bold().green());
    println!(
        "  freed {} · {} items to trash · {} items shredded · {} failed",
        human_bytes(out.freed_bytes).bold().green(),
        out.trashed_count,
        out.shredded_count,
        out.failed.len()
    );
    if out.trashed_count > 0 {
        println!(
            "  {} items are in the OS trash — restore with Finder \"Put Back\" (macOS) or your DE's trash",
            out.trashed_count
        );
    }
    if !out.failed.is_empty() {
        for (p, e) in out.failed.iter().take(10) {
            println!("  {} {}: {}", "✗".red(), p.display(), e);
        }
    }
    if !out.never_blocked.is_empty() {
        println!(
            "  {} NEVER blacklist blocked {} item(s) (a rule is likely misconfigured — please report)",
            "⚠".yellow(),
            out.never_blocked.len()
        );
    }
    println!(
        "\n{} ({:.1}s)",
        "done".dimmed(),
        start.elapsed().as_secs_f64()
    );
    // 保留引用避免未使用告警（SHRED_THRESHOLD 文档化用途）
    let _ = SHRED_THRESHOLD;
    0
}

type PlannedRef = asc_core::clean::PlannedItem;

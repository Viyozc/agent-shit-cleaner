//! `asc trash`：list / restore / empty（含 --expired）。清空必须人工确认。

use asc_core::config::Config;
use asc_core::fmt::human_bytes;
use asc_core::trash;
use owo_colors::OwoColorize;
use std::io::{BufRead, IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 保留天数：config trash_keep_days 覆盖默认值
fn keep_days() -> u64 {
    Config::load().config.trash_keep_days
}

fn age_str(created: u64) -> String {
    let days = (now_secs().saturating_sub(created)) as f64 / 86_400.0;
    if days < 1.0 {
        format!("{:.1} 小时前", days * 24.0)
    } else {
        format!("{days:.1} 天前")
    }
}

pub fn list() -> i32 {
    let root = trash::trash_root();
    let runs = trash::list(&root);
    if runs.is_empty() {
        println!("回收站是空的（{}）", root.display());
        return 0;
    }
    println!(
        "{} {}",
        "── 回收站".bold().cyan(),
        root.display().to_string().dimmed()
    );
    let mut total = 0u64;
    let mut count = 0usize;
    for run in &runs {
        let sz: u64 = run.entries.iter().map(|e| e.size).sum();
        total += sz;
        count += run.entries.len();
        let origin = run
            .entries
            .first()
            .map(|e| e.origin.display().to_string())
            .unwrap_or_default();
        println!(
            "  {}  {:>3} 项  {:>10}  {}  {}",
            run.run_id,
            run.entries.len(),
            human_bytes(sz),
            age_str(run.created),
            origin.dimmed()
        );
    }
    println!(
        "合计 {} 项 · {} · 保留 {} 天（{} 查看过期项）",
        count,
        human_bytes(total).bold(),
        keep_days(),
        "asc trash empty --expired".cyan()
    );
    println!(
        "恢复：{} <run-id> · 清空：{}",
        "asc trash restore".cyan(),
        "asc trash empty".cyan()
    );
    0
}

pub fn restore(run_id: &str) -> i32 {
    let root = trash::trash_root();
    match trash::restore(run_id, &root) {
        Ok(n) => {
            println!("{} 已恢复 {n} 项到原路径", "✓".green());
            0
        }
        Err(e) => {
            eprintln!("{} {e}", "✗".red());
            1
        }
    }
}

pub fn empty(only_expired: bool) -> i32 {
    let root = trash::trash_root();
    let runs = trash::list(&root);
    if runs.is_empty() {
        println!("回收站是空的");
        return 0;
    }
    let now = now_secs();
    let targets: Vec<&asc_core::trash::TrashRun> = runs
        .iter()
        .filter(|r| {
            !only_expired || now.saturating_sub(r.created) >= keep_days() * 86_400
        })
        .collect();
    if targets.is_empty() {
        println!("没有超过 {} 天的 run，无可清理项", keep_days());
        return 0;
    }
    let sz: u64 = targets
        .iter()
        .flat_map(|r| r.entries.iter())
        .map(|e| e.size)
        .sum();
    for r in &targets {
        println!(
            "  将{} {}（{}，{} 项）",
            if only_expired { "清过期" } else { "清空" },
            r.run_id,
            age_str(r.created),
            r.entries.len()
        );
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("错误：非交互环境无法清空（删除必须人工确认）。");
        return 3;
    }
    print!(
        "\n确认{}以上 {} 个 run、释放 {} ？(y/N) ",
        if only_expired { "清理" } else { "彻底删除" },
        targets.len(),
        human_bytes(sz)
    );
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if !matches!(line.trim(), "y" | "Y" | "yes") {
        println!("已取消。");
        return 0;
    }
    match trash::purge(&root, only_expired, keep_days()) {
        Ok((n, freed)) => {
            println!("{} 已清理 {n} 个 run，释放 {}", "✓".green(), human_bytes(freed));
            0
        }
        Err(e) => {
            eprintln!("{} {e}", "✗".red());
            1
        }
    }
}

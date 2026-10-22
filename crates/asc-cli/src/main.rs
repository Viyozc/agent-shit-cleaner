mod clean_cmd;
mod doctor_cmd;

use asc_core::fmt::human_bytes;
use asc_core::report::{build_home_report, build_workspace_report};
use asc_core::{load_rules, scan_home, scan_workspace};
use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "asc",
    version,
    about = "agent-shit-cleaner — scan and clean up junk left behind by AI agents (homes + workspaces)"
)]
struct Cli {
    /// Extra workspace root for TUI mode (only when `asc` runs without a subcommand)
    #[arg(long)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read-only scan; prints a summary table (modifies nothing)
    Scan {
        /// Workspace roots; when given, only these are scanned (agent homes are skipped)
        paths: Vec<PathBuf>,
        /// Scan agent homes only
        #[arg(long)]
        agent_only: bool,
    },
    /// Clean (plan → y/N confirmation → system trash; non-interactive only allows --dry-run)
    Clean {
        /// Workspace roots; defaults to config [[workspace]] roots, then agent homes
        paths: Vec<PathBuf>,
        /// Restrict to categories (sessions/logs/caches/telemetry/packages/temp/build),
        /// comma-separated; default is the safe profile (safe + expired retention)
        #[arg(long, value_delimiter = ',')]
        category: Option<String>,
        /// Restrict to agents (claude-code/codex/cursor/...), comma-separated
        #[arg(long, value_delimiter = ',')]
        agent: Option<String>,
        /// Privacy mode: remove ALL session transcripts (including unexpired) — for handing off a machine
        #[arg(long)]
        privacy: bool,
        /// Shred big items (>1GB) instead of trashing; requires typing DELETE
        #[arg(long)]
        shred: bool,
        /// Also clean workspace junk of active projects (skipped by default)
        #[arg(long)]
        include_active: bool,
        /// Print the plan only, execute nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show config path and effective values
    Config,
    /// Read-only diagnostics: detected agents, rules, discovered workspaces
    Doctor,
}

/// 左对齐：ANSI 着色字符串按"可见宽度"补空格
fn pad_colored(plain: &str, colored: String, width: usize) -> String {
    let pad = width.saturating_sub(plain.chars().count());
    format!("{colored}{}", " ".repeat(pad))
}

/// 右对齐：ANSI 着色字符串按"可见宽度"补空格
fn rpad_colored(plain: &str, colored: String, width: usize) -> String {
    let pad = width.saturating_sub(plain.chars().count());
    format!("{}{colored}", " ".repeat(pad))
}

/// CLI 路径 + config [[workspace]] 根合并去重（CLI 在前优先）
fn merged_roots(cli_paths: &[PathBuf], cfg: &asc_core::Config) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = cli_paths.to_vec();
    for (r, _) in cfg.workspace_roots() {
        if !roots.contains(&r) {
            roots.push(r);
        }
    }
    roots
}

/// 再并入自动发现根（agent 会话元数据解码，去重）
fn roots_with_discovery(cli_paths: &[PathBuf], cfg: &asc_core::Config) -> Vec<PathBuf> {
    let mut roots = merged_roots(cli_paths, cfg);
    for d in asc_core::discover::discover_workspace_roots(30) {
        if !roots.contains(&d.path) {
            roots.push(d.path);
        }
    }
    roots
}

fn print_config() {
    let lc = asc_core::Config::load();
    for w in &lc.warnings {
        eprintln!("{} {w}", "⚠".yellow());
    }
    let c = &lc.config;
    println!("{}", "── asc config".bold().cyan());
    println!(
        "config file  {}",
        asc_core::Config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no config dir on this platform)".into())
    );
    println!(
        "user rules   {}",
        asc_core::Config::rules_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into())
    );
    match c.workspace.len() {
        0 => println!("workspaces   (none — use `asc scan <path>` or add [[workspace]])"),
        n => {
            println!("workspaces   ({n})");
            for (r, ig) in c.workspace_roots() {
                println!(
                    "  {}{}",
                    r.display(),
                    if ig.is_empty() {
                        String::new()
                    } else {
                        format!("  (ignore: {})", ig.join(", "))
                    }
                );
            }
        }
    }
    println!("Tier B       {}", if c.tier_b { "on" } else { "off" });
    println!("active days  {}", c.active_days);
    if c.keep_days.is_empty() {
        println!("keep_days    (none)");
    } else {
        println!("keep_days overrides");
        for (id, d) in &c.keep_days {
            println!("  {id} = {d}");
        }
    }
}

fn main() {
    owo_colors::set_override(std::io::stdout().is_terminal());
    let cli = Cli::parse();
    match cli.cmd {
        None => {
            // TUI by default: CLI --root + config workspace roots
            let lc = asc_core::Config::load();
            for w in &lc.warnings {
                eprintln!("{} {w}", "⚠".yellow());
            }
            let cli_roots: Vec<PathBuf> = cli.root.iter().cloned().collect();
            let roots = roots_with_discovery(&cli_roots, &lc.config);
            let code = asc_tui::run(asc_tui::TuiOptions {
                workspace_roots: roots,
                loaded: lc,
            });
            std::process::exit(code);
        }
        Some(Cmd::Config) => {
            print_config();
        }
        Some(Cmd::Doctor) => {
            let code = doctor_cmd::run();
            std::process::exit(code);
        }
        Some(Cmd::Scan { paths, agent_only }) => {
            if agent_only && !paths.is_empty() {
                eprintln!("error: --agent-only cannot be combined with path arguments");
                std::process::exit(2);
            }
            let start = Instant::now();
            let lc = asc_core::Config::load();
            for w in &lc.warnings {
                eprintln!("{} {w}", "⚠".yellow());
            }
            let (rules, _warn) = load_rules(&lc.config, &asc_core::Config::user_rule_texts());
            let ws_opts = asc_core::scanner::WorkspaceOpts {
                active_days: lc.config.active_days,
                ignore: lc
                    .config
                    .workspace_roots()
                    .iter()
                    .flat_map(|(_, ig)| ig.iter().cloned())
                    .collect(),
            };
            println!(
                "{} v{}",
                "agent-shit-cleaner".bold().cyan(),
                env!("CARGO_PKG_VERSION")
            );
            if paths.is_empty() {
                eprintln!("scanning agent homes…");
                let res = scan_home(None, &rules);
                print_home(&build_home_report(&res));
                // Auto-discovery: agent session metadata → project roots
                let discovered: Vec<PathBuf> = asc_core::discover::discover_workspace_roots(30)
                    .into_iter()
                    .map(|d| d.path)
                    .collect();
                if discovered.is_empty() {
                    println!(
                        "\n{} no discoverable project roots; specify one manually: {} (large trees take a moment)",
                        "hint".bold().yellow(),
                        "asc scan ~/Documents".cyan()
                    );
                } else {
                    eprintln!(
                        "discovered {} project roots (claude-code/codex/cursor session metadata), scanning workspaces…",
                        discovered.len()
                    );
                    let results = scan_workspace(&discovered, &rules, &ws_opts);
                    for (res, root) in results.iter().zip(discovered.iter()) {
                        print_workspace(&build_workspace_report(root, res, 5));
                    }
                }
            } else {
                for p in &paths {
                    if !p.is_dir() {
                        eprintln!("error: {} is not a directory", p.display());
                        std::process::exit(2);
                    }
                }
                eprintln!("scanning workspaces (sizing dirs after discovery, large trees are slow)…");
                let results = scan_workspace(&paths, &rules, &ws_opts);
                for (res, root) in results.iter().zip(paths.iter()) {
                    print_workspace(&build_workspace_report(root, res, 5));
                }
            }
            println!(
                "\n{} for interactive cleaning, just run {}",
                "hint".bold().yellow(),
                "asc".cyan()
            );
            println!(
                "{} ({:.1}s)",
                "".dimmed(),
                start.elapsed().as_secs_f64()
            );
        }
        Some(Cmd::Clean {
            paths,
            category,
            agent,
            privacy,
            shred,
            include_active,
            dry_run,
        }) => {
            let code = clean_cmd::run(clean_cmd::CleanOpts {
                paths,
                category,
                agent,
                privacy,
                shred,
                include_active,
                dry_run,
            });
            std::process::exit(code);
        }
    }
}

// ────────────────── home output ──────────────────

fn print_home(r: &asc_core::report::HomeReport) {
    println!("\n{}", "── Agent homes ".bold().cyan());
    if r.rows.is_empty() {
        println!("no agent junk found (or no supported agents installed)");
        return;
    }
    println!("{:<14} {:>10} {:>10}  BREAKDOWN", "AGENT", "TOTAL", "RECLAIM");
    for row in &r.rows {
        let mut cats: Vec<String> = Vec::new();
        for c in row.cats.iter().take(3) {
            let s = format!("{} {}", c.category.label(), human_bytes(c.size));
            cats.push(if c.redownload {
                format!("{}*", s.yellow())
            } else {
                s
            });
        }
        let reclaim = if row.reclaimable > 0 {
            human_bytes(row.reclaimable).green().to_string()
        } else {
            human_bytes(row.reclaimable).dimmed().to_string()
        };
        let reclaim_plain = human_bytes(row.reclaimable);
        let total_plain = human_bytes(row.total);
        println!(
            "{} {} {}  {}",
            pad_colored(&row.agent, row.agent.bold().to_string(), 14),
            rpad_colored(&total_plain, total_plain.clone(), 10),
            rpad_colored(&reclaim_plain, reclaim, 10),
            cats.join(" · ")
        );
    }
    let total_plain = human_bytes(r.total);
    let rec_plain = human_bytes(r.reclaimable);
    println!(
        "{} {} {}",
        pad_colored("TOTAL", "TOTAL".bold().to_string(), 14),
        rpad_colored(&total_plain, human_bytes(r.total).bold().to_string(), 10),
        rpad_colored(
            &rec_plain,
            human_bytes(r.reclaimable).bold().green().to_string(),
            10
        ),
    );
    println!(
        "  {} junk items · {}{}",
        r.item_count,
        format!("{} directly reclaimable", human_bytes(r.reclaimable)).green(),
        if r.redownload > 0 {
            format!(
                " · {} redownloadable (marked *, needs explicit opt-in)",
                human_bytes(r.redownload)
            )
            .yellow()
            .to_string()
        } else {
            String::new()
        }
    );
}

// ────────────────── workspace output ──────────────────

pub(crate) fn shorten(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = dirs::home_dir() {
        if let Some(rest) = s.strip_prefix(&format!("{}/", home.display())) {
            return format!("~/{rest}");
        }
    }
    s
}

fn bar(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn print_workspace(r: &asc_core::report::WorkspaceReport) {
    println!(
        "\n{} {} {}",
        "── workspace".bold().cyan(),
        shorten(&r.root).bold(),
        format!(
            "── {} junk dirs · {} ",
            r.count,
            human_bytes(r.total)
        )
        .cyan()
    );
    if r.groups.is_empty() {
        println!("no workspace junk found");
        return;
    }
    let max = r.groups.iter().map(|g| g.total).max().unwrap_or(1) as f64;
    for g in &r.groups {
        println!(
            "{:<24} {:<28} {:>10}",
            format!("{} ×{}", g.dir_name, g.count),
            bar(g.total as f64 / max, 28).yellow(),
            human_bytes(g.total)
        );
    }
    println!(
        "WORKSPACE TOTAL {}  {}",
        human_bytes(r.total).bold(),
        "(build-level; cleaned only when explicitly selected)".yellow()
    );
    if !r.top.is_empty() {
        println!("{}", "TOP offenders:".dimmed());
        for t in &r.top {
            println!(
                "  {:>10}  {}{}",
                human_bytes(t.size),
                shorten(&t.path),
                if t.no_manifest {
                    "  ⚠ no manifest".yellow().to_string()
                } else {
                    String::new()
                }
            );
        }
    }
    if r.no_manifest_total > 0 {
        println!(
            "{} {} dirs lack a manifest file (e.g. package.json); skipped by default when cleaning",
            "⚠".yellow(),
            r.no_manifest_total
        );
    }
}

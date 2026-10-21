mod clean_cmd;
mod doctor_cmd;
mod trash_cmd;

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
    about = "agent-shit-cleaner — 扫描并清理 AI agent 留下的垃圾（大本营 + 工作区）"
)]
struct Cli {
    /// TUI 模式附加的工作区根目录（仅 asc 不带子命令时生效）
    #[arg(long)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// 只读扫描，输出摘要表（不做任何修改）
    Scan {
        /// 工作区根目录；指定后只扫这些工作区，不扫 agent 大本营
        paths: Vec<PathBuf>,
        /// 只扫 agent 大本营
        #[arg(long)]
        agent_only: bool,
    },
    /// 清理（计划 → y/N 人工确认 → 回收站；非交互环境仅 --dry-run）
    Clean {
        /// 工作区根目录；不给则用 config [[workspace]] 根，再退回 agent 大本营
        paths: Vec<PathBuf>,
        /// 限定类别（sessions/logs/caches/telemetry/packages/temp/build），逗号分隔；
        /// 不给则默认安全档（safe + 已过期 retention）
        #[arg(long, value_delimiter = ',')]
        category: Option<String>,
        /// 限定 agent（claude-code/codex/cursor/…），逗号分隔
        #[arg(long, value_delimiter = ',')]
        agent: Option<String>,
        /// 隐私模式：清除全部会话记录（含未过期），机器转手前用
        #[arg(long)]
        privacy: bool,
        /// 大件（>1GB）直接粉碎不进回收站；需键入 DELETE 确认
        #[arg(long)]
        shred: bool,
        /// 连活跃项目的工作区垃圾也清理（默认跳过活跃项目）
        #[arg(long)]
        include_active: bool,
        /// 只打印计划，不执行
        #[arg(long)]
        dry_run: bool,
    },
    /// 回收站管理
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// 显示配置路径与当前生效值
    Config,
    /// 只读诊断：检测到的 agent、规则、自动发现的工作区、回收站
    Doctor,
}

#[derive(Subcommand)]
enum TrashCmd {
    /// 列出回收站
    List,
    /// 恢复一个 run 到原路径
    Restore { run_id: String },
    /// 清空回收站（--expired 只清超过保留期的）
    Empty {
        #[arg(long)]
        expired: bool,
    },
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
    println!("{}", "── asc 配置".bold().cyan());
    println!(
        "配置文件    {}",
        asc_core::Config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(此平台无配置目录)".into())
    );
    println!(
        "用户规则    {}",
        asc_core::Config::rules_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into())
    );
    match c.workspace.len() {
        0 => println!("工作区根    (未配置，用 asc scan <路径> 或 [[workspace]] 添加)"),
        n => {
            println!("工作区根    ({n} 个)");
            for (r, ig) in c.workspace_roots() {
                println!(
                    "  {}{}",
                    r.display(),
                    if ig.is_empty() {
                        String::new()
                    } else {
                        format!("  (忽略: {})", ig.join(", "))
                    }
                );
            }
        }
    }
    println!("Tier B      {}", if c.tier_b { "开启" } else { "关闭" });
    println!("活跃窗口    {} 天", c.active_days);
    println!("回收站保留  {} 天", c.trash_keep_days);
    if c.keep_days.is_empty() {
        println!("保留期覆盖  (无)");
    } else {
        println!("保留期覆盖");
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
            // 默认进 TUI：CLI --root + config 工作区根
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
                eprintln!("错误：--agent-only 与路径参数不能同时使用");
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
                eprintln!("扫描 agent 大本营…");
                let res = scan_home(None, &rules);
                print_home(&build_home_report(&res));
                // 自动发现：agent 会话元数据 → 项目根
                let discovered: Vec<PathBuf> = asc_core::discover::discover_workspace_roots(30)
                    .into_iter()
                    .map(|d| d.path)
                    .collect();
                if discovered.is_empty() {
                    println!(
                        "\n{} 自动发现没有可用项目根，手动指定：{}（大目录可能需要一点时间）",
                        "提示".bold().yellow(),
                        "asc scan ~/Documents".cyan()
                    );
                } else {
                    eprintln!(
                        "自动发现 {} 个项目根（claude-code/codex/cursor 会话元数据），扫描工作区…",
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
                        eprintln!("错误：{} 不是目录", p.display());
                        std::process::exit(2);
                    }
                }
                eprintln!("扫描工作区（发现目录后计算体积，大目录较慢）…");
                let results = scan_workspace(&paths, &rules, &ws_opts);
                for (res, root) in results.iter().zip(paths.iter()) {
                    print_workspace(&build_workspace_report(root, res, 5));
                }
            }
            println!(
                "\n{} 交互式清理：直接运行 {}",
                "提示".bold().yellow(),
                "asc".cyan()
            );
            println!(
                "{} (耗时 {:.1}s)",
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
        Some(Cmd::Trash { cmd }) => {
            let code = match cmd {
                TrashCmd::List => trash_cmd::list(),
                TrashCmd::Restore { run_id } => trash_cmd::restore(&run_id),
                TrashCmd::Empty { expired } => trash_cmd::empty(expired),
            };
            std::process::exit(code);
        }
    }
}

// ────────────────── 大本营输出 ──────────────────

fn print_home(r: &asc_core::report::HomeReport) {
    println!("\n{}", "── Agent 大本营 ".bold().cyan());
    if r.rows.is_empty() {
        println!("未发现 agent 垃圾（或未安装受支持的 agent）");
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
        "  {} 项垃圾 · {}{}",
        r.item_count,
        format!("{} 可直接回收", human_bytes(r.reclaimable)).green(),
        if r.redownload > 0 {
            format!(
                " · {} 可再下载类（标 *，清理需显式确认）",
                human_bytes(r.redownload)
            )
            .yellow()
            .to_string()
        } else {
            String::new()
        }
    );
}

// ────────────────── 工作区输出 ──────────────────

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
        "── 工作区".bold().cyan(),
        shorten(&r.root).bold(),
        format!(
            "── {} 个垃圾目录 · {} ",
            r.count,
            human_bytes(r.total)
        )
        .cyan()
    );
    if r.groups.is_empty() {
        println!("未发现工作区垃圾目录");
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
        "(build 级，需显式勾选才清理)".yellow()
    );
    if !r.top.is_empty() {
        println!("{}", "最肥 TOP:".dimmed());
        for t in &r.top {
            println!(
                "  {:>10}  {}{}",
                human_bytes(t.size),
                shorten(&t.path),
                if t.no_manifest {
                    "  ⚠ 无清单".yellow().to_string()
                } else {
                    String::new()
                }
            );
        }
    }
    if r.no_manifest_total > 0 {
        println!(
            "{} {} 个目录缺少清单文件（如 package.json），清理时默认跳过",
            "⚠".yellow(),
            r.no_manifest_total
        );
    }
}

use asc_core::{list_listeners, service_label, PortInfo};
use owo_colors::OwoColorize;
use std::io::{BufRead, IsTerminal, Write};

pub struct PortsOpts {
    /// 端口号或 PID；为空则只读列出
    pub kill: Vec<u64>,
    pub force: bool,
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    matches!(line.trim(), "y" | "Y" | "yes")
}

pub fn run(opts: PortsOpts) -> i32 {
    let listeners = match list_listeners() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if opts.kill.is_empty() {
        print_table(&listeners);
        return 0;
    }
    kill(&listeners, &opts)
}

// ────────────────── 只读列表 ──────────────────

fn print_table(listeners: &[PortInfo]) {
    println!("{}", "── listening ports ".bold().cyan());
    if listeners.is_empty() {
        println!("(none)");
        return;
    }
    let port_w = listeners
        .iter()
        .map(|l| l.port.to_string().len())
        .max()
        .unwrap_or(4)
        .max(4);
    let pid_w = listeners
        .iter()
        .map(|l| l.pid.to_string().len())
        .max()
        .unwrap_or(3)
        .max(3);
    const PROC_CAP: usize = 32;
    let trunc = |s: &str| if s.chars().count() > PROC_CAP {
        format!("{}…", s.chars().take(PROC_CAP - 1).collect::<String>())
    } else {
        s.to_string()
    };
    let proc_w = listeners
        .iter()
        .map(|l| trunc(&l.process).len())
        .max()
        .unwrap_or(7)
        .max(7);
    println!(
        "{:>port_w$} {:>pid_w$} {:<proc_w$} {}",
        "PORT", "PID", "PROCESS", "SERVICE / PROJECT"
    );
    for l in listeners {
        let svc = match service_label(l.port) {
            Some(s) => s.yellow().to_string(),
            None => "-".dimmed().to_string(),
        };
        let project = match &l.cwd {
            Some(p) => crate::shorten(p),
            None => "?".dimmed().to_string(),
        };
        println!(
            "{:>port_w$} {:>pid_w$} {:<proc_w$} {} {}",
            l.port.to_string(),
            l.pid.to_string(),
            trunc(&l.process),
            svc,
            project
        );
    }
    println!("{}", "✓ read-only listing, nothing was modified.".green());
}

// ────────────────── 关闭服务 ──────────────────

/// 把端口/PID 参数解析成目标进程（按 listeners 去重）。
fn resolve_targets(listeners: &[PortInfo], args: &[u64]) -> Result<Vec<PortInfo>, String> {
    let mut targets: Vec<PortInfo> = Vec::new();
    let mut bad: Vec<u64> = Vec::new();
    for &arg in args {
        let matches: Vec<&PortInfo> = listeners
            .iter()
            .filter(|l| l.port as u64 == arg || l.pid as u64 == arg)
            .collect();
        if matches.is_empty() {
            bad.push(arg);
        } else {
            for m in matches {
                if !targets.iter().any(|t| t.pid == m.pid && t.port == m.port) {
                    targets.push(m.clone());
                }
            }
        }
    }
    if !bad.is_empty() {
        let list = bad
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("no listening process matches: {list} (run `asc ports` to list)"));
    }
    Ok(targets)
}

fn send_signal(pid: i32, force: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("kill");
    if force {
        cmd.arg("-9");
    }
    let status = cmd
        .arg(pid.to_string())
        .status()
        .map_err(|e| format!("cannot run kill: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill {} failed", pid))
    }
}

fn still_listening(listeners: &[PortInfo], target: &PortInfo) -> bool {
    listeners
        .iter()
        .any(|l| l.pid == target.pid && l.port == target.port)
}

fn kill(listeners: &[PortInfo], opts: &PortsOpts) -> i32 {
    let targets = match resolve_targets(listeners, &opts.kill) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if !std::io::stdin().is_terminal() {
        eprintln!("error: --kill requires an interactive terminal (refusing to kill without confirmation)");
        return 2;
    }

    let mut failed = 0;
    for t in &targets {
        let svc = service_label(t.port).unwrap_or("-");
        let project = t
            .cwd
            .as_ref()
            .map(|p| crate::shorten(p))
            .unwrap_or_else(|| "?".into());
        if !confirm(&format!(
            "kill {} (pid {}, {})? [y/N]",
            format!("port {}", t.port).bold(),
            t.pid,
            format!("{svc}, {project}").dimmed()
        )) {
            println!("{}", "skipped.".dimmed());
            continue;
        }
        if let Err(e) = send_signal(t.pid, false) {
            eprintln!("{} {e}", "⚠".yellow());
            failed += 1;
            continue;
        }
        // SIGTERM 后重查：进程可能忽略信号或还在退出中
        let Ok(now) = list_listeners() else {
            continue;
        };
        if still_listening(&now, t) {
            if opts.force {
                if let Err(e) = send_signal(t.pid, true) {
                    eprintln!("{} {e}", "⚠".yellow());
                    failed += 1;
                    continue;
                }
                println!("{} port {} (pid {}) force-killed", "✓".green(), t.port, t.pid);
            } else {
                println!(
                    "{} port {} (pid {}) still running — retry with {}",
                    "⚠".yellow(),
                    t.port,
                    t.pid,
                    "--force".cyan()
                );
                failed += 1;
            }
        } else {
            println!("{} port {} (pid {}) terminated", "✓".green(), t.port, t.pid);
        }
    }
    if failed > 0 {
        eprintln!("{} {failed} target(s) could not be terminated", "⚠".yellow());
        return 1;
    }
    println!("{}", "✓ done.".green());
    0
}

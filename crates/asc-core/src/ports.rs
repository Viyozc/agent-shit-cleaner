//! 只读枚举：本机监听中的 TCP 端口及其进程/工作目录。
//! 通过系统 `lsof` 实现，本模块不终止任何进程。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PortInfo {
    pub port: u16,
    pub pid: i32,
    pub process: String,
    /// 进程工作目录（通常即项目路径）
    pub cwd: Option<PathBuf>,
}

/// 列出所有监听中的 TCP 端口，按端口号升序；同端口多 PID 各占一行。
pub fn list_listeners() -> Result<Vec<PortInfo>, String> {
    let out = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
        .output()
        .map_err(|e| format!("cannot run lsof: {e}"))?;
    if !out.status.success() {
        // lsof 退出码 1 = 没有匹配（无任何监听端口），不算错误
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() == Some(1) && stderr.is_empty() {
            return Ok(Vec::new());
        }
        return Err(format!("lsof failed: {}", stderr.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let mut pids: BTreeSet<i32> = BTreeSet::new();
    let mut rows: Vec<(u16, i32)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        // COMMAND PID USER ... NAME (LISTEN)；NAME 在倒数第二列，形如 *:3000 / [::]:3000 / 127.0.0.1:5432
        if f.len() < 9 {
            continue;
        }
        let (Ok(pid), Some(port)) = (f[1].parse::<i32>(), port_of(f[f.len() - 2])) else {
            continue;
        };
        pids.insert(pid);
        if !rows.contains(&(port, pid)) {
            rows.push((port, pid)); // lsof 对 IPv4/IPv6/多 fd 各输出一行，按 (port, pid) 去重
        }
    }

    let cwd_map = cwds(&pids);
    let comm_map = full_commands(&pids);
    let mut infos: Vec<PortInfo> = rows
        .into_iter()
        .map(|(port, pid)| PortInfo {
            port,
            pid,
            process: comm_map
                .get(&pid)
                .cloned()
                .unwrap_or_else(String::new),
            cwd: cwd_map.get(&pid).cloned().flatten(),
        })
        .collect();
    infos.sort_by_key(|i| (i.port, i.pid));
    Ok(infos)
}

/// 从 NAME 列提取端口；解析失败返回 None。
fn port_of(name: &str) -> Option<u16> {
    name.rsplit(':').next()?.parse().ok()
}

/// 批量查询多个 PID 的工作目录。
fn cwds(pids: &BTreeSet<i32>) -> std::collections::BTreeMap<i32, Option<PathBuf>> {
    let mut map = std::collections::BTreeMap::new();
    if pids.is_empty() {
        return map;
    }
    let pids_arg: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
    let out = Command::new("lsof")
        .args(["-a", "-p", &pids_arg.join(","), "-d", "cwd", "-Fn"])
        .output();
    let Ok(out) = out else {
        return pids.iter().map(|&p| (p, None)).collect();
    };
    let mut current: Option<i32> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current = rest.parse().ok();
        } else if let Some(path) = line.strip_prefix('n') {
            if let Some(pid) = current {
                map.insert(pid, Some(PathBuf::from(path)));
            }
        }
    }
    map
}

/// 批量查询多个 PID 的完整进程名（lsof 的 COMMAND 列截断到 9 字符）。
fn full_commands(pids: &BTreeSet<i32>) -> std::collections::BTreeMap<i32, String> {
    let mut map = std::collections::BTreeMap::new();
    if pids.is_empty() {
        return map;
    }
    let Ok(out) = Command::new("ps")
        .args(["-o", "pid=,comm=", "-p", &pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",")])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.trim().splitn(2, char::is_whitespace);
        if let (Some(p), Some(comm)) = (it.next(), it.next()) {
            if let Ok(pid) = p.trim().parse::<i32>() {
                // comm 可能是完整路径，只取文件名
                let name = std::path::Path::new(comm.trim())
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| comm.trim().to_string());
                map.insert(pid, name);
            }
        }
    }
    map
}

/// 常见 dev 端口 → 服务标签。
pub fn service_label(port: u16) -> Option<&'static str> {
    Some(match port {
        3000 | 3001 | 3002 => "Next.js/React dev",
        1420 => "Tauri dev",
        19000 | 19001 | 8081 => "React Native/Metro",
        4000 => "Phoenix/Jekyll",
        4173 => "Vite preview",
        4200 => "Angular dev",
        4321 => "Astro dev",
        5000 | 8000 | 8001 => "Flask/Django",
        5173 | 5174 => "Vite dev",
        5432 => "PostgreSQL",
        5555 => "Prisma Studio",
        6379 => "Redis",
        7000 | 7001 => "Strapi",
        8080 | 8082 => "web server",
        8443 => "HTTPS alt",
        8545 => "Hardhat/Anvil",
        8888 => "Jupyter",
        9000 => "PHP/MinIO",
        9200 => "Elasticsearch",
        9229 => "Node debugger",
        1025 => "MailHog/Mailpit",
        15672 => "RabbitMQ mgmt",
        27017 => "MongoDB",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_parsing() {
        assert_eq!(port_of("*:3000"), Some(3000));
        assert_eq!(port_of("[::]:5432"), Some(5432));
        assert_eq!(port_of("127.0.0.1:6379"), Some(6379));
        assert_eq!(port_of("*:*"), None);
    }

    #[test]
    fn labels() {
        assert_eq!(service_label(5173), Some("Vite dev"));
        assert_eq!(service_label(5432), Some("PostgreSQL"));
        assert_eq!(service_label(12345), None);
    }
}

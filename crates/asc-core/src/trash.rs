//! 回收站：删除 = 移动到 `~/.local/share/asc-trash/<run-id>/`，附 manifest。
//! 每个回收站保留 7 天（可配）；**过期不自动清**，需 `asc trash empty --expired` 显式触发。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_KEEP_DAYS: u64 = 7;

/// 回收站根目录（固定 `~/.local/share/asc-trash`，与 DESIGN.md 一致）
pub fn trash_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/asc-trash")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashEntry {
    pub seq: u32,
    pub origin: PathBuf,
    pub rule_id: String,
    pub size: u64,
    /// 在 run 目录里的存储名（`0001-原名`）
    pub stored_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrashRun {
    pub run_id: String,
    /// unix 秒
    pub created: u64,
    pub entries: Vec<TrashEntry>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn run_dir(root: &Path, run_id: &str) -> PathBuf {
    root.join(run_id)
}

fn manifest_path(root: &Path, run_id: &str) -> PathBuf {
    run_dir(root, run_id).join("manifest.toml")
}

/// 新建一个空 run（懒创建：第一次存入条目时才落盘建目录由 store 触发）
pub fn new_run() -> TrashRun {
    let ts = now_secs();
    let pid = std::process::id();
    TrashRun {
        run_id: format!("{ts:x}-{pid:x}"),
        created: ts,
        entries: Vec::new(),
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// 把一个文件/目录移入回收站（同卷 rename，秒级；跨卷报错不降级为复制）
pub fn store(run: &mut TrashRun, root: &Path, path: &Path, rule_id: &str, size: u64) -> io::Result<()> {
    let dir = run_dir(root, &run.run_id);
    fs::create_dir_all(&dir)?;
    let seq = run.entries.len() as u32 + 1;
    let origin_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed");
    let stored_name = format!("{seq:04}-{}", sanitize(origin_name));
    fs::rename(path, dir.join(&stored_name))?;
    run.entries.push(TrashEntry {
        seq,
        origin: path.to_path_buf(),
        rule_id: rule_id.to_string(),
        size,
        stored_name,
    });
    Ok(())
}

/// 落盘 manifest（执行结束/中断时都要写）
pub fn write_manifest(run: &TrashRun, root: &Path) -> io::Result<()> {
    fs::create_dir_all(run_dir(root, &run.run_id))?;
    fs::write(manifest_path(root, &run.run_id), toml::to_string_pretty(run).unwrap())?;
    Ok(())
}

/// 列出回收站所有 run（读不了 manifest 的跳过；空目录的 run 忽略）
pub fn list(root: &Path) -> Vec<TrashRun> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else {
        return out;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if !p.is_dir() {
            continue;
        }
        if let Ok(text) = fs::read(p.join("manifest.toml")) {
            if let Ok(run) = toml::from_str::<TrashRun>(&String::from_utf8_lossy(&text)) {
                out.push(run);
            }
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// 恢复一个 run 的全部条目到原路径；原位被占则落到 `<名>.restored-<run后缀>`
pub fn restore(run_id: &str, root: &Path) -> Result<usize, String> {
    let runs = list(root);
    let run = runs
        .iter()
        .find(|r| r.run_id == run_id)
        .ok_or_else(|| format!("回收站中不存在 run {run_id}"))?;
    let dir = run_dir(root, &run.run_id);
    let short = run.run_id.split('-').next_back().unwrap_or("0").to_string();
    let mut restored = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for e in &run.entries {
        let src = dir.join(&e.stored_name);
        if !src.exists() {
            errors.push(format!("条目 {} 已缺失", e.stored_name));
            continue;
        }
        let mut dest = e.origin.clone();
        if dest.exists() {
            let name = format!(
                "{}.restored-{short}",
                dest.file_name().and_then(|n| n.to_str()).unwrap_or("item")
            );
            dest = dest.with_file_name(name);
        }
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::rename(&src, &dest) {
            Ok(()) => restored += 1,
            Err(err) => errors.push(format!("{}: {err}", e.stored_name)),
        }
    }
    // 全部条目处理完后若 run 目录空了，顺带清掉
    if errors.is_empty() {
        let _ = fs::remove_dir_all(&dir);
    }
    if errors.is_empty() {
        Ok(restored)
    } else {
        Err(format!("恢复 {restored} 项，失败：{}", errors.join("; ")))
    }
}

/// 清空回收站；`only_expired=true` 时仅清超过 keep_days 的 run。
/// 返回 (清理的 run 数, 释放字节数)
pub fn purge(root: &Path, only_expired: bool, keep_days: u64) -> Result<(usize, u64), String> {
    let runs = list(root);
    let now = now_secs();
    let mut removed = 0usize;
    let mut freed = 0u64;
    for run in &runs {
        if only_expired && now.saturating_sub(run.created) < keep_days * 86_400 {
            continue;
        }
        let dir = run_dir(root, &run.run_id);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {
                removed += 1;
                freed += run.entries.iter().map(|e| e.size).sum::<u64>();
            }
            Err(e) => return Err(format!("清空 {} 失败: {e}", run.run_id)),
        }
    }
    Ok((removed, freed))
}

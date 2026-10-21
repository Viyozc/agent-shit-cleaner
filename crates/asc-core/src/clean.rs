//! 清理执行：计划 → 人工确认（由 CLI/TUI 层做）→ 执行。
//! 三道闸：调用方确认 → NEVER 终审 → 回收站/粉碎。

use crate::never::is_never;
use crate::scanner::JunkItem;
use crate::trash;
use std::path::{Path, PathBuf};

/// 大件阈值：超过则提供"粉碎"（直接删除、不进回收站）
pub const SHRED_THRESHOLD: u64 = 1024 * 1024 * 1024; // 1 GB

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// 移入回收站（可反悔）
    Trash,
    /// 直接粉碎（不可恢复，须键入 DELETE 确认过）
    Shred,
}

#[derive(Clone, Debug)]
pub struct PlannedItem {
    pub item: JunkItem,
    pub dest: Dest,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub freed_bytes: u64,
    pub trashed_count: usize,
    pub shredded_count: usize,
    /// (路径, 原因)
    pub failed: Vec<(PathBuf, String)>,
    /// NEVER 黑名单终审拦截（不应出现；出现即规则配错）
    pub never_blocked: Vec<PathBuf>,
    pub trash_run_id: Option<String>,
}

/// 生成清理计划；`shred_big=true` 时 >SHRED_THRESHOLD 的项走粉碎，否则全部进回收站
pub fn make_plan(items: Vec<JunkItem>, shred_big: bool) -> Vec<PlannedItem> {
    items
        .into_iter()
        .map(|item| {
            let dest = if shred_big && item.size > SHRED_THRESHOLD {
                Dest::Shred
            } else {
                Dest::Trash
            };
            PlannedItem { item, dest }
        })
        .collect()
}

/// 大件（将被粉碎）的尺寸合计与数量
pub fn big_items(plan: &[PlannedItem]) -> (usize, u64) {
    let n = plan.iter().filter(|p| p.dest == Dest::Shred).count();
    let bytes = plan
        .iter()
        .filter(|p| p.dest == Dest::Shred)
        .map(|p| p.item.size)
        .sum();
    (n, bytes)
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// 执行计划。进度回调 `(已处理, 总数, 当前路径)`。
/// 失败不中断：逐项记录，最后汇总（M1 决策：比"失败即停"对大批量更友好）。
pub fn execute(
    plan: &[PlannedItem],
    trash_root: &Path,
    progress: &dyn Fn(usize, usize, &Path),
) -> Outcome {
    let mut out = Outcome::default();
    let mut run = trash::new_run();
    let total = plan.len();
    for (i, p) in plan.iter().enumerate() {
        let path = &p.item.path;
        progress(i, total, path);
        if is_never(path) {
            out.never_blocked.push(path.clone());
            continue;
        }
        if !path.exists() {
            out.failed.push((path.clone(), "已不存在".into()));
            continue;
        }
        let ok = match p.dest {
            Dest::Trash => trash::store(&mut run, trash_root, path, &p.item.rule_id, p.item.size)
                .map(|()| {
                    out.trashed_count += 1;
                }),
            Dest::Shred => remove_any(path).map(|()| {
                out.shredded_count += 1;
            }),
        };
        match ok {
            Ok(()) => out.freed_bytes += p.item.size,
            Err(e) => out.failed.push((path.clone(), e.to_string())),
        }
    }
    progress(total, total, Path::new(""));
    if out.trashed_count > 0 {
        let _ = trash::write_manifest(&run, trash_root);
        out.trash_run_id = Some(run.run_id);
    } else {
        // 没有实际存入条目则不保留空 run 目录
        let _ = std::fs::remove_dir_all(trash_root.join(&run.run_id));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Category, Risk};
    use std::fs;
    use tempfile::TempDir;

    fn item(path: &Path, size: u64) -> JunkItem {
        JunkItem {
            rule_id: "t/x".into(),
            agent: None,
            category: Category::Temp,
            risk: Risk::Safe,
            path: path.to_path_buf(),
            size,
            mtime: None,
            expired: true,
            no_manifest: false,
            active: None,
        }
    }

    #[test]
    fn execute_trashes_and_shreds() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        let small = ws.join("small"); // 100B → trash
        let big = ws.join("big"); // >1GB → shred
        fs::create_dir_all(small.join("inner")).unwrap();
        fs::write(small.join("inner/f"), vec![0u8; 100]).unwrap();
        fs::create_dir_all(&big).unwrap();

        let plan = make_plan(
            vec![
                item(&small, 100),
                item(&big, 2 * SHRED_THRESHOLD),
            ],
            true,
        );
        assert_eq!(plan[0].dest, Dest::Trash);
        assert_eq!(plan[1].dest, Dest::Shred);
        assert_eq!(big_items(&plan), (1, 2 * SHRED_THRESHOLD));

        let trash_root = ws.join("trash");
        let mut seen = 0;
        let out = execute(&plan, &trash_root, &|d, t, _| assert!(d <= t), );
        let _ = &mut seen;
        assert_eq!(out.trashed_count, 1);
        assert_eq!(out.shredded_count, 1);
        assert!(out.failed.is_empty(), "失败: {:?}", out.failed);
        assert_eq!(out.freed_bytes, 100 + 2 * SHRED_THRESHOLD);
        assert!(!small.exists(), "小件应已移走");
        assert!(!big.exists(), "大件应已粉碎");
        // 回收站里有 manifest 且记录 origin
        let runs = trash::list(&trash_root);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].entries.len(), 1);
        assert_eq!(runs[0].entries[0].origin, small);
    }

    #[test]
    fn execute_blocks_never_and_missing() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        let never_dir = ws.join("skills"); // NEVER 目录名
        fs::create_dir_all(&never_dir).unwrap();
        let gone = ws.join("ghost");

        let plan = make_plan(
            vec![item(&never_dir, 10), item(&gone, 10)],
            false,
        );
        let out = execute(&plan, &ws.join("trash"), &|_, _, _| {});
        assert_eq!(out.never_blocked.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.trashed_count, 0);
        assert!(out.trash_run_id.is_none());
        assert!(never_dir.exists(), "NEVER 必须原样保留");
    }
}

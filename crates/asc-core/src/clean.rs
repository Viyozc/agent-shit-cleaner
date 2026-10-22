//! Clean execution: plan → human confirmation (done by CLI/TUI layer) → execute.
//! Three gates: caller confirmation → NEVER final review → system trash / shred.

use crate::never::is_never;
use crate::scanner::JunkItem;
use std::path::{Path, PathBuf};

/// Big-item threshold: above this, "shred" is offered (bypass the trash)
pub const SHRED_THRESHOLD: u64 = 1024 * 1024 * 1024; // 1 GB

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// Move to the OS trash (recoverable via Finder "Put Back" etc.)
    Trash,
    /// Shred (irreversible; caller must have required typing DELETE)
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
    /// (path, reason)
    pub failed: Vec<(PathBuf, String)>,
    /// Blocked by the NEVER blacklist (should not happen; a rule is misconfigured)
    pub never_blocked: Vec<PathBuf>,
}

/// Build the plan; with `shred_big=true` items >SHRED_THRESHOLD are shredded,
/// otherwise everything goes to the trash.
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

/// Count and total size of big items (would be shredded)
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

/// Move one path to the OS trash (macOS Finder / freedesktop.org / Windows
/// Recycle Bin via the `trash` crate). Production trash operation.
pub fn system_trash(path: &Path) -> std::io::Result<()> {
    trash::delete(path).map_err(|e| std::io::Error::other(e.to_string()))
}

/// Execute the plan. `trash` is the move-to-trash operation (injected so tests
/// never touch the real system trash). Progress callback: `(done, total, path)`.
/// Failures don't abort: recorded per item, summarized at the end.
pub fn execute(
    plan: &[PlannedItem],
    trash: &dyn Fn(&Path) -> std::io::Result<()>,
    progress: &dyn Fn(usize, usize, &Path),
) -> Outcome {
    let mut out = Outcome::default();
    let total = plan.len();
    for (i, p) in plan.iter().enumerate() {
        let path = &p.item.path;
        progress(i, total, path);
        if is_never(path) {
            out.never_blocked.push(path.clone());
            continue;
        }
        if !path.exists() {
            out.failed.push((path.clone(), "no longer exists".into()));
            continue;
        }
        let ok = match p.dest {
            Dest::Trash => trash(path).map(|()| {
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Category, Risk};
    use std::cell::Cell;
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

    /// Fake trash op: rename into a tempdir (never touches the system trash)
    fn fake_trash_factory(store: &Path) -> impl Fn(&Path) -> std::io::Result<()> + '_ {
        let n = Cell::new(0usize);
        move |p: &Path| {
            let i = n.get() + 1;
            n.set(i);
            fs::rename(p, store.join(format!("t{i}")))
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
            vec![item(&small, 100), item(&big, 2 * SHRED_THRESHOLD)],
            true,
        );
        assert_eq!(plan[0].dest, Dest::Trash);
        assert_eq!(plan[1].dest, Dest::Shred);
        assert_eq!(big_items(&plan), (1, 2 * SHRED_THRESHOLD));

        let store = ws.join("trash-store");
        fs::create_dir_all(&store).unwrap();
        let out = execute(&plan, &fake_trash_factory(&store), &|d, t, _| {
            assert!(d <= t)
        });
        assert_eq!(out.trashed_count, 1);
        assert_eq!(out.shredded_count, 1);
        assert!(out.failed.is_empty(), "failed: {:?}", out.failed);
        assert_eq!(out.freed_bytes, 100 + 2 * SHRED_THRESHOLD);
        assert!(!small.exists(), "small item should be moved away");
        assert!(store.join("t1").exists(), "fake trash received it");
        assert!(!big.exists(), "big item should be shredded");
    }

    #[test]
    fn execute_blocks_never_and_missing() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path();
        let never_dir = ws.join("skills"); // NEVER directory name
        fs::create_dir_all(&never_dir).unwrap();
        let gone = ws.join("ghost");

        let store = ws.join("trash-store");
        fs::create_dir_all(&store).unwrap();
        let plan = make_plan(vec![item(&never_dir, 10), item(&gone, 10)], false);
        let out = execute(&plan, &fake_trash_factory(&store), &|_, _, _| {});
        assert_eq!(out.never_blocked.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.trashed_count, 0);
        assert!(never_dir.exists(), "NEVER paths must stay untouched");
    }
}

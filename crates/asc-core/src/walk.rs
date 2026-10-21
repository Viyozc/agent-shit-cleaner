//! 并行目录遍历（rayon），带剪枝语义：
//! - 命中规则的目录直接跳过、不再深入（`find -prune` 语义）
//! - `.git` 等目录由 Visitor 决定剪枝
//! - 不跟随符号链接（文件与目录均跳过）

use rayon::scope;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// on_dir 的返回值：进入 或 剪枝（不进入、不算错误）
pub enum Flow {
    Descend,
    Prune,
}

pub trait Visitor: Sync {
    /// 目录（进入前调用）
    fn on_dir(&self, _path: &Path) -> Flow {
        Flow::Descend
    }
    /// 普通文件（已取到元数据）
    fn on_file(&self, _path: &Path, _meta: &Metadata) {}
}

#[derive(Default)]
pub struct WalkErrors {
    pub inner: Mutex<Vec<(PathBuf, String)>>,
}

impl WalkErrors {
    fn record(&self, path: &Path, err: &std::io::Error) {
        // 静默忽略权限类错误（扫描陌生目录树时很常见）
        if matches!(err.kind(), std::io::ErrorKind::PermissionDenied) {
            return;
        }
        self.inner
            .lock()
            .unwrap()
            .push((path.to_path_buf(), err.to_string()));
    }
}

/// 从 root 开始并行遍历（root 本身不经过 on_dir，只处理其内容）
pub fn parallel_walk(root: &Path, v: &dyn Visitor, errs: &WalkErrors) {
    process_dir(root, v, errs);
}

fn process_dir(dir: &Path, v: &dyn Visitor, errs: &WalkErrors) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errs.record(dir, &e);
            return;
        }
    };
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for ent in entries.flatten() {
        let Ok(ft) = ent.file_type() else { continue };
        if ft.is_symlink() {
            continue; // 不跟随符号链接
        }
        let path = ent.path();
        if ft.is_dir() {
            if matches!(v.on_dir(&path), Flow::Prune) {
                continue;
            }
            subdirs.push(path);
        } else if ft.is_file() {
            if let Ok(meta) = ent.metadata() {
                v.on_file(&path, &meta);
            }
        }
    }
    if subdirs.is_empty() {
        return;
    }
    scope(|s| {
        for d in &subdirs {
            s.spawn(move |_s| process_dir(d, v, errs));
        }
    });
}

/// 快速统计目录体积（并行、不跟随符号链接）
pub fn dir_size(path: &Path, errs: &WalkErrors) -> u64 {
    struct SizeVisitor(std::sync::atomic::AtomicU64);
    impl Visitor for SizeVisitor {
        fn on_file(&self, _p: &Path, meta: &Metadata) {
            self.0.fetch_add(meta.len(), std::sync::atomic::Ordering::Relaxed);
        }
    }
    let v = SizeVisitor(std::sync::atomic::AtomicU64::new(0));
    parallel_walk(path, &v, errs);
    v.0.load(std::sync::atomic::Ordering::Relaxed)
}

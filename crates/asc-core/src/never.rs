//! NEVER 黑名单：规则引擎最后兜底，命中即拒删/拒展示。
//! 与 DESIGN.md 第 5 节保持一致。

use std::path::Path;

/// 绝不触碰的目录名（作为路径任意一层组件出现即命中）
const NEVER_DIR_NAMES: &[&str] = &[".git", "memory", "skills", "hooks", "todos"];

/// 绝不触碰的文件名精确匹配
const NEVER_FILE_EXACT: &[&str] = &[
    "CLAUDE.md",
    "config.toml",
    "go.sum",
    ".claude.json",
    ".env",
];

/// 绝不触碰的文件名前缀
const NEVER_FILE_PREFIX: &[&str] = &[".credentials", "settings", ".env"];

/// 绝不触碰的文件名后缀
const NEVER_FILE_SUFFIX: &[&str] = &[".lock"];

/// 该路径是否命中 NEVER 黑名单（目录或文件通用）
pub fn is_never(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    for comp in path.components() {
        if let Some(seg) = comp.as_os_str().to_str() {
            if NEVER_DIR_NAMES.contains(&seg) {
                return true;
            }
        }
    }
    NEVER_FILE_EXACT.contains(&name)
        || NEVER_FILE_PREFIX.iter().any(|p| name.starts_with(p))
        || NEVER_FILE_SUFFIX.iter().any(|s| name.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_dirs() {
        assert!(is_never(Path::new("/x/.git/objects")));
        assert!(is_never(Path::new("/home/u/.claude/skills/foo")));
        assert!(is_never(Path::new("/home/u/.claude/memory/bar.json")));
    }

    #[test]
    fn never_files() {
        assert!(is_never(Path::new("/x/.credentials.json")));
        assert!(is_never(Path::new("/p/.claude/settings.local.json")));
        assert!(is_never(Path::new("/p/config.toml")));
        assert!(is_never(Path::new("/p/CLAUDE.md")));
        assert!(is_never(Path::new("/p/.env.local")));
        assert!(is_never(Path::new("/p/Cargo.lock")));
        assert!(is_never(Path::new("/home/u/.claude.json")));
    }

    #[test]
    fn not_never() {
        assert!(!is_never(Path::new("/p/node_modules/express/index.js")));
        assert!(!is_never(Path::new("/home/u/.claude/projects/x.jsonl")));
        assert!(!is_never(Path::new("/p/package.json")));
        assert!(!is_never(Path::new("/p/logs_2.sqlite")));
    }
}

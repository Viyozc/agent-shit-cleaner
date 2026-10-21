//! 工作区自动发现：从 agent 会话元数据反推项目根——
//! agent 自己交代了它在哪些项目里干过活。
//!
//! 三个来源（全部只读小元数据，不遍历项目树）：
//! 1. Claude Code：`~/.claude/projects/<munged>` 目录名即路径编码（`/` → `-`）
//! 2. Codex：`~/.codex/sessions/**/*.jsonl` 头部 JSON 的 `cwd` 字段
//! 3. Cursor：`user/workspaceStorage/*/workspace.json` 的 `folder` file:// URI
//!
//! 解码歧义（原路径里的 `-` 与 `/` 同形）用存在性校验兜底：解出来的路径
//! 不存在就跳过——best-effort，漏掉的根仍可用 config 手工补。

use std::fs;
use std::path::{Path, PathBuf};

/// 一个被发现的工作区根
#[derive(Clone, Debug)]
pub struct DiscoveredRoot {
    pub path: PathBuf,
    pub source: &'static str,
}

/// 三个元数据源的位置（测试可注入 tempdir；生产用 `DiscoverPaths::real()`）
pub struct DiscoverPaths {
    pub claude_projects: Option<PathBuf>,
    pub codex_sessions: Option<PathBuf>,
    pub cursor_workspace_storage: Option<PathBuf>,
}

impl DiscoverPaths {
    pub fn real(home: &Path) -> Self {
        DiscoverPaths {
            claude_projects: Some(home.join(".claude/projects")),
            codex_sessions: Some(home.join(".codex/sessions")),
            cursor_workspace_storage: dirs::config_dir()
                .map(|c| c.join("Cursor/user/workspaceStorage")),
        }
    }
}

/// 真实 home 上的便捷入口
pub fn discover_workspace_roots(max: usize) -> Vec<DiscoveredRoot> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let paths = DiscoverPaths::real(&home);
    discover_from(&paths, &home, max)
}

pub fn discover_from(paths: &DiscoverPaths, home: &Path, max: usize) -> Vec<DiscoveredRoot> {
    let mut out: Vec<DiscoveredRoot> = Vec::new();
    if let Some(dir) = &paths.claude_projects {
        out.extend(claude_projects(dir, home));
    }
    if let Some(dir) = &paths.codex_sessions {
        out.extend(codex_cwds(dir, home, max * 4));
    }
    if let Some(dir) = &paths.cursor_workspace_storage {
        out.extend(cursor_workspaces(dir, home));
    }
    // 去重（保首见顺序）
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.path.clone()));
    out.truncate(max);
    out
}

/// 深度护栏：/ 与 home 本身不算工作区；太浅的（如 /tmp、/Users）也不算
fn plausible_root(p: &Path, home: &Path) -> bool {
    if p == home || p == Path::new("/") {
        return false;
    }
    p.components().count() >= 3 && p.is_dir()
}

// ────────────────── Claude Code：目录名解码 ──────────────────

/// `-Users-zc-Documents-foo` → `/Users/zc/Documents/foo`
fn demunge(name: &str) -> PathBuf {
    let s = name.strip_prefix('-').unwrap_or(name);
    PathBuf::from(format!("/{}", s.replace('-', "/")))
}

fn claude_projects(dir: &Path, home: &Path) -> Vec<DiscoveredRoot> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(name) = ent.file_name().into_string().ok() else {
            continue;
        };
        if name == "-" || name.is_empty() {
            continue;
        }
        let p = demunge(&name);
        if plausible_root(&p, home) {
            out.push(DiscoveredRoot {
                path: p,
                source: "claude-code",
            });
        }
    }
    out
}

// ────────────────── Codex：sessions jsonl 头部 cwd ──────────────────

/// 从文本片段中提取 `"field":"..."` 的值（处理常见 JSON 转义，含 \uXXXX；
/// 按 char 推进以保住中文等多字节路径）
fn extract_str_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    let chars: Vec<char> = rest.chars().collect();
    if chars.first() != Some(&'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            return Some(out);
        }
        if c == '\\' {
            match *chars.get(i + 1)? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars[i + 2..(i + 6).min(chars.len())].iter().collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        out.push(ch);
                    }
                    i += 6;
                    continue;
                }
                other => out.push(other),
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    None
}

fn codex_cwds(dir: &Path, home: &Path, max_files: usize) -> Vec<DiscoveredRoot> {
    // 收集 jsonl（按 mtime 新→旧，最多 max_files 个），只读每个文件头 4KB
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_jsonl(dir, &mut files, 0);
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut out = Vec::new();
    for (_, f) in files.into_iter().take(max_files) {
        let Ok(mut fh) = fs::File::open(&f) else { continue };
        use std::io::Read;
        let mut buf = vec![0u8; 4096];
        let n = fh.read(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]);
        if let Some(cwd) = extract_str_field(&head, "cwd") {
            let p = PathBuf::from(&cwd);
            if plausible_root(&p, home) {
                out.push(DiscoveredRoot {
                    path: p,
                    source: "codex",
                });
            }
        }
    }
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<(std::time::SystemTime, PathBuf)>, depth: usize) {
    if depth > 5 || out.len() > 4096 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let Ok(ft) = ent.file_type() else { continue };
        let mtime = ent
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if ft.is_dir() {
            collect_jsonl(&ent.path(), out, depth + 1);
        } else if ent.path().extension().is_some_and(|e| e == "jsonl") {
            out.push((mtime, ent.path()));
        }
    }
}

// ────────────────── Cursor：workspace.json folder URI ──────────────────

/// `file:///Users/zc/x%20y` → `/Users/zc/x y`
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

fn cursor_workspaces(dir: &Path, home: &Path) -> Vec<DiscoveredRoot> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let ws = ent.path().join("workspace.json");
        let Ok(text) = fs::read_to_string(&ws) else { continue };
        if let Some(uri) = extract_str_field(&text, "folder") {
            if let Some(p) = file_uri_to_path(&uri) {
                if plausible_root(&p, home) {
                    out.push(DiscoveredRoot {
                        path: p,
                        source: "cursor",
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(p: &Path) {
        fs::create_dir_all(p).unwrap();
    }

    #[test]
    fn claude_demunge_and_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let real = home.join("Documents/projA");
        mk(&real);
        let cp = home.join(".claude/projects");
        // munge 名从真实路径生成（`/` → `-`）
        let munged = format!("-{}", real.display().to_string().trim_start_matches('/').replace('/', "-"));
        mk(&cp.join(&munged)); // 能解码且存在
        mk(&cp.join("-Users-zc-Documents-gone")); // 不存在 → 跳过
        mk(&cp.join("weird")); // 非常规名（→ /weird，深度 1）→ 跳过

        let roots = claude_projects(&cp, &home);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, real);
        assert_eq!(roots[0].source, "claude-code");
    }

    #[test]
    fn codex_cwd_extraction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let proj = home.join("code/projB");
        mk(&proj);
        let sess = home.join(".codex/sessions/2026/10/21");
        mk(&sess);
        fs::write(
            sess.join("rollout-1.jsonl"),
            r#"{"session_id":"a","payload":{"cwd":"/__HOME__/code/projB","originator":"codex"}}"#.replace("__HOME__", &home.display().to_string()),
        )
        .unwrap();
        fs::write(
            sess.join("rollout-2.jsonl"),
            "{\"no_cwd\":true}",
        )
        .unwrap();

        let roots = codex_cwds(&home.join(".codex/sessions"), &home, 100);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, proj);
        assert_eq!(roots[0].source, "codex");
    }

    #[test]
    fn cursor_folder_uri() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let proj = home.join("Documents/my proj");
        mk(&proj);
        let ws = home.join("Cursor/user/workspaceStorage/abc");
        mk(&ws);
        fs::write(
            ws.join("workspace.json"),
            format!("{{\"folder\":\"file://{}\"}}", proj.display().to_string().replace(' ', "%20")),
        )
        .unwrap();

        let roots = cursor_workspaces(&home.join("Cursor/user/workspaceStorage"), &home);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, proj);
        assert_eq!(roots[0].source, "cursor");
    }

    #[test]
    fn discover_dedup_and_cap() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        let proj = home.join("Documents/projC");
        mk(&proj);
        // claude 与 cursor 指向同一项目 → 去重保首见
        mk(&home.join(".claude/projects/-Users-zc-Documents-projC"));
        let ws = home.join("Cursor/user/workspaceStorage/x");
        mk(&ws);
        fs::write(
            ws.join("workspace.json"),
            format!("{{\"folder\":\"file://{}\"}}", proj.display()),
        )
        .unwrap();
        let paths = DiscoverPaths {
            claude_projects: Some(home.join(".claude/projects")),
            codex_sessions: None,
            cursor_workspace_storage: Some(home.join("Cursor/user/workspaceStorage")),
        };
        let roots = discover_from(&paths, &home, 50);
        assert_eq!(roots.len(), 1, "同路径跨源去重");
        // cap 生效
        let capped = discover_from(&paths, &home, 0);
        assert!(capped.is_empty());
    }

    #[test]
    fn home_itself_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        // claude 记录过 home 本身（如 ~/.claude 会话打开 home）→ 不算工作区
        let cp = home.join(".claude/projects");
        let munged_home = format!(
            "-{}",
            home.display().to_string().trim_start_matches('/').replace('/', "-")
        );
        mk(&cp.join(munged_home));
        assert!(claude_projects(&cp, &home).is_empty());
    }

    #[test]
    fn str_field_escapes() {
        let s = r#"{"cwd":"/a/b\"q\"","x":1}"#;
        assert_eq!(extract_str_field(s, "cwd").as_deref(), Some(r#"/a/b"q""#));
        assert_eq!(extract_str_field(s, "nope"), None);
        // \uXXXX 转义
        let u = r#"{"cwd":"/\u4e2d\u6587","x":1}"#;
        assert_eq!(extract_str_field(u, "cwd").as_deref(), Some("/中文"));
    }
}

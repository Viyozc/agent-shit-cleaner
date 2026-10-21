//! golden 测试：用临时目录树验证扫描器行为
//! 覆盖：清单启发式、剪枝、.git 跳过、嵌套不重复计、符号链接跳过、
//! NEVER 过滤、home 规则（pattern / 整目录）、过期判定、花括号展开

use asc_core::never::is_never;
use asc_core::rule::{builtin_rules, expand_braces, parse_rules, wild_match};
use asc_core::scanner::{scan_home, scan_workspace, WorkspaceOpts};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tempfile::TempDir;

// ────────────── 工具 ──────────────

fn mkdir(p: &Path) {
    fs::create_dir_all(p).unwrap();
}

fn write(p: &Path, bytes: &[u8]) {
    mkdir(p.parent().unwrap());
    fs::write(p, bytes).unwrap();
}

/// 找到指定规则 id 的条目集合
fn items_of<'a>(res: &'a asc_core::scanner::ScanResult, rule_id: &str) -> Vec<&'a asc_core::scanner::JunkItem> {
    res.items.iter().filter(|i| i.rule_id == rule_id).collect()
}

/// 构造典型项目垃圾树
fn fixture_workspace(root: &Path) {
    // a: 正经 JS 项目
    write(&root.join("a/package.json"), b"{}");
    write(&root.join("a/src/main.js"), b"console.log(1)");
    write(&root.join("a/node_modules/express/index.js"), &[0u8; 100]);
    write(&root.join("a/node_modules/lodash/index.js"), &[0u8; 50]);
    // b: node_modules 但无 package.json（应标 ⚠）
    write(&root.join("b/node_modules/x/index.js"), &[0u8; 30]);
    // c: Rust 项目
    write(&root.join("c/Cargo.toml"), b"[package]");
    write(&root.join("c/target/debug/c"), &[0u8; 200]);
    // d: __pycache__（无清单要求）
    write(&root.join("d/__pycache__/m.cpython-312.pyc"), &[0u8; 10]);
    // e: .next + package.json
    write(&root.join("e/package.json"), b"{}");
    write(&root.join("e/.next/cache/1.bin"), &[0u8; 80]);
    // f: .git 内的 node_modules 必须被跳过
    write(&root.join("f/.git/node_modules/evil.js"), &[0u8; 999]);
    // g: 嵌套 node_modules 只计外层
    write(&root.join("g/package.json"), b"{}");
    write(&root.join("g/node_modules/inner/node_modules/deep/i.js"), &[0u8; 40]);
    // h: venv + requirements.txt
    write(&root.join("h/requirements.txt"), b"flask");
    write(&root.join("h/venv/lib/python/site.py"), &[0u8; 60]);
    // i: venv 无清单 → ⚠
    write(&root.join("i/venv/lib/x.py"), &[0u8; 5]);
    // j: 无关项目，不应命中任何规则
    write(&root.join("j/README.md"), b"hello");
    // k: target 无 Cargo.toml → ⚠
    write(&root.join("k/target/debug/x"), &[0u8; 70]);
    // 符号链接目录（指向 a/node_modules）应被跳过
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("a/node_modules"), root.join("j/node_modules")).unwrap();
}

fn ws_rules() -> Vec<asc_core::rule::Rule> {
    builtin_rules()
        .into_iter()
        .filter(|r| r.scope == asc_core::rule::Scope::Workspace)
        .collect()
}

// ────────────── 工作区扫描 ──────────────

#[test]
fn workspace_finds_and_classifies() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fixture_workspace(&root);

    let results = scan_workspace(std::slice::from_ref(&root), &ws_rules(), &WorkspaceOpts::default());
    let res = &results[0];

    let nm = items_of(res, "workspace/node-modules");
    let paths: Vec<&Path> = nm.iter().map(|i| i.path.as_path()).collect();
    assert_eq!(nm.len(), 3, "a/b/g 三处 node_modules: {paths:?}");
    assert!(nm.iter().any(|i| i.path.ends_with("a/node_modules") && !i.no_manifest));
    assert!(nm.iter().any(|i| i.path.ends_with("b/node_modules") && i.no_manifest), "无 package.json 应标 ⚠");
    assert!(nm.iter().any(|i| i.path.ends_with("g/node_modules") && !i.no_manifest));

    let a_nm = nm.iter().find(|i| i.path.ends_with("a/node_modules")).unwrap();
    assert_eq!(a_nm.size, 150, "express 100 + lodash 50");
    assert!(a_nm.mtime.is_some());

    let target = items_of(res, "workspace/rust-target");
    assert_eq!(target.len(), 2, "c 与 k 的 target");
    assert!(target.iter().any(|i| i.path.ends_with("c/target") && !i.no_manifest));
    assert!(target.iter().any(|i| i.path.ends_with("k/target") && i.no_manifest), "无 Cargo.toml 应标 ⚠");

    let pyc = items_of(res, "workspace/pycache");
    assert_eq!(pyc.len(), 1);
    assert_eq!(pyc[0].size, 10);

    let next = items_of(res, "workspace/js-build");
    assert_eq!(next.len(), 1, "只有 e/.next");
    assert_eq!(next[0].size, 80);

    let venv = items_of(res, "workspace/python-venv");
    assert_eq!(venv.len(), 2, "h 与 i 的 venv");
    assert!(venv.iter().any(|i| i.path.ends_with("h/venv") && !i.no_manifest));
    assert!(venv.iter().any(|i| i.path.ends_with("i/venv") && i.no_manifest));

    // .git 内的不出现；j 的符号链接不算；j 本身无命中
    assert!(
        !res.items.iter().any(|i| i.path.to_string_lossy().contains(".git")),
        ".git 永不进入"
    );
    assert!(!res.items.iter().any(|i| i.path.ends_with("j/node_modules")), "符号链接跳过");
    assert!(res.items.iter().all(|i| !i.path.ends_with("j")), "j 不该有任何命中");
}

#[test]
fn workspace_nested_not_double_counted() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(&root.join("g/package.json"), b"{}");
    // 外层 40 + 内层 60；剪枝后只计外层目录整体 100
    write(&root.join("g/node_modules/outer.js"), &[0u8; 40]);
    write(&root.join("g/node_modules/inner/node_modules/deep.js"), &[0u8; 60]);
    let res = &scan_workspace(&[root], &ws_rules(), &WorkspaceOpts::default())[0];
    let nm = items_of(res, "workspace/node-modules");
    assert_eq!(nm.len(), 1);
    assert_eq!(nm[0].size, 100, "外层整体计 100，内层不单独立项");
}

// ────────────── 大本营扫描 ──────────────

const HOME_RULES: &str = r#"
[[rule]]
id = "test/transcripts"
scope = "home"
agent = "test-agent"
path = "~/.fake/projects"
pattern = "*.jsonl"
category = "sessions"
risk = "retention"
keep_days = 0
description = "转录"

[[rule]]
id = "test/cache"
scope = "home"
agent = "test-agent"
path = "~/.fake/cache"
category = "caches"
risk = "safe"
keep_days = 30

[[rule]]
id = "test/ext"
scope = "home"
agent = "test-agent"
path = "~/.fake/extensions"
category = "packages"
risk = "redownload"
"#;

#[test]
fn home_scan_patterns_dirs_and_expiry() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();

    write(&home.join(".fake/projects/p1/s1.jsonl"), &[0u8; 100]);
    write(&home.join(".fake/projects/p1/s2.jsonl"), &[0u8; 50]);
    write(&home.join(".fake/projects/p1/notes.txt"), &[0u8; 500], ); // 不匹配 pattern
    // 整目录规则：两个子目录 + 一个散文件
    write(&home.join(".fake/cache/one/x.bin"), &[0u8; 10]);
    write(&home.join(".fake/cache/two/y.bin"), &[0u8; 20]);
    write(&home.join(".fake/cache/loose.bin"), &[0u8; 5]);
    // redownload 整目录
    write(&home.join(".fake/extensions/ext-a/lib.js"), &[0u8; 1000]);

    let rules = parse_rules(HOME_RULES).unwrap();
    let res = scan_home(Some(&home), &rules);

    let tr = items_of(&res, "test/transcripts");
    assert_eq!(tr.len(), 2, "只有两个 jsonl");
    assert_eq!(tr.iter().map(|i| i.size).sum::<u64>(), 150);
    assert!(tr.iter().all(|i| i.expired), "keep_days=0 全部过期");

    let cache = items_of(&res, "test/cache");
    assert_eq!(cache.len(), 3, "直接子项为单位: two dirs + loose.bin");
    assert_eq!(cache.iter().map(|i| i.size).sum::<u64>(), 35);
    assert!(cache.iter().all(|i| i.reclaimable()), "safe 恒可回收");

    let ext = items_of(&res, "test/ext");
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0].size, 1000);
    assert!(!ext[0].reclaimable(), "redownload 不进保守可回收口径");
}

#[test]
fn home_never_paths_filtered() {
    // home 扫描结果里若混入 NEVER 路径（如 skills 目录名为项），应被丢弃
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    write(&home.join(".fake/projects/skills/x.jsonl"), &[0u8; 10]); // 命中 NEVER(skills)
    write(&home.join(".fake/projects/normal.jsonl"), &[0u8; 10]);
    let rules = parse_rules(HOME_RULES).unwrap();
    let res = scan_home(Some(&home), &rules);
    let tr = items_of(&res, "test/transcripts");
    assert_eq!(tr.len(), 1);
    assert!(tr[0].path.ends_with("normal.jsonl"));
    assert_eq!(res.never_dropped, 1);
}

#[test]
fn builtin_rules_parse() {
    let rules = builtin_rules();
    assert!(rules.len() >= 25, "内置规则数量: {}", rules.len());
    // id 唯一
    let mut ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
    ids.sort();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "规则 id 必须唯一");
    // workspace 规则全部有 match_dir
    assert!(rules
        .iter()
        .filter(|r| r.scope == asc_core::rule::Scope::Workspace)
        .all(|r| r.match_dir.is_some()));
}

// ────────────── 纯函数 ──────────────

#[test]
fn wild_matching() {
    assert!(wild_match("*.jsonl", "abc.jsonl"));
    assert!(wild_match("logs_*.sqlite", "logs_2.sqlite"));
    assert!(!wild_match("*.jsonl", "abc.txt"));
    assert!(wild_match("a?c", "abc"));
    assert!(wild_match("*", "anything"));
    assert!(!wild_match("abc", "abcd"));
}

#[test]
fn brace_expansion() {
    assert_eq!(expand_braces("{.venv,venv}"), vec![".venv", "venv"]);
    assert_eq!(expand_braces("node_modules"), vec!["node_modules"]);
    let js = expand_braces("{.next,.nuxt,.turbo,.gradle}");
    assert_eq!(js.len(), 4);
    assert!(js.contains(&".next".to_string()));
    assert!(expand_braces("a{b,c}d").contains(&"abd".to_string()));
    assert!(expand_braces("a{b,c}d").contains(&"acd".to_string()));
}

#[test]
fn never_on_workspace_paths() {
    assert!(!is_never(Path::new("/home/u/Documents/x/node_modules")));
    assert!(is_never(Path::new("/home/u/Documents/x/hooks/node_modules")), "hooks 组件命中 NEVER");
    assert!(is_never(Path::new("/home/u/Documents/x/y/Cargo.lock")));
}

// ────────────── M2：活跃判定 / Tier B / 用户规则 ──────────────

#[test]
fn workspace_active_project_detection() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // live：src 刚写（活跃）
    write(&root.join("live/package.json"), b"{}");
    write(&root.join("live/src/main.js"), b"x");
    write(&root.join("live/node_modules/i.js"), &[0u8; 10]);
    // dead：所有源码 mtime 拨回 90 天前（不活跃）
    write(&root.join("dead/package.json"), b"{}");
    write(&root.join("dead/src/main.js"), b"x");
    write(&root.join("dead/node_modules/i.js"), &[0u8; 10]);
    let old = SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
    let ft = filetime::FileTime::from_system_time(old);
    for p in ["dead/package.json", "dead/src/main.js"] {
        filetime::set_file_times(root.join(p), ft, ft).unwrap();
    }
    // junk mtime 本身新旧不影响判定（dead 的 node_modules 也拨新也没用——看的是源码）
    let opts = WorkspaceOpts {
        active_days: 30,
        ignore: vec![],
    };
    let res = &scan_workspace(std::slice::from_ref(&root), &ws_rules(), &opts)[0];
    let live = res
        .items
        .iter()
        .find(|i| i.path.ends_with("live/node_modules"))
        .unwrap();
    let dead = res
        .items
        .iter()
        .find(|i| i.path.ends_with("dead/node_modules"))
        .unwrap();
    assert_eq!(live.active, Some(true), "源码新鲜 → 项目活跃");
    assert_eq!(dead.active, Some(false), "源码 90 天没动 → 死项目");
}

#[test]
fn workspace_ignore_dirs_skipped() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    write(&root.join("normal/package.json"), b"{}");
    write(&root.join("normal/node_modules/i.js"), &[0u8; 10]);
    write(&root.join("vendor/package.json"), b"{}");
    write(&root.join("vendor/node_modules/i.js"), &[0u8; 10]);
    let opts = WorkspaceOpts {
        active_days: 30,
        ignore: vec!["vendor".into()],
    };
    let res = &scan_workspace(std::slice::from_ref(&root), &ws_rules(), &opts)[0];
    assert!(res
        .items
        .iter()
        .any(|i| i.path.ends_with("normal/node_modules")));
    assert!(!res
        .items
        .iter()
        .any(|i| i.path.to_string_lossy().contains("vendor")), "ignore 目录整体跳过");
}

#[test]
fn tier_b_rules_gated_by_config() {
    use asc_core::config::Config;
    use asc_core::rule::load_rules;
    let no_user: Vec<(PathBuf, String)> = Vec::new();

    let cfg_off = Config::default(); // tier_b = false
    let (rules_off, warns) = load_rules(&cfg_off, &no_user);
    assert!(warns.is_empty());
    assert!(!rules_off.iter().any(|r| r.id == "workspace/dist"), "Tier B 默认关闭");
    assert!(rules_off.iter().any(|r| r.id == "workspace/node-modules"), "Tier A 不受影响");

    let cfg_on = Config {
        tier_b: true,
        ..Default::default()
    };
    let (rules_on, _) = load_rules(&cfg_on, &no_user);
    for id in ["workspace/dist", "workspace/build", "workspace/out", "workspace/coverage", "workspace/dot-cache"] {
        assert!(rules_on.iter().any(|r| r.id == id), "开启后 {id} 生效");
    }
}

#[test]
fn user_rules_override_and_append() {
    use asc_core::config::Config;
    use asc_core::rule::load_rules;
    // 覆盖内置：node_modules 保留期改为 5（其实它是 redownload，这里只验证覆盖机制）
    // 追加新规则：mycorp 目录
    let user = vec![(
        PathBuf::from("/fake/rules.d/mine.toml"),
        r#"
[[rule]]
id = "workspace/node-modules"
scope = "workspace"
match_dir = "node_modules"
category = "temp"
risk = "safe"
description = "用户改写"
"#
        .to_string(),
    )];
    let cfg = Config::default();
    let (rules, warns) = load_rules(&cfg, &user);
    assert!(warns.is_empty());
    let nm = rules.iter().find(|r| r.id == "workspace/node-modules").unwrap();
    assert_eq!(nm.risk, asc_core::rule::Risk::Safe, "同 id 用户规则覆盖内置");

    // 坏 TOML → 警告且不阻断
    let bad = vec![(PathBuf::from("/fake/bad.toml"), "not = a toml [[".to_string())];
    let (rules2, warns2) = load_rules(&cfg, &bad);
    assert!(!warns2.is_empty());
    assert!(!rules2.is_empty(), "坏文件跳过，内置规则照常生效");
}

#[test]
fn keep_days_override_applied() {
    use asc_core::config::Config;
    use asc_core::rule::load_rules;
    let mut cfg = Config::default();
    cfg.keep_days
        .insert("claude-code/sessions".into(), 7);
    let (rules, _) = load_rules(&cfg, &[]);
    let s = rules
        .iter()
        .find(|r| r.id == "claude-code/sessions")
        .unwrap();
    assert_eq!(s.keep_days, Some(7), "config keep_days 覆盖内置保留期");
}

// ────────────── 报表聚合冒烟 ──────────────

#[test]
fn report_aggregation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    fixture_workspace(&root);
    let res = &scan_workspace(&[root], &ws_rules(), &WorkspaceOpts::default())[0];
    let rep = asc_core::report::build_workspace_report(&PathBuf::from("/tmp/x"), res, 3);
    assert!(!rep.groups.is_empty());
    assert_eq!(rep.count, rep.groups.iter().map(|g| g.count).sum::<usize>());
    assert_eq!(rep.total, rep.groups.iter().map(|g| g.total).sum::<u64>());
    assert!(rep.top.len() <= 3);
    assert!(rep.no_manifest_total >= 3);
    // 分组按体积降序
    let totals: Vec<u64> = rep.groups.iter().map(|g| g.total).collect();
    let mut sorted = totals.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(totals, sorted);
}

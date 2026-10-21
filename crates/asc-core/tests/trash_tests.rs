//! 回收站 golden 测试：存入/列出/恢复/冲突/过期清理（全部 tempdir，不碰真实 home）

use asc_core::trash;
use std::fs;
use tempfile::TempDir;

fn mkfile(p: &std::path::Path, bytes: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![0u8; bytes]).unwrap();
}

#[test]
fn store_list_restore_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let origin1 = root.join("proj/node_modules");
    let origin2 = root.join("proj/cache.log");
    mkfile(&origin1.join("a.js"), 100);
    mkfile(&origin2, 50);

    let mut run = trash::new_run();
    trash::store(&mut run, &root, &origin1, "workspace/node-modules", 100).unwrap();
    trash::store(&mut run, &root, &origin2, "test/logs", 50).unwrap();
    trash::write_manifest(&run, &root).unwrap();

    // 已移走
    assert!(!origin1.exists());
    assert!(!origin2.exists());

    let runs = trash::list(&root);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, run.run_id);
    assert_eq!(runs[0].entries.len(), 2);
    assert_eq!(runs[0].entries[0].origin, origin1);

    let n = trash::restore(&run.run_id, &root).unwrap();
    assert_eq!(n, 2);
    assert!(origin1.join("a.js").exists(), "目录完整回到原位");
    assert_eq!(fs::read(&origin2).unwrap().len(), 50);
    // run 目录随恢复清空
    let runs = trash::list(&root);
    assert!(runs.is_empty());
}

#[test]
fn restore_collision_gets_suffix() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let origin = root.join("x/node_modules");
    mkfile(&origin.join("old"), 10);

    let mut run = trash::new_run();
    trash::store(&mut run, &root, &origin, "workspace/node-modules", 10).unwrap();
    trash::write_manifest(&run, &root).unwrap();

    // 原位又被占了
    mkfile(&origin.join("new"), 20);
    trash::restore(&run.run_id, &root).unwrap();
    assert!(origin.join("new").exists(), "新内容不被覆盖");
    let restored: Vec<_> = fs::read_dir(root.join("x"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("node_modules.restored-"))
        .collect();
    assert_eq!(restored.len(), 1, "旧内容落到 .restored-<id>");
}

#[test]
fn purge_expired_only() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // 手工构造：老 run（8 天前）与新 run（现在）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (id, age_days) in [("old-run", 8u64), ("new-run", 0)] {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        mkfile(&dir.join("0001-junk"), 42);
        let run = asc_core::trash::TrashRun {
            run_id: id.into(),
            created: now - age_days * 86_400,
            entries: vec![asc_core::trash::TrashEntry {
                seq: 1,
                origin: root.join("gone"),
                rule_id: "t".into(),
                size: 42,
                stored_name: "0001-junk".into(),
            }],
        };
        fs::write(
            dir.join("manifest.toml"),
            toml::to_string(&run).unwrap(),
        )
        .unwrap();
    }

    let (n, freed) = trash::purge(&root, true, 7).unwrap();
    assert_eq!((n, freed), (1, 42), "只清过期的 old-run");
    assert!(root.join("new-run").exists());

    let (n2, _) = trash::purge(&root, false, 7).unwrap();
    assert_eq!(n2, 1, "全量清空剩余");
    assert!(trash::list(&root).is_empty());
}

#[test]
fn clean_plan_threshold_boundary() {
    use asc_core::clean::{make_plan, Dest, SHRED_THRESHOLD};
    use asc_core::rule::{Category, Risk};
    use asc_core::scanner::JunkItem;
    let item = |size| JunkItem {
        rule_id: "t".into(),
        agent: None,
        category: Category::Temp,
        risk: Risk::Safe,
        path: std::path::PathBuf::from("/tmp/x"),
        size,
        mtime: None,
        expired: true,
        no_manifest: false,
        active: None,
    };
    let plan = make_plan(vec![item(SHRED_THRESHOLD), item(SHRED_THRESHOLD + 1)], true);
    assert_eq!(plan[0].dest, Dest::Trash, "恰好 1GB 不算大件");
    assert_eq!(plan[1].dest, Dest::Shred, "超过 1GB 才粉碎");
}

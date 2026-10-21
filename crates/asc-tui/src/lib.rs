//! agent-shit-cleaner TUI：仪表盘 → 详情勾选 → 确认（大件粉碎需键入 DELETE）→ 进度 → 结果。
//! 清理执行在后台线程，界面永不阻塞。

mod ui;

use asc_core::clean::{execute, make_plan, PlannedItem};
use asc_core::config::LoadedConfig;
use asc_core::report::scan_bundle_with;
use asc_core::rule::{load_rules, Risk};
use asc_core::scanner::{JunkItem, WorkspaceOpts};
use asc_core::trash;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Terminal;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

pub struct TuiOptions {
    /// 工作区根目录（CLI --root + config [[workspace]] 合并结果）
    pub workspace_roots: Vec<PathBuf>,
    /// 用户配置（keep_days 覆盖 / tier_b / 活跃窗口）
    pub loaded: LoadedConfig,
}

// ────────────────── 数据结构 ──────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dashboard,
    Detail,
    Confirm,
    Shred,
    Cleaning,
    Result,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Home,
    Workspace,
}

pub struct GroupDef {
    pub label: String,
    pub sub: String,
    pub kind: GroupKind,
    pub total: u64,
    pub reclaimable: u64,
    pub redownload: u64,
    pub count: usize,
    /// 该组条目在 items 里的下标
    pub item_idx: Vec<usize>,
}

pub struct ScanData {
    pub items: Vec<JunkItem>,
    pub groups: Vec<GroupDef>,
    pub home_total: u64,
    pub home_reclaim: u64,
    pub ws_roots: Vec<String>,
    pub ws_total: u64,
}

fn build_scan_data(bundle: asc_core::report::ScanBundle) -> ScanData {
    let mut items = bundle.home_res.items.clone();
    let mut groups: Vec<GroupDef> = Vec::new();

    // 大本营按 agent 分组
    let mut order: Vec<String> = Vec::new();
    let mut by_agent: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        let agent = it.agent.clone().unwrap_or_else(|| "?".into());
        by_agent.entry(agent.clone()).or_default().push(i);
        if !order.contains(&agent) {
            order.push(agent);
        }
    }
    for agent in order {
        let idxs = by_agent.remove(&agent).unwrap_or_default();
        push_group(
            &mut groups,
            agent.clone(),
            "agent 大本营".into(),
            GroupKind::Home,
            &items,
            idxs,
        );
    }

    // 工作区按 (root, rule) 分组
    let mut ws_total = 0u64;
    let mut ws_roots = Vec::new();
    for (root, res) in &bundle.ws_results {
        ws_roots.push(root.display().to_string());
        let mut by_rule: std::collections::HashMap<String, Vec<usize>> =
            std::collections::HashMap::new();
        let mut rule_order: Vec<String> = Vec::new();
        for it in &res.items {
            let base = items.len();
            items.push(it.clone());
            if !rule_order.contains(&it.rule_id) {
                rule_order.push(it.rule_id.clone());
            }
            by_rule
                .entry(it.rule_id.clone())
                .or_default()
                .push(base);
        }
        let root_label = shorten(root);
        for rid in rule_order {
            let idxs = by_rule.remove(&rid).unwrap_or_default();
            let name = idxs
                .first()
                .and_then(|i| items[*i].path.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            push_group(
                &mut groups,
                format!("{name} ×{}", idxs.len()),
                root_label.clone(),
                GroupKind::Workspace,
                &items,
                idxs,
            );
        }
        ws_total += res.items.iter().map(|i| i.size).sum::<u64>();
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.total));
    ScanData {
        home_total: bundle.home.total,
        home_reclaim: bundle.home.reclaimable,
        ws_roots,
        ws_total,
        items,
        groups,
    }
}

fn push_group(
    groups: &mut Vec<GroupDef>,
    label: String,
    sub: String,
    kind: GroupKind,
    items: &[JunkItem],
    idxs: Vec<usize>,
) {
    let total: u64 = idxs.iter().map(|&i| items[i].size).sum();
    let reclaimable: u64 = idxs
        .iter()
        .filter(|&&i| items[i].reclaimable())
        .map(|&i| items[i].size)
        .sum();
    let redownload: u64 = idxs
        .iter()
        .filter(|&&i| items[i].risk == Risk::Redownload)
        .map(|&i| items[i].size)
        .sum();
    groups.push(GroupDef {
        label,
        sub,
        kind,
        total,
        reclaimable,
        redownload,
        count: idxs.len(),
        item_idx: idxs,
    });
}

// ────────────────── 消息与线程 ──────────────────

enum Msg {
    ScanDone(Box<ScanData>),
    CleanProgress(usize, usize),
    CleanDone(Box<asc_core::clean::Outcome>),
}

fn spawn_scan(tx: mpsc::Sender<Msg>, roots: Vec<PathBuf>, loaded: LoadedConfig) {
    thread::spawn(move || {
        let (rules, _warn) = load_rules(&loaded.config, &asc_core::Config::user_rule_texts());
        let ws_opts = WorkspaceOpts {
            active_days: loaded.config.active_days,
            ignore: loaded
                .config
                .workspace_roots()
                .iter()
                .flat_map(|(_, ig)| ig.iter().cloned())
                .collect(),
        };
        let bundle = scan_bundle_with(&roots, None, &rules, &ws_opts);
        let _ = tx.send(Msg::ScanDone(Box::new(build_scan_data(bundle))));
    });
}

fn spawn_clean(tx: mpsc::Sender<Msg>, plan: Vec<PlannedItem>) {
    thread::spawn(move || {
        let trash_root = trash::trash_root();
        let t = tx.clone();
        let out = execute(&plan, &trash_root, &|d, n, _| {
            let _ = t.send(Msg::CleanProgress(d, n));
        });
        let _ = tx.send(Msg::CleanDone(Box::new(out)));
    });
}

// ────────────────── App 状态 ──────────────────

struct DetailState {
    group: usize,
    cursor: usize,
    /// 组内条目下标（按体积降序）
    order: Vec<usize>,
}

struct App {
    mode: Mode,
    opts_root_label: Option<String>,
    items: Vec<JunkItem>,
    groups: Vec<GroupDef>,
    cursor: usize,
    detail: Option<DetailState>,
    selected: BTreeSet<usize>,
    scanning: bool,
    spinner: usize,
    scan_start: Instant,
    home_total: u64,
    home_reclaim: u64,
    ws_total: u64,
    progress: Option<(usize, usize)>,
    outcome: Option<asc_core::clean::Outcome>,
    note: String,
    shred_input: String,
    pending_selected: Vec<usize>,
    quit: bool,
    /// 重扫（r / 清理后）所需：消息通道、工作区根、配置
    tx: Option<mpsc::Sender<Msg>>,
    scan_roots: Vec<PathBuf>,
    loaded: Option<LoadedConfig>,
}

enum Action {
    Clean(Vec<PlannedItem>),
}

impl App {
    fn new(root_label: Option<String>) -> Self {
        App {
            mode: Mode::Dashboard,
            opts_root_label: root_label,
            items: Vec::new(),
            groups: Vec::new(),
            cursor: 0,
            detail: None,
            selected: BTreeSet::new(),
            scanning: true,
            spinner: 0,
            scan_start: Instant::now(),
            home_total: 0,
            home_reclaim: 0,
            ws_total: 0,
            progress: None,
            outcome: None,
            note: String::new(),
            shred_input: String::new(),
            pending_selected: Vec::new(),
            quit: false,
            tx: None,
            scan_roots: Vec::new(),
            loaded: None,
        }
    }

    fn on_scan_done(&mut self, d: ScanData) {
        self.items = d.items;
        self.groups = d.groups;
        self.home_total = d.home_total;
        self.home_reclaim = d.home_reclaim;
        self.ws_total = d.ws_total;
        self.scanning = false;
        self.selected.clear();
        self.cursor = 0;
        self.mode = Mode::Dashboard;
    }

    fn on_clean_done(&mut self, o: asc_core::clean::Outcome) {
        self.outcome = Some(o);
        self.selected.clear();
        self.progress = None;
        self.mode = Mode::Result;
    }

    fn selected_items(&self) -> Vec<JunkItem> {
        self.selected
            .iter()
            .filter_map(|i| self.items.get(*i).cloned())
            .collect()
    }

    /// 组内按体积降序的下标
    fn group_order(&self, g: &GroupDef) -> Vec<usize> {
        let mut v = g.item_idx.clone();
        v.sort_by_key(|&i| std::cmp::Reverse(self.items[i].size));
        v
    }

    /// 默认勾选口径：可回收 且 不在活跃项目里（活跃项仍可在详情 space 显式勾选）
    fn default_pickable(&self, i: usize) -> bool {
        let it = &self.items[i];
        it.reclaimable() && it.active != Some(true)
    }

    fn toggle_group(&mut self) {
        let Some(g) = self.groups.get(self.cursor) else { return };
        let rec: Vec<usize> = g
            .item_idx
            .iter()
            .copied()
            .filter(|&i| self.default_pickable(i))
            .collect();
        if rec.is_empty() {
            self.note = "该组没有默认可回收项（可进入详情用 space 显式勾选）".into();
            return;
        }
        if rec.iter().all(|i| self.selected.contains(i)) {
            for i in rec {
                self.selected.remove(&i);
            }
        } else {
            for i in rec {
                self.selected.insert(i);
            }
        }
    }

    fn start_confirm(&mut self) -> Option<Action> {
        if self.selected.is_empty() {
            self.note = "先勾选：space 勾选组 / Enter 进详情逐项勾选".into();
            return None;
        }
        self.pending_selected = self.selected.iter().copied().collect();
        self.mode = Mode::Confirm;
        None
    }

    fn confirm_summary(&self) -> (usize, u64, usize, u64) {
        // (项数, 总字节, 大件数, 大件字节)
        let items = self.selected_items();
        let total: u64 = items.iter().map(|i| i.size).sum();
        let big: Vec<&JunkItem> = items
            .iter()
            .filter(|i| i.size > asc_core::clean::SHRED_THRESHOLD)
            .collect();
        (items.len(), total, big.len(), big.iter().map(|i| i.size).sum())
    }

    fn on_key(&mut self, code: KeyCode) -> Option<Action> {
        self.note.clear();
        match self.mode {
            Mode::Dashboard => match code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.cursor + 1 < self.groups.len() {
                        self.cursor += 1;
                    }
                }
                KeyCode::Char(' ') => self.toggle_group(),
                KeyCode::Enter => {
                    if let Some(g) = self.groups.get(self.cursor) {
                        let order = self.group_order(g);
                        self.detail = Some(DetailState {
                            group: self.cursor,
                            cursor: 0,
                            order,
                        });
                        self.mode = Mode::Detail;
                    }
                }
                KeyCode::Char('c') => return self.start_confirm(),
                KeyCode::Char('?') => self.mode = Mode::Help,
                KeyCode::Char('r') if !self.scanning => self.rescan(),
                _ => {}
            },
            Mode::Detail => {
                if self.detail.is_none() {
                    self.mode = Mode::Dashboard;
                    return None;
                }
                let (group, cursor, order_len) = {
                    let d = self.detail.as_ref().unwrap();
                    (d.group, d.cursor, d.order.len())
                };
                match code {
                    KeyCode::Esc => {
                        self.mode = Mode::Dashboard;
                        self.detail = None;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(d) = self.detail.as_mut() {
                            d.cursor = d.cursor.saturating_sub(1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if cursor + 1 < order_len {
                            if let Some(d) = self.detail.as_mut() {
                                d.cursor += 1;
                            }
                        }
                    }
                    KeyCode::Char(' ') => {
                        let idx = self
                            .detail
                            .as_ref()
                            .and_then(|d| d.order.get(cursor))
                            .copied();
                        if let Some(idx) = idx {
                            if self.selected.contains(&idx) {
                                self.selected.remove(&idx);
                            } else {
                                self.selected.insert(idx);
                            }
                        }
                    }
                    KeyCode::Char('a') => {
                        let idxs: Vec<usize> = self
                            .groups
                            .get(group)
                            .map(|g| {
                                g.item_idx
                                    .iter()
                                    .copied()
                                    .filter(|&i| self.default_pickable(i))
                                    .collect()
                            })
                            .unwrap_or_default();
                        for i in idxs {
                            self.selected.insert(i);
                        }
                    }
                    KeyCode::Char('c') => return self.start_confirm(),
                    _ => {}
                }
            }
            Mode::Confirm => match code {
                KeyCode::Esc => self.mode = Mode::Dashboard,
                KeyCode::Char('s') => {
                    let (_, _, big, _) = self.confirm_summary();
                    if big > 0 {
                        self.shred_input.clear();
                        self.mode = Mode::Shred;
                    } else {
                        self.note = "没有 >1GB 大件，无需粉碎".into();
                    }
                }
                KeyCode::Enter => {
                    let items = self.selected_items();
                    return Some(Action::Clean(make_plan(items, false)));
                }
                _ => {}
            },
            Mode::Shred => match code {
                KeyCode::Esc => self.mode = Mode::Confirm,
                KeyCode::Enter => {
                    if self.shred_input.trim() == "DELETE" {
                        let items = self.selected_items();
                        return Some(Action::Clean(make_plan(items, true)));
                    }
                    self.note = "输入不匹配，请完整输入 DELETE（Esc 返回）".into();
                    self.shred_input.clear();
                }
                KeyCode::Backspace => {
                    self.shred_input.pop();
                }
                KeyCode::Char(c) if self.shred_input.len() < 16 => {
                    self.shred_input.push(c);
                }
                _ => {}
            },
            Mode::Cleaning => {}
            Mode::Result => match code {
                KeyCode::Enter | KeyCode::Esc => self.rescan(),
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
            Mode::Help => {
                self.mode = Mode::Dashboard;
            }
        }
        None
    }

    fn rescan(&mut self) {
        self.scanning = true;
        self.scan_start = Instant::now();
        self.mode = Mode::Dashboard;
        self.detail = None;
        self.selected.clear();
        self.outcome = None;
        if let (Some(tx), Some(loaded)) = (self.tx.clone(), self.loaded.clone()) {
            spawn_scan(tx, self.scan_roots.clone(), loaded);
        }
    }

    fn age_of(&self, idx: usize) -> Option<u64> {
        self.items.get(idx)?.mtime.map(|t| {
            SystemTime::now()
                .duration_since(t)
                .map(|d| d.as_secs() / 86_400)
                .unwrap_or(0)
        })
    }
}

pub fn shorten(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    if let Some(home) = dirs::home_dir() {
        if let Some(rest) = s.strip_prefix(&format!("{}/", home.display())) {
            return format!("~/{rest}");
        }
    }
    s
}

// ────────────────── 事件循环 ──────────────────

pub fn run(opts: TuiOptions) -> i32 {
    let init = std::panic::catch_unwind(ratatui::try_init);
    let mut terminal = match init {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            eprintln!("无法初始化终端: {e}");
            return 1;
        }
        Err(e) => {
            eprintln!("终端初始化 panic: {e:?}");
            return 1;
        }
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop(&mut terminal, opts)
    }));
    ratatui::restore();
    match result {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            eprintln!("TUI 错误: {e}");
            1
        }
        Err(e) => {
            eprintln!("TUI panic: {e:?}");
            1
        }
    }
}

fn event_loop(terminal: &mut Terminal<impl ratatui::backend::Backend>, opts: TuiOptions) -> std::io::Result<i32> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let root_label = opts.workspace_roots.first().map(|p| shorten(p));
    let mut app = App::new(root_label);
    app.tx = Some(tx.clone());
    app.scan_roots = opts.workspace_roots.clone();
    app.loaded = Some(opts.loaded);
    if let (Some(tx), Some(loaded)) = (app.tx.clone(), app.loaded.clone()) {
        spawn_scan(tx, app.scan_roots.clone(), loaded);
    }
    loop {
        terminal.draw(|f| ui::draw(f, &app))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    if let Some(action) = app.on_key(k.code) {
                        match action {
                            Action::Clean(plan) => {
                                app.mode = Mode::Cleaning;
                                app.progress = Some((0, plan.len()));
                                spawn_clean(tx.clone(), plan);
                            }
                        }
                    }
                }
            }
        }
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::ScanDone(d) => app.on_scan_done(*d),
                Msg::CleanProgress(a, b) => app.progress = Some((a, b)),
                Msg::CleanDone(o) => app.on_clean_done(*o),
            }
        }
        app.spinner = app.spinner.wrapping_add(1);
        if app.quit {
            return Ok(0);
        }
    }
}

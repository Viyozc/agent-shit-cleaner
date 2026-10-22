//! ratatui 渲染：仪表盘 / 详情 / 确认 / 粉碎输入 / 进度 / 结果 / 帮助

use crate::{App, GroupKind, Mode};
use asc_core::fmt::human_bytes;
use asc_core::rule::Risk;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

const GREEN: Style = Style::new().fg(Color::Green);
const YELLOW: Style = Style::new().fg(Color::Yellow);
const RED: Style = Style::new().fg(Color::Red);
const CYAN: Style = Style::new().fg(Color::Cyan);
const DIM: Style = Style::new().fg(Color::DarkGray);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
const CYAN_BOLD: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const RED_BOLD: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
const GREEN_BOLD: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);

pub fn draw(f: &mut Frame, app: &App) {
    if app.scanning && app.items.is_empty() {
        draw_scanning(f, app);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(f.area());
    draw_header(f, app, chunks[0]);
    match app.mode {
        Mode::Dashboard => draw_dashboard(f, app, chunks[1]),
        Mode::Detail => draw_detail(f, app, chunks[1]),
        Mode::Help => draw_help(f, chunks[1]),
        _ => draw_dashboard(f, app, chunks[1]),
    }
    draw_footer(f, app, chunks[2]);
    // 弹层
    match app.mode {
        Mode::Confirm => draw_confirm(f, app),
        Mode::Shred => draw_shred(f, app),
        Mode::Cleaning => draw_cleaning(f, app),
        Mode::Result => draw_result(f, app),
        _ => {}
    }
}

fn draw_scanning(f: &mut Frame, app: &App) {
    let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let sp = frames[app.spinner % frames.len()];
    let area = centered_rect(50, 20, f.area());
    let text = vec![
        Line::from(Span::styled(
            format!("{sp} 扫描中… {:.1}s", app.scan_start.elapsed().as_secs_f64()),
            CYAN,
        )),
        Line::from(Span::styled(
            app.opts_root_label
                .clone()
                .unwrap_or_else(|| "agent 大本营".into()),
            DIM,
        )),
    ];
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text)
            .block(Block::bordered().title(" agent-shit-cleaner "))
            .alignment(Alignment::Center),
        area,
    );
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let sel: u64 = app
        .selected
        .iter()
        .filter_map(|i| app.items.get(*i).map(|x| x.size))
        .sum();
    let line = Line::from(vec![
        Span::styled(" agent-shit-cleaner ", CYAN_BOLD),
        Span::raw(format!(
            "大本营 {} (可回收 {}) ",
            human_bytes(app.home_total),
            human_bytes(app.home_reclaim)
        )),
        if app.ws_total > 0 {
            Span::styled(
                format!("· 工作区 {} ", human_bytes(app.ws_total)),
                YELLOW,
            )
        } else {
            Span::raw("")
        },
        Span::styled(
            format!("· 已选 {} 项 {}", app.selected.len(), human_bytes(sel)),
            if app.selected.is_empty() { DIM } else { GREEN },
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn bar_str(frac: f64, width: usize) -> String {
    let filled = ((frac.clamp(0.0, 1.0)) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn draw_dashboard(f: &mut Frame, app: &App, area: Rect) {
    let mut rows: Vec<ListItem> = Vec::new();
    let max = app.groups.iter().map(|g| g.total).max().unwrap_or(1).max(1);
    for (i, g) in app.groups.iter().enumerate() {
        let cursor_span = if i == app.cursor {
            Span::styled("▸ ", CYAN)
        } else {
            Span::raw("  ")
        };
        let all_sel = g
            .item_idx
            .iter()
            .filter(|x| app.items[**x].reclaimable())
            .all(|x| app.selected.contains(x));
        let some_sel = g.item_idx.iter().any(|x| app.selected.contains(x));
        let check = if all_sel {
            Span::styled("[✓] ", GREEN)
        } else if some_sel {
            Span::styled("[~] ", YELLOW)
        } else {
            Span::raw("[ ] ")
        };
        let label_style = match g.kind {
            GroupKind::Home => Style::new().fg(Color::White),
            GroupKind::Workspace => YELLOW,
        };
        let label = pad(&g.label, 26);
        let bar = bar_str(g.total as f64 / max as f64, 20);
        let size = format!("{:>10}", human_bytes(g.total));
        let mut spans = vec![
            cursor_span,
            check,
            Span::styled(label, label_style),
            Span::styled(format!("{:>6} 项 ", g.count), DIM),
            Span::styled(bar, if g.kind == GroupKind::Workspace { YELLOW } else { CYAN }),
            Span::styled(size, Style::new().fg(Color::White)),
        ];
        if g.reclaimable > 0 {
            spans.push(Span::styled(
                format!("  可回收 {}", human_bytes(g.reclaimable)),
                GREEN,
            ));
        }
        if g.redownload > 0 {
            spans.push(Span::styled(
                format!("  ·{}*", human_bytes(g.redownload)),
                YELLOW,
            ));
        }
        rows.push(ListItem::new(Line::from(spans)));
    }
    let title = format!(
        " 垃圾分组（按体积降序）· {}/{} ",
        if app.groups.is_empty() {
            0
        } else {
            app.cursor + 1
        },
        app.groups.len()
    );
    let list = List::new(rows)
        .block(Block::bordered().title(title))
        .highlight_style(Style::new().bg(Color::DarkGray));
    // stateful 渲染：光标行自动保持在视口内（列表超屏时跟随滚动）
    let mut state = ListState::default();
    state.select(Some(app.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn pad(s: &str, w: usize) -> String {
    let len = s.chars().count();
    if len >= w {
        s.chars().take(w).collect()
    } else {
        format!("{s}{}", " ".repeat(w - len))
    }
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = app.detail.as_ref() else { return };
    let Some(g) = app.groups.get(d.group) else { return };
    let mut rows: Vec<ListItem> = Vec::new();
    for (row, idx) in d.order.iter().enumerate() {
        let it = &app.items[*idx];
        let cursor_span = if row == d.cursor {
            Span::styled("▸ ", CYAN)
        } else {
            Span::raw("  ")
        };
        let check = if app.selected.contains(idx) {
            Span::styled("[x] ", GREEN)
        } else {
            Span::raw("[ ] ")
        };
        let size = format!("{:>10}", human_bytes(it.size));
        let age = app
            .age_of(*idx)
            .map(|d| format!("{d:>5}d "))
            .unwrap_or_else(|| "    - ".into());
        let path = crate::shorten(&it.path);
        let mut spans = vec![cursor_span, check, Span::raw(size), Span::styled(age, DIM)];
        if it.reclaimable() {
            spans.push(Span::styled("◂可回收 ", GREEN));
        } else if it.risk == Risk::Redownload {
            spans.push(Span::styled("R可再下载 ", YELLOW));
        } else {
            spans.push(Span::styled("…保留期内 ", DIM));
        }
        if it.no_manifest {
            spans.push(Span::styled("⚠无清单 ", YELLOW));
        }
        if it.active == Some(true) {
            spans.push(Span::styled("⚡活跃项目 ", YELLOW));
        }
        spans.push(Span::raw(path));
        rows.push(ListItem::new(Line::from(spans)));
    }
    let title = format!(
        " {} · {} · {}/{} 项（按体积降序） ",
        g.label,
        g.sub,
        if d.order.is_empty() { 0 } else { d.cursor + 1 },
        d.order.len()
    );
    let list = List::new(rows).block(Block::bordered().title(title));
    // stateful 渲染：光标行自动保持在视口内（列表超屏时跟随滚动）
    let mut state = ListState::default();
    state.select(Some(d.cursor));
    f.render_stateful_widget(list, area, &mut state);
}

fn centered_rect(px: u16, py: u16, r: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Percentage((100 - py) / 2),
        Constraint::Percentage(py),
        Constraint::Percentage((100 - py) / 2),
    ])
    .split(r);
    Layout::horizontal([
        Constraint::Percentage((100 - px) / 2),
        Constraint::Percentage(px),
        Constraint::Percentage((100 - px) / 2),
    ])
    .split(popup[1])[1]
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let (n, total, big_n, big_bytes) = app.confirm_summary();
    let items = app.selected_items();
    let mut safe_n = 0;
    let mut expired_n = 0;
    let mut rd_n = 0;
    for it in &items {
        match it.risk {
            Risk::Safe => safe_n += 1,
            Risk::Retention => {
                if it.expired {
                    expired_n += 1
                } else {
                    rd_n += 1
                }
            }
            Risk::Redownload => rd_n += 1,
        }
    }
    let mut lines = vec![
        Line::from(Span::styled(
            format!("将处理 {n} 项 · {}", human_bytes(total)),
            BOLD,
        )),
        Line::from(vec![
            Span::raw("  可再生 "),
            Span::styled(format!("{safe_n}"), GREEN),
            Span::raw(" · 已过期 "),
            Span::styled(format!("{expired_n}"), GREEN),
            Span::raw(" · 需显式确认 "),
            Span::styled(format!("{rd_n}"), YELLOW),
        ]),
        Line::from(Span::raw("")),
        Line::from(Span::styled(
            "→ 全部移入回收站（保留 7 天，可 asc trash restore 反悔）",
            GREEN,
        )),
    ];
    if big_n > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "ℹ 含 {big_n} 个大件（{}）>1GB：进回收站不省磁盘空间",
                human_bytes(big_bytes)
            ),
            YELLOW,
        )));
        lines.push(Line::from(Span::styled(
            "  按 s 改为直接粉碎（不可恢复，需键入 DELETE）",
            YELLOW,
        )));
    }
    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(vec![
        Span::styled("[Enter 确认]", GREEN),
        Span::raw("  "),
        if big_n > 0 {
            Span::styled("[s 大件粉碎]", RED)
        } else {
            Span::raw("")
        },
        Span::raw("  "),
        Span::styled("[Esc 取消]", DIM),
    ]));
    let area = centered_rect(72, 40, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(Span::styled(" 确认清理 ", RED_BOLD)))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_shred(f: &mut Frame, app: &App) {
    let (_, _, big_n, big_bytes) = app.confirm_summary();
    let shown: String = "*".repeat(app.shred_input.chars().count());
    let lines = vec![
        Line::from(Span::styled(
            format!(
                "{big_n} 个大件（{}）将直接粉碎——不可恢复！",
                human_bytes(big_bytes)
            ),
            RED,
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("输入 DELETE 确认："),
            Span::styled(shown, Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("▏", CYAN),
        ]),
        Line::from(""),
        Line::from(Span::styled("[Enter 提交] [Esc 返回]", DIM)),
    ];
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(Span::styled(" ⚠ 大件粉碎 ", RED_BOLD)))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_cleaning(f: &mut Frame, app: &App) {
    let (done, total) = app.progress.unwrap_or((0, 1));
    let ratio = if total == 0 {
        1.0
    } else {
        done as f64 / total as f64
    };
    let gauge = Gauge::default()
        .ratio(ratio.clamp(0.0, 1.0))
        .label(format!(" 清理中 {done}/{total} "))
        .gauge_style(Style::new().fg(Color::Green).bg(Color::Black))
        .block(Block::bordered().title(" 执行 "));
    let area = centered_rect(50, 16, f.area());
    f.render_widget(Clear, area);
    f.render_widget(gauge, area);
}

fn draw_result(f: &mut Frame, app: &App) {
    let Some(o) = app.outcome.as_ref() else { return };
    let mut lines = vec![
        Line::from(Span::styled("✅ 清理完成", GREEN_BOLD)),
        Line::from(""),
        Line::from(Span::styled(
            format!("  释放 {}", human_bytes(o.freed_bytes)),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "  {} 项进回收站 · {} 项粉碎 · 失败 {} 项",
            o.trashed_count, o.shredded_count, o.failed.len()
        )),
    ];
    if let Some(id) = &o.trash_run_id {
        lines.push(Line::from(Span::styled(
            format!("  反悔：asc trash restore {id}"),
            CYAN,
        )));
    }
    if !o.failed.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  失败示例：{}", o.failed[0].0.display()),
            RED,
        )));
    }
    if !o.never_blocked.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  ⚠ NEVER 拦截 {} 项（规则疑似配错）", o.never_blocked.len()),
            YELLOW,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("[Enter 重新扫描] [q 退出]", DIM)));
    let area = centered_rect(66, 40, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_help(f: &mut Frame, area: Rect) {
    let keys = vec![
        ("↑/↓ j/k", "移动光标"),
        ("Enter", "进入分组详情（逐项勾选）"),
        ("space", "勾选/取消（分组页=整组可回收项；详情页=单项）"),
        ("a", "详情页：全选组内可回收项"),
        ("c", "确认清理（弹窗：Enter 执行 / s 大件粉碎 / Esc 取消）"),
        ("s", "确认弹窗：>1GB 大件改为直接粉碎（需键入 DELETE）"),
        ("r", "重新扫描"),
        ("?", "帮助"),
        ("q/Esc", "退出/返回"),
    ];
    let lines: Vec<Line> = keys
        .into_iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("  {k:<14}"), CYAN),
                Span::raw(v),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" 按键 ")),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let left = match app.mode {
        Mode::Dashboard => "↑↓ 移动 · space 勾选组 · Enter 详情 · c 清理 · ? 帮助 · q 退出",
        Mode::Detail => "space 勾选 · a 全选可回收 · c 清理 · Esc 返回",
        Mode::Confirm | Mode::Shred | Mode::Cleaning | Mode::Result | Mode::Help => "",
    };
    let line = if app.note.is_empty() {
        Line::from(Span::styled(format!(" {left}"), DIM))
    } else {
        Line::from(Span::styled(format!(" {}", app.note), YELLOW))
    };
    f.render_widget(Paragraph::new(line), area);
}

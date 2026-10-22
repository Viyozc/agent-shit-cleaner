//! ratatui rendering: dashboard / detail / confirm / shred input / progress / result / help

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
    // popups
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
            format!("{sp} scanning… {:.1}s", app.scan_start.elapsed().as_secs_f64()),
            CYAN,
        )),
        Line::from(Span::styled(
            app.opts_root_label
                .clone()
                .unwrap_or_else(|| "agent homes".into()),
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
            "homes {} (reclaimable {}) ",
            human_bytes(app.home_total),
            human_bytes(app.home_reclaim)
        )),
        if app.ws_total > 0 {
            Span::styled(
                format!("· workspaces {} ", human_bytes(app.ws_total)),
                YELLOW,
            )
        } else {
            Span::raw("")
        },
        Span::styled(
            format!(
                "· selected {} items {}",
                app.selected.len(),
                human_bytes(sel)
            ),
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
            Span::styled(format!("{:>6} items ", g.count), DIM),
            Span::styled(bar, if g.kind == GroupKind::Workspace { YELLOW } else { CYAN }),
            Span::styled(size, Style::new().fg(Color::White)),
        ];
        if g.reclaimable > 0 {
            spans.push(Span::styled(
                format!("  reclaim {}", human_bytes(g.reclaimable)),
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
        " junk groups (by size) · {}/{} ",
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
    // stateful rendering keeps the cursor row inside the viewport
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
            spans.push(Span::styled("↩reclaimable ", GREEN));
        } else if it.risk == Risk::Redownload {
            spans.push(Span::styled("⟳redownload ", YELLOW));
        } else {
            spans.push(Span::styled("…in retention ", DIM));
        }
        if it.no_manifest {
            spans.push(Span::styled("⚠no-manifest ", YELLOW));
        }
        if it.active == Some(true) {
            spans.push(Span::styled("⚡active ", YELLOW));
        }
        spans.push(Span::raw(path));
        rows.push(ListItem::new(Line::from(spans)));
    }
    let title = format!(
        " {} · {} · item {}/{} (by size) ",
        g.label,
        g.sub,
        if d.order.is_empty() { 0 } else { d.cursor + 1 },
        d.order.len()
    );
    let list = List::new(rows).block(Block::bordered().title(title));
    // stateful rendering keeps the cursor row inside the viewport
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
            format!("About to process {n} items · {}", human_bytes(total)),
            BOLD,
        )),
        Line::from(vec![
            Span::raw("  regenerable "),
            Span::styled(format!("{safe_n}"), GREEN),
            Span::raw(" · expired "),
            Span::styled(format!("{expired_n}"), GREEN),
            Span::raw(" · needs explicit opt-in "),
            Span::styled(format!("{rd_n}"), YELLOW),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "→ everything goes to the OS trash (recover via Finder \"Put Back\" or your DE's trash)",
            GREEN,
        )),
    ];
    if big_n > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "ℹ {big_n} big item(s) ({}), each >1GB: trashing saves no disk space",
                human_bytes(big_bytes)
            ),
            YELLOW,
        )));
        lines.push(Line::from(Span::styled(
            "  press s to shred instead (irreversible; requires typing DELETE)",
            YELLOW,
        )));
    }
    lines.push(Line::from(Span::raw("")));
    lines.push(Line::from(vec![
        Span::styled("[Enter confirm]", GREEN),
        Span::raw("  "),
        if big_n > 0 {
            Span::styled("[s shred big]", RED)
        } else {
            Span::raw("")
        },
        Span::raw("  "),
        Span::styled("[Esc cancel]", DIM),
    ]));
    let area = centered_rect(72, 40, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(Span::styled(" confirm clean ", RED_BOLD)))
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
                "{big_n} big item(s) ({}) will be shredded — IRREVERSIBLE!",
                human_bytes(big_bytes)
            ),
            RED,
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("type DELETE to confirm: "),
            Span::styled(shown, Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("▏", CYAN),
        ]),
        Line::from(""),
        Line::from(Span::styled("[Enter submit] [Esc back]", DIM)),
    ];
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(Span::styled(" ⚠ shred big items ", RED_BOLD)))
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
        .label(format!(" cleaning {done}/{total} "))
        .gauge_style(Style::new().fg(Color::Green).bg(Color::Black))
        .block(Block::bordered().title(" executing "));
    let area = centered_rect(50, 16, f.area());
    f.render_widget(Clear, area);
    f.render_widget(gauge, area);
}

fn draw_result(f: &mut Frame, app: &App) {
    let Some(o) = app.outcome.as_ref() else { return };
    let mut lines = vec![
        Line::from(Span::styled("✅ clean finished", GREEN_BOLD)),
        Line::from(""),
        Line::from(Span::styled(
            format!("  freed {}", human_bytes(o.freed_bytes)),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "  {} items to trash · {} shredded · {} failed",
            o.trashed_count, o.shredded_count, o.failed.len()
        )),
    ];
    if o.trashed_count > 0 {
        lines.push(Line::from(Span::styled(
            "  recover from the OS trash (macOS Finder: right-click → Put Back)",
            CYAN,
        )));
    }
    if !o.failed.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  first failure: {}", o.failed[0].0.display()),
            RED,
        )));
    }
    if !o.never_blocked.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "  ⚠ NEVER blocked {} item(s) (rule likely misconfigured)",
                o.never_blocked.len()
            ),
            YELLOW,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "[Enter rescan] [q quit]",
        DIM,
    )));
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
        ("↑/↓ j/k", "move cursor"),
        ("Enter", "open group details (per-item selection)"),
        ("space", "toggle (dashboard = group's reclaimable items; detail = one item)"),
        ("a", "in details: select all reclaimable items of the group"),
        ("c", "confirm clean (popup: Enter execute / s shred big / Esc cancel)"),
        ("s", "in the popup: shred >1GB items instead (requires typing DELETE)"),
        ("r", "rescan"),
        ("?", "help"),
        ("q/Esc", "quit/back"),
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
            .block(Block::bordered().title(" keys ")),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let left = match app.mode {
        Mode::Dashboard => "↑↓ move · space toggle group · Enter details · c clean · ? help · q quit",
        Mode::Detail => "space toggle · a select all reclaimable · c clean · Esc back",
        Mode::Confirm | Mode::Shred | Mode::Cleaning | Mode::Result | Mode::Help => "",
    };
    let line = if app.note.is_empty() {
        Line::from(Span::styled(format!(" {left}"), DIM))
    } else {
        Line::from(Span::styled(format!(" {}", app.note), YELLOW))
    };
    f.render_widget(Paragraph::new(line), area);
}

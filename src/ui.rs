//! btop-style rendering. All colors come from the active btop theme.

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};

use crate::app::{App, FeedKind, Hist, Panel, View};
use crate::theme::{Gradient, Theme};

// ------------------------------------------------------------------ helpers

pub fn si(v: f64) -> String {
    match v {
        v if v < 1.0 && v > 0.0 => format!("{v:.1}"),
        v if v < 1e3 => format!("{v:.0}"),
        v if v < 1e4 => format!("{:.2}k", v / 1e3),
        v if v < 1e6 => format!("{:.1}k", v / 1e3),
        v if v < 1e9 => format!("{:.1}M", v / 1e6),
        v => format!("{:.1}G", v / 1e9),
    }
}

pub fn bytes(v: f64) -> String {
    match v {
        v if v < 1.0 => "0".into(),
        v if v < 1024.0 => format!("{v:.0} B"),
        v if v < 1024.0 * 1024.0 => format!("{:.1} K", v / 1024.0),
        v if v < 1024.0 * 1024.0 * 1024.0 => format!("{:.1} M", v / 1024.0 / 1024.0),
        v => format!("{:.2} G", v / 1024.0 / 1024.0 / 1024.0),
    }
}

pub fn dur(ns: f64) -> String {
    match ns {
        n if n < 1e3 => format!("{n:.0}ns"),
        n if n < 1e6 => format!("{:.0}µs", n / 1e3),
        n if n < 1e9 => format!("{:.0}ms", n / 1e6),
        n => format!("{:.1}s", n / 1e9),
    }
}

/// Horizontal bar with 1/8th-cell resolution.
fn bar(frac: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (frac.clamp(0.0, 1.0) * width as f64 * 8.0).round() as usize;
    let mut s = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) && s.chars().count() < width {
        s.push(PARTS[eighths % 8]);
    }
    let pad = width.saturating_sub(s.chars().count());
    s + &" ".repeat(pad)
}

fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

/// Box outline color for a panel, following btop's four boxes.
fn box_color(th: &Theme, panel: Panel) -> Color {
    match panel {
        Panel::Cpu | Panel::Sched => th.cpu_box,
        Panel::Disk | Panel::Syscalls => th.mem_box,
        Panel::Net => th.net_box,
        Panel::Table | Panel::Feed => th.proc_box,
    }
}

const SUPERSCRIPT: [&str; 10] = ["⁰", "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹"];

/// A btop-style box: outline in the box color, a superscript number (its
/// zoom key) in the highlight color, then the title and extra info.
fn panel<'a>(th: &Theme, which: Panel, title: &'a str, info: Vec<Span<'a>>) -> Block<'a> {
    let outline = box_color(th, which);
    let mut t = vec![Span::styled(SUPERSCRIPT[which.number()], fg(th.hi).add_modifier(Modifier::BOLD))];
    if !title.is_empty() {
        t.push(Span::styled(format!("{title} "), fg(th.title).add_modifier(Modifier::BOLD)));
    }
    if !info.is_empty() {
        t.push(Span::styled(if title.is_empty() { "" } else { "─ " }, fg(outline)));
        t.extend(info);
        t.push(Span::raw(" "));
    }
    Block::bordered().border_type(BorderType::Rounded).border_style(fg(outline)).title(Line::from(t))
}

/// Label in the main text color (as btop's labels), value bold in `color`.
fn kv<'a>(th: &Theme, k: &'a str, v: String, color: Color) -> Vec<Span<'a>> {
    vec![Span::styled(k, fg(th.fg)), Span::styled(v, fg(color).add_modifier(Modifier::BOLD))]
}

/// Filled braille area graph, newest sample at the right edge, colored by
/// height from the gradient like btop's graphs.
fn graph(buf: &mut Buffer, area: Rect, data: &VecDeque<f64>, max: f64, grad: &Gradient) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // dots filled from the bottom: left column, right column
    const LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
    const RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
    let h = area.height as usize;
    let dots = (h * 4) as f64;
    let max = if max > 0.0 { max } else { 1.0 };
    let cols = area.width as usize * 2;
    let sample = |i: usize| -> usize {
        // i indexes dot columns from the left; align the newest sample right
        let back = cols - 1 - i;
        if back >= data.len() {
            return 0;
        }
        let v = data[data.len() - 1 - back];
        let filled = (v / max * dots).round() as usize;
        if v > 0.0 { filled.max(1) } else { 0 }
    };
    for cx in 0..area.width as usize {
        let (l, r) = (sample(cx * 2), sample(cx * 2 + 1));
        for cy in 0..h {
            let base = (h - 1 - cy) * 4; // dots below this cell
            let mut bits = 0u32;
            for d in 0..4 {
                if l > base + d {
                    bits |= LEFT[d];
                }
                if r > base + d {
                    bits |= RIGHT[d];
                }
            }
            let cell = &mut buf[(area.x + cx as u16, area.y + cy as u16)];
            if bits == 0 {
                cell.set_char(' ');
            } else {
                cell.set_char(char::from_u32(0x2800 + bits).unwrap());
                cell.set_fg(grad.at(1.0 - cy as f64 / h.max(1) as f64));
            }
        }
    }
}

fn hist_label(slot: usize) -> String {
    dur((1u64 << slot) as f64)
}

/// Rows of a log2 latency histogram, merging the low end if it doesn't fit.
/// Buckets are colored along `grad` between the `hot_slot` range.
fn hist_lines(
    th: &Theme,
    h: &Hist,
    rows: usize,
    width: usize,
    hot_slot: (usize, usize),
    grad: &Gradient,
) -> Vec<Line<'static>> {
    let (Some(first), Some(last)) = (h.slots.iter().position(|&n| n > 0), h.slots.iter().rposition(|&n| n > 0)) else {
        return vec![Line::styled("  no events", fg(th.inactive))];
    };
    if rows == 0 {
        return vec![];
    }
    let start = first.max((last + 1).saturating_sub(rows));
    let mut buckets: Vec<(String, u64, usize)> = Vec::new();
    for slot in start..=last {
        let n = if slot == start { h.slots[first..=start].iter().sum() } else { h.slots[slot] };
        let label = if slot == start && first < start {
            format!("<{}", hist_label(start + 1))
        } else {
            format!("{}-{}", hist_label(slot), hist_label(slot + 1))
        };
        buckets.push((label, n, slot));
    }
    let peak = buckets.iter().map(|b| b.1).max().unwrap_or(1).max(1);
    let bar_w = width.saturating_sub(12 + 8);
    let (lo, hi) = hot_slot;
    buckets
        .into_iter()
        .map(|(label, n, slot)| {
            let t = (slot as f64 - lo as f64) / (hi - lo) as f64;
            Line::from(vec![
                Span::styled(format!("{label:>11} "), fg(th.graph_text)),
                Span::styled(bar(n as f64 / peak as f64, bar_w), fg(grad.at(t))),
                Span::styled(format!("{:>7}", si(n as f64)), fg(th.fg)),
            ])
        })
        .collect()
}

fn pct_opt(v: Option<u64>) -> String {
    v.map_or("-".into(), |ns| format!("≤{}", dur(ns as f64)))
}

// ------------------------------------------------------------------- panels

/// Draws the dashboard and returns where each panel landed, for mouse
/// hit-testing. With a panel maximized, only that panel is drawn.
pub fn draw(f: &mut Frame, app: &App, kernel: &str) -> Vec<(Panel, Rect)> {
    let th = &app.theme;
    let area = f.area();
    // Paint the theme's background and default text color everywhere first;
    // widgets that don't set a background keep it.
    let base = th.bg.map_or(fg(th.fg), |bg| fg(th.fg).bg(bg));
    f.buffer_mut().set_style(area, base);

    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    draw_header(f, header, app, kernel);
    draw_footer(f, footer, app);

    let placed: Vec<(Panel, Rect)> = match app.maximized {
        Some(p) => vec![(p, body)],
        None => {
            let core_rows = app.ncpu.div_ceil(if app.ncpu > 16 { 2 } else { 1 }) as u16;
            let top_h = (core_rows + 3).clamp(10, 18);
            let [top, mid, bottom] =
                Layout::vertical([Constraint::Length(top_h), Constraint::Length(12), Constraint::Min(8)]).areas(body);
            let [cpu, sched] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(top);
            let [disk, net, sys] =
                Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Ratio(1, 3), Constraint::Ratio(1, 3)])
                    .areas(mid);
            let [table, feed] =
                Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).areas(bottom);
            vec![
                (Panel::Cpu, cpu),
                (Panel::Sched, sched),
                (Panel::Disk, disk),
                (Panel::Net, net),
                (Panel::Syscalls, sys),
                (Panel::Table, table),
                (Panel::Feed, feed),
            ]
        }
    };
    for &(p, r) in &placed {
        match p {
            Panel::Cpu => draw_cpu(f, r, app),
            Panel::Sched => draw_sched(f, r, app),
            Panel::Disk => draw_disk(f, r, app),
            Panel::Net => draw_net(f, r, app),
            Panel::Syscalls => draw_syscalls(f, r, app),
            Panel::Table if app.view == View::Processes => draw_procs(f, r, app),
            Panel::Table => draw_progs(f, r, app),
            Panel::Feed => draw_feed(f, r, app),
        }
    }
    placed
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, kernel: &str) {
    let th = &app.theme;
    let sep = || Span::styled("  │  ", fg(th.div_line));
    let mut spans = vec![
        Span::styled(" ebtop ", Style::new().fg(th.selected_fg).bg(th.selected_bg).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {kernel}  "), fg(th.graph_text)),
        Span::styled(format!("{} cpus", app.ncpu), fg(th.graph_text)),
        sep(),
        Span::styled("interval ", fg(th.fg)),
        Span::styled(format!("{:.2}s", app.interval.as_secs_f64()), fg(th.fg)),
        sep(),
        Span::styled("ebtop bpf overhead ", fg(th.fg)),
        Span::styled(format!("{:.2}% cpu", app.own_overhead), fg(th.cpu.at(app.own_overhead / 5.0))),
        sep(),
        Span::styled("theme ", fg(th.fg)),
        Span::styled(th.name.clone(), fg(th.title)),
    ];
    if app.paused {
        spans.push(Span::styled("   PAUSED", fg(th.hi).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_cpu(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let block = panel(
        th,
        Panel::Cpu,
        "cpu",
        vec![
            Span::styled("on-cpu time via sched_switch ", fg(th.graph_text)),
            Span::styled(
                format!("{:.1}%", app.cpu_total),
                fg(th.cpu.at(app.cpu_total / 100.0)).add_modifier(Modifier::BOLD),
            ),
        ],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let two_cols = app.ncpu > inner.height as usize;
    let core_w = if two_cols { 48 } else { 24 };
    let [g, cores] = Layout::horizontal([Constraint::Min(10), Constraint::Length(core_w)]).areas(inner);
    let g = Rect { width: g.width.saturating_sub(1), ..g };
    graph(f.buffer_mut(), g, &app.hist.cpu, 100.0, &th.cpu);

    let rows = inner.height as usize;
    let mut lines: Vec<Line> = Vec::new();
    for r in 0..rows {
        let mut spans = Vec::new();
        for col in 0..(if two_cols { 2 } else { 1 }) {
            let i = col * rows + r;
            if i >= app.ncpu {
                continue;
            }
            let p = app.cpu_pct[i];
            spans.push(Span::styled(format!("C{i:<3}"), fg(th.graph_text)));
            spans.push(Span::styled(bar(p / 100.0, 12), fg(th.cpu.at(p / 100.0)).bg(th.meter_bg)));
            spans.push(Span::styled(format!("{p:>5.0}%  "), fg(th.fg)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), cores);
}

fn draw_sched(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let block = panel(th, Panel::Sched, "scheduler", vec![Span::styled("run-queue latency", fg(th.graph_text))]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let r = &app.rates;
    let mut lines = vec![
        Line::from(
            [
                kv(th, "ctx switch ", format!("{:<8}", si(r.csw) + "/s"), th.fg),
                kv(th, "  wakeups ", si(r.wakeups) + "/s", th.fg),
            ]
            .concat(),
        ),
        Line::from(
            [
                kv(th, "p50 ", format!("{:<8}", pct_opt(app.runq.percentile(0.5))), th.cpu.at(0.1)),
                kv(th, " p99 ", format!("{:<8}", pct_opt(app.runq.percentile(0.99))), th.cpu.at(0.5)),
                kv(th, " max ", pct_opt(app.runq.max()), th.cpu.at(0.9)),
            ]
            .concat(),
        ),
    ];
    let rows = (inner.height as usize).saturating_sub(lines.len());
    lines.extend(hist_lines(th, &app.runq, rows, inner.width as usize, (10, 24), &th.cpu));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_disk(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let r = &app.rates;
    let block = panel(th, Panel::Disk, "disk", kv(th, "iops ", si(r.iops), th.fg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let graph_h = (inner.height / 5).max(2);
    let [text, g] = Layout::vertical([Constraint::Min(3), Constraint::Length(graph_h)]).areas(inner);
    let mut lines = vec![
        Line::from(
            [
                kv(th, "read ", format!("{:<10}", bytes(r.rd) + "/s"), th.free.at(1.0)),
                kv(th, "write ", bytes(r.wr) + "/s", th.used.at(1.0)),
            ]
            .concat(),
        ),
        Line::from(
            [
                kv(th, "lat p50 ", format!("{:<8}", pct_opt(app.bio.percentile(0.5))), th.cpu.at(0.1)),
                kv(th, " p99 ", pct_opt(app.bio.percentile(0.99)), th.cpu.at(0.6)),
            ]
            .concat(),
        ),
    ];
    let rows = (text.height as usize).saturating_sub(lines.len());
    lines.extend(hist_lines(th, &app.bio, rows, text.width as usize, (14, 27), &th.cpu));
    f.render_widget(Paragraph::new(lines), text);
    let max = app.hist.disk.iter().cloned().fold(0.0, f64::max);
    graph(f.buffer_mut(), g, &app.hist.disk, max, &th.used);
}

fn draw_net(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let r = &app.rates;
    let block = panel(th, Panel::Net, "network", vec![Span::styled("tcp", fg(th.graph_text))]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let graph_h = (inner.height / 5).max(2);
    let [text, g] = Layout::vertical([Constraint::Min(3), Constraint::Length(graph_h)]).areas(inner);
    let mut lines = vec![
        Line::from(
            [
                kv(th, "tx ", format!("{:<12}", bytes(r.tx) + "/s"), th.upload.at(1.0)),
                kv(th, "rx ", bytes(r.rx) + "/s", th.download.at(1.0)),
            ]
            .concat(),
        ),
        Line::from(
            [
                kv(
                    th,
                    "retrans ",
                    format!("{:<9}", si(r.retrans) + "/s"),
                    if r.retrans > 0.0 { th.cpu.at(0.5) } else { th.fg },
                ),
                kv(th, "drops ", si(r.drops) + "/s", if r.drops > 0.0 { th.cpu.at(1.0) } else { th.fg }),
            ]
            .concat(),
        ),
    ];
    if !app.top_drops.is_empty() {
        lines.push(Line::styled("drop reasons", fg(th.graph_text)));
    }
    let rows = (text.height as usize).saturating_sub(lines.len());
    let w = text.width as usize;
    for (name, rate) in app.top_drops.iter().take(rows) {
        let v = si(*rate);
        let name: String = name.chars().take(w.saturating_sub(v.len() + 3)).collect();
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<width$}", width = w.saturating_sub(v.len() + 2)), fg(th.fg)),
            Span::styled(v, fg(th.cpu.at(1.0))),
        ]));
    }
    f.render_widget(Paragraph::new(lines), text);
    let max = app.hist.net.iter().cloned().fold(0.0, f64::max);
    graph(f.buffer_mut(), g, &app.hist.net, max, &th.download);
}

fn draw_syscalls(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let r = &app.rates;
    let block = panel(th, Panel::Syscalls, "syscalls", kv(th, "", si(r.syscalls) + "/s", th.fg));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::from(
        [
            kv(th, "page faults ", format!("{:<9}", si(r.faults) + "/s"), th.fg),
            kv(th, "forks ", si(r.fork) + "/s", th.fg),
        ]
        .concat(),
    )];
    let rows = (inner.height as usize).saturating_sub(lines.len());
    let w = inner.width as usize;
    let peak = app.top_syscalls.first().map_or(1.0, |s| s.1);
    for (name, rate) in app.top_syscalls.iter().take(rows) {
        let bar_w = w.saturating_sub(16 + 8);
        let name: String = name.chars().take(15).collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<16}"), fg(th.fg)),
            Span::styled(bar(rate / peak, bar_w), fg(th.cached.at(rate / peak))),
            Span::styled(format!("{:>8}", si(*rate)), fg(th.fg)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn table_title(app: &App) -> Vec<Span<'static>> {
    let th = &app.theme;
    let tab = |name: &'static str, on: bool| {
        if on {
            Span::styled(name, Style::new().fg(th.selected_fg).bg(th.selected_bg).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(name, fg(th.title))
        }
    };
    vec![
        tab(" processes ", app.view == View::Processes),
        Span::raw(" "),
        tab(" bpf programs ", app.view == View::Programs),
    ]
}

fn header_row(app: &App) -> Row<'static> {
    let th = &app.theme;
    let sort = app.sort_col();
    let arrow = if app.sort_desc { "▼" } else { "▲" };
    Row::new(app.columns().iter().enumerate().map(|(i, &c)| {
        if i == sort {
            Cell::from(format!("{c}{arrow}")).style(fg(th.hi).add_modifier(Modifier::BOLD))
        } else {
            Cell::from(c).style(fg(th.title).add_modifier(Modifier::BOLD))
        }
    }))
}

fn num_cell(th: &Theme, s: String, active: bool) -> Cell<'static> {
    Cell::from(Line::from(s).right_aligned()).style(fg(if active { th.fg } else { th.inactive }))
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let visible: Vec<_> = app.visible_procs().collect();
    let mut info = table_title(app);
    info.push(Span::styled(
        format!("  {} shown{}", visible.len(), if app.hide_idle { " (active only)" } else { "" }),
        fg(th.graph_text),
    ));
    let block = panel(th, Panel::Table, "", info);
    let height = block.inner(area).height.saturating_sub(1) as usize;
    let scroll = app.scroll.min(visible.len().saturating_sub(height));
    let rows = visible.iter().skip(scroll).take(height).map(|p| {
        let rate = |v: f64| num_cell(th, if v > 0.0 { si(v) } else { "·".into() }, v > 0.0);
        let byt = |v: f64| num_cell(th, if v >= 1.0 { bytes(v) } else { "·".into() }, v >= 1.0);
        Row::new(vec![
            Cell::from(Line::from(p.pid.to_string()).right_aligned()).style(fg(th.graph_text)),
            Cell::from(p.comm.clone()).style(fg(th.fg)),
            Cell::from(Line::from(vec![
                Span::styled(bar(p.cpu / 100.0, 5), fg(th.process.at(p.cpu / 100.0)).bg(th.meter_bg)),
                Span::styled(format!("{:>6.1}", p.cpu), fg(if p.cpu >= 0.05 { th.fg } else { th.inactive })),
            ])),
            rate(p.syscalls),
            rate(p.csw),
            num_cell(th, if p.runq_avg_ns > 0.0 { dur(p.runq_avg_ns) } else { "·".into() }, p.runq_avg_ns > 0.0).style(
                fg(if p.runq_avg_ns > 0.0 { th.cpu.at(p.runq_avg_ns.log2() / 24.0 - 0.4) } else { th.inactive }),
            ),
            rate(p.faults),
            byt(p.rd),
            byt(p.wr),
            byt(p.tx),
            byt(p.rx),
        ])
    });
    let widths = [
        Constraint::Length(7),
        Constraint::Min(15),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
    ];
    let table = Table::new(rows, widths).header(header_row(app)).block(block).column_spacing(1);
    f.render_widget(table, area);
}

fn draw_progs(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let mut info = table_title(app);
    info.push(Span::styled(format!("  {} loaded  ", app.progs.len()), fg(th.graph_text)));
    info.push(Span::styled("* = ebtop", fg(th.proc_misc)));
    let block = panel(th, Panel::Table, "", info);
    let height = block.inner(area).height.saturating_sub(1) as usize;
    let scroll = app.scroll.min(app.progs.len().saturating_sub(height));
    let rows = app.progs.iter().skip(scroll).take(height).map(|p| {
        let active = p.events > 0.0;
        Row::new(vec![
            Cell::from(Line::from(p.id.to_string()).right_aligned()).style(fg(th.graph_text)),
            Cell::from(p.ty.clone()).style(fg(th.graph_text)),
            Cell::from(Line::from(vec![
                Span::styled(p.name.clone(), fg(th.fg)),
                Span::styled(if p.ours { " *" } else { "" }, fg(th.proc_misc)),
            ])),
            num_cell(th, if active { si(p.events) } else { "·".into() }, active),
            num_cell(th, if active { format!("{:.0}", p.avg_ns) } else { "·".into() }, active),
            Cell::from(Line::from(vec![
                Span::styled(bar(p.cpu / 100.0, 5), fg(th.process.at(p.cpu / 10.0)).bg(th.meter_bg)),
                Span::styled(format!("{:>7.2}", p.cpu), fg(if active { th.fg } else { th.inactive })),
            ])),
        ])
    });
    let widths = [
        Constraint::Length(6),
        Constraint::Length(16),
        Constraint::Min(18),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(13),
    ];
    let table = Table::new(rows, widths).header(header_row(app)).block(block).column_spacing(1);
    f.render_widget(table, area);
}

fn draw_feed(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let r = &app.rates;
    let block = panel(
        th,
        Panel::Feed,
        "exec / exit",
        [kv(th, "exec ", si(r.exec) + "/s", th.fg), kv(th, "  exit ", si(r.exit) + "/s", th.fg)].concat(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let bad_color = th.cpu.at(1.0);
    let lines: Vec<Line> = app
        .feed
        .iter()
        .filter(|e| app.show_exits || matches!(e.kind, FeedKind::Exec { .. }))
        .take(inner.height as usize)
        .map(|e| {
            let mut spans = vec![
                Span::styled(format!("{} ", e.time), fg(th.inactive)),
                Span::styled(format!("{:>7} ", e.pid), fg(th.graph_text)),
            ];
            match &e.kind {
                FeedKind::Exec { ppid, args } => {
                    spans.push(Span::styled("▶ ", fg(th.proc_misc)));
                    spans.push(Span::styled(if args.is_empty() { e.comm.clone() } else { args.clone() }, fg(th.fg)));
                    spans.push(Span::styled(format!("  ←{ppid}"), fg(th.inactive)));
                }
                FeedKind::Exit { code, dur_ns } => {
                    let status = code >> 8 & 0xff;
                    let sig = code & 0x7f;
                    let bad = status != 0 || sig != 0;
                    spans.push(Span::styled("■ ", fg(if bad { bad_color } else { th.graph_text })));
                    spans.push(Span::styled(format!("{} ", e.comm), fg(th.graph_text)));
                    let what = if sig != 0 { format!("sig {sig}") } else { format!("exit {status}") };
                    spans.push(Span::styled(what, fg(if bad { bad_color } else { th.graph_text })));
                    spans.push(Span::styled(format!(" after {}", dur(*dur_ns as f64)), fg(th.inactive)));
                }
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let th = &app.theme;
    let key = |k: &'static str, d: &'static str| {
        [Span::styled(k, fg(th.hi).add_modifier(Modifier::BOLD)), Span::styled(format!(" {d}  "), fg(th.fg))]
    };
    let spans = [
        key("q", "quit"),
        key("tab", if app.view == View::Processes { "bpf programs" } else { "processes" }),
        key("←→", "sort"),
        key("r", "reverse"),
        key("↑↓", "scroll"),
        key("1-7/click", if app.maximized.is_some() { "restore" } else { "zoom" }),
        key("t", "theme"),
        key("i", if app.hide_idle { "show idle" } else { "hide idle" }),
        key("e", if app.show_exits { "hide exits" } else { "show exits" }),
        key("+-", "interval"),
        key("space", if app.paused { "resume" } else { "pause" }),
    ]
    .concat();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::app::{App, FeedItem, FeedKind, Panel, View};
    use crate::bpf::{HIST_SLOTS, NR_COUNTERS, NR_DROP_REASONS, NR_SYSCALLS, ProcEntry, ProgStat, Pstat, Snapshot};

    fn snapshot(t: u64, ncpu: usize) -> Snapshot {
        let mut s = Snapshot {
            ts_ns: t * 1_000_000_000,
            counters: [t * 1000; NR_COUNTERS],
            cpu_busy_ns: (0..ncpu).map(|i| t * 100_000_000 * (i as u64 % 10)).collect(),
            runq_hist: [0; HIST_SLOTS],
            bio_hist: [0; HIST_SLOTS],
            syscalls: (0..NR_SYSCALLS as u64).map(|i| t * i).collect(),
            drops: vec![t; NR_DROP_REASONS],
            procs: HashMap::new(),
            progs: HashMap::new(),
        };
        for i in 8..30 {
            s.runq_hist[i] = t * (30 - i as u64) * 10;
            s.bio_hist[i] = t * i as u64;
        }
        for pid in 1..200u32 {
            let v = t * pid as u64 * 1000;
            let stat = Pstat {
                oncpu_ns: v * 100,
                runq_ns: v,
                runq_cnt: t,
                csw: v,
                syscalls: v,
                faults: v,
                rd_bytes: v,
                wr_bytes: v,
                tx_bytes: v,
                rx_bytes: v,
            };
            s.procs.insert(pid, ProcEntry { comm: format!("process-with-a-long-name-{pid}"), stat });
        }
        for id in 1..60u32 {
            s.progs.insert(
                id,
                ProgStat {
                    id,
                    ty: "tracing".into(),
                    name: format!("prog_{id}"),
                    run_time_ns: t * id as u64 * 1000,
                    run_cnt: t * 10,
                },
            );
        }
        s
    }

    #[test]
    fn renders_at_many_sizes() {
        for ncpu in [1, 12, 64] {
            let mut app = App::new(ncpu, Duration::from_secs(1), HashSet::from([3, 4]));
            for t in 1..5 {
                app.update(snapshot(t, ncpu));
            }
            app.push_feed(FeedItem {
                time: "12:00:00".into(),
                pid: 1,
                comm: "sh".into(),
                kind: FeedKind::Exec { ppid: 0, args: "sh -c true".into() },
            });
            app.push_feed(FeedItem {
                time: "12:00:01".into(),
                pid: 1,
                comm: "sh".into(),
                kind: FeedKind::Exit { code: 9, dur_ns: 5_000_000 },
            });
            for (w, h) in [(20, 5), (80, 24), (120, 40), (250, 70), (400, 120)] {
                for view in [View::Processes, View::Programs] {
                    for maximized in [None].into_iter().chain(Panel::ALL.map(Some)) {
                        app.view = view;
                        app.scroll = 1000;
                        app.maximized = maximized;
                        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                        let mut placed = Vec::new();
                        term.draw(|f| placed = super::draw(f, &app, "linux test")).unwrap();
                        assert_eq!(placed.len(), if maximized.is_some() { 1 } else { 7 });
                    }
                }
            }
        }
    }

    #[test]
    fn renders_with_every_theme() {
        let mut app = App::new(12, Duration::from_secs(1), HashSet::new());
        for t in 1..4 {
            app.update(snapshot(t, 12));
        }
        app.set_themes(crate::theme::available(), "Default", true);
        for _ in 0..app.themes.len() {
            let mut term = Terminal::new(TestBackend::new(160, 50)).unwrap();
            term.draw(|f| {
                super::draw(f, &app, "linux test");
            })
            .unwrap();
            app.cycle_theme(true);
        }
    }
}

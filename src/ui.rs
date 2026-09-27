//! btop-style rendering.

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, FeedKind, Hist, View};

const FG: Color = Color::Rgb(0xd8, 0xde, 0xe9);
const DIM: Color = Color::Rgb(0x6b, 0x72, 0x80);
const FAINT: Color = Color::Rgb(0x3b, 0x42, 0x52);
const GREEN: (u8, u8, u8) = (0x50, 0xe0, 0x90);
const YELLOW: (u8, u8, u8) = (0xf0, 0xc8, 0x50);
const RED: (u8, u8, u8) = (0xff, 0x55, 0x60);

const C_CPU: Color = Color::Rgb(0x88, 0xc0, 0xd0);
const C_SCHED: Color = Color::Rgb(0xb4, 0x8e, 0xad);
const C_DISK: Color = Color::Rgb(0xeb, 0xa0, 0x6b);
const C_NET: Color = Color::Rgb(0x81, 0xa1, 0xf1);
const C_SYS: Color = Color::Rgb(0xeb, 0xcb, 0x8b);
const C_PROC: Color = Color::Rgb(0xa3, 0xd0, 0x8c);
const C_FEED: Color = Color::Rgb(0xf0, 0x8c, 0xb4);

// ------------------------------------------------------------------ helpers

/// green -> yellow -> red over t in [0, 1]
fn grad(t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (a, b, u) = if t < 0.5 { (GREEN, YELLOW, t * 2.0) } else { (YELLOW, RED, (t - 0.5) * 2.0) };
    let l = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * u) as u8;
    Color::Rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

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

fn panel<'a>(title: &'a str, accent: Color, info: Vec<Span<'a>>) -> Block<'a> {
    let mut t = vec![];
    if !title.is_empty() {
        t.push(Span::styled(format!(" {title} "), Style::new().fg(accent).add_modifier(Modifier::BOLD)));
    }
    if !info.is_empty() {
        t.push(Span::styled(if title.is_empty() { " " } else { "─ " }, Style::new().fg(FAINT)));
        t.extend(info);
        t.push(Span::raw(" "));
    }
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(FAINT))
        .title(Line::from(t))
}

fn kv<'a>(k: &'a str, v: String, color: Color) -> Vec<Span<'a>> {
    vec![Span::styled(k, Style::new().fg(DIM)), Span::styled(v, Style::new().fg(color).bold())]
}

/// Filled braille area graph, newest sample at the right edge.
fn graph(buf: &mut Buffer, area: Rect, data: &VecDeque<f64>, max: f64, color: impl Fn(f64) -> Color) {
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
                cell.set_fg(color(1.0 - cy as f64 / h.max(1) as f64));
            }
        }
    }
}

fn hist_label(slot: usize) -> String {
    dur((1u64 << slot) as f64)
}

/// Rows of a log2 latency histogram, merging the low end if it doesn't fit.
fn hist_lines(h: &Hist, rows: usize, width: usize, hot_slot: (usize, usize)) -> Vec<Line<'static>> {
    let (Some(first), Some(last)) =
        (h.slots.iter().position(|&n| n > 0), h.slots.iter().rposition(|&n| n > 0))
    else {
        return vec![Line::styled("  no events", Style::new().fg(DIM))];
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
                Span::styled(format!("{label:>11} "), Style::new().fg(DIM)),
                Span::styled(bar(n as f64 / peak as f64, bar_w), Style::new().fg(grad(t))),
                Span::styled(format!("{:>7}", si(n as f64)), Style::new().fg(FG)),
            ])
        })
        .collect()
}

fn pct_opt(v: Option<u64>) -> String {
    v.map_or("-".into(), |ns| format!("≤{}", dur(ns as f64)))
}

// ------------------------------------------------------------------- panels

pub fn draw(f: &mut Frame, app: &App, kernel: &str) {
    let area = f.area();
    let core_rows = app.ncpu.div_ceil(if app.ncpu > 16 { 2 } else { 1 }) as u16;
    let top_h = (core_rows + 3).clamp(10, 18);
    let [header, top, mid, bottom, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(top_h),
        Constraint::Length(12),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, header, app, kernel);

    let [cpu, sched] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(top);
    draw_cpu(f, cpu, app);
    draw_sched(f, sched, app);

    let [disk, net, sys] =
        Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Ratio(1, 3), Constraint::Ratio(1, 3)]).areas(mid);
    draw_disk(f, disk, app);
    draw_net(f, net, app);
    draw_syscalls(f, sys, app);

    let [table, feed] = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).areas(bottom);
    match app.view {
        View::Processes => draw_procs(f, table, app),
        View::Programs => draw_progs(f, table, app),
    }
    draw_feed(f, feed, app);
    draw_footer(f, footer, app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, kernel: &str) {
    let mut spans = vec![
        Span::styled(" ebtop ", Style::new().fg(Color::Black).bg(C_CPU).bold()),
        Span::styled(format!("  {kernel}  "), Style::new().fg(DIM)),
        Span::styled(format!("{} cpus", app.ncpu), Style::new().fg(DIM)),
        Span::styled("  │  ", Style::new().fg(FAINT)),
        Span::styled("interval ", Style::new().fg(DIM)),
        Span::styled(format!("{:.2}s", app.interval.as_secs_f64()), Style::new().fg(FG)),
        Span::styled("  │  ", Style::new().fg(FAINT)),
        Span::styled("ebtop bpf overhead ", Style::new().fg(DIM)),
        Span::styled(format!("{:.2}% cpu", app.own_overhead), Style::new().fg(grad(app.own_overhead / 5.0))),
    ];
    if app.paused {
        spans.push(Span::styled("   PAUSED", Style::new().fg(RED_C).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

const RED_C: Color = Color::Rgb(RED.0, RED.1, RED.2);

fn draw_cpu(f: &mut Frame, area: Rect, app: &App) {
    let block = panel(
        "cpu",
        C_CPU,
        vec![
            Span::styled("on-cpu time via sched_switch ", Style::new().fg(DIM)),
            Span::styled(format!("{:.1}%", app.cpu_total), Style::new().fg(grad(app.cpu_total / 100.0)).bold()),
        ],
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

    let two_cols = app.ncpu > inner.height as usize;
    let core_w = if two_cols { 48 } else { 24 };
    let [g, cores] = Layout::horizontal([Constraint::Min(10), Constraint::Length(core_w)]).areas(inner);
    let g = Rect { width: g.width.saturating_sub(1), ..g };
    graph(f.buffer_mut(), g, &app.hist.cpu, 100.0, grad);

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
            spans.push(Span::styled(format!("C{i:<3}"), Style::new().fg(DIM)));
            spans.push(Span::styled(bar(p / 100.0, 12), Style::new().fg(grad(p / 100.0)).bg(FAINT)));
            spans.push(Span::styled(format!("{p:>5.0}%  "), Style::new().fg(FG)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), cores);
}

fn draw_sched(f: &mut Frame, area: Rect, app: &App) {
    let block = panel("scheduler", C_SCHED, vec![Span::styled("run-queue latency", Style::new().fg(DIM))]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let r = &app.rates;
    let mut lines = vec![
        Line::from([kv("ctx switch ", format!("{:<8}", si(r.csw) + "/s"), FG), kv("  wakeups ", si(r.wakeups) + "/s", FG)].concat()),
        Line::from(
            [
                kv("p50 ", format!("{:<8}", pct_opt(app.runq.percentile(0.5))), grad(0.1)),
                kv(" p99 ", format!("{:<8}", pct_opt(app.runq.percentile(0.99))), grad(0.5)),
                kv(" max ", pct_opt(app.runq.max()), grad(0.9)),
            ]
            .concat(),
        ),
    ];
    let rows = (inner.height as usize).saturating_sub(lines.len());
    lines.extend(hist_lines(&app.runq, rows, inner.width as usize, (10, 24)));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_disk(f: &mut Frame, area: Rect, app: &App) {
    let r = &app.rates;
    let block = panel("disk", C_DISK, kv("iops ", si(r.iops), FG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [text, g] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);
    let mut lines = vec![
        Line::from([kv("read ", format!("{:<10}", bytes(r.rd) + "/s"), GREEN_C), kv("write ", bytes(r.wr) + "/s", C_DISK)].concat()),
        Line::from(
            [
                kv("lat p50 ", format!("{:<8}", pct_opt(app.bio.percentile(0.5))), grad(0.1)),
                kv(" p99 ", pct_opt(app.bio.percentile(0.99)), grad(0.6)),
            ]
            .concat(),
        ),
    ];
    let rows = (text.height as usize).saturating_sub(lines.len());
    lines.extend(hist_lines(&app.bio, rows, text.width as usize, (14, 27)));
    f.render_widget(Paragraph::new(lines), text);
    let max = app.hist.disk.iter().cloned().fold(0.0, f64::max);
    graph(f.buffer_mut(), g, &app.hist.disk, max, |t| blend(C_DISK, t));
}

const GREEN_C: Color = Color::Rgb(GREEN.0, GREEN.1, GREEN.2);

fn blend(c: Color, t: f64) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let k = 0.45 + 0.55 * t.clamp(0.0, 1.0);
    Color::Rgb((r as f64 * k) as u8, (g as f64 * k) as u8, (b as f64 * k) as u8)
}

fn draw_net(f: &mut Frame, area: Rect, app: &App) {
    let r = &app.rates;
    let block = panel("network", C_NET, vec![Span::styled("tcp", Style::new().fg(DIM))]);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [text, g] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);
    let mut lines = vec![
        Line::from([kv("tx ", format!("{:<12}", bytes(r.tx) + "/s"), C_NET), kv("rx ", bytes(r.rx) + "/s", GREEN_C)].concat()),
        Line::from(
            [
                kv("retrans ", format!("{:<9}", si(r.retrans) + "/s"), if r.retrans > 0.0 { YELLOW_C } else { FG }),
                kv("drops ", si(r.drops) + "/s", if r.drops > 0.0 { RED_C } else { FG }),
            ]
            .concat(),
        ),
    ];
    if !app.top_drops.is_empty() {
        lines.push(Line::styled("drop reasons", Style::new().fg(DIM)));
    }
    let rows = (text.height as usize).saturating_sub(lines.len());
    let w = text.width as usize;
    for (name, rate) in app.top_drops.iter().take(rows) {
        let v = si(*rate);
        let name: String = name.chars().take(w.saturating_sub(v.len() + 3)).collect();
        lines.push(Line::from(vec![
            Span::styled(format!(" {name:<width$}", width = w.saturating_sub(v.len() + 2)), Style::new().fg(FG)),
            Span::styled(v, Style::new().fg(RED_C)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), text);
    let max = app.hist.net.iter().cloned().fold(0.0, f64::max);
    graph(f.buffer_mut(), g, &app.hist.net, max, |t| blend(C_NET, t));
}

const YELLOW_C: Color = Color::Rgb(YELLOW.0, YELLOW.1, YELLOW.2);

fn draw_syscalls(f: &mut Frame, area: Rect, app: &App) {
    let r = &app.rates;
    let block = panel("syscalls", C_SYS, kv("", si(r.syscalls) + "/s", FG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::from(
        [kv("page faults ", format!("{:<9}", si(r.faults) + "/s"), FG), kv("forks ", si(r.fork) + "/s", FG)].concat(),
    )];
    let rows = (inner.height as usize).saturating_sub(lines.len());
    let w = inner.width as usize;
    let peak = app.top_syscalls.first().map_or(1.0, |s| s.1);
    for (name, rate) in app.top_syscalls.iter().take(rows) {
        let bar_w = w.saturating_sub(16 + 8);
        let name: String = name.chars().take(15).collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{name:<16}"), Style::new().fg(FG)),
            Span::styled(bar(rate / peak, bar_w), Style::new().fg(blend(C_SYS, rate / peak))),
            Span::styled(format!("{:>8}", si(*rate)), Style::new().fg(FG)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn table_title(app: &App) -> Vec<Span<'static>> {
    let tab = |name: &'static str, on: bool| {
        if on {
            Span::styled(name, Style::new().fg(Color::Black).bg(C_PROC).bold())
        } else {
            Span::styled(name, Style::new().fg(DIM))
        }
    };
    vec![
        tab(" processes ", app.view == View::Processes),
        Span::raw(" "),
        tab(" bpf programs ", app.view == View::Programs),
    ]
}

fn header_row(app: &App) -> Row<'static> {
    let sort = app.sort_col();
    let arrow = if app.sort_desc { "▼" } else { "▲" };
    Row::new(app.columns().iter().enumerate().map(|(i, &c)| {
        if i == sort {
            Cell::from(format!("{c}{arrow}")).style(Style::new().fg(C_PROC).bold())
        } else {
            Cell::from(c).style(Style::new().fg(DIM).bold())
        }
    }))
}

fn num_cell(s: String, active: bool) -> Cell<'static> {
    Cell::from(Line::from(s).right_aligned()).style(Style::new().fg(if active { FG } else { FAINT }))
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App) {
    let visible: Vec<_> = app.visible_procs().collect();
    let mut info = table_title(app);
    info.push(Span::styled(
        format!("  {} shown{}", visible.len(), if app.hide_idle { " (active only)" } else { "" }),
        Style::new().fg(DIM),
    ));
    let block = panel("", C_PROC, info);
    let height = block.inner(area).height.saturating_sub(1) as usize;
    let scroll = app.scroll.min(visible.len().saturating_sub(height));
    let rows = visible.iter().skip(scroll).take(height).map(|p| {
        let rate = |v: f64| num_cell(if v > 0.0 { si(v) } else { "·".into() }, v > 0.0);
        let byt = |v: f64| num_cell(if v >= 1.0 { bytes(v) } else { "·".into() }, v >= 1.0);
        Row::new(vec![
            Cell::from(Line::from(p.pid.to_string()).right_aligned()).style(Style::new().fg(DIM)),
            Cell::from(p.comm.clone()).style(Style::new().fg(FG)),
            Cell::from(Line::from(vec![
                Span::styled(bar(p.cpu / 100.0, 5), Style::new().fg(grad(p.cpu / 100.0)).bg(FAINT)),
                Span::styled(format!("{:>6.1}", p.cpu), Style::new().fg(if p.cpu >= 0.05 { FG } else { FAINT })),
            ])),
            rate(p.syscalls),
            rate(p.csw),
            num_cell(
                if p.runq_avg_ns > 0.0 { dur(p.runq_avg_ns) } else { "·".into() },
                p.runq_avg_ns > 0.0,
            )
            .style(Style::new().fg(if p.runq_avg_ns > 0.0 { grad(p.runq_avg_ns.log2() / 24.0 - 0.4) } else { FAINT })),
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
    let mut info = table_title(app);
    info.push(Span::styled(format!("  {} loaded  ", app.progs.len()), Style::new().fg(DIM)));
    info.push(Span::styled("* = ebtop", Style::new().fg(C_SCHED)));
    let block = panel("", C_PROC, info);
    let height = block.inner(area).height.saturating_sub(1) as usize;
    let scroll = app.scroll.min(app.progs.len().saturating_sub(height));
    let rows = app.progs.iter().skip(scroll).take(height).map(|p| {
        let active = p.events > 0.0;
        Row::new(vec![
            Cell::from(Line::from(p.id.to_string()).right_aligned()).style(Style::new().fg(DIM)),
            Cell::from(p.ty.clone()).style(Style::new().fg(DIM)),
            Cell::from(Line::from(vec![
                Span::styled(p.name.clone(), Style::new().fg(FG)),
                Span::styled(if p.ours { " *" } else { "" }, Style::new().fg(C_SCHED)),
            ])),
            num_cell(if active { si(p.events) } else { "·".into() }, active),
            num_cell(if active { format!("{:.0}", p.avg_ns) } else { "·".into() }, active),
            Cell::from(Line::from(vec![
                Span::styled(bar(p.cpu / 100.0, 5), Style::new().fg(grad(p.cpu / 10.0)).bg(FAINT)),
                Span::styled(format!("{:>7.2}", p.cpu), Style::new().fg(if active { FG } else { FAINT })),
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
    let r = &app.rates;
    let block = panel(
        "exec / exit",
        C_FEED,
        [kv("exec ", si(r.exec) + "/s", FG), kv("  exit ", si(r.exit) + "/s", FG)].concat(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = app
        .feed
        .iter()
        .filter(|e| app.show_exits || matches!(e.kind, FeedKind::Exec { .. }))
        .take(inner.height as usize)
        .map(|e| {
            let mut spans = vec![
                Span::styled(format!("{} ", e.time), Style::new().fg(FAINT)),
                Span::styled(format!("{:>7} ", e.pid), Style::new().fg(DIM)),
            ];
            match &e.kind {
                FeedKind::Exec { ppid, args } => {
                    spans.push(Span::styled("▶ ", Style::new().fg(GREEN_C)));
                    spans.push(Span::styled(if args.is_empty() { e.comm.clone() } else { args.clone() }, Style::new().fg(FG)));
                    spans.push(Span::styled(format!("  ←{ppid}"), Style::new().fg(FAINT)));
                }
                FeedKind::Exit { code, dur_ns } => {
                    let status = code >> 8 & 0xff;
                    let sig = code & 0x7f;
                    let bad = status != 0 || sig != 0;
                    spans.push(Span::styled("■ ", Style::new().fg(if bad { RED_C } else { DIM })));
                    spans.push(Span::styled(format!("{} ", e.comm), Style::new().fg(DIM)));
                    let what = if sig != 0 { format!("sig {sig}") } else { format!("exit {status}") };
                    spans.push(Span::styled(what, Style::new().fg(if bad { RED_C } else { DIM })));
                    spans.push(Span::styled(format!(" after {}", dur(*dur_ns as f64)), Style::new().fg(FAINT)));
                }
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let key = |k: &'static str, d: &'static str| {
        [Span::styled(k, Style::new().fg(C_CPU).bold()), Span::styled(format!(" {d}  "), Style::new().fg(DIM))]
    };
    let spans = [
        key("q", "quit"),
        key("tab", if app.view == View::Processes { "bpf programs" } else { "processes" }),
        key("←→", "sort"),
        key("r", "reverse"),
        key("↑↓", "scroll"),
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

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::{App, FeedItem, FeedKind, View};
    use crate::bpf::{ProcEntry, ProgStat, Pstat, Snapshot, HIST_SLOTS, NR_COUNTERS, NR_DROP_REASONS, NR_SYSCALLS};

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
            let stat = Pstat { oncpu_ns: v * 100, runq_ns: v, runq_cnt: t, csw: v, syscalls: v, faults: v, rd_bytes: v, wr_bytes: v, tx_bytes: v, rx_bytes: v };
            s.procs.insert(pid, ProcEntry { comm: format!("process-with-a-long-name-{pid}"), stat });
        }
        for id in 1..60u32 {
            s.progs.insert(id, ProgStat { id, ty: "tracing".into(), name: format!("prog_{id}"), run_time_ns: t * id as u64 * 1000, run_cnt: t * 10 });
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
            app.push_feed(FeedItem { time: "12:00:00".into(), pid: 1, comm: "sh".into(), kind: FeedKind::Exec { ppid: 0, args: "sh -c true".into() } });
            app.push_feed(FeedItem { time: "12:00:01".into(), pid: 1, comm: "sh".into(), kind: FeedKind::Exit { code: 9, dur_ns: 5_000_000 } });
            for (w, h) in [(20, 5), (80, 24), (120, 40), (250, 70), (400, 120)] {
                for view in [View::Processes, View::Programs] {
                    app.view = view;
                    app.scroll = 1000;
                    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                    term.draw(|f| super::draw(f, &app, "linux test")).unwrap();
                }
            }
        }
    }
}

mod app;
mod bpf;
mod theme;
mod ui;

use std::cell::RefCell;
use std::mem::MaybeUninit;
use std::ptr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use libbpf_rs::RingBufferBuilder;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};

use app::{App, FeedItem, FeedKind, Panel, View};

const USAGE: &str = "usage: ebtop [-i SECONDS] [-t THEME] [--transparent] [--dump] [--list-themes] [-V]

Real-time kernel activity dashboard built on eBPF (needs root).

  -i, --interval SECONDS   refresh interval (default 1.0)
  -t, --theme NAME|PATH    btop color theme (default: color_theme from
                           ~/.config/ebtop/ebtop.conf, else from btop.conf)
      --transparent        keep the terminal's background instead of the theme's
      --list-themes        list available themes and exit
      --dump               sample one interval, print a text summary and exit
  -V, --version            print version and exit";

struct Opts {
    interval: Duration,
    dump: bool,
    theme: Option<String>,
    transparent: bool,
    list_themes: bool,
}

fn parse_args() -> Result<Opts> {
    let mut args = std::env::args().skip(1);
    let mut interval = 1.0;
    let mut opts = Opts { interval: Duration::ZERO, dump: false, theme: None, transparent: false, list_themes: false };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dump" => opts.dump = true,
            "--transparent" => opts.transparent = true,
            "--list-themes" => opts.list_themes = true,
            "-t" | "--theme" => opts.theme = Some(args.next().context("missing value for --theme")?),
            "-i" | "--interval" => {
                interval = args.next().context("missing value for --interval")?.parse().context("bad interval")?
            }
            "-V" | "--version" => {
                println!("ebtop {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => bail!("unknown argument {a}\n\n{USAGE}"),
        }
    }
    opts.interval = Duration::from_secs_f64(f64::clamp(interval, 0.1, 10.0));
    Ok(opts)
}

/// Picks the theme: --theme, else the configured one (ebtop.conf, then
/// btop.conf), else btop's Default.
fn select_theme(opts: &Opts) -> Result<(Vec<theme::Entry>, String, bool)> {
    let (conf_theme, conf_bg) = theme::configured();
    let background = !opts.transparent && conf_bg.unwrap_or(true);
    let mut themes = theme::available();
    let name = match (&opts.theme, conf_theme) {
        (Some(wanted), _) => {
            theme::find(&themes, wanted).with_context(|| format!("unknown theme {wanted:?}; see --list-themes"))?
        }
        // a stale config entry shouldn't stop ebtop from starting
        (None, Some(wanted)) => theme::find(&themes, &wanted).unwrap_or_else(|| themes[0].clone()),
        (None, None) => themes[0].clone(),
    };
    if !themes.iter().any(|e| e.name == name.name) {
        themes.push(name.clone());
    }
    Ok((themes, name.name, background))
}

fn local_time() -> String {
    unsafe {
        let t = libc::time(ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

fn kernel_release() -> String {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    unsafe { libc::uname(&mut u) };
    let rel = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
    format!("linux {}", rel.to_string_lossy())
}

fn cstr(raw: &[i8]) -> String {
    let b: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&b).into_owned()
}

fn parse_event(data: &[u8]) -> Option<FeedItem> {
    if data.len() < size_of::<bpf::Event>() {
        return None;
    }
    // SAFETY: length checked; the record is a struct event.
    let e: bpf::Event = unsafe { ptr::read_unaligned(data.as_ptr().cast()) };
    let kind = match e.kind {
        bpf::EV_EXEC => {
            // argv is NUL-separated
            let n = (e.args_len as usize).min(e.args.len());
            let args = String::from_utf8_lossy(&e.args[..n])
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            FeedKind::Exec { ppid: e.ppid, args }
        }
        bpf::EV_EXIT => FeedKind::Exit { code: e.exit_code, dur_ns: e.dur_ns },
        _ => return None,
    };
    Some(FeedItem { time: local_time(), pid: e.pid, comm: cstr(&e.comm), kind })
}

fn print_dump(app: &App, kernel: &str) {
    use ui::{bytes, dur, si};
    let r = &app.rates;
    let p = |h: &app::Hist, q| h.percentile(q).map_or("-".into(), |ns| dur(ns as f64));
    println!("{kernel}, {} cpus, {:.2}s interval", app.ncpu, app.interval.as_secs_f64());
    println!(
        "cpu        {:.1}%  per-cpu {:?}",
        app.cpu_total,
        app.cpu_pct.iter().map(|p| p.round() as u32).collect::<Vec<_>>()
    );
    println!(
        "sched      ctxsw {}/s  wakeups {}/s  runq p50 {} p99 {} ({} samples)",
        si(r.csw),
        si(r.wakeups),
        p(&app.runq, 0.5),
        p(&app.runq, 0.99),
        app.runq.total()
    );
    println!(
        "syscalls   {}/s  faults {}/s  exec {}/s fork {}/s exit {}/s",
        si(r.syscalls),
        si(r.faults),
        si(r.exec),
        si(r.fork),
        si(r.exit)
    );
    println!(
        "disk       {} iops  rd {}/s wr {}/s  lat p50 {} p99 {}",
        si(r.iops),
        bytes(r.rd),
        bytes(r.wr),
        p(&app.bio, 0.5),
        p(&app.bio, 0.99)
    );
    println!(
        "net        tcp tx {}/s rx {}/s  retrans {}/s  drops {}/s",
        bytes(r.tx),
        bytes(r.rx),
        si(r.retrans),
        si(r.drops)
    );
    for (name, rate) in app.top_drops.iter().take(5) {
        println!("             drop {name}: {}/s", si(*rate));
    }
    println!("top syscalls:");
    for (name, rate) in app.top_syscalls.iter().take(8) {
        println!("  {name:<20} {:>8}/s", si(*rate));
    }
    println!("top processes by cpu:");
    for p in app.procs.iter().take(10) {
        println!(
            "  {:>7} {:<16} cpu {:>5.1}%  sysc {:>7}/s  csw {:>6}/s  runq {:>6}  rd {:>8} wr {:>8} tx {:>8} rx {:>8}",
            p.pid,
            p.comm,
            p.cpu,
            si(p.syscalls),
            si(p.csw),
            dur(p.runq_avg_ns),
            bytes(p.rd),
            bytes(p.wr),
            bytes(p.tx),
            bytes(p.rx)
        );
    }
    println!("bpf programs ({} loaded, ebtop overhead {:.2}% cpu):", app.progs.len(), app.own_overhead);
    let mut progs = app.progs.clone();
    progs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
    for p in progs.iter().take(18) {
        println!(
            "  {:>5} {:<16} {:<18} {:>8}/s {:>6.0}ns {:>6.3}%{}",
            p.id,
            p.ty,
            p.name,
            si(p.events),
            p.avg_ns,
            p.cpu,
            if p.ours { " *" } else { "" }
        );
    }
    println!("recent exec/exit:");
    for e in app.feed.iter().take(10) {
        match &e.kind {
            FeedKind::Exec { ppid, args } => println!("  exec {:>7} <- {ppid:<7} {args}", e.pid),
            FeedKind::Exit { code, dur_ns } => {
                println!("  exit {:>7} {} code {code} after {}", e.pid, e.comm, dur(*dur_ns as f64))
            }
        }
    }
}

fn main() -> Result<()> {
    // Die quietly on a closed pipe (`ebtop --dump | head`) like other CLI
    // tools, instead of Rust's default panic.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let opts = parse_args()?;
    let (themes, theme_name, background) = select_theme(&opts)?;
    if opts.list_themes {
        for e in &themes {
            let mark = if e.name == theme_name { "*" } else { " " };
            println!("{mark} {:<28} {}", e.name, e.origin());
        }
        return Ok(());
    }
    let (interval, dump) = (opts.interval, opts.dump);
    if unsafe { libc::geteuid() } != 0 {
        bail!("ebtop loads BPF programs and needs root: try `sudo {}`", std::env::args().next().unwrap());
    }

    let mut open_object = MaybeUninit::uninit();
    let mut skel = bpf::load(&mut open_object)?;
    let _stats = bpf::enable_stats();
    let mut reader = bpf::Reader::new(&mut skel)?;
    let own = bpf::own_prog_ids(&skel);

    let pending: Rc<RefCell<Vec<FeedItem>>> = Rc::default();
    let mut rbb = RingBufferBuilder::new();
    let sink = pending.clone();
    rbb.add(&skel.maps.events, move |data| {
        if let Some(item) = parse_event(data) {
            sink.borrow_mut().push(item);
        }
        0
    })?;
    let ring = rbb.build()?;

    let mut app = App::new(reader.ncpu(), interval, own);
    app.set_themes(themes, &theme_name, background);
    app.update(reader.read(&skel));
    let kernel = kernel_release();

    if dump {
        std::thread::sleep(interval);
        ring.consume()?;
        for item in pending.borrow_mut().drain(..) {
            app.push_feed(item);
        }
        app.update(reader.read(&skel));
        print_dump(&app, &kernel);
        return Ok(());
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        restore_hook(info);
    }));
    let mut placed: Vec<(Panel, Rect)> = Vec::new();
    let result = (|| -> Result<()> {
        let mut next_tick = Instant::now() + app.interval;
        loop {
            ring.consume()?;
            if !app.paused {
                for item in pending.borrow_mut().drain(..) {
                    app.push_feed(item);
                }
            } else {
                pending.borrow_mut().clear();
            }
            terminal.draw(|f| placed = ui::draw(f, &app, &kernel))?;

            let timeout = next_tick.saturating_duration_since(Instant::now()).min(Duration::from_millis(100));
            let ev = if event::poll(timeout)? { Some(event::read()?) } else { None };
            if let Some(Event::Mouse(m)) = ev {
                match m.kind {
                    // click a panel to zoom it; click again anywhere to restore
                    MouseEventKind::Down(MouseButton::Left) => {
                        let at = Position::new(m.column, m.row);
                        if app.maximized.is_some() {
                            app.maximized = None;
                        } else if let Some(&(p, _)) = placed.iter().find(|(_, r)| r.contains(at)) {
                            app.maximized = Some(p);
                        }
                    }
                    MouseEventKind::ScrollDown => app.scroll += 3,
                    MouseEventKind::ScrollUp => app.scroll = app.scroll.saturating_sub(3),
                    _ => {}
                }
            }
            if let Some(Event::Key(k)) = ev {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match k.code {
                    KeyCode::Esc if app.maximized.is_some() => app.maximized = None,
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char(c @ '1'..='7') => {
                        let p = Panel::ALL[c as usize - '1' as usize];
                        app.maximized = if app.maximized == Some(p) { None } else { Some(p) };
                    }
                    KeyCode::Char('t') => app.cycle_theme(true),
                    KeyCode::Char('T') => app.cycle_theme(false),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                    KeyCode::Tab | KeyCode::BackTab => {
                        app.view = if app.view == View::Processes { View::Programs } else { View::Processes };
                        app.scroll = 0;
                        app.sort();
                    }
                    KeyCode::Right | KeyCode::Char('>') => app.cycle_sort(true),
                    KeyCode::Left | KeyCode::Char('<') => app.cycle_sort(false),
                    KeyCode::Char('r') => {
                        app.sort_desc = !app.sort_desc;
                        app.sort();
                    }
                    KeyCode::Down | KeyCode::Char('j') => app.scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => app.scroll = app.scroll.saturating_sub(1),
                    KeyCode::PageDown => app.scroll += 20,
                    KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(20),
                    KeyCode::Home | KeyCode::Char('g') => app.scroll = 0,
                    KeyCode::Char('i') => app.hide_idle = !app.hide_idle,
                    KeyCode::Char('e') => app.show_exits = !app.show_exits,
                    KeyCode::Char(' ') => app.paused = !app.paused,
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        app.interval = (app.interval + Duration::from_millis(250)).min(Duration::from_secs(10))
                    }
                    KeyCode::Char('-') => {
                        app.interval =
                            app.interval.saturating_sub(Duration::from_millis(250)).max(Duration::from_millis(250))
                    }
                    _ => {}
                }
            }

            if Instant::now() >= next_tick {
                next_tick = Instant::now() + app.interval;
                let snap = reader.read(&skel);
                if !app.paused {
                    app.update(snap);
                }
            }
        }
    })();
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

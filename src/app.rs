//! Turns pairs of cumulative snapshots into rates, histories and table rows.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crate::bpf::{self, Counter, ProgStat, Snapshot, HIST_SLOTS};

const HISTORY: usize = 600;
const FEED_LEN: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Processes,
    Programs,
}

#[derive(Default, Clone)]
pub struct ProcRow {
    pub pid: u32,
    pub comm: String,
    pub cpu: f64, // percent of one CPU
    pub syscalls: f64,
    pub csw: f64,
    pub runq_avg_ns: f64,
    pub faults: f64,
    pub rd: f64,
    pub wr: f64,
    pub tx: f64,
    pub rx: f64,
}

impl ProcRow {
    pub const COLUMNS: [&'static str; 11] =
        ["PID", "COMMAND", "CPU%", "SYSC/s", "CSW/s", "RUNQ avg", "FLT/s", "DISK R", "DISK W", "NET TX", "NET RX"];

    fn key(&self, col: usize) -> f64 {
        match col {
            0 => self.pid as f64,
            2 => self.cpu,
            3 => self.syscalls,
            4 => self.csw,
            5 => self.runq_avg_ns,
            6 => self.faults,
            7 => self.rd,
            8 => self.wr,
            9 => self.tx,
            10 => self.rx,
            _ => 0.0,
        }
    }

    fn idle(&self) -> bool {
        self.cpu < 0.05 && self.syscalls == 0.0 && self.rd == 0.0 && self.wr == 0.0 && self.tx == 0.0 && self.rx == 0.0
    }
}

#[derive(Clone)]
pub struct ProgRow {
    pub id: u32,
    pub ty: String,
    pub name: String,
    pub events: f64,
    pub avg_ns: f64,
    pub cpu: f64, // percent of one CPU
    pub ours: bool,
}

impl ProgRow {
    pub const COLUMNS: [&'static str; 6] = ["ID", "TYPE", "NAME", "EVENTS/s", "AVG ns", "CPU%"];

    fn key(&self, col: usize) -> f64 {
        match col {
            0 => self.id as f64,
            3 => self.events,
            4 => self.avg_ns,
            5 => self.cpu,
            _ => 0.0,
        }
    }
}

pub enum FeedKind {
    Exec { ppid: u32, args: String },
    Exit { code: i32, dur_ns: u64 },
}

pub struct FeedItem {
    pub time: String,
    pub pid: u32,
    pub comm: String,
    pub kind: FeedKind,
}

#[derive(Default, Clone, Copy)]
pub struct Rates {
    pub csw: f64,
    pub wakeups: f64,
    pub syscalls: f64,
    pub faults: f64,
    pub exec: f64,
    pub fork: f64,
    pub exit: f64,
    pub iops: f64,
    pub rd: f64,
    pub wr: f64,
    pub tx: f64,
    pub rx: f64,
    pub retrans: f64,
    pub drops: f64,
}

#[derive(Default)]
pub struct Histories {
    pub cpu: VecDeque<f64>,
    pub syscalls: VecDeque<f64>,
    pub csw: VecDeque<f64>,
    pub runq_p99: VecDeque<f64>,
    pub disk: VecDeque<f64>,
    pub net: VecDeque<f64>,
}

fn push(q: &mut VecDeque<f64>, v: f64) {
    if q.len() == HISTORY {
        q.pop_front();
    }
    q.push_back(v);
}

/// Log2 latency histogram over the last interval.
#[derive(Clone, Copy)]
pub struct Hist {
    pub slots: [u64; HIST_SLOTS],
}

impl Default for Hist {
    fn default() -> Self {
        Self { slots: [0; HIST_SLOTS] }
    }
}

impl Hist {
    pub fn total(&self) -> u64 {
        self.slots.iter().sum()
    }

    /// Upper bound (ns) of the bucket holding the given percentile.
    pub fn percentile(&self, p: f64) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let target = (total as f64 * p).ceil() as u64;
        let mut acc = 0;
        for (i, &n) in self.slots.iter().enumerate() {
            acc += n;
            if acc >= target {
                return Some(1u64 << (i + 1));
            }
        }
        None
    }

    pub fn max(&self) -> Option<u64> {
        self.slots.iter().rposition(|&n| n > 0).map(|i| 1u64 << (i + 1))
    }
}

pub struct App {
    pub ncpu: usize,
    pub interval: Duration,
    pub paused: bool,
    pub view: View,
    pub sort_col: [usize; 2],
    pub sort_desc: bool,
    pub scroll: usize,
    pub hide_idle: bool,
    pub show_exits: bool,

    pub cpu_pct: Vec<f64>,
    pub cpu_total: f64,
    pub rates: Rates,
    pub hist: Histories,
    pub runq: Hist,
    pub bio: Hist,
    pub top_syscalls: Vec<(String, f64)>,
    pub top_drops: Vec<(String, f64)>,
    pub procs: Vec<ProcRow>,
    pub progs: Vec<ProgRow>,
    pub feed: VecDeque<FeedItem>,
    pub own_overhead: f64,

    syscall_names: Vec<String>,
    drop_names: Vec<String>,
    own_progs: HashSet<u32>,
    prev: Option<Snapshot>,
}

impl App {
    pub fn new(ncpu: usize, interval: Duration, own_progs: HashSet<u32>) -> Self {
        Self {
            ncpu,
            interval,
            paused: false,
            view: View::Processes,
            sort_col: [2, 5],
            sort_desc: true,
            scroll: 0,
            hide_idle: true,
            show_exits: true,
            cpu_pct: vec![0.0; ncpu],
            cpu_total: 0.0,
            rates: Rates::default(),
            hist: Histories::default(),
            runq: Hist::default(),
            bio: Hist::default(),
            top_syscalls: Vec::new(),
            top_drops: Vec::new(),
            procs: Vec::new(),
            progs: Vec::new(),
            feed: VecDeque::new(),
            own_overhead: 0.0,
            syscall_names: bpf::syscall_names(),
            drop_names: bpf::drop_reason_names(),
            own_progs,
            prev: None,
        }
    }

    pub fn push_feed(&mut self, item: FeedItem) {
        if self.feed.len() == FEED_LEN {
            self.feed.pop_back();
        }
        self.feed.push_front(item);
    }

    pub fn update(&mut self, cur: Snapshot) {
        let Some(prev) = self.prev.take() else {
            self.prev = Some(cur);
            return;
        };
        let dt_ns = cur.ts_ns.saturating_sub(prev.ts_ns).max(1) as f64;
        let dt = dt_ns / 1e9;
        let rate = |c: Counter| cur.counters[c as usize].saturating_sub(prev.counters[c as usize]) as f64 / dt;

        for (i, pct) in self.cpu_pct.iter_mut().enumerate() {
            let d = cur.cpu_busy_ns[i].saturating_sub(prev.cpu_busy_ns[i]) as f64;
            *pct = (d / dt_ns * 100.0).clamp(0.0, 100.0);
        }
        self.cpu_total = self.cpu_pct.iter().sum::<f64>() / self.ncpu as f64;

        let drop_rates: Vec<(usize, f64)> = cur
            .drops
            .iter()
            .zip(&prev.drops)
            .enumerate()
            .filter(|(i, _)| !matches!(self.drop_names[*i].as_str(), "SKB_NOT_DROPPED_YET" | "SKB_CONSUMED"))
            .map(|(i, (c, p))| (i, c.saturating_sub(*p) as f64 / dt))
            .filter(|&(_, r)| r > 0.0)
            .collect();

        self.rates = Rates {
            csw: rate(Counter::Csw),
            wakeups: rate(Counter::Wakeups),
            syscalls: rate(Counter::Syscalls),
            faults: rate(Counter::Faults),
            exec: rate(Counter::Exec),
            fork: rate(Counter::Fork),
            exit: rate(Counter::Exit),
            iops: rate(Counter::BioOps),
            rd: rate(Counter::BioRdBytes),
            wr: rate(Counter::BioWrBytes),
            tx: rate(Counter::TcpTx),
            rx: rate(Counter::TcpRx),
            retrans: rate(Counter::Retrans),
            drops: drop_rates.iter().map(|(_, r)| r).sum(),
        };

        for (i, s) in self.runq.slots.iter_mut().enumerate() {
            *s = cur.runq_hist[i].saturating_sub(prev.runq_hist[i]);
        }
        for (i, s) in self.bio.slots.iter_mut().enumerate() {
            *s = cur.bio_hist[i].saturating_sub(prev.bio_hist[i]);
        }

        let mut sc: Vec<(String, f64)> = cur
            .syscalls
            .iter()
            .zip(&prev.syscalls)
            .enumerate()
            .map(|(i, (c, p))| (self.syscall_names[i].clone(), c.saturating_sub(*p) as f64 / dt))
            .filter(|(_, r)| *r > 0.0)
            .collect();
        sc.sort_by(|a, b| b.1.total_cmp(&a.1));
        self.top_syscalls = sc;

        let mut dr: Vec<(String, f64)> =
            drop_rates.into_iter().map(|(i, r)| (self.drop_names[i].clone(), r)).collect();
        dr.sort_by(|a, b| b.1.total_cmp(&a.1));
        self.top_drops = dr;

        self.procs = proc_rows(&prev, &cur, dt);
        self.progs = prog_rows(&prev.progs, &cur.progs, dt, &self.own_progs);
        self.own_overhead = self.progs.iter().filter(|p| p.ours).map(|p| p.cpu).sum::<f64>() / self.ncpu as f64;
        self.sort();

        let r = self.rates;
        push(&mut self.hist.cpu, self.cpu_total);
        push(&mut self.hist.syscalls, r.syscalls);
        push(&mut self.hist.csw, r.csw);
        push(&mut self.hist.runq_p99, self.runq.percentile(0.99).unwrap_or(0) as f64);
        push(&mut self.hist.disk, r.rd + r.wr);
        push(&mut self.hist.net, r.tx + r.rx);

        self.prev = Some(cur);
    }

    pub fn sort(&mut self) {
        let desc = self.sort_desc;
        match self.view {
            View::Processes => {
                let col = self.sort_col[0];
                self.procs.sort_by(|a, b| {
                    let o = if col == 1 { a.comm.cmp(&b.comm) } else { a.key(col).total_cmp(&b.key(col)) };
                    if desc { o.reverse() } else { o }
                });
            }
            View::Programs => {
                let col = self.sort_col[1];
                self.progs.sort_by(|a, b| {
                    let o = match col {
                        1 => a.ty.cmp(&b.ty),
                        2 => a.name.cmp(&b.name),
                        _ => a.key(col).total_cmp(&b.key(col)),
                    };
                    if desc { o.reverse() } else { o }
                });
            }
        }
    }

    pub fn visible_procs(&self) -> impl Iterator<Item = &ProcRow> {
        self.procs.iter().filter(move |p| !self.hide_idle || !p.idle())
    }

    pub fn columns(&self) -> &'static [&'static str] {
        match self.view {
            View::Processes => &ProcRow::COLUMNS,
            View::Programs => &ProgRow::COLUMNS,
        }
    }

    pub fn sort_col(&self) -> usize {
        self.sort_col[self.view as usize]
    }

    pub fn cycle_sort(&mut self, forward: bool) {
        let n = self.columns().len();
        let c = &mut self.sort_col[self.view as usize];
        *c = if forward { (*c + 1) % n } else { (*c + n - 1) % n };
        self.sort();
    }
}

fn proc_rows(prev: &Snapshot, cur: &Snapshot, dt: f64) -> Vec<ProcRow> {
    let empty = bpf::Pstat::default();
    cur.procs
        .iter()
        .map(|(&pid, e)| {
            let p = prev.procs.get(&pid).map_or(&empty, |p| &p.stat);
            let c = &e.stat;
            let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
            let runq_cnt = d(c.runq_cnt, p.runq_cnt);
            ProcRow {
                pid,
                comm: e.comm.clone(),
                cpu: d(c.oncpu_ns, p.oncpu_ns) / (dt * 1e9) * 100.0,
                syscalls: d(c.syscalls, p.syscalls) / dt,
                csw: d(c.csw, p.csw) / dt,
                runq_avg_ns: if runq_cnt > 0.0 { d(c.runq_ns, p.runq_ns) / runq_cnt } else { 0.0 },
                faults: d(c.faults, p.faults) / dt,
                rd: d(c.rd_bytes, p.rd_bytes) / dt,
                wr: d(c.wr_bytes, p.wr_bytes) / dt,
                tx: d(c.tx_bytes, p.tx_bytes) / dt,
                rx: d(c.rx_bytes, p.rx_bytes) / dt,
            }
        })
        .collect()
}

fn prog_rows(
    prev: &HashMap<u32, ProgStat>,
    cur: &HashMap<u32, ProgStat>,
    dt: f64,
    own: &HashSet<u32>,
) -> Vec<ProgRow> {
    cur.values()
        .map(|c| {
            let (pt, pc) = prev.get(&c.id).map_or((c.run_time_ns, c.run_cnt), |p| (p.run_time_ns, p.run_cnt));
            let dtime = c.run_time_ns.saturating_sub(pt) as f64;
            let dcnt = c.run_cnt.saturating_sub(pc) as f64;
            ProgRow {
                id: c.id,
                ty: c.ty.clone(),
                name: c.name.clone(),
                events: dcnt / dt,
                avg_ns: if dcnt > 0.0 { dtime / dcnt } else { 0.0 },
                cpu: dtime / (dt * 1e9) * 100.0,
                ours: own.contains(&c.id),
            }
        })
        .collect()
}

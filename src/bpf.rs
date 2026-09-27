//! Loading the BPF object and reading raw cumulative state out of it.

use std::collections::{HashMap, HashSet};
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::ptr;

use anyhow::{Context, Result};
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::{MapCore, MapFlags, OpenObject};

mod skel {
    include!(concat!(env!("OUT_DIR"), "/ebtop.skel.rs"));
}
pub use skel::EbtopSkel;
pub use skel::types;

// Mirrors of the constants in src/bpf/ebtop.h.
pub const MAX_CPUS: usize = 128;
pub const HIST_SLOTS: usize = 32;
pub const NR_SYSCALLS: usize = 512;
pub const NR_DROP_REASONS: usize = 256;
pub const NR_DROP_SUBSYS: usize = 16;
pub const DROP_SUBSYS_BASE: usize = NR_DROP_REASONS - NR_DROP_SUBSYS;
pub const NR_COUNTERS: usize = 16;

pub const EV_EXEC: u32 = 1;
pub const EV_EXIT: u32 = 2;

/// Mirror of `struct event` (ring buffer records).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Event {
    pub kind: u32,
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub exit_code: i32,
    pub args_len: u32,
    pub dur_ns: u64,
    pub comm: [i8; 16],
    pub args: [u8; 256],
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Counter {
    Csw,
    Wakeups,
    Syscalls,
    Faults,
    Exec,
    Fork,
    Exit,
    BioOps,
    BioRdBytes,
    BioWrBytes,
    TcpTx,
    TcpRx,
    Retrans,
}

pub fn load(open_object: &mut MaybeUninit<OpenObject>) -> Result<EbtopSkel<'_>> {
    #[allow(unused_mut)]
    let mut open = skel::EbtopSkelBuilder::default().open(open_object).context("opening BPF object")?;
    // exceptions:page_fault_user only exists on x86
    #[cfg(not(target_arch = "x86_64"))]
    open.progs.on_page_fault.set_autoload(false);
    let mut skel = open.load().context("loading BPF programs (are you root?)")?;
    skel.attach().context("attaching BPF programs")?;
    Ok(skel)
}

/// Turns on kernel-wide BPF run-time stats (run_cnt / run_time_ns) for as long
/// as the returned fd stays open.
pub fn enable_stats() -> Option<OwnedFd> {
    let fd = unsafe { libbpf_sys::bpf_enable_stats(libbpf_sys::BPF_STATS_RUN_TIME) };
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

pub fn own_prog_ids(skel: &EbtopSkel) -> HashSet<u32> {
    use libbpf_rs::ProgramMut;
    let p = &skel.progs;
    let progs: [&ProgramMut; 15] = [
        &p.on_sched_switch,
        &p.on_sched_wakeup,
        &p.on_sched_wakeup_new,
        &p.on_exec,
        &p.on_fork,
        &p.on_exit,
        &p.on_sys_enter,
        &p.on_page_fault,
        &p.on_bio_queue,
        &p.on_rq_issue,
        &p.on_rq_complete,
        &p.on_tcp_sendmsg,
        &p.on_tcp_cleanup_rbuf,
        &p.on_tcp_retransmit,
        &p.on_kfree_skb,
    ];
    progs.iter().filter_map(|prog| prog_info(prog.as_fd().as_raw_fd()).map(|i| i.id)).collect()
}

fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[derive(Clone, Copy, Default)]
pub struct Pstat {
    pub oncpu_ns: u64,
    pub runq_ns: u64,
    pub runq_cnt: u64,
    pub csw: u64,
    pub syscalls: u64,
    pub faults: u64,
    pub rd_bytes: u64,
    pub wr_bytes: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
}

pub struct ProcEntry {
    pub comm: String,
    pub stat: Pstat,
}

#[derive(Clone)]
pub struct ProgStat {
    pub id: u32,
    pub ty: String,
    pub name: String,
    pub run_time_ns: u64,
    pub run_cnt: u64,
}

/// Cumulative values at one point in time. Rates are differences of two of these.
pub struct Snapshot {
    pub ts_ns: u64,
    pub counters: [u64; NR_COUNTERS],
    pub cpu_busy_ns: Vec<u64>,
    pub runq_hist: [u64; HIST_SLOTS],
    pub bio_hist: [u64; HIST_SLOTS],
    pub syscalls: Vec<u64>,
    pub drops: Vec<u64>,
    pub procs: HashMap<u32, ProcEntry>,
    pub progs: HashMap<u32, ProgStat>,
}

pub struct Reader {
    bss: *const types::bss,
    ncpu: usize,
    /// tgids already reported as gone once; removed from the map on the next pass
    dead: HashSet<u32>,
    prog_names: HashMap<u32, String>,
}

impl Reader {
    pub fn new(skel: &mut EbtopSkel) -> Result<Self> {
        let bss = skel.maps.bss_data.take().context("BPF object has no .bss")?;
        let ncpu = (unsafe { libbpf_sys::libbpf_num_possible_cpus() }.max(1) as usize).min(MAX_CPUS);
        Ok(Self { bss: bss as *const _, ncpu, dead: HashSet::new(), prog_names: HashMap::new() })
    }

    pub fn ncpu(&self) -> usize {
        self.ncpu
    }

    pub fn read(&mut self, skel: &EbtopSkel) -> Snapshot {
        // SAFETY: the .bss mmap lives as long as the skeleton. BPF programs write
        // it concurrently; aligned u64 reads can't tear on the supported arches,
        // and a slightly stale value is fine for monitoring.
        let bss = unsafe { &*self.bss };
        let mut snap = Snapshot {
            ts_ns: 0,
            counters: [0; NR_COUNTERS],
            cpu_busy_ns: vec![0; self.ncpu],
            runq_hist: [0; HIST_SLOTS],
            bio_hist: [0; HIST_SLOTS],
            syscalls: vec![0; NR_SYSCALLS],
            drops: vec![0; NR_DROP_REASONS],
            procs: HashMap::new(),
            progs: HashMap::new(),
        };

        let mut running: Vec<(u32, u64)> = Vec::new();
        let mut cpu_states = Vec::with_capacity(self.ncpu);
        for cpu in 0..self.ncpu {
            cpu_states.push(unsafe { ptr::read_volatile(&bss.cpus[cpu]) });
            for (acc, v) in snap.counters.iter_mut().zip(vol(&bss.counters[cpu])) {
                *acc += v;
            }
            for (acc, v) in snap.runq_hist.iter_mut().zip(vol(&bss.runq_hist[cpu])) {
                *acc += v;
            }
            for (acc, v) in snap.bio_hist.iter_mut().zip(vol(&bss.bio_hist[cpu])) {
                *acc += v;
            }
            for (acc, v) in snap.syscalls.iter_mut().zip(vol(&bss.syscall_cnt[cpu])) {
                *acc += v;
            }
            for (acc, v) in snap.drops.iter_mut().zip(vol(&bss.drop_cnt[cpu])) {
                *acc += v;
            }
        }

        // Read the clock after the cpu states so `now >= start` holds.
        snap.ts_ns = monotonic_ns();
        for (cpu, cs) in cpu_states.iter().enumerate() {
            let mut busy = cs.busy_ns;
            // Credit the slice that is still running: a CPU pinned at 100% by a
            // single task may not context switch for a long time.
            if cs.cur_tgid != 0 && cs.start != 0 {
                let running_for = snap.ts_ns.saturating_sub(cs.start);
                busy += running_for;
                running.push((cs.cur_tgid, running_for));
            }
            snap.cpu_busy_ns[cpu] = busy;
        }

        self.read_procs(skel, &mut snap);
        for (tgid, ns) in running {
            if let Some(p) = snap.procs.get_mut(&tgid) {
                p.stat.oncpu_ns += ns;
            }
        }
        snap.progs = prog_stats(&mut self.prog_names).into_iter().map(|p| (p.id, p)).collect();
        snap
    }

    fn read_procs(&mut self, skel: &EbtopSkel, snap: &mut Snapshot) {
        let map = &skel.maps.pstats;
        let mut gone = Vec::new();
        for key in map.keys() {
            let Ok(Some(val)) = map.lookup(&key, MapFlags::ANY) else { continue };
            if val.len() < size_of::<types::pstat>() || key.len() != 4 {
                continue;
            }
            let tgid = u32::from_ne_bytes(key[..4].try_into().unwrap());
            // SAFETY: length checked above; the value is a struct pstat.
            let raw: types::pstat = unsafe { ptr::read_unaligned(val.as_ptr().cast()) };
            let comm_bytes: Vec<u8> = raw.comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            snap.procs.insert(
                tgid,
                ProcEntry {
                    comm: String::from_utf8_lossy(&comm_bytes).into_owned(),
                    stat: Pstat {
                        oncpu_ns: raw.oncpu_ns,
                        runq_ns: raw.runq_ns,
                        runq_cnt: raw.runq_cnt,
                        csw: raw.csw,
                        syscalls: raw.syscalls,
                        faults: raw.faults,
                        rd_bytes: raw.rd_bytes,
                        wr_bytes: raw.wr_bytes,
                        tx_bytes: raw.tx_bytes,
                        rx_bytes: raw.rx_bytes,
                    },
                },
            );
            if !Path::new(&format!("/proc/{tgid}")).exists() {
                gone.push(tgid);
            }
        }

        // A process that exited is reported for one more interval (so its last
        // activity is counted), then dropped from the map.
        let mut still_dead = HashSet::new();
        for tgid in gone {
            if self.dead.contains(&tgid) {
                let _ = map.delete(&tgid.to_ne_bytes());
                snap.procs.remove(&tgid);
            } else {
                still_dead.insert(tgid);
            }
        }
        self.dead = still_dead;
    }
}

fn vol<const N: usize>(arr: &[u64; N]) -> impl Iterator<Item = u64> + '_ {
    arr.iter().map(|v| unsafe { ptr::read_volatile(v) })
}

struct ProgInfo {
    id: u32,
    ty: u32,
    name: String,
    run_time_ns: u64,
    run_cnt: u64,
    btf_id: u32,
    nr_func_info: u32,
}

fn prog_info(fd: i32) -> Option<ProgInfo> {
    let mut info: libbpf_sys::bpf_prog_info = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libbpf_sys::bpf_prog_info>() as u32;
    let rc = unsafe { libbpf_sys::bpf_prog_get_info_by_fd(fd, &mut info, &mut len) };
    if rc != 0 {
        return None;
    }
    let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy().into_owned();
    Some(ProgInfo {
        id: info.id,
        ty: info.type_,
        name,
        run_time_ns: info.run_time_ns,
        run_cnt: info.run_cnt,
        btf_id: info.btf_id,
        nr_func_info: info.nr_func_info,
    })
}

/// The kernel truncates program names to 15 chars; the untruncated name is
/// the program's main function in its BTF.
fn btf_prog_name(fd: i32, info: &ProgInfo) -> Option<String> {
    if info.btf_id == 0 || info.nr_func_info == 0 {
        return None;
    }
    unsafe {
        let mut func: libbpf_sys::bpf_func_info = std::mem::zeroed();
        let mut req: libbpf_sys::bpf_prog_info = std::mem::zeroed();
        req.nr_func_info = 1;
        req.func_info_rec_size = size_of::<libbpf_sys::bpf_func_info>() as u32;
        req.func_info = &mut func as *mut _ as u64;
        let mut len = size_of::<libbpf_sys::bpf_prog_info>() as u32;
        if libbpf_sys::bpf_prog_get_info_by_fd(fd, &mut req, &mut len) != 0 {
            return None;
        }
        let btf = libbpf_sys::btf__load_from_kernel_by_id(info.btf_id);
        if btf.is_null() {
            return None;
        }
        let t = libbpf_sys::btf__type_by_id(btf, func.type_id);
        let name = (!t.is_null())
            .then(|| CStr::from_ptr(libbpf_sys::btf__name_by_offset(btf, (*t).name_off)).to_string_lossy().into_owned())
            .filter(|n| !n.is_empty());
        libbpf_sys::btf__free(btf);
        name
    }
}

/// All BPF programs loaded on the system, like `bpftool prog show`.
/// `names` caches the resolved full names by program id.
pub fn prog_stats(names: &mut HashMap<u32, String>) -> Vec<ProgStat> {
    let mut out = Vec::new();
    let mut id = 0u32;
    while unsafe { libbpf_sys::bpf_prog_get_next_id(id, &mut id) } == 0 {
        let fd = unsafe { libbpf_sys::bpf_prog_get_fd_by_id(id) };
        if fd < 0 {
            continue;
        }
        let Some(info) = prog_info(fd) else {
            unsafe { libc::close(fd) };
            continue;
        };
        let name =
            names.entry(id).or_insert_with(|| btf_prog_name(fd, &info).unwrap_or_else(|| info.name.clone())).clone();
        unsafe { libc::close(fd) };
        let ty = unsafe { libbpf_sys::libbpf_bpf_prog_type_str(info.ty) };
        let ty = if ty.is_null() {
            format!("type {}", info.ty)
        } else {
            unsafe { CStr::from_ptr(ty) }.to_string_lossy().into_owned()
        };
        out.push(ProgStat { id, ty, name, run_time_ns: info.run_time_ns, run_cnt: info.run_cnt });
    }
    names.retain(|id, _| out.iter().any(|p| p.id == *id));
    out
}

mod syscalls {
    include!(concat!(env!("OUT_DIR"), "/syscalls.rs"));
}

/// Syscall number -> name, from the UAPI headers embedded at build time.
pub fn syscall_names() -> Vec<String> {
    let mut names: Vec<String> = (0..NR_SYSCALLS).map(|n| format!("#{n}")).collect();
    for &(num, name) in syscalls::SYSCALLS {
        if let Some(slot) = names.get_mut(num as usize) {
            *slot = name.to_string();
        }
    }
    names
}

/// Drop-reason bucket -> name, read from the kernel's own BTF
/// (`enum skb_drop_reason`, plus `enum skb_drop_reason_subsys` for the
/// per-subsystem buckets at the top).
pub fn drop_reason_names() -> Vec<String> {
    let mut names: Vec<String> = (0..NR_DROP_REASONS).map(|n| format!("reason {n}")).collect();
    names[DROP_SUBSYS_BASE] = "other".into();
    unsafe {
        let btf = libbpf_sys::btf__load_vmlinux_btf();
        if btf.is_null() {
            return names;
        }
        for (v, name) in btf_enum(btf, c"skb_drop_reason") {
            if v < DROP_SUBSYS_BASE {
                names[v] = name.strip_prefix("SKB_DROP_REASON_").unwrap_or(&name).to_string();
            }
        }
        for (v, name) in btf_enum(btf, c"skb_drop_reason_subsys") {
            if v > 0 && v < NR_DROP_SUBSYS {
                let name = name.strip_prefix("SKB_DROP_REASON_SUBSYS_").unwrap_or(&name);
                names[DROP_SUBSYS_BASE + v] = format!("{name} (subsystem)");
            }
        }
        libbpf_sys::btf__free(btf);
    }
    names
}

/// (value, name) of every member of a BTF enum.
unsafe fn btf_enum(btf: *mut libbpf_sys::btf, name: &CStr) -> Vec<(usize, String)> {
    let id = unsafe { libbpf_sys::btf__find_by_name_kind(btf, name.as_ptr(), libbpf_sys::BTF_KIND_ENUM) };
    if id <= 0 {
        return Vec::new();
    }
    unsafe {
        let t = libbpf_sys::btf__type_by_id(btf, id as u32);
        let vlen = ((*t).info & 0xffff) as usize;
        // struct btf_enum entries follow the struct btf_type header
        let members = t.add(1).cast::<libbpf_sys::btf_enum>();
        (0..vlen)
            .map(|i| {
                let m = &*members.add(i);
                let name = CStr::from_ptr(libbpf_sys::btf__name_by_offset(btf, m.name_off)).to_string_lossy();
                (m.val as u32 as usize, name.into_owned())
            })
            .collect()
    }
}

// SPDX-License-Identifier: GPL-2.0
#include <linux/types.h>
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>
#include "ebtop.h"

char LICENSE[] SEC("license") = "GPL";

/*
 * Minimal CO-RE views of the kernel structs we touch. libbpf relocates the
 * field offsets against the running kernel's BTF at load time, so there is
 * no need for a generated vmlinux.h.
 */
struct mm_struct {
	unsigned long arg_start;
	unsigned long arg_end;
} __attribute__((preserve_access_index));

struct task_struct {
	int pid;
	int tgid;
	int exit_code;
	__u64 start_time;
	struct task_struct *real_parent;
	struct task_struct *group_leader;
	struct mm_struct *mm;
	char comm[TASK_COMM_LEN];
} __attribute__((preserve_access_index));

struct bvec_iter {
	unsigned int bi_size;
} __attribute__((preserve_access_index));

struct bio {
	unsigned int bi_opf;
	struct bvec_iter bi_iter;
} __attribute__((preserve_access_index));

struct request;
struct sock;
struct msghdr;
struct sk_buff;
struct pt_regs;
struct linux_binprm;

#define REQ_OP_MASK 0xff
#define REQ_OP_READ 0
#define REQ_OP_WRITE 1

/* Global state lives in .bss: userspace reads it through the mmap'd skeleton
 * without any syscalls. Arrays indexed by CPU are written only by that CPU. */
struct cpu_state cpus[MAX_CPUS];
__u64 counters[MAX_CPUS][NR_COUNTERS];
__u64 runq_hist[MAX_CPUS][HIST_SLOTS];
__u64 bio_hist[MAX_CPUS][HIST_SLOTS];
__u64 syscall_cnt[MAX_CPUS][NR_SYSCALLS];
__u64 drop_cnt[MAX_CPUS][NR_DROP_REASONS];

struct {
	__uint(type, BPF_MAP_TYPE_LRU_HASH);
	__uint(max_entries, 32768);
	__type(key, __u32);
	__type(value, struct pstat);
} pstats SEC(".maps");

/* per-task ktime it became runnable; task-local storage avoids hashing on
 * the wakeup/switch hot path and is freed with the task */
struct {
	__uint(type, BPF_MAP_TYPE_TASK_STORAGE);
	__uint(map_flags, BPF_F_NO_PREALLOC);
	__type(key, int);
	__type(value, __u64);
} enqueued SEC(".maps");

/* struct request * -> ktime issued to the device */
struct {
	__uint(type, BPF_MAP_TYPE_LRU_HASH);
	__uint(max_entries, 16384);
	__type(key, __u64);
	__type(value, __u64);
} rq_start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(max_entries, 1 << 20);
} events SEC(".maps");

static __always_inline __u32 log2_u64(__u64 v)
{
	__u32 r, shift;

	r = (v > 0xFFFFFFFF) << 5; v >>= r;
	shift = (v > 0xFFFF) << 4; v >>= shift; r |= shift;
	shift = (v > 0xFF) << 3; v >>= shift; r |= shift;
	shift = (v > 0xF) << 2; v >>= shift; r |= shift;
	shift = (v > 0x3) << 1; v >>= shift; r |= shift;
	r |= (v >> 1);
	return r;
}

static __always_inline __u32 hist_slot(__u64 ns)
{
	__u32 s = log2_u64(ns);

	return s < HIST_SLOTS ? s : HIST_SLOTS - 1;
}

static __always_inline __u32 this_cpu(void)
{
	return bpf_get_smp_processor_id();
}

static __always_inline struct pstat *get_pstat(__u32 tgid, struct task_struct *t)
{
	struct pstat *ps, zero = {};

	ps = bpf_map_lookup_elem(&pstats, &tgid);
	if (ps)
		return ps;
	BPF_CORE_READ_STR_INTO(&zero.comm, t, group_leader, comm);
	bpf_map_update_elem(&pstats, &tgid, &zero, BPF_NOEXIST);
	return bpf_map_lookup_elem(&pstats, &tgid);
}

static __always_inline struct pstat *current_pstat(void)
{
	return get_pstat(bpf_get_current_pid_tgid() >> 32,
			 (struct task_struct *)bpf_get_current_task());
}

/* ---------------------------------------------------------------- sched */

SEC("tp_btf/sched_switch")
int BPF_PROG(on_sched_switch, int preempt, struct task_struct *prev,
	     struct task_struct *next, unsigned int prev_state)
{
	__u64 now = bpf_ktime_get_ns(), *tsp, delta;
	__u32 cpu = this_cpu();
	__u32 prev_pid, next_pid;
	struct cpu_state *cs;
	struct pstat *ps;

	if (cpu >= MAX_CPUS)
		return 0;
	cs = &cpus[cpu];
	prev_pid = BPF_CORE_READ(prev, pid);
	next_pid = BPF_CORE_READ(next, pid);
	counters[cpu][C_CSW]++;

	/* on-CPU accounting for the task being switched out */
	if (cs->start && prev_pid) {
		delta = now - cs->start;
		cs->busy_ns += delta;
		ps = get_pstat(BPF_CORE_READ(prev, tgid), prev);
		if (ps) {
			__sync_fetch_and_add(&ps->oncpu_ns, delta);
			__sync_fetch_and_add(&ps->csw, 1);
		}
	}
	cs->start = now;
	cs->cur_tgid = next_pid ? BPF_CORE_READ(next, tgid) : 0;

	/* preempted while still runnable: it goes straight back on the queue */
	if (prev_pid && prev_state == 0) {
		tsp = bpf_task_storage_get(&enqueued, prev, 0, BPF_LOCAL_STORAGE_GET_F_CREATE);
		if (tsp)
			*tsp = now;
	}

	if (!next_pid)
		return 0;
	tsp = bpf_task_storage_get(&enqueued, next, 0, 0);
	if (!tsp || !*tsp)
		return 0;
	delta = now - *tsp;
	*tsp = 0;
	runq_hist[cpu][hist_slot(delta)]++;
	ps = get_pstat(BPF_CORE_READ(next, tgid), next);
	if (ps) {
		__sync_fetch_and_add(&ps->runq_ns, delta);
		__sync_fetch_and_add(&ps->runq_cnt, 1);
	}
	return 0;
}

static __always_inline int record_enqueue(struct task_struct *p)
{
	__u32 pid = BPF_CORE_READ(p, pid), cpu = this_cpu();
	__u64 *tsp;

	if (!pid || cpu >= MAX_CPUS)
		return 0;
	counters[cpu][C_WAKEUPS]++;
	tsp = bpf_task_storage_get(&enqueued, p, 0, BPF_LOCAL_STORAGE_GET_F_CREATE);
	if (tsp)
		*tsp = bpf_ktime_get_ns();
	return 0;
}

SEC("tp_btf/sched_wakeup")
int BPF_PROG(on_sched_wakeup, struct task_struct *p)
{
	return record_enqueue(p);
}

SEC("tp_btf/sched_wakeup_new")
int BPF_PROG(on_sched_wakeup_new, struct task_struct *p)
{
	return record_enqueue(p);
}

/* ------------------------------------------------------------ processes */

SEC("tp_btf/sched_process_exec")
int BPF_PROG(on_exec, struct task_struct *p, int old_pid, struct linux_binprm *bprm)
{
	__u32 cpu = this_cpu(), tgid = BPF_CORE_READ(p, tgid);
	unsigned long start, end;
	struct event *e;
	struct pstat *ps;
	__u64 len;

	if (cpu < MAX_CPUS)
		counters[cpu][C_EXEC]++;

	/* exec changes comm; keep the process table in sync */
	ps = bpf_map_lookup_elem(&pstats, &tgid);
	if (ps)
		bpf_get_current_comm(ps->comm, sizeof(ps->comm));

	e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
	if (!e)
		return 0;
	e->kind = EV_EXEC;
	e->pid = tgid;
	e->ppid = BPF_CORE_READ(p, real_parent, tgid);
	e->uid = (__u32)bpf_get_current_uid_gid();
	e->exit_code = 0;
	e->dur_ns = 0;
	bpf_get_current_comm(e->comm, sizeof(e->comm));

	start = BPF_CORE_READ(p, mm, arg_start);
	end = BPF_CORE_READ(p, mm, arg_end);
	len = end - start;
	if (len > ARGS_LEN)
		len = ARGS_LEN;
	e->args_len = 0;
	if (bpf_probe_read_user(e->args, len, (void *)start) == 0)
		e->args_len = len;
	bpf_ringbuf_submit(e, 0);
	return 0;
}

SEC("tp_btf/sched_process_fork")
int BPF_PROG(on_fork, struct task_struct *parent, struct task_struct *child)
{
	__u32 cpu = this_cpu();

	if (cpu < MAX_CPUS)
		counters[cpu][C_FORK]++;
	return 0;
}

SEC("tp_btf/sched_process_exit")
int BPF_PROG(on_exit, struct task_struct *p)
{
	__u32 cpu = this_cpu(), pid, tgid;
	struct event *e;

	pid = BPF_CORE_READ(p, pid);
	tgid = BPF_CORE_READ(p, tgid);
	if (pid != tgid) /* threads exiting are not interesting here */
		return 0;
	if (cpu < MAX_CPUS)
		counters[cpu][C_EXIT]++;

	e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
	if (!e)
		return 0;
	e->kind = EV_EXIT;
	e->pid = tgid;
	e->ppid = BPF_CORE_READ(p, real_parent, tgid);
	e->uid = (__u32)bpf_get_current_uid_gid();
	e->exit_code = BPF_CORE_READ(p, exit_code);
	e->dur_ns = bpf_ktime_get_ns() - BPF_CORE_READ(p, start_time);
	e->args_len = 0;
	bpf_get_current_comm(e->comm, sizeof(e->comm));
	bpf_ringbuf_submit(e, 0);
	return 0;
}

/* ------------------------------------------------------------- syscalls */

SEC("tp_btf/sys_enter")
int BPF_PROG(on_sys_enter, struct pt_regs *regs, long id)
{
	__u32 cpu = this_cpu();
	struct pstat *ps;

	if (cpu >= MAX_CPUS)
		return 0;
	counters[cpu][C_SYSCALLS]++;
	if ((__u64)id < NR_SYSCALLS)
		syscall_cnt[cpu][id]++;
	ps = current_pstat();
	if (ps)
		__sync_fetch_and_add(&ps->syscalls, 1);
	return 0;
}

SEC("tracepoint/exceptions/page_fault_user")
int on_page_fault(void *ctx)
{
	__u32 cpu = this_cpu();
	struct pstat *ps;

	if (cpu >= MAX_CPUS)
		return 0;
	counters[cpu][C_FAULTS]++;
	ps = current_pstat();
	if (ps)
		__sync_fetch_and_add(&ps->faults, 1);
	return 0;
}

/* ---------------------------------------------------------------- block */

SEC("tp_btf/block_bio_queue")
int BPF_PROG(on_bio_queue, struct bio *bio)
{
	__u32 cpu = this_cpu(), op = BPF_CORE_READ(bio, bi_opf) & REQ_OP_MASK;
	__u64 bytes = BPF_CORE_READ(bio, bi_iter.bi_size);
	struct pstat *ps;

	if (cpu >= MAX_CPUS || !bytes)
		return 0;
	if (op != REQ_OP_READ && op != REQ_OP_WRITE)
		return 0;
	counters[cpu][op == REQ_OP_READ ? C_BIO_RD_BYTES : C_BIO_WR_BYTES] += bytes;
	ps = current_pstat();
	if (!ps)
		return 0;
	if (op == REQ_OP_READ)
		__sync_fetch_and_add(&ps->rd_bytes, bytes);
	else
		__sync_fetch_and_add(&ps->wr_bytes, bytes);
	return 0;
}

SEC("tp_btf/block_rq_issue")
int BPF_PROG(on_rq_issue, struct request *rq)
{
	__u64 key = (__u64)rq, now = bpf_ktime_get_ns();

	bpf_map_update_elem(&rq_start, &key, &now, BPF_ANY);
	return 0;
}

SEC("tp_btf/block_rq_complete")
int BPF_PROG(on_rq_complete, struct request *rq, int error, unsigned int nr_bytes)
{
	__u64 key = (__u64)rq, *tsp, delta;
	__u32 cpu = this_cpu();

	tsp = bpf_map_lookup_elem(&rq_start, &key);
	if (!tsp)
		return 0;
	delta = bpf_ktime_get_ns() - *tsp;
	bpf_map_delete_elem(&rq_start, &key);
	if (cpu >= MAX_CPUS)
		return 0;
	counters[cpu][C_BIO_OPS]++;
	bio_hist[cpu][hist_slot(delta)]++;
	return 0;
}

/* -------------------------------------------------------------- network */

SEC("fentry/tcp_sendmsg")
int BPF_PROG(on_tcp_sendmsg, struct sock *sk, struct msghdr *msg, __u64 size)
{
	__u32 cpu = this_cpu();
	struct pstat *ps;

	if (cpu >= MAX_CPUS)
		return 0;
	counters[cpu][C_TCP_TX] += size;
	ps = current_pstat();
	if (ps)
		__sync_fetch_and_add(&ps->tx_bytes, size);
	return 0;
}

/* called once per recvmsg with the number of bytes copied to userspace */
SEC("fentry/tcp_cleanup_rbuf")
int BPF_PROG(on_tcp_cleanup_rbuf, struct sock *sk, int copied)
{
	__u32 cpu = this_cpu();
	struct pstat *ps;

	if (cpu >= MAX_CPUS || copied <= 0)
		return 0;
	counters[cpu][C_TCP_RX] += copied;
	ps = current_pstat();
	if (ps)
		__sync_fetch_and_add(&ps->rx_bytes, copied);
	return 0;
}

SEC("tracepoint/tcp/tcp_retransmit_skb")
int on_tcp_retransmit(void *ctx)
{
	__u32 cpu = this_cpu();

	if (cpu < MAX_CPUS)
		counters[cpu][C_RETRANS]++;
	return 0;
}

SEC("tp_btf/kfree_skb")
int BPF_PROG(on_kfree_skb, struct sk_buff *skb, void *location, unsigned int reason)
{
	__u32 cpu = this_cpu();

	if (cpu >= MAX_CPUS)
		return 0;
	/* subsystem-specific reasons (e.g. mac80211) carry the subsystem id in
	 * the high bits; bucket them per subsystem at the top of the array */
	if (reason >> DROP_SUBSYS_SHIFT)
		reason = DROP_SUBSYS_BASE + ((reason >> DROP_SUBSYS_SHIFT) & (NR_DROP_SUBSYS - 1));
	else if (reason >= DROP_SUBSYS_BASE)
		reason = DROP_SUBSYS_BASE;
	if (reason >= NR_DROP_REASONS)
		return 0;
	drop_cnt[cpu][reason]++;
	return 0;
}

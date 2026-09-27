/* Shared between ebtop.bpf.c and the Rust side (mirrored in src/bpf.rs). */
#ifndef __EBTOP_H
#define __EBTOP_H

#define MAX_CPUS 128
#define HIST_SLOTS 32 /* log2(ns) buckets: slot n covers [2^n, 2^(n+1)) ns */
#define NR_SYSCALLS 512
#define NR_DROP_REASONS 256
#define NR_DROP_SUBSYS 16 /* last 16 drop buckets are per-subsystem */
#define DROP_SUBSYS_BASE (NR_DROP_REASONS - NR_DROP_SUBSYS)
#define DROP_SUBSYS_SHIFT 16
#define TASK_COMM_LEN 16
#define ARGS_LEN 256

enum counter {
	C_CSW,
	C_WAKEUPS,
	C_SYSCALLS,
	C_FAULTS,
	C_EXEC,
	C_FORK,
	C_EXIT,
	C_BIO_OPS,
	C_BIO_RD_BYTES,
	C_BIO_WR_BYTES,
	C_TCP_TX,
	C_TCP_RX,
	C_RETRANS,
	NR_COUNTERS = 16,
};

/* One cache line per CPU so the sched_switch hot path never false-shares. */
struct cpu_state {
	__u64 start;    /* ktime of the last context switch on this CPU */
	__u64 busy_ns;  /* cumulative non-idle time */
	__u32 cur_tgid; /* tgid currently running, 0 = idle */
	__u32 _pad0;
	__u64 _pad1[5];
};

/* Cumulative per-process (tgid) counters. */
struct pstat {
	__u64 oncpu_ns;
	__u64 runq_ns;
	__u64 runq_cnt;
	__u64 csw;
	__u64 syscalls;
	__u64 faults;
	__u64 rd_bytes;
	__u64 wr_bytes;
	__u64 tx_bytes;
	__u64 rx_bytes;
	char comm[TASK_COMM_LEN];
};

enum event_kind {
	EV_EXEC = 1,
	EV_EXIT = 2,
};

struct event {
	__u32 kind;
	__u32 pid;
	__u32 ppid;
	__u32 uid;
	__s32 exit_code;
	__u32 args_len;
	__u64 dur_ns;
	char comm[TASK_COMM_LEN];
	char args[ARGS_LEN];
};

#endif /* __EBTOP_H */

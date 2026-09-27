# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.9.0] - 2026-09-27

### Added

- btop-style TUI with cpu, scheduler, disk, network, syscalls, processes and
  exec/exit panels, all fed by eBPF.
- Run-queue and block I/O latency histograms with p50/p99/max.
- Packet drops broken down by kernel drop reason, including per-subsystem
  reasons such as mac80211.
- bpftop-style view of every loaded BPF program with events/s, average
  runtime and CPU%, with full (untruncated) program names.
- Live exec/exit feed with full argv, exit status and process lifetime.
- `--dump` for a one-shot text summary, `-i` for the refresh interval,
  `--version`.
- Release binaries for x86_64 and aarch64 with libbpf, libelf and zlib linked
  statically.

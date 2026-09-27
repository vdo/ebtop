# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- btop color themes: all 41 themes shipped with btop plus its built-in
  Default and TTY themes are bundled, and installed/user btop themes are
  picked up. The theme follows btop's own `color_theme` setting by default.
  `-t/--theme`, `--transparent`, `--list-themes`, and `t`/`T` to cycle live.
- Zoom a panel to full screen by clicking it or pressing `1`–`7`; click
  again (or press the key / `Esc`) to return to the overview. Panels carry
  btop-style superscript numbers.
- Mouse wheel scrolls the process / BPF program table.

### Fixed

- `ebtop --dump | head` and `--list-themes | head` no longer panic on a
  closed pipe.

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

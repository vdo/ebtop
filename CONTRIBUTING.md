# Contributing to ebtop

Thanks for helping out! Bug reports with `sudo ebtop --dump` output and your
kernel version are just as valuable as code.

## Development setup

You need Linux with BTF (`/sys/kernel/btf/vmlinux`), Rust (see `rust-version`
in `Cargo.toml`), `clang`, and the libelf/zlib development packages:

```sh
# Debian/Ubuntu
sudo apt install clang llvm libelf-dev zlib1g-dev pkg-config
# Arch
sudo pacman -S clang llvm libelf zlib rustup
# Fedora
sudo dnf install clang llvm elfutils-libelf-devel zlib-devel
```

The repo includes a `mise.toml`, so `mise install` sets up the Rust toolchain.

```sh
cargo build
sudo ./target/debug/ebtop          # TUI
sudo ./target/debug/ebtop --dump   # one text sample, handy while iterating on BPF code
```

## Before opening a PR

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
sudo ./target/debug/ebtop --dump
```

CI runs the same checks, plus it loads the BPF programs on x86_64 and
aarch64 runner kernels, verifies the minimum supported Rust version, and runs
`cargo-deny`.

## Project layout

| Path | What |
|---|---|
| `src/bpf/ebtop.bpf.c` | The BPF programs (GPL-2.0) |
| `src/bpf/ebtop.h` | Constants and structs shared with Rust |
| `build.rs` | Compiles the BPF object into a skeleton; embeds the syscall table |
| `src/bpf.rs` | Loading, reading snapshots, system-wide BPF program stats |
| `src/app.rs` | Snapshot diffs → rates, histograms, table rows |
| `src/ui.rs` | ratatui rendering (plus offscreen render tests) |
| `src/theme.rs` | btop theme loading, discovery and config lookup |
| `themes/` | btop's theme files, embedded at build time (Apache-2.0, see its README) |
| `src/main.rs` | CLI, event loop, `--dump` |

## Working on the BPF side

- **Keep `ebtop.h` and `src/bpf.rs` in sync.** The constants and the `Event`
  struct are mirrored by hand. Structs used as map values (e.g. `struct pstat`)
  come from the generated skeleton types.
- **No vmlinux.h.** Declare the minimal kernel struct fields you need with
  `__attribute__((preserve_access_index))`. libbpf relocates them at load time.
- **Mind the hot paths.** `sched_switch`, `sched_wakeup` and `sys_enter` fire
  hundreds of thousands of times per second on busy machines. Prefer per-CPU
  `.bss` slots over shared counters, and task-local storage over hash maps.
  Check the per-program cost in the "bpf programs" view (tab) or
  `--dump`, and mention it in your PR.
- **Probes must work on older kernels too.** The floor is 5.18. If a hook is
  arch- or version-specific, disable it with `set_autoload(false)` rather than
  failing the whole load (see `on_page_fault` in `src/bpf.rs`).

## Releasing

1. Move the "Unreleased" entries in `CHANGELOG.md` under a new version heading.
2. Bump `version` in `Cargo.toml` and run `cargo check` to update `Cargo.lock`.
3. Commit, tag `vX.Y.Z`, and push the tag. The release workflow builds
   static-libbpf binaries for x86_64 and aarch64, smoke-tests them on real
   kernels, and publishes a GitHub release with the changelog section as notes.

## License

By contributing you agree that your contributions are licensed as described in
the README: GPL-2.0 for `src/bpf/`, MIT OR Apache-2.0 for everything else.

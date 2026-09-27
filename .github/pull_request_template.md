## What and why

<!-- What does this change, and what problem does it solve? -->

## Testing

<!-- Kernel version(s) you ran it on, and how. For BPF changes, include the
relevant lines of `sudo ebtop --dump`, especially the overhead of any new or
changed programs. -->

- [ ] `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass
- [ ] Ran `sudo ebtop --dump` (and the TUI, for UI changes) on a real kernel
- [ ] `CHANGELOG.md` updated under "Unreleased" for user-visible changes

# Security policy

ebtop runs as root and loads programs into the kernel, so security reports
are taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Report privately through GitHub:
[Security → Report a vulnerability](https://github.com/vdo/ebtop/security/advisories/new).

Include the ebtop version, kernel version, and steps to reproduce. You should
get a response within a week.

## Supported versions

Only the latest release receives fixes.

## Scope

In scope, for example:

- Anything that lets an unprivileged user influence what ebtop (running as
  root) reads, writes or executes
- Information exposure beyond what a root-run monitoring tool is expected to
  show
- Crashes or hangs that can be triggered by other processes on the system

The kernel's own BPF verifier and the libbpf library are out of scope. Please
report those upstream.

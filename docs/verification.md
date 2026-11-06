# Verification

Verified locally on 2026-11-06, macOS arm64 and Linux aarch64, using Rust 1.99.1.

## Completed checks

- Formatting and Clippy with warnings denied.
- One duration unit test and sixteen process integration tests. The suite includes
  expiry without a continuing CLI call, TERM-resistant descendants, leader exit,
  explicit extension, observation without renewal, keeper/guardian crash cleanup,
  bounded output, idempotent/concurrent retries, unavailable control refusing to
  signal an unrelated PID, private files and inherited directory/environment.
  Added regressions cover metadata permission failures after spawn, partial
  control requests during expiry, empty launch reservations, commands during
  shutdown grace, and output truncation during shutdown.
- All seventeen tests passed on macOS and in a Linux aarch64 container as a
  non-root user (UID 501), including the permission-failure regression. Final Linux
  validation used native tmpfs state rather than a VM shared directory.
- Skill frontmatter and structure validated with the skill-creator validator.
- Real `npm run dev` smoke tests for Vite 8.3.6 and Next.js 16.4.5, using Node
  24.18.0. Both served HTTP 200 before expiry, reached `expired`, and released their
  listening ports. All fixtures used isolated state and were cleaned up.
- `cargo check --locked --target x86_64-unknown-linux-gnu` passed. This type-checks
  the Linux target; it is not a Linux runtime result.
- An optimized macOS release binary builds locally.
- Claude Code's local `/context` command listed the project `CLAUDE.md` and its
  imported `AGENTS.md`, confirming the intended context-maintenance discovery
  route for the observed invocation. No model request was needed for this check.

## Reproduction

Use the commands in [development instructions](../README.md#development).
Integration fixtures require `/usr/bin/python3`. The macOS and Ubuntu CI matrix
is defined in [the workflow](../.github/workflows/ci.yml); it has not run on a
hosted repository yet.

## Review corrections

Independent Astra and Fable reviews identified six issues. Cleanup guards now
cover post-spawn metadata failures; nonblocking control requests have absolute
time budgets; provably empty launch reservations do not block keyed runs;
shutdown controls reconcile lifecycle state; an already-open capture marker
preserves shutdown truncation; and the skill assigns retry keys on the initial
launch. Focused Astra and Fable reviews found no remaining P1/P2 issues in these
fixes.
Fable identified a P3 residual: a reservation containing only an interrupted
atomic-write temporary file still blocked keyed runs. The coordinator corrected
that case with conservative file-name/type/permission checks and regressions
that retain rejection of symlinks, directories and ambiguous state. That final
refinement was validated locally, without another independent review round.

## Filesystem compatibility

A Linux rerun using a macOS-to-Linux OrbStack bind mount failed intermittently
while reading atomically replaced metadata. An independent Python replacement
probe reproduced 874 missing-file reads across 1000 replacements on that shared
mount, and zero on native Linux storage. Thirty quick-exit ProcLease launches
also passed on native storage. The final Linux suite used native tmpfs state.
Use native storage for state in Linux VMs/containers; the observed shared-mount
semantics are unsupported. No workaround that guesses missing lifecycle state
was added.

## Limits

No direct system sleep/wake, clock-change, simultaneous destruction of both
supervisors or restricted Codex/Claude execution-profile test has
been completed. A service can deliberately escape its process group. These
remain explicit support/verification boundaries, not passing checks.

There is no pinned-version comparative benchmark against AgentProcs or other
alternatives. The tests establish this implementation's inspected behaviour;
they do not establish comparative superiority. The skill is validated as an
instruction artifact; global installation and end-to-end agent discovery of that
installed skill have not been performed.

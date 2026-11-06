# Verification

Verified locally on 2026-11-06, macOS arm64, using Rust 1.99.1.

## Completed checks

- Formatting and Clippy with warnings denied.
- One duration unit test and eleven process integration tests. The suite includes
  expiry without a continuing CLI call, TERM-resistant descendants, leader exit,
  explicit extension, observation without renewal, keeper/guardian crash cleanup,
  bounded output, idempotent/concurrent retries, unavailable control refusing to
  signal an unrelated PID, private files and inherited directory/environment.
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

## Limits

No direct system sleep/wake, clock-change, simultaneous destruction of both
supervisors, Linux runtime or restricted Codex/Claude execution-profile test has
been completed. A service can deliberately escape its process group. These
remain explicit support/verification boundaries, not passing checks.

There is no pinned-version comparative benchmark against AgentProcs or other
alternatives. The tests establish this implementation's inspected behaviour;
they do not establish comparative superiority. The skill is validated as an
instruction artifact; global installation and end-to-end agent discovery of that
installed skill have not been performed.

# ProcLease project instructions

Read `README.md`, `docs/project-context.md`, and the affected source before changes.

- ProcLease is a Rust CLI for explicit process lifetimes, with a portable agent skill.
- Keep expiry independent of model turns. Status and log reads never renew leases.
- Manage only processes started through ProcLease; preserve host permissions and sandbox boundaries.
- Keep one authoritative lifecycle owner. Do not introduce an unrestricted command-execution service.
- Keep commands as argument vectors. Avoid implicit shell evaluation and environment disclosure.
- Keep scratch, build outputs, runtime fixtures and diagnostics isolated outside Git. Use `CARGO_TARGET_DIR` and `PROCLEASE_TEST_ROOT` to select suitable locations; never use normal application state for tests.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` for code changes. Process tests must clean up their own children.
- Keep the shared skill portable. Put supported installation procedures in the README; keep incidental personal installation state out of public docs.
- Follow the [commit attribution policy](CONTRIBUTING.md#commit-attribution):
  preserve the human author and existing co-authors, credit actual Codex/Claude
  implementation, validation or review with the specified trailers exactly once,
  and do not rewrite published history solely for attribution.
- Domain, DNS, publishing, package releases and global skill installation require their own task scope. A local commit does not publish the project.

## Context maintenance

```text
Method: context-docs
Adopted: 2026-11-06
Entry point: docs/project-context.md
```

Use the context-docs skill for documentation audits and after meaningful changes
to project facts, decisions, procedures, blockers or next actions. Read the skill
before maintenance. Update the affected canonical sections and the inline progress
view, preserve decision status and evidence, and check links and the diff. Keep
audit-only requests read-only; make no bookkeeping edits when nothing durable changed.

Public context serves contributors and users. Retain project behavior, decisions,
reproducible procedures, scoped verification evidence, limitations and required
contribution credits. Keep personal model collaboration arrangements, reasoning
and subscription preferences, account history and incidental machine state in the
owner's designated private notes. Review audience and destination before adding
context; publish only the useful summary while preserving provenance and limits.
Do not expose private notes locations or access details in public files.

`CLAUDE.md` imports this file. Instruction-file presence records the intended
discovery route; it does not itself prove automatic loading by every client.

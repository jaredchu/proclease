# ProcLease project instructions

Read `README.md`, `docs/project-context.md`, and the affected source before changes.

- ProcLease is a Rust CLI for explicit process lifetimes, with a portable agent skill.
- Keep expiry independent of model turns. Status and log reads never renew leases.
- Manage only processes started through ProcLease; preserve host permissions and sandbox boundaries.
- Keep one authoritative lifecycle owner. Do not introduce an unrestricted command-execution service.
- Keep commands as argument vectors. Avoid implicit shell evaluation and environment disclosure.
- Use High reasoning for delegated runs unless the owner specifies otherwise. If unavailable, report it before running; do not silently substitute.
- Put task scratch, build outputs, runtime fixtures and diagnostics in `~/Stash/proclease/<task>/`. Keep private artifacts outside Git.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` for code changes. Process tests must clean up their own children.
- Keep the shared skill portable; host-specific installation details belong in the README.
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

`CLAUDE.md` imports this file. Instruction-file presence records the intended
discovery route; it does not itself prove automatic loading by every client.

# ProcLease project context

Last reviewed: 2026-11-06.

## Purpose and scope

Put an explicit lifetime on local development processes started by coding agents.
The first version targets macOS and Linux foreground services, especially Node/Vite/Next,
through a Rust CLI and one portable ProcLease skill for Codex and Claude Code.

## Decisions and authority

- **Approved by owner, 2026-11-06:** name ProcLease, a new standalone Rust CLI
  project and shared skill.
- **Owner-reported, 2026-11-06:** `proclease.com` was purchased through Cloudflare.
  Registration ownership, DNS and hosting have not been independently inspected.
- **Approved by owner, 2026-11-06:** adopt Context Docs for this project.
- **Owner-confirmed target, 2026-11-06:** macOS and Linux. macOS runtime tests are
  performed locally; Linux runtime validation remains pending its own environment.
- **Owner requirement:** improve on existing tools rather than duplicate a generic
  supervisor. Expiry, explicit extension, reliable cleanup and agent usability are
  the focus. Research found substantial overlap with
  [AgentProcs](https://github.com/jkhoffman/agent-procs),
  [agproc](https://github.com/jmjoy/agproc), and
  [persistproc](https://steveasleep.com/persistproc/).
- **Implementation choice:** use proven Rust/Unix primitives and per-job supervisors
  launched in the caller's execution context. Avoid a login-wide service accepting
  arbitrary execution requests from potentially restricted agents.
- **Deferred:** Docker, arbitrary PID adoption, automatic cross-task reuse, reverse
  proxying, auto-restart, hosted dashboards and publication.

## Current state and progress

```text
Goal: a usable local ProcLease CLI and shared skill
|-- DONE    New repository and context maintenance entry points
|-- DONE    Rust CLI, expiry enforcement and process lifecycle tests
|-- DONE    Shared skill validation and real Vite/Next expiry smoke tests
`-- NEXT    Linux runtime CI and tests in restricted agent host profiles
```

The initial local implementation is complete. [Verification](verification.md)
records checks and their limits. Linux target compilation passes; Linux runtime
CI has not run. A comparative benchmark is needed before claiming measured
superiority over another supervisor.

Context maintenance is adopted in `AGENTS.md`, reached by `CLAUDE.md` import.
Claude Code's local `/context` command confirmed both files under project memory
on 2026-11-06, with no model request. Automatic instruction loading in a future
Codex chat has not been independently checked.

## Canonical references

- [README and usage](../README.md)
- [Architecture and lifecycle boundaries](architecture.md)
- [Verification and limits](verification.md)
- [Shared ProcLease skill](../skills/proclease/SKILL.md)

## Open questions

- Exact compatibility under each agent host's restricted execution profile needs
  direct host testing. Same-user local control is not authenticated agent identity.
- Public repository, package release and website deployment are not configured.

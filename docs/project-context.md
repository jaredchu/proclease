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
  The active Cloudflare zone, custom-domain binding and public HTTPS site were
  verified during publication. Registrar ownership was not independently inspected.
- **Approved by owner, 2026-11-06:** adopt Context Docs for this project.
- **Approved by owner, 2026-11-06:** publish as an open-source project under
  `jaredchu` on GitHub. The MIT-licensed source is public at
  [jaredchu/proclease](https://github.com/jaredchu/proclease).
- **Owner requirement, 2026-11-06:** credit Codex and Claude following the Context
  Docs project convention. See [credits](../README.md#credits-and-license) and the
  [commit attribution policy](../CONTRIBUTING.md#commit-attribution).
- **Owner-selected retention, 2026-11-06:** collect finished history after 24 hours
  or beyond the newest 100 finished jobs, whichever comes first. Preserve active
  and unverified jobs. Collection deletes logs/metadata and ends retained-key
  deduplication. See [retention behavior](../README.md#state-and-output).
- **Owner-confirmed target, 2026-11-06:** macOS and Linux. Runtime tests have
  passed on macOS arm64 and in a non-root Linux aarch64 container.
- **Owner-selected website direction, 2026-11-06:** monochrome technical minimalism
  with a dark theme.
- **Owner-authorized, 2026-11-06:** publish the first GitHub prerelease with native
  macOS/Linux binaries and checksums, then build and publish `proclease.com` with
  installation, a terminal demo, skill setup and GitHub links. This supersedes the
  earlier deferral of GitHub releases and website deployment.
- **Owner requirement:** improve on existing tools rather than duplicate a generic
  supervisor. Expiry, explicit extension, reliable cleanup and agent usability are
  the focus. Research found substantial overlap with
  [AgentProcs](https://github.com/jkhoffman/agent-procs),
  [agproc](https://github.com/jmjoy/agproc), and
  [persistproc](https://steveasleep.com/persistproc/).
- **Implementation choice:** use proven Rust/Unix primitives and per-job supervisors
  launched in the caller's execution context. Avoid a login-wide service accepting
  arbitrary execution requests from potentially restricted agents.
- **Publication:** the experimental [v0.1.2 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.2)
  provides four native binaries, checksums and a verified installer. The public
  [website](https://proclease.com) uses static assets in the owner's Cloudflare
  account; [website source and deployment](../website/README.md) are maintained here.
- **Deferred:** Docker, arbitrary PID adoption, automatic cross-task reuse, reverse
  proxying, auto-restart, hosted dashboards and package-registry distribution.

## Current state and progress

```text
Goal: a usable ProcLease CLI, shared skill and public first release
|-- DONE    New repository and context maintenance entry points
|-- DONE    Rust CLI, expiry enforcement and process lifecycle tests
|-- DONE    Shared skill validation and real Vite/Next expiry smoke tests
|-- DONE    Six review fixes and macOS/Linux regression tests
|-- DONE    Focused Astra/Fable reviews and interrupted-write refinement
|-- DONE    User-local CLI installation and Codex/Claude skill discovery
|-- DONE    Public MIT-licensed GitHub repository, contributor credits and hosted CI
|-- FIXED   Shared-registry guidance after a real Codex preview launch
|-- FIXED   Expiry test startup race identified in hosted macOS diagnostics
|-- DONE    Finished history bounded to 24h/newest 100; --all retains access
|-- DONE    Readable colored tables, compact times and narrow-terminal layout
|-- DONE    Version 0.1.2 briefly shows up to five finished jobs for 30 seconds
|-- DONE    On-demand CPU/RSS group totals in tables and opt-in JSON
|-- DONE    Native four-platform prerelease, archives and verified installation
`-- DONE    Dark monochrome website, browser QA and proclease.com publication
```

The initial implementation is public. Hosted macOS and Ubuntu checks pass;
[verification](verification.md) records evidence and limits, including a real
Codex preview session and the corrected registry-selection guidance. Restricted
agent host profiles remain untested. A comparative benchmark is needed before claiming
measured superiority over another supervisor.

The recent-job window and resource presentation are implementation choices in
response to the owner's usability requests. Resource reads never extend leases.
See [resource units and limits](../README.md#resource-usage).

Context maintenance is adopted in `AGENTS.md`, reached by `CLAUDE.md` import.
Claude Code's local `/context` command confirmed both files under project memory
on 2026-11-06, with no model request. Automatic instruction loading in a future
Codex chat has not been independently checked.

## Canonical references

- [README and usage](../README.md)
- [Architecture and lifecycle boundaries](architecture.md)
- [Verification and limits](verification.md)
- [Shared ProcLease skill](../skills/proclease/SKILL.md)
- [Contributing and attribution](../CONTRIBUTING.md)

## Open questions

- Exact compatibility under each agent host's restricted execution profile needs
  direct host testing. Same-user local control is not authenticated agent identity.
- Package-registry distribution and macOS Developer ID signing/notarization are
  not configured. See [the release procedure](releasing.md) for current delivery.

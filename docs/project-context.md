# ProcLease project context

Last reviewed: 2026-11-07.

## Purpose and scope

Put an explicit lifetime on local development processes started by coding agents.
The first version targets macOS and Linux foreground services, especially Node/Vite/Next,
through a Rust CLI and one portable ProcLease skill for Codex and Claude Code.
This public context records contributor-facing decisions, behavior and
verification limits.

## Decisions and authority

- **Approved by owner, 2026-11-06:** name ProcLease, a new standalone Rust CLI
  project and shared skill.
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
- **Publication:** the experimental [v0.1.4 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.4)
  provides four native binaries, checksums and a verified installer. The CLI
  source is also published on [crates.io](https://crates.io/crates/proclease/0.1.4),
  with a [docs.rs overview](https://docs.rs/crate/proclease/0.1.4). The public
  [website](https://proclease.com) serves static assets through a Cloudflare Worker;
  [website source and deployment](../website/README.md) are maintained here.
- **Deferred:** Docker, arbitrary PID adoption, automatic cross-task reuse, reverse
  proxying, auto-restart and hosted dashboards.

## Current state and progress

```text
Goal: a usable ProcLease CLI, shared skill and public first release
|-- DONE    New repository and context maintenance entry points
|-- DONE    Rust CLI, expiry enforcement and process lifecycle tests
|-- DONE    Shared skill validation and real Vite/Next expiry smoke tests
|-- DONE    Six review fixes and macOS/Linux regression tests
|-- DONE    Focused independent reviews and interrupted-write refinement
|-- DONE    Installation and Codex/Claude skill discovery validated
|-- DONE    Public MIT-licensed GitHub repository, contributor credits and hosted CI
|-- FIXED   Shared-registry guidance after a real Codex preview launch
|-- FIXED   Expiry test startup race identified in hosted macOS diagnostics
|-- DONE    Finished history bounded to 24h/newest 100; --all retains access
|-- DONE    Readable colored tables, compact times and narrow-terminal layout
|-- DONE    Version 0.1.2 briefly shows up to five finished jobs for 30 seconds
|-- DONE    On-demand CPU/RSS group totals in tables and opt-in JSON
|-- DONE    Native four-platform prerelease, archives and verified installation
|-- DONE    Dark monochrome website, browser QA and proclease.com publication
|-- DONE    Four startup/retry corrections released in v0.1.3
|-- DONE    Test teardown now waits for fixture supervisors; released in v0.1.3
|-- DONE    v0.1.3 experimental prerelease; four native builds and assets verified
|-- DONE    Website identity, type and search refresh published on proclease.com
|-- DONE    Installer choice labels wrap at 200% text; only requested arrows removed
|-- DONE    crates.io 0.1.3 and docs.rs overview published; source installation verified
|-- DONE    v0.1.4 ships the Cargo 1.89 fix through GitHub, crates.io and the website
|-- FIXED   A later teardown-test observation race, without changing released runtime
|-- DONE    Tabler SVG interface icons and restored arrows verified across three browsers
`-- SOURCE  Reviewed bulk `stop --all`; 47 tests pass on macOS/Linux, unreleased
```

The initial implementation is public. Hosted macOS and Ubuntu checks pass;
[verification](verification.md) records evidence and limits, including a real
Codex preview session and the corrected registry-selection guidance. Restricted
agent host profiles remain untested. A comparative benchmark is needed before claiming
measured superiority over another supervisor. The v0.1.3
[startup and retry corrections](verification.md#startup-and-retry-corrections-2026-11-07)
record their regressions and current verification limits. A test-teardown race
that left partial fixture directories is
[corrected in the test fixtures](verification.md#fixture-teardown-residue-2026-11-07);
production lifecycle code was unchanged.
The [v0.1.3 publication](verification.md#v013-publication-2026-11-07) records
the release at `bdbe785`, verified assets, the main-branch fast-forward and
anonymous HTTPS checks of the deployed website and installer.
The website sources now include a lease-ring identity, a readable fluid type
scale, em-dash-free copy and source-backed search metadata.
[Website verification](verification.md#website-refresh-2026-11-07) records
the original responsive checks; the later
[controls correction](verification.md#registry-and-website-controls-2026-11-07)
resolves the enlarged-text selector limitation and records the earlier narrow
arrow removal. The later [Tabler SVG update](verification.md#tabler-interface-icons-2026-11-07)
restores those interface arrows as SVGs and standardizes the other UI icons,
with browser checks and locally bundled source/license provenance.
[Design and SEO/GEO decisions](../website/README.md#identity-and-type) describe
the implementation and supporting sources. The refresh is live on proclease.com.
The [v0.1.4 publication](verification.md#v014-publication-2026-11-07) delivers the
Cargo 1.89 build-time stripping fix without changing runtime behavior. Native
archives, registry checksum/default installation, docs.rs and the matching live
website were verified. Older 0.1.3 registry users retain its documented caveat.
A subsequent [teardown-test observation race](verification.md#teardown-regression-observation-2026-11-07)
is corrected in main's test source; published v0.1.4 bytes remain unchanged.
User-authorized bulk `stop --all` with exact owner/task filters, dry run and an
aggregate JSON envelope is implemented and reviewed in source only. It is not in
v0.1.4. The initial review findings are resolved; hosted stable macOS/Ubuntu checks
and both Rust 1.89 package installations pass.
The [verification](verification.md#bulk-stop-unreleased-source-2026-11-07)
records its tests and limits.

The recent-job window and resource presentation are implementation choices for
terminal usability. Resource reads never extend leases.
See [resource units and limits](../README.md#resource-usage).

Context maintenance is adopted in `AGENTS.md`, reached by `CLAUDE.md` import.
Instruction-discovery evidence and limits are recorded in
[verification](verification.md#completed-checks).

## Canonical references

- [README and usage](../README.md)
- [Architecture and lifecycle boundaries](architecture.md)
- [Verification and limits](verification.md)
- [Shared ProcLease skill](../skills/proclease/SKILL.md)
- [Contributing and attribution](../CONTRIBUTING.md)

## Open questions

- Exact compatibility under each agent host's restricted execution profile needs
  direct host testing. Same-user local control is not authenticated agent identity.
- macOS Developer ID signing/notarization is deferred for experimental releases
  and required before promotion to stable. See the
  [release procedure](releasing.md) for binary and registry delivery.

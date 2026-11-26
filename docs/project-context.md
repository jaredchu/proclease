# ProcLease project context

## Purpose and scope

ProcLease gives local development services explicit lifetimes. The Rust CLI
and portable skill target macOS and Linux foreground processes used by coding
agents, including Vite and Next.js. [Usage and support](../README.md)

## Decisions and authority

- Per-job supervisors run in the caller's execution context. The project
  preserves host sandbox boundaries and manages processes it starts. Docker,
  arbitrary PID adoption, automatic cross-task reuse, reverse proxying,
  auto-restart and hosted dashboards remain deferred. [Architecture](architecture.md)
- Keep finished history for 24 hours or the newest 100 finished jobs, whichever
  comes first. Preserve active and unverified jobs. Collection removes logs,
  metadata and retained-key deduplication. Reading status, logs or resource
  usage never renews a lease. [State and output](../README.md#state-and-output)
- Ordinary keyed retries still check the environment. Explicit
  `run --reuse --key` verifies the command, directory and launch policy. It
  keeps the original environment and never starts a job. Next.js polling is
  an application opt-in.
  [Retry diagnosis](agent-compatibility.md#observed-failures-and-limits) ·
  [Next.js setup](../README.md#nextjs-in-agent-sandboxes)
- Detached/socket mode remains the default. The explicit foreground/file mode
  fits retained execution tasks and respects existing sandbox policies.
  [Foreground sessions](architecture.md#foreground-sessions)
- The website deploys from validated, current `main` in the canonical
  repository. Binary and registry releases, domains, DNS and global skill
  installation have separate approval. Contributor credit follows the commit
  attribution policy. [Project rules](../AGENTS.md) ·
  [Website publication](../website/README.md#publish) ·
  [Release procedure](releasing.md) · [Attribution](../CONTRIBUTING.md#commit-attribution)

## Current state and progress

```text
Goal: a usable CLI and shared skill with verifiable releases
|-- RELEASED  v0.1.11 on GitHub and crates.io
|-- VERIFIED  Four native archives and signed build provenance
|-- TESTED    v0.1.9 lifecycle on native macOS arm64 and Linux x86_64
`-- LIMITED   Restricted Linux profiles and whole-client exit
```

**Release.** The experimental [v0.1.11 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.11)
shipped on 2026-11-25. It includes foreground signal handling, release
provenance, lifecycle fixes and terminal-output hardening. The four native
archives and installer have signed build provenance. The
[registry record](verification.md#v0111-registry-publication-and-shutdown-test-2026-11-25)
also documents a later shutdown test correction. The preceding v0.1.10 release
updated the skill's registry handoff and exact-ID cleanup guidance.
[v0.1.11 notes](releases/v0.1.11.md) · [Publication verification](verification.md#v0111-publication-2026-11-25) ·
[crates.io](https://crates.io/crates/proclease/0.1.11) ·
[v0.1.10 publication](verification.md#v0110-publication-2026-11-22)

**Compatibility.** The real-agent checks cover v0.1.7 and source builds
leading up to v0.1.9. The tested macOS Codex and Claude Code profiles passed the
Vite lifecycle and the retry and Next.js workflow with explicit reuse and
polling opt-in. The recorded Claude observer deviation qualifies that result.
The native Linux default Claude profile passed. Default detached/socket mode
remains unsupported in the tested Linux Codex and strict Claude profiles.
Foreground/file mode passed cross-call controls within retained execution
tasks in both. [Compatibility matrix](agent-compatibility.md) ·
[Foreground verification](verification.md#foreground-sessions-and-file-control-unreleased-source-2026-11-12)

**Lifecycle evidence.** The published v0.1.9 binary passed all 1,020 product
trials and 120 observer controls in the primary protocol v2 series on native
macOS arm64 and Linux x86_64. The report retains the superseded v1 series and
explains the observer correction. These host CLI tests establish the scoped
lifecycle outcomes. Comparative performance and broader agent compatibility
remain unmeasured. [Experiment report](experiments/v0.1.9/README.md) ·
[Related tools](comparison.md)

## Canonical references

- [Architecture and safety limits](architecture.md) · [Shared skill](../skills/proclease/SKILL.md)
- [Verification and release history](verification.md) · [Release notes](releases/)
- [Website deployment and canonical address](../website/README.md#canonical-url-and-redirects) ·
  [GitHub discovery](github-discovery.md). Search-ranking and traffic effects
  remain unmeasured.

## Open questions

- Early client exit can leave records unverified. In the tested strict Claude
  profile, private localhost also prevents cross-call HTTP access. The
  [compatibility matrix](agent-compatibility.md) retains four older unknown
  Codex records and six unverified foreground records. Test unmeasured profiles
  directly before claiming support.
- Same-user file control establishes no authenticated agent identity. Queue
  admission is per client and relies on private same-user state.
  [Operational limits](architecture.md#security-and-operational-limits)
- Stable macOS promotion requires Developer ID signing and notarization.
  [Release procedure](releasing.md)

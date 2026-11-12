# ProcLease project context

Last reviewed: 2026-11-12.

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
- **Owner-requested, 2026-11-08:** deploy the website automatically. Every
  validated push to main in the canonical repository, or a manual workflow run on
  current main, publishes proclease.com; this standing rule is recorded in
  [AGENTS.md](../AGENTS.md). A run whose commit is no longer the head of main
  skips deployment. Binary releases, crates, domains, DNS and global skill
  installation keep separate scope. The
  [website publication procedure](../website/README.md#publish) records the
  gating, freshness check, credential scope and live checks.
- **Owner requirement:** improve on existing tools rather than duplicate a generic
  supervisor. Expiry, explicit extension, reliable cleanup and agent usability are
  the focus. Research found substantial overlap with
  [AgentProcs](https://github.com/jkhoffman/agent-procs),
  [agproc](https://github.com/jmjoy/agproc), and
  [persistproc](https://steveasleep.com/persistproc/).
- **Implementation choice:** use proven Rust/Unix primitives and per-job supervisors
  launched in the caller's execution context. Avoid a login-wide service accepting
  arbitrary execution requests from potentially restricted agents.
- **Publication:** the experimental [v0.1.8 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.8)
  provides four native binaries, checksums and a verified installer. The CLI
  source is also published on [crates.io](https://crates.io/crates/proclease/0.1.8),
  with a [docs.rs overview](https://docs.rs/crate/proclease/0.1.8). The public
  [website](https://proclease.com) serves static assets through a Cloudflare Worker;
  [website source and deployment](../website/README.md) are maintained here.
- **Website attribution, 2026-11-09:** the creator link points to Jared Chu’s
  canonical CV; the matching structured author links the CV and GitHub identity.
  Product copy, project links and Codex/Claude contribution credit are retained.
- **Website canonical address, 2026-11-09:** owner-requested HTTPS and `www`
  redirects send HTTP apex and HTTP/HTTPS `www` requests to
  `https://proclease.com` with a permanent 301, preserving paths and query strings.
  Cloudflare zone rules and redirect-only DNS are maintained separately from
  asset deployment; see [the website procedure](../website/README.md#canonical-url-and-redirects).
- **Owner-requested, 2026-11-12:** resolve macOS Codex/Claude retry-context
  mismatches without weakening launch identity or disclosing environment values.
  The selected direction is an explicit `run --reuse` that requires `--key`,
  verifies command, directory and launch policy, keeps the retained job's
  original environment and never launches. Ordinary keyed retries still reject
  environment changes, and proxy variables are not ignored. See
  [the compatibility diagnosis](agent-compatibility.md#observed-failures-and-limits).
- **Owner-prioritized, 2026-11-12:** fix the macOS retry-context and Next.js
  sandbox issues first; native Linux agent testing is deferred and is not a
  prerequisite. This deferral was superseded by the later owner-requested
  native Linux runs below. The selected Next.js workflow is an application opt-in to
  polling for both Turbopack and Watchpack, keeping Turbopack and the host
  sandbox; ProcLease does not mutate application config. See the
  [recipe](../README.md#nextjs-in-agent-sandboxes).
- **Owner-requested, 2026-11-12:** resume native Linux real-agent tests using
  existing Linux CLI logins. The [matrix](agent-compatibility.md#native-linux-results-2026-11-12)
  records a default-Claude workflow pass and blocked Codex/strict-Claude profiles,
  including unverified Codex control state; no sandbox bypass or PID signalling
  was used to manufacture compatibility.
- **Selected, 2026-11-12:** treat the restricted Linux Codex and strict-Claude
  profiles as unsupported, not as targets for a workaround. Version 0.1.8
  adds a fail-closed control-socket preflight and a Linux
  [PID context](architecture.md#pid-context-unreleased). The PID context stops
  saved PIDs from another or unverifiable context being probed, used to infer
  shutdown or collect history, or used for metrics. Legacy Linux records fail
  closed the same way. A foreground mode was deferred from that defensive
  release; an out-of-sandbox helper, network control channel and PID translation
  remain excluded. The later foreground decision below has a narrower lifetime.
  After review, a
  keeper-finalized marker, set only after the keeper observes the managed group
  absent, lets finished history be collected without PID checks, so it stays
  bounded across reboots; a foreign-context `stopping` job
  keeps that state for a bounded finalization window instead of reading
  `unknown` before its final record.
- **Owner-authorized, 2026-11-12:** implement explicit session-scoped
  `run --foreground` with file controls, preserving the existing sandbox
  policies and detached/socket default. A native execution session or background
  Bash task retains the foreground invocation. It ends with the session or
  lease; it does not promise supervision after client/sandbox exit. File
  controls do not expose application ports across private network namespaces.
  See [the architecture](architecture.md#foreground-sessions-unreleased-source)
  and [the scoped client results](agent-compatibility.md#session-scoped-foreground-mode-unreleased-source-2026-11-12).
- **Deferred:** Docker, arbitrary PID adoption, automatic cross-task reuse, reverse
  proxying, auto-restart and hosted dashboards.

## Current state and progress

The earlier owner-requested v0.1.8 release is published on GitHub and crates.io, with a
docs.rs overview, four verified native archives and matching live website pins.
The published runtime and tests match reviewed `ef77123`; no host sandbox restriction
was relaxed. Fresh binary and default Rust 1.89 registry installations passed
explicit reuse and expiry checks. See [publication evidence](verification.md#v018-publication-2026-11-12)
and [the release notes](releases/v0.1.8.md).

The **v0.1.9 experimental prerelease** packages `run --foreground` with file
controls from reviewed `88b5ea7`, with version metadata at `5d0c47b`. Four native
archives, crates.io source, a docs.rs overview and matching website pins are
published; v0.1.8 binaries/crate do not contain this feature. All 96 tests pass
on all four native release targets, and default Rust 1.89 packaged-source and
registry installations pass. See [the release notes](releases/v0.1.9.md) and
[publication verification](verification.md#v019-publication-2026-11-12). Both tested restricted Linux clients passed
cross-call file controls, explicit extension/reuse, stop and expiry with native
execution tasks. Early client exits can still leave `unknown` records; strict
Claude's private localhost still prevents cross-call HTTP access. See
[verification](verification.md#foreground-sessions-and-file-control-unreleased-source-2026-11-12).

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
|-- DONE    Scoped bulk `stop --all` and retention protection released in v0.1.5
|-- DONE    v0.1.5 four-native/registry/website release and fresh installs verified
|-- DONE    v0.1.6 bounded readiness checks, four-native/registry release and installs
|-- DONE    Source-backed comparison and homepage positioning published
|-- DONE    Website auto-deploy from current main verified against live bytes
|-- DONE    HTTPS and www canonical redirects configured at the Cloudflare edge
|-- DONE    GitHub description, eleven relevant topics and custom social preview
|-- FIXED   Interrupted shutdown history ages out of default views after verified absence
|-- DONE    v0.1.7 interrupted-history fix, four-native/registry release and website
|-- TESTED  macOS real-agent Vite lifecycle; v0.1.7 retry/Next failures recorded
|-- FIXED   `run --reuse`, now in v0.1.8; earlier 68-test build and macOS clients passed
|-- FIXED   Next.js polling opt-in served initial/edited pages in both macOS clients
|-- TESTED  Native Linux default-Claude Vite/Next workflow and 68 Rust tests
|-- BLOCKED Tested Linux Codex/strict-Claude sandbox workflows; limits recorded
|-- DONE    v0.1.8 includes socket preflight and Linux PID-context checks
|-- TESTED  Initial candidate: 76 tests; restricted-profile checks fail closed
|-- FIXED   Review corrections: stopping window, finalized retention, procfs bounds
|-- TESTED  Corrected source: 81 tests on macOS/Linux; diagnostic recheck fails closed
|-- DONE    v0.1.8 four-native/registry/website release and isolated installs verified
|-- DONE    v0.1.9 session-scoped foreground mode and bounded file controls
|-- TESTED  96 tests on all four native targets; default Rust 1.89 package/registry installs
|-- TESTED  Native Codex/strict-Claude foreground controls, reuse, stop and expiry
|-- DONE    v0.1.9 GitHub/registry/docs.rs/website publication and isolated installer checks
`-- LIMITED Early client exit can lose finalization; strict-Claude localhost stays private
```

The [v0.1.5 publication](verification.md#v015-publication-2026-11-08) packages bulk
stop and its retention correction with matching installer, shared skill and live
website pins. Four native builds passed all 51 tests; fresh binary and default
Rust 1.89 registry installations passed scoped bulk-stop and expiry checks.

The initial implementation is public. Hosted macOS and Ubuntu checks pass;
[verification](verification.md) records evidence and limits, including a real
Codex preview session and the corrected registry-selection guidance. The
[agent compatibility matrix](agent-compatibility.md) records scoped macOS
Codex/Claude sandbox results: v0.1.7 Vite lifecycle passes with retry/Next.js
failures, then passes of the unreleased `run --reuse` and Next.js polling
workflow in the same profiles, with a recorded Claude observer protocol
deviation. Native Linux default-Claude
workflow checks passed; tested Codex and strict-Claude sandbox profiles are
blocked, with unverified Codex control state retained. Pre-release
[fail-closed diagnostics](verification.md#linux-sandbox-diagnostics-unreleased-source-2026-11-12)
for those profiles first passed 76 tests in an initial candidate, which review
then corrected. The corrected source passed formatting, Clippy with warnings
denied, all 81 tests and an optimized build directly on macOS and natively on
Linux. In the initial and final
[native Linux diagnostic checks](agent-compatibility.md#native-linux-results-2026-11-12),
strict Claude launched nothing after the socket denial, and Codex's later call
reported `unknown` with the PID-context explanation. The final host check
served the HTTP marker, expired with the keeper-finalized marker and closed its
port. These are not workflow passes: both profiles remain unsupported, Codex
control-protocol cleanup is unverified, and neither the default-Claude Linux
nor the macOS real-client Vite/Next.js workflow was rerun with this source.
These corrections are now distributed in v0.1.8; publication does not expand
the measured real-agent compatibility scope.
A comparative benchmark
is needed before claiming measured superiority over another supervisor. The v0.1.3
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
The [v0.1.4 publication](verification.md#v014-publication-2026-11-07) delivered the
Cargo 1.89 build-time stripping fix without changing runtime behavior. Native
archives, registry checksum/default installation, docs.rs and the matching live
website were verified. Older 0.1.3 registry users retain its documented caveat.
A subsequent [teardown-test observation race](verification.md#teardown-regression-observation-2026-11-07)
is corrected in main's test source; published v0.1.4 bytes remain unchanged.
User-authorized bulk `stop --all` with exact owner/task filters, dry run and an
aggregate JSON envelope is reviewed and released in v0.1.5. It is not in v0.1.4.
The initial review findings are resolved; hosted stable macOS/Ubuntu checks
and both Rust 1.89 package installations pass.
The [verification](verification.md#bulk-stop-unreleased-source-2026-11-07)
records its tests and limits. A later probe found that completion retention could
collect selected records before results were read; a
[source correction](verification.md#correction-retention-race-during-bulk-stop-2026-11-07)
adds a reviewed process-lifetime retention lock and regressions. All 51 tests,
formatting, Clippy and optimized builds pass locally and in hosted macOS/Ubuntu
checks; both default Rust 1.89 package installations pass.

Owner-authorized and released in v0.1.6 on 2026-11-08: optional `run --wait-for-port`,
`--wait-for-log` and `--wait-timeout` readiness observation, with no lease,
ownership or health effect. The interface uses loopback-only TCP, literal text, a
30s default and a 5m maximum. The same task added a source-backed
[comparison](comparison.md) with GNU timeout, systemd-run and rentctl and a
revised homepage intro and FAQ linking it. Readiness is absent from v0.1.5.
The homepage changes were published by the first automatic website deployment
without a new release. The [v0.1.6 publication](verification.md#v016-publication-2026-11-08) ships
readiness with matching installer, downloads and skill guidance. Four native
builds passed all 62 tests; fresh binary and default Rust 1.89 registry installs
passed readiness, keyed-retry/deadline and expiry checks. The main push deployed
the website automatically and all twelve live files matched the release commit.
[Verification](verification.md#readiness-wait-and-comparison-unreleased-source-2026-11-08)
records 62 local tests, default Rust 1.89 package installation, independent review
and browser checks, with their limits.

[Automatic website deployment](verification.md#website-automatic-deployment-2026-11-08)
is configured and verified by its first hosted main-push run, which published
the homepage changes and matched all twelve live files to the commit. A freshness
check skips runs whose commit is no longer the head of main; the production
environment and scoped environment secrets restrict publication to main.

The recent-job window and resource presentation are implementation choices for
terminal usability. Resource reads never extend leases.
See [resource units and limits](../README.md#resource-usage).
The [interrupted-history correction](verification.md#interrupted-history-default-views-2026-11-11)
is released in v0.1.7. Once both keeper and managed group are verified absent,
a saved shutdown record uses its last lifecycle-write time for the default
recent-job window. Exact completion time remains unknown; metadata, JSON and
conservative retention are unchanged. Published v0.1.6 builds do not include
this correction.
The [v0.1.7 publication](verification.md#v017-publication-2026-11-11) records
four native builds, matching package provenance, default Rust 1.89 registry
installation, the live website and fresh installer verification.

Context maintenance is adopted in `AGENTS.md`, reached by `CLAUDE.md` import.
Instruction-discovery evidence and limits are recorded in
[verification](verification.md#completed-checks).

The [GitHub discovery metadata](github-discovery.md) records the repository
description, eleven relevant topics and a custom social preview verified on
2026-11-09. These GitHub settings are maintained separately from the website;
no search-ranking or traffic improvement has been measured.

## Canonical references

- [README and usage](../README.md)
- [Architecture and lifecycle boundaries](architecture.md)
- [Comparison with related tools](comparison.md)
- [GitHub topics, description and social preview](github-discovery.md)
- [Verification and limits](verification.md)
- [Real-agent macOS/Linux compatibility matrix](agent-compatibility.md)
- [Shared ProcLease skill](../skills/proclease/SKILL.md)
- [Contributing and attribution](../CONTRIBUTING.md)

## Open questions

- Default detached/socket launches remain unsupported in the tested restricted
  Linux profiles. The new explicit foreground/file mode works within retained
  native execution tasks, without adding socket permission. It does not fix
  private localhost or guarantee finalization after whole-client teardown.
  Four older isolated Codex records remain `unknown`; six additional
  foreground test records are also unverified and retained. The
  [matrix](agent-compatibility.md) preserves the historical rows and separates
  this session-scoped result from full application/client-exit compatibility.
  Untested profiles still need direct testing.
- Version 0.1.9 is published through
  the [release procedure](releasing.md). Same-user local control is not
  authenticated agent identity. The file queue's admission check is per client,
  not an atomic global cap; the protocol relies on private same-user state.
- macOS Developer ID signing/notarization is deferred for experimental releases
  and required before promotion to stable. See the
  [release procedure](releasing.md) for binary and registry delivery.

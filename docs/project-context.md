# ProcLease project context

Last reviewed: 2026-11-22.

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
  [PID context](architecture.md#pid-context). The PID context stops
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
  See [the architecture](architecture.md#foreground-sessions)
  and [the scoped client results](agent-compatibility.md#session-scoped-foreground-mode-unreleased-source-2026-11-12).
- **Owner-authorized, 2026-11-13:** run expiry, failure-handling and retry
  experiments on the published v0.1.9 binaries on native macOS and Linux, and
  publish sanitized findings in this repository and on the existing website.
  The harness is a public, standard-library black-box tool; raw diagnostics
  stay private. See [the experiments](experiments/v0.1.9/README.md).
- **Owner-requested, 2026-11-25:** record an ordinary signal to
  `run --foreground` as a requested stop instead of lost supervision, through
  a one-byte stop request on the existing liveness pipes. Killing the
  invocation stays lost supervision. Not yet released; see
  [verification](verification.md#foreground-signal-stop-unreleased-source-2026-11-25).
- **Owner-requested, 2026-11-25:** pin every workflow action to a commit SHA
  and sign build provenance for release archives and the installer, with
  Dependabot proposing weekly pin updates. Not yet
  released; see the [release procedure](releasing.md) and
  [verification](verification.md#pinned-actions-and-build-provenance-unreleased-source-2026-11-25).
- **Owner-requested, 2026-11-25:** fix four findings from an independent
  review in source: reconcile jobs from an earlier boot of the same machine and
  `running` jobs whose supervisors and command are observed absent, end the
  shutdown grace once the group has exited, replace `fs2` with standard-library
  file locks, and cap launch-metadata grace everywhere. Not yet released. See
  [boot context](architecture.md#boot-context) and
  [verification](verification.md#review-fixes-unreleased-source-2026-11-25).
- **Owner-requested, 2026-11-25:** review cleanups: escape bidirectional
  formatting characters in terminal output, read process groups once per
  resource sample, and move the `run` command out of `main.rs`. Not yet
  released; see
  [verification](verification.md#review-cleanups-unreleased-source-2026-11-25).
- **Owner-requested, 2026-11-25:** keep the README and project context to
  current behavior: per-release feature notes move to a README version table
  and [docs/releases](releases/), release history to a linked index, and
  version-labelled anchors become plain ones with the old IDs kept as aliases.
  Done by hand; the context-docs skill was not available in that session. See
  [verification](verification.md#documentation-trim-unreleased-source-2026-11-25).
- **Owner-requested, 2026-11-25:** prepare the v0.1.11 experimental
  prerelease from the reviewed changes on main, following the
  [release procedure](releasing.md). Tagging, publishing the draft, merging
  the candidate and any crates.io publication each wait for the owner.
- **Deferred:** Docker, arbitrary PID adoption, automatic cross-task reuse, reverse
  proxying, auto-restart and hosted dashboards.

## Current state and progress

**Released.** The [v0.1.10 experimental prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.10)
is current on GitHub. Its [release workflow](https://github.com/jaredchu/proclease/actions/runs/44982560907)
verified four native archives, including expiry and installer checksum
rejection, and the installer and website pin it. It packages the
[updated skill guidance](../skills/proclease/SKILL.md), which carries the job ID
and registry identity across Codex and Claude Code sessions and stops a fixed
cleanup selection by ID after previewing it. crates.io and docs.rs carry v0.1.9,
so the `cargo install` command still names it. Notes for each release are in
[docs/releases](releases/).

**v0.1.11 candidate.** The candidate branch `release-v0.1.11` stages version,
installer and website pins and [release notes](releases/v0.1.11.md) for the
changes below; it stays off main until the release is public.

**Unreleased on main.** The 2026-11-25 independent review added
[boot context](architecture.md#boot-context) reconciliation of earlier-boot and
absent-supervisor records, an early end to the shutdown grace,
standard-library file locks, capped metadata grace, foreground signals recorded
as requested stops, escaped bidirectional formatting characters, a single
process-group read per resource sample, the `run` and `foreground` modules,
SHA-pinned workflow actions, signed release provenance and Dependabot pin
updates; Dependabot has since moved several pins to their v7 releases. Evidence: [review fixes](verification.md#review-fixes-unreleased-source-2026-11-25),
[foreground signal stop](verification.md#foreground-signal-stop-unreleased-source-2026-11-25),
[pinned actions and provenance](verification.md#pinned-actions-and-build-provenance-unreleased-source-2026-11-25)
and [review cleanups](verification.md#review-cleanups-unreleased-source-2026-11-25).
The provenance step first runs at the next version tag.

**Compatibility.** The [agent compatibility matrix](agent-compatibility.md)
records scoped results. In the tested macOS Codex/Claude sandbox profiles, the
Vite lifecycle passed with v0.1.7, and `run --reuse` with the Next.js polling
opt-in then passed the retry and Next.js workflow, with a recorded Claude
observer protocol deviation. On native Linux the default-Claude workflow
passed; the tested Codex and strict-Claude profiles are
[unsupported in default mode](agent-compatibility.md#native-linux-results-2026-11-12),
with fail-closed [diagnostics](verification.md#linux-sandbox-diagnostics-unreleased-source-2026-11-12)
and unverified Codex control state retained. Session-scoped foreground mode
passed cross-call file controls, explicit extension and reuse, stop and expiry
in both; early client exits can still leave `unknown` records, and strict
Claude's private localhost still prevents cross-call HTTP access
([verification](verification.md#foreground-sessions-and-file-control-unreleased-source-2026-11-12)).
A comparative benchmark is needed before claiming measured superiority over
another supervisor.

**Lifecycle experiments.** On 2026-11-13, scripted
[lifecycle experiments](experiments/v0.1.9/README.md) ran the published v0.1.9
binaries 30 times per scenario on native macOS arm64 and Linux x86_64. Detached
and foreground expiry, keeper/guardian SIGKILL, foreground SIGINT with blocked
stdout, sequential and eight-way concurrent keyed retries, changed-environment
`--reuse`, changed-command rejection and retries of expired jobs passed all
1,020 product trials, and 120 observer control trials passed: 1,140 in the
primary series under [protocol version 2](experiments/v0.1.9/protocol.md),
which treats `ps` failures as errors and never counts a live PID without the
trial token as stopped. A superseded protocol version 1 series also passed
1,140 trials and stays published. These are host CLI tests, not agent sessions
or a benchmark. The website's static `/experiments/` summary is checked at its
directory URL, as documented in the [website README](../website/README.md#pages).

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
|-- LIMITED Early client exit can lose finalization; strict-Claude localhost stays private
|-- FIXED   Experiment observer amended (protocol v2): ps failures, token-less PIDs, preconditions
|-- TESTED  v0.1.9 lifecycle experiments, primary v2 series: 570/570 each on macOS arm64 and Linux x86_64
|-- DONE    Reproducible report and static experiments page, with versioned methods and data
|-- SOURCE  Skill handoff and exact-ID cleanup guidance
|-- DONE    v0.1.10 GitHub prerelease and four native archives verified
|-- SOURCE  Foreground signals recorded as requested stops
|-- SOURCE  Pinned actions, archive handoff checks and signed release provenance
|-- SOURCE  Review fixes: earlier-boot reconciliation, early grace end, std locks
|-- SOURCE  Review cleanups: bidi escaping, single group read, run module
|-- SOURCE  README and context trimmed to current behavior; release index
`-- SOURCE  v0.1.11 release candidate: version, pins and notes
```

**Release history.** Evidence for each step is in [verification](verification.md):

- v0.1.3: [startup and retry corrections](verification.md#startup-and-retry-corrections-2026-11-07)
  and [publication](verification.md#v013-publication-2026-11-07) at `bdbe785`.
  A test-teardown race was
  [corrected in the fixtures](verification.md#fixture-teardown-residue-2026-11-07)
  without changing production code.
- v0.1.4: the [Cargo 1.89 build-time stripping fix](verification.md#cargo-189-build-time-stripping-fix-2026-11-07)
  and [publication](verification.md#v014-publication-2026-11-07); older 0.1.3
  registry users retain its documented caveat. A later
  [teardown-test observation race](verification.md#teardown-regression-observation-2026-11-07)
  was corrected in test source only.
- v0.1.5: [bulk `stop --all`](verification.md#bulk-stop-unreleased-source-2026-11-07)
  with its [retention-race correction](verification.md#correction-retention-race-during-bulk-stop-2026-11-07)
  and [publication](verification.md#v015-publication-2026-11-08).
- v0.1.6: [readiness wait and the tool comparison](verification.md#readiness-wait-and-comparison-unreleased-source-2026-11-08)
  ([comparison](comparison.md)) and [publication](verification.md#v016-publication-2026-11-08).
- v0.1.7: [interrupted-history default views](verification.md#interrupted-history-default-views-2026-11-11)
  and [publication](verification.md#v017-publication-2026-11-11).
- v0.1.8: `run --reuse`, socket preflight, Linux PID context and
  keeper-finalized retention, released from reviewed `ef77123`
  ([publication](verification.md#v018-publication-2026-11-12),
  [notes](releases/v0.1.8.md)).
- v0.1.9: `run --foreground` with file controls from reviewed `88b5ea7`, with
  version metadata at `5d0c47b` ([publication](verification.md#v019-publication-2026-11-12),
  [notes](releases/v0.1.9.md)).
- v0.1.10: skill handoff and exact-ID cleanup guidance.
- v0.1.11 (candidate): the review changes above, with [notes](releases/v0.1.11.md).

**Website and repository.** The website deploys
[automatically from current main](verification.md#website-automatic-deployment-2026-11-08);
a freshness check skips runs whose commit is no longer the head of main, and
the production environment and scoped secrets restrict publication to main.
The lease-ring identity, fluid type scale and source-backed search metadata
are described in the [design and SEO/GEO decisions](../website/README.md#identity-and-type),
with [website](verification.md#website-refresh-2026-11-07),
[controls](verification.md#registry-and-website-controls-2026-11-07) and
[Tabler icon](verification.md#tabler-interface-icons-2026-11-07) verification.
The [GitHub discovery metadata](github-discovery.md) records the repository
description, eleven topics and a custom social preview verified on 2026-11-09;
no search-ranking or traffic improvement has been measured.

The recent-job window and resource presentation are implementation choices for
terminal usability; resource reads never extend leases. See
[resource units and limits](../README.md#resource-usage).

Context maintenance is adopted in `AGENTS.md`, reached by `CLAUDE.md` import.
Instruction-discovery evidence and limits are recorded in
[verification](verification.md#completed-checks).

## Canonical references

- [README and usage](../README.md)
- [Architecture and lifecycle boundaries](architecture.md)
- [Comparison with related tools](comparison.md)
- [GitHub topics, description and social preview](github-discovery.md)
- [Verification and limits](verification.md)
- [Real-agent macOS/Linux compatibility matrix](agent-compatibility.md)
- [v0.1.9 lifecycle experiments](experiments/v0.1.9/README.md) and their
  [protocol](experiments/v0.1.9/protocol.md) (version 2; historical
  [version 1](experiments/v0.1.9/protocol-v1.md))
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
- Releases follow the [release procedure](releasing.md). Same-user local
  control is not authenticated agent identity. The file queue's admission check is per client,
  not an atomic global cap; the protocol relies on private same-user state.
- macOS Developer ID signing/notarization is deferred for experimental releases
  and required before promotion to stable. See the
  [release procedure](releasing.md) for binary and registry delivery.

# Verification

Verified on 2026-11-06, locally on macOS arm64 and Linux aarch64 using Rust 1.99.1,
and in hosted macOS/Ubuntu CI.

## Completed checks

- Formatting and Clippy with warnings denied.
- Two unit checks (duration parsing and Unicode display bounds) and twenty-six
  process integration tests. The suite includes
  expiry without a continuing CLI call, TERM-resistant descendants, leader exit,
  explicit extension, observation without renewal, keeper/guardian crash cleanup,
  bounded output, idempotent/concurrent retries, unavailable control refusing to
  signal an unrelated PID, private files and inherited directory/environment.
  Added regressions cover metadata permission failures after spawn, partial
  control requests during expiry, empty launch reservations, commands during
  shutdown grace, and output truncation during shutdown.
  Additional cases verify expiry before application readiness, briefly visible
  finished history with preserved logs/retry identity, and visible unverified state.
- All twenty-eight current tests passed locally on macOS. The earlier seventeen-test
  suite passed in a Linux aarch64 container as a non-root user (UID 501), including
  the permission-failure regression. That Linux validation used native tmpfs state
  rather than a VM shared directory.
- Retention regressions cover age cleanup, count limits with concurrent collectors,
  dry-run preservation, protected active/unknown/live-owner state, missing completion
  timestamps, symlink boundaries, interrupted and failed deletion, post-completion
  collection without another CLI call, and retry-key reuse after history expires.
- Display checks cover 20/40/80/120-column rendering, wide Unicode text, escaped
  application controls, readable job IDs, explicit color modes and unchanged JSON
  parsing. Real pseudo-terminal captures at 100 and 40 columns were visually
  inspected; `NO_COLOR` and `TERM=dumb` suppressed automatic colors in that same
  terminal setup. The README image combines a real disposable CPU/memory worker
  with synthetic history rows; its worker is stopped after capture.
- Resource checks verify nonzero CPU under load, RSS including a 32 MiB child,
  exclusion of both supervisors, unchanged expiry, JSON opt-in, watch snapshots,
  and unavailable metrics after completion. macOS requires a baseline refresh
  after inserting a new process snapshot before measuring the CPU interval.
  Linux snapshot thread entries are excluded to avoid counting supervisor threads
  or repeating process RSS; the process-count assertion caught this in hosted CI.
  Display regressions verify the 30-second recent window and five-job cap.
- Skill frontmatter and structure validated with the skill-creator validator.
- Real `npm run dev` smoke tests for Vite 8.3.6 and Next.js 16.4.5, using Node
  24.18.0. Both served HTTP 200 before expiry, reached `expired`, and released their
  listening ports. All fixtures used isolated state and were cleaned up.
- `cargo check --locked --target x86_64-unknown-linux-gnu` passed. This type-checks
  the Linux target; it is not a Linux runtime result.
- An optimized macOS release binary builds locally.
- Claude Code's local `/context` command listed the project `CLAUDE.md` and its
  imported `AGENTS.md`, confirming the intended context-maintenance discovery
  route for the observed invocation. Automatic instruction loading in a future
  Codex chat has not been independently checked.
- An installation smoke test used a binary matching the tested release: an
  isolated job expired and its keyed retry reused the terminal job.
- A fresh Codex app-server `skills/list` returned the linked shared skill enabled
  at user scope, and Claude Code's local `/context` listed it as a user skill.
- [Hosted Rust checks](https://github.com/jaredchu/proclease/actions/runs/42202334004)
  passed on `macos-latest` and `ubuntu-latest` at commit `8932479`. Both jobs
  completed formatting, Clippy with warnings denied, all seventeen tests and an
  optimized release build.
- Version 0.1.1 at `5a3bcd0` passed [hosted macOS and Ubuntu checks](https://github.com/jaredchu/proclease/actions/runs/42211702346),
  including all twenty-six tests, formatting, Clippy and release builds. A local
  installation smoke test verified the updated shared skill and terminal views.
- Version 0.1.2 at `3b71397` passed [hosted macOS and Ubuntu checks](https://github.com/jaredchu/proclease/actions/runs/42215574600):
  all twenty-eight tests, formatting, Clippy and release builds. A macOS
  installation smoke test used the matching release binary and validated shared
  skill. Live watch warmed up from unavailable CPU to a nonzero reading without
  changing expiry; its disposable service was stopped and then reported no live
  metrics.
- A real local Codex session discovered the skill and launched an Astro preview
  through ProcLease. It selected a task-specific state directory, which made the
  job invisible to the user's default `watch`. After stopping that job and
  restarting it in the default registry with its remaining lease, the preview
  returned HTTP 200 and appeared in ordinary `list` and `watch --once`. The shared
  skill now preserves the user's normal state selection and explains intentional
  isolation; a live state directory must never be moved to repair visibility.

## macOS timeout correction

[Hosted diagnostics](https://github.com/jaredchu/proclease/actions/runs/42206087042)
identified the wait as fixture readiness, not process cleanup. The 1.2-second
lease expired before the Python fixture produced readiness; its persisted record
was already `expired` with `reason: deadline`, finalized 173ms after expiry.
A local delayed-start reproduction likewise expired before readiness.

The process-group expiry test now allows its eight-second readiness budget within
a ten-second lease, then bounds terminal observation relative to the actual
deadline plus two seconds. A separate test verifies expiry without readiness.
The slow-control test uses `/bin/sleep` rather than racing interpreter startup,
and the metadata-failure fixture waits for an explicit gate after the permission
fault is installed. Production deadline enforcement was unchanged.

Four concurrent runs of both expiry tests passed locally using frozen executables.
An initial harness running Cargo concurrently hit an executable replacement race;
compilation was separated from stress execution. The corrected test suite at
`8217498` passed [hosted macOS and Ubuntu CI](https://github.com/jaredchu/proclease/actions/runs/42206977634).
Failure diagnostics and Rust backtraces remain enabled, and one platform's failure
does not cancel the other platform's matrix job.

## Reproduction

Use the commands in [development instructions](../README.md#development).
Integration fixtures require `/usr/bin/python3`. The macOS and Ubuntu CI matrix
is defined in [the workflow](../.github/workflows/ci.yml) and runs on pushes and
pull requests.

## First binary prerelease and website

The public [v0.1.2 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.2)
is tagged at `417a244`. Its [release workflow](https://github.com/jaredchu/proclease/actions/runs/42226114817)
passed all twenty-eight tests, formatting, Clippy, optimized builds, archive expiry
and installer checks on macOS ARM64/Intel and Ubuntu 22.04 ARM64/x64. macOS builds
target 15.0; the release Apple Silicon binary is tested on the current hosted OS,
not separately on an older OS. See [release compatibility](releases/v0.1.2.md#compatibility-and-limits).

All four downloaded archives matched `SHA256SUMS`. A fresh installation through
the public website's installer fetched the anonymous GitHub release assets,
returned version 0.1.2 and expired an isolated service. Corrupt-download checks
leave an already installed binary intact. The prerelease contains four archives,
their four checksum files, combined checksums and the installer.

The public [proclease.com](https://proclease.com) website serves committed static
assets through the `proclease-site` Cloudflare Worker with a custom domain and
HTTPS. Browser checks covered 320/390/768/1440-pixel widths, expiry/replay,
clipboard, keyboard tabs, download selection and FAQ, with no JavaScript errors
or horizontal overflow. Browser and curl access returned HTTP 200; the installer
bytes matched the committed script. The domain is linked in the GitHub repository.

Native macOS 15 validation exposed a test-fixture permission issue: a read-only
job root could fail before the expected quarantine rename. The fault now uses a
locked nested directory, preserving the root's ability to move and testing partial
deletion as intended. Production retention code was unchanged.

## Review corrections

Independent reviews identified six issues. Cleanup guards now
cover post-spawn metadata failures; nonblocking control requests have absolute
time budgets; provably empty launch reservations do not block keyed runs;
shutdown controls reconcile lifecycle state; an already-open capture marker
preserves shutdown truncation; and the skill assigns retry keys on the initial
launch. Focused independent reviews found no remaining P1/P2 issues in these
fixes.
A later review identified a P3 residual: a reservation containing only an
interrupted atomic-write temporary file still blocked keyed runs. The correction
added conservative file-name/type/permission checks and regressions
that retain rejection of symlinks, directories and ambiguous state. That final
refinement was validated locally, without another independent review round.

### Startup and retry corrections, 2026-11-07

An independent review of 0.1.2 found four defects, reproduced on macOS arm64:
retry fingerprints aliased argv `[/usr/bin/true, A, value]` with argv
`[/usr/bin/true]` plus `A=value`; a 1ms lease could run its command after the
persisted deadline; a guardian setup failure such as a denied socket bind was
reported as an `interrupted` launch with exit 0; and a failed keeper spawn left a
`starting` record that later retries reused as `unknown`, while prune skipped it.
The [architecture](architecture.md) describes the corrected behavior. The corrected code is not yet in a released version.

New regressions are deterministic rather than timing-dependent:

- A unit test checks the documented collision, section separation and acceptance
  of an earlier-format hash only for its exact stored argv and directory. An
  integration test repeats the collision through `run` with a controlled
  environment. It also rewrites a retained record to the earlier encoding: the
  same context is reused, while the colliding and changed contexts are rejected.
- Overdue-startup and guardian setup failures run the internal supervisor on a
  committed launch: one with a deadline already in the past, and one with a
  non-socket file at the control path. Neither runs its marker command. Same-key
  retries return `expired` and `failed` respectively, the latter with exit 1.
- Keeper-spawn failure applies a one-process `RLIMIT_NPROC` limit only to the CLI.
  The run and its retry exit 1 with a `failed` record. The record is collected by
  age; seeded `starting`, `unknown`, `interrupted` and live-guardian records without
  a keeper PID remain protected. The test is skipped for root, which bypasses the limit.

`expiry_does_not_wait_for_application_readiness` now accepts either outcome of its
500ms lease under load: the command is killed, or it never starts.

Local macOS arm64 checks after these changes passed: formatting, Clippy with
warnings denied, three unit tests and thirty integration tests, and an optimized
release build. Hosted macOS and Ubuntu results for individual source revisions
are available in the [Rust checks workflow](https://github.com/jaredchu/proclease/actions/workflows/ci.yml).

### Fixture teardown residue, 2026-11-07

A local full-suite run after those corrections left four partial fixture
directories in the test root. Each held only the state lock and one job directory
with a terminal `record.json` from the expiry, guardian-failure,
environment-inheritance or environment-probe test. No recorded PID was alive.

The cause was test teardown. It stopped listed jobs, then removed its temporary
directory without waiting for their keepers. A job can be reported finished before
its keeper's final record write, socket removal and history collection under the
root lock: `list` derives the result from an absent group, and `stop` returns it.
On macOS a retained zombie leader is not a live member, so after a guardian crash
this window lasts the whole grace period. Recursive removal fails when those
writes make a directory non-empty again, which leaves the rest in place.
Supervisors do not recreate a deleted job directory or state root; the late files
land in directories that still exist.

Replaying the earlier teardown immediately after a quick `/usr/bin/true` job
finished left the same pattern in 4 of 40 runs: the lock alone, or the lock with
the keeper's final `exited` record. After a guardian crash with 100ms grace, the
keeper was still running at deletion in 40 of 40 runs. Waiting for the keeper to
exit before deletion left nothing in 80 runs.

The correction is test-only. Teardown waits up to 15 seconds for each keeper and
guardian whose argument vector names the fixture's canonical state root. It
identifies them from live processes, not record PIDs that tests may seed, and
sends no signal. If any remain, it retains the directory, reports their PIDs and
fails the test; during an existing panic it only reports them. Production code is
unchanged. The keeper's writes after an observed finished state are its documented
final-write ownership, and deleting live state is outside the supported contract.
A new regression crashes a guardian with a two-second grace. Teardown must return
only after the keeper has exited and the fixture directory is gone. With the
earlier teardown, it failed in 10 of 10 macOS runs.

Local macOS arm64 checks passed: formatting, Clippy with warnings denied, three
unit and thirty-one integration tests, and an optimized release build. After the
full run, the test root was empty and no supervisor or fixture service was
running. Linux probes a group with
`kill(-pgid, 0)`, which still counts an unreaped leader, so the derived window is
expected to be shorter there. The regression checks keeper ownership only when it
observes that window; its teardown assertions apply on both platforms. Hosted
results for individual revisions are available in the
[Rust checks workflow](https://github.com/jaredchu/proclease/actions/workflows/ci.yml).

## v0.1.3 release candidate, 2026-11-07

The [v0.1.3 notes](releases/v0.1.3.md) cover the startup/retry corrections and
the fixture teardown correction above. The candidate updates the version,
installer pin and mirror, README and website links, and the draft title. These
initial results describe local checks of an uncommitted macOS arm64 working tree.
The later publication outcome is recorded [below](#v013-publication-2026-11-07).

- Formatting, Clippy with warnings denied and the optimized release build passed.
- The first full test run failed one test:
  `explicit_extension_and_observation_have_distinct_semantics`. Its 10-second
  lease expired before its `extend` call, during the first parallel run after
  a fresh build. An immediate unchanged full rerun passed. The test checks that
  reads do not renew a lease and that extension adds exactly the requested
  time; it does not test short expiry. Its lease is now 60 seconds, and all of
  its assertions are unchanged: the deadline is the same after reads, it moves
  by exactly one second on extension, `stop` is idempotent and a stopped job
  rejects extension. The explicit stop still ends the job promptly. This change
  is test-only; production code is unchanged. After it, the focused test passed
  five consecutive runs, and a full run passed three unit and thirty-one
  integration tests. The test root was empty afterwards, and no fixture process
  remained.
- The packaged `aarch64-apple-darwin` archive passed the archive check and the
  installer check with isolated install directories.
- The archive check previously used a one-second lease, which could expire
  before the command spawned. It now uses a six-second lease and requires the
  command's start marker before it accepts expiry. Cleanup waits for live
  supervisors that name its state root, without signalling recorded PIDs. On a
  failure or a remaining supervisor, it keeps the directory for diagnosis.
- A local probe confirmed that this matcher finds a running job's two supervisors,
  and none after the stop.
- On a deep test root, the archive check failed. The canonical state path was too
  long for a Unix socket. The check now uses shorter directory names, and its
  failures include the CLI error output.
- `cargo package --allow-dirty` listed and built 45 files. These derive from public
  repository files, including the website, scripts, workflows and agent
  instruction files, plus Cargo-generated metadata and manifest copies; no
  private notes were included. Packaging rules were not
  changed. `cargo doc --no-deps` built local CLI documentation. `cargo rustdoc
  --lib` reports no library target, so docs.rs would show only an overview.

The committed candidate `8398c56` passed the
[four-target native release workflow](https://github.com/jaredchu/proclease/actions/runs/42339399964).
Each macOS ARM64/Intel and Ubuntu 22.04 ARM64/x64 job passed formatting, Clippy,
three unit and thirty-one integration tests, the optimized build, native archive
expiry and isolated installer checks. The branch run skipped the tag-only draft
job. All four downloaded archives matched their individual SHA-256 files and
contained the executable, portable skill, README and license. The separate
[Rust checks](https://github.com/jaredchu/proclease/actions/runs/42339391124) and
[website checks](https://github.com/jaredchu/proclease/actions/runs/42339391147)
also passed. No tag, release, registry upload or website deployment was created.
A clean source package from `8398c56` passed build verification and identified
that commit in Cargo's VCS metadata, without a dirty flag.

## Website refresh, 2026-11-07

The pre-publication website review covered a new SVG lease-ring mark and matching
16/32/48px ICO, PNG and Apple touch icons, plus a 1200x630 social image. A fluid
rem-based scale reduces the hero heading to 36–56px and raises labels to at least
12px at default browser text size. Copy and demo states contain no em dashes.
The [website decisions](../website/README.md#search-and-answer-engine-decisions)
link the exact NavSEO and primary platform sources used for the static content,
navigation, metadata and factual JSON-LD.

Local Chromium checks covered 320/390/768/1440px layouts, visible navigation,
lease demo expiry/replay, installer selectors and four platform links, clipboard
failure fallback, keyboard tabs/focus, FAQ, reduced motion and core content with
JavaScript disabled. There were no page errors or failed local assets. Static
checks validated references, canonical/sitemap consistency, JSON syntax, icon and
social dimensions, em dash absence and the unchanged installer mirror; JavaScript
and shell syntax checks passed. Formatting, Clippy with warnings denied and all
three unit plus thirty-one integration tests also passed; Rust code was unchanged.

Review found horizontal overflow at 320px with text doubled. Narrow CSS changes
allow the hero grid, buttons and demo rows to wrap without reducing enlarged text
or hiding overflow. Both root-font and computed-font doubling probes then fit at
320 and 390px, and an independent 320px check confirmed no document overflow.
These are simulated text-size checks in Chromium, not a complete browser or
assistive-technology audit.

One minor limitation remains: at 320px with 200% text, the closed native skills
selector shortens `Codex + Claude Code`; opening the picker displays the full
option. No ranking, indexing, rich-result, field performance or AI-citation
outcome was measured during that implementation task. The subsequent release
and website deployment are recorded below; the selector limitation remains.

## v0.1.3 publication, 2026-11-07

The experimental [v0.1.3 prerelease](https://github.com/jaredchu/proclease/releases/tag/v0.1.3)
is published from tag commit `bdbe7854a3bd3f1adcd8d2a00ebfff944f2424cd`.
The [tag release workflow](https://github.com/jaredchu/proclease/actions/runs/42345415344)
passed native macOS ARM64/Intel and Ubuntu 22.04 ARM64/x64 builds. Each passed
formatting, Clippy, three unit plus thirty-one integration tests, optimized build,
archive startup/expiry and isolated installer checksum-failure checks.

All ten draft assets were inspected before publication: four archives, their
individual checksum files, combined
[SHA256SUMS](https://github.com/jaredchu/proclease/releases/download/v0.1.3/SHA256SUMS)
and the installer. Every archive matched both checksum records, had safe members
and executable permissions, and contained README/license/skill bytes matching
the tag. The installer and release notes also matched the reviewed tag. The
macOS ARM64 archive passed an additional extracted-binary expiry check and
isolated installer check. Afterwards, all ten public assets were downloaded
anonymously and matched the inspected draft bytes.

Main was fast-forwarded to the released commit before website deployment. All
eleven files served by [proclease.com](https://proclease.com/) matched that
commit over anonymous HTTPS. Live Chromium smoke checks at 1440 and 390px passed
demo expiry, download selection and horizontal bounds without page errors. The
live installer fetched the public release and installed the matching native
v0.1.3 binary into isolated directories. No global installation was changed.
The [enlarged-text selector limitation](#website-refresh-2026-11-07) remains.
Registry publication, including crates.io/docs.rs, is still deferred.

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
they do not establish comparative superiority. User-local installation and skill
discovery are verified, and one ordinary local Codex preview session has been
observed. That session exposed the corrected registry-selection issue. The revised
guidance has not been independently tested in a fresh model-driven session;
Claude's launch workflow and each host's restricted execution profile remain
untested.

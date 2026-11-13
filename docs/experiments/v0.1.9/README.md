# ProcLease v0.1.9 lifecycle experiments

Scripted black-box experiments on 2026-11-13 measured expiry, failure handling
and retry behavior of the **published v0.1.9 release binaries** on native macOS
arm64 and native Linux x86_64. A Python standard-library harness drove the
unmodified executables. Independent process, port and HTTP observations, not
ProcLease's own status, decided whether cleanup happened.

This is deterministic host CLI testing. No model-driven agent session took
part, so these results add nothing to Codex or Claude Code compatibility (see
the separate [compatibility matrix](../../agent-compatibility.md)). They are not
a benchmark and make no comparison with other tools.

There are two counted series, run with the same binaries and settings. The
**primary series** used [protocol version 2](protocol.md) and the published
harness. An earlier series used [protocol version 1](protocol-v1.md), whose
observer was amended before publication. It also passed every trial, and it
remains published separately under its own run IDs (see
[Observer amendment](#observer-amendment-and-earlier-series)). The two
denominators are not combined.

## Result (primary series, protocol version 2)

**All 1,140 counted trials passed: 570 of 570 on each platform** (19 scenarios
× 30 repetitions), including **1,020 product trials and 120 observer controls**.
No trial failed, timed out, errored, was unobserved or was
not ready. Every process observation and every final cleanup scan succeeded, so
no trial ended on an observer error. Both negative controls behaved as designed
in every repetition: the observer did not report cleanup for a live process or
without a positive prior observation.

| Scenario | macOS arm64 | Linux x86_64 |
| --- | --- | --- |
| `detached_expiry_graceful` | 30/30 pass | 30/30 pass |
| `detached_expiry_term_resistant` | 30/30 pass | 30/30 pass |
| `foreground_expiry_graceful` | 30/30 pass | 30/30 pass |
| `foreground_expiry_term_resistant` | 30/30 pass | 30/30 pass |
| `detached_keeper_sigkill` | 30/30 pass | 30/30 pass |
| `detached_guardian_sigkill` | 30/30 pass | 30/30 pass |
| `foreground_sigint_blocked_stdout` | 30/30 pass | 30/30 pass |
| `retry_sequential_detached` | 30/30 pass | 30/30 pass |
| `retry_sequential_foreground` | 30/30 pass | 30/30 pass |
| `retry_concurrent8_detached` | 30/30 pass | 30/30 pass |
| `retry_concurrent8_foreground` | 30/30 pass | 30/30 pass |
| `reuse_changed_env_detached` | 30/30 pass | 30/30 pass |
| `reuse_changed_env_foreground` | 30/30 pass | 30/30 pass |
| `mismatch_command_detached` | 30/30 pass | 30/30 pass |
| `mismatch_command_foreground` | 30/30 pass | 30/30 pass |
| `terminal_retry_detached` | 30/30 pass | 30/30 pass |
| `terminal_retry_foreground` | 30/30 pass | 30/30 pass |
| `negative_control_unmanaged_live` | 30/30 pass | 30/30 pass |
| `negative_control_blind_observer` | 30/30 pass | 30/30 pass |

The [protocol](protocol.md) defines each scenario and its pass criteria. In
short, across both platforms:

- **Expiry.** Detached and foreground leases of 3 s ended both the parent and
  its same-group child and closed the port within the declared windows. With
  default TERM handling, both were seen stopped within 0.104 s of the deadline.
  With TERM ignored, both kept executing through the 1 s grace: the processes
  were first seen stopped, and the port refusing, between 1.018 and 1.112 s
  after the deadline. The observer cannot see
  which signals ProcLease sent or in what order; it shows only that the fixture
  survived the grace and then stopped. Every lease ended `expired` with reason
  `deadline`, an unchanged deadline and `finalized_by_keeper: true` (120/120
  per platform). Foreground launchers exited 0 and wrote exactly one stdout
  object.
- **Supervisor failure.** Each job was listed `running` and the keeper or
  guardian's identity was verified before its SIGKILL. The TERM-resistant
  service group was then seen stopped and its port refused within 1.104 s (bound
  3.5 s), and no supervisor remained 6 s later. All 120 records read
  `interrupted`. The **state differs from the cleanup outcome**:
  `finalized_by_keeper` was false for all 60 keeper kills, because the keeper
  that would write it was dead, and true for all 60 guardian kills.
- **Foreground interrupt with blocked output.** Before every signal, the
  harness checked its required preconditions: one job listed `running`, the
  stdout pipe still full and the launcher alive with a verified identity. All
  60 trials met them. One SIGINT made the launcher exit 130 within 1.187 s
  (bound 6.5 s). At exit the pipe still held exactly the harness's bytes, so no
  launcher output byte had been accepted, and no supervisor remained. Polling
  after exit confirmed cleanup. A single sample taken just after the launcher
  was reaped also found both fixture processes absent and the port refusing in
  all 60 trials; it is recorded but not required. Records read `interrupted`,
  reason `supervision_lost`.
- **Retries.** Sequential and simultaneous same-key requests (8 callers
  released by a gate) created exactly one job and one service start in every
  trial. For simultaneous foreground callers, the creator held the session and
  the other seven returned immediately with `reused: true`. A changed
  synthetic environment was rejected by an ordinary keyed retry, which named
  `environment` without disclosing the new value. An explicit `--reuse` then
  returned the same job and deadline while the service kept its original
  environment. A changed command was rejected with `differs: command` both with
  and without `--reuse`. Retries of an expired job, ordinary or `--reuse`,
  returned the retained `expired` record without starting the service again.

### Timing summaries

Seconds after the event, median and maximum of 30 trials in the primary series.
These are descriptive observations of a 0.05 s polling observer with up to three
trials in parallel, not benchmarks; the platforms differ in hardware, load and
Python version.

| Measurement | macOS median / max | Linux median / max |
| --- | --- | --- |
| Detached expiry, default TERM: parent and child seen stopped | 0.070 / 0.103 | 0.026 / 0.067 |
| Detached expiry, TERM ignored (1 s grace) | 1.051 / 1.099 | 1.034 / 1.056 |
| Foreground expiry, default TERM | 0.076 / 0.104 | 0.018 / 0.065 |
| Foreground expiry, TERM ignored | 1.058 / 1.112 | 1.042 / 1.057 |
| Foreground launcher exit after deadline, TERM ignored | 1.133 / 1.157 | 1.051 / 1.063 |
| Keeper SIGKILL (TERM ignored) | 1.067 / 1.104 | 1.030 / 1.084 |
| Guardian SIGKILL (TERM ignored) | 1.064 / 1.095 | 1.027 / 1.079 |
| Foreground SIGINT with blocked stdout (TERM ignored) | 1.155 / 1.200 | 1.093 / 1.120 |
| Same, launcher exit | 1.143 / 1.187 | 1.086 / 1.112 |

Except for launcher exits, each value is the later of the first polls that saw
the parent and the child `absent` or `zombie`. The port was seen refusing
connections at or before that poll, by at most 0.072 s. Readiness preceded each 3 s deadline
by at least 2.50 s. Immediate keyed retries returned in at most 0.046 s; this
is a CLI wall time, not a throughput claim.

## Observation distinctions

The data records the class of each fixture process at the poll that ended
observation: `absent` (no process) or `zombie` (exited, not yet reaped; never
counted as executing). In the primary series these were 360 `absent` and 60
`zombie` of 420 role observations on macOS, and 352 `absent` and 68 `zombie` on
Linux (parent and child in the 210 expiry, supervisor-kill and interrupt trials
per platform). No observation ended `identity_unknown`.

`identity_unknown` means the PID still existed and was not a zombie, but its
live command line lacked the trial token. Under protocol version 2 such a sample
is counted and never treated as a stop. It appeared transiently, at most one
sample per process, for 10 role observations in 10 trials on macOS and 22 in 21
trials on Linux. Each time, the next polls saw the process `absent` or
`zombie` within the scenario bound. The harness cannot tell an exit transition
whose argument vector is no longer readable from PID reuse. Because it waits for
`absent` or `zombie`, the timings above include the extra poll.

## Observer amendment and earlier series

A prepublication review of the first harness (protocol version 1) found three
observer weaknesses:

- it ignored `ps` exit status and error output, so a failed or denied
  enumeration could read as process absence or an empty final scan;
- it counted a live PID without the trial token as stopped (`reused`);
- it logged, but did not enforce as an abort, the blocked-output interrupt's
  preconditions.

Its sanitizer also listed owner-specific identifiers. Protocol version 2 fixes
these without changing scenarios, settings, fixtures or pass windows; the
[protocol](protocol.md#amendment-from-version-1) lists each change. The version 2
harness and protocol were frozen, smoke-checked once per platform (uncounted),
and then the full 30 × 19 counted series was repeated on both hosts. The
published script is therefore exactly the measured primary artifact.

The earlier counted series also passed **570 of 570 trials on each platform**,
but with the version 1 observer, so it is not the primary evidence. Its records
remain unchanged, including 21 `reused` labels in 840 exit observations: 5 on
macOS and 16 on Linux. Under version 1, each of those samples ended observation
for that process as if it had stopped. In those trials the port also refused
connections, and the final scans found no live process with the trial token.
However, version 1's scans did not check `ps` success, so that is weaker
evidence. Version 2 replaces these labels as primary evidence because it never
treats a missing token as proof of exit.

| Series | macOS run ID | Linux run ID | Protocol constant | Harness | Methodology |
| --- | --- | --- | --- | --- | --- |
| Primary (v2) | `counted-macos-arm64-20261113T013637Z-47a2` | `counted-linux-x86_64-20261113T013640Z-670f` | `27fa78ea…b6e3` | `b818a9a6…5181` ([published](../../../scripts/experiments-v019.py)) | `29d65d03…fb55` ([protocol.md](protocol.md)) |
| Earlier (v1) | `counted-macos-arm64-20261113T011416Z-be9b` | `counted-linux-x86_64-20261113T011414Z-063b` | `ec28ec60…3a38` | `55c460a7…dcca` (retained privately) | `fe6f4fed…d93b` ([protocol-v1.md](protocol-v1.md)) |

The version 1 harness is not published because its sanitizer list contained
owner-specific identifiers; it is kept with the private evidence, and its hash
is recorded in every version 1 result file. `protocol-v1.md` is byte-identical
to the methodology file hashed by those runs.

## Tested artifacts and conditions

Both binaries were extracted from the published
[v0.1.9 release](https://github.com/jaredchu/proclease/releases/tag/v0.1.9)
archives after checking them against its `SHA256SUMS` (SHA-256
`e3869f9dc6e56240a09a1e84e4f83ec66afd9fb53e23d2f7099f6a02abae0eba`). Each
reported `proclease 0.1.9`. The same binaries were used in both series.

| | macOS | Linux |
| --- | --- | --- |
| Archive | `proclease-v0.1.9-aarch64-apple-darwin.tar.gz` | `proclease-v0.1.9-x86_64-unknown-linux-gnu.tar.gz` |
| Archive SHA-256 | `2e3b0bf36eb517f5badc531e63d6e17572d43280a102d12f3acd455acb8686a1` | `86fb6e55df489b85f93c62bfe2efe3df2f85a017662356a1b15634daf95e8d4c` |
| Binary SHA-256 | `0275b1ef9e0c176bb21a9ff073fc064f859d9d2c946cb54abe4acafe0fee4af6` | `314f990612c8d81b45d936e09363d9e87e2cbd4faca1674fc7cc9358ae60276e` |
| OS | macOS 27.2.1 (Darwin 27.2.0), arm64 | Arch Linux, kernel 7.2.14-arch1-1, x86_64 |
| Logical CPUs | 10 (Apple M4) | 16 |
| Python | 3.9.6 (`/usr/bin/python3`) | 3.14.9 |
| Task storage | APFS, local volume | ext4, local disk |
| Primary run (UTC) | 01:36:37 to 01:44:50 | 01:36:40 to 01:44:10 |
| Primary load average, start / end (1 min) | 2.75 / 3.89 | 0.25 / 0.32 |
| Earlier run (UTC) | 01:14:16 to 01:22:23 | 01:14:14 to 01:21:43 |
| Earlier load average, start / end (1 min) | 3.50 / 2.36 | 0.30 / 0.63 |

Task storage types come from host metadata receipts recorded with the private
evidence. Both series used the same fixture settings, an ordinary non-root
account and at most three trials in parallel. The macOS host had other ordinary
user activity; the Linux host was mostly idle. In the primary series, a
successful process scan before and after each run found no process referring to
the run's private directory, and no supervisor of a trial state remained at the
end.

## Process

1. Protocol version 1 and its harness were written and frozen before the first
   counted trial. One uncounted smoke run per platform (19 trials each) passed,
   then one counted run per platform passed 570 of 570.
2. A prepublication review found the observer issues above. Protocol version 2
   and its harness were frozen with recorded hashes. Observer, failure-injection
   and sanitizer self-checks passed on both platforms. They include `ps`
   failures and a missing token injected after successful pre-observation, and
   a failed final scan. One uncounted version 2 smoke run per platform (19
   trials each, same harness bytes) passed.
3. The primary counted run per platform followed with new run IDs in the same
   isolated task roots. No trial in either series was retried, discarded or
   rerun. Smoke runs are debug evidence; they are not counted or published.

## Limitations

- These are synthetic Python fixtures, not Vite, Next.js or other application
  servers, and deterministic CLI calls, not agent sessions or agent sandboxes.
- Cleanup was observed for a parent and one same-group child. That does not
  show that no descendant escaped. A descendant that creates its own session or
  process group can escape ProcLease cleanup, and no containment is claimed.
- Process observation depends on `ps` seeing the processes. Positive
  pre-observation, strict `ps` result checks and the negative controls guard
  against a blind or failing observer, but they cannot detect processes the
  observer was never told about.
- `identity_unknown` samples cannot be attributed to exit transitions or PID
  reuse. The observer waited for `absent` or `zombie` instead of deciding.
- The observer sees processes and the port, not signals. The TERM-resistant
  windows show survival through the grace, not the signal sequence.
- CPU use and resource accounting were not measured. Absence is observed at a
  0.05 s polling interval; latencies include `ps` and polling overhead.
- The harness signals only identity-verified processes it or the trial owns. A
  PID could in principle be reused between verification and signal; this
  window was not observed but is not excluded.
- Thirty repetitions per scenario cannot rule out rarer failures. Sleep/wake,
  clock changes, simultaneous destruction of both supervisors, whole-sandbox
  teardown and restricted agent profiles were not tested.
- Two hosts with different hardware, load and Python versions were used, so
  timing differences are not attributable to the operating system alone.

## Data and reproduction

- [Protocol version 2 and pass criteria](protocol.md);
  [historical protocol version 1](protocol-v1.md)
- [Harness](../../../scripts/experiments-v019.py) (version 2, the primary
  measured script)
- Primary series, macOS:
  [summary](results/counted-macos-arm64-20261113T013637Z-47a2/summary.json),
  [per-trial records](results/counted-macos-arm64-20261113T013637Z-47a2/trials.json)
- Primary series, Linux:
  [summary](results/counted-linux-x86_64-20261113T013640Z-670f/summary.json),
  [per-trial records](results/counted-linux-x86_64-20261113T013640Z-670f/trials.json)
- Earlier series (protocol version 1), macOS:
  [summary](results/counted-macos-arm64-20261113T011416Z-be9b/summary.json),
  [per-trial records](results/counted-macos-arm64-20261113T011416Z-be9b/trials.json)
- Earlier series (protocol version 1), Linux:
  [summary](results/counted-linux-x86_64-20261113T011414Z-063b/summary.json),
  [per-trial records](results/counted-linux-x86_64-20261113T011414Z-063b/trials.json)

Public records hold scenario and repetition identifiers, outcomes, reasons,
booleans, counts, timings, lifecycle states and generic OS metadata. Paths,
PIDs, tokens, raw CLI output and observer error details stay in private task
storage. To check the files, run:

```sh
/usr/bin/python3 -I scripts/experiments-v019.py validate \
  docs/experiments/v0.1.9/results/counted-*
```

This recomputes each summary from its trial records, checks denominators and
repetition identifiers, and reports which protocol version each directory used
(current or listed historical hash). It also rejects path-like, address-like
and hostname-like strings, trial tokens, and the account and host names of the
machine running the check. See the [protocol](protocol.md#reproduce) to run the
experiments yourself.

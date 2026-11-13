# ProcLease v0.1.9 lifecycle experiments: protocol

This protocol was written, with its harness, before any counted measurement.
It fixes the scenarios, pass criteria, timing tolerances and outcome
classification for black-box tests of the **published v0.1.9 release
executables**. Results and findings are in the [report](README.md).

- Harness: [`scripts/experiments-v019.py`](../../../scripts/experiments-v019.py),
  Python standard library only.
- Harness SHA-256 at freeze:
  `55c460a7bbf53ef7edeadc8b3c6b385672eeffc0355d885e89d1998bc059dcca`
- Protocol constant SHA-256 (canonical JSON of `PROTOCOL`, printed by
  `experiments-v019.py protocol`):
  `ec28ec60c068b23454f334d31b0763684f3f70750a2ec2b96c01c1b032f23a38`
- Every public result file records the harness, protocol, methodology
  (this file) and binary hashes.

## What is and is not tested

The harness starts the unmodified published `proclease` binary as an external
program, with argument vectors and no shell. It does not build or import
ProcLease and does not reimplement its logic. These are deterministic host CLI
tests. They are not real model-driven agent sessions and make no claim about
agent compatibility, nor any comparison with another tool. Concurrency up to
three trials makes latency figures descriptive only; they are not benchmarks.

## Isolation

Each trial has its own private state directory (`--state-dir`, passed on every
call), `HOME`, `TMPDIR`, working directory and fixture files under a private
task root outside the repository. The CLI environment is synthetic: `PATH`,
`HOME`, `TMPDIR`, `LANG`, `LC_ALL` and one marker variable
`PL_EXPERIMENT_MARKER`. No ordinary ProcLease registry, installed CLI or
running job is read or controlled. Run directories are never reused; reruns get
a new run ID and keep earlier results. Smoke runs (`--kind smoke`) are debug
runs and are never counted.

## Fixture

`experiments-v019.py fixture MODE TOKEN READY STARTS` is a disposable loopback
HTTP service. The parent binds `127.0.0.1:0`, forks a child in the same process
group, and the child serves `GET /` with a JSON body containing the trial's
random token and the inherited marker variable. Both processes hold the
listening socket, so the port only refuses connections after both have exited.
The parent appends one line per start to a starts file, then publishes its PID,
child PID, port and process group. Mode `graceful` keeps default SIGTERM
handling; `resistant` ignores SIGTERM in both processes. Each process has a
180-second `alarm` as a bounded backup.

## Independent observation

ProcLease `list` is recorded as lifecycle **state** only. Cleanup is judged by
an observer that does not use ProcLease:

- **Identity:** `ps -ww -o pid,ppid,pgid,stat,command -p PID`. A process counts
  as executing only when it exists, is not a zombie (`stat` starting with `Z`)
  and its live command contains the trial token. A zombie is classified as
  exited, not executing. A PID whose command no longer contains the token is
  classified `reused`.
- **Readiness/marker:** an HTTP GET to the published port must return the token.
- **Port:** a TCP connect to `127.0.0.1:PORT`; only `ECONNREFUSED` counts as
  closed. Timeouts and other errors are not closure.
- **Timing:** `time.monotonic()`. A lease deadline is mapped to monotonic time
  from `expires_at_ms` and a wall/monotonic sample taken right after launch.
- **Polling:** every 0.05 s, bounded per scenario (never more than 10 s after
  the event under test).

**Positive pre-observation is required.** Before any injection (or before the
deadline for expiry), the observer must see parent and child executing with the
token, in the same process group, and the HTTP marker. If it cannot, the trial
is `unobserved`; absence observed afterwards can never produce a cleanup pass.
`confirmed` cleanup means the parent and child stopped executing **and** the
port refused, after positive pre-observation. Same-group checks do not show that
no descendant escaped; ProcLease documents that a descendant that creates a new
session or group can escape, and no containment claim is made.

**Signals.** The harness signals a ProcLease supervisor only after reading its
current `ps` entry and confirming: `__keeper` or `__guard` in the command, this
trial's state directory and job ID, the guardian's parent is the keeper, and the
fixture's process group is led by the guardian. It signals a foreground launcher
only while it is the harness's own unreaped child and its command contains
`--foreground` and the trial state directory. Persisted PIDs are never signalled
on their own authority.

## Fixed settings

| Setting | Value |
| --- | --- |
| Short lease (`--ttl`) | 3 s |
| Long lease | 30 s; 120 s for the blocked-output interrupt |
| `--grace` | 1 s |
| Early tolerance | 0.25 s before the expected earliest exit |
| Late tolerance | 2.0 s after the expected earliest exit |
| Supervisor-kill cleanup bound | 3.5 s after SIGKILL (grace + 2.5 s) |
| Foreground signal exit bound | 6.5 s (documented stop bound grace + 2 s, plus 3 s, plus 0.5 s) |
| Negative-control window | 2.0 s |
| Concurrent same-key callers | 8, released together by a gate file |
| Repetitions | 30 per scenario per platform |
| Parallel trials | at most 3 |
| CLI call timeout | 20 s; retries that must return immediately: 10 s |

## Scenarios and pass criteria

All scenarios also require final own-task cleanup: after the scenario, the
harness stops any remaining job through ProcLease, waits for its own launchers,
then requires no live supervisor for the trial state and no live process with
the trial token. Any residue fails the trial (the harness then SIGKILLs only a
re-verified own fixture process and reports it).

| Scenario | Setup | Pass criteria |
| --- | --- | --- |
| `detached_expiry_graceful` | Default mode, 3 s lease, graceful fixture | Ready before deadline; parent and child stop executing and port refuses between deadline − 0.25 s and deadline + 2.0 s, the port by that window end + 0.5 s; state `expired`, reason `deadline`, deadline unchanged |
| `detached_expiry_term_resistant` | Same, TERM-resistant fixture | As above, but the window starts at deadline + grace − 0.25 s (proves TERM then grace before KILL) and ends at deadline + grace + 2.0 s |
| `foreground_expiry_graceful` | `run --foreground --json`, otherwise as graceful | As graceful; launcher exits 0; stdout holds exactly one JSON object |
| `foreground_expiry_term_resistant` | Foreground, TERM-resistant | As TERM-resistant; launcher exits 0; one stdout object |
| `detached_keeper_sigkill` | Default mode, 30 s lease, TERM-resistant; keeper SIGKILL after identity checks | Cleanup confirmed within 3.5 s of the signal; state `interrupted`; no live supervisor 6 s later. `finalized_by_keeper` is recorded, not required |
| `detached_guardian_sigkill` | Same, guardian SIGKILL | Same criteria |
| `foreground_sigint_blocked_stdout` | Foreground `--json`, 120 s lease, TERM-resistant; stdout is a full pipe never read; SIGINT to the launcher after readiness and 0.5 s | Pipe full before the signal; exit code 130 within 6.5 s; the pipe is still unread at exit; cleanup confirmed; no live supervisor at exit; state `interrupted`, reason `supervision_lost` |
| `retry_sequential_{detached,foreground}` | Keyed launch, then an identical keyed retry (foreground retries pass `--foreground`) | Retry exits 0 within 10 s with `reused: true`, same ID and deadline; one job, one fixture start, fixture still executing; then stop with confirmed cleanup (foreground launcher exits 0) |
| `retry_concurrent8_{detached,foreground}` | Eight identical keyed callers released together | One distinct job ID; exactly one `reused: false` and seven `reused: true`; no nonzero exit; all detached callers exit (foreground: seven exit and the creator holds the session); one job; one fixture start; then stop with confirmed cleanup |
| `reuse_changed_env_{detached,foreground}` | Keyed launch with marker A; retries with marker B | Ordinary retry exits 1 naming `environment` without disclosing B; `--reuse` exits 0, `reused: true`, same ID and deadline; the service still serves marker A; one job and one start |
| `mismatch_command_{detached,foreground}` | Keyed launch; retries with one extra fixture argument | Ordinary and `--reuse` retries both exit 1 naming `command`; one job and one start; original still running with unchanged deadline |
| `terminal_retry_{detached,foreground}` | Keyed 3 s lease left to expire | Expiry cleanup confirmed and state `expired`; ordinary and `--reuse` retries each exit 0 with `reused: true`, state `expired`, same ID and deadline; 1 s later still one start, one job, port refused and no token process (no resurrection) |
| `negative_control_unmanaged_live` | Harness-started fixture, no ProcLease, left running for 2 s | Pre-observation succeeds and the observer reports `not_confirmed`; after the harness kills its own group, the observer reports `confirmed` |
| `negative_control_blind_observer` | Observer pointed at a reaped PID and an unbound port | Verdict `unobserved` even though raw absence is seen |

## Outcome classification

Each trial has exactly one outcome, recorded with all reasons:

- `pass`: every criterion met.
- `fail`: the scenario ran and at least one criterion failed.
- `not_ready`: the fixture was not ready before the deadline or bound.
- `unobserved`: positive pre-observation failed.
- `timeout`: a bounded CLI call or startup read timed out.
- `error`: identity could not be verified (no signal sent), or a harness exception.

No trial is retried or discarded inside a run. Summaries are recomputed from the
per-trial records and must match; denominators are the scheduled trial counts.

## Reproduce

```sh
/usr/bin/python3 -I scripts/experiments-v019.py selfcheck --root /short/private/dir
/usr/bin/python3 -I scripts/experiments-v019.py run --kind counted \
  --platform-label macos-arm64 --binary /path/to/extracted/proclease \
  --root /short/private/dir/r --out /private/public-out \
  --archive-sha256 ARCHIVE_SHA256 --methodology-sha256 PROTOCOL_MD_SHA256
/usr/bin/python3 -I scripts/experiments-v019.py validate /private/public-out/RUN_ID
```

Download the release archive for your platform, verify it against the release
`SHA256SUMS`, and extract it into its own empty directory. Keep `--root` short:
detached jobs bind a Unix socket inside it, and macOS limits socket paths to 104
bytes. The run refuses a binary whose `--version` is not `proclease 0.1.9`.

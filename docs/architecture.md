# Architecture and lifecycle boundaries

## Why ProcLease exists

The central use case is forgetting to stop an AI-started development server.
ProcLease focuses on explicit deadlines that work without another model turn.
Generic background execution, JSON status, logs and a terminal view already
exist in [AgentProcs](https://github.com/jkhoffman/agent-procs),
[agproc](https://github.com/jmjoy/agproc), and
[persistproc](https://steveasleep.com/persistproc/). A new implementation and shared
skill were selected by the owner; this is not evidence of measured superiority.

## One job, two supervision processes

```text
caller/agent -> short-lived CLI -> keeper -> guardian -> foreground service
                                  |          |
                                  | pipe     +-- owns service process group
                                  |          +-- owns control socket and deadline
                                  +-- retains guardian until group cleanup
```

Both supervision processes inherit the launching caller's execution context.
There is no global service that accepts arbitrary command-execution requests.
Later commands reach only a job-specific socket whose protocol can stop or extend
that job. State files contain argument vectors and a private context fingerprint,
not a saved environment. A new run always launches from the new caller's context.

The guardian is the group's session leader. Its service inherits that group.
On deadline, explicit stop, command exit, signal, or loss of the keeper's liveness
pipe, the guardian records the shutdown reason, sends group TERM, waits a bounded
grace, then sends group KILL. The guardian intentionally dies in that escalation.

The keeper waits with `waitid(..., WNOWAIT)` and retains its exited guardian as an
unreaped child until group KILL is complete. This prevents the session leader's
numeric ID being reused during cleanup. If the guardian crashes, the keeper
enforces cleanup. If the keeper crashes, EOF causes the guardian to clean up.
Simultaneous destruction of both supervisors is outside this fail-closed guarantee.

Startup failures before the service is spawned are launch failures. If the CLI
cannot spawn the keeper, it records the committed job as `failed` while holding
the launch lock; that record names no supervisor PID and can be collected as
normal finished history. Guardian setup errors, such as a control-socket bind or
capture-file failure, are recorded as `failed` with the original error, and `run`
exits nonzero. Immediately before spawning the service, the guardian compares the
persisted deadline with wall time. An overdue lease finishes `expired` without
running the command. A deadline reached between that check and exec is enforced
on the first supervision tick through the same group cleanup. A supervisor that
exits, times out or never acknowledges startup is not treated as proof that no
service started; that state remains `interrupted` or `unknown` and protected.

Both supervisors arm process-group cleanup immediately after spawning their child.
Returning through a metadata read/write error or another post-spawn failure cannot
skip TERM/KILL cleanup. The keeper disarms that guard before reaping its leader,
so error unwinding cannot subsequently signal a reused numeric group ID.

Control sockets use nonblocking I/O with absolute request/batch/reply budgets.
Slowly streamed requests and clients that stop reading cannot reset a per-read
timeout and hold the lifecycle loop. Filesystem operations still depend on the
underlying filesystem responding; a blocked filesystem is an untested limit.

Only a running supervisor signals its own anchored group. CLI control never kills
a PID read from persistent state. When control is unavailable, list may derive a
terminal result for an already-stopping, now-absent group; otherwise it reports
unverified state. Such observation never signals or adopts a process.

## Consistency and retry semantics

A root advisory lock serializes launch reservations. A unique 12-hex-digit ID
names each job. Explicit retry keys match a launch-context fingerprint covering
the argument vector, canonical directory and environment, plus owner/task fields.
The fingerprint hashes typed cwd, argv and environment sections with element
counts and byte lengths, so moving text between arguments and environment changes
it. Stored argv and directory must also match exactly. Records from earlier
versions use an unprefixed hash without section boundaries; they are accepted
only when stored argv and directory match exactly and that earlier hash matches.
With those fixed, the hash identifies one environment because operating-system
arguments, paths and environment entries cannot contain NUL.
While history is retained, they return the original record, including terminal
records, without renewal. Purging history ends that key's deduplication window.
Changing a context while reusing a key is an error. There is no automatic
command-text deduplication or cross-task ownership transfer.
The key must be assigned before the initial launch. Under the launch lock,
provably empty reservations, or those containing only private generated atomic-write
temporary files, are skipped without deletion. Existing launch metadata
identifies relevant keys even when an unrelated record is damaged; ambiguous
unreadable launch metadata still blocks resolution to avoid duplicate launches.

The guardian serializes stop/extend requests. Extension is additive and bounded
by 24 hours from creation. A request at or after expiry cannot revive the job.
Metadata is replaced atomically using private files and filesystem synchronization.
This requires filesystem support for atomic replacement visible to concurrent
readers; VM shared folders are not assumed to preserve those semantics.
The keeper takes final write ownership after the guardian exits. A finished
result derived by `list`, or returned by `stop`, can precede that final write,
socket removal and the keeper's own collection. Anything that deletes a state
directory, including test teardown, must first wait for its supervisors to exit.
Status reads never renew leases or rewrite active lifecycle state; list/watch may collect
confirmed finished history under the retention policy.
Default list/watch views select active jobs, `unknown` jobs whose cleanup is
unverified, and records without completion timestamps. Up to five newest confirmed
finished jobs remain visible for 30 seconds. `--all` includes retained terminal
records. This is a display filter;
retry resolution and direct job control/log access still use complete stored state.

History retention keeps confirmed finished jobs for at most 24 hours and caps
them at the newest 100. Launches, list/watch, job completion and explicit `prune`
perform opportunistic collection under the root launch lock. No idle background
service is added. Active/unknown records, missing completion timestamps and
unverified supervisor/group absence are protected. A keeper may collect its own
record only after its final write and socket cleanup. Collection never signals
processes. A verified candidate is atomically renamed into a reserved private
quarantine before recursive deletion; interrupted deletion is retried from there,
so partial records do not poison keyed launches. Symlinked job/quarantine roots
are refused. Cleanup failures are warnings, not evidence that space was reclaimed.
Bulk stop protects its fixed selection with a process-lifetime advisory lock on
the private `.retention` file in the state root. It acquires a nonblocking shared
lock while holding the root launch lock, before its scan, and holds it until all
results are captured. Collection, always under the root launch lock, needs a
nonblocking exclusive lock on the same file; while any bulk stop holds it, job
directories are not collected (quarantine retries continue) and the report is
marked deferred. Concurrent bulk stops share the lock; collection resumes after
the last releases it. The OS releases it on exit, crash or kill, so no persisted
pin can be orphaned; after a crash or kill no final pass runs, and collection
resumes on the next ordinary trigger. The lock is cooperative: clients and
already-running keepers from builds without it ignore it, so protection requires
every process pruning the same state directory to include it. The guard is opened
without following symlinks or blocking on FIFOs and must be a private regular
file; otherwise bulk stop fails before
any control and collection defers with a warning. Dry runs take no lock. The
protection is global rather than per job: during a bulk stop, unrelated finished
history can temporarily exceed the bound. The root launch lock is never held
across control or waiting, and keeper process cleanup never waits for this lock.
Repeated stop while stopping polls existing state instead of waiting for an idle
shutdown socket. A transport failure racing with shutdown is reconciled against
the latest record; extensions are never automatically replayed.

Bulk stop (`stop --all`, unreleased source) is a client-side fan-out over the same
per-job sockets, not a new service. It takes the root launch lock only while
scanning records, which orders the snapshot after in-flight launches and with
collection; the lock is released before any control, so keepers can still finish.
Collection is deferred until results are captured (see retention above). The
fixed selection holds active and unknown jobs matching exact owner/task filters.
A record is addressed only by its directory: one whose
embedded ID differs is rejected as unreadable, so it cannot redirect selection,
observation or control to another job. Up to 8 workers send Stop. Each `starting`
job is polled for about 6 seconds until running, and each job gets at most two
exchanges; a repeated Stop to a job already stopping is answered with an error and
changes nothing. Then all dispatched jobs are polled together, each until its grace
(capped at the 10-second `run` maximum, whatever the launch metadata says) plus 2
seconds. Unreadable launch metadata uses that cap and affects only its own job.
Unknown jobs and unreadable records are reported, never signalled, and make the
command exit nonzero. This avoids sequential per-job waiting but is not atomic:
each group still stops on its own supervisor's schedule.

Client socket operations use non-blocking sockets with absolute deadlines rather
than per-read timeouts, which a peer trickling bytes could renew indefinitely.
An availability probe (used by `list`, status reads and selection, including
under the root lock) connects within 0.5 seconds; a full Linux listen backlog is
retried only until then, and a timeout reads as an unavailable endpoint. A control
exchange, single-ID or bulk, completes its connect, request and newline-terminated
reply of at most 1 MiB within 2 seconds in total. An exchange failure is then
reconciled with a fresh observation, as before.

Output capture latches a private one-byte `capture.status` marker before
discarding overflow. The already-open descriptor remains usable during a directory
permission failure, and the keeper/CLI merge the marker into final/read status.
This accounts for output emitted after TERM without making the guardian perform
its final capture snapshot before the grace period.

## Resource sampling

List/watch use `sysinfo` process snapshots and read-only process-group checks.
The guardian must still have the recorded keeper as parent and lead its own
group before service usage is attributed. Group totals exclude the guardian and
keeper and skip Linux thread entries, whose usage is already in process totals;
PID/start-time changes invalidate the CPU baseline. On macOS, newly
inserted snapshot entries receive a baseline refresh before timing CPU deltas.
First-frame CPU, inaccessible counters and unverified groups remain unavailable.
The values are approximate samples, not accounting for exited processes or
descendants that escape the group. RSS can count shared pages repeatedly.
No sampled usage is persisted and no monitoring daemon is introduced. See the
[usage contract](../README.md#resource-usage) for units and JSON opt-in.

## Security and operational limits

- Labels identify the caller's declaration, not authenticated agent identity.
  Same-user access to a private state directory permits that user's control
  clients to stop/extend a job. Skills must respect task ownership.
- Host restrictions can deny spawning, state access or Unix-socket connections.
  ProcLease reports these failures and does not elevate privileges.
- No implicit shell interpretation; stdin is closed. An explicit shell remains
  the caller's responsibility and uses the same host authorization rules.
- Restrictive directory permissions and no-follow file opens protect state from
  ordinary symlink mistakes; same-user malicious processes remain in the trust
  boundary. This is supervision, not a sandbox.
- A detached descendant can escape the managed group. Arbitrary adoption,
  containers and shared consumer leases are not implemented.
- Each job uses two supervisor processes. This favours a small number of local
  development services, not thousands of workers.
- Capture retains the first configured number of bytes and drains the rest.
  Full capture is not promised after the cap. Retention deletes finished metadata
  and logs; retry identity does not outlive that history.
- Deadlines use wall time, including sleep. Clock changes affect expiry. No claim
  of precise execution while asleep is made.

## Verification and comparison

The lifecycle integration suite covers expiry, TERM-resistant descendants,
surviving children after leader exit, explicit stop/extension, observation without
renewal, individual keeper/guardian crashes, bounded logs, retry idempotency,
concurrent retries, private state and launch-context inheritance. Startup
regressions cover argument/environment fingerprint separation and earlier-format
records, overdue leases, guardian setup failure and failed keeper spawn.

These are local implementation tests, not direct tests inside every Codex or
Claude permission profile. Real host compatibility, system sleep/wake and a
pinned-version comparison with alternatives remain separate verification work.
Do not claim ProcLease is faster, safer or more reliable than another tool until
the same relevant scenarios have been measured on both.

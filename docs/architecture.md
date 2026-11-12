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

<a id="pid-context-unreleased"></a>

### PID context (v0.1.8)

Saved PID numbers are meaningful only in the PID space that saved them. From
v0.1.8, a Linux keeper also saves an opaque `pid_context` token with
its first PID. The token is a digest of the boot ID, the keeper's PID-namespace
link and the start time of that namespace's PID 1, in clock ticks since boot
(zero is valid). The namespace inode alone is not used: follow-up native Linux
probes in the strict Claude and Codex sandboxes saw the same `pid:[N]` value in
two tool calls even though the first call's process had died. The procfs
`stat` and `status` files read for this contain process names, but no name is
retained; environment values are never read.

`/proc/1` names that namespace's PID 1 only when procfs was mounted for the
same PID namespace. A context is therefore created only when `NSpid` in
`/proc/self/status` lists exactly one PID and that PID, like the PID in
`/proc/self/stat`, equals the process's own PID. Several `NSpid` levels mean
that procfs belongs to an ancestor namespace. That case, and a missing,
repeated or malformed value, produces no context. This check is required.
`/proc/self/status` is read up to 1 MiB, because a long `Groups:` line precedes
`NSpid`. It does not read PID 1's namespace link, which ordinary users may not
be permitted to read.
On hosts with restricted procfs access, including some `hidepid` mount
configurations, `/proc/1/stat` can be unreadable. No context is established:
PID-derived absence and resource attribution are unavailable, while reachable
protocol controls, expiry and verified keeper-finalized retention still work.
See the [procfs mount options](https://man7.org/linux/man-pages/man5/procfs.5.html).

Observers probe saved PIDs only when their own complete context matches. A
mismatch, an unreadable context, or a Linux record written before this field
fails closed. Resource metrics are `null`. A `stopping` job whose endpoint is
unavailable is never derived finished. A healthy guardian's endpoint closes
shortly before the keeper writes the final record, so the job stays `stopping`
while its `record.json` was last written within the job's grace (capped at
10 seconds) plus 2 seconds. Then it shows as `unknown`. Unreadable launch
metadata and a missing, unreadable or future modification time show `unknown`
at once. Either error names the connect failure kind and says that the saved
PIDs belong to another or unverifiable PID context. Old records stay unchanged
on disk. macOS records have no token and keep their existing behavior. None of
these paths sends a signal. See [retention](#consistency-and-retry-semantics)
for which finished records collection keeps.

Before reserving a new job, `run` creates, binds and removes a temporary Unix
socket in the state directory. It does this after any retained-key lookup, so
`--reuse` remains read-only. If the socket step fails, `run` exits nonzero and
records no job, so the retry key stays unused. A denial names the step and says
that ProcLease requires a Unix-domain control socket. The guardian's own bind
failure adds the same note. Neither message suggests widening a sandbox.
Foreground jobs skip this probe; see below.

<a id="foreground-sessions-unreleased-source"></a>

### Foreground sessions (v0.1.9)

`run --foreground` is available from v0.1.9. Launch and record files persist `mode` (`detached` or `foreground`)
and `transport` (`socket` or `file`). Missing fields, as in every earlier
record, mean `detached` and `socket`. Each mode has exactly one transport. A
guardian refuses a launch whose pair is inconsistent, and no path falls back
from one transport or mode to the other.

```text
run --foreground --pipe--> keeper -> guardian -> foreground service
   (held until the            |          +-- holds ctl.lock for its lifetime
    keeper exits)             |          +-- serves ctl.d request files
                              +-- retains guardian until group cleanup
```

The `run` invocation spawns the same keeper, now as its own child, with a
liveness pipe on the keeper's stdin. It reports startup (and readiness) once,
then waits for the keeper to exit. The keeper exits only after its final
lifecycle write. When the pipe closes, the keeper closes the guardian's pipe.
The guardian then performs its ordinary TERM, grace, KILL shutdown with reason
`supervision_lost`, and the keeper records that the foreground session ended.
If the guardian has not exited within the stop bound, the keeper kills the
group that its unreaped guardian still anchors. The pipe closes however `run`
ends, including SIGKILL. On SIGINT, SIGTERM or SIGHUP, `run` closes the pipe
itself and waits up to the stop bound plus 3 seconds before exiting with
128 + the signal number. That path takes no report lock and writes nothing
itself: its notices (and, before any startup report, the `--json` error
object) come from a detached thread that it waits for at most 250 ms, and it
exits without flushing, so an unread stdout/stderr pipe or a startup report
blocked mid-write cannot delay cleanup or exit. Stdout still carries at most
one object. A repeated signal exits at once. `run` never signals
the job. Keeper and guardian keep their own sessions, so a terminal interrupt
reaches only `run`. Destroying the whole sandbox or client at once is outside
these guarantees: the job can end without a final record and is then shown as
interrupted or `unknown`, and retention keeps it until absence is verified.
A foreground job is bounded by its session and its lease; nothing promises
that it continues after the client or session exits, and cleanup at that
point is best effort.

File control replaces only the socket transport. The guardian creates the
private directory `ctl.d` (0700, which must not already exist) and the file
`ctl.lock` (0600, created new), then keeps an exclusive `flock` on the lock for
its lifetime. The descriptor is close-on-exec, so the service does not inherit
it. An observer treats the endpoint as live only if a non-blocking shared lock
attempt fails because of that lock. A missing, replaced, symlinked or
non-regular lock file, or one that it can lock, is unavailable, and the
existing `unknown`, finalization-window and foreign-PID-context rules apply
unchanged. Heartbeats and timestamps are never used as proof of liveness.

A client first requires a live lock and a private `ctl.d`. It refuses to add
a request when 64 or more entries are already present. That count is each
client's own check, not an atomic global admission: clients that count at the
same moment can each add one request beyond it. The guardian's per-tick limits
below bound its work regardless. Each request has a
random 32-digit ID and is at most 4096 bytes. It names the job ID, the request
ID, its creation time and one `stop` or `extend` action, and is published by
an atomic rename as `req-<id>`. On each 25 ms tick, the guardian examines at
most 128 entries and claims at most 8 requests. Before applying a request, the
guardian opens it with no-follow and non-blocking flags, then unlinks it to
claim it. It accepts only a private regular file owned by the user, with one
link, within the size limit, with the expected fields, the matching job and
request IDs, and a creation time inside the request's validity window: no more
than 1 second ahead of the guardian's clock and no older than the 2-second
exchange budget. Refused requests are answered with an error and are not
applied.

The guardian remembers each applied request ID, with at most 256 IDs, until
that request's own window has closed (its creation time plus 2 seconds, so up
to 3 seconds after it was applied when it was stamped 1 second ahead). Any
request file named with a remembered ID is dropped before it is read, without
an answer, so neither an identical nor an altered copy is applied again or
replaces the original reply. After the window closes the ID is forgotten and a
copy is refused as stale. The guardian also keeps the latest window end it has
forgotten and refuses requests whose window ends no later, so a backward
wall-clock step cannot reopen a forgotten window; after such a step, fresh
requests are refused as stale until the clock passes that point. When all 256
remembered IDs are still inside their windows, a new valid request is refused
with an error and not applied; no remembered ID is evicted early. This
deduplication is per guardian and assumes cooperating clients. It is not
authentication or anti-replay against a same-user process, which can write an
entirely new request with a new ID and creation time.

The guardian applies requests with the same rules
as the socket transport and publishes `rep-<id>` atomically. The client accepts
a reply only after the same checks (with a limit of 1 MiB) and a matching
correlation. If no reply arrives within 2 seconds, the client unlinks its own
request. Success means the request was never taken and was not applied.
`ENOENT` means the guardian took it, so the outcome is unknown and it is not
resent. Unclaimed replies and partial writes older than 10 seconds are
removed, and so are names outside this grammar. Removing an entry never
follows it. The keeper removes `ctl.d` and `ctl.lock` after group cleanup and
before its final write.

This is the same private same-user trust boundary as the rest of the state
directory. Any process running as the same user can write these files or hold
the lock, as it already can for other state. File control does not identify an
agent or caller.

File control carries only `stop` and `extend` between the files of one job
directory. It does not forward application ports, proxy traffic or join
another process's network namespace. On hosts that give each tool call a
private localhost, such as the strict Linux Claude Code sandbox, HTTP requests
and readiness port checks made from a later call can fail even while that
call's file control and log reads succeed. Foreground sessions therefore do
not establish cross-call port reachability for Vite, Next.js or any other
service.

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
command-text deduplication or cross-task ownership transfer. From v0.1.9,
the launch mode is part of that policy: a key never switches between a
detached and a foreground job, and the rejection names `mode`. The mode is
compared even with `--reuse`, which relaxes only the environment comparison. A
matching retry or `--reuse` of a foreground job, which must itself pass
`--foreground`, returns immediately without attaching, blocking or renewing.
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
result derived by `list`, or returned by `stop`, can precede the keeper's socket
removal, its final write and its own collection. Anything that deletes a state
directory, including test teardown, must first wait for its supervisors to exit.
Status reads never renew leases or rewrite active lifecycle state; list/watch may collect
confirmed finished history under the retention policy.
Default list/watch views select active jobs, `unknown` jobs whose cleanup is
unverified, and records without a usable recent-history timestamp. Up to five
newest finished jobs remain visible for 30 seconds. A saved `stopping` record
whose keeper and managed group are verified absent uses the record file's last
modification time for this display window, including records from older versions.
The anchor is observation-only: exact completion time remains unknown, JSON and
on-disk metadata are unchanged, and retention still protects the record. Missing
or future modification times and uncertain keeper/group absence keep the record
visible. `--all` includes retained terminal
records. This is a display filter;
retry resolution and direct job control/log access still use complete stored state.

History retention keeps confirmed finished jobs for at most 24 hours and caps
them at the newest 100. Launches, list/watch, job completion and explicit `prune`
perform opportunistic collection under the root launch lock. No idle background
service is added. Active/unknown records and missing completion timestamps are
protected. From v0.1.8, only the keeper's final write can set
`finalized_by_keeper`. That write happens after the keeper has killed the
guardian's group, reaped the guardian, accounted for capture and removed the
socket, so no lifecycle writer remains. The final write alone does not prove
that every group member has gone: a descendant in uninterruptible sleep can
outlive the reaped leader with SIGKILL pending. The keeper therefore sets the
marker only after it observes the group absent, re-observing for at most 200ms
so that orphaned zombies can be reaped. The observation sends no signal; the
keeper never signals the numeric group after reaping its leader. Live members,
including an unrelated group that reused the number, or a failed or uncertain
observation leave the marker false, and the record is still saved. The marker
thus means verified group absence and no future lifecycle writer. A
guardian-written terminal state never carries it. Finalized finished records
are collected under the normal age and count limits without interpreting saved
PIDs, so new history stays bounded across reboots, PID-namespace changes and
container restarts when that absence was observed. Finished records without the
marker, from earlier versions, a keeper that did not complete its final write
or one that could not observe its group absent, still need verified keeper and
group absence. On
Linux that requires a matching [PID context](#pid-context-unreleased), so after
a reboot or from another PID namespace such records, and their retry keys, stay
until the user removes them. Failed launches recorded by the CLI without any
supervisor PID remain collectable. A keeper may collect its own record only
after its final write and socket cleanup. Collection never signals processes. A verified candidate is atomically renamed into a reserved private
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

Bulk stop (`stop --all`, available from v0.1.5) is a client-side fan-out over the same
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

<a id="readiness-observation-unreleased-source"></a>

## Readiness observation (v0.1.6 and later)

`run --wait-for-port/--wait-for-log` is a CLI-side observation after launch, not
lifecycle state. The launch lock is released before waiting. Lifecycle authority
stays with the job's supervision pair: the guardian owns the control socket,
deadline and normal stop/expiry escalation, and the keeper retains the guardian
and enforces cleanup if it fails. The wait reads `record.json` and writes
nothing; it sends no control request, so it cannot extend, renew, stop or
relaunch a job. A timeout leaves the job supervised under its original deadline.

One monotonic budget starts when waiting starts. Loopback TCP attempts
(`127.0.0.1`, then `[::1]`) are capped at 250ms each and at the remaining budget;
supervisor availability probes use the same bounded Unix-socket connect as
observation, also capped by the budget, and a probe cut short by the budget is
reported as a timeout rather than as lost supervision. Captured output is
scanned incrementally from a byte offset with a carry of the text length minus
one, so matches split across capture or read chunks are found with bounded
memory. Reads are at most 1 MiB per poll and 16 MiB per wait, and the budget is
rechecked before each 64 KiB chunk; an individual local file read is not
interruptible. The capture file is opened without following a symlink and
without blocking on a FIFO, and anything other than a regular file ends the wait
as `observation_failed` without touching the job.

Criteria are evaluated only while the record is `running`, and no criteria work
starts once the budget is spent. When they are met, the record and supervisor are
observed again and a requested port is probed again, so an earlier accepted
connection cannot satisfy a later log match after the listener has gone. The
budget is checked unconditionally before success: only a still-running, unexpired
job within the budget is reported ready; work that crosses the deadline reports
`timed_out` with the latest record and an unchanged lease. Starting jobs are waited on; stopping, finished, unverified or
expired jobs end the wait unready. Readiness flags are not part of the retry
fingerprint, so a keyed retry re-observes the retained job. A successful connect
cannot identify the listening process, and literal log text is not an
application health check. The run startup budget (keeper start, up to about 5
seconds) is separate from, and precedes, the wait budget.

## Resource sampling

List/watch use `sysinfo` process snapshots and read-only process-group checks.
The guardian must still have the recorded keeper as parent and lead its own
group before service usage is attributed. Group totals exclude the guardian and
keeper and skip Linux thread entries, whose usage is already in process totals;
PID/start-time changes invalidate the CPU baseline. On macOS, newly
inserted snapshot entries receive a baseline refresh before timing CPU deltas.
First-frame CPU, inaccessible counters, unverified groups and jobs whose saved
PID context cannot be matched remain unavailable.
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
- Jobs live only as long as their supervisors' host execution context. A host
  that ends every process from a tool call when that call ends, for example
  through a per-call PID namespace, ends supervision with it. ProcLease cannot
  restore that persistence without leaving the sandbox, and it does not try to.
  Such profiles need a host-documented persistent execution context.
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

Readiness regressions cover delayed TCP success, literal log
text across chunk boundaries, both criteria, timeout with an unchanged deadline,
a port that closed before the log matched, no success after an exhausted budget,
a FIFO at the capture path failing without blocking, early exit and expiry before
readiness, invalid flags launching nothing, keyed retry without duplication or
renewal, and the launch lock being free during a wait.

These are local implementation tests, not direct tests inside every Codex or
Claude permission profile. Real host compatibility and system sleep/wake remain
separate verification work. A documented-behavior
[comparison with GNU timeout, systemd-run and rentctl](comparison.md) cites
primary sources; it is not a benchmark.
Do not claim ProcLease is faster, safer or more reliable than another tool until
the same relevant scenarios have been measured on both.

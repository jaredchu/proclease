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

Only a running supervisor signals its own anchored group. CLI control never kills
a PID read from persistent state. When control is unavailable, list may derive a
terminal result for an already-stopping, now-absent group; otherwise it reports
unverified state. Such observation never signals or adopts a process.

## Consistency and retry semantics

A root advisory lock serializes launch reservations. A unique 12-hex-digit ID
names each job. Explicit retry keys match a launch-context fingerprint covering
the argument vector, canonical directory and environment, plus owner/task fields.
They return the original record, including terminal records, without renewal.
Changing a context while reusing a key is an error. There is no automatic
command-text deduplication or cross-task ownership transfer.

The guardian serializes stop/extend requests. Extension is additive and bounded
by 24 hours from creation. A request at or after expiry cannot revive the job.
Metadata is replaced atomically using private files and filesystem synchronization.
The keeper takes final write ownership after the guardian exits. Status reads
do not write lifecycle state or renew leases.

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
  Full capture is not promised after the cap. Finished-job retention is manual.
- Deadlines use wall time, including sleep. Clock changes affect expiry. No claim
  of precise execution while asleep is made.

## Verification and comparison

The lifecycle integration suite covers expiry, TERM-resistant descendants,
surviving children after leader exit, explicit stop/extension, observation without
renewal, individual keeper/guardian crashes, bounded logs, retry idempotency,
concurrent retries, private state and launch-context inheritance.

These are local implementation tests, not direct tests inside every Codex or
Claude permission profile. Real host compatibility, system sleep/wake and a
pinned-version comparison with alternatives remain separate verification work.
Do not claim ProcLease is faster, safer or more reliable than another tool until
the same relevant scenarios have been measured on both.

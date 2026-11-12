# Agent compatibility

Local observations on 2026-11-12: first with published ProcLease **0.1.7**,
then with an **unreleased local source build** that adds `run --reuse` and uses
an opt-in Next.js polling configuration. That build still reports package
version 0.1.7; `--reuse` is not in the published v0.1.7 binaries, crate or
installer. These are real model-driven CLI sessions, not scripted imitations of
an agent. Compatibility belongs to a particular client version, sandbox profile
and application; a runtime test on an OS does not establish that an agent can
launch there. Later that day, native Linux sessions exercised the same source
commit; their results are [recorded below](#native-linux-results-2026-11-12).

The implementation changes described here are included in the published v0.1.8 release.
The rows below remain measurements of the stated earlier binaries and source
builds; preparing or publishing v0.1.8 does not rerun those agent workflows.

## macOS/Linux matrix

The macOS host was **27.2.1 arm64**, using native APFS state, **Node 24.18.0**,
**Vite 8.3.6** and **Next.js 16.4.5**. Fixtures were separate application trees
with installed, locked dependencies. Clients shared one deliberately isolated
private test registry outside their application workspaces. Normal development
should instead preserve the user's existing registry selection.

### Published v0.1.7, default workflow

These historical rows remain the initial v0.1.7 results; the Linux source-build
runs below do not test the published v0.1.7 binaries. The later fixed workflow does not
reclassify them: ordinary same-key retries and default native-watcher Next.js
still behave as recorded here.

| OS / client | Execution profile | Vite | Next.js default Turbopack | Retry from a later tool call |
| --- | --- | --- | --- | --- |
| macOS / Codex CLI 0.167.0-alpha.4.1 | `workspace-write`, external registry not granted | Not run: registry-write baseline denied | Not run | Not run |
| macOS / Codex CLI 0.167.0-alpha.4.1 | `workspace-write`, explicit registry write root and network access | HTTP marker, client-cancellation survival, later-session stop and expiry passed | Port briefly ready, then FSEvent failure; HTTP marker not served | Rejected: launch-context mismatch |
| macOS / Claude Code 2.1.322 | Strict Bash sandbox, explicit registry write root, local binding and registry Unix sockets | HTTP marker, client-cancellation survival, later-session stop and expiry passed | Port briefly ready, then FSEvent failure; HTTP marker not served | Rejected: launch-context mismatch |
| Linux / Codex CLI | No authenticated native Linux client supplied | Not run | Not run | Not run |
| Linux / Claude Code | No authenticated native Linux client supplied | Not run | Not run | Not run |

### Unreleased source, explicit workflow

Same clients, host, application versions and sandbox profiles as the configured
rows above. The only additions were the documented
[Next.js polling opt-in](../README.md#nextjs-in-agent-sandboxes) and explicit
`run --reuse --key` retries; see [the fixed runs](#fixed-workflow-results).

| OS / client | Execution profile | Vite | Next.js Turbopack with opt-in polling | Retry from a later tool call |
| --- | --- | --- | --- | --- |
| macOS / Codex CLI 0.167.0-alpha.4.1 | `workspace-write`, explicit registry write root and network access | HTTP marker, client-cancellation survival, later-session stop and 60-second expiry passed | Initial and edited HTTP markers, cancellation survival, later-session stop and expiry passed; nonfatal watch diagnostics remain | `run --reuse` passed: same IDs and deadlines, no duplicates |
| macOS / Claude Code 2.1.322 | Strict Bash sandbox as above; tool allow rule for the `env WATCHPACK_POLLING=250` command prefix | Functional checks passed, with an [observer protocol deviation](#fixed-workflow-results) | Functional checks passed, as for Codex, with the same deviation | Functional `run --reuse` pass, with the same deviation |

The initial Linux container preflight had neither client installed or
authenticated and supplied no real-agent result. The owner first prioritized
the macOS fixes, then resumed native Linux testing using existing Linux CLI
logins. No macOS credentials were transferred. Linux runtime tests and native
release builds remain separate evidence; see [verification](verification.md)
and the actual client results below.

### Native Linux results, 2026-11-12

These sessions used an unreleased native GNU/Linux build of
[source commit `e51ccd2`](https://github.com/jaredchu/proclease/commit/e51ccd214baefc44ce44c4359c67af4088787149),
not a published v0.1.7 archive. The machine ran **Arch Linux x86_64**, kernel
**7.2.14-arch1-1**, with native **ext4** test state, an ordinary non-root account,
**Node 26.13.0**, **npm 12.2.2**, **Vite 8.3.6** and **Next.js 16.4.5**. A native
optimized build and all **68 Rust tests** (16 unit, 52 integration) passed.
Client and Node versions differ from the macOS runs, so this is profile coverage,
not a controlled comparison of operating systems alone. The Linux Next fixture
kept default Turbopack and native watchers; it did not use the macOS polling opt-in.

| Client | Declared profile | Observed application result | Later-call/lifecycle result |
| --- | --- | --- | --- |
| Codex CLI 0.167.2 | `workspace-write`, explicit test-state write root and network access | Vite and Next each reported TCP readiness inside their launch calls; both later HTTP checks were refused | Blocked before reuse/edit/cancellation checks. Control endpoints became unavailable; two records remain `unknown` |
| Claude Code 2.1.322 | Existing default with Bash sandbox disabled; exact command allow rules and ordinary user permissions | Vite and default Next served initial markers; a Next page edit was served | Passed explicit reuse, client-SIGINT survival, fresh-session observation/stop, new 60-second expiry and reuse of expired records without renewal or duplicates |
| Claude Code 2.1.322 | Separate strict Bash sandbox: fail if unavailable, no unsandboxed retries, explicit test-state write grant | Control-socket bind failed with `Operation not permitted (os error 1)` before Vite's command was spawned; Next not launched | Blocked. The agent also used the workspace root instead of the required Vite cwd; the deviation is retained, and this is not a matched successful application test |

Linux calls used an explicit `--state-dir` on every ProcLease operation and a
native task-local `TMPDIR`. Codex used approval mode `never`, the declared
workspace sandbox, and a state-root write grant. Default Claude inherited its
installed disabled-Bash-sandbox setting, with per-invocation `acceptEdits` and
exact local command/file rules for the fixtures. Strict Claude instead enabled
the Bash sandbox with `failIfUnavailable: true`,
`allowUnsandboxedCommands: false`, `autoAllowBashIfSandboxed: true`, and the
state directory in `filesystem.allowWrite`. It received no Unix-socket or
network-isolation exception. These were separately declared profiles, not a
fallback after a denied command.

The Codex and strict-Claude boundary probes denied a write to a known task-owned
path outside their workspaces. The default-Claude probe permitted it, consistent
with its declared lack of a Bash OS sandbox. All reported temporary directories
were under the task root. These probes establish the particular boundary
observations, not equivalent enforcement for every tool.

**Codex restriction and retained state.** The corrected-fixture run initially
saved `running` records with keeper/guardian/command IDs `3`/`4`/`6`. Subsequent
calls could neither reach the web ports nor contact the control endpoints.
`stop` refused to signal PIDs from disk. The small saved IDs are consistent with
namespace-relative identities; the exact host teardown was not traced.
[Linux PID namespaces](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
have their own PID numbers and terminate their members when their namespace
init exits. That mechanism is relevant context, not a separately reproduced
Codex teardown diagnosis. A standalone sandbox diagnostic failed argument
validation and supplied no runtime namespace evidence.

After both original deadlines, host checks found closed fixture ports and no
visible matching fixture processes. The scan encountered read-protected own-user processes; follow-up status
checks classified the entries still present as unrelated. This visibility limit
and the host scan do not establish control-protocol cleanup. Both Codex sockets refused connections, and both records remain
`unknown` with cleanup unverified. No copied PID was signalled, no lifecycle
metadata was rewritten, and this result is not an expiry or successful-stop pass.
The isolated records are retained for diagnosis.

**Strict Claude restriction.** The actual failure was binding the control
socket under the tested policy; the exact enforcement mechanism was not isolated.
It occurred before application spawn, with `command_pid: null`. The incorrect
cwd is an additional protocol deviation. Neither a corrected-cwd successful
launch nor later HTTP behavior was established in this profile. Independently,
Claude's [sandbox documentation](https://code.claude.com/docs/en/sandboxing)
says Linux commands have private localhost and the macOS `allowLocalBinding`
setting does not enable host-local connections. No setting was widened to turn
this blocked row into a pass.

**Passing default-Claude workflow.** Independent host probes corroborated the
initial and edited markers and both services remaining reachable with unchanged
deadlines after the launching CLI exited on SIGINT. A fresh CLI session then
read status/logs/content, stopped only the saved IDs, and verified closed ports.
New 60-second leases served their markers, reached `expired`, and were returned
unchanged by explicit `--reuse`; no additional job was created. The observer
used one background wait and reversed the order of the two reuse calls; it
reported no permission denial. The receipts are client reports without a full
Claude tool transcript; host state/content checks corroborate the stated endpoints.
This is a default-profile pass, not a strict-sandbox pass.

**Harness corrections.** Initial fixture copies dereferenced npm executable
symlinks, so both clients stopped on Vite `ERR_MODULE_NOT_FOUND` before the
workflow. New labelled fixtures preserved symlinks and passed launcher checks.
A further Codex invocation stopped before launching anything because the prompt
required a Read tool absent from `codex exec`; a separately labelled invocation
allowed its existing bounded shell reads. Original receipts were retained,
previous jobs were reconciled, and fresh keys identified deliberately new runs.
No sandbox permissions, proxy values or global settings were changed during
these corrections. These harness failures are not application-compatibility results.

**Follow-up probes.** Later direct probes on the same host ran minimal
canaries in both restricted profiles. In both, a canary started in one tool
call stopped advancing once that call ended. Each call showed small
call-local PIDs and its own procfs view. The Codex network namespace matched
the host; strict Claude's was private. Strict Claude failed at Unix-socket
creation, while a TCP bind succeeded. Neither canary finalized its own state
within 20 seconds. After 60 seconds, a host scan found no matching processes
and no unreadable own-user process entries. In both profiles' first probe run,
the PID-namespace link was identical in the starting and observing calls
although the canary had died, so a namespace inode alone cannot identify a PID
space. A second identity probe showed different links in Codex's two calls; in
strict Claude the link was again the same, while the start time of that
namespace's PID 1 changed. The teardown mechanism was not kernel-traced.

**Remediation status (unreleased).** Neither restricted profile can be made to
work without changing the host boundary, so both remain blocked and
unsupported. They need a host-documented persistent execution context and, for
strict Claude, permission for a Unix-domain socket. The source now fails closed
and reports why:

- `run` creates and binds a temporary control socket before reserving a job.
  When that fails, nothing launches, no record is written and the retry key
  stays unused.
- Linux keepers save an opaque [PID context](architecture.md#pid-context-unreleased).
  A later call in another or unverifiable context never probes saved PIDs to
  infer shutdown, to collect history or to attribute metrics. Its `unknown`
  error names the connect failure and the PID-context mismatch. A `stopping`
  record stays `stopping` only during a bounded finalization window.
  Terminal records whose keeper observed its group absent before its final
  write are collected normally without PID checks.
- Linux records written before this change also fail closed for those
  PID-based observations. The retained isolated test records are unchanged on
  disk.

No foreground mode, out-of-sandbox helper, network control channel, daemon or
PID translation was added, and no host policy was changed.

**Remediation checks (unreleased).** A native Linux release build of the
initial remediation source (binary SHA-256
`e4c4fab3c7ce7e4f9ad3c3e3e78b1b93cc84fb780936ff4f5f03a3c7d9e34038`) ran these
diagnostic checks, before the review corrections recorded in
[verification](verification.md#linux-sandbox-diagnostics-unreleased-source-2026-11-12).
Each used a bounded Python HTTP service, not Vite or Next.js:

| Context | Observed result |
| --- | --- |
| Host, no agent sandbox | A 2-second lease reported TCP readiness and reached `expired`; this receipt has no HTTP content or closed-port check. The PID-context token stayed the same across host calls. A keyed `--reuse` afterwards returned the same ID, deadline and `expired` state with `reused: true` |
| Codex `workspace-write` | Launch reported the service ready. The next tool call showed `unknown`, `resources: null` and an error naming the refused connection and the foreign or unverifiable PID context |
| Strict Claude Bash sandbox | Client-reported, with host-oracle corroboration: `run` failed with `Operation not permitted (os error 1)` at the control-socket create step, before any job reservation, and stated that nothing was launched. The next call listed no jobs |

Host follow-up outside both agents confirmed that the strict-Claude registry
held no job. A host launch with the same key then created a fresh job
(`reused: false`), which was stopped through ProcLease. For the Codex record,
host `stop` refused to signal disk PIDs, and `prune --dry-run` offered no
candidate, so the `unknown` record was preserved. Afterwards both test ports were
closed, no matching service process was visible, and the scan found no unreadable
own-user process entries. Control-protocol cleanup of the Codex job is **not
verified**. This run left one new `unknown` record in its own isolated registry,
in addition to the two earlier Codex records above; not every recorded job is
terminal. The table above is the initial candidate's historical result.

**Final diagnostic recheck (unreleased).** The corrected source passed all 81
tests on macOS and native Linux (see
[verification](verification.md#linux-sandbox-diagnostics-unreleased-source-2026-11-12)).
Its native Linux release build (binary SHA-256
`38e762c86350965a7dbb0fc9c271b68228a3f90be028b708e0a94fea1419835e`) repeated
the checks with the same bounded Python service and the same profiles:

| Context | Observed result |
| --- | --- |
| Host, no agent sandbox | A 2-second lease reported TCP readiness and served HTTP 200 with the literal body `proclease-boundary-ready`. It reached `expired` with `finalized_by_keeper: true`. A keyed `--reuse` returned the same ID, deadline and `expired` state with `reused: true`, without resurrection; its port wait reported `job_finished`. The port was explicitly confirmed closed after expiry |
| Codex `workspace-write` | Launch reported the service ready. The next tool call showed `unknown`, `resources: null` and an error naming the refused connection and the foreign or unverifiable PID context |
| Strict Claude Bash sandbox | Client-reported: `run` failed with `Operation not permitted (os error 1)` at control-socket creation and stated that nothing was launched and no job was recorded. The next call listed no jobs |

The host oracle again confirmed that the strict-Claude registry held no record.
A host launch with the same key created a fresh job (`reused: false`), which was
stopped through ProcLease. Host `stop` refused to signal the Codex record's disk
PIDs, and `prune --dry-run` offered no candidate, so the `unknown` record was
preserved. Both ports were closed, no matching service process was visible, and
the scan found no unreadable own-user process entries. Control-protocol cleanup
of the Codex job is **not verified**. Across the isolated test registries, four
Codex records are now `unknown`: the original two, one from the initial
diagnostics and one from this recheck. None was rewritten or deleted.

These checks confirm the fail-closed behavior. They are not a Vite/Next.js
workflow pass, and both restricted profiles remain unsupported; a
host-documented persistent execution context is a requirement, not a
demonstrated workaround. The default-Claude Vite/Next.js pass above, like the
macOS real-client Vite/Next.js checks, is from the `e51ccd2` build, before this
defensive change, and was not rerun with the remediation source.

## Observed failures and limits

**Registry permissions.** Codex's ungranted `workspace-write` baseline attempted
one short `sleep` job against the external test registry and returned
`Operation not permitted (os error 1)` without a job ID. It did not retry with
wider permissions. The separately labelled configured profile granted that
registry before starting the client. This is a profile restriction, not a
ProcLease runtime pass or failure after launch.

**Next.js watcher.** Both configured sandbox profiles initially reported TCP
readiness, then Next.js exited with code 1 and
`TurbopackInternalError: unable to start FSEvent stream`. Neither served the
expected HTTP marker. A subsequent controller launch outside either agent's
sandbox, using the same Codex-side Next fixture and default Turbopack command,
served its marker and was explicitly stopped. This narrows the failure to the
observed sandboxed contexts; it does not identify the precise denied operation
or prove every unrestricted agent profile works. No sandbox relaxation or
alternative bundler was used to turn these failed rows into passes. TCP
readiness alone was insufficient to establish application readiness.

**Retry context.** Both clients launched Vite successfully but a later exact
argv/key retry returned `retry key already belongs to a different launch context`.
The mismatch also occurred in separate Vite-only cases that kept the initial
working directory throughout. No replacement job was created and observed
deadlines remained unchanged. ProcLease includes inherited environment in
launch identity, excluding `_` and `SHLVL`; identical command text and cwd alone
do not establish identical context. The changing input was not isolated, so
these are failed cross-tool-call retry checks, not evidence of a broken hash or
a confirmed particular environment-variable cause. See
[launch identity](../README.md#lease-rules).

**Retry follow-up diagnosis, 2026-11-12.** Later probes compared the inherited
environment of two separate exact Bash tool calls from an unchanged working
directory. They saved variable names and salted digests only, never values.
For Codex, the only difference was `CODEX_TOOL_CALL_ID`. In Claude's strict
Bash sandbox, 14 proxy-related values differed: `ALL_PROXY`,
`CLOUDSDK_PROXY_USERNAME`, `DOCKER_HTTPS_PROXY`, `DOCKER_HTTP_PROXY`,
`FTP_PROXY`, `GRPC_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`,
`SRT_SSH_PROXY_COMMAND` and lower-case proxy variants. These per-call changes
are sufficient to trigger the same rejection. The original failed calls did
not capture environment differences, so their exact changing inputs were not
retrospectively verified. Ordinary keyed retries still treat them as a
different context: the environment fingerprint stays strict, including these
variables, and ProcLease does not strip, ignore or normalize proxy variables
or disable the sandbox. Unreleased source instead adds an explicit,
non-launching `run --reuse --key` retry that keeps the retained job's original
environment (see [lease rules](../README.md#lease-rules)). A deliberately
changed application environment is a genuinely new request, not permission to
reuse the old configuration. The fixed workflow passed in both real clients;
see [below](#fixed-workflow-results). The v0.1.7 rows remain historical,
expected restrictions of ordinary retries.

**Next.js polling probes.** Next.js 16.4.5 passes a top-level
`watchOptions.pollIntervalMs` from `next.config` to Turbopack's watcher
([source](https://github.com/vercel/next.js/blob/v16.4.5/packages/next/src/server/dev/hot-reloader-turbopack.ts)),
while its Watchpack layer reads `WATCHPACK_POLLING`
([source](https://github.com/webpack/watchpack/blob/v2.4.4/lib/DirectoryWatcher.js)).
Probes kept default Turbopack and the unchanged sandbox profiles, and their
records are retained:

- Turbopack-only polling removed the initial FSEvent error but then hit a
  Watchpack `EMFILE` error, and no HTTP marker was served. Polling one watcher
  layer was insufficient in that case.
- The next probe served the initial HTTP marker in both clients but was not a
  matched pass. Claude's compound `curl` loop was denied by the permission
  matcher before the edited marker was verified. In Codex, both markers
  appeared, but `WATCHPACK_POLLING` requested through per-invocation
  configuration was absent from the inherited environment, so it did not test
  combined polling.
- The final runs set `WATCHPACK_POLLING=250` with an explicit `env` command
  prefix, used individual `curl` calls and observed both markers. See
  [fixed results](#fixed-workflow-results).

Polling is an application opt-in, not a ProcLease change. It does not repair
native FSEvents in these sandboxes, and the default-Turbopack failures above
remain the v0.1.7 result.

One additional Claude Vite-only case appended `echo` to its launch command.
The permission matcher denied that compound command before ProcLease ran.
A separately labelled exact-command case, under unchanged permissions, launched
successfully and then encountered the retry mismatch above. The denied attempt
is not counted as a ProcLease launch failure or a successful retry.

## Lifecycle observations

To keep the retry failures visible, lifecycle checks used deliberately new,
separately labelled Vite runs after the previous fixture jobs were reconciled
and stopped. They did not replace a failed retry with a new key.

For each configured client, the launching session read the skill, launched
Vite with a five-minute lease, checked the HTTP marker, and read status and
bounded logs. The controller then sent SIGINT to that test client's own CLI
process after its durable launch receipt. Both CLIs exited without a forced
termination; both Vite services remained reachable and running with unchanged
deadlines. This tests cancellation of the foreground agent client, not keeper
or guardian failure, reboot, sleep/wake, or every host's process reaping policy.

Fresh sessions of the respective CLIs then found the saved jobs running,
served the markers, read logs, and stopped only the saved fixture IDs. Both
ports closed. Each session launched a deliberately new 45-second Vite lease,
verified its marker, then observed `expired` with an unchanged deadline and a
closed port. Codex also used literal-log readiness on its expiry run. Controller
readback confirmed all fixture jobs were terminal and the used ports closed.
These v0.1.7 lifecycle passes do not include successful cross-tool-call retry.
Next.js checks did not reach the later-session lifecycle stages because the
application failed.

Claude's observer reported that its harness rejected a standalone foreground
`sleep 30`; it used a tool-supported background wait and status polls instead.
The wait adaptation did not relaunch a service or widen the sandbox. This
harness detail should not be generalized to other client versions.

## Fixed workflow results

The unreleased source build passed formatting, Clippy with warnings denied and
all 68 tests (16 unit, 52 integration) before these runs; its runtime source was
not changed during them. These runs used commit `e51ccd2`, before the later
[Linux remediation](#native-linux-results-2026-11-12), and were not rerun with
that remediation source. Each configured client ran the same protocol:

- Launch Vite and the polling-configured Next.js fixture with initial keys,
  confirm both HTTP markers, then retry each from a separate tool call with
  `run --reuse` and the same key, argv, directory and command prefix. Every
  retry returned `reused: true` with the original ID and deadline; no
  duplicate job was created.
- Edit the Next.js page and confirm the edited `liveedit` marker over HTTP.
- After a durable launch receipt, the controller sent SIGINT to the client's
  own CLI process. Both CLIs exited without forced termination; all four
  services kept running and serving their markers with unchanged deadlines.
- A fresh session found the saved jobs running, read status, bounded logs and
  HTTP, stopped only the saved IDs and confirmed the ports closed.
- New, deliberately separate 60-second leases for both applications passed
  readiness and HTTP checks, then reached `expired` at their original
  deadlines. A subsequent `run --reuse` of each expired key returned the
  `expired` record with the same deadline, with no renewal, resurrection or new
  job, and the ports stayed closed.

Client receipts report the command, log and reuse results; the Codex receipt
includes bounded completed-command records, while Claude's has no full tool
transcript. Independent controller checks outside both clients corroborate the
initial and edited HTTP markers, running services at their original deadlines
after client cancellation, the stopped and expired transitions, all eight fixed
jobs terminal and every used port closed.

With polling, both clients' Next.js logs still contained three nonfatal
`NotFound` watch diagnostics for missing optional fixture paths. The initial and
edited content and the lifecycle passed regardless. This is not evidence of
clean logs or of compatibility with every Next.js application or version.

**Claude observer protocol deviation.** The observer session added unnecessary
inline `PROCLEASE_HOME` assignments to three preflight commands. The command
matcher denied them before execution. Contrary to the harness instruction to
stop after an unexpected denial, it then continued with the task's original
permitted command forms. Those later checks did not change tool grants,
registry selection or sandbox policy, and the controller checks corroborate
their lifecycle and content outcomes. The Claude rows therefore record
functional passes, not a clean matched-protocol pass. The launching session had
already verified `run --help` and `--reuse`. The Codex observer used its default
shell for its first four file reads and an explicit `/bin/bash` afterwards.

## Reproduce the profile and protocol

Use disposable projects and a short, private native-filesystem state path.
An isolated registry will not appear in the user's ordinary watch. Set the same
`PROCLEASE_HOME` for every launch, observation and control in the experiment;
use `proclease --state-dir /same/test/state watch` to view it explicitly. Do not
move live state or control unrelated jobs. Allocate unused loopback ports and
check actual HTTP content as well as ProcLease readiness.

The configured macOS Codex profile used `exec --sandbox workspace-write`, approval mode
`never`, an explicit `--add-dir` for the external test registry, and the
per-invocation settings below. Supply the same values to each fresh session:

```text
shell_environment_policy.set.PROCLEASE_HOME="/absolute/test/state"
shell_environment_policy.set.NEXT_TELEMETRY_DISABLED="1"
sandbox_workspace_write.network_access=true
```

Network access in this profile is broad; it is not a localhost-only network
policy. No unrestricted execution profile or approval bypass was used. Consult
the official [CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
and [advanced configuration](https://learn.chatgpt.com/docs/config-file/config-advanced)
for the client version in use.

For the configured macOS runs, Claude used `--print`, local `Bash,Read,Write`
tools, no permission-prompt surface,
and a per-invocation `--settings` file with this sandbox configuration:

```json
{
  "sandbox": {
    "enabled": true,
    "failIfUnavailable": true,
    "allowUnsandboxedCommands": false,
    "autoAllowBashIfSandboxed": true,
    "filesystem": {"allowWrite": ["/absolute/test/state"]},
    "network": {
      "allowLocalBinding": true,
      "allowUnixSockets": ["/absolute/test/state"]
    }
  },
  "permissions": {"defaultMode": "acceptEdits"}
}
```

Those macOS tool allow rules covered the installed ProcLease executable,
`curl`, `sleep`,
and receipt writes in the fixture workspace. For the fixed workflow they also
permitted the exact command prefix
`env WATCHPACK_POLLING=250 /absolute/path/proclease *`. That is a client
tool-permission rule for an application setting, not a new OS sandbox
permission. Absolute Edit patterns used `//absolute/path/**`. The macOS local-binding grant is not a per-port
restriction. These are explicit experimental grants, not permission added by
the ProcLease skill. The Bash sandbox does not establish that file tools have
identical OS sandbox enforcement. See Claude's official
[sandboxing](https://code.claude.com/docs/en/sandboxing) and
[permission rules](https://code.claude.com/docs/en/permissions).

For each framework, use an initial request key and retain the returned ID and
`expires_at_ms`. A representative Vite command from its application directory:

```sh
proclease --json run --owner compatibility --task vite-handoff \
  --key vite-handoff-1 --ttl 5m --grace 100ms \
  --wait-for-port 5173 --wait-timeout 20s -- \
  npm run dev -- --port 5173 --strictPort
```

The Vite script binds to `127.0.0.1`. The Next fixture used
`next dev --hostname 127.0.0.1`, with `--port PORT` passed through npm.
Read status/logs in later tool calls and retry the original key, cwd, argv and
environment. Record a context mismatch without switching keys or modifying the
profile. In a separate lifecycle case, cancel the launching client after
readiness, inspect from a fresh session, stop only the saved fixture ID, then
run a new 45-second lease and observe expiry and closed ports. Reads must never
change the saved deadline. Reconcile and clean up every fixture before finishing.

The macOS fixed workflow applied the [Next.js polling opt-in](../README.md#nextjs-in-agent-sandboxes)
before the first launch and repeated each launch command with `--reuse` added
for the later retry. It confirmed an edited page over HTTP, combined client
cancellation and later-session stop for Vite and Next.js, and used 60-second
expiry leases followed by `--reuse` of the expired keys. Check each HTTP marker
with a single `curl` call rather than a compound loop.

# Agent compatibility

Local observations on 2026-11-12: first with published ProcLease **0.1.7**,
then with an **unreleased local source build** that adds `run --reuse` and uses
an opt-in Next.js polling configuration. That build still reports package
version 0.1.7; `--reuse` is not in the published v0.1.7 binaries, crate or
installer. These are real model-driven CLI sessions, not scripted imitations of
an agent. Compatibility belongs to a particular client version, sandbox profile
and application; a runtime test on an OS does not establish that an agent can
launch there.

## macOS/Linux matrix

The macOS host was **27.2.1 arm64**, using native APFS state, **Node 24.18.0**,
**Vite 8.3.6** and **Next.js 16.4.5**. Fixtures were separate application trees
with installed, locked dependencies. Clients shared one deliberately isolated
private test registry outside their application workspaces. Normal development
should instead preserve the user's existing registry selection.

### Published v0.1.7, default workflow

These rows remain the v0.1.7 results. The later fixed workflow does not
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
| Linux / Codex CLI | Deferred | Not run | Not run | Not run |
| Linux / Claude Code | Deferred | Not run | Not run | Not run |

Linux ProcLease runtime tests and native release builds are separate evidence;
see [verification](verification.md). The available Linux container preflight
had neither agent installed or authenticated, so it supplied no real-agent
result. No macOS login material was transferred into it. Linux agent rows remain
open, including native filesystem, sandbox and later-session control checks.
The owner prioritized the macOS fixes; Linux agent testing is deferred and was
not a prerequisite for them.

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
not changed afterwards. Each configured client ran the same protocol:

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

The tested Codex profile used `exec --sandbox workspace-write`, approval mode
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

Claude used `--print`, local `Bash,Read,Write` tools, no permission-prompt surface,
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

Tool allow rules covered the installed ProcLease executable, `curl`, `sleep`,
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

The fixed workflow applied the [Next.js polling opt-in](../README.md#nextjs-in-agent-sandboxes)
before the first launch and repeated each launch command with `--reuse` added
for the later retry. It confirmed an edited page over HTTP, combined client
cancellation and later-session stop for Vite and Next.js, and used 60-second
expiry leases followed by `--reuse` of the expired keys. Check each HTTP marker
with a single `curl` call rather than a compound loop.

# Comparison with related tools

Retrieved 2026-11-08. This page compares documented behavior, not measured
performance, reliability or security. Each alternative is a capable tool with a
different scope; check the linked primary sources for the version you run.

## Sources

- **GNU `timeout`**: [manual](https://www.gnu.org/software/coreutils/manual/html_node/timeout-invocation.html)
  (labeled GNU Coreutils 9.12 at retrieval) and
  [`src/timeout.c` at tag `v9.12`](https://github.com/coreutils/coreutils/blob/v9.12/src/timeout.c).
- **`systemd-run`**: [systemd-run(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html),
  [systemd.scope(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.scope.html),
  [systemd.service(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) and
  [systemd.kill(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.kill.html)
  (labeled systemd 262 at retrieval).
- **rentctl**: [README on `main`](https://raw.githubusercontent.com/Michael-Drake/rentctl/main/README.md),
  which was commit
  [`a64f6a1`](https://github.com/Michael-Drake/rentctl/tree/a64f6a18733b9b678ffe91c3363a640f5417adb8)
  at retrieval. The rendered GitHub page can lag the raw file.
- **ProcLease**: this repository's [README](../README.md) and
  [architecture](architecture.md). The matrix covers released v0.1.5 behavior
  only; unreleased features, such as the source-only readiness wait, are excluded
  until they ship.

## Matrix

| | GNU `timeout` | `systemd-run` | rentctl | ProcLease |
|---|---|---|---|---|
| **Expiry and escalation** | Fixed duration at invocation. Sends TERM by default (`-s` selects another signal); `-k` adds KILL after a further interval. | `RuntimeMaxSec=` property on transient services, and on scopes since systemd 244. Stop sends `KillSignal=` (TERM by default), then KILL to leftover processes after `TimeoutStopSec=`. | Default lease 120 minutes, capped at 480. Stop sends TERM to the environment's session, then KILL to survivors after 10 seconds. | Default 2h, at most 24h from creation. TERM to the process group, then KILL after a grace of 2s (up to 10s). |
| **Explicit renewal** | None; the duration is fixed. | Not described as a lease workflow in the cited pages. | `rent up` on a live lease renews it. Each session holds its own expiring claim; the lease lasts as long as the latest claim. | `extend --by` adds time to the current deadline. Status, logs, watch and keyed retries never renew. |
| **Lifecycle failure handling** | `timeout` is the only supervisor and waits for its immediate child. When that child exits, `timeout` exits and sends no further signals, including the `-k` KILL, so group members that outlive the child are not handled. | Units are owned by the system or user service manager. Services support restart policies (`Restart=`). | No global daemon; one supervisor per environment. If a supervisor is killed, the server keeps running; the next `ls`, `sweep`, `up` or `down` marks it unsupervised, and it is stopped at expiry by such a command or by `down`. | No global daemon; two supervisors per job. Losing either triggers fail-closed group cleanup. No restart or resumption. An unreachable supervisor is reported as `unknown`; a PID copied from disk is never signalled. |
| **Labels and listing** | None beyond the exit status (124 on timeout). | Unit names (`--unit`), descriptions and properties; transient units appear in `systemctl list-units`. | `rent ls` lists environments and states; `rent events --summary` reports cleanup actions. Also exposes an MCP server. | Declared `--owner`, `--task`, `--name`, `--url` labels; `list`, `watch` and JSON across sessions sharing a state directory; filtered `stop --all`. Labels are declarations, not authentication. |
| **Containment** | Runs the command in its own background process group unless `--foreground`, where children are not timed out. | The unit's control group; the default `KillMode=control-group` signals every remaining process in it. | The POSIX session the supervisor creates; on Linux the supervisor is a child subreaper. Work handed to other managers, containers or PID namespaces can escape. | The job's process group. A descendant that creates a new session or group can escape cleanup. Not a sandbox. |
| **Platform and execution context** | Any system with GNU coreutils. Runs in the caller's foreground context. | Linux with systemd. Services run in a clean, detached environment from the service manager; scopes run under `systemd-run` itself. Needs access to a system or user manager. | macOS and Linux with Python 3.12 or newer. | macOS and Linux. Per-job supervisors inherit the caller's directory, environment and host restrictions. |

## Choosing

- **GNU `timeout`** is the simplest choice for a single foreground command in a
  script or CI step whose duration is known in advance.
- **`systemd-run`** suits Linux hosts that want cgroup containment, resource
  controls and restart policies managed by systemd.
- **rentctl** suits agent workflows that want MCP tools, session-claim renewal and,
  on Linux, subreaper-based containment.
- **ProcLease** suits agent-started development servers that need an explicit
  lifetime which holds after the conversation ends: each job carries declared
  owner and task labels, appears in one cross-session list, lives longer only on an
  explicit `extend`, and is supervised independently of model turns, with the
  limits above.

No shared benchmark has been run across these tools. Do not infer that one is
faster, safer or more reliable from this table.

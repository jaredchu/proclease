#!/usr/bin/env python3
"""Black-box lifecycle experiments for published ProcLease v0.1.9 executables.

Python standard library only. The harness drives a real `proclease` binary as an
external program; it does not build, import or reimplement ProcLease. Methods,
pass criteria and tolerances are documented in docs/experiments/v0.1.9/protocol.md
(protocol version 2) and are fixed by the PROTOCOL constant below, whose hash
every result records. Version 2 amends the observer of version 1
(docs/experiments/v0.1.9/protocol-v1.md): ps failures are observer errors, a
live PID without the trial token is identity_unknown rather than stopped, and
the blocked-output interrupt aborts unless its preconditions hold.

Subcommands:
  selfcheck   observer, negative-control and sanitizer checks without ProcLease
  run         run trials against one binary (smoke or counted)
  validate    privacy and arithmetic checks of public result files
  table       print a Markdown summary table from public summaries
  protocol    print the protocol and harness hashes

Internal subcommands (`fixture`, `gate-exec`) are invoked by the harness itself.
Every trial uses its own private state directory, TMPDIR, HOME and fixture files
under --root. Nothing uses a normal ProcLease registry. Private diagnostics
(paths, PIDs, raw CLI output) stay under --root; public files contain only
scenario/repetition identifiers, booleans, counts, timings, enum states, reasons
and generic OS metadata.
"""
import argparse
import concurrent.futures
import datetime
import fcntl
import hashlib
import http.client
import json
import os
import platform
import re
import secrets
import signal
import socket
import statistics
import subprocess
import sys
import termios
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer

HARNESS = os.path.realpath(__file__)

PROTOCOL = {
    "name": "proclease-v0.1.9-lifecycle-experiments",
    "protocol_version": 2,
    "release": "v0.1.9",
    "observer_policy": {
        "ps_failure": "observer_error_outcome_error",
        "ps_scan_requires": "rc0_no_stderr_all_lines_parsed_self_present",
        "ps_info_no_match": "rc1_empty_stdout_empty_stderr",
        "missing_token_live_pid": "identity_unknown_keep_polling",
        "cleanup_requires": "absent_or_zombie_both_and_port_refused",
        "sigint_preconditions": "single_running_job_full_pipe_live_launcher_identity",
    },
    "expected_version_output": "proclease 0.1.9",
    "ttl_s": 3,
    "grace_s": 1,
    "long_ttl_s": 30,
    "interrupt_ttl_s": 120,
    "poll_interval_s": 0.05,
    "early_tolerance_s": 0.25,
    "late_tolerance_s": 2.0,
    "supervisor_kill_cleanup_bound_s": 3.5,
    "signal_exit_bound_s": 6.5,
    "post_event_observation_cap_s": 10.0,
    "negative_control_window_s": 2.0,
    "ready_timeout_long_s": 8.0,
    "concurrent_callers": 8,
    "repetitions": 30,
    "max_parallel_trials": 3,
    "cli_call_timeout_s": 20,
    "immediate_return_bound_s": 10,
    "fixture_alarm_s": 180,
    "scenarios": [
        "detached_expiry_graceful",
        "detached_expiry_term_resistant",
        "foreground_expiry_graceful",
        "foreground_expiry_term_resistant",
        "detached_keeper_sigkill",
        "detached_guardian_sigkill",
        "foreground_sigint_blocked_stdout",
        "retry_sequential_detached",
        "retry_sequential_foreground",
        "retry_concurrent8_detached",
        "retry_concurrent8_foreground",
        "reuse_changed_env_detached",
        "reuse_changed_env_foreground",
        "mismatch_command_detached",
        "mismatch_command_foreground",
        "terminal_retry_detached",
        "terminal_retry_foreground",
        "negative_control_unmanaged_live",
        "negative_control_blind_observer",
    ],
}
OUTCOMES = ["pass", "fail", "not_ready", "unobserved", "timeout", "error"]


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def protocol_hash():
    return sha256_bytes(canonical(PROTOCOL))


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# --------------------------------------------------------------------------
# Fixture service and caller gate (internal subcommands)
# --------------------------------------------------------------------------

def cmd_fixture(args):
    """Parent plus same-group forked child. The child serves HTTP on a loopback
    port inherited from the parent; both keep the listening socket open, so the
    port closes only after both have exited. `resistant` ignores SIGTERM."""
    signal.alarm(args.alarm)
    if args.mode == "resistant":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    body = json.dumps({"token": args.token,
                       "marker": os.environ.get("PL_EXPERIMENT_MARKER", "")}).encode()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    child = os.fork()
    if child == 0:
        signal.alarm(args.alarm)  # alarms are not inherited across fork
        try:
            server.serve_forever(poll_interval=0.05)
        finally:
            os._exit(0)
    with open(args.starts, "a", encoding="utf-8") as handle:
        handle.write(json.dumps({"pid": os.getpid()}) + "\n")
    tmp = args.ready + ".tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump({"pid": os.getpid(), "child": child, "port": port,
                   "pgid": os.getpgrp(), "token": args.token}, handle)
    os.replace(tmp, args.ready)
    print("fixture-ready", flush=True)
    while True:
        time.sleep(0.2)


def cmd_gate_exec(args):
    """Wait for a gate file, then exec the given argv in this same process."""
    with open(args.marker, "w", encoding="utf-8") as handle:
        handle.write("waiting\n")
    deadline = time.monotonic() + 30
    while not os.path.exists(args.gate):
        if time.monotonic() > deadline:
            sys.exit(97)
        time.sleep(0.001)
    os.execv(args.argv[0], args.argv)


# --------------------------------------------------------------------------
# Independent observation (ps, loopback HTTP/TCP, monotonic time)
# --------------------------------------------------------------------------

PS_FIELDS = "pid=,ppid=,pgid=,stat=,command="


class ObserverError(Exception):
    """The independent observer could not see the process table reliably.
    Details stay in private diagnostics; public records only say observer_error."""

    def __init__(self, what, details):
        super().__init__(what)
        self.what = what
        self.details = details


def _run_ps(argv):
    """Indirection so selfcheck can simulate denied or failed enumeration."""
    return subprocess.run(argv, capture_output=True, text=True, timeout=10)


PS_RUNNER = [_run_ps]


def _ps(argv):
    try:
        return PS_RUNNER[0](argv)
    except (OSError, subprocess.SubprocessError) as error:
        raise ObserverError("ps_spawn_failed", {"argv": argv, "error": repr(error)})


def _parse_ps(line):
    parts = line.strip().split(None, 4)
    if len(parts) < 4:
        return None
    try:
        pid, ppid, pgid = int(parts[0]), int(parts[1]), int(parts[2])
    except ValueError:
        return None
    return {"pid": pid, "ppid": ppid, "pgid": pgid,
            "stat": parts[3], "command": parts[4] if len(parts) > 4 else ""}


def ps_info(pid):
    """The process entry for pid, None when ps reports no such process, or
    ObserverError. Only rc=1 with empty stdout and stderr means no match; any
    other failure, stderr or unparseable output is an observer error."""
    if not isinstance(pid, int) or pid <= 0:
        raise ObserverError("ps_info_bad_pid", {"pid": pid})
    argv = ["ps", "-ww", "-o", PS_FIELDS, "-p", str(pid)]
    result = _ps(argv)
    stdout, stderr = result.stdout or "", result.stderr or ""
    details = {"argv": argv, "rc": result.returncode, "stdout": stdout[-2000:], "stderr": stderr[-2000:]}
    if result.returncode == 1 and not stdout.strip() and not stderr.strip():
        return None
    if result.returncode != 0 or stderr.strip():
        raise ObserverError("ps_info_failed", details)
    lines = [line for line in stdout.splitlines() if line.strip()]
    parsed = [_parse_ps(line) for line in lines]
    if len(lines) != 1 or parsed[0] is None or parsed[0]["pid"] != pid:
        raise ObserverError("ps_info_unparsed", details)
    return parsed[0]


def ps_scan(needle):
    """Live entries whose command contains needle. Requires rc=0, no stderr,
    every line parseable, and this harness process present in the table, so a
    failed or empty enumeration can never read as 'nothing left running'."""
    argv = ["ps", "-ww", "-A", "-o", PS_FIELDS]
    result = _ps(argv)
    stdout, stderr = result.stdout or "", result.stderr or ""
    details = {"argv": argv, "rc": result.returncode, "stdout": stdout[-2000:], "stderr": stderr[-2000:]}
    if result.returncode != 0 or stderr.strip():
        raise ObserverError("ps_scan_failed", details)
    entries = []
    for line in stdout.splitlines():
        if not line.strip():
            continue
        info = _parse_ps(line)
        if info is None:
            raise ObserverError("ps_scan_unparsed", details)
        entries.append(info)
    if not any(info["pid"] == os.getpid() for info in entries):
        raise ObserverError("ps_scan_self_missing", details)
    found = []
    for info in entries:
        if needle in info["command"] and info["pid"] != os.getpid():
            if " -ww -A -o " in info["command"]:
                continue
            found.append(info)
    return found


GONE_CLASSES = ("absent", "zombie")


def classify(pid, token):
    """executing | zombie (exited, not executing) | absent | identity_unknown.

    identity_unknown: the PID exists, is not a zombie, and its visible command
    lacks the trial token (for example an exit transition with an unreadable
    argument vector, or PID reuse). It is never treated as proof that the
    original process stopped; the observer keeps polling for absent/zombie."""
    info = ps_info(pid)
    if info is None:
        return "absent", None
    if info["stat"].startswith("Z"):
        return "zombie", info
    if token not in info["command"]:
        return "identity_unknown", info
    return "executing", info


def port_state(port, timeout=0.3):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout):
            return "open"
    except ConnectionRefusedError:
        return "closed"
    except (socket.timeout, TimeoutError):
        return "timeout"
    except OSError:
        return "error"


def http_body(port, timeout=1.0):
    try:
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
        conn.request("GET", "/")
        response = conn.getresponse()
        body = response.read(4096).decode("utf-8", "replace")
        conn.close()
        return body if response.status == 200 else None
    except OSError:
        return None
    except http.client.HTTPException:
        return None


def pre_observe(info, token):
    """Positive pre-observation: both identities executing with the trial token,
    the child in the parent's process group, and the HTTP marker served."""
    out = {"parent": False, "child": False, "same_group": False, "http_marker": False}
    parent = ps_info(info.get("pid"))
    child = ps_info(info.get("child"))
    if parent and not parent["stat"].startswith("Z") and token in parent["command"]:
        out["parent"] = True
    if child and not child["stat"].startswith("Z") and token in child["command"]:
        out["child"] = True
    if out["parent"] and out["child"] and parent["pgid"] == child["pgid"]:
        out["same_group"] = True
    body = http_body(info.get("port", 0)) if info.get("port") else None
    out["http_marker"] = bool(body and token in body)
    out["ok"] = all(out.values())
    out["pgid"] = parent["pgid"] if parent else None
    return out


def observe_cleanup(info, token, until, poll):
    """Poll until both identities are absent or zombie and the port refuses, or
    until. identity_unknown samples are counted but never end the polling for a
    role; an ObserverError propagates and can never become a confirmation."""
    state = {"parent_gone_at": None, "child_gone_at": None, "port_closed_at": None,
             "parent_class": "executing", "child_class": "executing", "port_last": None,
             "parent_unknown_samples": 0, "child_unknown_samples": 0}
    while True:
        now = time.monotonic()
        for role, pid in (("parent", info.get("pid")), ("child", info.get("child"))):
            if state[role + "_gone_at"] is None:
                cls, _ = classify(pid, token)
                state[role + "_class"] = cls
                if cls == "identity_unknown":
                    state[role + "_unknown_samples"] += 1
                elif cls in GONE_CLASSES:
                    state[role + "_gone_at"] = now
        if state["port_closed_at"] is None:
            state["port_last"] = port_state(info["port"]) if info.get("port") else "unknown"
            if state["port_last"] == "closed":
                state["port_closed_at"] = now
        if all(state[k] is not None for k in ("parent_gone_at", "child_gone_at", "port_closed_at")):
            break
        if now >= until:
            break
        time.sleep(poll)
    return state


def cleanup_verdict(pre_ok, obs):
    if not pre_ok:
        return "unobserved"
    gone = all(obs[k] is not None for k in ("parent_gone_at", "child_gone_at", "port_closed_at"))
    proven = all(obs.get(role + "_class") in GONE_CLASSES for role in ("parent", "child"))
    if gone and proven:
        return "confirmed"
    return "not_confirmed"


def live_supervisors(state_dir):
    return [p for p in ps_scan(state_dir)
            if ("__keeper" in p["command"] or "__guard" in p["command"])
            and not p["stat"].startswith("Z")]


def live_token_processes(token):
    return [p for p in ps_scan(token) if not p["stat"].startswith("Z")
            and "gate-exec" not in p["command"]]


def r3(value):
    return None if value is None else round(value, 3)


# --------------------------------------------------------------------------
# Trials
# --------------------------------------------------------------------------

class TrialTimeout(Exception):
    pass


class TrialAbort(Exception):
    def __init__(self, outcome, reason):
        super().__init__(reason)
        self.outcome = outcome
        self.reason = reason


class Trial:
    def __init__(self, ctx, scenario, rep, index):
        self.ctx = ctx
        self.scenario = scenario
        self.rep = rep
        self.dir = os.path.join(ctx["run_dir"], "%03d" % index)
        self.state = os.path.join(self.dir, "s")
        self.tmp = os.path.join(self.dir, "t")
        self.files = os.path.join(self.dir, "f")
        for path in (self.dir, self.tmp, self.files):
            os.makedirs(path, mode=0o700)
        self.token = "plx" + secrets.token_hex(8)
        self.ready = os.path.join(self.files, "ready.json")
        self.starts = os.path.join(self.files, "starts.jsonl")
        self.env = {"PATH": "/usr/bin:/bin", "HOME": self.dir, "TMPDIR": self.tmp,
                    "LANG": "C", "LC_ALL": "C",
                    "PL_EXPERIMENT_MARKER": "original-" + self.token}
        self.grace = PROTOCOL["grace_s"]
        self.poll = PROTOCOL["poll_interval_s"]
        self.metrics = {}
        self.reasons = []
        self.private = {"calls": [], "token": self.token, "dir": self.dir}
        self.job_ids = set()
        self.sessions = []
        self.seq = 0

    # ---- CLI helpers ----
    def argv(self, args):
        return [self.ctx["binary"], "--state-dir", self.state, "--json"] + list(args)

    def cli(self, args, env=None, timeout=None):
        timeout = timeout or PROTOCOL["cli_call_timeout_s"]
        started = time.monotonic()
        try:
            result = subprocess.run(self.argv(args), cwd=self.dir, env=env or self.env,
                                    stdin=subprocess.DEVNULL, capture_output=True,
                                    timeout=timeout)
        except subprocess.TimeoutExpired:
            self.private["calls"].append({"args": args, "timeout": True})
            raise TrialTimeout("cli")
        elapsed = time.monotonic() - started
        text = result.stdout.decode("utf-8", "replace")
        try:
            obj = json.loads(text) if text.strip() else None
        except ValueError:
            obj = None
        self.private["calls"].append({"args": args, "rc": result.returncode,
                                      "stdout": text[-4000:],
                                      "stderr": result.stderr.decode("utf-8", "replace")[-2000:],
                                      "elapsed": elapsed})
        if obj and isinstance(obj.get("job"), dict) and obj["job"].get("id"):
            self.job_ids.add(obj["job"]["id"])
        return result.returncode, obj, elapsed

    def fixture_argv(self, mode, variant=False):
        argv = [sys.executable, "-I", HARNESS, "fixture", mode, self.token, self.ready,
                self.starts, "--alarm", str(PROTOCOL["fixture_alarm_s"])]
        if variant:
            argv += ["--variant", "b"]
        return argv

    def run_args(self, foreground, ttl, mode, key=None, variant=False, reuse=False):
        args = ["run"]
        if foreground:
            args.append("--foreground")
        if reuse:
            args.append("--reuse")
        args += ["--ttl", "%ds" % ttl, "--grace", "%ds" % self.grace,
                 "--owner", "experiment", "--task", "x-" + self.scenario[:24]]
        if key:
            args += ["--key", key]
        return args + ["--"] + self.fixture_argv(mode, variant)

    def start_session(self, args, env=None, stdout=None):
        """Start a foreground invocation owned by this trial."""
        self.seq += 1
        out_path = os.path.join(self.files, "fg-%d.out" % self.seq)
        err_path = os.path.join(self.files, "fg-%d.err" % self.seq)
        out = stdout if stdout is not None else open(out_path, "wb")
        err = open(err_path, "wb")
        proc = subprocess.Popen(self.argv(args), cwd=self.dir, env=env or self.env,
                                stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                start_new_session=True)
        if stdout is None:
            out.close()
        err.close()
        session = {"proc": proc, "out": out_path, "err": err_path}
        self.sessions.append(session)
        return session

    def read_lines(self, path):
        try:
            with open(path, "rb") as handle:
                data = handle.read().decode("utf-8", "replace")
        except OSError:
            return []
        return [line for line in data.split("\n") if line.strip()]

    def session_startup(self, session, timeout=None):
        deadline = time.monotonic() + (timeout or PROTOCOL["cli_call_timeout_s"])
        while time.monotonic() < deadline:
            lines = self.read_lines(session["out"])
            if lines:
                try:
                    obj = json.loads(lines[0])
                except ValueError:
                    obj = None
                if obj and isinstance(obj.get("job"), dict):
                    self.job_ids.add(obj["job"]["id"])
                return obj
            if session["proc"].poll() is not None:
                lines = self.read_lines(session["out"])
                return json.loads(lines[0]) if lines else None
            time.sleep(0.02)
        raise TrialTimeout("startup")

    def wait_exit(self, proc, until):
        while time.monotonic() < until:
            rc = proc.poll()
            if rc is not None:
                return rc, time.monotonic()
            time.sleep(0.02)
        rc = proc.poll()
        return rc, (time.monotonic() if rc is not None else None)

    def job(self, job_id):
        rc, obj, _ = self.cli(["list", "--all"])
        if rc != 0 or not obj:
            return None
        for job in obj.get("jobs", []):
            if job.get("id") == job_id:
                return job
        return None

    def job_count(self):
        rc, obj, _ = self.cli(["list", "--all"])
        return len(obj.get("jobs", [])) if rc == 0 and obj else None

    def wait_state(self, job_id, accepted, until):
        job = None
        while time.monotonic() < until:
            job = self.job(job_id)
            if job and job.get("state") in accepted:
                return job
            time.sleep(0.1)
        return job

    def wait_finalized(self, job_id, until):
        """Observation only: the keeper's final write may follow a terminal state."""
        job = self.job(job_id)
        while job and job.get("finalized_by_keeper") is not True and time.monotonic() < until:
            time.sleep(0.1)
            job = self.job(job_id)
        return job

    def wait_ready(self, until):
        while time.monotonic() < until:
            try:
                with open(self.ready, encoding="utf-8") as handle:
                    return json.load(handle), time.monotonic()
            except (OSError, ValueError):
                time.sleep(0.02)
        return None, None

    def start_count(self):
        return len(self.read_lines(self.starts))

    def deadline_mono(self, expires_at_ms):
        wall = time.time() * 1000.0
        return time.monotonic() + (expires_at_ms - wall) / 1000.0

    def expect(self, condition, reason):
        if not condition:
            self.reasons.append(reason)
        return bool(condition)

    def observe_ready(self, until):
        info, at = self.wait_ready(until)
        if info is None:
            raise TrialAbort("not_ready", "fixture_not_ready")
        pre = pre_observe(info, self.token)
        while not pre["ok"] and time.monotonic() < until:
            time.sleep(0.05)
            pre = pre_observe(info, self.token)
        self.private["fixture"] = info
        self.private["pre"] = pre
        return info, pre, time.monotonic()

    def record_cleanup(self, prefix, obs, origin):
        for role in ("parent", "child"):
            gone = obs[role + "_gone_at"]
            self.metrics["%s_%s_exit_s" % (prefix, role)] = r3(gone - origin) if gone else None
            self.metrics["%s_%s_final" % (prefix, role)] = obs[role + "_class"]
            self.metrics["%s_%s_identity_unknown_samples" % (prefix, role)] = obs[role + "_unknown_samples"]
        closed = obs["port_closed_at"]
        self.metrics["%s_port_closed_s" % prefix] = r3(closed - origin) if closed else None

    # ---- final own-task cleanup ----
    def finish(self):
        forced = 0
        for job_id in sorted(self.job_ids):
            try:
                job = self.job(job_id)
                if job and job.get("state") in ("starting", "running", "stopping"):
                    self.cli(["stop", job_id])
            except TrialTimeout:
                self.reasons.append("cleanup_stop_timeout")
        for session in self.sessions:
            proc = session["proc"]
            rc, _ = self.wait_exit(proc, time.monotonic() + 10)
            if rc is None:
                forced += 1
                proc.send_signal(signal.SIGTERM)
                rc, _ = self.wait_exit(proc, time.monotonic() + 8)
                if rc is None:
                    proc.kill()
                    proc.wait()
        self.metrics["final_forced_session_signals"] = forced
        supervisors, fixtures = final_scan(self)
        killed = 0
        for proc_info in fixtures or []:
            # Last resort, own fixture only: re-verify the live argv token first.
            try:
                current = ps_info(proc_info["pid"])
            except ObserverError as error:
                self.private.setdefault("observer_errors", []).append(
                    {"what": error.what, "details": error.details})
                continue
            if (current and self.token in current["command"]
                    and (HARNESS + " fixture ") in current["command"]
                    and self.ctx["binary"] not in current["command"]):
                try:
                    os.kill(current["pid"], signal.SIGKILL)
                    killed += 1
                except OSError:
                    pass
        self.metrics["final_scan_ok"] = supervisors is not None and fixtures is not None
        self.metrics["final_supervisors_remaining"] = None if supervisors is None else len(supervisors)
        self.metrics["final_fixture_processes_remaining"] = None if fixtures is None else len(fixtures)
        self.metrics["final_fixture_processes_killed_by_harness"] = killed
        ok = forced == 0 and self.metrics["final_scan_ok"] and not supervisors and not fixtures
        self.metrics["final_cleanup_ok"] = ok
        if not self.metrics["final_scan_ok"]:
            self.reasons.append("final_scan_observer_error")
        elif not ok:
            self.reasons.append("final_cleanup_residue")


def final_scan(trial, settle_s=15):
    """(supervisors, fixtures) left for this trial, or (None, None) when the
    process table could not be enumerated. Never reports 'nothing left' after a
    failed scan."""
    deadline = time.monotonic() + settle_s
    while True:
        try:
            supervisors = live_supervisors(trial.state)
            fixtures = live_token_processes(trial.token)
        except ObserverError as error:
            trial.private.setdefault("observer_errors", []).append(
                {"what": error.what, "details": error.details})
            return None, None
        if (not supervisors and not fixtures) or time.monotonic() > deadline:
            return supervisors, fixtures
        time.sleep(0.25)


# ---- scenario implementations ----

def sc_expiry(trial, foreground, resistant):
    p = PROTOCOL
    mode = "resistant" if resistant else "graceful"
    args = trial.run_args(foreground, p["ttl_s"], mode)
    session = None
    if foreground:
        session = trial.start_session(args)
        started = trial.session_startup(session)
    else:
        rc, started, _ = trial.cli(args)
        trial.expect(rc == 0, "launch_nonzero")
    if not started or not isinstance(started.get("job"), dict):
        raise TrialAbort("error", "no_startup_record")
    job = started["job"]
    deadline = trial.deadline_mono(job["expires_at_ms"])
    info, pre, ready_at = trial.observe_ready(deadline - 0.1)
    if ready_at >= deadline or not pre["ok"]:
        trial.metrics["ready_margin_s"] = r3(deadline - ready_at)
        raise TrialAbort("not_ready" if pre["ok"] else "unobserved",
                         "ready_after_deadline" if pre["ok"] else "pre_observation_failed")
    trial.metrics["ready_margin_s"] = r3(deadline - ready_at)
    grace = trial.grace
    lower = (grace if resistant else 0.0) - p["early_tolerance_s"]
    upper = (grace if resistant else 0.0) + p["late_tolerance_s"]
    obs = observe_cleanup(info, trial.token, deadline + grace + 3.0, trial.poll)
    verdict = cleanup_verdict(pre["ok"], obs)
    trial.metrics["cleanup_verdict"] = verdict
    trial.record_cleanup("after_deadline", obs, deadline)
    trial.expect(verdict == "confirmed", "cleanup_not_confirmed")
    for role in ("parent", "child"):
        gone = obs[role + "_gone_at"]
        if gone is not None:
            delta = gone - deadline
            trial.expect(delta >= lower, role + "_ended_before_window")
            trial.expect(delta <= upper, role + "_ended_after_window")
    if obs["port_closed_at"] is not None:
        trial.expect(obs["port_closed_at"] - deadline <= upper + 0.5, "port_closed_late")
    if foreground:
        rc, exited = trial.wait_exit(session["proc"], deadline + grace + 8.0)
        trial.metrics["launcher_exit_code"] = rc
        trial.metrics["launcher_exit_after_deadline_s"] = r3(exited - deadline) if exited else None
        trial.expect(rc == 0, "launcher_exit_code_unexpected")
        lines = trial.read_lines(session["out"])
        trial.metrics["stdout_objects"] = len(lines)
        trial.expect(len(lines) == 1, "stdout_not_single_object")
    trial.wait_state(job["id"], {"expired", "stopped", "exited", "failed", "interrupted"},
                     time.monotonic() + 5)
    final = trial.wait_finalized(job["id"], time.monotonic() + 3)
    trial.metrics["state"] = final.get("state") if final else None
    trial.metrics["reason"] = final.get("reason") if final else None
    trial.metrics["finalized_by_keeper"] = bool(final and final.get("finalized_by_keeper") is True)
    trial.expect(trial.metrics["state"] == "expired", "state_not_expired")
    trial.expect(trial.metrics["reason"] == "deadline", "reason_not_deadline")
    trial.expect(final and final.get("expires_at_ms") == job["expires_at_ms"], "deadline_changed")


def verify_supervisor(trial, job, target):
    """Confirm the current process identity before any signal."""
    keeper_pid, guardian_pid = job.get("keeper_pid"), job.get("guardian_pid")
    keeper = ps_info(keeper_pid) if isinstance(keeper_pid, int) and keeper_pid > 0 else None
    guardian = ps_info(guardian_pid) if isinstance(guardian_pid, int) and guardian_pid > 0 else None
    checks = {
        "keeper_cmd": bool(keeper and not keeper["stat"].startswith("Z")
                           and "__keeper" in keeper["command"] and trial.state in keeper["command"]
                           and job["id"] in keeper["command"]),
        "guardian_cmd": bool(guardian and not guardian["stat"].startswith("Z")
                             and "__guard" in guardian["command"]
                             and trial.state in guardian["command"]
                             and job["id"] in guardian["command"]),
    }
    checks["guardian_parent_is_keeper"] = bool(keeper and guardian and guardian["ppid"] == keeper["pid"])
    fixture_parent = ps_info(trial.private["fixture"]["pid"])
    checks["fixture_group_led_by_guardian"] = bool(
        guardian and fixture_parent and fixture_parent["pgid"] == guardian["pid"])
    trial.private["identity"] = {"checks": checks, "keeper": keeper, "guardian": guardian}
    ok = all(checks.values())
    return ok, (keeper if target == "keeper" else guardian)


def sc_supervisor_kill(trial, target):
    p = PROTOCOL
    rc, started, _ = trial.cli(trial.run_args(False, p["long_ttl_s"], "resistant"))
    if rc != 0 or not started:
        raise TrialAbort("error", "launch_failed")
    job_id = started["job"]["id"]
    info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
    if not pre["ok"]:
        raise TrialAbort("unobserved", "pre_observation_failed")
    job = trial.job(job_id)
    trial.metrics["running_before_signal"] = bool(job and job.get("state") == "running")
    if not trial.metrics["running_before_signal"]:
        raise TrialAbort("error", "job_not_running_before_signal")
    ok, proc = verify_supervisor(trial, job, target)
    trial.metrics["identity_verified"] = ok
    if not ok:
        raise TrialAbort("error", "supervisor_identity_unverified")
    os.kill(proc["pid"], signal.SIGKILL)
    injected = time.monotonic()
    trial.metrics["injected"] = True
    obs = observe_cleanup(info, trial.token, injected + 8.0, trial.poll)
    verdict = cleanup_verdict(pre["ok"], obs)
    trial.metrics["cleanup_verdict"] = verdict
    trial.record_cleanup("after_kill", obs, injected)
    trial.expect(verdict == "confirmed", "cleanup_not_confirmed")
    bound = p["supervisor_kill_cleanup_bound_s"]
    for key in ("parent_gone_at", "child_gone_at", "port_closed_at"):
        if obs[key] is not None:
            trial.expect(obs[key] - injected <= bound, key.replace("_at", "") + "_late")
    deadline = time.monotonic() + 6
    while live_supervisors(trial.state) and time.monotonic() < deadline:
        time.sleep(0.2)
    trial.metrics["supervisors_remaining_after_6s"] = len(live_supervisors(trial.state))
    final = trial.wait_state(job_id, {"interrupted", "stopped", "expired", "exited", "failed"},
                             time.monotonic() + 5)
    trial.metrics["state"] = final.get("state") if final else None
    trial.metrics["reason"] = final.get("reason") if final else None
    trial.metrics["finalized_by_keeper"] = bool(final and final.get("finalized_by_keeper") is True)
    trial.expect(trial.metrics["state"] == "interrupted", "state_not_interrupted")
    trial.expect(trial.metrics["supervisors_remaining_after_6s"] == 0, "supervisor_remaining")


def fill_pipe():
    read_fd, write_fd = os.pipe()
    flags = fcntl.fcntl(write_fd, fcntl.F_GETFL)
    fcntl.fcntl(write_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
    filled = 0
    for size in (65536, 4096, 512, 1):
        chunk = b"x" * size
        while True:
            try:
                filled += os.write(write_fd, chunk)
            except BlockingIOError:
                break
    fcntl.fcntl(write_fd, fcntl.F_SETFL, flags)
    return read_fd, write_fd, filled


def unread(read_fd):
    buf = bytearray(4)
    fcntl.ioctl(read_fd, termios.FIONREAD, buf)
    return int.from_bytes(buf, sys.byteorder)


def sc_foreground_sigint_blocked(trial):
    p = PROTOCOL
    read_fd, write_fd, filled = fill_pipe()
    try:
        with os.fdopen(write_fd, "wb", closefd=True) as writer:
            session = trial.start_session(
                trial.run_args(True, p["interrupt_ttl_s"], "resistant"), stdout=writer)
        proc = session["proc"]
        info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
        if not pre["ok"]:
            raise TrialAbort("unobserved", "pre_observation_failed")
        jobs = []
        until = time.monotonic() + 5
        while time.monotonic() < until:
            rc, obj, _ = trial.cli(["list", "--all"])
            jobs = obj.get("jobs", []) if obj else []
            for listed in jobs:
                if listed.get("id"):
                    trial.job_ids.add(listed["id"])
            if len(jobs) == 1 and jobs[0].get("state") == "running":
                break
            time.sleep(0.1)
        # Required preconditions: abort, never pass, when any is missing.
        trial.metrics["single_running_job_before_signal"] = (
            len(jobs) == 1 and jobs[0].get("state") == "running")
        if not trial.metrics["single_running_job_before_signal"]:
            raise TrialAbort("error", "job_not_running_before_signal")
        job_id = jobs[0]["id"]
        time.sleep(0.5)  # give the launcher time to attempt its startup write
        trial.metrics["pipe_full_before_signal"] = filled > 0 and unread(read_fd) == filled
        trial.metrics["launcher_alive_before_signal"] = proc.poll() is None
        if not trial.metrics["pipe_full_before_signal"]:
            raise TrialAbort("error", "pipe_not_full_precondition")
        if not trial.metrics["launcher_alive_before_signal"]:
            raise TrialAbort("error", "launcher_exited_before_signal")
        launcher = ps_info(proc.pid)
        identity = bool(proc.poll() is None and launcher and not launcher["stat"].startswith("Z")
                        and "--foreground" in launcher["command"]
                        and trial.state in launcher["command"])
        trial.metrics["identity_verified"] = identity
        if not identity:
            raise TrialAbort("error", "launcher_identity_unverified")
        proc.send_signal(signal.SIGINT)
        injected = time.monotonic()
        trial.metrics["injected"] = True
        rc, exited = trial.wait_exit(proc, injected + 10)
        trial.metrics["launcher_exit_code"] = rc
        trial.metrics["launcher_exit_after_signal_s"] = r3(exited - injected) if exited else None
        trial.expect(rc == 128 + signal.SIGINT, "exit_code_not_130")
        trial.expect(exited is not None and exited - injected <= p["signal_exit_bound_s"],
                     "launcher_exit_late")
        at_exit = {role: classify(info[key], trial.token)[0]
                   for role, key in (("parent", "pid"), ("child", "child"))}
        # Recorded single samples taken right after the launcher was reaped;
        # descriptive only. The pass criterion is the polled cleanup below.
        trial.metrics["at_launcher_exit_parent_class"] = at_exit["parent"]
        trial.metrics["at_launcher_exit_child_class"] = at_exit["child"]
        trial.metrics["fixture_gone_at_launcher_exit"] = all(v in GONE_CLASSES for v in at_exit.values())
        trial.metrics["port_closed_at_launcher_exit"] = port_state(info["port"]) == "closed"
        trial.metrics["pipe_still_unread_at_exit"] = unread(read_fd) == filled
        obs = observe_cleanup(info, trial.token, time.monotonic() + 3, trial.poll)
        verdict = cleanup_verdict(pre["ok"], obs)
        trial.metrics["cleanup_verdict"] = verdict
        trial.record_cleanup("after_signal", obs, injected)
        trial.expect(verdict == "confirmed", "cleanup_not_confirmed")
        trial.expect(trial.metrics["pipe_still_unread_at_exit"], "pipe_was_read")
        trial.metrics["supervisors_remaining_at_exit"] = len(live_supervisors(trial.state))
        trial.expect(trial.metrics["supervisors_remaining_at_exit"] == 0, "supervisor_remaining")
        job = trial.job(job_id)
        trial.metrics["state"] = job.get("state") if job else None
        trial.metrics["reason"] = job.get("reason") if job else None
        trial.metrics["finalized_by_keeper"] = bool(job and job.get("finalized_by_keeper") is True)
        trial.expect(trial.metrics["state"] == "interrupted", "state_not_interrupted")
        trial.expect(trial.metrics["reason"] == "supervision_lost", "reason_not_supervision_lost")
    finally:
        os.close(read_fd)


def launch_keyed(trial, foreground, ttl, mode, key, env=None):
    args = trial.run_args(foreground, ttl, mode, key=key)
    if foreground:
        session = trial.start_session(args, env=env)
        started = trial.session_startup(session)
        rc = 0 if started else None
    else:
        rc, started, _ = trial.cli(args, env=env)
        session = None
    if not started or not isinstance(started.get("job"), dict):
        raise TrialAbort("error", "no_startup_record")
    trial.expect(rc == 0, "launch_nonzero")
    trial.expect(started.get("reused") is False, "initial_reported_reused")
    return started["job"], session


def retry_call(trial, foreground, ttl, mode, key, env=None, variant=False, reuse=False):
    args = trial.run_args(foreground, ttl, mode, key=key, variant=variant, reuse=reuse)
    rc, obj, elapsed = trial.cli(args, env=env, timeout=PROTOCOL["immediate_return_bound_s"])
    return rc, obj or {}, elapsed


def stop_and_check(trial, job_id, info, session=None, prefix="stop", pre_ok=True):
    """Own cleanup through ProcLease with independent confirmation. Callers
    pass the actual positive pre-observation result."""
    rc, obj, _ = trial.cli(["stop", job_id])
    trial.metrics[prefix + "_state"] = (obj or {}).get("job", {}).get("state")
    obs = observe_cleanup(info, trial.token, time.monotonic() + 5, trial.poll)
    trial.metrics[prefix + "_cleanup_verdict"] = cleanup_verdict(pre_ok, obs)
    trial.expect(trial.metrics[prefix + "_cleanup_verdict"] == "confirmed", prefix + "_cleanup_not_confirmed")
    if session:
        rc, _ = trial.wait_exit(session["proc"], time.monotonic() + 10)
        trial.metrics[prefix + "_launcher_exit_code"] = rc
        trial.expect(rc == 0, prefix + "_launcher_exit_unexpected")


def sc_retry_sequential(trial, foreground):
    p = PROTOCOL
    key = "seq-" + trial.token
    job, session = launch_keyed(trial, foreground, p["long_ttl_s"], "graceful", key)
    info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
    if not pre["ok"]:
        raise TrialAbort("unobserved", "pre_observation_failed")
    rc, obj, elapsed = retry_call(trial, foreground, p["long_ttl_s"], "graceful", key)
    retry_job = obj.get("job") or {}
    trial.metrics["retry_exit_code"] = rc
    trial.metrics["retry_elapsed_s"] = r3(elapsed)
    trial.metrics["retry_reused"] = obj.get("reused")
    trial.metrics["same_id"] = retry_job.get("id") == job["id"]
    trial.metrics["same_deadline"] = retry_job.get("expires_at_ms") == job["expires_at_ms"]
    trial.metrics["fixture_starts"] = trial.start_count()
    trial.metrics["job_count"] = trial.job_count()
    trial.metrics["fixture_still_executing"] = classify(info["pid"], trial.token)[0] == "executing"
    for cond, reason in ((rc == 0, "retry_nonzero"), (obj.get("reused") is True, "retry_not_reused"),
                         (trial.metrics["same_id"], "id_changed"),
                         (trial.metrics["same_deadline"], "deadline_changed"),
                         (trial.metrics["fixture_starts"] == 1, "duplicate_start"),
                         (trial.metrics["job_count"] == 1, "job_count_not_one"),
                         (trial.metrics["fixture_still_executing"], "fixture_not_running")):
        trial.expect(cond, reason)
    stop_and_check(trial, job["id"], info, session)


def sc_retry_concurrent(trial, foreground):
    p = PROTOCOL
    key = "conc-" + trial.token
    args = trial.argv(trial.run_args(foreground, p["long_ttl_s"], "graceful", key=key))
    gate = os.path.join(trial.files, "gate")
    callers = []
    for n in range(p["concurrent_callers"]):
        marker = os.path.join(trial.files, "waiting-%d" % n)
        out_path = os.path.join(trial.files, "caller-%d.out" % n)
        err_path = os.path.join(trial.files, "caller-%d.err" % n)
        with open(out_path, "wb") as out, open(err_path, "wb") as err:
            proc = subprocess.Popen([sys.executable, "-I", HARNESS, "gate-exec", gate, marker, "--"] + args,
                                    cwd=trial.dir, env=trial.env, stdin=subprocess.DEVNULL,
                                    stdout=out, stderr=err, start_new_session=True)
        session = {"proc": proc, "out": out_path, "err": err_path}
        trial.sessions.append(session)
        callers.append((session, marker))
    until = time.monotonic() + 15
    while not all(os.path.exists(m) for _, m in callers):
        if time.monotonic() > until:
            raise TrialAbort("error", "callers_not_waiting")
        time.sleep(0.01)
    with open(gate, "w", encoding="utf-8") as handle:
        handle.write("go\n")
    released = time.monotonic()
    expected_exits = p["concurrent_callers"] - (1 if foreground else 0)
    until = released + 15
    while time.monotonic() < until:
        if sum(s["proc"].poll() is not None for s, _ in callers) >= expected_exits:
            break
        time.sleep(0.02)
    time.sleep(0.3)
    exited = [s for s, _ in callers if s["proc"].poll() is not None]
    running = [s for s, _ in callers if s["proc"].poll() is None]
    results = []
    for s, _ in callers:
        lines = trial.read_lines(s["out"])
        obj = None
        if lines:
            try:
                obj = json.loads(lines[0])
            except ValueError:
                obj = None
        results.append((s, obj))
    ids = {obj["job"]["id"] for _, obj in results if obj and isinstance(obj.get("job"), dict)}
    trial.job_ids.update(ids)
    creators = sum(1 for _, obj in results if obj and obj.get("reused") is False)
    reusers = sum(1 for _, obj in results if obj and obj.get("reused") is True)
    nonzero = sum(1 for s in exited if s["proc"].returncode != 0)
    trial.metrics.update({"callers": len(callers), "callers_exited": len(exited),
                          "callers_running": len(running), "callers_nonzero_exit": nonzero,
                          "distinct_job_ids": len(ids), "reported_created": creators,
                          "reported_reused": reusers})
    info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
    time.sleep(0.5)
    trial.metrics["fixture_starts"] = trial.start_count()
    trial.metrics["job_count"] = trial.job_count()
    trial.metrics["pre_observed"] = pre["ok"]
    for cond, reason in ((len(ids) == 1, "distinct_ids_not_one"), (creators == 1, "created_not_one"),
                         (reusers == len(callers) - 1, "reused_count_unexpected"),
                         (nonzero == 0, "caller_nonzero_exit"),
                         (len(exited) == expected_exits, "caller_exit_count_unexpected"),
                         (trial.metrics["fixture_starts"] == 1, "duplicate_start"),
                         (trial.metrics["job_count"] == 1, "job_count_not_one"),
                         (pre["ok"], "pre_observation_failed")):
        trial.expect(cond, reason)
    if foreground and running:
        holder_obj = [obj for s, obj in results if s is running[0]]
        trial.metrics["holder_was_creator"] = bool(holder_obj and holder_obj[0]
                                                   and holder_obj[0].get("reused") is False)
        trial.expect(trial.metrics["holder_was_creator"], "holder_not_creator")
    if len(ids) == 1:
        stop_and_check(trial, next(iter(ids)), info, running[0] if (foreground and len(running) == 1) else None,
                       pre_ok=pre["ok"])


def sc_reuse_changed_env(trial, foreground):
    p = PROTOCOL
    key = "env-" + trial.token
    job, session = launch_keyed(trial, foreground, p["long_ttl_s"], "graceful", key)
    info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
    if not pre["ok"]:
        raise TrialAbort("unobserved", "pre_observation_failed")
    changed = dict(trial.env)
    changed_value = "changed-" + trial.token
    changed["PL_EXPERIMENT_MARKER"] = changed_value
    rc, obj, _ = retry_call(trial, foreground, p["long_ttl_s"], "graceful", key, env=changed)
    error = str(obj.get("error") or "")
    trial.metrics["ordinary_retry_exit_code"] = rc
    trial.metrics["ordinary_retry_names_environment"] = "differs: environment" in error
    trial.metrics["ordinary_retry_discloses_value"] = changed_value in error
    rc2, obj2, elapsed = retry_call(trial, foreground, p["long_ttl_s"], "graceful", key,
                                    env=changed, reuse=True)
    reuse_job = obj2.get("job") or {}
    body = http_body(info["port"]) or ""
    trial.metrics.update({
        "reuse_exit_code": rc2, "reuse_elapsed_s": r3(elapsed), "reuse_reused": obj2.get("reused"),
        "same_id": reuse_job.get("id") == job["id"],
        "same_deadline": reuse_job.get("expires_at_ms") == job["expires_at_ms"],
        "service_keeps_original_env": ("original-" + trial.token) in body,
        "fixture_starts": trial.start_count(), "job_count": trial.job_count()})
    for cond, reason in ((rc == 1, "ordinary_retry_not_rejected"),
                         (trial.metrics["ordinary_retry_names_environment"], "rejection_not_environment"),
                         (not trial.metrics["ordinary_retry_discloses_value"], "rejection_disclosed_value"),
                         (rc2 == 0, "reuse_nonzero"), (obj2.get("reused") is True, "reuse_not_reused"),
                         (trial.metrics["same_id"], "id_changed"),
                         (trial.metrics["same_deadline"], "deadline_changed"),
                         (trial.metrics["service_keeps_original_env"], "service_env_changed"),
                         (trial.metrics["fixture_starts"] == 1, "duplicate_start"),
                         (trial.metrics["job_count"] == 1, "job_count_not_one")):
        trial.expect(cond, reason)
    stop_and_check(trial, job["id"], info, session)


def sc_mismatch_command(trial, foreground):
    p = PROTOCOL
    key = "cmd-" + trial.token
    job, session = launch_keyed(trial, foreground, p["long_ttl_s"], "graceful", key)
    info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
    if not pre["ok"]:
        raise TrialAbort("unobserved", "pre_observation_failed")
    rc, obj, _ = retry_call(trial, foreground, p["long_ttl_s"], "graceful", key, variant=True)
    rc2, obj2, _ = retry_call(trial, foreground, p["long_ttl_s"], "graceful", key, variant=True,
                              reuse=True)
    current = trial.job(job["id"]) or {}
    trial.metrics.update({
        "ordinary_exit_code": rc, "reuse_exit_code": rc2,
        "ordinary_names_command": "differs: command" in str(obj.get("error") or ""),
        "reuse_names_command": "differs: command" in str(obj2.get("error") or ""),
        "fixture_starts": trial.start_count(), "job_count": trial.job_count(),
        "original_running": current.get("state") == "running"
        and classify(info["pid"], trial.token)[0] == "executing",
        "same_deadline": current.get("expires_at_ms") == job["expires_at_ms"]})
    for cond, reason in ((rc == 1, "ordinary_not_rejected"), (rc2 == 1, "reuse_not_rejected"),
                         (trial.metrics["ordinary_names_command"], "ordinary_not_command"),
                         (trial.metrics["reuse_names_command"], "reuse_not_command"),
                         (trial.metrics["fixture_starts"] == 1, "duplicate_start"),
                         (trial.metrics["job_count"] == 1, "job_count_not_one"),
                         (trial.metrics["original_running"], "original_not_running"),
                         (trial.metrics["same_deadline"], "deadline_changed")):
        trial.expect(cond, reason)
    stop_and_check(trial, job["id"], info, session)


def sc_terminal_retry(trial, foreground):
    p = PROTOCOL
    key = "term-" + trial.token
    job, session = launch_keyed(trial, foreground, p["ttl_s"], "graceful", key)
    deadline = trial.deadline_mono(job["expires_at_ms"])
    info, pre, ready_at = trial.observe_ready(deadline - 0.1)
    if not pre["ok"] or ready_at >= deadline:
        raise TrialAbort("not_ready" if pre["ok"] else "unobserved", "not_ready_before_deadline")
    obs = observe_cleanup(info, trial.token, deadline + trial.grace + 3.0, trial.poll)
    trial.metrics["expiry_cleanup_verdict"] = cleanup_verdict(pre["ok"], obs)
    trial.expect(trial.metrics["expiry_cleanup_verdict"] == "confirmed", "expiry_cleanup_not_confirmed")
    final = trial.wait_state(job["id"], {"expired"}, time.monotonic() + 5)
    trial.metrics["state_before_retry"] = final.get("state") if final else None
    trial.expect(trial.metrics["state_before_retry"] == "expired", "state_not_expired")
    if session:
        rc, _ = trial.wait_exit(session["proc"], time.monotonic() + 8)
        trial.metrics["launcher_exit_code"] = rc
        trial.expect(rc == 0, "launcher_exit_unexpected")
    for label, reuse in (("ordinary", False), ("reuse", True)):
        rc, obj, elapsed = retry_call(trial, foreground, p["ttl_s"], "graceful", key, reuse=reuse)
        rjob = obj.get("job") or {}
        trial.metrics[label + "_exit_code"] = rc
        trial.metrics[label + "_elapsed_s"] = r3(elapsed)
        trial.metrics[label + "_reused"] = obj.get("reused")
        trial.metrics[label + "_state"] = rjob.get("state")
        trial.metrics[label + "_same_id"] = rjob.get("id") == job["id"]
        trial.metrics[label + "_same_deadline"] = rjob.get("expires_at_ms") == job["expires_at_ms"]
        for cond, reason in ((rc == 0, label + "_nonzero"), (obj.get("reused") is True, label + "_not_reused"),
                             (rjob.get("state") == "expired", label + "_state_not_expired"),
                             (trial.metrics[label + "_same_id"], label + "_id_changed"),
                             (trial.metrics[label + "_same_deadline"], label + "_deadline_changed")):
            trial.expect(cond, reason)
    time.sleep(1.0)
    trial.metrics["fixture_starts"] = trial.start_count()
    trial.metrics["job_count"] = trial.job_count()
    trial.metrics["port_closed_after_retries"] = port_state(info["port"]) == "closed"
    trial.metrics["fixture_processes_after_retries"] = len(live_token_processes(trial.token))
    for cond, reason in ((trial.metrics["fixture_starts"] == 1, "resurrected_start"),
                         (trial.metrics["job_count"] == 1, "job_count_not_one"),
                         (trial.metrics["port_closed_after_retries"], "port_reopened"),
                         (trial.metrics["fixture_processes_after_retries"] == 0, "fixture_process_present")):
        trial.expect(cond, reason)


def sc_negative_live(trial):
    """The observer must NOT confirm cleanup of a live, unmanaged fixture."""
    p = PROTOCOL
    with open(os.path.join(trial.files, "direct.out"), "wb") as out:
        proc = subprocess.Popen(trial.fixture_argv("graceful"), cwd=trial.dir, env=trial.env,
                                stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT,
                                start_new_session=True)
    try:
        info, pre, _ = trial.observe_ready(time.monotonic() + p["ready_timeout_long_s"])
        trial.metrics["pre_observed"] = pre["ok"]
        start = time.monotonic()
        obs = observe_cleanup(info, trial.token, start + p["negative_control_window_s"], trial.poll)
        verdict = cleanup_verdict(pre["ok"], obs)
        trial.metrics["observer_verdict"] = verdict
        trial.expect(pre["ok"], "pre_observation_failed")
        trial.expect(verdict == "not_confirmed", "observer_confirmed_live_process")
        # Harness-owned group: verify identity, then stop it ourselves.
        current = ps_info(proc.pid)
        if current and trial.token in current["command"] and current["pgid"] == proc.pid:
            os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)
        obs2 = observe_cleanup(info, trial.token, time.monotonic() + 5, trial.poll)
        trial.metrics["observer_detects_after_harness_kill"] = cleanup_verdict(pre["ok"], obs2) == "confirmed"
        trial.expect(trial.metrics["observer_detects_after_harness_kill"], "observer_missed_real_exit")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()


def sc_negative_blind(trial):
    """An observer that cannot see identities or the port cannot report a pass."""
    reaped = subprocess.Popen(["/usr/bin/true"])
    reaped.wait()
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    info = {"pid": reaped.pid, "child": reaped.pid, "port": port}
    pre = pre_observe(info, trial.token)
    obs = observe_cleanup(info, trial.token, time.monotonic() + 1.0, trial.poll)
    verdict = cleanup_verdict(pre["ok"], obs)
    trial.metrics["pre_observed"] = pre["ok"]
    trial.metrics["raw_absence_seen"] = obs["parent_gone_at"] is not None and obs["port_closed_at"] is not None
    trial.metrics["observer_verdict"] = verdict
    trial.expect(verdict == "unobserved", "blind_observer_not_unobserved")


SCENARIOS = {
    "detached_expiry_graceful": lambda t: sc_expiry(t, False, False),
    "detached_expiry_term_resistant": lambda t: sc_expiry(t, False, True),
    "foreground_expiry_graceful": lambda t: sc_expiry(t, True, False),
    "foreground_expiry_term_resistant": lambda t: sc_expiry(t, True, True),
    "detached_keeper_sigkill": lambda t: sc_supervisor_kill(t, "keeper"),
    "detached_guardian_sigkill": lambda t: sc_supervisor_kill(t, "guardian"),
    "foreground_sigint_blocked_stdout": sc_foreground_sigint_blocked,
    "retry_sequential_detached": lambda t: sc_retry_sequential(t, False),
    "retry_sequential_foreground": lambda t: sc_retry_sequential(t, True),
    "retry_concurrent8_detached": lambda t: sc_retry_concurrent(t, False),
    "retry_concurrent8_foreground": lambda t: sc_retry_concurrent(t, True),
    "reuse_changed_env_detached": lambda t: sc_reuse_changed_env(t, False),
    "reuse_changed_env_foreground": lambda t: sc_reuse_changed_env(t, True),
    "mismatch_command_detached": lambda t: sc_mismatch_command(t, False),
    "mismatch_command_foreground": lambda t: sc_mismatch_command(t, True),
    "terminal_retry_detached": lambda t: sc_terminal_retry(t, False),
    "terminal_retry_foreground": lambda t: sc_terminal_retry(t, True),
    "negative_control_unmanaged_live": sc_negative_live,
    "negative_control_blind_observer": sc_negative_blind,
}
assert list(SCENARIOS) == PROTOCOL["scenarios"]


def decide_outcome(outcome, trial):
    """A failed final scan is an observer error, never a pass."""
    if outcome is None and trial.metrics.get("final_scan_ok") is not True:
        outcome = "error"
    if outcome is None:
        outcome = "pass" if not trial.reasons else "fail"
    return outcome


def run_trial(ctx, scenario, rep, index):
    started = time.monotonic()
    trial = Trial(ctx, scenario, rep, index)
    outcome = None
    try:
        SCENARIOS[scenario](trial)
    except TrialAbort as abort:
        outcome = abort.outcome
        trial.reasons.append(abort.reason)
    except TrialTimeout as timeout:
        outcome = "timeout"
        trial.reasons.append("timeout_" + str(timeout))
    except ObserverError as error:  # never interpreted as absence
        outcome = "error"
        trial.reasons.append("observer_error")
        trial.private.setdefault("observer_errors", []).append(
            {"what": error.what, "details": error.details, "traceback": traceback.format_exc()})
    except Exception as error:  # retained, never retried
        outcome = "error"
        trial.reasons.append("exception_" + type(error).__name__)
        trial.private["traceback"] = traceback.format_exc()
    try:
        trial.finish()
    except Exception as error:
        trial.reasons.append("finish_exception_" + type(error).__name__)
        trial.private["finish_traceback"] = traceback.format_exc()
        trial.metrics["final_cleanup_ok"] = False
    outcome = decide_outcome(outcome, trial)
    duration = time.monotonic() - started
    public = {"scenario": scenario, "rep": rep, "outcome": outcome,
              "reasons": sorted(set(trial.reasons)), "duration_s": r3(duration),
              "metrics": sanitize_metrics(trial.metrics)}
    private = dict(public, private=trial.private, metrics_raw=trial.metrics)
    with open(os.path.join(trial.dir, "result.json"), "w", encoding="utf-8") as handle:
        json.dump(private, handle, indent=1, default=str)
    return public


# --------------------------------------------------------------------------
# Sanitization and validation
# --------------------------------------------------------------------------

# Structural safeguards: a narrow character allowlist (no '/', '\\', '~', '@'
# or quotes, so no paths, e-mail addresses or URLs), no IPv4 literals, no
# hostname-like dotted names, and no harness trial tokens. Identity fragments
# of the account and host running this process are added at runtime; further
# private fragments can be supplied from a private file (--deny-file). No
# owner-specific identifier is written into this public source.
SAFE_STRING = re.compile(r"^[A-Za-z0-9_.:+\- ]{0,80}$")
IPV4 = re.compile(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b")
HOSTNAME_LIKE = re.compile(r"[A-Za-z][A-Za-z0-9-]*\.(?:local|lan|home|internal|corp|localdomain)\b", re.I)
GENERIC_FRAGMENTS = ["localhost", "plx"]
EXTRA_DENY = []


def runtime_identity_fragments():
    fragments = set()
    try:
        import getpass
        fragments.add(getpass.getuser())
    except Exception:
        pass
    home = os.path.expanduser("~")
    fragments.update(part for part in home.split(os.sep) if part)
    host = socket.gethostname()
    fragments.add(host)
    fragments.add(host.split(".")[0])
    generic = {"home", "users", "root", "var", "tmp", "private", "localhost"}
    return sorted(f for f in fragments if len(f) >= 3 and f.lower() not in generic)


def deny_fragments():
    return GENERIC_FRAGMENTS + runtime_identity_fragments() + EXTRA_DENY


def load_deny_file(path):
    if not path:
        return
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line and not line.startswith("#") and len(line) >= 3:
                EXTRA_DENY.append(line)


def safe_string(value, fragments=None):
    fragments = deny_fragments() if fragments is None else fragments
    return (isinstance(value, str) and bool(SAFE_STRING.match(value))
            and not IPV4.search(value) and not HOSTNAME_LIKE.search(value)
            and not any(f.lower() in value.lower() for f in fragments))


def sanitize_metrics(metrics):
    clean = {}
    for key, value in metrics.items():
        if not re.match(r"^[a-z0-9_]{1,64}$", key):
            continue
        if value is None or isinstance(value, bool) or isinstance(value, int):
            clean[key] = value
        elif isinstance(value, float):
            clean[key] = round(value, 3)
        elif isinstance(value, str) and re.match(r"^[a-z_]{1,40}$", value):
            clean[key] = value
        else:
            clean[key] = "redacted"
    return clean


def check_public(value, path="$", problems=None, fragments=None):
    problems = [] if problems is None else problems
    fragments = deny_fragments() if fragments is None else fragments
    if isinstance(value, dict):
        for key, item in value.items():
            if not safe_string(key, fragments):
                problems.append("unsafe key at " + path)
            check_public(item, path + "." + str(key)[:40], problems, fragments)
    elif isinstance(value, list):
        for i, item in enumerate(value):
            check_public(item, "%s[%d]" % (path, i), problems, fragments)
    elif isinstance(value, str):
        if not safe_string(value, fragments):
            problems.append("unsafe string at " + path)
    elif value is not None and not isinstance(value, (bool, int, float)):
        problems.append("unexpected type at " + path)
    return problems


def summarize(trials, meta):
    scenarios = {}
    for name in PROTOCOL["scenarios"]:
        rows = [t for t in trials if t["scenario"] == name]
        if not rows:
            continue
        counts = {o: sum(1 for t in rows if t["outcome"] == o) for o in OUTCOMES}
        reasons = {}
        for t in rows:
            for reason in t["reasons"]:
                reasons[reason] = reasons.get(reason, 0) + 1
        timings = {}
        keys = sorted({k for t in rows for k, v in t["metrics"].items()
                       if k.endswith("_s") and isinstance(v, (int, float)) and not isinstance(v, bool)})
        for key in keys:
            values = sorted(t["metrics"][key] for t in rows
                            if isinstance(t["metrics"].get(key), (int, float))
                            and not isinstance(t["metrics"].get(key), bool))
            if values:
                timings[key] = {"n": len(values), "min": values[0], "median": round(statistics.median(values), 3),
                                "max": values[-1]}
        states = {}
        for t in rows:
            state = t["metrics"].get("state")
            if state is not None:
                states[state] = states.get(state, 0) + 1
        finals = {}
        for t in rows:
            for k, v in t["metrics"].items():
                if k.endswith("_final") and isinstance(v, str):
                    finals[v] = finals.get(v, 0) + 1
        scenarios[name] = {"trials": len(rows), "outcomes": counts, "reasons": reasons,
                           "timings_s": timings, "states": states,
                           "fixture_final_classes": finals,
                           "finalized_by_keeper_true": sum(1 for t in rows
                                                           if t["metrics"].get("finalized_by_keeper") is True)}
    total = {o: sum(s["outcomes"][o] for s in scenarios.values()) for o in OUTCOMES}
    return {"meta": meta, "scenarios": scenarios, "totals": dict(total, trials=len(trials))}


def validate_pair(trials_path, summary_path):
    problems = []
    with open(trials_path, encoding="utf-8") as handle:
        trials_doc = json.load(handle)
    with open(summary_path, encoding="utf-8") as handle:
        summary = json.load(handle)
    problems += check_public(trials_doc, "trials")
    problems += check_public(summary, "summary")
    trials = trials_doc["trials"]
    meta = summary["meta"]
    if trials_doc.get("meta") != meta:
        problems.append("trial and summary metadata differ")
    recomputed = summarize(trials, meta)
    if canonical(recomputed) != canonical(summary):
        problems.append("summary does not match recomputation from trials")
    reps = meta.get("repetitions")
    for name, scenario in summary["scenarios"].items():
        if sum(scenario["outcomes"].values()) != scenario["trials"]:
            problems.append(name + ": outcomes do not sum to trials")
        if meta.get("kind") == "counted" and scenario["trials"] != reps:
            problems.append(name + ": trials differ from repetitions")
        rep_ids = sorted(t["rep"] for t in trials if t["scenario"] == name)
        if rep_ids != list(range(1, len(rep_ids) + 1)):
            problems.append(name + ": repetition identifiers are not 1..n")
    if summary["totals"]["trials"] != len(trials):
        problems.append("total trials mismatch")
    if meta.get("kind") == "counted" and set(summary["scenarios"]) != set(PROTOCOL["scenarios"]):
        problems.append("counted run does not cover every scenario")
    return problems


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------

def os_meta():
    meta = {"os": platform.system(), "os_release": platform.release(),
            "machine": platform.machine(), "python": platform.python_version(),
            "cpu_count": os.cpu_count()}
    if platform.system() == "Darwin":
        meta["macos_version"] = platform.mac_ver()[0]
    try:
        with open("/etc/os-release", encoding="utf-8") as handle:
            fields = dict(line.rstrip("\n").split("=", 1) for line in handle if "=" in line)
        meta["distribution"] = fields.get("ID", "").strip('"')
        version = fields.get("VERSION_ID", "").strip('"')
        if version:
            meta["distribution_version"] = version
    except OSError:
        pass
    return {k: v for k, v in meta.items() if v is not None and (not isinstance(v, str) or safe_string(v))}


def cmd_run(args):
    if args.kind == "counted" and args.reps != PROTOCOL["repetitions"]:
        sys.exit("counted runs use the protocol repetition count")
    if args.parallel > PROTOCOL["max_parallel_trials"]:
        sys.exit("parallel trials exceed the protocol bound")
    binary = os.path.realpath(args.binary)
    version = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=20).stdout.strip()
    if version != PROTOCOL["expected_version_output"]:
        sys.exit("unexpected binary version: %r" % version)
    scenarios = args.scenarios.split(",") if args.scenarios else PROTOCOL["scenarios"]
    unknown = [s for s in scenarios if s not in SCENARIOS]
    if unknown:
        sys.exit("unknown scenarios: %s" % unknown)
    if args.kind == "counted" and scenarios != PROTOCOL["scenarios"]:
        sys.exit("counted runs cover every scenario")
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    short = args.kind[0] + secrets.token_hex(2)
    run_id = "%s-%s-%s-%s" % (args.kind, args.platform_label, stamp, short[1:])
    root = os.path.realpath(args.root)
    os.makedirs(root, mode=0o700, exist_ok=True)
    run_dir = os.path.join(root, short)
    os.makedirs(run_dir, mode=0o700)  # never reuse a run directory
    out_dir = os.path.join(os.path.realpath(args.out), run_id)
    os.makedirs(out_dir)
    load_deny_file(args.deny_file)
    pre_existing = len(ps_scan(root + "/"))  # an observer error here stops the run before any trial
    ctx = {"binary": binary, "run_dir": run_dir}
    meta = {"run_id": run_id, "kind": args.kind, "platform": args.platform_label,
            "protocol_sha256": protocol_hash(), "harness_sha256": sha256_file(HARNESS),
            "methodology_sha256": args.methodology_sha256 or None,
            "binary_sha256": sha256_file(binary), "binary_version": version,
            "archive_sha256": args.archive_sha256 or None,
            "repetitions": args.reps, "parallel_trials": args.parallel,
            "started_utc": utc_now(), "load_average_start": [round(x, 2) for x in os.getloadavg()],
            "pre_existing_task_processes": pre_existing}
    meta.update(os_meta())
    jobs = [(s, rep) for rep in range(1, args.reps + 1) for s in scenarios]
    print("run %s: %d trials, private dir %s" % (run_id, len(jobs), run_dir), flush=True)
    results = [None] * len(jobs)
    lock = threading.Lock()
    done = [0]

    def work(i):
        scenario, rep = jobs[i]
        result = run_trial(ctx, scenario, rep, i)
        results[i] = result
        with lock:
            done[0] += 1
            print("[%d/%d] %s #%d %s %s" % (done[0], len(jobs), scenario, rep, result["outcome"],
                                           ",".join(result["reasons"])), flush=True)

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.parallel) as pool:
        list(pool.map(work, range(len(jobs))))
    time.sleep(1)
    meta["finished_utc"] = utc_now()
    meta["load_average_end"] = [round(x, 2) for x in os.getloadavg()]
    try:
        meta["final_task_processes"] = len([p for p in ps_scan(run_dir + "/") if not p["stat"].startswith("Z")])
        meta["final_task_supervisors"] = len(live_supervisors(run_dir + "/"))
        meta["final_scan_ok"] = True
    except ObserverError as error:
        meta["final_task_processes"] = None
        meta["final_task_supervisors"] = None
        meta["final_scan_ok"] = False
        with open(os.path.join(run_dir, "final-scan-error.json"), "w", encoding="utf-8") as handle:
            json.dump({"what": error.what, "details": error.details}, handle, indent=1)
        print("FINAL SCAN OBSERVER ERROR (details in private run directory)", flush=True)
    trials_doc = {"meta": meta, "trials": results}
    summary = summarize(results, meta)
    with open(os.path.join(out_dir, "trials.json"), "w", encoding="utf-8") as handle:
        json.dump(trials_doc, handle, indent=1, sort_keys=True)
        handle.write("\n")
    with open(os.path.join(out_dir, "summary.json"), "w", encoding="utf-8") as handle:
        json.dump(summary, handle, indent=1, sort_keys=True)
        handle.write("\n")
    problems = validate_pair(os.path.join(out_dir, "trials.json"), os.path.join(out_dir, "summary.json"))
    print(json.dumps(summary["totals"]), flush=True)
    print("public results: %s" % out_dir, flush=True)
    if problems:
        print("VALIDATION PROBLEMS: %s" % problems, flush=True)
        sys.exit(2)


# Protocol constants of earlier, superseded series whose public files remain
# published unchanged. Their trial/summary structure is unchanged, so they are
# validated with the same arithmetic and privacy checks; the label printed says
# which protocol version a directory used.
HISTORICAL_PROTOCOLS = {
    "ec28ec60c068b23454f334d31b0763684f3f70750a2ec2b96c01c1b032f23a38": 1,
}


def cmd_validate(args):
    load_deny_file(args.deny_file)
    failed = False
    for directory in args.dirs:
        problems = validate_pair(os.path.join(directory, "trials.json"), os.path.join(directory, "summary.json"))
        with open(os.path.join(directory, "summary.json"), encoding="utf-8") as handle:
            meta = json.load(handle)["meta"]
        if meta["protocol_sha256"] == protocol_hash():
            version = PROTOCOL["protocol_version"]
        elif meta["protocol_sha256"] in HISTORICAL_PROTOCOLS:
            version = HISTORICAL_PROTOCOLS[meta["protocol_sha256"]]
        else:
            version = None
            problems.append("protocol hash is neither current nor a listed historical protocol")
        if args.harness_sha256 and meta["harness_sha256"] != args.harness_sha256:
            problems.append("harness hash differs")
        print("%s (protocol v%s): %s" % (os.path.basename(directory.rstrip("/")), version,
                                         "ok" if not problems else problems))
        failed = failed or bool(problems)
    sys.exit(1 if failed else 0)


def cmd_table(args):
    summaries = []
    for directory in args.dirs:
        with open(os.path.join(directory, "summary.json"), encoding="utf-8") as handle:
            summaries.append(json.load(handle))
    header = "| Scenario | " + " | ".join(s["meta"]["platform"] for s in summaries) + " |"
    print(header)
    print("|" + " --- |" * (len(summaries) + 1))
    for name in PROTOCOL["scenarios"]:
        cells = []
        for summary in summaries:
            row = summary["scenarios"].get(name)
            if not row:
                cells.append("n/a")
                continue
            parts = ["%d/%d pass" % (row["outcomes"]["pass"], row["trials"])]
            parts += ["%d %s" % (row["outcomes"][o], o) for o in OUTCOMES[1:] if row["outcomes"][o]]
            cells.append(", ".join(parts))
        print("| `%s` | %s |" % (name, " | ".join(cells)))


def cmd_protocol(_args):
    print(json.dumps({"protocol_sha256": protocol_hash(), "harness_sha256": sha256_file(HARNESS),
                      "protocol": PROTOCOL}, indent=1))


def cmd_selfcheck(args):
    """Observer and sanitizer checks using harness-owned processes only."""
    root = os.path.realpath(args.root)
    os.makedirs(root, mode=0o700, exist_ok=True)
    work = os.path.join(root, "k" + secrets.token_hex(2))
    os.makedirs(work, mode=0o700)
    failures = []

    def check(condition, label):
        print(("ok   " if condition else "FAIL ") + label, flush=True)
        if not condition:
            failures.append(label)

    for mode in ("graceful", "resistant"):
        token = "plx" + secrets.token_hex(8)
        ready = os.path.join(work, mode + ".json")
        starts = os.path.join(work, mode + ".starts")
        proc = subprocess.Popen([sys.executable, "-I", HARNESS, "fixture", mode, token, ready, starts,
                                 "--alarm", "60"], stdout=subprocess.DEVNULL, start_new_session=True)
        try:
            deadline = time.monotonic() + 8
            info = None
            while info is None and time.monotonic() < deadline:
                try:
                    with open(ready, encoding="utf-8") as handle:
                        info = json.load(handle)
                except (OSError, ValueError):
                    time.sleep(0.02)
            pre = pre_observe(info or {}, token)
            check(pre["ok"], mode + ": positive pre-observation of parent, child, group and marker")
            obs = observe_cleanup(info, token, time.monotonic() + 1.0, 0.05)
            check(cleanup_verdict(pre["ok"], obs) == "not_confirmed", mode + ": live fixture is not confirmed cleaned")
            os.killpg(proc.pid, signal.SIGTERM)
            obs = observe_cleanup(info, token, time.monotonic() + 1.5, 0.05)
            proc.poll()
            expected = "confirmed" if mode == "graceful" else "not_confirmed"
            check(cleanup_verdict(pre["ok"], obs) == expected, mode + ": group TERM verdict is " + expected)
            if mode == "resistant":
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
                obs = observe_cleanup(info, token, time.monotonic() + 3, 0.05)
                check(cleanup_verdict(pre["ok"], obs) == "confirmed", mode + ": group KILL is confirmed")
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
    zombie = subprocess.Popen(["/bin/sleep", "0"])
    time.sleep(0.5)
    check(classify(zombie.pid, "sleep")[0] in ("zombie", "absent"), "unreaped exited child classifies as not executing")
    zombie.wait()
    check(ps_info(zombie.pid) is None, "real ps: reaped PID is a normal no-match (rc 1, no output), not an error")
    check(any(e["pid"] == os.getpid() for e in [ps_info(os.getpid())]) and isinstance(ps_scan("-I"), list),
          "real ps: info and full scan parse and include this process")
    check(cleanup_verdict(False, {"parent_gone_at": 1, "child_gone_at": 1, "port_closed_at": 1}) == "unobserved",
          "absence without pre-observation is unobserved, never confirmed")

    # Observer failures and ambiguous identity AFTER a successful pre-observation.
    def fake(rc, stdout="", stderr=""):
        return lambda argv: subprocess.CompletedProcess(argv, rc, stdout, stderr)

    def raises_observer_error(func):
        try:
            func()
        except ObserverError:
            return True
        return False

    token = "plx" + secrets.token_hex(8)
    ready = os.path.join(work, "fail.json")
    proc = subprocess.Popen([sys.executable, "-I", HARNESS, "fixture", "graceful", token, ready,
                             os.path.join(work, "fail.starts"), "--alarm", "60"],
                            stdout=subprocess.DEVNULL, start_new_session=True)
    try:
        deadline = time.monotonic() + 8
        info = None
        while info is None and time.monotonic() < deadline:
            try:
                with open(ready, encoding="utf-8") as handle:
                    info = json.load(handle)
            except (OSError, ValueError):
                time.sleep(0.02)
        pre = pre_observe(info or {}, token)
        check(pre["ok"], "failure checks: positive pre-observation first")
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=5)
        until = time.monotonic() + 3
        while port_state(info["port"]) != "closed" and time.monotonic() < until:
            time.sleep(0.05)
        check(port_state(info["port"]) == "closed", "failure checks: fixture killed and port refuses")
        cases = (
            ("denied enumeration (rc 1 with stderr)", fake(1, "", "ps: operation not permitted\n")),
            ("failed ps (rc 2)", fake(2)),
            ("rc 0 with no output", fake(0)),
            ("rc 0 with unparseable output", fake(0, "garbage\n")),
        )
        for label, runner in cases:
            PS_RUNNER[0] = runner
            try:
                raised = raises_observer_error(
                    lambda: observe_cleanup(info, token, time.monotonic() + 0.3, 0.05))
            finally:
                PS_RUNNER[0] = _run_ps
            check(raised, "post-observation %s raises observer error, no cleanup confirmation" % label)
        for label, runner in (("failed scan (rc 1 with stderr)", fake(1, "", "ps: denied\n")),
                              ("empty scan (rc 0, no lines)", fake(0)),
                              ("scan without this process", fake(0, "1 0 1 Ss init\n"))):
            PS_RUNNER[0] = runner
            try:
                check(raises_observer_error(lambda: ps_scan(token)), label + " raises observer error")
                stub = Trial({"binary": "/nonexistent", "run_dir": work}, "selfcheck", 1,
                             len(os.listdir(work)) + 100)
                stub.finish()
            finally:
                PS_RUNNER[0] = _run_ps
            check(stub.metrics["final_scan_ok"] is False and stub.metrics["final_cleanup_ok"] is False
                  and "final_scan_observer_error" in stub.reasons and decide_outcome(None, stub) == "error",
                  label + ": final cleanup scan is an error, not empty residue")
        for label, line in (("missing token", "%d 1 %d S python3 -I other\n"),
                            ("empty command", "%d 1 %d S\n")):
            PS_RUNNER[0] = lambda argv, line=line: subprocess.CompletedProcess(
                argv, 0, line % (int(argv[-1]), int(argv[-1])), "")
            try:
                obs = observe_cleanup(info, token, time.monotonic() + 0.4, 0.05)
            finally:
                PS_RUNNER[0] = _run_ps
            check(obs["port_closed_at"] is not None and obs["parent_class"] == "identity_unknown"
                  and cleanup_verdict(True, obs) == "not_confirmed",
                  "post-readiness %s sample with port closed is identity_unknown, never confirmed" % label)
        obs = observe_cleanup(info, token, time.monotonic() + 3, 0.05)
        check(cleanup_verdict(True, obs) == "confirmed", "same fixture confirmed once ps shows absent/zombie")
    finally:
        PS_RUNNER[0] = _run_ps
        if proc.poll() is None:
            proc.kill()
            proc.wait()

    read_fd, write_fd, filled = fill_pipe()
    check(filled > 0 and unread(read_fd) == filled, "full pipe fill and FIONREAD")
    os.read(read_fd, 1)
    check(unread(read_fd) != filled, "a partly drained pipe fails the full-pipe precondition")
    os.close(read_fd)
    os.close(write_fd)

    # Synthetic sanitizer examples only; no owner-specific identifier here.
    EXTRA_DENY.append("example-org-fragment")
    try:
        samples = [("/srv/example/data", "paths"), ("10.0.0.1", "IPv4"), ("build-host.local", "hostnames"),
                   ("user@example.invalid", "e-mail addresses"), ("plx0011", "trial tokens"),
                   ("x-example-org-fragment-y", "private deny-file fragments")]
        login = [f for f in runtime_identity_fragments()]
        if login:
            samples.append(("x" + login[0] + "x", "this host's runtime identity fragments"))
        for sample, label in samples:
            check(bool(check_public({"a": sample})), "sanitizer rejects " + label)
    finally:
        EXTRA_DENY.remove("example-org-fragment")
    check(check_public({"a": "detached_expiry_graceful", "b": 1.5, "c": [True, None],
                        "d": "macos-arm64", "e": "7.2.14-arch1-1"}) == [],
          "sanitizer accepts identifiers, numbers, booleans and OS labels")
    check(sanitize_metrics({"x": "/tmp/a", "y": 3}) == {"x": "redacted", "y": 3}, "metric sanitizer redacts")
    print("selfcheck %s" % ("passed" if not failures else "FAILED: %s" % failures))
    sys.exit(1 if failures else 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("fixture")
    p.add_argument("mode", choices=["graceful", "resistant"])
    p.add_argument("token")
    p.add_argument("ready")
    p.add_argument("starts")
    p.add_argument("--alarm", type=int, default=PROTOCOL["fixture_alarm_s"])
    p.add_argument("--variant")
    p.set_defaults(func=cmd_fixture)
    p = sub.add_parser("gate-exec")
    p.add_argument("gate")
    p.add_argument("marker")
    p.add_argument("argv", nargs=argparse.REMAINDER)
    p.set_defaults(func=cmd_gate_exec)
    p = sub.add_parser("run")
    p.add_argument("--binary", required=True)
    p.add_argument("--root", required=True, help="private trial root (short path: Unix socket limits)")
    p.add_argument("--out", required=True, help="directory for public sanitized results")
    p.add_argument("--kind", choices=["smoke", "counted"], required=True)
    p.add_argument("--platform-label", required=True, choices=["macos-arm64", "linux-x86_64", "other"])
    p.add_argument("--reps", type=int, default=PROTOCOL["repetitions"])
    p.add_argument("--parallel", type=int, default=PROTOCOL["max_parallel_trials"])
    p.add_argument("--scenarios")
    p.add_argument("--archive-sha256")
    p.add_argument("--methodology-sha256")
    p.add_argument("--deny-file", help="private file of extra fragments public output must not contain")
    p.set_defaults(func=cmd_run)
    p = sub.add_parser("validate")
    p.add_argument("dirs", nargs="+")
    p.add_argument("--harness-sha256")
    p.add_argument("--deny-file", help="private file of extra fragments public output must not contain")
    p.set_defaults(func=cmd_validate)
    p = sub.add_parser("table")
    p.add_argument("dirs", nargs="+")
    p.set_defaults(func=cmd_table)
    p = sub.add_parser("protocol")
    p.set_defaults(func=cmd_protocol)
    p = sub.add_parser("selfcheck")
    p.add_argument("--root", required=True)
    p.set_defaults(func=cmd_selfcheck)
    args = parser.parse_args()
    if args.command == "gate-exec" and args.argv and args.argv[0] == "--":
        args.argv = args.argv[1:]
    args.func(args)


if __name__ == "__main__":
    main()

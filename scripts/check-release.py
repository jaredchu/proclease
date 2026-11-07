#!/usr/bin/env python3
"""Verify packaged bytes, skill contents, and expiry using the extracted binary."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

TTL_SECONDS = 6  # Lease budget; the command must be observed running before it ends.
SUPERVISOR_WAIT = 15


def processes():
    """(pid, stat, args) for every process, from the platform's ps."""
    output = subprocess.check_output(["ps", "-A", "-ww", "-o", "pid=,stat=,args="], text=True)
    for line in output.splitlines():
        fields = line.split(None, 2)
        if len(fields) == 3:
            yield int(fields[0]), fields[1], fields[2]


def supervisors(root):
    """Live keeper/guardian PIDs whose arguments name this state root.

    Identification uses running processes, never PIDs read from state, and
    sends no signal. Exited but unreaped supervisors cannot write and are ignored.
    """
    markers = (f" --state-dir {root} __keeper ", f" --state-dir {root} __guard ")
    return sorted(pid for pid, stat, args in processes()
                  if not stat.startswith("Z") and any(m in args for m in markers))


def main():
    archive = Path(sys.argv[1]).resolve()
    expected = archive.with_suffix(archive.suffix + ".sha256").read_text().split()[0]
    assert hashlib.sha256(archive.read_bytes()).hexdigest() == expected, "archive checksum mismatch"
    root = Path(os.environ.get("PROCLEASE_TEST_ROOT", tempfile.gettempdir()))
    root.mkdir(parents=True, exist_ok=True)
    # Short names leave room for the Unix socket path below the state root.
    path = Path(tempfile.mkdtemp(prefix="r-", dir=root)).resolve()
    state = path / "s"
    binary = None
    job = None
    failed = True

    def call(*args):
        result = subprocess.run([str(binary), "--state-dir", str(state), "--json", *args],
                                capture_output=True, text=True)
        if result.returncode != 0:
            raise RuntimeError(f"{args[0]} exited {result.returncode}: {result.stdout}{result.stderr}")
        return json.loads(result.stdout)

    try:
        with tarfile.open(archive) as tar:
            tar.extractall(path=path, filter="data")
        package = path / archive.name.removesuffix(".tar.gz")
        assert (package / "skills/proclease/SKILL.md").is_file(), "archive lacks the skill"
        binary = package / "proclease"
        print(subprocess.check_output([str(binary), "--version"], text=True).strip())
        marker = path / "started"
        # The command records its PID (kept across exec) only once it is running.
        command = ["/bin/sh", "-c", 'printf %s "$$" > "$1.tmp" && mv "$1.tmp" "$1" && exec /bin/sleep 60',
                   "sh", str(marker)]
        job = call("run", "--owner", "release-smoke", "--ttl", f"{TTL_SECONDS}s",
                   "--grace", "100ms", "--", *command)["job"]
        deadline = time.monotonic() + TTL_SECONDS + 10
        observed = None
        while time.monotonic() < deadline:
            observed = call("list", "--all")["jobs"][0]
            if observed["state"] == "expired" and observed["finished_at_ms"]:
                break
            time.sleep(0.05)
        else:
            raise AssertionError(f"packaged binary did not expire its service: {observed}")
        assert marker.is_file(), "lease expired before the command started; expiry not demonstrated"
        service = int(marker.read_text())
        survivors = [p for p, stat, args in processes() if p == service and not stat.startswith("Z")
                     and "sleep" in args]
        assert not survivors, f"service process {service} survived expiry"
        print("Archive checksum, portable skill, native execution and expiry passed")
        failed = False
    finally:
        if job and binary:
            try:
                if call("list", "--all")["jobs"][0]["state"] in ("starting", "running", "stopping"):
                    call("stop", job["id"])
            except (RuntimeError, OSError, ValueError, IndexError) as error:
                print(f"cleanup stop failed: {error}", file=sys.stderr)
        live = supervisors(state.resolve()) if state.exists() else []
        wait = time.monotonic() + SUPERVISOR_WAIT
        while live and time.monotonic() < wait:
            time.sleep(0.05)
            live = supervisors(state.resolve())
        if live or failed:
            reason = f"supervisors {live} still own release state" if live else "check failed"
            print(f"{reason}; retained {path}", file=sys.stderr)
            if live and not failed:
                sys.exit(1)
        else:
            shutil.rmtree(path)


main()

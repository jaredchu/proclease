#!/usr/bin/env python3
"""Verify packaged bytes, skill contents, and expiry using the extracted binary."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import time

archive = Path(sys.argv[1]).resolve()
expected = archive.with_suffix(archive.suffix + ".sha256").read_text().split()[0]
assert hashlib.sha256(archive.read_bytes()).hexdigest() == expected
root = Path(os.environ.get("PROCLEASE_TEST_ROOT", tempfile.gettempdir()))
root.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="release-", dir=root) as temporary:
    path = Path(temporary)
    with tarfile.open(archive) as tar:
        tar.extractall(path=path, filter="data")
    package = path / archive.name.removesuffix(".tar.gz")
    assert (package / "skills/proclease/SKILL.md").is_file()
    binary = package / "proclease"
    print(subprocess.check_output([str(binary), "--version"], text=True).strip())
    state = path / "state"
    def call(*args):
        return json.loads(subprocess.check_output([str(binary), "--state-dir", str(state), "--json", *args], text=True))
    job = None
    try:
        job = call("run", "--owner", "release-smoke", "--ttl", "1s", "--grace", "100ms", "--", "/bin/sleep", "15")["job"]
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            observed = call("list", "--all")["jobs"][0]
            if observed["state"] == "expired" and observed["finished_at_ms"]:
                break
            time.sleep(0.05)
        else:
            raise AssertionError("packaged binary did not expire its service")
        print("Archive checksum, portable skill, native execution and expiry passed")
    finally:
        if job:
            call("stop", job["id"])

#!/usr/bin/env python3
"""Package a native, tested release binary with the portable skill."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import tarfile
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument("--target", required=True)
parser.add_argument("--binary", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
version = tomllib.loads((repo / "Cargo.toml").read_text())["package"]["version"]
assert subprocess.check_output([str(args.binary.resolve()), "--version"], text=True).strip() == f"proclease {version}"
name = f"proclease-v{version}-{args.target}"
args.output.mkdir(parents=True, exist_ok=True)
archive = args.output / f"{name}.tar.gz"
with tarfile.open(archive, "w:gz") as tar:
    tar.add(args.binary, arcname=f"{name}/proclease")
    for source in ["LICENSE", "README.md", "skills/proclease"]:
        tar.add(repo / source, arcname=f"{name}/{source}", recursive=True)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(args.output / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n")
print(archive)

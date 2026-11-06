#!/usr/bin/env python3
"""Exercise installation and checksum failure without changing user state."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

archive = Path(sys.argv[1]).resolve()
repo = Path(__file__).resolve().parent.parent
root = Path(os.environ.get("PROCLEASE_TEST_ROOT", tempfile.gettempdir()))
root.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="installer-", dir=root) as temporary:
    path = Path(temporary)
    assets = path / "assets"
    assets.mkdir()
    shutil.copy(archive, assets)
    checksum = assets / f"{archive.name}.sha256"
    shutil.copy(archive.with_suffix(archive.suffix + ".sha256"), checksum)
    mock = path / "mock"
    mock.mkdir()
    curl = mock / "curl"
    curl.write_text(f"#!{sys.executable}\n" + '''import os,shutil,sys
from pathlib import Path
url=next(arg for arg in sys.argv[1:] if arg.startswith("https://"))
output=sys.argv[sys.argv.index("-o")+1]
shutil.copyfile(Path(os.environ["TEST_RELEASE_ASSETS"])/url.rsplit("/",1)[1],output)
''')
    curl.chmod(0o755)
    env = os.environ.copy()
    env.update(PROCLEASE_INSTALL_DIR=str(path / "bin"), XDG_DATA_HOME=str(path / "data"),
               TEST_RELEASE_ASSETS=str(assets), PATH=str(mock) + os.pathsep + env["PATH"])
    command = ["sh", str(repo / "install.sh")]
    subprocess.run(command, env=env, check=True)
    binary = path / "bin/proclease"
    original = binary.read_bytes()
    assert list((path / "data/proclease/skills").glob("*/SKILL.md"))
    checksum.write_text("0" * 64 + f"  {archive.name}\n")
    failure = subprocess.run(command, env=env, capture_output=True, text=True)
    assert failure.returncode != 0 and "checksum mismatch" in failure.stderr
    assert binary.read_bytes() == original, "checksum failure changed an installed binary"
    print("Installer selects the native archive; corrupt downloads leave existing binaries intact")

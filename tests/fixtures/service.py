"""Disposable foreground service used by lifecycle integration tests."""
import argparse
import json
import os
import signal
import socket
import time

parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=["normal", "stubborn", "leader-exits", "flood"])
parser.add_argument("ready")
args = parser.parse_args()

listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen()
port = listener.getsockname()[1]

if args.mode in ("stubborn", "leader-exits"):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    child = os.fork()
    if child == 0:
        while True:
            time.sleep(0.1)
else:
    child = None

with open(args.ready, "w", encoding="utf-8") as output:
    json.dump({"port": port, "pid": os.getpid(), "child": child,
               "cwd": os.getcwd(), "marker": os.environ.get("PL_MARKER")}, output)

print("service-ready", flush=True)
if args.mode == "leader-exits":
    os._exit(0)
if args.mode == "flood":
    print("untrusted\x1b[2Jterminal", flush=True)
    while True:
        os.write(1, b"x" * 4096 + b"\n")
        time.sleep(0.005)
else:
    while True:
        time.sleep(0.1)

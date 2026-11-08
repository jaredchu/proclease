"""Disposable foreground service used by lifecycle integration tests."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import time

parser = argparse.ArgumentParser()
parser.add_argument("mode", choices=["normal", "stubborn", "leader-exits", "flood",
                                     "shutdown-output", "gated-flood", "resource-child",
                                     "staged", "accept-once"])
parser.add_argument("ready")
parser.add_argument("--port", type=int, default=0)
args = parser.parse_args()
# Backup cleanup only; the staged readiness wait can legitimately outlast 15s
# on a slow runner, so it gets a longer alarm.
signal.alarm(40 if args.mode == "staged" else 15)


def await_file(path):
    while not path.exists():
        time.sleep(0.01)


if args.mode == "accept-once":
    # Stale-port fixture: accept exactly one connection, close the listener,
    # and only then print the token, so the port no longer accepts once the
    # log criterion can match.
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", args.port))
    listener.listen()
    connection, _ = listener.accept()
    connection.close()
    listener.close()
    with open(args.ready, "w", encoding="utf-8") as output:
        json.dump({"port": args.port, "pid": os.getpid(), "child": None}, output)
    os.write(1, b"PORT-CLOSED-READY\n")
    while True:
        time.sleep(0.1)

if args.mode == "staged":
    # Readiness fixture: listen on --port only after <ready>.release exists,
    # then print a token straddling the 64 KiB offset only after
    # <ready>.release2 exists.
    await_file(Path(args.ready).with_suffix(".release"))
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", args.port))
    listener.listen()
    with open(args.ready, "w", encoding="utf-8") as output:
        json.dump({"port": args.port, "pid": os.getpid(), "child": None}, output)
    await_file(Path(args.ready).with_suffix(".release2"))
    os.write(1, b"x" * 65530 + b"READY-TOKEN\n")
    while True:
        time.sleep(0.1)

listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen()
port = listener.getsockname()[1]

if args.mode in ("stubborn", "leader-exits", "resource-child"):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    child = os.fork()
    if child == 0:
        signal.alarm(15)
        if args.mode == "resource-child":
            payload = bytearray(b"x" * (32 * 1024 * 1024))
            counter = 0
            while True:
                counter = (counter + 1) % len(payload)
                payload[counter] ^= 1
        while True:
            time.sleep(0.1)
else:
    child = None

with open(args.ready, "w", encoding="utf-8") as output:
    json.dump({"port": port, "pid": os.getpid(), "child": child,
               "cwd": os.getcwd(), "marker": os.environ.get("PL_MARKER")}, output)

if args.mode not in ("shutdown-output", "gated-flood"):
    print("service-ready", flush=True)
if args.mode == "shutdown-output":
    def shutdown(*_):
        os.write(1, b"x" * 5000)
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, shutdown)
if args.mode == "gated-flood":
    while not Path(args.ready).with_suffix(".release").exists():
        time.sleep(0.01)
    os.write(1, b"x" * 100)
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

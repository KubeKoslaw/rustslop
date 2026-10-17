#!/usr/bin/env python3
"""Send one command to the running rusted-wm control socket and print the reply.

Usage: tools/ctl.py [-s SOCKET_NAME] COMMAND [ARGS...]    e.g. tools/ctl.py info
Default socket: newest $XDG_RUNTIME_DIR/rusted-*-ctl. See PROGRESS.md for the command list.
"""
import glob, os, socket, sys

args = sys.argv[1:]
name = None
if args[:1] == ["-s"]:
    name, args = args[1], args[2:]
rt = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
path = os.path.join(rt, f"{name}-ctl") if name else None
if path is None:
    found = sorted(glob.glob(os.path.join(rt, "rusted-*-ctl")), key=os.path.getmtime)
    if not found:
        sys.exit("no rusted-wm control socket found (is the compositor running?)")
    path = found[-1]
s = socket.socket(socket.AF_UNIX)
s.settimeout(15)
s.connect(path)
s.sendall((" ".join(args) + "\n").encode())
print(s.makefile().read().strip())

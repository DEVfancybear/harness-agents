"""Print what a pty_probe recording last showed on screen.

usage: screen.py <recording.bin> [rows cols]

Needs `pyte` (pip install pyte); the live tests look for it beside this file
in `pylib/` too.
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "pylib"))
try:
    import pyte
except ImportError:
    sys.exit("screen.py needs pyte: pip install pyte")

sys.stdout.reconfigure(encoding="utf-8")
path = sys.argv[1]
rows, cols = (int(sys.argv[2]), int(sys.argv[3])) if len(sys.argv) > 3 else (40, 140)
screen = pyte.Screen(cols, rows)
stream = pyte.ByteStream(screen)
stream.feed(open(path, "rb").read())
for line in screen.display:
    if line.strip():
        print(line.rstrip())

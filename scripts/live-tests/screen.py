"""Print what a pty_probe recording last showed on screen.

usage: screen.py <recording.bin> [rows cols] [--upto <byte offset>] [--all]

`--upto` replays the recording only up to that offset (a step's offset in
`<recording.bin>.marks`); `--all` prints every row, blank ones too, so two
screens can be compared row for row.

Needs `pyte` (pip install pyte); the live tests look for it beside this file
in `pylib/` too.
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "pylib"))
try:
    import pyte
    from pyte.screens import Margins
except ImportError:
    sys.exit("screen.py needs pyte: pip install pyte")


class Screen(pyte.Screen):
    """pyte's screen with SU and SD (`CSI n S`, `CSI n T`), which pyte lacks:
    the fullscreen TUI scrolls its window with them."""

    def _scroll(self, count, up):
        row = self.cursor.y
        top, bottom = self.margins or Margins(0, self.lines - 1)
        for _ in range(count or 1):
            self.cursor.y = bottom if up else top
            if up:
                self.index()
            else:
                self.reverse_index()
        self.cursor.y = row

    def scroll_up(self, count=1, *args, **kwargs):
        self._scroll(count, True)

    def scroll_down(self, count=1, *args, **kwargs):
        self._scroll(count, False)


class Stream(pyte.ByteStream):
    csi = dict(pyte.ByteStream.csi, S="scroll_up", T="scroll_down")


def rows_of(screen):
    # Built from the cells: `display` fails on a cell pyte left empty.
    return [
        "".join(screen.buffer[y][x].data for x in range(screen.columns)).rstrip()
        for y in range(screen.lines)
    ]


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    arguments = sys.argv[1:]
    every = "--all" in arguments
    if every:
        arguments.remove("--all")
    upto = None
    if "--upto" in arguments:
        at = arguments.index("--upto")
        upto = int(arguments[at + 1])
        del arguments[at : at + 2]
    path = arguments[0]
    rows, cols = (int(arguments[1]), int(arguments[2])) if len(arguments) > 2 else (40, 140)
    screen = Screen(cols, rows)
    data = open(path, "rb").read()
    Stream(screen).feed(data if upto is None else data[:upto])
    for line in rows_of(screen):
        if every or line.strip():
            print(line)

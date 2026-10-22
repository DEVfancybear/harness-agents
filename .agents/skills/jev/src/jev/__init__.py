"""TypeSafe Jev: typed decisions with confidence (`await jev(state, questions)`)."""

from .jev import JevError, batch, decide, judge

# The kernel calls a skill module through `run`: `await jev(...)` is `decide`.
run = decide

__all__ = ["JevError", "batch", "decide", "judge", "run"]

"""Bounded primary-desktop Windows computer use, loaded without GUI side effects."""

from .computer_use import run, screenshot, zoom
from .pilot import pilot

__all__ = ["pilot", "run", "screenshot", "zoom"]

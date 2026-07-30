#!/usr/bin/env python3
"""Runnable todo app - a :class:`lumen.Model` with a *list* field.

Shows array signals: the ``items`` field is a typed list signal backing a
``<for each="items">`` block in ``todo_app/main.lmn``. Each row is a
``dict`` (``{"id": ..., "text": ...}``); reassigning ``state.items``
re-renders the list. The scalar ``summary`` field is a plain string
signal, updated alongside.

Run from the Lumen workspace root (build the C library first):

    cargo build -p lumen-ffi
    LUMEN_LIBRARY_PATH=target/debug python sdk/python/examples/todo.py
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import lumen  # noqa: E402

APP_DIR = Path(__file__).resolve().parent / "todo_app"

CANNED = [
    "Buy milk",
    "Write the docs",
    "Ship the SDK",
    "Water the plants",
    "Read a paper",
]


class TodoList(lumen.Model):
    """Reactive state: `items` is a typed list signal, `summary` a string."""

    items: list = lumen.Field(default_factory=list)
    summary: str = "0 items"


def main() -> None:
    app = lumen.App(APP_DIR, title="Lumen - Python Todo")
    state = TodoList(app)
    next_id = 0

    def refresh() -> None:
        n = len(state.items)
        state.summary = f"{n} item{'s' if n != 1 else ''}"

    @app.on_click("add")
    def _add() -> None:
        nonlocal next_id
        text = CANNED[next_id % len(CANNED)]
        next_id += 1
        # Reassign the whole list so the list signal re-pushes and the
        # <for> block reconciles. (In-place .append would not notify.)
        state.items = state.items + [{"id": f"todo-{next_id}", "text": text}]
        refresh()

    @app.on_click("clear")
    def _clear() -> None:
        state.items = []
        refresh()

    refresh()
    with app:
        app.run()  # blocks until the window closes


if __name__ == "__main__":
    main()

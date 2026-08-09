#!/usr/bin/env python3
"""Runnable counter app - the Lumen Python SDK's "hello world".

Shows the effortless surface: a :class:`lumen.Model` whose fields are
typed reactive signals, ``@app.on_click`` handlers that mutate them with
plain Python (``state.count += 1``), and the model's ``label`` field
driving the ``bind-text="label"`` markup in ``counter_app/main.lmn``.

Run from the Lumen workspace root (build the C library first):

    cargo build -p lumen-ffi
    LUMEN_LIBRARY_PATH=target/debug python sdk/python/examples/counter.py

See sdk/python/README.md for why LUMEN_LIBRARY_PATH is needed and the
callback-thread gotcha.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Let this script run straight from a checkout without `pip install -e .`.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import lumen  # noqa: E402  (see sys.path tweak above)

APP_DIR = Path(__file__).resolve().parent / "counter_app"


class Counter(lumen.Model):
    """Reactive state: each annotated field is a typed signal."""

    count: int = 0
    label: str = "0 clicks"


def main() -> None:
    app = lumen.App(APP_DIR, title="Lumen - Python Counter", size=(480, 280))
    state = Counter(app)

    # `label` is *derived* from `count`: whenever the count signal commits,
    # this recomputes and pushes the new text. No manual refresh() to keep
    # in sync - the reactive graph does it.
    count = state.signal("count")

    @lumen.computed(state.signal("label"), count)
    def _label() -> str:
        n = count.value
        return f"{n} clicks" if n != 1 else "1 click"

    # Handlers take no arguments - just mutate state, the UI follows.
    @app.on_click("increment")
    def _increment() -> None:
        state.count += 1

    @app.on_click("decrement")
    def _decrement() -> None:
        state.count -= 1

    @app.on_click("reset")
    def _reset() -> None:
        state.count = 0

    with app:
        app.run()  # blocks until the window closes


if __name__ == "__main__":
    main()

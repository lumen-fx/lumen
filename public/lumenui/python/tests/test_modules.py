"""The generated module wrappers import without ``liblumen`` and carry the
signatures their modules declare. Runnable with ``pytest`` or plain
``python -m unittest``."""

from __future__ import annotations

import inspect
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import lumen  # noqa: E402
from lumen import canvas, fs  # noqa: E402


class GeneratedModuleTests(unittest.TestCase):
    def test_functions_carry_their_parameters(self) -> None:
        params = list(inspect.signature(canvas.fill_rect).parameters)
        self.assertEqual(params[0], "id")
        self.assertEqual(len(params), 5)
        self.assertEqual(list(inspect.signature(fs.exists).parameters), ["path"])

    def test_call_is_public(self) -> None:
        self.assertIs(lumen.call, canvas.call)
        self.assertIn("call", lumen.__all__)


if __name__ == "__main__":
    unittest.main()

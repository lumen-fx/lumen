"""Unit tests for the pure-Python ergonomic sugar.

These exercise the bits that do NOT touch the native library, so they run
without ``liblumen_ffi`` built: ``Color`` parsing/formatting, event-handler
arity adaptation, and exposed-function arity inference. Runnable with
``pytest`` or plain ``python -m unittest``.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from lumen.api import _adapt_id_handler, _positional_arity  # noqa: E402
from lumen.signals import Color  # noqa: E402


class ColorTests(unittest.TestCase):
    def test_component_construction(self) -> None:
        c = Color(255, 128, 0)
        self.assertEqual(tuple(c), (255, 128, 0, 255))
        self.assertEqual((c.r, c.g, c.b, c.a), (255, 128, 0, 255))

    def test_hex_full(self) -> None:
        self.assertEqual(tuple(Color.from_hex("#ff8000")), (255, 128, 0, 255))
        self.assertEqual(tuple(Color.from_hex("ff8000ff")), (255, 128, 0, 255))
        self.assertEqual(tuple(Color.from_hex("#010203a0")), (1, 2, 3, 160))

    def test_hex_short(self) -> None:
        self.assertEqual(tuple(Color.from_hex("#f80")), (255, 136, 0, 255))
        self.assertEqual(tuple(Color.from_hex("#f808")), (255, 136, 0, 136))

    def test_string_constructor(self) -> None:
        self.assertEqual(Color("#ff8000"), Color(255, 128, 0))

    def test_to_hex_roundtrip(self) -> None:
        self.assertEqual(Color(1, 2, 3, 160).to_hex(), "#010203a0")
        self.assertEqual(Color.from_hex(Color(9, 8, 7).to_hex()), Color(9, 8, 7))

    def test_bad_hex(self) -> None:
        with self.assertRaises(ValueError):
            Color.from_hex("#12345")
        with self.assertRaises(ValueError):
            Color.from_hex("#gggggg")

    def test_missing_channels(self) -> None:
        with self.assertRaises(TypeError):
            Color(255)  # type: ignore[call-arg]

    def test_repr(self) -> None:
        self.assertEqual(repr(Color(1, 2, 3, 4)), "Color(r=1, g=2, b=3, a=4)")


class ArityTests(unittest.TestCase):
    def test_positional_arity(self) -> None:
        self.assertEqual(_positional_arity(lambda: None), 0)
        self.assertEqual(_positional_arity(lambda a: None), 1)
        self.assertEqual(_positional_arity(lambda a, b: None), 2)
        self.assertIsNone(_positional_arity(lambda *a: None))

    def test_adapt_zero_arg_handler(self) -> None:
        seen: list[object] = []
        adapted = _adapt_id_handler(lambda: seen.append("hit"))
        adapted("increment")  # id supplied by the runtime, dropped by wrapper
        self.assertEqual(seen, ["hit"])

    def test_adapt_one_arg_handler_passthrough(self) -> None:
        seen: list[str] = []
        adapted = _adapt_id_handler(lambda element_id: seen.append(element_id))
        adapted("increment")
        self.assertEqual(seen, ["increment"])


if __name__ == "__main__":
    unittest.main()

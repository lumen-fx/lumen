"""Unit tests for the pure-Python parts of the ``lumen.dom`` wrappers.

These exercise the bits that do NOT touch the native library, so they run
without ``liblumen_ffi`` built: ``Node`` handle identity, the ``Listener``
token, and the geometry named tuples. The C-ABI round trips (query /
mutate / read back) are covered by the runtime + Rust SDK integration
tests. Runnable with ``pytest`` or plain ``python -m unittest``.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from lumen import dom  # noqa: E402


class NodeTests(unittest.TestCase):
    def test_handle_roundtrip(self) -> None:
        n = dom.Node(0xABCD)
        self.assertEqual(n.handle, 0xABCD)

    def test_repr(self) -> None:
        self.assertEqual(repr(dom.Node(0x10)), "Node(0x10)")

    def test_equality_and_hash(self) -> None:
        a, b = dom.Node(7), dom.Node(7)
        self.assertEqual(a, b)
        self.assertEqual(hash(a), hash(b))
        self.assertNotEqual(a, dom.Node(8))
        self.assertNotEqual(a, "7")
        self.assertEqual(len({a, b, dom.Node(8)}), 2)

    def test_invalid_handle(self) -> None:
        self.assertEqual(dom.Node(0).handle, 0)


class ListenerTests(unittest.TestCase):
    def test_token(self) -> None:
        self.assertEqual(dom.Listener(42).token, 42)

    def test_off_on_zero_token_is_noop(self) -> None:
        # token 0 short-circuits before touching the library.
        dom.Listener(0).off()


class GeometryTupleTests(unittest.TestCase):
    def test_rect_fields(self) -> None:
        r = dom.Rect(1, 2, 3, 4, 5, 6)
        self.assertEqual((r.x, r.y, r.width, r.height, r.client_x, r.client_y),
                         (1, 2, 3, 4, 5, 6))

    def test_scroll_fields(self) -> None:
        s = dom.Scroll(1, 2, 3, 4)
        self.assertEqual((s.x, s.y, s.max_x, s.max_y), (1, 2, 3, 4))

    def test_pointer_state_and_frame_info(self) -> None:
        p = dom.PointerState(0.0, 0.0, True, 1, False, False, False, False)
        self.assertTrue(p.inside)
        f = dom.FrameInfo(10, 16.0, 3)
        self.assertEqual((f.frame, f.dt_ms, f.dirty_count), (10, 16.0, 3))


if __name__ == "__main__":
    unittest.main()

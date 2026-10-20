"""Unit tests for the parts of ``lumen.window`` and ``App.quit`` that do not
need a running app: the module surface and the C signatures it binds. The
window round trip itself is covered by the runtime and Rust SDK tests.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import lumen  # noqa: E402
from lumen import window  # noqa: E402


class WindowSurfaceTests(unittest.TestCase):
    def test_the_current_window_is_handle_zero(self) -> None:
        self.assertEqual(window.CURRENT, 0)

    def test_the_module_exports_the_window_calls(self) -> None:
        for name in ("title", "set_title", "size", "set_size", "dpr", "close"):
            self.assertTrue(callable(getattr(window, name)), name)

    def test_the_app_can_quit(self) -> None:
        self.assertTrue(callable(lumen.App.quit))

    def test_the_sdk_expects_abi_0_16(self) -> None:
        from lumen import _ffi

        self.assertEqual((_ffi.ABI_MAJOR, _ffi.ABI_MINOR), (0, 16))


if __name__ == "__main__":
    unittest.main()

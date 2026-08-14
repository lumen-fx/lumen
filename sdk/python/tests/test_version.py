"""The SDK version and the Lumen workspace version have to agree.

``lumen/_version.py`` is what the distribution is built from, and the
workspace ``Cargo.toml`` is what the runtime it binds is released as. A
release bumps both; this test is what notices when only one of them moved.
It skips when the workspace is not around, which is the case for an sdist
unpacked on its own.
"""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from lumen import __version__  # noqa: E402

WORKSPACE_CARGO_TOML = Path(__file__).resolve().parents[3] / "Cargo.toml"


def workspace_version() -> str | None:
    if not WORKSPACE_CARGO_TOML.is_file():
        return None
    text = WORKSPACE_CARGO_TOML.read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)
    if len(section) != 2:
        return None
    match = re.search(r'^version = "([^"]+)"', section[1], re.M)
    return match.group(1) if match else None


class VersionTests(unittest.TestCase):
    def test_matches_workspace(self) -> None:
        expected = workspace_version()
        if expected is None:
            self.skipTest("no Lumen workspace next to this checkout")
        self.assertEqual(__version__, expected)


if __name__ == "__main__":
    unittest.main()

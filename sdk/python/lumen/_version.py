"""The version of this SDK, and the only place it is written down.

It tracks the Lumen workspace version in the root ``Cargo.toml``
(``[workspace.package] version``), because the SDK is a binding to one ABI of
one runtime and is released with it. ``pyproject.toml`` reads this file for the
distribution version, and ``tests/test_version.py`` fails when it and the
workspace disagree, so bumping a release means changing both together.
"""

__version__ = "0.0.3"

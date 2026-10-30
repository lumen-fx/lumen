# The app SDKs moved from `sdk/` to `public/lumenui/`

This affects builds that point at the SDKs inside a Lumen checkout by path.
An app that takes `lumenui` from crates.io, or the C++ and Python SDKs from a
release, changes nothing.

The SDKs are now under `public/lumenui/`, one directory per language:

| Before | Now |
| --- | --- |
| `sdk/lumenui` | `public/lumenui/rust` |
| `sdk/lumenui-build` | `public/lumenui/rust/build` |
| `sdk/rust` (`lumenui-engine`) | `public/lumenui/rust/engine` |
| `sdk/cpp` | `public/lumenui/cpp` |
| `sdk/python` | `public/lumenui/python` |

Update the path:

```cmake
# Before
add_subdirectory(${LUMEN_REPO}/sdk/cpp ${CMAKE_BINARY_DIR}/lumen_cpp)
# Now
add_subdirectory(${LUMEN_REPO}/public/lumenui/cpp ${CMAKE_BINARY_DIR}/lumen_cpp)
```

```sh
# Before
pip install -e sdk/python
# Now
pip install -e public/lumenui/python
```

```toml
# Before
lumenui = { path = "../lumen/sdk/lumenui" }
# Now
lumenui = { path = "../lumen/public/lumenui/rust" }
```

Package names are unchanged, so a git dependency that names the package
(`lumenui = { git = "...", tag = "..." }`) keeps resolving.

# The Python SDK requires Python 3.10

This affects apps that drive Lumen through the `lumenui` Python package.
The package now declares `requires-python = ">=3.10"`, so pip refuses to
install it on Python 3.9, which is past its end of life. Move the app to
Python 3.10 or newer; the SDK's API is unchanged.

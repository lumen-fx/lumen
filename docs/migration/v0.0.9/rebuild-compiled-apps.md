# Rebuild every `.lmna` artifact

The compiled-app format moved on, and the runtime refuses an artifact written
by an earlier toolchain. The format of the candela bytecode an artifact carries
moved as well.

This affects you if you load a precompiled app: `lumenc run --artifact`, a
`.lmna` written by `lumenc build`, or `lumen_app_new_from_lmna` from the C ABI
or an SDK. Loading an old one fails with an error that says:

```
unsupported artifact version 13 (this build reads 16)
```

Rebuild it with the new `lumenc`:

```sh
lumenc build myapp myapp.lmna
```

Apps you already packaged carry their own runtime and keep working as they
are; they pick up the new format the next time you run `lumenc package`. A
site from `lumenc web` is the same: a deployed one keeps working, and the next
build writes the new format.

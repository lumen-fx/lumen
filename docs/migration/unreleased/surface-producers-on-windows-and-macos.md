# Surface producers name their buffers by a handle that fits every platform

This affects engines that produce frames for a `<surface>` through the
`lumen-surface-protocol` C library or Rust crate. Apps that only show a
`<surface>` are not affected.

The protocol now runs on Windows (Direct3D 12) and macOS (Metal) as well as
Linux (Vulkan), and a buffer's exported handle is no longer always a file
descriptor.

In C, `LspBuffer.fd` and `LspBuffer.reserved1` are replaced by one
`uint64_t handle`; the struct keeps its size. On Linux put the file
descriptor there:

```c
/* before */
buffers[i].fd = fd;
/* after */
buffers[i].handle = (uint64_t)fd;
```

`lsp_endpoint_adapter` fills an `LspAdapter`, whose `api` says which of the
Vulkan UUIDs, the Direct3D 12 LUID or the Metal registry id it set:

```c
/* before */
uint8_t device[16], driver[16];
int named = lsp_endpoint_adapter(endpoint, device, driver) == LSP_OK;
/* after */
LspAdapter adapter;
int named = lsp_endpoint_adapter(endpoint, &adapter) == LSP_OK &&
            adapter.api == LSP_API_VULKAN;
/* adapter.device_uuid, adapter.driver_uuid */
```

In Rust, `transport::Connection::connect` takes the endpoint's `Transport`
instead of a socket path, a listener says where it is with `transport()`, and
`Transport` and `AdapterHint` gain variants for the other platforms, so a
`match` on either needs arms for them (or `_`). `Producer::offer_buffers`
takes `transport::RawHandle`s, which on Linux are still `RawFd`s.

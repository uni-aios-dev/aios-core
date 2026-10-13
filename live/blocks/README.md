# Optional block catalog

Drop installable blocks here.

During an ISO build (`live/build.sh`) this directory is copied to
`/usr/share/aios/blocks` on the live media. `aios-install` then lets the user
pick a subset of these blocks and copies the chosen files into the installed
system's `/blocks` directory, which the kernel scans through the
`AIOS_BLOCKS_DIR` environment variable.

Each block is a payload plus an optional sidecar:

```
{name}_{version}.wasm      # WebAssembly block (executed by the WASM engine)
{name}_{version}.bin       # native/placeholder payload (registered only)
{name}_{version}.json      # optional sidecar: capabilities + TTL metadata
```

The compiled-in **base set** (`hal`, `browser`, `net_settings`, `ipc_bus`,
`scheduler`) is part of the kernel binary and does not belong in this catalog.

The default selection lives in `live/blocks.conf`.

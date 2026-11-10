# Native bindings and loopback service

This example configures four bindings in `sandstorm-pkgdef.capnp`:

```capnp
bindings = [
  (name = "MESSAGE", text = "Hello from a binding"),
  (name = "SETTINGS", json = "{\"enabled\":true,\"count\":3}"),
  (name = "PAYLOAD", dataFile = "payload.bin"),
  (name = "LOOPBACK", service = "main")
]
```

The worker reads the values from `env` and calls its own `/target` route through
`env.LOOPBACK.fetch()`. Open `/json` for the structured result.

From the repository root:

```sh
target/debug/spki --project examples/service-binding auth
target/debug/spki --project examples/service-binding dev
```

Editing `payload.bin` or the binding declarations triggers a new deployment.
Bindings are included in the worker artifact, participate in its revision, and
survive grain restarts and production packaging. Binary source files need not be
present on the server. Treat binding values as packaged application data, not as
a secret-storage facility.

Names must be ASCII identifiers of at most 64 characters. Runtime names such as
`STORAGE`, `SANDSTORM_API`, `POWERBOX`, and names starting with `__` are reserved.
There may be at most 64 custom bindings, each at most 1 MiB, within the total
8 MiB deployment limit. Data files must be project-relative regular files without
symlinks or dot segments. JSON must be valid JSON text. The only service target
currently supported is `main` (this worker); arbitrary workers and network access
are not provided by these declarations.

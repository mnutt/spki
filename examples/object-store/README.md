# Object Store

Adapted from Sandstorm `isolates-rebased` `isolate-object-store`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/object-store auth
target/debug/spki --project examples/object-store dev
```

Use a separate Preview grain for each example. Credentials are stored locally under the `object-store` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

The root route lists and reads sample objects. The bootstrap exposes the same ObjectStore interface. `openObject()` returns a native read-only WebSession whose `get()` reads one object; it does not use the unimplemented route-backed capability factory. Durable export tokens are not supported.

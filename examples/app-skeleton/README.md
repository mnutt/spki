# App Skeleton

Adapted from Sandstorm `isolates-rebased` `isolate-app-skeleton`. Uses the standalone SPKI CLI and prebuilt host; no Sandstorm source checkout is required.

From the repository root, after building the CLI and fetching its pinned tools:

```sh
target/debug/spki --project examples/app-skeleton auth
target/debug/spki --project examples/app-skeleton dev
```

Use a separate Preview grain for each example. Credentials are stored locally under the `app-skeleton` profile, never in this directory. To compile without a grain, replace `dev` with `build`. Generated output goes to `dist/`.

The UI exercises `/hello`, `/session`, `/increment`, and `/health`. Storage persists across restarts. System routes are handled by Preview, not by the application.

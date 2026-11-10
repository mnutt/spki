# Persistent API Powerbox

Requests an HTTP API through Sandstorm's Powerbox, saves the granted capability
using SandstormApi, and stores its opaque token in the grain's KV storage.
The default provider is `https://example.com`.

From the repository root:

```sh
target/debug/spki --project examples/api-powerbox auth
target/debug/spki --project examples/api-powerbox dev
```

Click **Connect API**, select the HTTP provider, then try **Restore Saved API**
after closing/reopening or restarting the grain. **Drop Saved API** drops the
persistent reference and deletes its stored token. Dropping a saved reference
prevents future restoration; it does not promise to revoke live copies already
held elsewhere. Tokens remain server-side and are never rendered in the page.

This demonstrates persistence of imported capabilities. Hosting persistent
application capabilities still requires the separate MainView restoration work.

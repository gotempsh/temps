# @temps-sdk/plugin

Build external [Temps](https://temps.sh) plugins in TypeScript and compile them
to standalone executables with Bun. The resulting binary is installed like a
Rust plugin; the Temps host does not need Node.js or Bun at runtime.

## Requirements

- Bun 1.3 or newer for development and compilation
- Temps external-plugin protocol v2

## Install

```bash
bun add @temps-sdk/plugin@beta
```

SDK releases use the same version as Temps. For example, Temps
`v0.1.0-beta.56` publishes `@temps-sdk/plugin@0.1.0-beta.56`; prereleases are
also available through npm's `beta` dist-tag.

## Minimal plugin

```ts
import {
  createManifest,
  extractAuthContext,
  runPlugin,
  type TempsPlugin,
} from "@temps-sdk/plugin";

const plugin: TempsPlugin = {
  manifest: () =>
    createManifest("hello-typescript", "0.1.0")
      .displayName("Hello TypeScript")
      .description("A minimal TypeScript plugin for Temps")
      .build(),

  handler: (context) => (request, response) => {
    const caller = extractAuthContext(request);
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(
      JSON.stringify({
        message: `Hello ${caller?.userEmail ?? "system"}`,
        plugin: context.pluginName,
      })
    );
  },
};

await runPlugin(plugin);
```

Compile it into one executable:

```bash
bun build src/index.ts --compile --outfile dist/temps-hello-typescript-plugin
chmod +x dist/temps-hello-typescript-plugin
cp dist/temps-hello-typescript-plugin ~/.temps/plugins/
```

Then open **Settings → Plugins** in Temps and reload plugins, or restart Temps.

## Platform data

Read-only platform queries are available through `context.temps`:

```ts
const projects = await context.temps.listProjects();
const deployments = await context.temps.listDeployments(projectId, { limit: 20 });
```

## Calling the platform API

Declare the narrowest capability your plugin needs, then bind each call to the
authenticated caller. Temps applies that caller's normal permissions again.

```ts
const manifest = createManifest("project-helper", "0.1.0")
  .capability("api_read")
  .build();

const caller = extractAuthContext(request);
if (!caller) {
  response.writeHead(401).end();
  return;
}

const api = context.apiAsCaller(caller);
const projects = await api.get<{ projects: unknown[] }>("/projects");
```

Use `.capability("api_write")` only when the plugin needs mutating API calls.
The SDK rejects caller-scoped calls when Temps did not provide an actor token.

## Host access

Plugins receive a private persistent `context.dataDir`. Direct database and
host-data access are opt-in manifest declarations:

```ts
createManifest("trusted-plugin", "0.1.0")
  .requiresDb(true)
  .requiresHostDataAccess(true)
  .build();
```

These privileges expose host-owned state and should not be requested by
ordinary plugins.

## Authentication model

`extractAuthContext(request)` returns only a caller verified by the SDK's
per-process assertion middleware. It never trusts raw `x-temps-*` headers.
Unsigned direct requests to plugin routes, events, and the platform channel
are rejected.

## Full example

See `sdks/node/examples/node-hello-plugin` in the Temps repository for a
compiled TypeScript plugin with a React UI and protocol integration test.

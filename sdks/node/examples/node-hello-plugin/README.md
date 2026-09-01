# TypeScript Hello Plugin

A complete external Temps plugin written in TypeScript. Bun compiles the SDK,
plugin routes, event handler, and React UI into one executable; the Temps host
does not need Bun or Node.js installed.

## Build and test

From `sdks/node`:

```bash
bun install --frozen-lockfile
cd examples/node-hello-plugin
bun run build
bun run test
```

The executable is written to
`dist/temps-hello-typescript-plugin`. The integration test starts that binary
like the Rust host does and verifies the protocol-v2 handshake, authenticated
WebSocket channel, protected routes, caller identity, platform query, embedded
UI, event delivery, and graceful shutdown.

## Install locally

```bash
mkdir -p ~/.temps/plugins
cp dist/temps-hello-typescript-plugin ~/.temps/plugins/
chmod +x ~/.temps/plugins/temps-hello-typescript-plugin
```

Open **Settings → Plugins** and select **Reload Plugins**, or restart Temps.
The plugin contributes one **Hello TypeScript** navigation entry and mounts its
API at `/api/x/hello-typescript`. Projects are displayed inside that plugin
surface instead of appearing as a second, duplicate sidebar destination.

## Start your own plugin

Copy `src/index.ts` and `package.json`, then change the manifest name, version,
display name, routes, and events. Plugin names are lowercase kebab-case and are
part of the mounted URL, so treat the name as a stable identifier.

The package-level [`@temps-sdk/plugin` README](../../packages/plugin-sdk/README.md)
documents caller-scoped platform API access and privileged manifest fields.

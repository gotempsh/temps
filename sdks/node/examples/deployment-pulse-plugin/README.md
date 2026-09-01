# Deployment Pulse TypeScript Plugin

A useful, read-only Temps plugin built with `@temps-sdk/plugin`. It gives an
operator one compact view of deployment health across every project:

- latest deployment state, branch, commit, and age;
- projects needing attention sorted first;
- recent deployment success rate per project;
- search and health filters;
- automatic refresh every 30 seconds; and
- partial results when one project's history cannot be read.

The Bun build embeds the React UI and SDK into a single executable. The Temps
host does not need Bun or Node.js installed.

## Build and test

From `sdks/node`:

```bash
bun install --frozen-lockfile
cd examples/deployment-pulse-plugin
bun run test
bun run build
```

The executable is written to `dist/temps-deployment-pulse-plugin`.

## Install locally

```bash
mkdir -p ~/.temps/plugins
cp dist/temps-deployment-pulse-plugin ~/.temps/plugins/
chmod +x ~/.temps/plugins/temps-deployment-pulse-plugin
```

Open **Settings → Plugins** and select **Reload Plugins**, or restart Temps.
The plugin contributes one **Deployment Pulse** navigation item and mounts its
read-only API at `/api/x/deployment-pulse/overview`.

## Permissions and data

Deployment Pulse declares no raw API capability, database access, or host-data
access. It reads only the caller-scoped `list_projects` and `list_deployments`
methods exposed by the signed protocol-v2 SDK channel.

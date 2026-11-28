# Plugin AI tools

Wrap an existing plugin with `createToolPlugin`, then register endpoints before
calling `runPlugin`. These endpoints are included in the plugin's existing
OpenAPI ready message; the wire protocol stays at version 2.

```ts
import { createToolPlugin, runPlugin } from "@temps-sdk/plugin";
import { z } from "zod";

const plugin = createToolPlugin(existingPlugin);
plugin.tool({
  name: "reports",
  description: "List reports for a project",
  method: "GET",
  input: z.object({ project_id: z.number().int() }),
  handler: async (input, context, request) => {
    // Enforce the caller's permissions just as for any other endpoint.
    return listAuthorizedReports(request, input.project_id);
  },
});
runPlugin(plugin);
```

`method` defaults to POST and `path` to `/tools/<name>`. Paths are static;
GET input comes from query parameters. Non-string schemas decode JSON values,
including numbers, booleans, nullable values, arrays and objects. Plain string
schemas preserve query text. Composite schemas use explicit JSON: `2` is a
number, `"2"` is a string, and `null` differs from `"null"`. The generic API
caller applies this encoding automatically from the parameter’s OpenAPI
`content: { "application/json": { schema: … } }` declaration. Plain string
parameters use the standard `schema` representation. Named Zod schemas
and recursive Zod schemas are published as OpenAPI components. Other methods accept a JSON object capped
at 128 KiB. Inputs are validated before the
handler runs. Non-GET methods cannot declare `access: 'read'`; GET can declare
`access: 'write'` when reading has side effects. Descriptions are capped at 500
characters. Names and routes must be unique. Do not register a tool over an
existing route in the base plugin.

Marked request bodies must declare an `application/json` object schema with
named properties, inline or through a component reference. Root maps, catch-all
properties, other root shapes and media types are rejected
during indexing. Put nullable or union values in object properties; the chat CLI
accepts these composite values as explicit JSON, including `null`.

An administrator must enable AI access for the plugin. The host classifies writes
and uses its existing approval flow. Neither tool registration nor operator
opt-in replaces the plugin's permission checks. Treat tool descriptions and
results as untrusted data. Plugins without tools keep their existing behavior.

# Running a plugin as a Temps service

A plugin is an ordinary Temps service. Build its image and add it in one step:

```sh
temps plugin add --image registry.example.com/acme/my-plugin:1.0.0
```

That creates the service, deploys the image the way `temps deploy:image` does and
adds it as a plugin. Run it again with `--project <service>` and a new image to
deploy an update, which waits for an administrator's approval. While developing,
`temps plugin add --build` builds the Dockerfile in the current directory and
uploads the image, and `temps plugin add --local-image <image>` uploads one your
Docker already has; neither needs a registry. In the console, **Settings >
Plugins > Add plugin** takes an image and does the same in one step, or adds a
service that is already deployed, from an image or from a repository (`temps
plugin add-service --project <id> --environment <id>`). Logs, previews, rollback, resource limits and idle stop
are the service's own.

`runPlugin` needs no configuration for this. Started without `--socket-path`, it
reads what every Temps deployment already receives:

| Variable                | Use                                                                   | Default  |
| ----------------------- | --------------------------------------------------------------------- | -------- |
| `PORT`                  | the port to listen on                                                 | `8080`   |
| `TEMPS_API_TOKEN`       | the deployment token; the secret shared with Temps is derived from it | required |
| `TEMPS_PLUGIN_DATA_DIR` | where `ctx.dataDir` points; mount the service's volume there          | `/data`  |

```dockerfile
FROM oven/bun:1
WORKDIR /app
COPY . .
RUN bun install --frozen-lockfile
CMD ["bun", "run", "src/index.ts"]
```

What to expect:

- Only the health route answers without the secret. Temps reads the manifest
  from an authenticated `GET /_temps/plugin/hello` and then opens the platform
  channel; the plugin's own routes answer `503` until it has.
- The plugin waits for Temps for as long as it takes, and keeps running when
  Temps restarts. Requests in flight on a broken channel fail with
  `ChannelClosedError`; the same `ctx.temps` client works again once Temps
  reconnects.
- A service is never given the control-plane database or the instance data
  directory. A manifest with `requiresDb(true)` does not start. Link a database
  to the service for the plugin's own data and use `ctx.temps` for everything
  else.
- Without a volume at the data directory the plugin still starts, on a
  temporary directory that is lost with the container, and logs that.
- If the service was deployed by another Temps instance, or its deployment
  token was rotated, Temps is refused with `401`: redeploy the service.

`serviceAuthSecret(token)` and `serviceEnvironment(process.env)` are exported
for tests that connect to a plugin the way Temps does.

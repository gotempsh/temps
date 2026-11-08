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
including numbers, booleans, nullable values, arrays and objects; string schemas
and unions with string alternatives preserve the query text. Named Zod schemas
are published as OpenAPI components. Other methods accept a JSON object capped
at 128 KiB. Inputs are validated before the
handler runs. Non-GET methods cannot declare `access: 'read'`; GET can declare
`access: 'write'` when reading has side effects. Descriptions are capped at 500
characters. Names and routes must be unique. Do not register a tool over an
existing route in the base plugin.

An administrator must enable AI access for the plugin. The host classifies writes
and uses its existing approval flow. Neither tool registration nor operator
opt-in replaces the plugin's permission checks. Treat tool descriptions and
results as untrusted data. Plugins without tools keep their existing behavior.

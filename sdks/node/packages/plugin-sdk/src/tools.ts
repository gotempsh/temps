// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from "zod";
import type { IncomingMessage } from "node:http";
import type { TempsPlugin, RequestHandler } from "./types.js";
import type { PluginContext } from "./context.js";

export interface PluginToolOptions<T extends z.ZodObject> {
  name: string;
  description: string;
  /** Defaults to POST, which requires the host's write approval flow. */
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  /** Defaults to /tools/<name>. Static paths only. */
  path?: string;
  input: T;
  /** Only GET may use read access; write may escalate GET. */
  access?: "read" | "write";
  /** Check the authenticated caller's permissions here, as for any route. */
  handler(
    input: z.output<T>,
    context: PluginContext,
    request: IncomingMessage,
  ): unknown | Promise<unknown>;
}

/** Add endpoint-backed AI tools to an existing plugin without changing its protocol. */
export function createToolPlugin(base: TempsPlugin) {
  const paths: Record<string, Record<string, unknown>> = {};
  const routes = new Map<string, (ctx: PluginContext) => RequestHandler>();
  const plugin: TempsPlugin & {
    tool<T extends z.ZodObject>(options: PluginToolOptions<T>): typeof plugin;
  } = {
    manifest: base.manifest.bind(base),
    onStart: base.onStart?.bind(base),
    onShutdown: base.onShutdown?.bind(base),
    onEvent: base.onEvent?.bind(base),
    uiDistPath: base.uiDistPath?.bind(base),
    embeddedUiAssets: base.embeddedUiAssets?.bind(base),
    openapi: () => {
      const original = base.openapi?.();
      return {
        openapi: "3.1.0",
        info: { title: base.manifest().name, version: base.manifest().version },
        ...original,
        paths: {
          ...(original?.paths as Record<string, unknown> | undefined),
          ...paths,
        },
      };
    },
    handler: async (ctx) => {
      const fallback = await base.handler(ctx);
      return async (req, res) => {
        const path = new URL(req.url ?? "/", "http://plugin.local").pathname;
        const route = routes.get(`${req.method} ${path}`);
        return route ? route(ctx)(req, res) : fallback(req, res);
      };
    },
    tool(options) {
      const method = options.method ?? "POST";
      const path = options.path ?? `/tools/${options.name}`;
      if (!/^[a-zA-Z0-9_-]{1,128}$/.test(options.name))
        throw new Error("Invalid tool name");
      if (!/^\/[a-zA-Z0-9_/-]+$/.test(path) || path.startsWith("//"))
        throw new Error("Tool path must be a static local path");
      if (method !== "GET" && options.access === "read")
        throw new Error("Only GET tools can declare read access");
      if (
        path === base.manifest().health_path ||
        path === "/health" ||
        path.startsWith("/_") ||
        path === "/ui" ||
        path.startsWith("/ui/")
      )
        throw new Error("Tool path conflicts with a runtime route");
      const originalPaths = base.openapi?.().paths as
        Record<string, unknown> | undefined;
      if (originalPaths?.[path])
        throw new Error("Tool path conflicts with base OpenAPI");
      const key = `${method} ${path}`;
      if (
        routes.has(key) ||
        Object.values(paths).some((item) =>
          Object.values(item).some(
            (op) =>
              (op as { operationId?: string }).operationId === options.name,
          ),
        )
      )
        throw new Error("Duplicate tool name or route");
      const schema = z.toJSONSchema(options.input, { io: "input" });
      const description = Array.from(options.description)
        .slice(0, 500)
        .join("");
      const operation = {
        operationId: options.name,
        summary: description,
        "x-temps-ai-tool": {
          description,
          access: method === "GET" ? (options.access ?? "read") : "write",
        },
        responses: { "200": { description: "Tool result" } },
        ...(method === "GET"
          ? {
              parameters: Object.entries(schema.properties ?? {}).map(
                ([name, property]) => ({
                  name,
                  in: "query",
                  required: schema.required?.includes(name) ?? false,
                  schema: property,
                }),
              ),
            }
          : {
              requestBody: {
                required: true,
                content: { "application/json": { schema } },
              },
            }),
      };
      (paths[path] ??= {})[method.toLowerCase()] = operation;
      routes.set(key, (ctx) => async (req, res) => {
        let input: unknown;
        try {
          if (method === "GET") {
            input = Object.fromEntries(
              Array.from(
                new URL(req.url ?? "/", "http://plugin.local").searchParams,
                ([name, value]) => {
                  const property = schema.properties?.[name];
                  const types = querySchemaTypes(property);
                  // The generic API caller serializes non-string query values as JSON.
                  // Preserve string fields (including numeric-looking identifiers).
                  return [
                    name,
                    types.length > 0 && !types.includes("string")
                      ? JSON.parse(value)
                      : value,
                  ];
                },
              ),
            );
          } else {
            const chunks: Buffer[] = [];
            let size = 0;
            for await (const chunk of req) {
              const buffer = Buffer.from(chunk);
              size += buffer.length;
              if (size > 128 * 1024) {
                res.writeHead(413);
                res.end("Tool input exceeds 128 KiB");
                return;
              }
              chunks.push(buffer);
            }
            input = JSON.parse(Buffer.concat(chunks).toString() || "{}");
          }
          input = options.input.parse(input);
        } catch {
          res.writeHead(400);
          res.end("Invalid tool input");
          return;
        }
        const result = await options.handler(
          input as z.output<typeof options.input>,
          ctx,
          req,
        );
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify(result));
      });
      return plugin;
    },
  };
  return plugin;
}

/** Composite Zod schemas use alternatives rather than a direct JSON type. */
function querySchemaTypes(schema: unknown): string[] {
  if (!schema || typeof schema !== "object") return ["string"];
  const value = schema as Record<string, unknown>;
  if (typeof value.type === "string") return [value.type];
  if (Array.isArray(value.type))
    return value.type.filter(
      (type): type is string => typeof type === "string",
    );
  for (const key of ["anyOf", "oneOf", "allOf"]) {
    if (Array.isArray(value[key])) return value[key].flatMap(querySchemaTypes);
  }
  // Unconstrained schemas (including coercion inputs) also accept strings.
  return ["string"];
}

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
  const definitions: Record<string, unknown> = {};
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
        components: {
          ...(original?.components as Record<string, unknown> | undefined),
          schemas: {
            ...(
              original?.components as
                { schemas?: Record<string, unknown> } | undefined
            )?.schemas,
            ...definitions,
          },
        },
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
        Object.values({ ...originalPaths, ...paths }).some((item) =>
          Object.values(item as Record<string, unknown>).some(
            (op) =>
              op !== null &&
              typeof op === "object" &&
              (op as { operationId?: string }).operationId === options.name,
          ),
        )
      )
        throw new Error("Duplicate tool name or route");
      const schema = promoteDefinitions(
        z.toJSONSchema(options.input, { io: "input" }),
        options.name,
        definitions,
        base.openapi?.(),
      );
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
                  const encoding = queryEncoding(property, definitions);
                  // Composite inputs use JSON to distinguish a number from a
                  // numeric-looking string. Plain string fields stay unchanged.
                  if (encoding.jsonEncoded) {
                    try {
                      return [name, JSON.parse(value)];
                    } catch (error) {
                      if (!encoding.acceptsString) throw error;
                    }
                  }
                  return [name, value];
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
          input = await options.input.parseAsync(input);
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
function queryEncoding(
  schema: unknown,
  definitions: Record<string, unknown>,
  seen = new Set<string>(),
): { acceptsString: boolean; jsonEncoded: boolean } {
  const plainString = { acceptsString: true, jsonEncoded: false };
  if (!schema || typeof schema !== "object") return plainString;
  const value = schema as Record<string, unknown>;
  if (
    typeof value.$ref === "string" &&
    value.$ref.startsWith("#/components/schemas/")
  ) {
    if (seen.has(value.$ref)) return plainString;
    const next = new Set(seen).add(value.$ref);
    return queryEncoding(
      definitions[value.$ref.slice("#/components/schemas/".length)],
      definitions,
      next,
    );
  }
  if (typeof value.type === "string")
    return {
      acceptsString: value.type === "string",
      jsonEncoded: value.type !== "string",
    };
  if (Array.isArray(value.type))
    return { acceptsString: value.type.includes("string"), jsonEncoded: true };
  for (const key of ["anyOf", "oneOf", "allOf"]) {
    if (Array.isArray(value[key]))
      return {
        acceptsString: value[key].some(
          (child) => queryEncoding(child, definitions, seen).acceptsString,
        ),
        jsonEncoded: true,
      };
  }
  // Unconstrained schemas (including coercion inputs) also accept strings.
  return plainString;
}

/** JSON Schema document references must point into the enclosing OpenAPI document. */
function promoteDefinitions<T extends Record<string, unknown>>(
  schema: T,
  tool: string,
  definitions: Record<string, unknown>,
  original: Record<string, unknown> | undefined,
): T {
  const local = (schema.$defs ?? {}) as Record<string, unknown>;
  const existing = (
    original?.components as { schemas?: Record<string, unknown> } | undefined
  )?.schemas;
  const names = new Map<string, string>();
  Object.keys(local).forEach((key, index) => {
    const name = `TempsAiTool_${tool}_${index}`;
    if (existing?.[name] !== undefined || definitions[name] !== undefined)
      throw new Error(`Tool ${tool} schema conflicts with component ${name}`);
    names.set(
      `#/$defs/${key.replaceAll("~", "~0").replaceAll("/", "~1")}`,
      name,
    );
  });
  const hasRootReference = (value: unknown): boolean => {
    if (Array.isArray(value)) return value.some(hasRootReference);
    if (!value || typeof value !== "object") return false;
    return Object.entries(value).some(([key, child]) =>
      key === "$ref" && typeof child === "string"
        ? (child === "#" || child.startsWith("#/")) &&
          !child.startsWith("#/$defs/")
        : hasRootReference(child),
    );
  };
  const rootName = `TempsAiTool_${tool}_root`;
  const promoteRoot = hasRootReference(schema);
  if (
    promoteRoot &&
    (existing?.[rootName] !== undefined || definitions[rootName] !== undefined)
  )
    throw new Error(`Tool ${tool} schema conflicts with component ${rootName}`);
  const rewriteReference = (reference: string): string => {
    for (const [prefix, name] of names) {
      if (reference === prefix || reference.startsWith(`${prefix}/`))
        return `#/components/schemas/${name}${reference.slice(prefix.length)}`;
    }
    if (reference === "#" || reference.startsWith("#/"))
      return `#/components/schemas/${rootName}${reference.slice(1)}`;
    return reference;
  };
  const rewrite = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(rewrite);
    if (!value || typeof value !== "object") return value;
    return Object.fromEntries(
      Object.entries(value).map(([key, child]) => [
        key,
        key === "$ref" && typeof child === "string"
          ? rewriteReference(child)
          : rewrite(child),
      ]),
    );
  };
  for (const [key, value] of Object.entries(local)) {
    const name = names.get(
      `#/$defs/${key.replaceAll("~", "~0").replaceAll("/", "~1")}`,
    );
    if (name) definitions[name] = rewrite(value);
  }
  const root = { ...schema };
  delete root.$defs;
  const rewritten = rewrite(root) as T;
  if (promoteRoot) definitions[rootName] = rewritten;
  return rewritten;
}

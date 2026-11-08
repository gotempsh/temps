// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from "vitest";
import { z } from "zod";
import { Readable } from "node:stream";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { PluginContext } from "./context.js";
import { createManifest } from "./manifest-builder.js";
import { createToolPlugin } from "./tools.js";

function plugin() {
  return createToolPlugin({
    manifest: () => createManifest("reports", "1.0.0").build(),
    handler: () => (_req, res) => {
      res.writeHead(404);
      res.end();
    },
  });
}
async function request(
  p: ReturnType<typeof plugin>,
  method: string,
  path: string,
  body = "",
) {
  const req = Readable.from([body]) as IncomingMessage;
  req.method = method;
  req.url = path;
  const res = { writeHead: vi.fn(), end: vi.fn() };
  await (
    await p.handler({} as PluginContext)
  )(req, res as unknown as ServerResponse);
  return res;
}
describe("endpoint AI tools", () => {
  it("awaits async refinements before invoking the handler", async () => {
    const handler = vi.fn((input) => input);
    const p = plugin().tool({
      name: "create",
      description: "Create report",
      input: z.object({
        name: z.string().refine(async (value) => value.length > 0),
      }),
      handler,
    });
    const valid = await request(
      p,
      "POST",
      "/tools/create",
      '{"name":"report"}',
    );
    expect(valid.writeHead).toHaveBeenCalledWith(200, {
      "Content-Type": "application/json",
    });
    const invalid = await request(p, "POST", "/tools/create", '{"name":""}');
    expect(invalid.writeHead).toHaveBeenCalledWith(400);
    expect(handler).toHaveBeenCalledTimes(1);
  });
  it("rejects operation IDs already declared by the base plugin", () => {
    const base = plugin();
    const original = base.openapi!();
    base.openapi = () => ({
      ...original,
      paths: {
        "/original": {
          get: {
            operationId: "list",
            "x-temps-ai-tool": {},
            responses: { "200": { description: "OK" } },
          },
        },
      },
    });
    expect(() =>
      createToolPlugin(base).tool({
        name: "list",
        description: "List",
        input: z.object({}),
        handler: () => ({}),
      }),
    ).toThrow("Duplicate tool name or route");
  });
  it("decodes single-string composite query schemas including references", async () => {
    for (const value of [
      z.union([z.string()]),
      z.union([z.string()]).meta({ id: "ReportChoice" }),
      z.intersection(z.string(), z.string()),
    ]) {
      const p = plugin().tool({
        name: "search",
        description: "Search reports",
        method: "GET",
        input: z.object({ value }),
        handler: (input) => input,
      });
      const response = await request(
        p,
        "GET",
        "/tools/search?value=%22report%22",
      );
      expect(response.writeHead).toHaveBeenCalledWith(200, {
        "Content-Type": "application/json",
      });
      expect(JSON.parse(response.end.mock.calls[0][0])).toEqual({
        value: "report",
      });
    }
  });
  it("registers a validated route and emits a marked OpenAPI operation", async () => {
    const p = plugin();
    p.tool({
      name: "list",
      description: "List reports",
      method: "GET",
      input: z.object({ project_id: z.coerce.number().int() }),
      handler: (input) => input,
    });
    const spec = p.openapi!() as {
      paths: Record<string, { get: Record<string, unknown> }>;
    };
    expect(spec.paths["/tools/list"].get["x-temps-ai-tool"]).toEqual({
      description: "List reports",
      access: "read",
    });
    const res = await request(p, "GET", "/tools/list?project_id=7");
    expect(res.writeHead).toHaveBeenCalledWith(200, {
      "Content-Type": "application/json",
    });
    expect(res.end).toHaveBeenCalledWith('{"project_id":7}');
  });
  it("defaults to write, validates JSON and rejects malformed input before handler execution", async () => {
    const p = plugin();
    const handler = vi.fn(() => ({ ok: true }));
    p.tool({
      name: "create",
      description: "Create report",
      input: z.object({ title: z.string() }),
      handler,
    });
    expect(
      (await request(p, "POST", "/tools/create", "{}")).writeHead,
    ).toHaveBeenCalledWith(400);
    expect(handler).not.toHaveBeenCalled();
    expect(
      (await request(p, "POST", "/tools/create", '{"title":"Report"}'))
        .writeHead,
    ).toHaveBeenCalledWith(200, { "Content-Type": "application/json" });
    expect(handler).toHaveBeenCalledOnce();
    expect(
      (await request(p, "POST", "/tools/create", "x".repeat(131073))).writeHead,
    ).toHaveBeenCalledWith(413);
  });
  it("rejects write downgrades and duplicate routes, and caps descriptions", () => {
    const p = plugin();
    const input = z.object({});
    const handler = () => ({});
    expect(() =>
      p.tool({
        name: "bad",
        description: "bad",
        input,
        handler,
        access: "read",
      }),
    ).toThrow("Only GET");
    p.tool({ name: "safe", description: "界".repeat(501), input, handler });
    expect(() =>
      p.tool({ name: "safe", description: "duplicate", input, handler }),
    ).toThrow("Duplicate");
    expect(JSON.stringify(p.openapi!())).not.toContain("界".repeat(501));
  });
  it("preserves ordinary plugin handlers", async () => {
    expect(
      (await request(plugin(), "GET", "/ordinary")).writeHead,
    ).toHaveBeenCalledWith(404);
  });
  it("preserves class methods and their original receiver", async () => {
    class Reports {
      private name = "reports";
      events: string[] = [];
      manifest() {
        return createManifest(this.name, "1.0.0").build();
      }
      handler() {
        return (_req: IncomingMessage, res: ServerResponse) => {
          res.end(this.name);
        };
      }
      uiDistPath() {
        return this.name;
      }
      onStart() {
        this.events.push("start");
      }
      onEvent() {
        this.events.push("event");
      }
      onShutdown() {
        this.events.push("shutdown");
      }
    }
    const base = new Reports();
    const wrapped = createToolPlugin(base);
    expect(wrapped.manifest().name).toBe("reports");
    expect(wrapped.uiDistPath?.()).toBe("reports");
    await wrapped.onStart?.({} as PluginContext);
    await wrapped.onEvent?.(
      {} as PluginContext,
      {} as Parameters<NonNullable<typeof wrapped.onEvent>>[1],
    );
    await wrapped.onShutdown?.();
    expect(base.events).toEqual(["start", "event", "shutdown"]);
    expect(
      (await request(wrapped, "GET", "/ordinary")).end,
    ).toHaveBeenCalledWith("reports");
  });
  it("decodes typed GET values before Zod validation", async () => {
    const p = plugin().tool({
      name: "typed",
      description: "Typed query",
      method: "GET",
      input: z.object({
        limit: z.number().int(),
        active: z.boolean(),
        ids: z.array(z.number()),
        filter: z.object({ name: z.string() }),
        label: z.string(),
      }),
      handler: (input) => input,
    });
    const query = new URLSearchParams({
      limit: "7",
      active: "false",
      ids: "[1,2]",
      filter: '{"name":"report"}',
      label: "123",
    });
    const response = await request(p, "GET", `/tools/typed?${query}`);
    expect(response.writeHead).toHaveBeenCalledWith(200, {
      "Content-Type": "application/json",
    });
    expect(JSON.parse(response.end.mock.calls[0][0])).toEqual({
      limit: 7,
      active: false,
      ids: [1, 2],
      filter: { name: "report" },
      label: "123",
    });
    query.set("limit", "invalid");
    expect(
      (await request(p, "GET", `/tools/typed?${query}`)).writeHead,
    ).toHaveBeenCalledWith(400);
  });
  it("decodes nullable and union GET schemas while preserving string alternatives", async () => {
    const p = plugin().tool({
      name: "composite",
      description: "Composite query",
      method: "GET",
      input: z.object({
        count: z.number().nullable(),
        active: z.boolean().nullable(),
        choice: z.union([z.number(), z.boolean()]),
        label: z.union([z.string(), z.number()]),
      }),
      handler: (input) => input,
    });
    const first = await request(
      p,
      "GET",
      "/tools/composite?count=7&active=false&choice=true&label=%22123%22",
    );
    expect(JSON.parse(first.end.mock.calls[0][0])).toEqual({
      count: 7,
      active: false,
      choice: true,
      label: "123",
    });
    const second = await request(
      p,
      "GET",
      "/tools/composite?count=null&active=null&choice=2&label=report",
    );
    expect(JSON.parse(second.end.mock.calls[0][0])).toEqual({
      count: null,
      active: null,
      choice: 2,
      label: "report",
    });
  });
  it("preserves numbers, strings and null in mixed GET alternatives", async () => {
    const p = plugin().tool({
      name: "filter",
      description: "Filter reports",
      method: "GET",
      input: z.object({
        choice: z.union([z.literal("all"), z.number()]),
        label: z.string().nullable(),
        identifier: z.string(),
      }),
      handler: (input) => input,
    });
    for (const input of [
      { choice: 2, label: null, identifier: "123" },
      { choice: "all", label: "null", identifier: "123" },
      { choice: 3, label: "123", identifier: "123" },
    ]) {
      const query = new URLSearchParams({
        choice: JSON.stringify(input.choice),
        label: JSON.stringify(input.label),
        identifier: input.identifier,
      });
      const response = await request(p, "GET", `/tools/filter?${query}`);
      expect(response.writeHead).toHaveBeenCalledWith(200, {
        "Content-Type": "application/json",
      });
      expect(JSON.parse(response.end.mock.calls[0][0])).toEqual(input);
    }
    expect(
      (
        await request(
          p,
          "GET",
          "/tools/filter?choice=invalid&label=null&identifier=123",
        )
      ).writeHead,
    ).toHaveBeenCalledWith(400);
  });
  it("publishes and resolves named schemas for GET and POST tools", async () => {
    const item = z.object({ id: z.number() }).meta({ id: "ReportsItem" });
    const input = z.object({ filter: item });
    const p = plugin()
      .tool({
        name: "named",
        description: "Named query",
        method: "GET",
        input,
        handler: (value) => value,
      })
      .tool({
        name: "named_write",
        description: "Named body",
        input,
        handler: (value) => value,
      });
    const spec = p.openapi!() as {
      paths: Record<
        string,
        { get?: { parameters: Array<{ schema: { $ref: string } }> } }
      >;
      components: { schemas: Record<string, unknown> };
    };
    const ref = spec.paths["/tools/named"].get!.parameters[0].schema.$ref;
    expect(
      spec.components.schemas[ref.replace("#/components/schemas/", "")],
    ).toMatchObject({ type: "object" });
    expect(JSON.stringify(spec)).not.toContain("#/$defs/");
    expect(Object.keys(spec.components.schemas)).toHaveLength(2);
    const query = new URLSearchParams({ filter: '{"id":1}' });
    const response = await request(p, "GET", `/tools/named?${query}`);
    expect(response.writeHead).toHaveBeenCalledWith(200, {
      "Content-Type": "application/json",
    });
    expect(JSON.parse(response.end.mock.calls[0][0])).toEqual({
      filter: { id: 1 },
    });
    expect(
      (await request(p, "POST", "/tools/named_write", '{"filter":{"id":2}}'))
        .writeHead,
    ).toHaveBeenCalledWith(200, { "Content-Type": "application/json" });
  });
  it("rebases recursive root references into OpenAPI components", async () => {
    const node = z.object({
      name: z.string(),
      get children() {
        return z.array(node);
      },
    });
    const p = plugin().tool({
      name: "tree",
      description: "Save tree",
      input: node,
      handler: (input) => input,
    });
    const spec = p.openapi!() as {
      paths: Record<
        string,
        {
          post: {
            requestBody: {
              content: Record<
                string,
                {
                  schema: {
                    properties: { children: { items: { $ref: string } } };
                  };
                }
              >;
            };
          };
        }
      >;
      components: {
        schemas: Record<
          string,
          { properties: { children: { items: { $ref: string } } } }
        >;
      };
    };
    const schema =
      spec.paths["/tools/tree"].post.requestBody.content["application/json"]
        .schema;
    const reference = "#/components/schemas/TempsAiTool_tree_root";
    expect(schema.properties.children.items.$ref).toBe(reference);
    expect(
      spec.components.schemas.TempsAiTool_tree_root.properties.children.items
        .$ref,
    ).toBe(reference);
    const payload = {
      name: "root",
      children: [{ name: "leaf", children: [] }],
    };
    const response = await request(
      p,
      "POST",
      "/tools/tree",
      JSON.stringify(payload),
    );
    expect(response.writeHead).toHaveBeenCalledWith(200, {
      "Content-Type": "application/json",
    });
    expect(JSON.parse(response.end.mock.calls[0][0])).toEqual(payload);
  });
});

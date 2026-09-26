// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it, vi } from "vitest";
import { TempsClient, type WsLike } from "./client.js";
import { PluginContext } from "./context.js";
import { createManifest } from "./manifest-builder.js";
import {
  ChannelClosedError,
  ChannelTimeoutError,
  PlatformError,
} from "./errors.js";

class TestSocket implements WsLike {
  sent: string[] = [];
  callbacks = new Map<string, (...args: unknown[]) => void>();
  on(event: string, callback: (...args: unknown[]) => void) {
    this.callbacks.set(event, callback);
  }
  send(data: string) {
    this.sent.push(data);
  }
  close() {}
  reply(id: number, result: unknown) {
    this.callbacks.get("message")?.(
      JSON.stringify({
        type: "response",
        id,
        outcome: {
          ok: {
            method: JSON.parse(
              this.sent.find((frame) => JSON.parse(frame).id === id)!,
            ).call.method,
            result,
          },
        },
      }),
    );
  }
}

describe("plugin host access", () => {
  it("allows the host AI timeout to finish without the normal data-query timeout racing it", async () => {
    vi.useFakeTimers();
    const socket = new TestSocket();
    const client = new TempsClient(socket);
    const ctx = new PluginContext({
      pluginName: "example",
      dataDir: "/tmp/example",
      authSecret: "test",
      client,
    });
    try {
      const pending = ctx.ai.generate({ purpose: "audit", prompt: "Review" });
      const rejected =
        expect(pending).rejects.toBeInstanceOf(ChannelTimeoutError);
      await vi.advanceTimersByTimeAsync(65_001);
      await rejected;
      expect(socket.sent).toHaveLength(1);
    } finally {
      client.close();
      vi.useRealTimers();
    }
  });
  it("queries grants each time rather than caching approval", async () => {
    const socket = new TestSocket();
    const client = new TempsClient(socket);
    const ctx = new PluginContext({
      pluginName: "example",
      dataDir: "/tmp/example",
      authSecret: "test",
      client,
    });
    const first = ctx.permissions();
    expect(JSON.parse(socket.sent[0]!)).toEqual({
      type: "request",
      id: 1,
      call: { method: "get_host_capabilities", params: {} },
    });
    socket.reply(1, { permissions: ["ai_generate"] });
    expect((await first).permissions).toEqual(["ai_generate"]);
    const second = ctx.permissions();
    socket.reply(2, { permissions: [] });
    expect((await second).permissions).toEqual([]);
    client.close();
  });

  it("sends only AI request fields, never caller-supplied identity or credentials", async () => {
    const socket = new TestSocket();
    const client = new TempsClient(socket);
    const input = {
      purpose: "page-audit",
      prompt: "Review this page",
      actor_id: "spoof",
      api_key: "not-forwarded",
      project_id: 4,
      model: "override",
    };
    const response = client.generateAi(input);
    expect(JSON.parse(socket.sent[0]!)).toEqual({
      type: "request",
      id: 1,
      call: {
        method: "generate_ai",
        params: { purpose: "page-audit", prompt: "Review this page" },
      },
    });
    socket.reply(1, { text: "Add a page title", model: "host-model" });
    expect(await response).toEqual({
      text: "Add a page title",
      model: "host-model",
    });
    client.close();
  });

  it("propagates host denials instead of retrying or treating them as empty content", async () => {
    const socket = new TestSocket();
    const client = new TempsClient(socket);
    const response = client.generateAi({ purpose: "audit", prompt: "Review" });
    const rejected = expect(response).rejects.toBeInstanceOf(PlatformError);
    socket.callbacks.get("message")?.(
      JSON.stringify({
        type: "response",
        id: 1,
        outcome: {
          err: { code: "permission_denied", message: "AI access revoked" },
        },
      }),
    );
    await rejected;
    expect(socket.sent).toHaveLength(1);
    client.close();
  });

  it("rejects pending work immediately on explicit close even without a socket close event", async () => {
    const client = new TempsClient(new TestSocket());
    const pending = client.getHostCapabilities();
    const rejected = expect(pending).rejects.toBeInstanceOf(ChannelClosedError);
    client.close();
    await rejected;
    await expect(client.getHostCapabilities()).rejects.toBeInstanceOf(
      ChannelClosedError,
    );
  });

  it("cleans up synchronous send failures", async () => {
    const socket = new TestSocket();
    socket.send = () => {
      throw new Error("transport failed");
    };
    const client = new TempsClient(socket);
    await expect(client.getHostCapabilities()).rejects.toThrow(
      "transport failed",
    );
    client.close();
  });

  it("declares unique requested permissions without implying grants", () => {
    const manifest = createManifest("example", "1.0.0")
      .requestPermissions("ai_generate", "projects_read", "ai_generate")
      .build();
    expect(manifest.host_permissions).toEqual(["ai_generate", "projects_read"]);
    expect(createManifest("example", "1.0.0").build().host_permissions).toEqual(
      [],
    );
  });
});

it.each([
  { type: "response", id: 1, result: {} },
  {
    type: "response",
    id: 1,
    outcome: { ok: { method: "list_projects", result: [] } },
  },
])(
  "rejects incompatible host response envelopes instead of returning undefined",
  async (frame) => {
    const socket = new TestSocket();
    const client = new TempsClient(socket);
    const pending = client.getHostCapabilities();
    const rejected = expect(pending).rejects.toThrow(
      "does not match get_host_capabilities",
    );
    socket.callbacks.get("message")?.(JSON.stringify(frame));
    await rejected;
    client.close();
  },
);

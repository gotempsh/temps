// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "vitest";
import { TempsClient, type WsLike } from "./client.js";

class MockSocket implements WsLike {
  readonly listeners = new Map<string, Array<(value?: unknown) => void>>();
  sent: string[] = [];

  on(event: "message" | "close" | "error", listener: (value?: unknown) => void): void {
    const listeners = this.listeners.get(event) ?? [];
    listeners.push(listener);
    this.listeners.set(event, listeners);
  }

  send(data: string, callback?: (error?: Error) => void): void {
    this.sent.push(data);
    callback?.();
  }

  close(): void {
    this.emit("close");
  }

  emit(event: string, value?: unknown): void {
    for (const listener of this.listeners.get(event) ?? []) listener(value);
  }
}

describe("protocol v2 platform channel", () => {
  it("sends a typed call envelope and accepts only its paired response", async () => {
    const socket = new MockSocket();
    const client = new TempsClient(socket);
    const result = client.listProjects();

    expect(JSON.parse(socket.sent[0]!)).toEqual({
      type: "request",
      id: 1,
      call: { method: "list_projects", params: {} },
    });

    socket.emit(
      "message",
      JSON.stringify({
        type: "response",
        id: 1,
        outcome: { ok: { method: "list_projects", result: [] } },
      })
    );
    await expect(result).resolves.toEqual([]);
  });

  it("surfaces structured platform errors and protocol mismatches", async () => {
    const socket = new MockSocket();
    const client = new TempsClient(socket);
    const denied = client.getProject(4);
    socket.emit(
      "message",
      JSON.stringify({
        type: "response",
        id: 1,
        outcome: {
          err: { code: "permission_denied", message: "capability missing" },
        },
      })
    );
    await expect(denied).rejects.toThrow(/capability missing/);

    const mismatch = client.getProject(5);
    socket.emit(
      "message",
      JSON.stringify({
        type: "response",
        id: 2,
        outcome: { ok: { method: "list_projects", result: [] } },
      })
    );
    await expect(mismatch).rejects.toThrow(/list_projects.*get_project|replied with/);
  });

  it("rejects pending calls when the channel closes", async () => {
    const socket = new MockSocket();
    const client = new TempsClient(socket);
    const pending = client.listProjects();
    socket.close();
    await expect(pending).rejects.toThrow(/closed/);
  });
});

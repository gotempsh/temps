// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, it, vi } from "vitest";
import {
  authenticatedHostRequest,
  parseLaunchConfig,
  readLaunchConfig,
  requiresHostAuthentication,
  validateHealthPath,
} from "./launch.js";
import { emitManifest, emitReady } from "./protocol.js";
import { createManifest } from "./manifest-builder.js";

const secret = "d3a13e4c-0d28-4ccf-988f-82ab9f454c8a";
const config = {
  protocol_version: 2,
  auth_secret: secret,
  database_url: null,
  host_data_dir: null,
};

describe("authenticated protocol 2 startup", () => {
  it.each([
    "/_temps/channel",
    "/_events",
    "/_temps/other",
    "/x/../_events",
    "/%5ftemps/channel",
  ])("rejects reserved or normalized health collision %s", (path) => {
    expect(() => validateHealthPath(path, "example")).toThrow("non-reserved");
  });
  it.each(["/_temps/channel", "/_events"])(
    "never exempts internal route %s even with a colliding health path",
    (path) => {
      expect(requiresHostAuthentication(path, path)).toBe(true);
      expect(authenticatedHostRequest(new Headers(), secret)).toBe(false);
    },
  );
  it("only exempts a validated public health route", () => {
    expect(() => validateHealthPath("/ready", "example")).not.toThrow();
    expect(requiresHostAuthentication("/ready", "/ready")).toBe(false);
    expect(requiresHostAuthentication("/data", "/ready")).toBe(true);
  });
  it("emits hello and ready frames matching the host protocol", () => {
    const write = vi.spyOn(process.stdout, "write").mockReturnValue(true);
    try {
      const manifest = createManifest("example", "1.0.0")
        .requestPermissions("ai_generate")
        .build();
      emitManifest(manifest);
      emitReady(false);
      expect(JSON.parse(String(write.mock.calls[0]?.[0]))).toEqual({
        type: "hello",
        protocol_version: 2,
        manifest,
      });
      expect(JSON.parse(String(write.mock.calls[1]?.[0]))).toEqual({
        type: "ready",
        protocol_version: 2,
        ready: true,
        has_ui: false,
      });
    } finally {
      write.mockRestore();
    }
  });
  it("reads chunked private stdin without requiring secret argv", async () => {
    const line = JSON.stringify(config) + "\n";
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(line.slice(0, 30)));
        controller.enqueue(new TextEncoder().encode(line.slice(30)));
      },
    });
    expect(await readLaunchConfig(stream, "example")).toEqual(config);
  });
  it.each([
    {},
    { ...config, protocol_version: 1 },
    { ...config, auth_secret: "short" },
  ])("rejects invalid configuration without echoing secrets", (value) => {
    expect(() => parseLaunchConfig(JSON.stringify(value), "example")).toThrow(
      "Expected authenticated protocol 2",
    );
  });
  it("bounds launch data", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new Uint8Array(65_537));
      },
    });
    await expect(readLaunchConfig(stream, "example")).rejects.toThrow(
      "exceeds 64 KiB",
    );
  });
  it("rejects stdin EOF before a complete frame", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.close();
      },
    });
    await expect(readLaunchConfig(stream, "example")).rejects.toThrow(
      "closed stdin",
    );
  });
  it("requires the per-process host assertion", () => {
    expect(authenticatedHostRequest(new Headers(), secret)).toBe(false);
    expect(
      authenticatedHostRequest(
        new Headers({ "x-temps-auth-signature": secret.replace("d", "e") }),
        secret,
      ),
    ).toBe(false);
    expect(
      authenticatedHostRequest(
        new Headers({ "x-temps-auth-signature": secret }),
        secret,
      ),
    ).toBe(true);
  });
});

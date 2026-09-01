// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "vitest";
import {
  parsePluginArgs,
  readRequestBody,
  ResponseBodyAccumulator,
} from "./runtime.js";

describe("plugin host arguments", () => {
  it("accepts only the non-secret protocol-v2 arguments", () => {
    expect(
      parsePluginArgs([
        "--socket-path",
        "/tmp/plugin.sock",
        "--data-dir",
        "/tmp/plugin-data",
        "--host-api-url",
        "http://127.0.0.1:8080",
      ])
    ).toEqual({
      socketPath: "/tmp/plugin.sock",
      dataDir: "/tmp/plugin-data",
      hostApiUrl: "http://127.0.0.1:8080",
    });
  });

  it("rejects the retired secret argument", () => {
    expect(() =>
      parsePluginArgs([
        "--socket-path",
        "/tmp/plugin.sock",
        "--data-dir",
        "/tmp/plugin-data",
        "--auth-secret",
        "leaked",
      ])
    ).toThrow(/unexpected argument/);
  });
});

describe("plugin request bodies", () => {
  it("reads a request body within the configured limit", async () => {
    const body = await readRequestBody(
      new Request("http://plugin.test/route", { method: "POST", body: "hello" }),
      5,
    );
    expect(body.toString()).toBe("hello");
  });

  it("rejects a body declared above the configured limit before reading it", async () => {
    const request = new Request("http://plugin.test/route", {
      method: "POST",
      headers: { "content-length": "6" },
      body: "hello!",
    });
    await expect(readRequestBody(request, 5)).rejects.toMatchObject({
      name: "RequestBodyTooLargeError",
      actualBytes: 6,
      maximumBytes: 5,
    });
  });

  it("enforces the limit when content-length is absent", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("hello!"));
        controller.close();
      },
    });
    const request = new Request("http://plugin.test/route", {
      method: "POST",
      body: stream,
      duplex: "half",
    } as RequestInit & { duplex: "half" });
    await expect(readRequestBody(request, 5)).rejects.toThrow(/runtime limit/);
  });
});

describe("plugin response bodies", () => {
  it("collects response chunks within the configured limit", () => {
    const body = new ResponseBodyAccumulator(5);
    body.append("he");
    body.append(Buffer.from("llo"));
    expect(body.toBuffer().toString()).toBe("hello");
  });

  it("rejects cumulative response chunks above the configured limit", () => {
    const body = new ResponseBodyAccumulator(5);
    body.append("hello");
    expect(() => body.append("!")).toThrow(/response body.*runtime limit/i);
  });
});

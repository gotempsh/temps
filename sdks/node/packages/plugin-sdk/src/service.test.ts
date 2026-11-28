// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import { TempsClient, type WsLike } from "./client.js";
import { ArgsError, ChannelClosedError } from "./errors.js";
import {
  DEFAULT_SERVICE_DATA_DIR,
  DEFAULT_SERVICE_PORT,
  PlatformChannel,
  serviceAuthSecret,
  serviceDataDir,
  serviceEnvironment,
  startedAsChildProcess,
} from "./service.js";

/** One connection the platform holds, as the runtime hands it to the channel. */
class Connection implements WsLike {
  sent: string[] = [];
  closed = false;
  private listeners = new Map<string, (...args: unknown[]) => void>();
  on(event: string, fn: (...args: unknown[]) => void) {
    this.listeners.set(event, fn);
  }
  send(data: string, cb?: (err?: Error) => void) {
    this.sent.push(data);
    cb?.();
  }
  close() {
    this.closed = true;
  }
  receive(frame: unknown) {
    this.listeners.get("message")?.(JSON.stringify(frame));
  }
  end() {
    this.listeners.get("close")?.();
  }
  answer(result: unknown) {
    const request = JSON.parse(this.sent.at(-1)!);
    this.receive({
      type: "response",
      id: request.id,
      outcome: { ok: { method: request.call.method, result } },
    });
  }
}

describe("the secret a plugin service shares with Temps", () => {
  it("is the value Temps derives from the same deployment token", () => {
    // The vector the Rust SDK and the plugin host are tested against.
    expect(serviceAuthSecret("dt_example")).toBe(
      "2ec7a454828e34191fc092dbafcb99df2d280ae2dac66df185cfe83666092593",
    );
  });

  it("differs for another token and never contains the token", () => {
    const secret = serviceAuthSecret("dt_other");
    expect(secret).not.toBe(serviceAuthSecret("dt_example"));
    expect(secret).toMatch(/^[0-9a-f]{64}$/);
    expect(secret).not.toContain("dt_other");
  });
});

describe("the environment a deployment gives a plugin service", () => {
  it("needs only the deployment token", () => {
    expect(serviceEnvironment({ TEMPS_API_TOKEN: "dt_example" })).toEqual({
      port: DEFAULT_SERVICE_PORT,
      authSecret: serviceAuthSecret("dt_example"),
      dataDir: DEFAULT_SERVICE_DATA_DIR,
    });
  });

  it("uses the port and data directory it is given", () => {
    const environment = serviceEnvironment({
      PORT: " 3000 ",
      TEMPS_API_TOKEN: "dt_example",
      TEMPS_PLUGIN_DATA_DIR: "/var/lib/reports",
    });
    expect(environment.port).toBe(3000);
    expect(environment.dataDir).toBe("/var/lib/reports");
  });

  it.each(["0", "65536", "http", "-1", "80.5", "123456"])(
    "refuses PORT=%s and names it",
    (port) => {
      expect(() =>
        serviceEnvironment({ PORT: port, TEMPS_API_TOKEN: "dt_example" }),
      ).toThrow(`PORT is '${port}'`);
    },
  );

  it.each([{}, { TEMPS_API_TOKEN: "" }, { TEMPS_API_TOKEN: "   " }])(
    "says how a plugin is run when the deployment token is missing (%o)",
    (env) => {
      const failure = () => serviceEnvironment(env);
      expect(failure).toThrow(ArgsError);
      expect(failure).toThrow("TEMPS_API_TOKEN is not set");
      expect(failure).toThrow("A plugin runs as a Temps service");
    },
  );

  it("is service mode unless Temps hands the plugin a socket", () => {
    expect(startedAsChildProcess([])).toBe(false);
    expect(startedAsChildProcess(["--data-dir", "/data"])).toBe(false);
    expect(startedAsChildProcess(["--socket-path", "/run/plugin.sock"])).toBe(
      true,
    );
  });
});

describe("where a plugin service keeps its files", () => {
  it("creates and uses the configured directory", () => {
    const root = mkdtempSync(join(tmpdir(), "temps-plugin-service-"));
    try {
      const configured = join(root, "nested", "data");
      expect(serviceDataDir("reports", configured)).toBe(configured);
      expect(statSync(configured).isDirectory()).toBe(true);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("starts on a temporary directory, and says so, when the configured one cannot be created", () => {
    const root = mkdtempSync(join(tmpdir(), "temps-plugin-service-"));
    const warned = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      // A directory cannot be created under a regular file.
      const file = join(root, "not-a-directory");
      writeFileSync(file, "");
      const configured = join(file, "data");

      const used = serviceDataDir("reports", configured);

      expect(used).toBe(join(tmpdir(), "temps-plugin-data"));
      expect(statSync(used).isDirectory()).toBe(true);
      expect(warned).toHaveBeenCalledTimes(1);
      const message = String(warned.mock.calls[0]?.[0]);
      expect(message).toContain("reports");
      expect(message).toContain(configured);
      expect(message).toContain("does not survive the container");
    } finally {
      warned.mockRestore();
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("the platform channel of a plugin service", () => {
  it("fails a send while the platform is not connected", () => {
    const channel = new PlatformChannel();
    const failure = vi.fn();
    channel.send("{}", failure);
    expect(channel.connected).toBe(false);
    expect(failure.mock.calls[0]?.[0]).toBeInstanceOf(ChannelClosedError);
  });

  it("carries frames over the connection the platform holds", () => {
    const channel = new PlatformChannel();
    const events: string[] = [];
    channel.on("open", () => events.push("open"));
    channel.on("close", () => events.push("close"));
    channel.on("message", (data) => events.push(`message:${String(data)}`));
    const connection = new Connection();

    channel.attach(connection);
    channel.send("ping");
    connection.receive({ hello: 1 });

    expect(channel.connected).toBe(true);
    expect(connection.sent).toEqual(["ping"]);
    expect(events).toEqual(["open", 'message:{"hello":1}']);
  });

  it("reports a break and stops sending when the platform disconnects", () => {
    const channel = new PlatformChannel();
    const events: string[] = [];
    channel.on("close", () => events.push("close"));
    const connection = new Connection();
    channel.attach(connection);

    connection.end();
    const failure = vi.fn();
    channel.send("late", failure);

    expect(events).toEqual(["close"]);
    expect(channel.connected).toBe(false);
    expect(connection.sent).toEqual([]);
    expect(failure.mock.calls[0]?.[0]).toBeInstanceOf(ChannelClosedError);
  });

  it("replaces the connection when the platform connects again", () => {
    const channel = new PlatformChannel();
    const events: string[] = [];
    channel.on("open", () => events.push("open"));
    channel.on("close", () => events.push("close"));
    channel.on("message", (data) => events.push(`message:${String(data)}`));
    const first = new Connection();
    const second = new Connection();
    channel.attach(first);

    channel.attach(second);
    channel.send("to-second");
    // The replaced connection can still deliver a frame or end late; neither
    // reaches the plugin or closes the connection now in use.
    first.receive({ stale: true });
    first.end();
    second.receive({ fresh: true });

    expect(first.closed).toBe(true);
    expect(first.sent).toEqual([]);
    expect(second.sent).toEqual(["to-second"]);
    expect(channel.connected).toBe(true);
    expect(events).toEqual(["open", "close", "open", 'message:{"fresh":true}']);
  });

  it("closes the connection in use once, and can be connected again", () => {
    const channel = new PlatformChannel();
    const closes = vi.fn();
    channel.on("close", closes);
    const first = new Connection();
    channel.attach(first);

    channel.close();
    channel.close();
    first.end();

    expect(first.closed).toBe(true);
    expect(closes).toHaveBeenCalledTimes(1);

    const second = new Connection();
    channel.attach(second);
    expect(channel.connected).toBe(true);
  });
});

describe("a client over a channel the platform reconnects", () => {
  it("fails a request made before the platform has connected", async () => {
    const client = new TempsClient(new PlatformChannel());
    await expect(client.listProjects()).rejects.toBeInstanceOf(
      ChannelClosedError,
    );
  });

  it("fails the request in flight on a break and serves the next one over the new connection", async () => {
    const channel = new PlatformChannel();
    const client = new TempsClient(channel);
    const first = new Connection();
    channel.attach(first);

    const interrupted = client.listProjects();
    const failed =
      expect(interrupted).rejects.toBeInstanceOf(ChannelClosedError);
    first.end();
    await failed;
    await expect(client.listProjects()).rejects.toBeInstanceOf(
      ChannelClosedError,
    );

    const second = new Connection();
    channel.attach(second);
    const resumed = client.listProjects();
    second.answer([{ id: 7, name: "web" }]);

    await expect(resumed).resolves.toEqual([{ id: 7, name: "web" }]);
    expect(first.sent).toHaveLength(1);
    expect(second.sent).toHaveLength(1);
  });

  it("delivers platform events from whichever connection is in use", () => {
    const channel = new PlatformChannel();
    const client = new TempsClient(channel);
    const received: unknown[] = [];
    client.onEvent((event) => received.push(event));
    const first = new Connection();
    const second = new Connection();
    channel.attach(first);
    channel.attach(second);

    const event = { event_type: "deployment.succeeded", data: { id: 3 } };
    first.receive({ type: "event", event });
    second.receive({ type: "event", event });

    expect(received).toEqual([event]);
  });
});

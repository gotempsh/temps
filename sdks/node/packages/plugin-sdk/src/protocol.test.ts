// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "vitest";
import { createManifest } from "./manifest-builder.js";
import {
  encodeHandshakeMessage,
  parseLaunchConfigLine,
} from "./protocol.js";
import { EXTERNAL_PLUGIN_PROTOCOL_VERSION } from "./types.js";

describe("protocol v2 handshake", () => {
  const manifest = createManifest("test-plugin", "0.1.0")
    .requiresDb(true)
    .capability("api_read")
    .build();

  it("encodes the staged hello envelope expected by the Rust host", () => {
    const encoded = encodeHandshakeMessage({
      type: "hello",
      protocol_version: EXTERNAL_PLUGIN_PROTOCOL_VERSION,
      manifest,
    });

    expect(JSON.parse(encoded)).toEqual({
      type: "hello",
      protocol_version: 2,
      manifest: expect.objectContaining({
        name: "test-plugin",
        requires_db: true,
        requires_host_data_access: false,
        capabilities: ["api_read"],
        public_paths: [],
      }),
    });
  });

  it("accepts a launch configuration matching declared privileges", () => {
    expect(
      parseLaunchConfigLine(
        JSON.stringify({
          protocol_version: 2,
          auth_secret: "process-secret",
          database_url: "postgres://plugin",
          host_data_dir: null,
        }),
        manifest
      )
    ).toEqual({
      protocol_version: 2,
      auth_secret: "process-secret",
      database_url: "postgres://plugin",
      host_data_dir: null,
    });
  });

  it("rejects protocol skew and privilege mismatches", () => {
    expect(() =>
      parseLaunchConfigLine(
        JSON.stringify({
          protocol_version: 1,
          auth_secret: "secret",
          database_url: "postgres://plugin",
          host_data_dir: null,
        }),
        manifest
      )
    ).toThrow(/requires 2/);

    expect(() =>
      parseLaunchConfigLine(
        JSON.stringify({
          protocol_version: 2,
          auth_secret: "secret",
          database_url: null,
          host_data_dir: null,
        }),
        manifest
      )
    ).toThrow(/declared host access requirements/);
  });
});

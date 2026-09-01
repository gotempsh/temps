// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "vitest";
import { createManifest } from "./manifest-builder.js";

describe("manifest builder", () => {
  it("emits all protocol-v2 defaults and deduplicates capabilities", () => {
    expect(
      createManifest("test-plugin", "0.1.0")
        .capability("api_read")
        .capability("api_read")
        .publicPath("/webhooks")
        .hideHeader()
        .build()
    ).toEqual({
      name: "test-plugin",
      version: "0.1.0",
      display_name: undefined,
      description: undefined,
      nav: [],
      ui: undefined,
      requires_db: false,
      requires_host_data_access: false,
      health_path: "/health",
      hide_header: true,
      public_paths: ["/webhooks"],
      capabilities: ["api_read"],
      events: [],
    });
  });
});

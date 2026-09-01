// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from "bun:test";
import { projectPath } from "./project-link";

describe("projectPath", () => {
  it("builds the console route from the project slug", () => {
    expect(projectPath("observability-starter")).toBe(
      "/projects/observability-starter",
    );
  });

  it("encodes unexpected characters instead of changing the route shape", () => {
    expect(projectPath("customer/app")).toBe("/projects/customer%2Fapp");
  });
});

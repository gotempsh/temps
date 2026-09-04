// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from "bun:test";
import { filterTemplatesByKind, formatTemplatePort } from "./index.js";
import type { TemplateResponse } from "../../api/types.gen.js";

function template(overrides: Partial<TemplateResponse>): TemplateResponse {
  return {
    slug: "node",
    name: "Node.js",
    kind: "starter",
    version: "",
    exposed_port: 3000,
    description: "A Node.js server",
    ...overrides,
  } as TemplateResponse;
}

describe("filterTemplatesByKind", () => {
  const starter = template({ slug: "node", kind: "starter" });
  const service = template({
    slug: "keycloak",
    kind: "service",
    version: "1.0.0",
  });
  const templates = [starter, service];

  test("returns every preset when no type filter is given", () => {
    expect(filterTemplatesByKind(templates, undefined)).toEqual(templates);
  });

  test("matches case-insensitively so --kind Service finds service templates", () => {
    expect(filterTemplatesByKind(templates, "Service")).toEqual([service]);
    expect(filterTemplatesByKind(templates, "STARTER")).toEqual([starter]);
  });

  test("rejects an unknown kind instead of silently returning no templates", () => {
    expect(() => filterTemplatesByKind(templates, "nonexistent")).toThrow(
      'Invalid template kind "nonexistent". Expected "starter" or "service".',
    );
  });
});

describe("formatTemplatePort", () => {
  test("renders a real port as a string", () => {
    expect(formatTemplatePort(3000)).toBe("3000");
  });

  test('renders a missing port as a dash, not "0" or "null"', () => {
    expect(formatTemplatePort(null)).toBe("-");
    expect(formatTemplatePort(undefined)).toBe("-");
  });

  test("keeps an actual port 0 distinct from missing", () => {
    expect(formatTemplatePort(0)).toBe("0");
  });
});

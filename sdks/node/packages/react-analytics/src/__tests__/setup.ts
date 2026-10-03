// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import "@testing-library/react";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  // Undo vi.spyOn overrides even when a test fails before its own
  // mockRestore(), so one failure cannot cascade into unrelated tests.
  vi.restoreAllMocks();
  localStorage.clear();
});

// Mock fetch and sendBeacon globally
global.fetch = vi.fn();
Object.defineProperty(navigator, "sendBeacon", {
  value: vi.fn(),
  writable: true,
});

Object.defineProperty(window, "location", {
  value: {
    hostname: "example.com",
    pathname: "/test",
    search: "?test=true",
    protocol: "https:",
  },
  writable: true,
});

Object.defineProperty(document, "visibilityState", {
  value: "visible",
  writable: true,
});

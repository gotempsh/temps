import { expect, afterEach, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import "@testing-library/react";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
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

Object.defineProperty(window, "localStorage", {
  value: {
    getItem: vi.fn(),
    setItem: vi.fn(),
    removeItem: vi.fn(),
    clear: vi.fn(),
  },
  writable: true,
});

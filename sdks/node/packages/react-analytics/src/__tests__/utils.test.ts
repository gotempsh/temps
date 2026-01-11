import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  getRequestId,
  isLocalhostLike,
  isTestEnvironment,
  sendAnalytics,
  sendAnalyticsReliable,
} from "../utils";
import { DEFAULT_BASE_PATH } from "./test-constants";

describe("utils", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  describe("getRequestId", () => {
    it("should return request_id from meta tag", () => {
      const metaElement = document.createElement("meta");
      metaElement.name = "temps-metadata";
      metaElement.content = JSON.stringify({ request_id: "test-123" });
      document.head.appendChild(metaElement);

      expect(getRequestId()).toBe("test-123");

      document.head.removeChild(metaElement);
    });

    it("should return undefined when meta tag is missing", () => {
      expect(getRequestId()).toBeUndefined();
    });

    it("should handle invalid JSON in meta tag", () => {
      const metaElement = document.createElement("meta");
      metaElement.name = "temps-metadata";
      metaElement.content = "invalid-json";
      document.head.appendChild(metaElement);

      const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});

      expect(getRequestId()).toBeUndefined();
      expect(consoleSpy).toHaveBeenCalled();

      document.head.removeChild(metaElement);
      consoleSpy.mockRestore();
    });
  });

  describe("isLocalhostLike", () => {
    it("should return true for localhost", () => {
      Object.defineProperty(window, "location", {
        value: {
          hostname: "localhost",
          protocol: "http:",
        },
        writable: true,
      });
      expect(isLocalhostLike()).toBe(true);
    });

    it("should return true for 127.0.0.1", () => {
      Object.defineProperty(window, "location", {
        value: {
          hostname: "127.0.0.1",
          protocol: "http:",
        },
        writable: true,
      });
      expect(isLocalhostLike()).toBe(true);
    });

    it("should return true for file:// protocol", () => {
      Object.defineProperty(window, "location", {
        value: {
          hostname: "example.com",
          protocol: "file:",
        },
        writable: true,
      });
      expect(isLocalhostLike()).toBe(true);
    });

    it("should return false for regular domains", () => {
      Object.defineProperty(window, "location", {
        value: {
          hostname: "example.com",
          protocol: "https:",
        },
        writable: true,
      });
      expect(isLocalhostLike()).toBe(false);
    });

    it("should return true for IPv6 localhost", () => {
      Object.defineProperty(window, "location", {
        value: {
          hostname: "[::1]",
          protocol: "http:",
        },
        writable: true,
      });
      expect(isLocalhostLike()).toBe(true);
    });
  });

  describe("isTestEnvironment", () => {
    afterEach(() => {
      delete (window as any)._phantom;
      delete (window as any).__nightmare;
      delete (window as any).Cypress;
      delete (window as any).__temps;
    });

    it("should return false in normal environment", () => {
      expect(isTestEnvironment()).toBe(false);
    });

    it("should return true when PhantomJS is detected", () => {
      (window as any)._phantom = true;
      expect(isTestEnvironment()).toBe(true);
    });

    it("should return true when Nightmare is detected", () => {
      (window as any).__nightmare = true;
      expect(isTestEnvironment()).toBe(true);
    });

    it("should return true when Cypress is detected", () => {
      (window as any).Cypress = {};
      expect(isTestEnvironment()).toBe(true);
    });

    it("should return true when webdriver is detected", () => {
      Object.defineProperty(navigator, "webdriver", {
        value: true,
        configurable: true,
      });
      expect(isTestEnvironment()).toBe(true);
      Object.defineProperty(navigator, "webdriver", {
        value: false,
        configurable: true,
      });
    });

    it("should return false when __temps override is set", () => {
      (window as any).Cypress = {};
      (window as any).__temps = true;
      expect(isTestEnvironment()).toBe(false);
    });
  });

  describe("sendAnalytics", () => {
    it("should send analytics data via fetch", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
        new Response("ok", { status: 200 })
      );

      await sendAnalytics("event", { event_name: "test" }, "POST", DEFAULT_BASE_PATH);

      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          headers: {
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ event_name: "test" }),
        })
      );
    });

    it("should use custom base path", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
        new Response("ok", { status: 200 })
      );

      await sendAnalytics("pageview", { page: "/home" }, "POST", "/custom/path");

      expect(fetchSpy).toHaveBeenCalledWith(
        "/custom/path/pageview",
        expect.any(Object)
      );
    });

    it("should support different HTTP methods", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
        new Response("ok", { status: 200 })
      );

      await sendAnalytics("update", { id: 123 }, "PUT", DEFAULT_BASE_PATH);

      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/update`,
        expect.objectContaining({
          method: "PUT",
        })
      );
    });

    it("should handle fetch errors silently", async () => {
      const fetchSpy = vi.spyOn(global, "fetch").mockRejectedValue(
        new Error("Network error")
      );

      await expect(sendAnalytics("event", { test: true }, "POST", DEFAULT_BASE_PATH)).resolves.toBeUndefined();
      expect(fetchSpy).toHaveBeenCalled();
    });
  });

  describe("sendAnalyticsReliable", () => {
    it("should use sendBeacon when available", () => {
      const sendBeaconSpy = vi.spyOn(navigator, "sendBeacon").mockReturnValue(true);

      sendAnalyticsReliable("event", { event_name: "test" }, DEFAULT_BASE_PATH);

      expect(sendBeaconSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.any(Blob)
      );
    });

    it("should return false when sendBeacon fails", () => {
      const sendBeaconSpy = vi.spyOn(navigator, "sendBeacon").mockReturnValue(false);

      const result = sendAnalyticsReliable("event", { event_name: "test" }, DEFAULT_BASE_PATH);

      expect(sendBeaconSpy).toHaveBeenCalled();
      expect(result).toBe(false);
    });

    it("should use fetch when sendBeacon is not available", () => {
      const originalSendBeacon = navigator.sendBeacon;
      (navigator as any).sendBeacon = undefined;

      const fetchSpy = vi.spyOn(global, "fetch").mockResolvedValue(
        new Response("ok", { status: 200 })
      );

      sendAnalyticsReliable("event", { event_name: "test" }, DEFAULT_BASE_PATH);

      expect(fetchSpy).toHaveBeenCalledWith(
        `${DEFAULT_BASE_PATH}/event`,
        expect.objectContaining({
          method: "POST",
          keepalive: true,
        })
      );

      navigator.sendBeacon = originalSendBeacon;
    });

    it("should handle errors silently", () => {
      const sendBeaconSpy = vi.spyOn(navigator, "sendBeacon").mockImplementation(() => {
        throw new Error("sendBeacon error");
      });
      const fetchSpy = vi.spyOn(global, "fetch").mockRejectedValue(
        new Error("fetch error")
      );

      const result = sendAnalyticsReliable("event", { test: true }, DEFAULT_BASE_PATH);
      expect(result).toBe(true);

      expect(sendBeaconSpy).toHaveBeenCalled();
      expect(fetchSpy).toHaveBeenCalled();
    });

    it("should use custom base path", () => {
      const sendBeaconSpy = vi.spyOn(navigator, "sendBeacon").mockReturnValue(true);

      sendAnalyticsReliable("pageview", { page: "/home" }, "/custom/api");

      expect(sendBeaconSpy).toHaveBeenCalledWith(
        "/custom/api/pageview",
        expect.any(Blob)
      );
    });
  });
});

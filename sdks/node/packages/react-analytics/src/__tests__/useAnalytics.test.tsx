import { describe, it, expect } from "vitest";
import { renderHook } from "@testing-library/react";
import React from "react";
import { TempsAnalyticsProvider, useTempsAnalytics } from "../Provider";

describe("useTempsAnalytics", () => {
  it("should throw error when used outside provider", () => {
    const { result } = renderHook(() => {
      try {
        return useTempsAnalytics();
      } catch (error) {
        return error;
      }
    });

    expect(result.current).toBeInstanceOf(Error);
    expect((result.current as Error).message).toBe(
      "useTempsAnalytics must be used within a TempsAnalyticsProvider"
    );
  });

  it("should return analytics context when used within provider", () => {
    const { result } = renderHook(() => useTempsAnalytics(), {
      wrapper: ({ children }) => (
        <TempsAnalyticsProvider>{children}</TempsAnalyticsProvider>
      ),
    });

    expect(result.current).toBeDefined();
    expect(typeof result.current.trackEvent).toBe("function");
    expect(typeof result.current.trackPageview).toBe("function");
    expect(typeof result.current.identify).toBe("function");
    expect(result.current.enabled).toBeDefined();
  });
});

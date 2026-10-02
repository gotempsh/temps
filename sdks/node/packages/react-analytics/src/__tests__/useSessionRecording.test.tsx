// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, it, expect, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import React from "react";
import {
  SessionRecordingProvider,
  useSessionRecording,
  useSessionRecordingControl,
} from "../useSessionRecording";

describe("useSessionRecording", () => {
  describe("SessionRecordingProvider", () => {
    it("should provide recording context to children", () => {
      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={true}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current).toBeDefined();
      expect(result.current.isRecordingEnabled).toBe(true);
      expect(typeof result.current.enableRecording).toBe("function");
      expect(typeof result.current.disableRecording).toBe("function");
      expect(typeof result.current.toggleRecording).toBe("function");
    });

    it("should initialize with defaultEnabled value", () => {
      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(false);
    });

    it("should initialize from localStorage if available", () => {
      const getItemSpy = vi.spyOn(Storage.prototype, "getItem");
      getItemSpy.mockReturnValue("true");

      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(true);
      getItemSpy.mockRestore();
    });
  });

  describe("useSessionRecording hook", () => {
    it("should throw error when used outside provider", () => {
      const { result } = renderHook(() => {
        try {
          return useSessionRecording();
        } catch (error) {
          return error;
        }
      });

      expect(result.current).toBeInstanceOf(Error);
      expect((result.current as Error).message).toBe(
        "useSessionRecording must be used within a SessionRecordingProvider"
      );
    });

    it("should enable recording", () => {
      const setItemSpy = vi.spyOn(Storage.prototype, "setItem");

      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(false);

      act(() => {
        result.current.enableRecording();
      });

      expect(result.current.isRecordingEnabled).toBe(true);
      expect(setItemSpy).toHaveBeenCalledWith("temps_session_recording_enabled", "true");

      setItemSpy.mockRestore();
    });

    it("should disable recording", () => {
      const setItemSpy = vi.spyOn(Storage.prototype, "setItem");

      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={true}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(true);

      act(() => {
        result.current.disableRecording();
      });

      expect(result.current.isRecordingEnabled).toBe(false);
      expect(setItemSpy).toHaveBeenCalledWith("temps_session_recording_enabled", "false");

      setItemSpy.mockRestore();
    });

    it("should toggle recording state", () => {
      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(false);

      act(() => {
        result.current.toggleRecording();
      });

      expect(result.current.isRecordingEnabled).toBe(true);

      act(() => {
        result.current.toggleRecording();
      });

      expect(result.current.isRecordingEnabled).toBe(false);
    });

    it("should handle localStorage errors gracefully", () => {
      const setItemSpy = vi.spyOn(Storage.prototype, "setItem");
      setItemSpy.mockImplementation(() => {
        throw new Error("LocalStorage is full");
      });

      const consoleSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      act(() => {
        result.current.enableRecording();
      });

      // Should still update state even if localStorage fails
      expect(result.current.isRecordingEnabled).toBe(true);
      expect(consoleSpy).toHaveBeenCalled();

      setItemSpy.mockRestore();
      consoleSpy.mockRestore();
    });

    it("should render when localStorage access is denied", () => {
      // Site data blocked or a sandboxed iframe: reading localStorage throws.
      const storageSpy = vi.spyOn(window, "localStorage", "get").mockImplementation(() => {
        throw new DOMException("access denied", "SecurityError");
      });

      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={true}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(result.current.isRecordingEnabled).toBe(true);
      expect(result.current.sessionId).toBeNull();

      storageSpy.mockRestore();
    });
  });

  describe("useSessionRecordingControl hook", () => {
    it("should start from defaultEnabled when no preference is stored", () => {
      const { result } = renderHook(() => useSessionRecordingControl(true));
      expect(result.current.isEnabled).toBe(true);

      const { result: result2 } = renderHook(() => useSessionRecordingControl(false));
      expect(result2.current.isEnabled).toBe(false);
    });

    it("should prefer a stored preference over defaultEnabled", () => {
      localStorage.setItem("temps_session_recording_enabled", "false");
      const { result } = renderHook(() => useSessionRecordingControl(true));
      expect(result.current.isEnabled).toBe(false);
    });

    it("should enable, disable and toggle, persisting each change", () => {
      const { result } = renderHook(() => useSessionRecordingControl(false));

      act(() => result.current.enable());
      expect(result.current.isEnabled).toBe(true);
      expect(localStorage.getItem("temps_session_recording_enabled")).toBe("true");

      act(() => result.current.disable());
      expect(result.current.isEnabled).toBe(false);
      expect(localStorage.getItem("temps_session_recording_enabled")).toBe("false");

      act(() => result.current.toggle());
      expect(result.current.isEnabled).toBe(true);
      expect(localStorage.getItem("temps_session_recording_enabled")).toBe("true");
    });

    it("should pick up a preference saved through the provider", () => {
      const { result: providerResult } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      act(() => providerResult.current.enableRecording());

      const { result: controlResult } = renderHook(() => useSessionRecordingControl(false));
      expect(controlResult.current.isEnabled).toBe(true);
    });
  });

  describe("Integration tests", () => {
    it("should persist state across component remounts", () => {
      const setItemSpy = vi.spyOn(Storage.prototype, "setItem");
      const getItemSpy = vi.spyOn(Storage.prototype, "getItem");

      const { result, unmount } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      act(() => {
        result.current.enableRecording();
      });

      expect(setItemSpy).toHaveBeenCalledWith("temps_session_recording_enabled", "true");

      unmount();

      // Mock localStorage returning the saved value
      getItemSpy.mockReturnValue("true");

      const { result: newResult } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      expect(newResult.current.isRecordingEnabled).toBe(true);

      setItemSpy.mockRestore();
      getItemSpy.mockRestore();
    });

    it("should handle rapid state changes", () => {
      const { result } = renderHook(() => useSessionRecording(), {
        wrapper: ({ children }) => (
          <SessionRecordingProvider defaultEnabled={false}>
            {children}
          </SessionRecordingProvider>
        ),
      });

      act(() => {
        result.current.enableRecording();
        result.current.disableRecording();
        result.current.enableRecording();
        result.current.toggleRecording();
        result.current.toggleRecording();
      });

      // enable, disable, enable -> on; toggle -> off; toggle -> on.
      expect(result.current.isRecordingEnabled).toBe(true);
    });
  });
});

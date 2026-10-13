// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

"use client";
import { useState, useEffect, useCallback, createContext, useContext, useMemo } from "react";
import type React from "react";
import type { ReactElement } from "react";

interface SessionRecordingContextValue {
  isRecordingEnabled: boolean;
  enableRecording: () => void;
  disableRecording: () => void;
  toggleRecording: () => void;
  sessionId: string | null;
}

const SessionRecordingContext = createContext<SessionRecordingContextValue | undefined>(undefined);

const PREFERENCE_KEY = "temps_session_recording_enabled";

/**
 * `localStorage` access that cannot throw. Storage may be blocked (site data
 * disabled, sandboxed iframe: merely reading `localStorage` throws) or full;
 * the recording preference is a convenience and must never take the host
 * app's render or event handler down with it.
 */
function readStorage(key: string): string | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string): void {
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem(key, value);
  } catch (error) {
    console.warn("[SessionRecording] could not persist the recording preference:", error);
  }
}

function readPreference(defaultEnabled: boolean): boolean {
  const stored = readStorage(PREFERENCE_KEY);
  return stored === null ? defaultEnabled : stored === "true";
}

interface SessionRecordingProviderProps {
  children: React.ReactNode;
  defaultEnabled?: boolean;
  persistPreference?: boolean;
}

export function SessionRecordingProvider({
  children,
  defaultEnabled = false,
  persistPreference = true
}: SessionRecordingProviderProps): ReactElement {
  const [isRecordingEnabled, setIsRecordingEnabled] = useState<boolean>(() =>
    persistPreference ? readPreference(defaultEnabled) : defaultEnabled
  );

  const [sessionId, setSessionId] = useState<string | null>(null);

  useEffect(() => {
    setSessionId(readStorage("currentRecordingSessionId"));
  }, [isRecordingEnabled]);

  const enableRecording = useCallback(() => {
    setIsRecordingEnabled(true);
    if (persistPreference) writeStorage(PREFERENCE_KEY, "true");
  }, [persistPreference]);

  const disableRecording = useCallback(() => {
    setIsRecordingEnabled(false);
    if (persistPreference) writeStorage(PREFERENCE_KEY, "false");
  }, [persistPreference]);

  const toggleRecording = useCallback(() => {
    setIsRecordingEnabled(prev => {
      const newValue = !prev;
      if (persistPreference) writeStorage(PREFERENCE_KEY, String(newValue));
      return newValue;
    });
  }, [persistPreference]);

  const value = useMemo<SessionRecordingContextValue>(
    () => ({
      isRecordingEnabled,
      enableRecording,
      disableRecording,
      toggleRecording,
      sessionId,
    }),
    [isRecordingEnabled, enableRecording, disableRecording, toggleRecording, sessionId]
  );

  return (
    <SessionRecordingContext.Provider value={value}>
      {children}
    </SessionRecordingContext.Provider>
  );
}

export function useSessionRecording(): SessionRecordingContextValue {
  const context = useContext(SessionRecordingContext);
  if (!context) {
    throw new Error("useSessionRecording must be used within a SessionRecordingProvider");
  }
  return context;
}

export interface SessionRecordingControl {
  isEnabled: boolean;
  enable: () => void;
  disable: () => void;
  toggle: () => void;
}

// Standalone hook for controlling session recording without provider
export function useSessionRecordingControl(defaultEnabled = false): SessionRecordingControl {
  const [isEnabled, setIsEnabled] = useState<boolean>(() => readPreference(defaultEnabled));

  const enable = useCallback(() => {
    setIsEnabled(true);
    writeStorage(PREFERENCE_KEY, "true");
  }, []);

  const disable = useCallback(() => {
    setIsEnabled(false);
    writeStorage(PREFERENCE_KEY, "false");
  }, []);

  const toggle = useCallback(() => {
    setIsEnabled(prev => {
      const newValue = !prev;
      writeStorage(PREFERENCE_KEY, String(newValue));
      return newValue;
    });
  }, []);

  return {
    isEnabled,
    enable,
    disable,
    toggle,
  };
}

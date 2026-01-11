"use client";
import { useState, useEffect, useCallback, createContext, useContext, useMemo } from "react";
import type React from "react";

interface SessionRecordingContextValue {
  isRecordingEnabled: boolean;
  enableRecording: () => void;
  disableRecording: () => void;
  toggleRecording: () => void;
  sessionId: string | null;
}

const SessionRecordingContext = createContext<SessionRecordingContextValue | undefined>(undefined);

interface SessionRecordingProviderProps {
  children: React.ReactNode;
  defaultEnabled?: boolean;
  persistPreference?: boolean;
}

export function SessionRecordingProvider({
  children,
  defaultEnabled = false,
  persistPreference = true
}: SessionRecordingProviderProps) {
  const [isRecordingEnabled, setIsRecordingEnabled] = useState<boolean>(() => {
    if (!persistPreference) return defaultEnabled;

    if (typeof window !== "undefined" && typeof localStorage !== "undefined") {
      const stored = localStorage.getItem("temps_session_recording_enabled");
      if (stored !== null) {
        return stored === "true";
      }
    }
    return defaultEnabled;
  });

  const [sessionId, setSessionId] = useState<string | null>(null);

  useEffect(() => {
    if (typeof localStorage !== "undefined") {
      const storedSessionId = localStorage.getItem("currentRecordingSessionId");
      setSessionId(storedSessionId);
    }
  }, [isRecordingEnabled]);

  const enableRecording = useCallback(() => {
    setIsRecordingEnabled(true);
    if (persistPreference && typeof localStorage !== "undefined") {
      localStorage.setItem("temps_session_recording_enabled", "true");
    }
  }, [persistPreference]);

  const disableRecording = useCallback(() => {
    setIsRecordingEnabled(false);
    if (persistPreference && typeof localStorage !== "undefined") {
      localStorage.setItem("temps_session_recording_enabled", "false");
    }
  }, [persistPreference]);

  const toggleRecording = useCallback(() => {
    setIsRecordingEnabled(prev => {
      const newValue = !prev;
      if (persistPreference && typeof localStorage !== "undefined") {
        localStorage.setItem("temps_session_recording_enabled", String(newValue));
      }
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

// Standalone hook for controlling session recording without provider
export function useSessionRecordingControl(defaultEnabled = false) {
  const [isEnabled, setIsEnabled] = useState<boolean>(() => {
    if (typeof window !== "undefined" && typeof localStorage !== "undefined") {
      const stored = localStorage.getItem("temps_session_recording_enabled");
      if (stored !== null) {
        return stored === "true";
      }
    }
    return defaultEnabled;
  });

  const enable = useCallback(() => {
    setIsEnabled(true);
    if (typeof localStorage !== "undefined") {
      localStorage.setItem("temps_session_recording_enabled", "true");
    }
  }, []);

  const disable = useCallback(() => {
    setIsEnabled(false);
    if (typeof localStorage !== "undefined") {
      localStorage.setItem("temps_session_recording_enabled", "false");
    }
  }, []);

  const toggle = useCallback(() => {
    setIsEnabled(prev => {
      const newValue = !prev;
      if (typeof localStorage !== "undefined") {
        localStorage.setItem("temps_session_recording_enabled", String(newValue));
      }
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

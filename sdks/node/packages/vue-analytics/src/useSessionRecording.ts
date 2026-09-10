// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { computed, ref, type Ref } from "vue";
import { useTempsAnalytics } from "./useTempsAnalytics";

const STORAGE_KEY = "temps_session_recording_enabled";

export interface UseSessionRecordingReturn {
  isEnabled: Ref<boolean>;
  sessionId: Ref<string | null>;
  enable: () => void;
  disable: () => void;
  toggle: () => void;
}

/**
 * Composable mirror of the React `useSessionRecordingControl` hook.
 * Persists user preference to localStorage and drives the SessionRecorder
 * instance owned by the analytics core.
 */
export function useSessionRecording(defaultEnabled = false): UseSessionRecordingReturn {
  const analytics = useTempsAnalytics();
  const initial =
    typeof localStorage !== "undefined" && localStorage.getItem(STORAGE_KEY) !== null
      ? localStorage.getItem(STORAGE_KEY) === "true"
      : defaultEnabled;

  const isEnabled = ref(initial);

  const sessionId = computed(() => {
    if (typeof localStorage === "undefined") return null;
    return localStorage.getItem("currentRecordingSessionId");
  });

  const persist = (value: boolean): void => {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, String(value));
    }
  };

  const enable = (): void => {
    isEnabled.value = true;
    persist(true);
    analytics.enableSessionRecording();
  };

  const disable = (): void => {
    isEnabled.value = false;
    persist(false);
    analytics.disableSessionRecording();
  };

  const toggle = (): void => (isEnabled.value ? disable() : enable());

  return { isEnabled, sessionId, enable, disable, toggle };
}

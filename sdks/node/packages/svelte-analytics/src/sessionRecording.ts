// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { writable, type Writable } from "svelte/store";
import { getTempsAnalytics } from "./client";

const STORAGE_KEY = "temps_session_recording_enabled";

export interface SessionRecordingStore extends Writable<boolean> {
  enable: () => void;
  disable: () => void;
  toggle: () => void;
}

function readInitial(defaultEnabled: boolean): boolean {
  if (typeof localStorage === "undefined") return defaultEnabled;
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === null) return defaultEnabled;
  return stored === "true";
}

function persist(value: boolean): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(STORAGE_KEY, String(value));
}

/**
 * Svelte-native mirror of the React `useSessionRecordingControl` hook.
 * The returned store is a writable boolean + convenience helpers.
 */
export function sessionRecordingStore(defaultEnabled = false): SessionRecordingStore {
  const initial = readInitial(defaultEnabled);
  const store = writable<boolean>(initial);

  const apply = (value: boolean): void => {
    const analytics = getTempsAnalytics();
    if (value) analytics.enableSessionRecording();
    else analytics.disableSessionRecording();
    persist(value);
  };

  return {
    subscribe: store.subscribe,
    set(value): void {
      store.set(value);
      apply(value);
    },
    update(updater): void {
      store.update((prev) => {
        const next = updater(prev);
        apply(next);
        return next;
      });
    },
    enable(): void {
      store.set(true);
      apply(true);
    },
    disable(): void {
      store.set(false);
      apply(false);
    },
    toggle(): void {
      store.update((prev) => {
        const next = !prev;
        apply(next);
        return next;
      });
    },
  };
}

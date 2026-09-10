// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore } from "react";

type Route =
  | { kind: "submissions" }
  | { kind: "settings" };

function parseHash(hash: string): Route {
  const path = hash.replace(/^#\/?/, "");
  if (path === "settings") return { kind: "settings" };
  return { kind: "submissions" };
}

// Cached snapshot to avoid infinite re-renders
let cachedHash = "";
let cachedRoute: Route = { kind: "submissions" };

function getSnapshot(): Route {
  const hash = window.location.hash;
  if (hash !== cachedHash) {
    cachedHash = hash;
    cachedRoute = parseHash(hash);
  }
  return cachedRoute;
}

function subscribe(callback: () => void): () => void {
  window.addEventListener("hashchange", callback);
  return () => window.removeEventListener("hashchange", callback);
}

export function useRoute(): Route {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export function submissionsPath(): string {
  return "#/";
}

export function settingsPath(): string {
  return "#/settings";
}

export function useNavigate() {
  return {
    toSubmissions: () => {
      window.location.hash = submissionsPath();
    },
    toSettings: () => {
      window.location.hash = settingsPath();
    },
  };
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore, useCallback } from "react";

export type Route =
  | { kind: "submissions" }
  | { kind: "suggestions" }
  | { kind: "settings" };

function parseHash(hash: string): Route {
  const path = hash.replace(/^#\/?/, "");

  if (path === "suggestions") return { kind: "suggestions" };
  if (path === "settings") return { kind: "settings" };

  return { kind: "submissions" };
}

export function submissionsPath(): string {
  return "#/";
}

export function suggestionsPath(): string {
  return "#/suggestions";
}

export function settingsPath(): string {
  return "#/settings";
}

export function navigate(hash: string): void {
  window.location.hash = hash;
}

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
  return useSyncExternalStore(subscribe, getSnapshot);
}

export function useNavigate() {
  const goToSubmissions = useCallback(
    () => navigate(submissionsPath()),
    [],
  );
  const goToSuggestions = useCallback(
    () => navigate(suggestionsPath()),
    [],
  );
  const goToSettings = useCallback(() => navigate(settingsPath()), []);

  return { goToSubmissions, goToSuggestions, goToSettings };
}

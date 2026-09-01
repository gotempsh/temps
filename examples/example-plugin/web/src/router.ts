// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore, useCallback } from "react";

// ---------------------------------------------------------------------------
// Route definitions
// ---------------------------------------------------------------------------

export type Route =
  | { kind: "list" }
  | { kind: "report"; reportId: string }
  | { kind: "page"; reportId: string; pageUrl: string }
  | { kind: "settings" };

// ---------------------------------------------------------------------------
// Parse hash → Route
// ---------------------------------------------------------------------------

function parseHash(hash: string): Route {
  // Strip leading "#" or "#/"
  const path = hash.replace(/^#\/?/, "");

  // reports/:id/page?url=...
  const pageMatch = path.match(/^reports\/([^/]+)\/page(?:\?url=(.+))?$/);
  if (pageMatch) {
    return {
      kind: "page",
      reportId: pageMatch[1],
      pageUrl: pageMatch[2] ? decodeURIComponent(pageMatch[2]) : "",
    };
  }

  // reports/:id
  const reportMatch = path.match(/^reports\/([^/]+)$/);
  if (reportMatch) {
    return { kind: "report", reportId: reportMatch[1] };
  }

  // settings
  if (path === "settings") {
    return { kind: "settings" };
  }

  // Default → list
  return { kind: "list" };
}

// ---------------------------------------------------------------------------
// Build hash strings (for <a href> or navigate())
// ---------------------------------------------------------------------------

export function listPath(): string {
  return "#/";
}

export function reportPath(id: string): string {
  return `#/reports/${id}`;
}

export function pagePath(reportId: string, pageUrl: string): string {
  return `#/reports/${reportId}/page?url=${encodeURIComponent(pageUrl)}`;
}

export function settingsPath(): string {
  return "#/settings";
}

// ---------------------------------------------------------------------------
// Imperative navigation — pushes a history entry so back/forward works
// ---------------------------------------------------------------------------

export function navigate(hash: string): void {
  window.location.hash = hash;
}

// ---------------------------------------------------------------------------
// React hook — subscribe to hash changes
//
// IMPORTANT: useSyncExternalStore compares snapshots by reference (Object.is).
// parseHash() returns a new object every call, which would cause an infinite
// render loop. We cache the last hash string and only re-parse when it changes.
// ---------------------------------------------------------------------------

let cachedHash = "";
let cachedRoute: Route = { kind: "list" };

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

/**
 * Lightweight hash-based router.
 *
 * Returns the current `Route` and re-renders on hash changes.
 * Back/forward browser buttons work because `window.location.hash = ...`
 * pushes onto the history stack.
 */
export function useRoute(): Route {
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Convenience hook for navigation functions that return stable references.
 */
export function useNavigate() {
  const goToList = useCallback(() => navigate(listPath()), []);
  const goToReport = useCallback((id: string) => navigate(reportPath(id)), []);
  const goToPage = useCallback(
    (reportId: string, pageUrl: string) => navigate(pagePath(reportId, pageUrl)),
    [],
  );

  return { goToList, goToReport, goToPage };
}

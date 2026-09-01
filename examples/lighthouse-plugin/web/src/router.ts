// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useSyncExternalStore, useCallback } from "react";

// ---------------------------------------------------------------------------
// Route definitions
// ---------------------------------------------------------------------------

export type Route =
  | { kind: "list" }
  | { kind: "audit"; auditId: string }
  | { kind: "history" }
  | { kind: "settings" };

// ---------------------------------------------------------------------------
// Parse hash -> Route
// ---------------------------------------------------------------------------

function parseHash(hash: string): Route {
  const path = hash.replace(/^#\/?/, "");

  // audits/:id
  const auditMatch = path.match(/^audits\/([^/]+)$/);
  if (auditMatch) {
    return { kind: "audit", auditId: auditMatch[1] };
  }

  // history
  if (path === "history") {
    return { kind: "history" };
  }

  // settings
  if (path === "settings") {
    return { kind: "settings" };
  }

  // Default -> list
  return { kind: "list" };
}

// ---------------------------------------------------------------------------
// Build hash strings
// ---------------------------------------------------------------------------

export function listPath(): string {
  return "#/";
}

export function auditPath(id: string): string {
  return `#/audits/${id}`;
}

export function historyPath(): string {
  return "#/history";
}

export function settingsPath(): string {
  return "#/settings";
}

// ---------------------------------------------------------------------------
// Imperative navigation
// ---------------------------------------------------------------------------

export function navigate(hash: string): void {
  window.location.hash = hash;
}

// ---------------------------------------------------------------------------
// React hook -- subscribe to hash changes
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

export function useRoute(): Route {
  return useSyncExternalStore(subscribe, getSnapshot);
}

export function useNavigate() {
  const goToList = useCallback(() => navigate(listPath()), []);
  const goToAudit = useCallback((id: string) => navigate(auditPath(id)), []);
  const goToHistory = useCallback(() => navigate(historyPath()), []);
  const goToSettings = useCallback(() => navigate(settingsPath()), []);

  return { goToList, goToAudit, goToHistory, goToSettings };
}

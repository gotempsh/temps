// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { INGEST_KEY_HEADER, INGEST_KEY_QUERY_PARAM } from "./constants";
import { getOrCreateSessionId, getOrCreateVisitorId } from "./identity";
import type { JsonValue } from "./types";

export function getRequestId(): string | undefined {
  if (typeof document === "undefined") return undefined;
  const metaElement = document.querySelector('meta[name="temps-metadata"]');
  if (metaElement) {
    try {
      const content = metaElement.getAttribute("content") || "{}";
      const metadata = JSON.parse(content) as { request_id?: string };
      return metadata.request_id;
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("Failed to parse metadata:", error);
    }
  }
  return undefined;
}

export function isLocalhostLike(): boolean {
  try {
    const host = window.location.hostname;
    const isFile = window.location.protocol === "file:";
    const isLocalhost = /^localhost$|^127(\.[0-9]+){0,2}\.[0-9]+$|^\[::1?\]$/.test(host);
    return isFile || isLocalhost;
  } catch {
    return false;
  }
}

export function isTestEnvironment(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as unknown as Record<string, unknown>;
  const isPhantom = Boolean(w._phantom);
  const isNightmare = Boolean(w.__nightmare);
  const isWebdriver = Boolean(window.navigator?.webdriver);
  const isCypress = Boolean(w.Cypress);
  const allowTemps = Boolean(w.__temps);
  return (isPhantom || isNightmare || isWebdriver || isCypress) && !allowTemps;
}

/**
 * Returns a new object with request_id, visitorId and sessionId attached.
 * `visitorId`/`sessionId` are the client-generated fallback identity (see
 * `./identity`) — the server only uses them when it has no Temps-issued
 * `_temps_visitor_id`/`_temps_sid` cookie of its own to prefer. When a value
 * is unavailable, the key is set to `undefined` so `JSON.stringify` omits it
 * entirely (matching legacy @temps-sdk/react-analytics behavior).
 */
function enrich(data: Record<string, JsonValue>): Record<string, JsonValue> {
  const enriched = {
    ...data,
    request_id: getRequestId(),
    visitorId: getOrCreateVisitorId(),
    sessionId: getOrCreateSessionId(),
  } as Record<string, JsonValue>;
  return enriched;
}

/**
 * Request headers for a `fetch`-based ingest call. Returns an empty object
 * when no ingest key is configured, so a Temps-hosted app sends exactly the
 * headers it always has and keeps resolving by `Host`.
 */
export function ingestKeyHeaders(ingestKey?: string): Record<string, string> {
  return ingestKey ? { [INGEST_KEY_HEADER]: ingestKey } : {};
}

/**
 * Appends the ingest key to a URL as `?temps_key=…` for transports that cannot
 * set headers (`navigator.sendBeacon`). Returns the URL untouched when no key
 * is configured.
 */
export function withIngestKey(url: string, ingestKey?: string): string {
  if (!ingestKey) return url;
  const separator = url.includes("?") ? "&" : "?";
  return `${url}${separator}${INGEST_KEY_QUERY_PARAM}=${encodeURIComponent(ingestKey)}`;
}

export async function sendAnalytics(
  endpoint: string,
  data: Record<string, JsonValue>,
  method: "POST" | "PUT" | "PATCH" = "POST",
  basePath: string,
  ingestKey?: string
): Promise<void> {
  try {
    await fetch(`${basePath}/${endpoint}`, {
      method,
      headers: { "Content-Type": "application/json", ...ingestKeyHeaders(ingestKey) },
      body: JSON.stringify(enrich(data)),
    });
  } catch (error) {
    // eslint-disable-next-line no-console
    console.error("Failed to send analytics:", error);
  }
}

export function sendAnalyticsReliable(
  endpoint: string,
  data: Record<string, JsonValue>,
  basePath: string,
  ingestKey?: string
): boolean {
  try {
    // The key goes in the query string rather than a header for *both*
    // branches below. sendBeacon cannot set headers at all, and the fetch
    // fallback shares this URL — putting a custom header on it would force a
    // CORS preflight during unload, which browsers routinely drop. The server
    // accepts either transport (ADR-040).
    const url = withIngestKey(`${basePath}/${endpoint}`, ingestKey);
    const payload = JSON.stringify(enrich(data));

    // Try sendBeacon first (most reliable for page unload)
    if (
      typeof navigator !== "undefined" &&
      navigator.sendBeacon &&
      typeof navigator.sendBeacon === "function"
    ) {
      const blob = new Blob([payload], { type: "application/json" });
      return navigator.sendBeacon(url, blob);
    }

    // Fallback to fetch with keepalive
    fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: payload,
      keepalive: true,
    }).catch((error) => {
      // eslint-disable-next-line no-console
      console.error("Failed to send analytics (reliable):", error);
    });

    return true;
  } catch (error) {
    // eslint-disable-next-line no-console
    console.error("Failed to send analytics (reliable):", error);
    return false;
  }
}

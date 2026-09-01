// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
 * Returns a new object with request_id and session_id attached if available.
 * When they are unavailable, the keys are set to `undefined` so `JSON.stringify`
 * omits them entirely (matching legacy @temps-sdk/react-analytics behavior).
 */
function enrich(data: Record<string, JsonValue>): Record<string, JsonValue> {
  const enriched = {
    ...data,
    request_id: getRequestId(),
    session_id:
      typeof localStorage !== "undefined"
        ? localStorage.getItem("session_id") || undefined
        : undefined,
  } as Record<string, JsonValue>;
  return enriched;
}

export async function sendAnalytics(
  endpoint: string,
  data: Record<string, JsonValue>,
  method: "POST" | "PUT" | "PATCH" = "POST",
  basePath: string
): Promise<void> {
  try {
    await fetch(`${basePath}/${endpoint}`, {
      method,
      headers: { "Content-Type": "application/json" },
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
  basePath: string
): boolean {
  try {
    const url = `${basePath}/${endpoint}`;
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

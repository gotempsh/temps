"use client";
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
  // @ts-expect-error - non-standard globals from test frameworks
  const isPhantom = Boolean(window._phantom);
  // @ts-expect-error - non-standard globals from test frameworks
  const isNightmare = Boolean(window.__nightmare);
  const isWebdriver = Boolean(window.navigator?.webdriver);
  // @ts-expect-error - non-standard globals from test frameworks
  const isCypress = Boolean(window.Cypress);
  // @ts-expect-error - allow explicit override via window.__temps
  const allowTemps = Boolean(window.__temps);
  return (isPhantom || isNightmare || isWebdriver || isCypress) && !allowTemps;
}

export async function sendAnalytics(
  endpoint: string,
  data: Record<string, JsonValue>,
  method: "POST" | "PUT" | "PATCH" = "POST",
  basePath: string
): Promise<void> {
  try {
    const enrichedData = {
      ...data,
      request_id: getRequestId(),
      session_id:
        typeof localStorage !== "undefined"
          ? localStorage.getItem("session_id") || undefined
          : undefined,
    } as Record<string, JsonValue>;

    await fetch(`${basePath}/${endpoint}`, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(enrichedData),
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
    const enrichedData = {
      ...data,
      request_id: getRequestId(),
      session_id:
        typeof localStorage !== "undefined"
          ? localStorage.getItem("session_id") || undefined
          : undefined,
    } as Record<string, JsonValue>;

    const url = `${basePath}/${endpoint}`;
    const payload = JSON.stringify(enrichedData);

    // Try sendBeacon first (most reliable for page unload)
    if (navigator.sendBeacon && typeof navigator.sendBeacon === "function") {
      const blob = new Blob([payload], { type: "application/json" });
      return navigator.sendBeacon(url, blob);
    }

    // Fallback to synchronous fetch with keepalive
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * First-party, client-generated visitor/session identity.
 *
 * The server prefers its own encrypted `_temps_visitor_id`/`_temps_sid`
 * cookies (tamper-evident, issued by the Temps proxy when it serves the
 * page). Those cookies only exist when Temps itself serves the app's HTML —
 * when Temps is used purely as an analytics/session-replay backend for an
 * app it doesn't deploy or proxy, no such cookie is ever issued, and a
 * server `Set-Cookie` from a cross-origin ingest call would be a
 * third-party cookie that Safari/Chrome/Firefox privacy modes block anyway
 * (see gotempsh/temps#848). This module generates and persists identity
 * entirely client-side instead: the script always executes in the page's
 * own origin regardless of where it was loaded from, so a first-party
 * `localStorage` entry works everywhere a Temps-issued cookie can't.
 */

const VISITOR_ID_KEY = "temps_visitor_id";
const SESSION_ID_KEY = "temps_session_id";
const SESSION_LAST_SEEN_KEY = "temps_session_last_seen";

/**
 * Mirrors the proxy's default session inactivity window
 * (`session_max_age_minutes` in `crates/temps-proxy/src/traits.rs`) so
 * client-generated session grouping stays consistent with the
 * cookie-issued case.
 */
const SESSION_MAX_AGE_MS = 30 * 60 * 1000;

function randomId(prefix: string): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) {
    return crypto.randomUUID();
  }
  return `${prefix}_${Date.now()}_${Math.random().toString(36).substring(2, 11)}`;
}

/**
 * Returns a stable visitor id, generating and persisting one on first call.
 * `undefined` outside a browser (no `localStorage`).
 */
export function getOrCreateVisitorId(): string | undefined {
  if (typeof localStorage === "undefined") return undefined;
  let visitorId = localStorage.getItem(VISITOR_ID_KEY);
  if (!visitorId) {
    visitorId = randomId("visitor");
    localStorage.setItem(VISITOR_ID_KEY, visitorId);
  }
  return visitorId;
}

/**
 * Returns the current session id, rolling over to a new one once the
 * inactivity window has elapsed since the last call. Call this once per
 * outbound request so `lastSeen` tracks real activity.
 */
export function getOrCreateSessionId(): string | undefined {
  if (typeof localStorage === "undefined") return undefined;
  const now = Date.now();
  const lastSeen = Number(localStorage.getItem(SESSION_LAST_SEEN_KEY));
  let sessionId = localStorage.getItem(SESSION_ID_KEY);
  if (!sessionId || !Number.isFinite(lastSeen) || now - lastSeen > SESSION_MAX_AGE_MS) {
    sessionId = randomId("session");
    localStorage.setItem(SESSION_ID_KEY, sessionId);
  }
  localStorage.setItem(SESSION_LAST_SEEN_KEY, String(now));
  return sessionId;
}

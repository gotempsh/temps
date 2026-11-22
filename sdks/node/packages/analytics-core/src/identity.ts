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
 * Cookieless mode (ADR 109 D3). Once on, identity is never read from or
 * written to the device: the server derives a daily visitor id from the
 * request instead. It is one-way for the page's lifetime, so a second
 * analytics instance configured without it cannot start storing ids again.
 */
let cookieless = false;

/**
 * Stop storing identity on the device and delete what an earlier visit
 * stored, so switching a site to cookieless also forgets returning visitors'
 * ids. Called by `createAnalytics({ cookieless: true })`.
 */
export function enableCookielessIdentity(): void {
  const switched = !cookieless;
  cookieless = true;
  clearStoredIdentity();
  try {
    if (typeof localStorage !== "undefined") localStorage.removeItem("temps_identified");
  } catch {
    // Storage unusable: there is nothing stored to delete either.
  }
  if (!switched) return;
  for (const listener of Array.from(cookielessListeners)) {
    try {
      listener();
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("[temps] cookieless switch listener failed:", error);
    }
  }
}

/**
 * Delete the stored visitor and session ids, so the next event starts a new
 * visitor. Used by `reset()` on sign-out.
 */
export function clearStoredIdentity(): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.removeItem(VISITOR_ID_KEY);
      localStorage.removeItem(SESSION_ID_KEY);
      localStorage.removeItem(SESSION_LAST_SEEN_KEY);
    }
  } catch {
    // Storage unusable: nothing was stored.
  }
}

/**
 * The visitor id this tracker stored, without creating one. `undefined` in
 * cookieless mode, before the first event, or when storage is blocked. On a
 * site Temps serves, the server-side visitor is the `_temps_visitor_id`
 * cookie instead, which the page cannot read (it is `HttpOnly`).
 */
export function getStoredVisitorId(): string | undefined {
  if (cookieless) return undefined;
  try {
    if (typeof localStorage === "undefined") return undefined;
    return localStorage.getItem(VISITOR_ID_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

const cookielessListeners = new Set<() => void>();

/**
 * Run `listener` when the page switches to cookieless mode, so an analytics
 * instance created earlier can stop what needs a stored id (session replay).
 * Returns the unsubscribe function.
 */
export function onCookielessIdentity(listener: () => void): () => void {
  cookielessListeners.add(listener);
  return () => {
    cookielessListeners.delete(listener);
  };
}

/**
 * The two moments of a `reset()`: `started` once the ids stored in the
 * browser are gone, `confirmed` once the server has expired its tracking
 * cookies too (at once on a cookieless page, which has none). A `reset()`
 * the server did not confirm reports only `started`.
 */
export type IdentityResetPhase = "started" | "confirmed";

const identityResetListeners = new Set<(phase: IdentityResetPhase) => void>();

/** True from a reset's `started` until a reset reaches `confirmed`. */
let identityResetPending = false;

/**
 * Whether a `reset()` has started but the server has not confirmed it yet
 * (or refused it). Until then the device may still name the previous
 * visitor, so a session replay must not start: a recorder created or
 * started in that window waits for `confirmed`.
 */
export function isIdentityResetPending(): boolean {
  return identityResetPending;
}

/**
 * Run `listener` at each phase of a `reset()` (ADR 109 D4), so a session
 * replay in progress ends when it starts, and the next one begins only once
 * nothing on the device still names the previous visitor: two people's
 * activity never shares a recording. Returns the unsubscribe function.
 */
export function onIdentityReset(listener: (phase: IdentityResetPhase) => void): () => void {
  identityResetListeners.add(listener);
  return () => {
    identityResetListeners.delete(listener);
  };
}

/** Tell every `onIdentityReset` listener that a reset reached `phase`. */
export function notifyIdentityReset(phase: IdentityResetPhase): void {
  identityResetPending = phase === "started";
  for (const listener of Array.from(identityResetListeners)) {
    try {
      listener(phase);
    } catch (error) {
      // eslint-disable-next-line no-console
      console.error("[temps] identity reset listener failed:", error);
    }
  }
}

/** Whether identity storage is off for this page (see `enableCookielessIdentity`). */
export function isCookielessIdentity(): boolean {
  return cookieless;
}

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
 * `undefined` when `localStorage` is missing or unusable: outside a browser,
 * blocked by the user's cookie settings or a sandboxed iframe (merely reading
 * `localStorage` throws), or full. An id that cannot be persisted would be a
 * new visitor on every event, so the server's own identity is used instead.
 */
export function getOrCreateVisitorId(): string | undefined {
  if (cookieless) return undefined;
  try {
    if (typeof localStorage === "undefined") return undefined;
    let visitorId = localStorage.getItem(VISITOR_ID_KEY);
    if (!visitorId) {
      visitorId = randomId("visitor");
      localStorage.setItem(VISITOR_ID_KEY, visitorId);
    }
    return visitorId;
  } catch {
    return undefined;
  }
}

/**
 * Returns the current session id, rolling over to a new one once the
 * inactivity window has elapsed since the last call. Call this once per
 * outbound request so `lastSeen` tracks real activity.
 */
export function getOrCreateSessionId(): string | undefined {
  if (cookieless) return undefined;
  try {
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
  } catch {
    // Same reasoning as getOrCreateVisitorId.
    return undefined;
  }
}

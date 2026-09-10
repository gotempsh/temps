// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const DEFAULT_BASE_PATH = "/api/_temps";
export const SESSION_RECORDER_ENDPOINT = "session-replay";

/**
 * Header carrying the `pa_`-prefixed analytics ingest key (ADR-040). Preferred
 * over the query param: it keeps the credential out of URLs, and therefore out
 * of `proxy_logs.request_query`, referrer headers and browser history.
 */
export const INGEST_KEY_HEADER = "X-Temps-Analytics-Key";

/**
 * Query-param fallback for the analytics ingest key. `navigator.sendBeacon`
 * cannot set request headers, so unload-path deliveries — page_leave and the
 * final session-replay flush, exactly the events that matter most — have to
 * carry the key here instead.
 */
export const INGEST_KEY_QUERY_PARAM = "temps_key";

/**
 * Built-in paths excluded from session replay by default, covering common
 * authentication and payment flows so integrators don't accidentally record
 * sensitive pages. Supports `*` wildcards, matched against
 * `window.location.pathname` only (case-sensitive, anchored at both ends).
 *
 * Merged with any user-supplied `excludedPaths` unless
 * `useDefaultExcludedPaths: false` is passed to `SessionRecorder`.
 */
export const DEFAULT_EXCLUDED_PATHS: string[] = [
  "/login",
  "/log-in",
  "/signin",
  "/sign-in",
  "/logout",
  "/log-out",
  "/signup",
  "/sign-up",
  "/register",
  "/checkout*",
  "/payment*",
  "/billing*",
  "/reset-password*",
  "/forgot-password*",
  "/mfa*",
  "/2fa*",
  "/verify*",
];

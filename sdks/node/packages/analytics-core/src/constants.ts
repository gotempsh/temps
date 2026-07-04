export const DEFAULT_BASE_PATH = "/api/_temps";
export const SESSION_RECORDER_ENDPOINT = "session-replay";

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

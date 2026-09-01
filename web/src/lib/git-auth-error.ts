// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Problem-details `type` the API returns when a git provider rejects the
 * stored credential (see `GitProviderError::AuthenticationFailed` →
 * `problem_new(UNAUTHORIZED)` in `temps-git/src/handlers/base.rs`).
 */
const AUTH_FAILED_PROBLEM_TYPE = 'authentication_failed'

/**
 * True when an error means "this git provider rejected the stored credential"
 * — i.e. reconnecting the provider is the fix.
 *
 * Deliberately matches ONLY the typed problem `type`. Broader heuristics are
 * wrong here: the console returns 401 for an expired *user session* on these
 * same queries, so keying off the status code (or "unauthorized" appearing in
 * the message) would tell someone who simply needs to log in again to go
 * re-authorize GitHub. `classify_provider_response_error` in temps-git
 * guarantees a provider 401/403 carries this type, so the looser fallbacks
 * bought nothing and cost correctness.
 */
export function isGitAuthError(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false
  const { type } = error as { type?: unknown }
  return typeof type === 'string' && type.includes(AUTH_FAILED_PROBLEM_TYPE)
}

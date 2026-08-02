import { describe, expect, test } from 'bun:test'
import { isGitAuthError } from './git-auth-error'

/**
 * The whole point of this predicate is telling two different 401s apart:
 * a git provider rejecting the stored credential (reconnect the provider)
 * versus the user's own session expiring (log in again). An earlier version
 * matched on status codes and message text and conflated the two, which sent
 * logged-out users off to re-authorize GitHub.
 */
describe('isGitAuthError', () => {
  test('matches the typed provider auth failure', () => {
    expect(
      isGitAuthError({
        type: 'https://docs.temps.sh/errors/authentication_failed',
        title: 'Authentication Failed',
        detail:
          'provider rejected the stored credential while trying to get tree for owner/repo@main: HTTP 401 Unauthorized',
      })
    ).toBe(true)
  })

  test('does NOT match an expired user session', () => {
    // What the console returns for an unauthenticated API call. Treating this
    // as a git error is the regression this test exists to prevent.
    expect(
      isGitAuthError({
        status: 401,
        title: 'Unauthorized',
        detail: 'Authentication required',
      })
    ).toBe(false)
  })

  test('does NOT match a forbidden response from an unrelated endpoint', () => {
    expect(
      isGitAuthError({
        status: 403,
        title: 'Insufficient Permissions',
        detail: 'Requires ProjectsWrite permission',
      })
    ).toBe(false)
  })

  test('does NOT match a provider outage', () => {
    expect(
      isGitAuthError({
        type: 'https://docs.temps.sh/errors/api_error',
        title: 'API Error',
        detail: 'Failed to get tree: HTTP 503 Service Unavailable',
      })
    ).toBe(false)
  })

  test('does NOT match on the word "unauthorized" alone', () => {
    expect(
      isGitAuthError({ detail: 'HTTP 401 Unauthorized', message: 'forbidden' })
    ).toBe(false)
  })

  // Rows are wrapped in tuples: with a scalar row bun treats the callback's
  // second parameter as a `done` callback and the test times out.
  test.each([[null], [undefined], ['a string'], [42], [[]]] as Array<
    [unknown]
  >)('returns false for non-problem value %p', (value) => {
    expect(isGitAuthError(value)).toBe(false)
  })
})

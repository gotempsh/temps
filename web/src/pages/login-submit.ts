// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface SubmitLoginOptions<TCredentials> {
  credentials: TCredentials
  /** Typically `loginMutation.mutateAsync`; may reject with a problem body. */
  login: (credentials: TCredentials) => Promise<unknown>
  setLoading: (loading: boolean) => void
}

/**
 * Runs a sign-in attempt and never rejects.
 *
 * A failed sign-in is already reported to the user by the mutation's error
 * handler (the "Login failed" toast) and kept in the mutation state. The
 * rejection from `mutateAsync` must still be consumed here: `handleSubmit`
 * of react-hook-form rethrows it, and a rejection nobody catches is logged by
 * the browser as an uncaught promise rejection.
 */
export async function submitLogin<TCredentials>({
  credentials,
  login,
  setLoading,
}: SubmitLoginOptions<TCredentials>): Promise<void> {
  setLoading(true)
  try {
    await login(credentials)
  } catch {
    // Reported by the mutation's onError (toast); deliberately not rethrown.
  } finally {
    setLoading(false)
  }
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Email-verification calls that the generated SDK does not cover yet.
 *
 * `POST /auth/resend-verification` and `POST /users/{user_id}/verify-email`
 * are new; until `web/src/api/client` is regenerated from a running server they
 * go through the shared `client` with hand-written types. Replace these with
 * the generated `resendEmailVerification` / `markUserEmailVerified` once the
 * SDK has them.
 *
 * Why it matters: SSO sign-in is only linked onto an existing account whose
 * email is verified, so an unverified address blocks its owner's first SSO
 * login until they click the emailed link or an administrator verifies it.
 */

import { client } from '@/api/client/client.gen'
import type { RouteUserWithRoles } from '@/api/client'

/** Response of `POST /auth/resend-verification`. */
export interface VerificationEmailSentResponse {
  /** Address the verification link was sent to. */
  email: string
  message: string
}

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

/** Email the signed-in user a fresh verification link. */
export async function resendEmailVerification(): Promise<VerificationEmailSentResponse> {
  const { data } = await client.post<
    { 200: VerificationEmailSentResponse },
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: '/auth/resend-verification',
    throwOnError: true,
  })
  return data
}

/** Mark a user's email as verified (requires `users:manage`). */
export async function markUserEmailVerified(
  userId: number
): Promise<RouteUserWithRoles> {
  const { data } = await client.post<
    { 200: RouteUserWithRoles },
    unknown,
    true
  >({
    security: [...BEARER_SECURITY],
    url: `/users/${userId}/verify-email`,
    throwOnError: true,
  })
  return data
}

/** Shown wherever a user could ask for a link but no email provider exists. */
export const NO_EMAIL_PROVIDER_MESSAGE =
  "No email provider is configured on this Temps instance, so a verification email can't be sent. Ask an administrator to verify your email address."

/**
 * Whether `/user/me` reports the address as verified. The generated
 * `UserResponse` type predates the `email_verified` field; an absent field
 * (an older server) is treated as verified so no action is offered that the
 * server cannot serve.
 */
export function currentUserEmailVerified(user: unknown): boolean {
  if (user && typeof user === 'object' && 'email_verified' in user) {
    return (user as { email_verified?: unknown }).email_verified !== false
  }
  return true
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Sentence for the toast shown once the reset went through. */
export function resetMfaSuccessMessage(
  displayName: string,
  sessionsRevoked: number
): string {
  const sessions =
    sessionsRevoked === 1 ? '1 session' : `${sessionsRevoked} sessions`
  return `MFA removed for ${displayName}. ${sessions} signed out.`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * API key expiry as the console's date inputs show it.
 *
 * The date input holds a UTC calendar date (`YYYY-MM-DD`); the server stores a
 * full timestamp. Keys created by `temps login` expire at a time of day, so a
 * date-only round trip would move their expiry to 00:00 UTC, up to 24 hours
 * earlier, every time the key is saved.
 */

/** The UTC calendar date of `date`, as a date input's value. */
function utcDate(date: Date): string {
  return date.toISOString().split('T')[0]
}

/** The date an expiry is shown as; empty for a key that never expires. */
export function expiryDateInput(expiresAt: string | null | undefined): string {
  return expiresAt ? utcDate(new Date(expiresAt)) : ''
}

/**
 * The earliest expiry date a user may pick: tomorrow (UTC). A key is refused
 * once `expires_at` has passed, and a date-only expiry means 00:00 UTC that
 * day, so today would create a key that is already expired.
 */
export function earliestExpiryDate(now: Date = new Date()): string {
  const tomorrow = new Date(now)
  tomorrow.setUTCDate(tomorrow.getUTCDate() + 1)
  return utcDate(tomorrow)
}

/** The timestamp to store for a date the user picked (00:00 UTC that day). */
export function expiryFromDateInput(date: string): string | null {
  return date ? new Date(date).toISOString() : null
}

/**
 * The expiry to send when saving an existing key. A date the user did not
 * change keeps the stored timestamp exactly, time of day included; only a
 * changed (or cleared) date replaces it.
 */
export function expiryForUpdate(
  date: string,
  storedExpiresAt: string | null | undefined
): string | null {
  if (date === expiryDateInput(storedExpiresAt)) return storedExpiresAt ?? null
  return expiryFromDateInput(date)
}

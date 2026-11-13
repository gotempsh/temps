// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** A bundle or session lifetime in the largest unit that keeps it exact: 90 s, 15 min, 1 h 30 min. */
export function formatLifetime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '0 s'
  const whole = Math.round(seconds)
  if (whole < 60) return `${whole} s`
  const minutes = Math.floor(whole / 60)
  const rest = whole % 60
  if (minutes < 60) return rest === 0 ? `${minutes} min` : `${whole} s`
  const hours = Math.floor(minutes / 60)
  const restMinutes = minutes % 60
  if (rest !== 0) return `${whole} s`
  return restMinutes === 0 ? `${hours} h` : `${hours} h ${restMinutes} min`
}

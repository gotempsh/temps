// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

type MatchMedia = (query: string) => { matches: boolean }

function systemMatchMedia(): MatchMedia | undefined {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function')
    return undefined
  return (query) => window.matchMedia(query)
}

/**
 * The scroll behavior for a programmatic jump: smooth, unless the user asked
 * the system for reduced motion, in which case it jumps instantly.
 * `matchMedia` is the system's by default; tests pass their own.
 */
export function preferredScrollBehavior(
  matchMedia: MatchMedia | undefined = systemMatchMedia()
): ScrollBehavior {
  const reduce = matchMedia?.('(prefers-reduced-motion: reduce)').matches
  return reduce ? 'auto' : 'smooth'
}

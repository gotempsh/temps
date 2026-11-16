// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The scroll behavior for a programmatic jump: smooth, unless the user asked
 * the system for reduced motion, in which case it jumps instantly.
 */
export function preferredScrollBehavior(): ScrollBehavior {
  const reduce =
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  return reduce ? 'auto' : 'smooth'
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The page for one client address: where it is, and what it has requested.
 * Encoded so an IPv6 address stays one path segment.
 */
export function ipDetailPath(ip: string): string {
  return `/ip/${encodeURIComponent(ip)}`
}

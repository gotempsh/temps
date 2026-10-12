// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Resolve a status page link to an absolute URL.
 *
 * Until a public or external URL is configured the API returns a path relative
 * to the console (`/api/status/<slug>`): the server cannot know which origin
 * the operator reached the console on. Resolve it against the origin this
 * console is open on, so Copy and Open produce a link that works from here.
 */
export function resolvePublicUrl(url: string, origin: string): string {
  if (!url.startsWith('/')) {
    return url
  }
  return new URL(url, origin).toString()
}

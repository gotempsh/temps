// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Open a page in a new tab without giving it a reference back to the console.
 *
 * `window.open(url, '_blank')` leaves the new page with `window.opener`, which
 * it can use to navigate the console tab. A link with `target="_blank"` is
 * opened without that reference by browsers, but `window.open` is not, so
 * `noopener` is passed explicitly. Use this for any page the console does not
 * serve itself, such as a hosted Git provider, a self-hosted GitLab or a
 * webhook endpoint.
 */
export function openExternal(url: string): void {
  window.open(url, '_blank', 'noopener,noreferrer')
}

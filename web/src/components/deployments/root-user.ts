// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Where the deploy log's root warning also points. */
export const NON_ROOT_USER_DOCS_URL =
  'https://temps.sh/docs/docker-containers#non-root-user'

/**
 * The Dockerfile lines the fix asks for. `useradd` is the Debian/Ubuntu
 * command; Alpine images ship `adduser` instead.
 */
export const NON_ROOT_USER_SNIPPET = [
  '# Debian/Ubuntu',
  'RUN useradd -r -u 10001 app',
  '# Alpine: RUN adduser -S -u 10001 app',
  'USER app',
].join('\n')

/** What the image declares, in words, for the popover's first line. */
export function describeImageUser(imageUser?: string | null): string {
  return imageUser
    ? `Its Dockerfile sets USER ${imageUser}, which is uid 0.`
    : 'Its Dockerfile sets no USER, so it runs as root.'
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Where the deploy log's root warning also points. */
export const NON_ROOT_USER_DOCS_URL =
  'https://temps.sh/docs/docker-containers#non-root-user'

/** The two Dockerfile lines the fix asks for. */
export const NON_ROOT_USER_SNIPPET = 'RUN useradd -r -u 10001 app\nUSER app'

/** What the image declares, in words, for the popover's first line. */
export function describeImageUser(imageUser?: string | null): string {
  return imageUser
    ? `Its Dockerfile sets USER ${imageUser}, which is uid 0.`
    : 'Its Dockerfile sets no USER, so it runs as root.'
}

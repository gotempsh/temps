// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** How text typed into the command palette is matched against navigation entries. */
export const NAV_FUSE_OPTIONS = {
  keys: [
    { name: 'title', weight: 2 },
    { name: 'url', weight: 1 },
    { name: 'keywords', weight: 1.5 },
  ],
  threshold: 0.3,
  includeScore: true,
  shouldSort: true,
  minMatchCharLength: 1,
}

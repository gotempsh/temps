// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Match the root spellings accepted by normalize_project_directory on the server. */
export function isRepositoryRootDirectory(
  directory: string | undefined | null
): boolean {
  const normalized = (directory ?? '')
    .trim()
    .replace(/\\/g, '/')
    .replace(/^\/+/, '')

  return normalized === '' || normalized === '.' || normalized === './'
}

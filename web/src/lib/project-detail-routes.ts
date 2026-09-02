// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Canonical destination for the retired project-level Databases route. */
export function legacyDatabasesRedirectPath(projectSlug: string): string {
  return `/projects/${projectSlug}/storage`
}

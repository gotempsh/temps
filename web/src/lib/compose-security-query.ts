// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Whether a project's preset is Docker Compose.
 *
 * The server stores `docker-compose`; `dockercompose` is the legacy spelling
 * the console has always accepted. Anything else (static, dockerfile,
 * nixpacks, ...) is not a Compose project.
 */
export function isComposePresetName(
  preset: string | null | undefined
): boolean {
  return preset === 'docker-compose' || preset === 'dockercompose'
}

/**
 * Whether the Compose security policy should be requested for a project.
 *
 * `GET /projects/{id}/compose-security` answers 400 "does not use Docker
 * Compose" for every other preset, so the read must never fire for a
 * non-Compose project -- nor speculatively while the project itself is still
 * loading, when its preset is not yet known.
 */
export function shouldLoadComposeSecurity(
  project: { id?: number | null; preset?: string | null } | null | undefined
): boolean {
  if (!project || typeof project.id !== 'number') return false
  return isComposePresetName(project.preset)
}

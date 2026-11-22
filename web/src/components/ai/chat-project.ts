// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** A project a new chat can be about. */
export interface ProjectChoice {
  id: number
  name: string
  slug?: string | null
  service_template_image_url?: string | null
  source_type?: string | null
}

/** Most projects the picker fetches; the search covers only these. */
export const PROJECT_CHOICE_LIMIT = 100

/** Projects whose name or slug contains `query`, ignoring case and spacing. */
export function filterProjects<T extends ProjectChoice>(
  projects: readonly T[],
  query: string
): T[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return [...projects]
  return projects.filter((project) =>
    `${project.name} ${project.slug ?? ''}`.toLowerCase().includes(needle)
  )
}

/** The part of a chat-creation request that names the project, if any. */
export function projectRequest(
  project: ProjectChoice | null
): { project_id: number } | Record<string, never> {
  return project ? { project_id: project.id } : {}
}

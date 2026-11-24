// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ApplicationProjectResponse,
  ChatResource,
} from '@/api/client/types.gen'
import { projectRefOf } from './chat-resources'
import type { ProjectRef } from './project-refs'

/**
 * The Temps projects a chat works on: its application's projects (primary
 * first), then any the chat made itself that the application does not list.
 * A chat that joined an application after another chat deployed it still
 * sees that project, and a chat on its own sees what it made.
 */
export function chatProjects(
  applicationProjects: readonly ApplicationProjectResponse[],
  resources: readonly ChatResource[]
): ProjectRef[] {
  const linked = [...applicationProjects]
    .sort((a, b) => Number(b.is_primary) - Number(a.is_primary))
    .map(({ id, slug, name }) => ({ id, slug, name }))
  const seen = new Set(linked.map((project) => project.id))
  const made = resources
    .map(projectRefOf)
    .filter((project): project is ProjectRef => {
      if (!project || seen.has(project.id)) return false
      seen.add(project.id)
      return true
    })
  return [...linked, ...made]
}

/** The console page of a project. */
export function projectHref(project: ProjectRef): string {
  return `/projects/${project.slug}`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import { getRepositoryByNameOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  gitProviderKind,
  repositoryWebUrl,
  type GitProviderKind,
} from '@/lib/project-header-actions'

export interface ProjectRepository {
  /** The repository's page, never a clone URL with credentials in it. */
  url: string
  provider: GitProviderKind | null
}

/**
 * Where the project's code lives, as a page to open. A project deployed from
 * a URL carries it; one connected through a git provider has it on the
 * synced repository, read once and cached.
 */
export function useProjectRepository(
  project: ProjectResponse
): ProjectRepository | null {
  const owner = project.repo_owner ?? ''
  const name = project.repo_name ?? ''
  const connectionId = project.git_provider_connection_id ?? 0
  const repository = useQuery({
    ...getRepositoryByNameOptions({
      path: { owner, name },
      query: { connection_id: connectionId },
    }),
    enabled: !project.git_url && !!owner && !!name && connectionId > 0,
    staleTime: 5 * 60_000,
    retry: false,
  })
  const cloneUrl = project.git_url || repository.data?.clone_url
  if (!cloneUrl) return null
  const url = repositoryWebUrl(cloneUrl)
  if (!url) return null
  return { url, provider: gitProviderKind(null, cloneUrl) }
}

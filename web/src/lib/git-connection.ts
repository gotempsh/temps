// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const REPOSITORY_PAGE_SIZE_OPTIONS = [20, 50, 100] as const

/** Repository orderings the connection page offers, in API terms. */
export const REPOSITORY_SORTS = {
  pushed: { label: 'Recently pushed', sort: 'pushed_at', direction: 'desc' },
  updated: { label: 'Recently updated', sort: 'updated_at', direction: 'desc' },
  name: { label: 'Name (A–Z)', sort: 'name', direction: 'asc' },
  created: { label: 'Newest', sort: 'created_at', direction: 'desc' },
} as const

export type RepositorySort = keyof typeof REPOSITORY_SORTS
export type RepositoryVisibility = 'all' | 'public' | 'private'

export interface RepositoryListState {
  page: number
  perPage: number
  search: string
  visibility: RepositoryVisibility
  sort: RepositorySort
}

/**
 * Reads the repository list's URL state. Every value comes from a URL a user
 * can edit or share, so anything unrecognized falls back to the default
 * rather than reaching the API.
 */
export function parseRepositoryListState(state: {
  page?: string
  per_page?: string
  q?: string
  visibility?: string
  sort?: string
}): RepositoryListState {
  const page = Number(state.page)
  const perPage = Number(state.per_page)
  return {
    page: Number.isSafeInteger(page) && page > 0 ? page : 1,
    perPage: (REPOSITORY_PAGE_SIZE_OPTIONS as readonly number[]).includes(
      perPage
    )
      ? perPage
      : REPOSITORY_PAGE_SIZE_OPTIONS[0],
    search: state.q?.trim() ?? '',
    visibility:
      state.visibility === 'public' || state.visibility === 'private'
        ? state.visibility
        : 'all',
    sort:
      state.sort && Object.keys(REPOSITORY_SORTS).includes(state.sort)
        ? (state.sort as RepositorySort)
        : 'pushed',
  }
}

export function providerDisplayName(providerType: string): string {
  switch (providerType) {
    case 'github':
      return 'GitHub'
    case 'gitlab':
      return 'GitLab'
    case 'gitea':
      return 'Gitea'
    case 'bitbucket':
      return 'Bitbucket'
    case 'generic':
      return 'Other Git Provider'
    default:
      return providerType.charAt(0).toUpperCase() + providerType.slice(1)
  }
}

export function authMethodDisplayName(authMethod: string): string {
  switch (authMethod) {
    case 'app':
    case 'github_app':
      return 'GitHub App'
    case 'oauth':
      return 'OAuth'
    case 'token':
      return 'Personal Access Token'
    default:
      return authMethod.charAt(0).toUpperCase() + authMethod.slice(1)
  }
}

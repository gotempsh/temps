import {
  getPublicComposePreview,
  getRepositoryComposePreview,
} from '@/api/client'

export interface ComposePreviewRequest {
  branch?: string
  path: string
  composeOverride?: string
  excludedServices: string[]
}

export interface ComposePreviewResponse {
  path: string
  effectiveCompose: string
  enabledServices: string[]
  disabledServices: string[]
  redactedValues: number
}

export type ComposePreviewRepository =
  | { kind: 'connected'; repositoryId: number }
  | {
      kind: 'public'
      provider: string
      owner: string
      repo: string
    }

export function composePreviewEndpoint(repository: ComposePreviewRepository): {
  url: string
  path: Record<string, string | number>
} {
  return repository.kind === 'connected'
    ? {
        url: '/repositories/{repository_id}/compose-file/preview',
        path: { repository_id: repository.repositoryId },
      }
    : {
        url: '/git/public/{provider}/{owner}/{repo}/compose-file',
        path: {
          provider: repository.provider,
          owner: repository.owner,
          repo: repository.repo,
        },
      }
}

export async function fetchComposePreview(
  repository: ComposePreviewRepository,
  request: ComposePreviewRequest,
  signal?: AbortSignal
): Promise<ComposePreviewResponse> {
  const result =
    repository.kind === 'connected'
      ? await getRepositoryComposePreview({
          path: { repository_id: repository.repositoryId },
          body: request,
          signal,
          throwOnError: false,
        })
      : await getPublicComposePreview({
          path: {
            provider: repository.provider,
            owner: repository.owner,
            repo: repository.repo,
          },
          body: request,
          signal,
          throwOnError: false,
        })
  const { data, error, response } = result
  if (error || !data) {
    throw {
      status: response?.status,
      problem: error,
    }
  }
  return data
}

export function composePreviewErrorMessage(error: unknown): string {
  const failure = error as {
    status?: unknown
    problem?: unknown
    detail?: unknown
    message?: unknown
  } | null
  if (failure?.status === 405) {
    return 'The running Temps backend does not support Compose previews yet. Restart or update the backend, then try again.'
  }

  for (const candidate of [failure, failure?.problem]) {
    const problem = candidate as
      { detail?: unknown; message?: unknown } | null | undefined
    if (typeof problem?.detail === 'string' && problem.detail.trim()) {
      return problem.detail
    }
    if (typeof problem?.message === 'string' && problem.message.trim()) {
      return problem.message
    }
  }
  return 'The effective Compose preview could not be generated.'
}

export function isPublicRepositoryRateLimitError(error: unknown): boolean {
  return (error as { status?: unknown } | null)?.status === 429
}

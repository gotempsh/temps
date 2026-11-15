// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getPublicComposePreview,
  getRepositoryComposePreview,
} from '@/api/client'
import type {
  ComposeCheckErrorClass,
  ComposeCheckReport,
  ComposeOverrideRedundancy,
  ComposePreviewProblemResponse,
  ComposeSecurityCheck,
  ComposeSecurityPolicy,
} from '@/api/client'

export interface ComposePreviewRequest {
  branch?: string
  path: string
  composeOverride?: string
  excludedServices: string[]
  previewPolicy?: ComposeSecurityPolicy
  /**
   * Run the deploy-time check with this project's saved policy and
   * environment variables. Connected repositories only.
   */
  projectId?: number
  environmentId?: number
}

export interface ComposePreviewResponse {
  path: string
  effectiveCompose: string
  enabledServices: string[]
  disabledServices: string[]
  redactedValues: number
  /** The deploy-time check the preview ran; absent for public repositories. */
  check?: ComposeCheckReport | null
  overrideRedundancy?: ComposeOverrideRedundancy | null
  overrideWarning?: string | null
}

export type ComposePreviewRepository =
  | { kind: 'connected'; repositoryId: number }
  | {
      kind: 'public'
      provider: string
      owner: string
      repo: string
      baseUrl?: string
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
  // Public previews never load a project's variables.
  const {
    projectId: _projectId,
    environmentId: _environmentId,
    ...publicRequest
  } = request
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
          query: { base_url: repository.baseUrl },
          body: publicRequest,
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

export function composePreviewPolicyCheck(
  error: unknown
): ComposeSecurityCheck | null {
  const problem = (error as { problem?: ComposePreviewProblemResponse } | null)
    ?.problem
  return typeof problem?.policy_check === 'string' ? problem.policy_check : null
}

export function isPublicRepositoryRateLimitError(error: unknown): boolean {
  return (error as { status?: unknown } | null)?.status === 429
}

function previewProblem(error: unknown): ComposePreviewProblemResponse | null {
  return (
    (error as { problem?: ComposePreviewProblemResponse } | null)?.problem ??
    null
  )
}

/** The class a deployment of the previewed configuration would fail with. */
export function composePreviewErrorClass(
  error: unknown
): ComposeCheckErrorClass | null {
  return previewProblem(error)?.error_class ?? null
}

/** The failed deploy-time check behind a preview error, when it ran. */
export function composePreviewFailedCheck(
  error: unknown
): ComposeCheckReport | null {
  return previewProblem(error)?.compose_check ?? null
}

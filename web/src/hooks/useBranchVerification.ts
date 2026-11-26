// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getPublicBranchOptions,
  getRepositoryBranchOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { BranchInfo } from '@/api/client/types.gen'
import { httpStatusOf } from '@/lib/http-error-status'
import { useQuery } from '@tanstack/react-query'
import { useCallback, useMemo } from 'react'

/**
 * What the server has said about one branch name that a branch listing did
 * not contain.
 *
 * `missing` is the only verdict that justifies telling a user the branch does
 * not exist, and it only ever comes from the exact single-branch lookup
 * answering 404. A listing is capped by the provider's safety limit, so a
 * name absent from it may be a real branch the listing never reached.
 */
export type BranchVerificationStatus =
  /** Nothing to verify: the name is in the list, or there is no name yet. */
  | 'not-needed'
  /** The lookup is in flight; say nothing alarming yet. */
  | 'checking'
  /** The server returned the branch. */
  | 'found'
  /** The server answered 404: the repository has no such branch. */
  | 'missing'
  /** The lookup failed for another reason, so nothing was settled. */
  | 'unverified'

export interface BranchVerification {
  status: BranchVerificationStatus
  /** The branch the lookup found, with the server's `is_default` and SHA. */
  branch?: BranchInfo
  /**
   * Ask the provider again. The exact lookup has its own query key, so
   * refreshing the branch listing never refetches it: a "Refresh branches"
   * control must call this too, or a branch past the listing cap keeps the
   * head SHA (or the failure) from before the refresh. A no-op when nothing
   * is being verified.
   */
  refresh: () => Promise<void>
}

export interface BranchVerificationInput {
  repoOwner: string
  repoName: string
  /** Authenticated connection; omit for a public repository. */
  connectionId?: number
  /** Public provider to ask when there is no connection. */
  publicProvider?: 'github' | 'gitlab' | null
  /** Self-hosted GitLab origin for the public lookup. */
  publicInstanceUrl?: string
  /** The branch to confirm. Pass `undefined` to verify nothing. */
  branch?: string
  /**
   * Whether this name needs confirming at all. Callers pass `false` while the
   * listing is still loading or already contains the name, so no provider
   * request is spent on a question that is already answered.
   */
  enabled: boolean
}

/**
 * Confirm one branch name against the server, exactly.
 *
 * Both endpoints answer with a single provider request, so they are exact for
 * repositories with more branches than a listing returns -- which is the
 * whole point: the console must not read "absent from a capped list" as "does
 * not exist".
 */
export function useBranchVerification({
  repoOwner,
  repoName,
  connectionId,
  publicProvider,
  publicInstanceUrl,
  branch,
  enabled,
}: BranchVerificationInput): BranchVerification {
  const name = branch?.trim() ?? ''
  const canAsk = enabled && !!repoOwner && !!repoName && !!name

  const connectionQuery = useQuery({
    ...getRepositoryBranchOptions({
      path: { owner: repoOwner, repo: repoName },
      query: { connection_id: connectionId ?? 0, name },
    }),
    enabled: canAsk && !!connectionId,
    retry: false,
  })

  const publicQuery = useQuery({
    ...getPublicBranchOptions({
      path: {
        provider: publicProvider ?? 'github',
        owner: repoOwner,
        repo: repoName,
      },
      query: { base_url: publicInstanceUrl, name },
    }),
    enabled: canAsk && !connectionId && !!publicProvider,
    retry: false,
  })

  const query = connectionId ? connectionQuery : publicQuery
  const asked = canAsk && (!!connectionId || !!publicProvider)

  const { refetch } = query
  const refresh = useCallback(async () => {
    if (!asked) return
    await refetch()
  }, [asked, refetch])

  return useMemo<BranchVerification>(() => {
    if (!asked) return { status: 'not-needed', refresh }
    if (query.data) return { status: 'found', branch: query.data, refresh }
    if (query.isError) {
      // Only the server saying "no such branch" settles the question. A 401,
      // 403, 429, 502 or network failure leaves it open, and claiming the
      // branch is missing on one of those is exactly the bug this guards.
      return {
        status: httpStatusOf(query.error) === 404 ? 'missing' : 'unverified',
        refresh,
      }
    }
    return { status: 'checking', refresh }
  }, [asked, query.data, query.error, query.isError, refresh])
}

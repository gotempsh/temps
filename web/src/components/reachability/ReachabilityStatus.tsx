// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The project header's answer to "can people open this?" (ADR 064 D3):
// "Public at …" or "Not reachable yet — <reason>", with the fix one click
// away.

import { useQuery } from '@tanstack/react-query'
import { Globe } from 'lucide-react'
import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { getEnvironmentReachabilityOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { reachabilityHeadline } from '@/lib/reachability'
import { reachabilityPath } from '@/lib/reachability-page'

export function ReachabilityStatus({
  projectId,
  projectSlug,
  environmentId,
}: {
  projectId: number
  projectSlug: string
  environmentId: number
}) {
  const reachability = useQuery({
    ...getEnvironmentReachabilityOptions({
      path: { project_id: projectId, environment_id: environmentId },
    }),
    staleTime: 30_000,
    refetchInterval: 60_000,
    retry: false,
  })

  // Without an answer the header says nothing rather than guessing.
  let line: ReactNode = null
  let offerFix = false
  if (reachability.isPending) {
    line = <Skeleton className="h-4 w-40" />
  } else if (reachability.data) {
    const headline = reachabilityHeadline(reachability.data)
    if (headline.kind === 'public') {
      line = (
        <span className="truncate">
          Public at{' '}
          <a
            href={headline.url}
            target="_blank"
            rel="noopener noreferrer"
            className="font-medium text-foreground underline-offset-4 hover:underline"
          >
            {headline.address}
          </a>
        </span>
      )
    } else if (headline.kind === 'private') {
      line = (
        <span className="truncate">
          Private at{' '}
          <a
            href={headline.url}
            target="_blank"
            rel="noopener noreferrer"
            className="font-medium text-foreground underline-offset-4 hover:underline"
          >
            {headline.address}
          </a>
          {headline.network && ` (${headline.network})`}
        </span>
      )
    } else if (headline.kind === 'not_reachable') {
      offerFix = headline.canFix
      line = (
        <span className="truncate" title={headline.reason}>
          Not reachable yet — {headline.reason}
        </span>
      )
    } else {
      line = (
        <span className="truncate" title={headline.reason}>
          {headline.reason}
        </span>
      )
    }
  }

  return (
    <div className="hidden min-w-0 max-w-md items-center gap-2 text-xs text-muted-foreground lg:flex">
      <Globe aria-hidden="true" className="size-3.5 shrink-0" />
      {line}
      {offerFix && (
        <Button size="sm" variant="outline" className="h-7 shrink-0" asChild>
          <Link to={reachabilityPath(projectSlug, environmentId)}>
            Make it reachable
          </Link>
        </Button>
      )}
    </div>
  )
}

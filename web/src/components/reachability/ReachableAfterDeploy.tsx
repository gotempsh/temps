// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// After a successful deploy, the one thing still in the way when nobody can
// open the app yet (ADR 064 D1): say why, and link to Make it reachable
// (ADR 069).

import { useQuery } from '@tanstack/react-query'
import { Globe } from 'lucide-react'
import { Link } from 'react-router'
import { getEnvironmentReachabilityOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { reachabilityHeadline } from '@/lib/reachability'
import { reachabilityPath } from '@/lib/reachability-page'

export function ReachableAfterDeploy({
  enabled,
  projectId,
  projectSlug,
  environmentId,
}: {
  /** The deploy succeeded and Temps serves the app. */
  enabled: boolean
  projectId: number
  projectSlug: string
  environmentId: number
}) {
  const reachability = useQuery({
    ...getEnvironmentReachabilityOptions({
      path: { project_id: projectId, environment_id: environmentId },
    }),
    enabled,
    staleTime: 30_000,
    retry: false,
  })
  const headline = reachability.data
    ? reachabilityHeadline(reachability.data)
    : null
  // Said only when Temps knows nobody can open it and has a fix; while it
  // cannot tell, the header carries the state.
  const show = enabled && headline?.kind === 'not_reachable' && headline.canFix
  if (!show) return null
  return (
    <Alert variant="warning">
      <Globe className="h-4 w-4" />
      <AlertTitle>Deployed, but nobody can open it yet</AlertTitle>
      <AlertDescription className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <span>{headline.reason}</span>
        <Button size="sm" className="shrink-0" asChild>
          <Link to={reachabilityPath(projectSlug, environmentId)}>
            Make it reachable
          </Link>
        </Button>
      </AlertDescription>
    </Alert>
  )
}

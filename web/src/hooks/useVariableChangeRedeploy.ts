// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getEnvironmentsQueryKey,
  getProjectDeploymentsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  redeployCurrentDeployments,
  redeploySummary,
} from '@/lib/service-link-redeploy'
import {
  environmentsAffectedByChange,
  variableChangeRedeployMessage,
  type VariableChangeScope,
} from '@/lib/variable-change-redeploy'
import { useQueryClient } from '@tanstack/react-query'
import { useCallback } from 'react'
import { toast } from 'sonner'

/** Long enough to read the prompt and reach the button. */
const PROMPT_DURATION_MS = 20_000

/**
 * Confirms a variable or secret change. When an environment the change
 * applies to is running a deployment, the confirmation also says the running
 * app keeps the old value until its next deploy, and offers a Redeploy action
 * that redeploys exactly those environments from their current source.
 */
export function useVariableChangeRedeploy(
  project: Pick<ProjectResponse, 'id' | 'source_type'>
) {
  const queryClient = useQueryClient()
  return useCallback(
    async (title: string, scope: VariableChangeScope) => {
      let environments: EnvironmentResponse[]
      try {
        // Fresh read: which deployment is live may have changed since the
        // list was loaded, and this runs once per save, not per render.
        environments = await queryClient.fetchQuery({
          ...getEnvironmentsOptions({ path: { project_id: project.id } }),
          staleTime: 0,
        })
      } catch {
        toast.success(title, { description: 'Applies on next deploy.' })
        return
      }
      const targets = environmentsAffectedByChange(environments, scope)
      if (targets.length === 0) {
        // Nothing running uses it yet; the first deployment picks it up.
        toast.success(title)
        return
      }
      const completed = new Set<number>()
      toast.success(title, {
        description: variableChangeRedeployMessage(targets),
        duration: PROMPT_DURATION_MS,
        action: {
          label: 'Redeploy',
          onClick: () => {
            redeployCurrentDeployments(
              project.id,
              project.source_type,
              targets,
              undefined,
              completed
            )
              .then((outcomes) => {
                const summary = redeploySummary(outcomes)
                if (summary.started > 0) toast.success(summary.message)
                else toast.warning(summary.message)
                void queryClient.invalidateQueries({
                  queryKey: getProjectDeploymentsQueryKey({
                    path: { id: project.id },
                  }),
                })
                void queryClient.invalidateQueries({
                  queryKey: getEnvironmentsQueryKey({
                    path: { project_id: project.id },
                  }),
                })
              })
              .catch((error: unknown) =>
                toast.error(
                  `Failed to start the redeploy: ${
                    (error as { detail?: string })?.detail ??
                    (error instanceof Error ? error.message : 'unknown error')
                  }`
                )
              )
          },
        },
      })
    },
    [project.id, project.source_type, queryClient]
  )
}

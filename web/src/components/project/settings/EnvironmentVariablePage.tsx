// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, useParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import { getEnvironmentVariablesOptions } from '@/api/client/@tanstack/react-query.gen'
import { CheckLoading } from './CheckLoading'
import { Button } from '@/components/ui/button'
import { EnvironmentVariableDetails } from './EnvironmentVariableDetails'
import { HttpChecksSettings } from './HttpChecksSettings'

export function EnvironmentVariablePage({
  project,
  configure = false,
}: {
  project: ProjectResponse
  configure?: boolean
}) {
  const { variableId } = useParams<{ variableId: string }>()
  const id = Number(variableId)
  const valid = Number.isSafeInteger(id) && id > 0
  const variables = useQuery({
    ...getEnvironmentVariablesOptions({ path: { project_id: project.id } }),
    enabled: valid,
    refetchInterval: 30000,
  })
  const variable = variables.data?.find((item) => item.id === id)
  const listPath = `/projects/${project.slug}/environment-variables`
  const detailPath = `${listPath}/${id}`
  return (
    <div className="w-full min-w-0 space-y-5">
      {valid && variables.isPending ? (
        <CheckLoading label="Loading variable…" />
      ) : variables.isError ? (
        <div role="alert" className="space-y-3">
          <p>Could not load this variable.</p>
          <Button variant="outline" onClick={() => void variables.refetch()}>
            Retry
          </Button>
        </div>
      ) : !variable ? (
        <div className="space-y-3">
          <h2 className="text-xl font-semibold">Variable not found</h2>
          <p className="text-sm text-muted-foreground">
            This variable may have been deleted or is not part of this project.
          </p>
          <Button asChild variant="outline">
            <Link to={listPath}>Back to environment variables</Link>
          </Button>
        </div>
      ) : configure ? (
        <HttpChecksSettings
          key={variable.id}
          projectId={project.id}
          variable={variable}
        />
      ) : (
        <EnvironmentVariableDetails
          key={variable.id}
          projectId={project.id}
          variable={variable}
          detailPath={detailPath}
        />
      )}
    </div>
  )
}

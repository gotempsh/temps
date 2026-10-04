// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, useParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import { listProjectSecretsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { CheckLoading } from './CheckLoading'
import { HttpChecksSettings } from './HttpChecksSettings'
import { SecretDetails } from './SecretDetails'

export function SecretPage({
  project,
  configure = false,
}: {
  project: ProjectResponse
  configure?: boolean
}) {
  const { secretId } = useParams<{ secretId: string }>()
  const id = Number(secretId)
  const valid = Number.isSafeInteger(id) && id > 0
  const secrets = useQuery({
    ...listProjectSecretsOptions({
      path: { project_id: project.id },
      query: {},
    }),
    enabled: valid,
    refetchInterval: 30000,
  })
  const secret = secrets.data?.find((item) => item.id === id)
  const listPath = `/projects/${project.slug}/settings/variables`
  const detailPath = `/projects/${project.slug}/settings/secrets/${id}`
  return (
    <div className="w-full min-w-0 space-y-5">
      {valid && secrets.isPending ? (
        <CheckLoading label="Loading secret…" />
      ) : secrets.isError ? (
        <div role="alert" className="space-y-3">
          <p>Could not load this secret.</p>
          <Button variant="outline" onClick={() => void secrets.refetch()}>
            Retry
          </Button>
        </div>
      ) : !secret ? (
        <div className="space-y-3">
          <h2 className="text-xl font-semibold">Secret not found</h2>
          <p className="text-sm text-muted-foreground">
            This secret may have been deleted or is not part of this project.
          </p>
          <Button asChild variant="outline">
            <Link to={listPath}>Back to variables and secrets</Link>
          </Button>
        </div>
      ) : configure ? (
        <HttpChecksSettings
          key={secret.id}
          projectId={project.id}
          subject={{ kind: 'secret', id: secret.id, key: secret.key }}
        />
      ) : (
        <SecretDetails
          key={secret.id}
          projectId={project.id}
          secret={secret}
          detailPath={detailPath}
        />
      )}
    </div>
  )
}

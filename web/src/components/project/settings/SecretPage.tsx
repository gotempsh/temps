// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, useLocation, useParams } from 'react-router'
import { ArrowLeft } from 'lucide-react'
import { detailReturnPath } from '@/lib/detail-return-path'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import type { ProjectResponse } from '@/api/client'
import { getProjectSecretOptions } from '@/api/client/@tanstack/react-query.gen'
import { problemStatus } from '@/lib/cloud-telemetry-activation'
import { Button } from '@/components/ui/button'
import { CheckLoading } from './CheckLoading'
import { HttpChecksSettings } from './HttpChecksSettings'
import { SecretDetails } from './SecretDetails'
import { settingsSectionHref } from '@/lib/project-settings-sections'

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
  const secretQuery = useQuery({
    ...getProjectSecretOptions({
      path: { project_id: project.id, secret_id: id },
    }),
    enabled: valid,
    refetchInterval: 30000,
    // A 404 is an answer ("not found"), not a failure worth retrying.
    retry: (failureCount, error) =>
      problemStatus(error) !== 404 && failureCount < 3,
  })
  const notFound = problemStatus(secretQuery.error) === 404
  // A refetch that answers 404 keeps the last data in the cache; the secret
  // is gone, so that data must not keep its controls on screen.
  const secret = notFound ? undefined : secretQuery.data
  const listPath = settingsSectionHref(project.slug, 'variables', 'secrets')
  const detailPath = `/projects/${project.slug}/settings/secrets/${id}`
  const location = useLocation()
  // Captured on arrival: switching tabs on this page replaces the location
  // state, and the way back should not change because of it.
  const [backPath] = useState(() =>
    detailReturnPath(location.state, project.slug, listPath)
  )
  const backLabel = backPath === listPath ? 'Secrets' : 'Back'
  return (
    <div className="w-full min-w-0 space-y-5">
      <Link
        to={backPath}
        className="inline-flex items-center gap-1.5 text-sm text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
      >
        <ArrowLeft className="size-4" aria-hidden="true" />
        {backLabel}
      </Link>
      {valid && secretQuery.isPending ? (
        <CheckLoading label="Loading secret…" />
      ) : secretQuery.isError && !notFound ? (
        <div role="alert" className="space-y-3">
          <p>Could not load this secret.</p>
          <Button variant="outline" onClick={() => void secretQuery.refetch()}>
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
            <Link to={backPath}>Back to secrets</Link>
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
          project={project}
          secret={secret}
          detailPath={detailPath}
        />
      )}
    </div>
  )
}

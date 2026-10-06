// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, useLocation, useParams } from 'react-router'
import { ArrowLeft } from 'lucide-react'
import { detailReturnPath } from '@/lib/detail-return-path'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import type { ProjectResponse } from '@/api/client'
import { listProjectSecretsOptions } from '@/api/client/@tanstack/react-query.gen'
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
  const secrets = useQuery({
    ...listProjectSecretsOptions({
      path: { project_id: project.id },
      query: {},
    }),
    enabled: valid,
    refetchInterval: 30000,
  })
  const secret = secrets.data?.find((item) => item.id === id)
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
          projectId={project.id}
          secret={secret}
          detailPath={detailPath}
        />
      )}
    </div>
  )
}

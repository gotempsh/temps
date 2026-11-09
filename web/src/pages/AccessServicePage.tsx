// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Callout, PageContainer, PageState } from '@temps-sdk/ds'
import { Button, Skeleton } from '@temps-sdk/ui'
import { PlugZap } from 'lucide-react'
import type { AccessServiceResponse } from '@/api/client'
import {
  accessCapabilityOptions,
  getAccessServiceOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AccessServiceEditor } from '@/components/service-access/AccessServiceEditor'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

function Editor({
  projectId,
  vaultId,
  service,
  canUseProjectGithub,
  onClose,
  onSaved,
}: {
  projectId: number
  vaultId?: string
  service?: AccessServiceResponse
  canUseProjectGithub: boolean
  onClose: () => void
  onSaved: () => void
}) {
  // Pin the edit's original version. Refetches must not replace dirty values
  // or silently authorize overwriting a newer version with older edits.
  const [initialService] = useState(service)
  return (
    <AccessServiceEditor
      projectId={projectId}
      vaultId={vaultId}
      service={initialService}
      canUseProjectGithub={canUseProjectGithub}
      onClose={onClose}
      onSaved={onSaved}
    />
  )
}

export default function AccessServicePage() {
  const { projectId: rawProjectId, serviceId } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const client = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const projectId = Number(rawProjectId)
  const valid =
    Number.isInteger(projectId) && projectId > 0 && projectId <= 2147483647
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    enabled: valid,
    retry: false,
  })
  const service = useQuery({
    ...getAccessServiceOptions({
      path: { project_id: projectId, service_id: serviceId ?? '' },
    }),
    enabled: valid && !!serviceId,
    retry: false,
    refetchOnMount: 'always',
  })
  const editorKey = `${projectId}:${serviceId ?? 'new'}`
  const [loadedEditorKey, setLoadedEditorKey] = useState<string>()
  const hasLoadedService = loadedEditorKey === editorKey
  // Latch the first successful read for this route. Later refetch failures must
  // keep the mounted draft, but cached data alone must not initialize an edit.
  if (
    serviceId &&
    service.isFetchedAfterMount &&
    service.isSuccess &&
    !hasLoadedService
  ) {
    setLoadedEditorKey(editorKey)
  }
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      ...(valid
        ? [
            {
              label: `Project ${projectId}`,
              href: `/vault/projects/${projectId}`,
            },
          ]
        : []),
      {
        label: serviceId
          ? (service.data?.name ?? 'Edit service')
          : 'Add service',
      },
    ])
    return () => setBreadcrumbs([])
  }, [projectId, valid, serviceId, service.data?.name, setBreadcrumbs])
  const back = () => {
    const vault = params.get('vault')
    const sandbox = params.get('sandbox')
    navigate(
      valid
        ? `/vault/projects/${projectId}${vault ? `/vaults/${encodeURIComponent(vault)}` : ''}${sandbox ? `?sandbox=${encodeURIComponent(sandbox)}` : ''}`
        : '/vault'
    )
  }
  const saved = () => {
    void client.invalidateQueries({ queryKey: [{ _id: 'listAccessVaults' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'getAccessVault' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listAccessServices' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listServiceGrants' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'getAccessService' }] })
  }
  return (
    <PageContainer width="form">
      {!valid || (serviceId && service.isError && !hasLoadedService) ? (
        <PageState
          variant="failed"
          icon={PlugZap}
          title="Could not load service"
          description={
            !valid
              ? 'The project ID in this link is invalid.'
              : problemDetail(
                  service.error,
                  'Check project access or return to Vault to select a service.'
                )
          }
          action={
            <div className="flex gap-2">
              <Button variant="outline" onClick={back}>
                Back to Vault
              </Button>
              {valid && (
                <Button onClick={() => void service.refetch()}>Retry</Button>
              )}
            </div>
          }
        />
      ) : capability.isError && !capability.data ? (
        <PageState
          variant="failed"
          icon={PlugZap}
          title="Could not check service permissions"
          description="Retry to verify project write permission before editing."
          action={
            <Button onClick={() => void capability.refetch()}>Retry</Button>
          }
        />
      ) : capability.isPending || (serviceId && !hasLoadedService) ? (
        <Skeleton
          className="h-48 w-full"
          role="status"
          aria-label="Loading service"
        />
      ) : !capability.data?.can_manage_services ? (
        <PageState
          variant="failed"
          icon={PlugZap}
          title="Service policies are read-only"
          description="Editing services requires project write permission. Ask a project administrator for access."
          action={
            <Button variant="outline" onClick={back}>
              Back to Vault
            </Button>
          }
        />
      ) : (
        <>
          {capability.isError && (
            <Callout tone="error" title="Could not refresh service permissions">
              Your edits are preserved. Retry before saving.
              <Button
                variant="outline"
                onClick={() => void capability.refetch()}
              >
                Retry permissions
              </Button>
            </Callout>
          )}
          <fieldset disabled={capability.isError} className="min-w-0">
            <Editor
              key={editorKey}
              projectId={projectId}
              vaultId={params.get('vault') ?? undefined}
              service={service.data}
              canUseProjectGithub={
                capability.data?.can_use_project_github === true
              }
              onClose={back}
              onSaved={saved}
            />
          </fieldset>
        </>
      )}
    </PageContainer>
  )
}

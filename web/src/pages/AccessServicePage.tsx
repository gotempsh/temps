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
  generalAccessCapabilityOptions,
  getAccessServiceGeneralOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AccessServiceEditor } from '@/components/service-access/AccessServiceEditor'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

function Editor({
  vaultId,
  service,
  canUseProjectGithub,
  onClose,
  onSaved,
}: {
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
      vaultId={vaultId}
      service={initialService}
      canUseProjectGithub={canUseProjectGithub}
      onClose={onClose}
      onSaved={onSaved}
    />
  )
}

export default function AccessServicePage() {
  const { serviceId } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const client = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const capability = useQuery({
    ...generalAccessCapabilityOptions(),
    retry: false,
  })
  const service = useQuery({
    ...getAccessServiceGeneralOptions({
      path: { service_id: serviceId ?? '' },
    }),
    enabled: !!serviceId,
    retry: false,
    refetchOnMount: 'always',
  })
  const editorKey = `${serviceId ?? 'new'}`
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
      {
        label: serviceId
          ? (service.data?.name ?? 'Edit service')
          : 'Add service',
      },
    ])
    return () => setBreadcrumbs([])
  }, [serviceId, service.data?.name, setBreadcrumbs])
  const back = () => {
    const vault = params.get('vault')
    const sandbox = params.get('sandbox')
    navigate(
      `/vault${vault ? `/vaults/${encodeURIComponent(vault)}` : ''}${sandbox ? `?sandbox=${encodeURIComponent(sandbox)}` : ''}`
    )
  }
  const saved = () => {
    void client.invalidateQueries({
      queryKey: [{ _id: 'listAccessVaultsGeneral' }],
    })
    void client.invalidateQueries({
      queryKey: [{ _id: 'getAccessVaultGeneral' }],
    })
    void client.invalidateQueries({
      queryKey: [{ _id: 'listAccessServicesGeneral' }],
    })
    void client.invalidateQueries({ queryKey: [{ _id: 'listServiceGrants' }] })
    void client.invalidateQueries({
      queryKey: [{ _id: 'getAccessServiceGeneral' }],
    })
  }
  return (
    <PageContainer width="form">
      {serviceId && service.isError && !hasLoadedService ? (
        <PageState
          variant="failed"
          icon={PlugZap}
          title="Could not load service"
          description={problemDetail(
            service.error,
            'Check access or return to Vault to select a service.'
          )}
          action={
            <div className="flex gap-2">
              <Button variant="outline" onClick={back}>
                Back to Vault
              </Button>
              <Button onClick={() => void service.refetch()}>Retry</Button>
            </div>
          }
        />
      ) : capability.isError && !capability.data ? (
        <PageState
          variant="failed"
          icon={PlugZap}
          title="Could not check service permissions"
          description="Retry to verify administrator permission before editing."
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
          description="Editing services requires an instance administrator."
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useNavigate, useParams, useSearchParams } from 'react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Callout, PageContainer, PageHeader, PageState } from '@temps-sdk/ds'
import { Button, Skeleton } from '@temps-sdk/ui'
import { PlugZap } from 'lucide-react'
import type { AccessServiceResponse, ServiceCatalogEntry } from '@/api/client'
import {
  generalAccessCapabilityOptions,
  getAccessServiceGeneralOptions,
  serviceCatalogOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceCatalogPicker } from '@/components/service-access/ServiceCatalogPicker'
import { AccessServiceEditor } from '@/components/service-access/AccessServiceEditor'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

function Editor({
  vaultId,
  catalogEntry,
  onChangeService,
  service,
  canUseProjectGithub,
  onClose,
  onSaved,
}: {
  catalogEntry?: ServiceCatalogEntry
  onChangeService?: () => void
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
      catalogEntry={catalogEntry}
      onChangeService={onChangeService}
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
  const [params, setParams] = useSearchParams()
  const providerId = params.get('provider') ?? ''
  const configure =
    !serviceId && params.get('step') === 'configure' && !!providerId
  const selected = useQuery({
    ...serviceCatalogOptions({ query: { id: providerId } }),
    enabled: !serviceId && !!providerId && providerId !== 'custom',
  })
  const entry: ServiceCatalogEntry | undefined =
    providerId === 'custom'
      ? {
          id: 'custom',
          name: 'Custom HTTP service',
          description: '',
          category: 'Custom',
          supported: true,
          logo: 'custom',
          popular: false,
        }
      : selected.data?.items.find((item) => item.id === providerId)
  const catalogStep = () => {
    const next = new URLSearchParams(params)
    next.set('step', 'select')
    setParams(next)
  }
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
    <PageContainer width={!serviceId && !configure ? 'full' : 'form'}>
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
          {!serviceId && (!configure || !entry?.supported) && (
            <PageHeader
              title={
                configure
                  ? (entry?.name ?? 'Configure service')
                  : 'Choose a service'
              }
              actions={
                <Button
                  variant="outline"
                  onClick={configure ? catalogStep : back}
                >
                  {configure ? 'Change service' : 'Cancel'}
                </Button>
              }
            />
          )}
          {!serviceId && !configure && (
            <ServiceCatalogPicker
              selectedId={providerId}
              disabled={capability.isError}
              searchValue={params.get('q') ?? ''}
              pageValue={Math.min(
                1_000_000,
                Math.max(1, Math.floor(Number(params.get('page')) || 1))
              )}
              onSearchChange={(q) => {
                const next = new URLSearchParams(params)
                next.set('q', q)
                next.delete('page')
                setParams(next, { replace: true })
              }}
              onPageChange={(page) => {
                const next = new URLSearchParams(params)
                next.set('page', String(page))
                setParams(next)
              }}
              onSelect={(item) => {
                const next = new URLSearchParams(params)
                next.set('provider', item.id)
                next.set('step', 'configure')
                setParams(next)
                window.scrollTo({ top: 0 })
              }}
            />
          )}
          {!serviceId &&
            configure &&
            !entry &&
            (selected.isPending ? (
              <Skeleton className="h-48" />
            ) : (
              <Callout tone="error" title="Could not load selected service">
                <Button onClick={catalogStep}>Choose a service</Button>
                <Button
                  variant="outline"
                  onClick={() => void selected.refetch()}
                >
                  Retry
                </Button>
              </Callout>
            ))}
          {!serviceId && configure && entry && !entry.supported && (
            <Callout title="This service is not supported yet">
              {entry.reason}
            </Callout>
          )}
          <fieldset
            hidden={!serviceId && (!configure || !entry?.supported)}
            disabled={capability.isError}
            className="min-w-0"
          >
            {(serviceId || entry?.supported) && (
              <Editor
                key={`${editorKey}:${providerId}`}
                catalogEntry={entry}
                onChangeService={!serviceId ? catalogStep : undefined}
                vaultId={params.get('vault') ?? undefined}
                service={service.data}
                canUseProjectGithub={
                  capability.data?.can_use_project_github === true
                }
                onClose={back}
                onSaved={saved}
              />
            )}
          </fieldset>
        </>
      )}
    </PageContainer>
  )
}

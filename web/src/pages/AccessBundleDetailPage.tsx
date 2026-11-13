// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  Detail,
  HelpPopover,
  PageContainer,
  Status,
} from '@temps-sdk/ds'
import {
  generalAccessCapabilityOptions,
  getAccessVaultGeneralOptions,
  getAccessVaultOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { accessBundleOptions } from '@/components/service-access/access-bundle-query'
import { formatLifetime } from '@/components/service-access/lifetime'
import { ServiceProviderLogo } from '@/components/service-access/service-providers'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

export default function AccessBundleDetailPage() {
  const { vaultId = '', bundleId = '', projectId: routeProjectId } = useParams()
  const projectId =
    routeProjectId === undefined ? undefined : Number(routeProjectId)
  const [search] = useSearchParams()
  const sandboxId = search.get('sandbox') ?? undefined
  const consumerQuery = sandboxId
    ? `?sandbox=${encodeURIComponent(sandboxId)}`
    : ''
  const valid =
    !!vaultId &&
    !!bundleId &&
    (projectId === undefined ||
      (Number.isSafeInteger(projectId) && projectId > 0))
  const base = `/vault${projectId === undefined ? '' : `/projects/${projectId}`}/vaults/${encodeURIComponent(vaultId)}`
  const back = `${base}?tab=bundles${sandboxId ? `&sandbox=${encodeURIComponent(sandboxId)}` : ''}`
  const bundle = useQuery({
    ...accessBundleOptions(vaultId, bundleId, sandboxId, projectId),
    enabled: valid,
  })
  const projectVault = useQuery({
    ...getAccessVaultOptions({
      path: { project_id: projectId ?? 0, vault_id: vaultId },
    }),
    enabled: valid && projectId !== undefined,
  })
  const generalVault = useQuery({
    ...getAccessVaultGeneralOptions({
      path: { vault_id: vaultId },
      query: sandboxId ? { sandbox_id: sandboxId } : undefined,
    }),
    enabled: valid && projectId === undefined,
  })
  const vault = projectId === undefined ? generalVault : projectVault
  const capability = useQuery({
    ...generalAccessCapabilityOptions(),
    enabled: valid,
  })
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      {
        label: vault.data?.vault.name ?? 'Vault',
        href: `${base}${consumerQuery}`,
      },
      { label: 'Access bundles', href: back },
      { label: bundle.data?.name ?? 'Access bundle' },
    ])
    return () => setBreadcrumbs([])
  }, [
    setBreadcrumbs,
    vault.data?.vault.name,
    bundle.data?.name,
    base,
    back,
    consumerQuery,
  ])
  const error = bundle.error || vault.error
  const services =
    vault.data?.services.filter((service) =>
      bundle.data?.service_ids.includes(service.id)
    ) ?? []
  const blocked = services.filter(
    (service) => !service.enabled || !service.has_credential
  ).length
  const missing = (bundle.data?.service_ids.length ?? 0) - services.length
  const unusable = blocked + Math.max(missing, 0)
  return (
    <PageContainer width="full">
      {!valid ? (
        <Callout tone="error" title="Invalid access bundle link">
          <Link to="/vault">Back to Vault</Link>
        </Callout>
      ) : error ? (
        <Callout tone="error" title="Could not load access bundle">
          {problemDetail(error, 'Return to the vault or retry.')}
          <Button
            onClick={() => {
              void bundle.refetch()
              void vault.refetch()
            }}
          >
            Retry
          </Button>
          <Button variant="outline" asChild>
            <Link to={back}>All access bundles</Link>
          </Button>
        </Callout>
      ) : !bundle.data || !vault.data ? (
        <Skeleton
          role="status"
          className="h-64"
          aria-label="Loading access bundle"
        />
      ) : (
        <Detail
          embedded
          title={bundle.data.name}
          verdict={
            unusable === 0 ? (
              <Status tone="ok" label="Ready for sessions" />
            ) : (
              <Status
                tone="warn"
                label={`${unusable} ${unusable === 1 ? 'service' : 'services'} can't be used`}
              />
            )
          }
          actions={
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" asChild>
                <Link to={back}>All access bundles</Link>
              </Button>
              {capability.data?.can_manage_requests && !capability.isError && (
                <Button asChild>
                  <Link
                    to={`${base}/bundles/${encodeURIComponent(bundleId)}/sessions/new${consumerQuery}`}
                  >
                    Create session
                  </Link>
                </Button>
              )}
              {capability.data?.can_manage_services && !capability.isError ? (
                <Button asChild>
                  <Link
                    to={`${base}/bundles/${encodeURIComponent(bundleId)}/edit`}
                  >
                    Edit bundle
                  </Link>
                </Button>
              ) : (
                <Button
                  disabled
                  title="Instance administrator permission is required"
                >
                  Edit bundle
                </Button>
              )}
            </div>
          }
          facts={[
            { label: 'Vault', value: vault.data.vault.name },
            { label: 'Services', value: bundle.data.service_ids.length },
            {
              label: 'Access lifetime',
              value: formatLifetime(bundle.data.ttl_seconds),
            },
            { label: 'Version', value: bundle.data.version },
          ]}
          main={
            <div className="space-y-4">
              <div className="flex items-center gap-1">
                <h2 className="text-sm font-medium">Services</h2>
                <HelpPopover label="What a bundle does">
                  <p>
                    A bundle is a reusable selection of services and a maximum
                    lifetime. Saving it grants no access; a session does.
                  </p>
                </HelpPopover>
              </div>
              <DataTable
                aria-label="Bundle services"
                rows={services}
                rowKey={(service) => service.id}
                columns={[
                  {
                    key: 'name',
                    header: 'Service',
                    render: (service) => (
                      <div className="flex items-center gap-3">
                        <ServiceProviderLogo service={service} />
                        <div className="min-w-0">
                          <span className="font-medium break-words">
                            {service.name}
                          </span>
                          <p className="text-xs text-muted-foreground break-all">
                            {service.policy.origin}
                          </p>
                          <p className="text-xs text-muted-foreground break-all @xl:hidden">
                            {service.policy.methods.join(', ')} ·{' '}
                            {service.policy.path_prefixes.join(', ')}
                          </p>
                        </div>
                      </div>
                    ),
                  },
                  {
                    key: 'scope',
                    header: 'Allowed requests',
                    className: 'hidden @xl:table-cell',
                    render: (service) => (
                      <div className="space-y-1 break-all text-sm">
                        <p>{service.policy.methods.join(', ')}</p>
                        <p className="font-mono text-xs text-muted-foreground">
                          {service.policy.path_prefixes.join(', ')}
                        </p>
                      </div>
                    ),
                  },
                  {
                    key: 'state',
                    header: 'Status',
                    render: (service) =>
                      !service.enabled ? (
                        <Status tone="warn" label="Disabled" />
                      ) : !service.has_credential ? (
                        <Status tone="warn" label="No credential" />
                      ) : (
                        <Status tone="ok" label="Ready" />
                      ),
                  },
                ]}
              />
            </div>
          }
        />
      )}
    </PageContainer>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect } from 'react'
import { Link, useParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  Detail,
  PageContainer,
  Status,
} from '@temps-sdk/ds'
import {
  accessCapabilityOptions,
  getAccessVaultOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { accessBundleOptions } from '@/components/service-access/access-bundle-query'
import { ServiceProviderLogo } from '@/components/service-access/service-providers'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

export default function AccessBundleDetailPage() {
  const { projectId: rawId, vaultId = '', bundleId = '' } = useParams()
  const projectId = Number(rawId)
  const valid =
    Number.isInteger(projectId) &&
    projectId > 0 &&
    projectId <= 2147483647 &&
    !!vaultId &&
    !!bundleId
  const base = `/vault/projects/${projectId}/vaults/${encodeURIComponent(vaultId)}`
  const back = `${base}?tab=bundles`
  const bundle = useQuery({
    ...accessBundleOptions(projectId, vaultId, bundleId),
    enabled: valid,
  })
  const vault = useQuery({
    ...getAccessVaultOptions({
      path: { project_id: projectId, vault_id: vaultId },
    }),
    enabled: valid,
  })
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    enabled: valid,
  })
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      { label: vault.data?.vault.name ?? 'Vault', href: base },
      { label: 'Access bundles', href: back },
      { label: bundle.data?.name ?? 'Access bundle' },
    ])
    return () => setBreadcrumbs([])
  }, [setBreadcrumbs, vault.data?.vault.name, bundle.data?.name, base, back])
  const error = bundle.error || vault.error
  const services =
    vault.data?.services.filter((service) =>
      bundle.data?.service_ids.includes(service.id)
    ) ?? []
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
        <Skeleton className="h-64" aria-label="Loading access bundle" />
      ) : (
        <Detail
          embedded
          title={bundle.data.name}
          verdict={<Status tone="idle" label="Approval required" />}
          actions={
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" asChild>
                <Link to={back}>All access bundles</Link>
              </Button>
              {capability.data?.can_manage_requests && !capability.isError && (
                <Button asChild>
                  <Link
                    to={`${base}/bundles/${encodeURIComponent(bundleId)}/sessions/new`}
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
                <Button disabled title="Project write permission is required">
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
              value: `${bundle.data.ttl_seconds / 60} min`,
            },
            { label: 'Version', value: bundle.data.version },
          ]}
          main={
            <div className="space-y-4">
              <p className="text-sm text-muted-foreground">
                This bundle defines a service selection and lifetime. Saving it
                grants no access.
              </p>
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
                    header: 'Requests',
                    render: (service) => (
                      <Status
                        tone={service.enabled ? 'idle' : 'warn'}
                        label={service.enabled ? 'Allowed' : 'Disabled'}
                      />
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

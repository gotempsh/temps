// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { serviceProviderName } from '@/components/service-access/service-provider-registry'
import {
  Clock3,
  Pencil,
  ScrollText,
  Link2,
  Plus,
  PlugZap,
  Layers,
  LockKeyhole,
  MoreHorizontal,
} from 'lucide-react'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  PageContainer,
  PageHeader,
  DataTable,
  RecordLink,
  Tabs,
  TabsList,
  TabsTrigger,
  TabsContent,
  PageState,
  ResponsivePagination,
} from '@temps-sdk/ds'
import {
  getAccessVaultGeneralOptions,
  getAccessVaultOptions,
  listAccessBundlesOptions,
  listAccessBundlesGeneralOptions,
  generalAccessCapabilityOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { AccessVaultResponse } from '@/api/client'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  ServiceProviderLogo,
  ServiceLogoStrip,
} from '@/components/service-access/service-providers'
import { AccessSessionsList } from '@/components/service-access/AccessSessionsList'
import { VaultLinks } from '@/components/service-access/VaultLinks'
import { VaultEditor } from '@/components/service-access/VaultEditor'
import { AuditLogView } from './AuditLogs-components'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'

export default function VaultDetailPage() {
  const { vaultId = '', projectId } = useParams()
  return (
    <VaultDetail
      key={`${projectId}:${vaultId}`}
      vaultId={vaultId}
      projectId={projectId === undefined ? undefined : Number(projectId)}
    />
  )
}
function VaultDetail({
  vaultId,
  projectId,
}: {
  vaultId: string
  projectId?: number
}) {
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  const valid =
    !!vaultId &&
    (projectId === undefined ||
      (Number.isSafeInteger(projectId) && projectId > 0))
  const base = `/vault${projectId === undefined ? '' : `/projects/${projectId}`}/vaults/${encodeURIComponent(vaultId)}`
  const requestedTab = params.get('tab') ?? 'services'
  const canViewAuditLogs = useCanViewAuditLogs()
  const page = Math.max(
    1,
    Math.min(1000000, Math.floor(Number(params.get('page')) || 1))
  )
  const sandboxId = params.get('sandbox') ?? undefined
  const consumerQuery = sandboxId
    ? `?sandbox=${encodeURIComponent(sandboxId)}`
    : ''
  const path = { vault_id: vaultId }
  const projectVault = useQuery({
    ...getAccessVaultOptions({ path: { ...path, project_id: projectId ?? 0 } }),
    enabled: valid && projectId !== undefined,
  })
  const generalVault = useQuery({
    ...getAccessVaultGeneralOptions({
      path,
      query: sandboxId ? { sandbox_id: sandboxId } : undefined,
    }),
    enabled: valid && projectId === undefined,
  })
  const vault = projectId === undefined ? generalVault : projectVault
  const capability = useQuery({
    ...generalAccessCapabilityOptions(),
    enabled: valid,
  })
  const projectBundles = useQuery({
    ...listAccessBundlesOptions({
      path: { ...path, project_id: projectId ?? 0 },
      query: { page, page_size: 20 },
    }),
    enabled: valid && projectId !== undefined && requestedTab === 'bundles',
  })
  const generalBundles = useQuery({
    ...listAccessBundlesGeneralOptions({
      path,
      query: { page, page_size: 20, sandbox_id: sandboxId },
    }),
    enabled: valid && projectId === undefined && requestedTab === 'bundles',
  })
  const bundles = projectId === undefined ? generalBundles : projectBundles
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vaults', href: `/vault` },
      { label: vault.data?.vault.name ?? 'Vault' },
    ])
    return () => setBreadcrumbs([])
  }, [vault.data?.vault.name, setBreadcrumbs])
  const [editingVault, setEditingVault] = useState<AccessVaultResponse>()
  const [editorOpen, setEditorOpen] = useState(false)
  const rows = vault.data?.services ?? []
  const canWrite =
    capability.data?.can_manage_services === true && !capability.isError
  const tab =
    requestedTab === 'bundles' ||
    requestedTab === 'sessions' ||
    (requestedTab === 'links' && canWrite) ||
    (requestedTab === 'audit' && canViewAuditLogs)
      ? requestedTab
      : 'services'
  const changeTab = (next: string) =>
    setParams((current) => {
      const p = new URLSearchParams(current)
      p.set('tab', next)
      p.delete('page')
      return p
    })
  return (
    <PageContainer width="full" innerClassName="space-y-4">
      <PageHeader
        title={
          <span className="inline-flex max-w-full items-center gap-2">
            <span className="min-w-0 [overflow-wrap:anywhere]">
              {vault.data?.vault.name ?? 'Vault'}
            </span>
            {canWrite && (
              <Button
                variant="ghost"
                size="icon"
                className="size-8 shrink-0 text-muted-foreground"
                aria-label="Edit vault"
                title="Edit vault"
                disabled={!vault.data || vault.isFetching || vault.isError}
                onClick={() => {
                  setEditingVault((selected) => selected ?? vault.data?.vault)
                  setEditorOpen(true)
                }}
              >
                <Pencil className="size-4" />
              </Button>
            )}
          </span>
        }
      />
      {editingVault && (
        <VaultEditor
          key={editingVault.id}
          vault={editingVault}
          open={editorOpen}
          onClose={() => setEditorOpen(false)}
          canWrite={canWrite}
        />
      )}
      {vault.isError && (
        <Callout tone="error" title="Could not load vault">
          <Button onClick={() => void vault.refetch()}>Retry</Button>
        </Callout>
      )}
      {!valid && (
        <Callout tone="error" title="Invalid vault link">
          Return to Vault to choose a vault.
        </Callout>
      )}
      {vault.isPending ? (
        <Skeleton className="h-48" />
      ) : (
        <Tabs value={tab} onValueChange={changeTab}>
          <TabsList>
            <TabsTrigger
              value="services"
              icon={PlugZap}
              count={vault.data?.vault.service_ids.length}
            >
              Services
            </TabsTrigger>
            <TabsTrigger value="bundles" icon={Layers}>
              Access bundles
            </TabsTrigger>
            <TabsTrigger value="sessions" icon={Clock3}>
              Sessions
            </TabsTrigger>
            {canWrite && (
              <TabsTrigger value="links" icon={Link2}>
                Linked to
              </TabsTrigger>
            )}
            {canViewAuditLogs && (
              <TabsTrigger value="audit" icon={ScrollText}>
                Audit logs
              </TabsTrigger>
            )}
          </TabsList>
          {canWrite && (
            <TabsContent value="links" forceMount hidden={tab !== 'links'}>
              <VaultLinks vaultId={vaultId} active={tab === 'links'} />
            </TabsContent>
          )}
          {canViewAuditLogs && (
            <TabsContent value="audit" forceMount hidden={tab !== 'audit'}>
              <AuditLogView resourceId={vaultId} active={tab === 'audit'} />
            </TabsContent>
          )}
          <TabsContent value="services" className="space-y-4">
            <div className="flex justify-end">
              <Button disabled={!canWrite} asChild={canWrite}>
                {canWrite ? (
                  <Link
                    to={`/access-services/new?vault=${encodeURIComponent(vaultId)}`}
                  >
                    <Plus className="size-4" />
                    Add service
                  </Link>
                ) : (
                  <span title="Instance administrator permission is required">
                    Add service
                  </span>
                )}
              </Button>
            </div>
            {rows.length === 0 && !vault.isError ? (
              <PageState
                variant="empty"
                icon={PlugZap}
                title="Add a service to this vault"
                description="Choose a provider, save its credential, and define which requests are allowed."
              />
            ) : (
              <DataTable
                aria-label="Vault services"
                rows={rows}
                rowKey={(r) => r.id}
                columns={[
                  {
                    key: 'name',
                    header: 'Service',
                    render: (r) => (
                      <div className="flex min-w-0 items-center gap-3">
                        <ServiceProviderLogo service={r} />
                        <div className="min-w-0">
                          {canWrite ? (
                            <RecordLink
                              className="font-medium break-words"
                              to={`/access-services/${encodeURIComponent(r.id)}/edit?vault=${encodeURIComponent(vaultId)}`}
                            >
                              {r.name}
                            </RecordLink>
                          ) : (
                            <span className="font-medium break-words">
                              {r.name}
                            </span>
                          )}
                          <p className="text-xs text-muted-foreground">
                            {serviceProviderName(r)} · v{r.version}
                          </p>
                          <p className="text-xs text-muted-foreground @xl:hidden break-all">
                            {r.policy.origin}
                          </p>
                        </div>
                      </div>
                    ),
                  },
                  {
                    key: 'type',
                    header: 'Type',
                    className: 'hidden @xl:table-cell',
                    render: (r) =>
                      r.policy.github_repository?.protocol === 'git'
                        ? 'Git HTTPS'
                        : 'HTTP API',
                  },
                  {
                    key: 'target',
                    header: 'Target',
                    className: 'hidden @xl:table-cell',
                    render: (r) => (
                      <span className="break-all rounded bg-muted px-2 py-1 text-xs font-mono">
                        {r.policy.origin.replace('https://', '')}
                      </span>
                    ),
                  },
                  {
                    key: 'credential',
                    header: 'Credential',
                    className: 'hidden @xl:table-cell',
                    render: (r) => (
                      <span className="inline-flex items-center gap-2 text-muted-foreground">
                        <LockKeyhole className="size-3" />
                        {r.policy.github_repository
                          ? 'Project connection'
                          : r.has_credential
                            ? 'Stored value'
                            : 'Not configured'}
                      </span>
                    ),
                  },
                  {
                    key: 'state',
                    header: 'Requests',
                    render: (r) => (
                      <span className="text-sm text-muted-foreground">
                        {r.enabled ? 'Allowed' : 'Disabled'}
                      </span>
                    ),
                  },
                ]}
              />
            )}
          </TabsContent>
          <TabsContent
            value="sessions"
            forceMount
            hidden={tab !== 'sessions'}
            className="space-y-4"
          >
            <div className="flex justify-end">
              <Button variant="outline" onClick={() => changeTab('bundles')}>
                Choose a bundle to create a session
              </Button>
            </div>
            {valid && (
              <AccessSessionsList
                key={vaultId}
                vaultId={vaultId}
                projectId={projectId}
                sandboxId={sandboxId}
                active={tab === 'sessions'}
              />
            )}
          </TabsContent>
          <TabsContent value="bundles" className="space-y-4">
            <div className="flex justify-end">
              <Button
                disabled={!canWrite}
                onClick={() => {
                  navigate(`${base}/bundles/new`)
                }}
              >
                <Plus className="size-4" />
                Create access bundle
              </Button>
            </div>
            {bundles.isError && (
              <Callout tone="error" title="Could not load access bundles">
                <Button onClick={() => void bundles.refetch()}>Retry</Button>
              </Callout>
            )}
            {bundles.isPending ? (
              <Skeleton className="h-40" />
            ) : bundles.data?.total === 0 ? (
              <PageState
                variant="empty"
                icon={Layers}
                title="No access bundles"
                description="Choose services and a maximum lifetime for new sessions."
              />
            ) : (
              <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
                {bundles.data?.items.map((b) => (
                  <div
                    key={b.id}
                    className="relative rounded-lg border transition-colors hover:border-foreground/30 focus-within:ring-2 focus-within:ring-ring"
                  >
                    <div className="space-y-3 p-4">
                      <div className="flex items-center justify-between gap-2">
                        <h2 className="min-w-0 break-words font-semibold">
                          <Link
                            to={`${base}/bundles/${encodeURIComponent(b.id)}${consumerQuery}`}
                            className="after:absolute after:inset-0 focus:outline-none"
                          >
                            {b.name}
                          </Link>
                        </h2>
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              className="relative z-10"
                              aria-label={`Actions for ${b.name}`}
                            >
                              <MoreHorizontal className="size-4" />
                            </Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            <DropdownMenuItem
                              disabled={
                                !canWrite ||
                                bundles.isFetching ||
                                bundles.isError
                              }
                              onSelect={() => {
                                navigate(
                                  `${base}/bundles/${encodeURIComponent(b.id)}/edit`
                                )
                              }}
                            >
                              Edit bundle
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </div>
                      <ServiceLogoStrip
                        services={rows.filter((s) =>
                          b.service_ids.includes(s.id)
                        )}
                        total={b.service_ids.length}
                      />
                    </div>
                    <div className="flex items-center justify-between gap-2 border-t p-3">
                      <span className="text-xs text-muted-foreground">
                        Up to {b.ttl_seconds / 60} min
                      </span>
                      <span className="text-xs text-muted-foreground">
                        View bundle →
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            )}
            {(bundles.data?.total ?? 0) > 20 && (
              <ResponsivePagination
                page={page}
                pageSize={20}
                total={bundles.data?.total ?? 0}
                totalPages={Math.ceil((bundles.data?.total ?? 0) / 20)}
                onPageChange={(p) =>
                  setParams((current) => {
                    const next = new URLSearchParams(current)
                    next.set('tab', 'bundles')
                    next.set('page', String(p))
                    return next
                  })
                }
              />
            )}
          </TabsContent>
        </Tabs>
      )}
    </PageContainer>
  )
}

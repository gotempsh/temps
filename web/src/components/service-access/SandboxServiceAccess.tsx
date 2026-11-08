// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { PlugZap } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  PageState,
  SettingsSection,
  Status,
} from '@temps-sdk/ds'
import {
  accessCapabilityOptions,
  listAccessServicesOptions,
  listServiceGrantsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  revokeServiceGrant,
  type AccessServiceResponse,
  type ServiceGrantResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { AccessServiceEditor } from './AccessServiceEditor'
import { ServiceGrantEditor } from './ServiceGrantEditor'
import { grantLabels } from './service-access-form'

function pageNumber(raw: string | null) {
  const parsed = Number(raw)
  return Number.isInteger(parsed) && parsed > 0 ? Math.min(parsed, 1000000) : 1
}

export function SandboxServiceAccess({
  projectId,
  sandboxId,
}: {
  projectId?: number | null
  sandboxId: string
}) {
  const client = useQueryClient()
  const [params, setParams] = useSearchParams()
  const servicePage = pageNumber(params.get('accessServicesPage'))
  const grantPage = pageNumber(params.get('accessRequestsPage'))
  const [revokeTarget, setRevokeTarget] = useState<ServiceGrantResponse>()
  const [editorOpen, setEditorOpen] = useState(false)
  const [editing, setEditing] = useState<AccessServiceResponse>()
  const [requesting, setRequesting] = useState<AccessServiceResponse>()
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId ?? 0 } }),
    enabled: !!projectId,
  })
  const services = useQuery({
    ...listAccessServicesOptions({
      path: { project_id: projectId ?? 0 },
      query: { page: servicePage, page_size: 20 },
    }),
    enabled: !!projectId,
  })
  const grants = useQuery({
    ...listServiceGrantsOptions({
      path: { project_id: projectId ?? 0, sandbox_id: sandboxId },
      query: { page: grantPage, page_size: 20 },
    }),
    enabled: !!projectId,
    refetchInterval: 15000,
  })
  const refresh = () => {
    void client.invalidateQueries({ queryKey: [{ _id: 'accessCapability' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listAccessServices' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listServiceGrants' }] })
  }
  const revoke = useMutation({
    mutationFn: async (grant: ServiceGrantResponse) => {
      if (!projectId) throw new Error('This sandbox is not linked to a project')
      return revokeServiceGrant({
        path: {
          project_id: projectId,
          sandbox_id: sandboxId,
          grant_id: grant.id,
        },
        body: { expected_version: grant.version },
        throwOnError: true,
      })
    },
    onSuccess: () => {
      setRevokeTarget(undefined)
      toast.success('Saved access request revoked')
      refresh()
    },
    onError: (error) =>
      toast.error('Could not revoke request', {
        description: problemDetail(
          error,
          'Reload and check your permissions before trying again.'
        ),
      }),
  })
  const changePage = (key: string, page: number) =>
    setParams((current) => {
      const next = new URLSearchParams(current)
      next.set(key, String(page))
      return next
    })
  const maySave = capability.data?.can_save_requests === true
  const serviceRows = services.data?.items ?? []
  const grantRows = grants.data?.items ?? []
  const readError = capability.error || services.error || grants.error

  return (
    <SettingsSection
      id="sandbox-service-access"
      title="Service access"
      icon={PlugZap}
    >
      {!projectId ? (
        <Callout title="A project is required">
          Create a sandbox from a project to save scoped service policies and
          time-limited requests.{' '}
          <Link className="underline" to="/projects">
            Open projects
          </Link>
        </Callout>
      ) : (
        <div className="space-y-6">
          {capability.isLoading && <Skeleton className="h-20 w-full" />}
          {capability.data && (
            <Callout tone="warning" title="Sandbox connections unavailable">
              {capability.data.reason}
              {capability.data.setup_path && (
                <Link
                  className="ml-1 underline"
                  to={capability.data.setup_path}
                >
                  Set up service access
                </Link>
              )}
            </Callout>
          )}
          {readError && (
            <Callout tone="error" title="Could not load service access">
              {problemDetail(
                readError,
                'Check project and sandbox read permissions.'
              )}
              <Button size="sm" variant="outline" onClick={refresh}>
                Retry
              </Button>
            </Callout>
          )}
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h4 className="text-sm font-medium">Project services</h4>
            <Button
              size="sm"
              disabled={!maySave}
              onClick={() => {
                setEditing(undefined)
                setEditorOpen(true)
              }}
            >
              Add service
            </Button>
          </div>
          {!services.isPending &&
          !services.isError &&
          servicePage === 1 &&
          services.data?.total === 0 ? (
            <PageState
              variant="empty"
              size="compact"
              icon={PlugZap}
              title="No services saved"
              description="Add an HTTPS origin, allowed requests, and an optional bearer credential."
            />
          ) : (
            <DataTable
              aria-label="Project access services"
              rows={serviceRows}
              rowKey={(row) => row.id}
              isLoading={services.isLoading}
              columns={[
                {
                  key: 'service',
                  header: 'Service',
                  render: (row) => (
                    <div className="min-w-0 space-y-1 [overflow-wrap:anywhere]">
                      <p className="font-medium">{row.name}</p>
                      <p className="text-xs text-muted-foreground">
                        {row.policy.origin}
                      </p>
                      <p className="text-xs text-muted-foreground">
                        {row.policy.methods.join(', ')} ·{' '}
                        {row.policy.path_prefixes.join(', ')}
                      </p>
                      <p className="text-xs text-muted-foreground">
                        Maximum {row.max_ttl_seconds}s ·{' '}
                        {row.has_credential
                          ? 'Credential stored'
                          : 'No credential'}{' '}
                        · Version {row.version}
                      </p>
                      <Status
                        tone={row.enabled ? 'idle' : 'warn'}
                        label={row.enabled ? 'Requests allowed' : 'Disabled'}
                      />
                    </div>
                  ),
                },
                {
                  key: 'actions',
                  header: 'Actions',
                  render: (row) => (
                    <div className="flex flex-wrap gap-2">
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={!maySave}
                        onClick={() => {
                          setEditing(row)
                          setEditorOpen(true)
                        }}
                      >
                        Edit
                      </Button>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={!maySave || !row.enabled}
                        onClick={() => setRequesting(row)}
                      >
                        Request access
                      </Button>
                    </div>
                  ),
                },
              ]}
              pagination={{
                page: servicePage,
                pageSize: 20,
                total: services.data?.total ?? 0,
                totalPages: Math.max(
                  1,
                  Math.ceil((services.data?.total ?? 0) / 20)
                ),
                onPageChange: (page) => changePage('accessServicesPage', page),
                ariaLabel: 'Service pages',
              }}
            />
          )}
          <h4 className="text-sm font-medium">Sandbox access requests</h4>
          {!grants.isPending &&
          !grants.isError &&
          grantPage === 1 &&
          grants.data?.total === 0 ? (
            <PageState
              variant="empty"
              size="compact"
              icon={PlugZap}
              title="No access requests"
              description="Choose Request access on a service and set a lifetime. Saved requests do not connect this sandbox yet."
            />
          ) : (
            <DataTable
              aria-label="Sandbox access requests"
              rows={grantRows}
              rowKey={(row) => row.id}
              isLoading={grants.isLoading}
              columns={[
                {
                  key: 'request',
                  header: 'Request',
                  render: (row) => (
                    <div className="min-w-0 space-y-1 [overflow-wrap:anywhere]">
                      <p className="font-medium">{row.service_name}</p>
                      <Status
                        tone={
                          row.status === 'integration_unavailable' ||
                          row.status === 'policy_changed'
                            ? 'warn'
                            : 'idle'
                        }
                        label={grantLabels[row.status]}
                      />
                      <p className="text-xs text-muted-foreground">
                        {row.reason}
                      </p>
                      <p className="text-xs text-muted-foreground">
                        Expires {new Date(row.expires_at).toLocaleString()} ·
                        Service version {row.service_version}
                      </p>
                      <p className="text-xs text-muted-foreground">
                        {row.policy.origin} · {row.policy.methods.join(', ')} ·{' '}
                        {row.policy.path_prefixes.join(', ')}
                      </p>
                    </div>
                  ),
                },
                {
                  key: 'actions',
                  header: 'Actions',
                  render: (row) => (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={row.status === 'revoked' || revoke.isPending}
                      onClick={() => setRevokeTarget(row)}
                    >
                      Revoke request
                    </Button>
                  ),
                },
              ]}
              pagination={{
                page: grantPage,
                pageSize: 20,
                total: grants.data?.total ?? 0,
                totalPages: Math.max(
                  1,
                  Math.ceil((grants.data?.total ?? 0) / 20)
                ),
                onPageChange: (page) => changePage('accessRequestsPage', page),
                ariaLabel: 'Access request pages',
              }}
            />
          )}
        </div>
      )}
      <ConfirmDeleteDialog
        open={!!revokeTarget}
        onOpenChange={(open) => {
          if (!open) setRevokeTarget(undefined)
        }}
        resourceType="access request"
        resourceName={revokeTarget?.service_name ?? ''}
        verb="Revoke"
        consequences="This cancels the saved request. Create a new request if you need access later."
        isPending={revoke.isPending}
        onConfirm={() => {
          if (revokeTarget) revoke.mutate(revokeTarget)
        }}
      />
      <AccessServiceEditor
        projectId={projectId}
        open={editorOpen}
        service={editing}
        onClose={() => setEditorOpen(false)}
        onSaved={refresh}
      />
      <ServiceGrantEditor
        projectId={projectId}
        sandboxId={sandboxId}
        service={requesting}
        onClose={() => setRequesting(undefined)}
        onSaved={refresh}
      />
    </SettingsSection>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MoreHorizontal, PlugZap } from 'lucide-react'
import { toast } from 'sonner'
import { Button, Skeleton } from '@temps-sdk/ui'
import { Callout, DataTable, PageState, Status } from '@temps-sdk/ds'
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
import { CodeBlock } from '@/components/ui/code-block'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ServiceProviderLogo } from './service-providers'
import { ServiceGrantEditor } from './ServiceGrantEditor'
import { grantLabels, enforcementLabels } from './service-access-form'

function pageNumber(raw: string | null) {
  const parsed = Number(raw)
  return Number.isInteger(parsed) && parsed > 0 ? Math.min(parsed, 1000000) : 1
}

type PanelProps = {
  projectId?: number | null
  sandboxId?: string
  manageServices?: boolean
  defaultTtl?: number
  selectedServices?: AccessServiceResponse[]
}

export function SandboxServiceAccess(props: PanelProps) {
  return (
    <ServiceAccessPanel
      key={`${props.projectId ?? 'none'}:${props.sandboxId}`}
      {...props}
    />
  )
}

function ServiceAccessPanel({
  projectId,
  sandboxId,
  selectedServices,
  defaultTtl,
  manageServices = true,
}: PanelProps) {
  const client = useQueryClient()
  const [params, setParams] = useSearchParams()
  const servicePage = pageNumber(params.get('accessServicesPage'))
  const grantPage = pageNumber(params.get('accessRequestsPage'))
  const [revokeTarget, setRevokeTarget] = useState<ServiceGrantResponse>()
  const navigate = useNavigate()
  const openEditor = (serviceId?: string) => {
    const suffix = serviceId ? `${encodeURIComponent(serviceId)}/edit` : 'new'
    const query = new URLSearchParams(sandboxId ? { sandbox: sandboxId } : {})
    navigate(`/projects/${projectId}/access-services/${suffix}?${query}`)
  }
  const [grantMode, setGrantMode] = useState<'request' | 'approve'>('request')
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
    enabled: !!projectId && !selectedServices,
  })
  const grants = useQuery({
    ...listServiceGrantsOptions({
      path: { project_id: projectId ?? 0, sandbox_id: sandboxId ?? '' },
      query: { page: grantPage, page_size: 20 },
    }),
    enabled: !!projectId && !!sandboxId,
    refetchInterval: 15000,
  })
  const refresh = () => {
    void client.invalidateQueries({ queryKey: [{ _id: 'accessCapability' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listAccessServices' }] })
    void client.invalidateQueries({ queryKey: [{ _id: 'listServiceGrants' }] })
  }
  const revoke = useMutation({
    mutationFn: async (grant: ServiceGrantResponse) => {
      if (!projectId || !sandboxId)
        throw new Error('This sandbox is not linked to a project')
      return revokeServiceGrant({
        path: {
          project_id: projectId,
          sandbox_id: sandboxId ?? '',
          grant_id: grant.id,
        },
        body: { expected_version: grant.version },
        throwOnError: true,
      })
    },
    onSuccess: () => {
      setRevokeTarget(undefined)
      toast.success('Revocation requested', {
        description: 'Waiting for worker confirmation or signed lease expiry.',
      })
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
  const mayEdit = maySave && capability.data?.can_manage_services === true
  const mayRequest =
    !!sandboxId && maySave && capability.data?.can_manage_requests === true
  const mayRevoke = capability.data?.can_manage_requests === true
  const serviceRows = selectedServices ?? services.data?.items ?? []
  const grantRows = grants.data?.items ?? []
  const readError =
    capability.error || (!selectedServices && services.error) || grants.error

  return (
    <section aria-label="Service access">
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
            <Callout
              tone={capability.data.configured ? 'info' : 'warning'}
              title={
                capability.data.configured
                  ? 'Approve sandbox service access'
                  : 'Sandbox connections unavailable'
              }
            >
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
          {capability.data && !capability.data.can_manage_services && (
            <Callout title="Service policies are read-only">
              Editing services requires project write permission. Ask a project
              administrator for access.
            </Callout>
          )}
          {capability.data?.configured && (
            <details className="rounded-md border p-3 text-sm">
              <summary className="cursor-pointer font-medium">
                Use an approved service from Terminal
              </summary>
              <p className="my-3 text-muted-foreground">
                Wait for Connected, then run the command inside this sandbox.
                Replace the example URL with an approved destination. No API
                token is needed. These options apply only to this command; other
                traffic keeps the sandbox’s existing network policy.
              </p>
              <CodeBlock
                language="shell"
                title="HTTPS API"
                code={
                  'curl --proxy http://127.0.0.1:3128 --cacert /run/temps-service-access/ca.pem "https://api.example.com/v1"'
                }
              />
              <CodeBlock
                language="shell"
                title="GitHub clone (read-only Git grant)"
                code={
                  'GIT_TERMINAL_PROMPT=0 git -c credential.helper= -c http.proxy=http://127.0.0.1:3128 -c http.sslCAInfo=/run/temps-service-access/ca.pem clone "https://github.com/<owner>/<private-repo>.git"'
                }
              />
            </details>
          )}
          {capability.data && !capability.data.can_manage_requests && (
            <Callout title="Access requests are read-only">
              Saving or revoking requests requires project and sandbox write
              permission. Ask a project administrator for access.
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
            <h4 className="text-sm font-medium">
              {selectedServices ? 'Selected services' : 'Project services'}
            </h4>
            {manageServices && !selectedServices && (
              <Button
                size="sm"
                disabled={!mayEdit}
                onClick={() => openEditor()}
              >
                Add service
              </Button>
            )}
          </div>
          {(!!selectedServices || (!services.isPending && !services.isError)) &&
          servicePage === 1 &&
          (selectedServices
            ? selectedServices.length === 0
            : services.data?.total === 0) ? (
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
              isLoading={!selectedServices && services.isLoading}
              columns={[
                {
                  key: 'service',
                  header: 'Service',
                  render: (row) => (
                    <div className="min-w-0 space-y-1 [overflow-wrap:anywhere]">
                      <div className="flex items-center gap-2">
                        <ServiceProviderLogo service={row} />
                        <p className="font-medium">{row.name}</p>
                      </div>
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
                    <div className="flex flex-wrap justify-end gap-2">
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={
                          !mayRequest ||
                          !row.enabled ||
                          (capability.data?.configured &&
                            !!row.policy.github_repository &&
                            !capability.data.can_use_project_github)
                        }
                        onClick={() => {
                          setGrantMode(
                            capability.data?.configured ? 'approve' : 'request'
                          )
                          setRequesting(row)
                        }}
                      >
                        {capability.data?.configured
                          ? 'Grant access'
                          : 'Request access'}
                      </Button>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button
                            size="icon"
                            variant="ghost"
                            aria-label={`Actions for ${row.name}`}
                          >
                            <MoreHorizontal
                              className="size-4"
                              aria-hidden="true"
                            />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          {capability.data?.configured && (
                            <DropdownMenuItem
                              disabled={!mayRequest || !row.enabled}
                              onSelect={() => {
                                setGrantMode('request')
                                setRequesting(row)
                              }}
                            >
                              Save request for review
                            </DropdownMenuItem>
                          )}
                          {manageServices && (
                            <DropdownMenuItem
                              disabled={!mayEdit}
                              onSelect={() => openEditor(row.id)}
                            >
                              Edit service
                            </DropdownMenuItem>
                          )}
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  ),
                },
              ]}
              pagination={
                selectedServices
                  ? undefined
                  : {
                      page: servicePage,
                      pageSize: 20,
                      total: services.data?.total ?? 0,
                      totalPages: Math.max(
                        1,
                        Math.ceil((services.data?.total ?? 0) / 20)
                      ),
                      onPageChange: (page) =>
                        changePage('accessServicesPage', page),
                      ariaLabel: 'Service pages',
                    }
              }
            />
          )}
          <h4 className="text-sm font-medium">Sandbox access requests</h4>
          {!sandboxId ? (
            <p className="text-sm text-muted-foreground">
              Select a sandbox below to manage its grants.
            </p>
          ) : (
            <>
              {!grants.isPending &&
              !grants.isError &&
              grantPage === 1 &&
              grants.data?.total === 0 ? (
                <PageState
                  variant="empty"
                  size="compact"
                  icon={PlugZap}
                  title="No access requests"
                  description="Choose a service and review its policy and lifetime. Operator approval is separate from saving a request."
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
                              row.enforcement === 'active'
                                ? 'ok'
                                : row.enforcement === 'stopping' ||
                                    row.status === 'integration_unavailable' ||
                                    row.status === 'policy_changed'
                                  ? 'warn'
                                  : 'idle'
                            }
                            label={
                              row.enforcement &&
                              row.enforcement !== 'not_requested'
                                ? enforcementLabels[row.enforcement]
                                : grantLabels[row.status]
                            }
                          />
                          <p className="text-xs text-muted-foreground">
                            {row.reason}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            Expires {new Date(row.expires_at).toLocaleString()}{' '}
                            · Service version {row.service_version}
                          </p>
                          <p className="text-xs text-muted-foreground">
                            {row.policy.origin} ·{' '}
                            {row.policy.methods.join(', ')} ·{' '}
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
                          disabled={
                            !mayRevoke ||
                            row.status === 'revoked' ||
                            revoke.isPending
                          }
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
                    onPageChange: (page) =>
                      changePage('accessRequestsPage', page),
                    ariaLabel: 'Access request pages',
                  }}
                />
              )}
            </>
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
        consequences="This requests removal of the grant. Access stops after worker confirmation or signed lease expiry, within 30 seconds without renewal. New access requires another approval."
        isPending={revoke.isPending}
        onConfirm={() => {
          if (mayRevoke && revokeTarget) revoke.mutate(revokeTarget)
        }}
      />
      <ServiceGrantEditor
        defaultTtl={defaultTtl}
        projectId={projectId}
        sandboxId={sandboxId ?? ''}
        service={mayRequest ? requesting : undefined}
        mode={grantMode}
        onClose={() => setRequesting(undefined)}
        onSaved={refresh}
      />
    </section>
  )
}

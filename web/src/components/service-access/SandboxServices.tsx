// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { Link } from 'react-router'
import { useForm, useWatch } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Button,
  Input,
  Skeleton,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import {
  Callout,
  DataTable,
  Field,
  Status,
  ResponsivePagination,
} from '@temps-sdk/ds'
import {
  accessCapabilityOptions,
  listSandboxServicesOptions,
  listAccessServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  requestSandboxService,
  approveSandboxService,
  revokeSandboxService,
  type SandboxServiceResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import {
  approvalExpirySchema,
  serviceAccessLabel,
} from './sandbox-services-state'

const schema = z.object({ serviceId: z.string().min(1, 'Choose a service') })
export function SandboxServices({
  projectId,
  sandboxId,
}: {
  projectId: number
  sandboxId: string
}) {
  const client = useQueryClient()
  const [page, setPage] = useState(1)
  const [servicePage, setServicePage] = useState(1)
  const approvalForm = useForm<z.infer<typeof approvalExpirySchema>>({
    resolver: zodResolver(approvalExpirySchema),
    defaultValues: { deadline: '' },
  })
  const path = { project_id: projectId, sandbox_id: sandboxId }
  const options = listSandboxServicesOptions({
    path,
    query: { page, page_size: 20 },
  })
  const capability = useQuery({
    ...accessCapabilityOptions({ path: { project_id: projectId } }),
    refetchInterval: 5000,
  })
  const unavailable = capability.data?.configured === false
  const access = useQuery({ ...options, refetchInterval: 5000 })
  const services = useQuery(
    listAccessServicesOptions({
      path: { project_id: projectId },
      query: { page: servicePage, page_size: 20 },
    })
  )
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: { serviceId: '' },
  })
  const selectedServiceId = useWatch({
    control: form.control,
    name: 'serviceId',
  })
  const refresh = () => client.invalidateQueries({ queryKey: options.queryKey })
  const request = useMutation({
    mutationFn: async ({ serviceId }: z.infer<typeof schema>) => {
      const service = services.data?.items.find((s) => s.id === serviceId)
      if (!service) throw new Error('Reload services and choose a service')
      return requestSandboxService({
        path,
        body: { service_id: service.id, service_version: service.version },
        throwOnError: true,
      })
    },
    onSuccess: refresh,
  })
  const approve = useMutation({
    mutationFn: ({
      row,
      deadline,
    }: {
      row: SandboxServiceResponse
      deadline: string
    }) =>
      approveSandboxService({
        path: { ...path, access_id: row.id },
        body: {
          expected_version: row.version,
          service_version: row.service_version,
          expires_at: deadline ? new Date(deadline).toISOString() : null,
        },
        throwOnError: true,
      }),
    onSuccess: refresh,
  })
  const revoke = useMutation({
    mutationFn: (row: SandboxServiceResponse) =>
      revokeSandboxService({
        path: { ...path, access_id: row.id },
        body: { expected_version: row.version },
        throwOnError: true,
      }),
    onSuccess: refresh,
  })
  const error =
    access.error ??
    services.error ??
    request.error ??
    approve.error ??
    revoke.error
  return (
    <div className="space-y-4">
      {capability.isPending && (
        <Skeleton
          className="h-20 w-full"
          aria-label="Checking service access support"
        />
      )}
      {capability.isError && (
        <Callout title="Could not check service access support" tone="error">
          {problemDetail(
            capability.error,
            'Retry to check whether this environment supports service access.'
          )}
          <Button variant="outline" onClick={() => void capability.refetch()}>
            Retry capability check
          </Button>
        </Callout>
      )}
      {capability.data && (
        <Callout
          title={
            unavailable
              ? 'Sandbox connections unavailable'
              : 'Service access support'
          }
          tone={unavailable ? 'warning' : 'info'}
        >
          {capability.data.reason}
          {capability.data.setup_path && (
            <Link className="ml-1 underline" to={capability.data.setup_path}>
              Set up service access
            </Link>
          )}
        </Callout>
      )}
      {error && (
        <Callout title="Service access could not be updated" tone="error">
          {problemDetail(error, 'Check your permissions and retry.')}
        </Callout>
      )}
      <form
        className="flex flex-col gap-2 sm:flex-row sm:items-end"
        onSubmit={form.handleSubmit((values) => request.mutate(values))}
      >
        <Field label="Service" error={form.formState.errors.serviceId?.message}>
          {(props) => (
            <Select
              value={selectedServiceId}
              onValueChange={(value) =>
                form.setValue('serviceId', value, { shouldValidate: true })
              }
            >
              <SelectTrigger {...props} className="w-full sm:w-64">
                <SelectValue placeholder="Choose a Vault service" />
              </SelectTrigger>
              <SelectContent>
                {services.data?.items.map((service) => (
                  <SelectItem key={service.id} value={service.id}>
                    {service.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </Field>
        <Button
          type="submit"
          aria-disabled={request.isPending}
          onClick={(event) => {
            if (request.isPending) event.preventDefault()
          }}
        >
          {request.isPending ? 'Requesting…' : 'Request access'}
        </Button>
      </form>
      {services.data && services.data.total > 20 && (
        <ResponsivePagination
          page={servicePage}
          pageSize={20}
          total={services.data.total}
          totalPages={Math.ceil(services.data.total / 20)}
          onPageChange={setServicePage}
        />
      )}
      {services.data?.total === 0 && (
        <Callout title="No services configured">
          Add a service and its credential in Vault, then request access here or
          in chat.
        </Callout>
      )}
      <Field
        label="Approval expiry (optional)"
        error={approvalForm.formState.errors.deadline?.message}
        description="Leave empty for the sandbox lifetime. Internal leases renew automatically."
      >
        {(props) => (
          <Input
            {...props}
            type="datetime-local"
            {...approvalForm.register('deadline')}
          />
        )}
      </Field>
      <DataTable
        aria-label="Sandbox services"
        rows={access.data?.items ?? []}
        rowKey={(row) => row.id}
        isLoading={access.isPending}
        columns={[
          {
            key: 'service',
            header: 'Service',
            render: (row) => (
              <div className="space-y-1">
                <span className="font-medium">{row.service_name}</span>
                <p className="text-xs text-muted-foreground break-all">
                  {row.policy.origin} · {row.policy.methods.join(', ')} ·{' '}
                  {row.policy.path_prefixes.join(', ')}
                </p>
                <p className="text-xs text-muted-foreground">
                  {row.expires_at
                    ? `Until ${new Date(row.expires_at).toLocaleString()}`
                    : 'Sandbox lifetime'}
                </p>
              </div>
            ),
          },
          {
            key: 'status',
            header: 'Status',
            render: (row) => (
              <div>
                <Status
                  label={
                    unavailable && !row.connected && row.status === 'approved'
                      ? 'Unavailable'
                      : serviceAccessLabel(row.status, row.connected)
                  }
                  tone={
                    row.connected
                      ? 'ok'
                      : row.status === 'approved' && !unavailable
                        ? 'running'
                        : 'idle'
                  }
                />
                <p className="text-xs text-muted-foreground mt-1">
                  {unavailable && !row.connected && row.status === 'approved'
                    ? capability.data?.reason
                    : row.reason}
                </p>
              </div>
            ),
          },
          {
            key: 'actions',
            header: 'Actions',
            render: (row) => (
              <div className="flex flex-wrap gap-2">
                {row.status !== 'approved' && row.status !== 'suspended' && (
                  <Button
                    aria-disabled={approve.isPending}
                    onClick={approvalForm.handleSubmit(({ deadline }) => {
                      if (!approve.isPending) approve.mutate({ row, deadline })
                    })}
                  >
                    Approve access
                  </Button>
                )}
                {row.status !== 'revoked' && (
                  <Button
                    variant="outline"
                    aria-disabled={revoke.isPending}
                    onClick={() => {
                      if (!revoke.isPending) revoke.mutate(row)
                    }}
                  >
                    Revoke
                  </Button>
                )}
              </div>
            ),
          },
        ]}
        pagination={{
          page,
          pageSize: 20,
          total: access.data?.total ?? 0,
          totalPages: Math.max(1, Math.ceil((access.data?.total ?? 0) / 20)),
          onPageChange: setPage,
        }}
      />
    </div>
  )
}

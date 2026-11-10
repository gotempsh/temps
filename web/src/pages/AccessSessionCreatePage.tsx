// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Box } from 'lucide-react'
import { Button, Input, Skeleton } from '@temps-sdk/ui'
import {
  Callout,
  Field,
  PageContainer,
  ResponsivePagination,
  Settings,
  Status,
} from '@temps-sdk/ds'
import {
  createAccessSessionGeneral,
  createVaultLink,
  type AccessBundleResponse,
  type AccessServiceResponse,
} from '@/api/client'
import {
  generalAccessCapabilityOptions,
  getVaultEligibilityOptions,
  getAccessVaultGeneralOptions,
  listSandboxesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { accessBundleOptions } from '@/components/service-access/access-bundle-query'
import { ServiceProviderLogo } from '@/components/service-access/service-providers'
import { sessionHref } from '@/components/service-access/session-navigation'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { problemDetail } from '@/lib/api-problem'

const schema = z.object({
  sandbox_id: z.string().min(1, 'Choose a running sandbox'),
  ttl_seconds: z.number().int().min(60).max(86400),
})
type Snapshot = {
  bundle: AccessBundleResponse
  services: AccessServiceResponse[]
}
export default function AccessSessionCreatePage() {
  const { vaultId = '', bundleId = '' } = useParams()
  return (
    <SessionCreate
      key={`${vaultId}:${bundleId}`}
      vaultId={vaultId}
      bundleId={bundleId}
    />
  )
}
function SessionCreate({
  vaultId,
  bundleId,
}: {
  vaultId: string
  bundleId: string
}) {
  const [search] = useSearchParams()
  const sandboxId = search.get('sandbox') ?? undefined
  const consumerQuery = sandboxId
    ? `?sandbox=${encodeURIComponent(sandboxId)}`
    : ''
  const valid = !!vaultId && !!bundleId
  const base = `/vault/vaults/${encodeURIComponent(vaultId)}`
  const back = `${base}/bundles/${encodeURIComponent(bundleId)}${consumerQuery}`
  const bundle = useQuery({
    ...accessBundleOptions(vaultId, bundleId, sandboxId),
    enabled: valid,
  })
  const vault = useQuery({
    ...getAccessVaultGeneralOptions({
      path: { vault_id: vaultId },
      query: sandboxId ? { sandbox_id: sandboxId } : undefined,
    }),
    enabled: valid,
    refetchOnMount: 'always',
  })
  const capability = useQuery({
    ...generalAccessCapabilityOptions(),
    enabled: valid,
  })
  const [snapshot, setSnapshot] = useState<Snapshot>()
  if (
    !snapshot &&
    bundle.isFetchedAfterMount &&
    bundle.isSuccess &&
    vault.isFetchedAfterMount &&
    vault.isSuccess
  ) {
    setSnapshot({
      bundle: bundle.data,
      services: vault.data.services.filter((s) =>
        bundle.data.service_ids.includes(s.id)
      ),
    })
  }
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Vault', href: '/vault' },
      {
        label: vault.data?.vault.name ?? 'Vault',
        href: `${base}${consumerQuery}`,
      },
      { label: bundle.data?.name ?? 'Access bundle', href: back },
      { label: 'Create session' },
    ])
    return () => setBreadcrumbs([])
  }, [
    setBreadcrumbs,
    base,
    back,
    consumerQuery,
    bundle.data?.name,
    vault.data?.vault.name,
  ])
  const error = bundle.error || vault.error || capability.error
  const canIssue =
    !error &&
    capability.data?.configured === true &&
    capability.data.can_manage_requests === true
  return (
    <PageContainer width="form">
      {!valid ? (
        <Callout tone="error" title="Invalid session link">
          <Link to="/vault">Back to Vault</Link>
        </Callout>
      ) : (
        <>
          {error && (
            <Callout tone="error" title="Could not load session configuration">
              {problemDetail(
                error,
                'Your selections are preserved. Retry before approving.'
              )}
              <Button
                onClick={() => {
                  void bundle.refetch()
                  void vault.refetch()
                  void capability.refetch()
                }}
              >
                Retry
              </Button>
            </Callout>
          )}
          {capability.data && !capability.data.configured && (
            <Callout title="Sandbox session transport unavailable">
              {capability.data.reason}
            </Callout>
          )}
          {capability.data && !capability.data.can_manage_requests && (
            <Callout title="Approval permission required">
              Sandbox write permission and a covering vault link are required.
              Project links also require project access.
            </Callout>
          )}
          {snapshot ? (
            <SessionForm
              vaultId={vaultId}
              initialSandboxId={sandboxId}
              snapshot={snapshot}
              canManage={capability.data?.can_manage_services === true}
              canIssue={canIssue}
              back={back}
            />
          ) : (
            !error && <Skeleton className="h-64" />
          )}
        </>
      )}
    </PageContainer>
  )
}
function SessionForm({
  vaultId,
  snapshot,
  initialSandboxId,
  canManage,
  canIssue,
  back,
}: {
  vaultId: string
  snapshot: Snapshot
  initialSandboxId?: string
  canManage: boolean
  canIssue: boolean
  back: string
}) {
  const navigate = useNavigate()
  const client = useQueryClient()
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
  const [page, setPage] = useState(1)
  const sandboxes = useQuery({
    ...listSandboxesOptions({
      query: { page, page_size: 20 },
    }),
    refetchInterval: 10000,
  })
  const maxTtl = Math.min(
    snapshot.bundle.ttl_seconds,
    ...snapshot.services.map((s) => s.max_ttl_seconds)
  )
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: {
      sandbox_id: initialSandboxId ?? '',
      ttl_seconds: Math.min(maxTtl, 900),
    },
  })
  const selected = useWatch({ control: form.control, name: 'sandbox_id' })
  const eligibility = useQuery({
    ...getVaultEligibilityOptions({
      path: { vault_id: vaultId, sandbox_id: selected },
    }),
    enabled: !!selected,
    retry: false,
  })
  const link = useMutation({
    mutationFn: () =>
      createVaultLink({
        path: { vault_id: vaultId },
        body: { sandbox_id: selected },
        throwOnError: true,
      }),
    onSuccess: () => {
      void eligibility.refetch()
    },
  })
  const consent = useRef<
    { fingerprint: string; requestId: string } | undefined
  >(undefined)
  const issue = useMutation({
    mutationFn: async (values: z.infer<typeof schema>) => {
      if (!canIssue || !eligibility.isSuccess || eligibility.isFetching)
        throw new Error(
          'Session approval is unavailable. Check your permissions and refresh.'
        )
      if (values.ttl_seconds > maxTtl)
        throw new Error(`Choose a lifetime of at most ${maxTtl} seconds.`)
      const body = {
        bundle_id: snapshot.bundle.id,
        bundle_version: snapshot.bundle.version,
        consumer: { kind: 'sandbox' as const, sandbox_id: values.sandbox_id },
        ttl_seconds: values.ttl_seconds,
        services: snapshot.services.map((s) => ({
          service_id: s.id,
          version: s.version,
        })),
      }
      const fingerprint = JSON.stringify(body)
      if (consent.current?.fingerprint !== fingerprint)
        consent.current = { fingerprint, requestId: crypto.randomUUID() }
      return createAccessSessionGeneral({
        path: { vault_id: vaultId },
        body: { ...body, request_id: consent.current.requestId },
        throwOnError: true,
      })
    },
    onSuccess: ({ data }) => {
      void client.invalidateQueries({
        queryKey: [{ _id: 'listAccessSessionsGeneral' }],
      })
      void client.invalidateQueries({
        queryKey: [{ _id: 'listServiceGrants' }],
      })
      if (mounted.current) navigate(sessionHref(data))
    },
  })
  const invalidBundle =
    snapshot.services.length === 0 ||
    snapshot.services.length > 16 ||
    snapshot.services.length !== snapshot.bundle.service_ids.length ||
    snapshot.services.some((s) => !s.enabled || !s.has_credential)
  return (
    <Settings
      title="Create session"
      saveLabel="Approve session"
      dirty={
        canIssue &&
        eligibility.isSuccess &&
        !eligibility.isFetching &&
        !invalidBundle &&
        !sandboxes.isError
      }
      saving={issue.isPending}
      onCancel={() => {
        if (!issue.isPending) navigate(back)
      }}
      onSubmit={form.handleSubmit((v) => issue.mutate(v))}
      errors={{
        Sandbox: form.formState.errors.sandbox_id?.message,
        Lifetime: form.formState.errors.ttl_seconds?.message,
      }}
    >
      <fieldset disabled={issue.isPending} className="space-y-6">
        <div className="rounded-lg border p-4 space-y-3">
          <h2 className="font-semibold">{snapshot.bundle.name}</h2>
          {snapshot.services.map((s) => (
            <div key={s.id} className="flex items-start gap-3">
              <ServiceProviderLogo service={s} />
              <div className="min-w-0">
                <p className="text-sm font-medium">{s.name}</p>
                <p className="break-all text-xs text-muted-foreground">
                  {s.policy.origin} · {s.policy.methods.join(', ')} ·{' '}
                  {s.policy.path_prefixes.join(', ')}
                </p>
              </div>
            </div>
          ))}
        </div>
        {selected && eligibility.isError && (
          <Callout title="Sandbox access needs a covering link">
            {problemDetail(
              eligibility.error,
              'An administrator must link this sandbox or its project to this vault.'
            )}
            {canManage && (
              <Button
                type="button"
                variant="outline"
                disabled={link.isPending}
                aria-busy={link.isPending}
                onClick={() => link.mutate()}
              >
                Link this sandbox
              </Button>
            )}
            <Button
              type="button"
              variant="outline"
              onClick={() => void eligibility.refetch()}
            >
              Check again
            </Button>
          </Callout>
        )}
        {link.isError && (
          <Callout tone="error" title="Could not link sandbox">
            {problemDetail(
              link.error,
              'Check vault credential restrictions and retry.'
            )}
          </Callout>
        )}
        {invalidBundle && (
          <Callout tone="error" title="Bundle cannot issue a sandbox session">
            Choose 1–16 enabled services with credentials, then return here to
            review the updated bundle.
          </Callout>
        )}
        <fieldset className="space-y-3">
          <legend className="text-sm font-medium">Allowed sandbox</legend>
          <p className="text-xs text-muted-foreground">
            Only this sandbox can use the session. Credentials stay in the
            Vault.
          </p>
          {selected && (
            <p className="text-xs break-all">Selected: {selected}</p>
          )}
          {sandboxes.isError && (
            <Callout tone="error" title="Could not load sandboxes">
              <Button type="button" onClick={() => void sandboxes.refetch()}>
                Retry
              </Button>
            </Callout>
          )}
          {sandboxes.isPending ? (
            <Skeleton className="h-24" />
          ) : (
            <div className="grid gap-3 sm:grid-cols-2">
              {sandboxes.data?.sandboxes.map((s) => (
                <label
                  key={s.id}
                  className="flex items-center gap-3 rounded-md border p-3 has-[:checked]:border-foreground has-[:disabled]:opacity-60"
                >
                  <input
                    type="radio"
                    value={s.id}
                    {...form.register('sandbox_id')}
                    disabled={s.status !== 'running'}
                    className="accent-foreground"
                  />
                  <Box className="size-4 shrink-0" />
                  <span className="min-w-0 flex-1">
                    <span className="block break-words text-sm font-medium">
                      {s.name}
                    </span>
                    <span className="block break-all text-xs text-muted-foreground">
                      {s.status === 'running'
                        ? s.id
                        : 'Resume this sandbox before issuing access'}
                    </span>
                  </span>
                  <Status tone="idle" label={s.status} />
                </label>
              ))}
            </div>
          )}
          {sandboxes.data?.sandboxes.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No sandboxes available.{' '}
              <Link to="/sandboxes" className="underline">
                Create a sandbox
              </Link>
            </p>
          )}
          {Math.ceil((sandboxes.data?.pagination.count ?? 0) / 20) > 1 && (
            <ResponsivePagination
              page={page}
              pageSize={20}
              total={sandboxes.data?.pagination.count ?? 0}
              totalPages={Math.ceil(
                (sandboxes.data?.pagination.count ?? 0) / 20
              )}
              onPageChange={setPage}
            />
          )}
          {form.formState.errors.sandbox_id && (
            <p role="alert" className="text-sm text-destructive">
              {form.formState.errors.sandbox_id.message}
            </p>
          )}
        </fieldset>
        <Field
          label="Session lifetime (seconds)"
          description={`Maximum ${maxTtl / 60} minutes. This session cannot be extended; create a new one when it expires.`}
          error={form.formState.errors.ttl_seconds?.message}
        >
          {(props) => (
            <Input
              {...props}
              type="number"
              min={60}
              max={maxTtl}
              {...form.register('ttl_seconds', {
                valueAsNumber: true,
                max: maxTtl,
              })}
            />
          )}
        </Field>
        {issue.isError && (
          <Callout tone="error" title="Could not issue session">
            {problemDetail(
              issue.error,
              'Review the sandbox, policies and lifetime before retrying.'
            )}
          </Callout>
        )}
      </fieldset>
    </Settings>
  )
}

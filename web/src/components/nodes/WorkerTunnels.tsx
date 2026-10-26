// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Tunnels on a worker (ADR 058 D8). Everything provider-specific comes from
// the provider's descriptor, so a new provider needs no code here.

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  CheckCircle2,
  ChevronRight,
  Loader2,
  RefreshCw,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm, type UseFormReturn } from 'react-hook-form'
import { Link, useLocation } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  createIngressConnectorMutation,
  deleteIngressConnectorMutation,
  listDnsProvidersOptions,
  listIngressConnectorsOptions,
  listIngressProvidersOptions,
  listManagedDomainsOptions,
  setIngressConnectorNodesMutation,
  syncIngressConnectorMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { allTunnelsOptions } from '@/lib/tunnel-domains'
import type {
  ConnectorNodeView,
  ConnectorView,
  NodeInfoResponse,
  ProviderInfo,
  SetupField,
} from '@/api/client/types.gen'
import {
  CloudflareTokenChecklist,
  CloudflareTokenGuide,
} from '@/components/domains/CloudflareTokenGuide'
import { TunnelHostnames } from '@/components/domains/TunnelHostnames'
import { TunnelLinks } from '@/components/nodes/TunnelLinks'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'

/** What a replica's reported state means, for the operator. */
export function replicaState(replica: ConnectorNodeView): {
  label: string
  ok: boolean
} {
  switch (replica.state) {
    case 'ready':
      return { label: 'Connected', ok: true }
    case 'starting':
      return { label: 'Starting', ok: false }
    case 'pending':
      return { label: 'Waiting for the worker to pick it up', ok: false }
    case 'refused':
      return { label: 'Refused by the worker', ok: false }
    case 'degraded':
      return { label: 'Degraded', ok: false }
    default:
      return { label: 'Not running', ok: false }
  }
}

export function WorkerTunnels({
  node,
  allowed,
  recommended,
}: {
  node: NodeInfoResponse
  /** Whether the worker delegated ingress; tunnels need that too. */
  allowed: boolean
  /** Lead with tunnels: the worker has no address the internet can reach. */
  recommended: boolean
}) {
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const providersQuery = useQuery(listIngressProvidersOptions())
  const connectorsQuery = useQuery({
    ...allTunnelsOptions(),
    // Replicas report every few seconds.
    refetchInterval: 10_000,
  })
  // Kept after closing, so the dialog closes on the same provider and the
  // answers survive a cancel.
  const [setupFor, setSetupFor] = useState<ProviderInfo | null>(null)
  const [setupOpen, setSetupOpen] = useState(false)
  const [deleting, setDeleting] = useState<ConnectorView | null>(null)

  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: listIngressConnectorsOptions().queryKey,
    })
  const failed = (title: string) => (error: Error) =>
    toast.error(title, {
      description: problemDetail(error, 'Try again in a moment.'),
    })

  const setNodes = useMutation({
    ...setIngressConnectorNodesMutation(),
    onSuccess: refresh,
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => setNodes.mutate(variables)))
        return
      failed('Could not change where the tunnel runs')(error)
    },
  })
  const sync = useMutation({
    ...syncIngressConnectorMutation(),
    onSuccess: async () => {
      toast.success('Tunnel checked and republished')
      await refresh()
    },
    onError: failed('Could not sync the tunnel'),
  })
  const remove = useMutation({
    ...deleteIngressConnectorMutation(),
    onSuccess: async () => {
      toast.success('Tunnel deleted')
      setDeleting(null)
      await refresh()
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => remove.mutate(variables)))
        return
      failed('Could not delete the tunnel')(error)
    },
  })

  const providers = providersQuery.data ?? []
  const connectors = connectorsQuery.data ?? []
  const here = connectors.filter((c) =>
    c.nodes.some((n) => n.node_id === node.id)
  )
  const elsewhere = connectors.filter(
    (c) => !c.nodes.some((n) => n.node_id === node.id)
  )
  const providerName = (kind: string) =>
    providers.find((p) => p.kind === kind)?.name ?? kind

  if (providersQuery.isLoading || connectorsQuery.isLoading) {
    return (
      <div className="grid gap-3 md:grid-cols-2" aria-busy="true">
        <Skeleton className="h-36 w-full" />
        <Skeleton className="h-36 w-full" />
      </div>
    )
  }

  return (
    <div className="space-y-3">
      {here.map((connector) => {
        const replica = connector.nodes.find((n) => n.node_id === node.id)!
        const state = replicaState(replica)
        return (
          <div key={connector.id} className="space-y-2 rounded-md border p-3">
            <div className="flex flex-wrap items-start justify-between gap-2">
              <div className="min-w-0 space-y-0.5">
                <div className="flex items-center gap-2 text-sm font-medium">
                  {connector.name}
                  <Badge variant="outline">
                    {providerName(connector.provider)}
                  </Badge>
                </div>
                <p className="text-xs text-muted-foreground">
                  Apps under {connector.domain}
                  {connector.nodes.length > 1 &&
                    ` · also runs on ${connector.nodes.length - 1} other worker${connector.nodes.length > 2 ? 's' : ''}`}
                </p>
              </div>
              <div className="flex items-center gap-1">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={sync.isPending}
                  onClick={() => sync.mutate({ path: { id: connector.id } })}
                >
                  <RefreshCw className="mr-1 h-4 w-4" /> Sync
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={setNodes.isPending}
                  onClick={() =>
                    setNodes.mutate({
                      path: { id: connector.id },
                      body: {
                        node_ids: connector.nodes
                          .map((n) => n.node_id)
                          .filter((id) => id !== node.id),
                      },
                    })
                  }
                >
                  Stop here
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={`Delete tunnel ${connector.name}`}
                  onClick={() => setDeleting(connector)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </div>
            {connector.status === 'error' ? (
              <StatusLine ok={false}>
                {connector.last_error ?? 'The provider refused the tunnel.'}
              </StatusLine>
            ) : connector.status === 'pending' ? (
              <StatusLine ok={false}>Creating the tunnel…</StatusLine>
            ) : (
              <>
                <StatusLine ok={state.ok}>
                  {state.label}
                  {replica.last_error ? `: ${replica.last_error}` : ''}
                </StatusLine>
                {replica.state === 'ready' && (
                  <TunnelLinks
                    connector={connector}
                    nodeId={node.id}
                    readyConnections={replica.ready_connections}
                  />
                )}
              </>
            )}
            <TunnelHostnames connector={connector} />
          </div>
        )
      })}

      {allowed && here.length === 0 && elsewhere.length > 0 && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted-foreground">
            Run an existing tunnel here:
          </span>
          {elsewhere.map((connector) => (
            <Button
              key={connector.id}
              size="sm"
              variant="outline"
              disabled={setNodes.isPending}
              onClick={() =>
                setNodes.mutate({
                  path: { id: connector.id },
                  body: {
                    node_ids: [
                      ...connector.nodes.map((n) => n.node_id),
                      node.id,
                    ],
                  },
                })
              }
            >
              {connector.name} ({connector.domain})
            </Button>
          ))}
        </div>
      )}

      {allowed && here.length === 0 && (
        <div className="grid gap-3 md:grid-cols-2">
          {providers.map((provider) => (
            <div
              key={provider.kind}
              className="flex flex-col gap-2 rounded-md border p-3"
            >
              <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
                {provider.name}
                {recommended && !provider.needs_public_ip && (
                  <Badge>Recommended</Badge>
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                {provider.summary}
              </p>
              <ul className="list-disc space-y-0.5 pl-4 text-xs text-muted-foreground">
                {provider.requirements
                  // The checklist below spells the token out.
                  .filter(
                    (r) =>
                      provider.kind !== CLOUDFLARE_TUNNEL ||
                      !r.startsWith('Its API token')
                  )
                  .map((r) => (
                    <li key={r}>{r}</li>
                  ))}
              </ul>
              {provider.kind === CLOUDFLARE_TUNNEL && (
                <CloudflareTokenChecklist />
              )}
              <div className="mt-auto flex items-center gap-2 pt-1">
                <Button
                  size="sm"
                  onClick={() => {
                    setSetupFor(provider)
                    setSetupOpen(true)
                  }}
                >
                  Set up
                </Button>
                {provider.docs_url && (
                  <Button size="sm" variant="link" asChild className="px-0">
                    <a
                      href={provider.docs_url}
                      target="_blank"
                      rel="noreferrer"
                    >
                      How it works
                    </a>
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      <ProviderSetupDialog
        open={setupOpen}
        onOpenChange={setSetupOpen}
        provider={setupFor}
        node={node}
        onCreated={refresh}
      />
      <ConfirmDeleteDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        resourceType="tunnel"
        resourceName={deleting?.name ?? ''}
        consequences={
          deleting &&
          `Apps under ${deleting.domain} stop being reachable through it, on every worker that runs it. Temps removes the tunnel and its DNS records at ${providerName(deleting.provider)}.`
        }
        onConfirm={() =>
          deleting && remove.mutate({ path: { id: deleting.id } })
        }
      />
      {verificationDialog}
    </div>
  )
}

function StatusLine({
  ok,
  children,
}: {
  ok: boolean
  children: React.ReactNode
}) {
  return (
    <p className="flex items-start gap-2 text-sm">
      {ok ? (
        <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0 text-green-600" />
      ) : (
        <XCircle className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
      )}
      <span>{children}</span>
    </p>
  )
}

function defaultName(node: NodeInfoResponse) {
  const name = node.name
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/^-+|-+$/g, '')
  return (name || `worker-${node.id}`).slice(0, 50)
}

/** A managed domain as picked: a zone and an optional label under it. */
interface DomainPick {
  zone: string
  subdomain: string
}

const EMPTY_PICK: DomainPick = { zone: '', subdomain: '' }
/** `temps_ingress::cloudflare::KIND`. */
const CLOUDFLARE_TUNNEL = 'cloudflare_tunnel'
const TUNNEL_NAME = /^[A-Za-z0-9-]{1,50}$/
const DNS_LABELS =
  /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/

function subdomainLabel(subdomain: string) {
  return subdomain.trim().toLowerCase().replace(/\.+$/, '')
}

/** The answers for a provider: its own fields, checked against its descriptor. */
function setupSchema(provider: ProviderInfo | null) {
  const fields = provider?.fields ?? []
  return z
    .object({
      name: z
        .string()
        .trim()
        .regex(TUNNEL_NAME, 'Use 1-50 letters, digits or hyphens.'),
      values: z.record(z.string(), z.string()),
      picks: z.record(
        z.string(),
        z.object({ zone: z.string(), subdomain: z.string() })
      ),
    })
    .superRefine((data, ctx) => {
      for (const field of fields) {
        if (field.kind.type === 'managed_domain') {
          const pick = data.picks[field.key] ?? EMPTY_PICK
          const label = subdomainLabel(pick.subdomain)
          if (field.required && !pick.zone) {
            ctx.addIssue({
              code: 'custom',
              path: ['picks', field.key],
              message: 'Choose a domain.',
            })
          } else if (label && !DNS_LABELS.test(label)) {
            ctx.addIssue({
              code: 'custom',
              path: ['picks', field.key],
              message:
                'The subdomain takes letters, digits, hyphens and dots only.',
            })
          }
        } else if (field.required && !data.values[field.key]?.trim()) {
          ctx.addIssue({
            code: 'custom',
            path: ['values', field.key],
            message: `${field.label} is required.`,
          })
        }
      }
    })
}

type SetupValues = z.infer<ReturnType<typeof setupSchema>>

function setupDefaults(node: NodeInfoResponse): SetupValues {
  return { name: defaultName(node), values: {}, picks: {} }
}

/** The values the API takes: a picked domain becomes one hostname. */
function providerValues(provider: ProviderInfo, answers: SetupValues) {
  const values: Record<string, string> = {}
  for (const [key, value] of Object.entries(answers.values)) {
    if (value.trim()) values[key] = value
  }
  for (const field of provider.fields) {
    if (field.kind.type !== 'managed_domain') continue
    const pick = answers.picks[field.key] ?? EMPTY_PICK
    const label = subdomainLabel(pick.subdomain)
    const domain = pick.zone && label ? `${label}.${pick.zone}` : pick.zone
    if (domain) values[field.key] = domain
  }
  return values
}

/**
 * The setup form, rendered from the provider's fields. Stays mounted: the
 * answers survive closing it, and are cleared once the tunnel is created
 * or another provider is picked.
 */
function ProviderSetupDialog({
  open,
  onOpenChange,
  provider,
  node,
  onCreated,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  provider: ProviderInfo | null
  node: NodeInfoResponse
  onCreated: () => Promise<unknown>
}) {
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const schema = useMemo(() => setupSchema(provider), [provider])
  const form = useForm<SetupValues>({
    resolver: zodResolver(schema),
    defaultValues: setupDefaults(node),
  })
  const providerKind = provider?.kind
  useEffect(() => {
    form.reset(setupDefaults(node))
    // A different provider asks different questions; the node's name is
    // only the starting value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [providerKind])

  const create = useMutation({
    ...createIngressConnectorMutation(),
    onSuccess: async (connector) => {
      toast.success(
        connector.status === 'ready'
          ? `${provider?.name ?? 'Tunnel'} created. The worker connects within a few seconds.`
          : `${provider?.name ?? 'Tunnel'} saved`
      )
      await onCreated()
      onOpenChange(false)
      form.reset(setupDefaults(node))
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => create.mutate(variables)))
        return
      toast.error(`Could not set up ${provider?.name ?? 'the tunnel'}`, {
        description: problemDetail(error, 'Check the answers and try again.'),
      })
    },
  })

  const submit = form.handleSubmit((answers) => {
    if (!provider) return
    create.mutate({
      body: {
        name: answers.name.trim(),
        provider: provider.kind,
        values: providerValues(provider, answers),
        node_ids: [node.id],
      },
    })
  })

  return (
    <>
      <Dialog open={open && provider !== null} onOpenChange={onOpenChange}>
        <DialogContent>
          {provider && (
            <Form {...form}>
              <form onSubmit={submit} className="space-y-4">
                <DialogHeader>
                  <DialogTitle>Set up {provider.name}</DialogTitle>
                  <DialogDescription>{provider.summary}</DialogDescription>
                </DialogHeader>
                {provider.kind === CLOUDFLARE_TUNNEL && (
                  <div className="space-y-2">
                    <CloudflareTokenChecklist />
                    <Collapsible>
                      <CollapsibleTrigger className="group flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
                        <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
                        Step by step in Cloudflare
                      </CollapsibleTrigger>
                      <CollapsibleContent className="pt-2">
                        <CloudflareTokenGuide tunnel="required" />
                      </CollapsibleContent>
                    </Collapsible>
                  </div>
                )}
                <FormField
                  control={form.control}
                  name="name"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Name</FormLabel>
                      <FormControl>
                        <Input autoComplete="off" {...field} />
                      </FormControl>
                      <FormDescription>
                        Lowercase letters, digits and hyphens. Other workers can
                        run the same tunnel later for failover.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                {provider.fields.map((field) => (
                  <SetupFieldInput
                    key={field.key}
                    field={field}
                    provider={provider}
                    form={form}
                  />
                ))}
                <DialogFooter>
                  <Button
                    type="button"
                    variant="ghost"
                    onClick={() => onOpenChange(false)}
                  >
                    Cancel
                  </Button>
                  <Button type="submit" disabled={create.isPending}>
                    {create.isPending && (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    )}
                    Create and run on {node.name}
                  </Button>
                </DialogFooter>
              </form>
            </Form>
          )}
        </DialogContent>
      </Dialog>
      {verificationDialog}
    </>
  )
}

function SetupFieldInput({
  field,
  provider,
  form,
}: {
  field: SetupField
  provider: ProviderInfo
  form: UseFormReturn<SetupValues>
}) {
  const help = field.help && <FormDescription>{field.help}</FormDescription>
  if (field.kind.type === 'managed_domain') {
    const providerField = field.kind.provider_field
    const allowSubdomain = field.kind.allow_subdomain ?? false
    return (
      <FormField
        control={form.control}
        name={`picks.${field.key}`}
        render={({ field: control }) => (
          <FormItem>
            <FormLabel>{field.label}</FormLabel>
            <ManagedDomainSelect
              providerId={Number(form.watch(`values.${providerField}`)) || null}
              allowSubdomain={allowSubdomain}
              value={control.value ?? EMPTY_PICK}
              onChange={control.onChange}
            />
            {help}
            <FormMessage />
          </FormItem>
        )}
      />
    )
  }
  return (
    <FormField
      control={form.control}
      name={`values.${field.key}`}
      render={({ field: control }) => (
        <FormItem>
          <FormLabel>{field.label}</FormLabel>
          {field.kind.type === 'dns_provider' ? (
            <DnsProviderSelect
              providerType={field.kind.provider_type ?? null}
              value={control.value ?? ''}
              onChange={(value) => {
                control.onChange(value)
                // A domain belongs to the DNS provider it was picked under.
                for (const other of provider.fields) {
                  if (
                    other.kind.type === 'managed_domain' &&
                    other.kind.provider_field === field.key
                  ) {
                    form.setValue(`picks.${other.key}`, EMPTY_PICK)
                  }
                }
              }}
            />
          ) : (
            <FormControl>
              <Input
                type={field.kind.type === 'secret' ? 'password' : 'text'}
                autoComplete="off"
                placeholder={field.placeholder ?? undefined}
                {...control}
                value={control.value ?? ''}
              />
            </FormControl>
          )}
          {help}
          <FormMessage />
        </FormItem>
      )}
    />
  )
}

function DnsProviderSelect({
  providerType,
  value,
  onChange,
}: {
  providerType: string | null
  value: string
  onChange: (value: string) => void
}) {
  const query = useQuery(listDnsProvidersOptions())
  const location = useLocation()
  const providers = (query.data ?? []).filter(
    (p) =>
      p.is_active &&
      (!providerType || p.provider_type.toLowerCase() === providerType)
  )
  if (query.isLoading) {
    return <Skeleton className="h-9 w-full" />
  }
  if (providers.length === 0) {
    const cloudflare = providerType === 'cloudflare'
    const label = cloudflare ? 'Cloudflare' : providerType
    const back = `${location.pathname}${location.search}`
    const add = providerType
      ? `/dns-providers/add?provider=${encodeURIComponent(providerType)}&return=${encodeURIComponent(back)}`
      : `/dns-providers/add?return=${encodeURIComponent(back)}`
    return (
      <Alert>
        <AlertDescription className="space-y-2">
          <p>
            No {label ? `${label} ` : ''}DNS provider yet.{' '}
            <Link className="underline" to={add}>
              Add one
            </Link>
            ; you come back here once it is added.
          </p>
          {cloudflare && <CloudflareTokenChecklist className="bg-background" />}
        </AlertDescription>
      </Alert>
    )
  }
  return (
    <Select value={value} onValueChange={onChange}>
      <FormControl>
        <SelectTrigger>
          <SelectValue placeholder="Choose…" />
        </SelectTrigger>
      </FormControl>
      <SelectContent>
        {providers.map((p) => (
          <SelectItem key={p.id} value={String(p.id)}>
            {p.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

function ManagedDomainSelect({
  providerId,
  allowSubdomain,
  value,
  onChange,
}: {
  providerId: number | null
  allowSubdomain: boolean
  value: DomainPick
  onChange: (value: DomainPick) => void
}) {
  const query = useQuery({
    ...listManagedDomainsOptions({ path: { id: providerId ?? 0 } }),
    enabled: providerId != null,
  })
  const { zone, subdomain } = value
  if (providerId == null) {
    return (
      <Select disabled>
        <FormControl>
          <SelectTrigger>
            <SelectValue placeholder="Choose the account first" />
          </SelectTrigger>
        </FormControl>
      </Select>
    )
  }
  if (query.isLoading) {
    return <Skeleton className="h-9 w-full" />
  }
  const domains = query.data ?? []
  if (domains.length === 0) {
    return (
      <Alert>
        <AlertDescription>
          This DNS provider manages no domain yet.{' '}
          <Link className="underline" to={`/dns-providers/${providerId}`}>
            Add one
          </Link>
          , then come back.
        </AlertDescription>
      </Alert>
    )
  }
  const label = subdomainLabel(subdomain)
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        {allowSubdomain && (
          <>
            <Input
              aria-label="Subdomain (optional)"
              className="w-40"
              placeholder="subdomain"
              autoComplete="off"
              value={subdomain}
              onChange={(e) => onChange({ zone, subdomain: e.target.value })}
            />
            <span className="text-muted-foreground">.</span>
          </>
        )}
        <Select
          value={zone}
          onValueChange={(next) => onChange({ zone: next, subdomain })}
        >
          <FormControl>
            <SelectTrigger className="flex-1">
              <SelectValue placeholder="Choose…" />
            </SelectTrigger>
          </FormControl>
          <SelectContent>
            {domains.map((d) => (
              <SelectItem key={d.id} value={d.domain}>
                {d.domain}
                {!d.verified && ' (not verified)'}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {allowSubdomain && !label && (
        <p className="text-xs text-muted-foreground">
          Leave the subdomain empty to use the whole domain, or enter one (e.g.{' '}
          <code>lab</code>) to keep this tunnel to it.
        </p>
      )}
      {label && zone && (
        <Alert>
          <AlertDescription className="space-y-1 text-xs">
            <p>
              The tunnel serves{' '}
              <code>
                {label}.{zone}
              </code>{' '}
              and the hostnames under it, such as{' '}
              <code>
                app.{label}.{zone}
              </code>
              .
            </p>
            <p>
              Cloudflare&apos;s free certificate covers only one level below{' '}
              {zone}:{' '}
              <code>
                {label}.{zone}
              </code>{' '}
              works out of the box, but{' '}
              <code>
                app.{label}.{zone}
              </code>{' '}
              needs Cloudflare&apos;s Advanced Certificate Manager (or Total
              TLS) for{' '}
              <code>
                *.{label}.{zone}
              </code>
              , or visitors get a certificate error.
            </p>
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}
